//! Ports the adapters implement.
//!
//! These traits live in the domain, not beside their adapters, so an adapter
//! crate depends on the domain and never the reverse. Every adapter gets its
//! contract checked against [`conformance::assert_activity_index_contract`].

pub mod conformance;

use crate::error::{EmbedError, LtmxError};
use crate::types::{ActivityChunk, ChunkId, Origin, OriginSet, TimeRange};
use std::sync::Arc;

/// Bounds and filters applied to a retrieval request.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchParams {
    /// Maximum number of results to return.
    pub top_k: usize,
    /// Restricts results to these origins; empty means all origins.
    pub origins: OriginSet,
    /// Restricts results to chunks occurring inside this window.
    pub window: Option<TimeRange>,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            top_k: 5,
            origins: OriginSet::EMPTY,
            window: None,
        }
    }
}

impl SearchParams {
    /// Returns at most `top_k` results.
    pub fn with_top_k(mut self, top_k: usize) -> Self {
        self.top_k = top_k;
        self
    }

    /// Restricts the search to `window`.
    pub fn with_window(mut self, window: TimeRange) -> Self {
        self.window = Some(window);
        self
    }

    /// Restricts the search to a single origin.
    ///
    /// Accumulates: chaining two calls admits either origin, rather than the
    /// second call silently replacing the first.
    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origins = self.origins.with(origin);
        self
    }

    /// Adds `origins` to the accepted set.
    pub fn with_origins<I: IntoIterator<Item = Origin>>(mut self, origins: I) -> Self {
        for origin in origins {
            self.origins = self.origins.with(origin);
        }
        self
    }

    /// Reports whether `chunk` satisfies the origin and window filters.
    ///
    /// This is the single definition of "admits"; every adapter that supports
    /// filtering must agree with it, and the conformance suite asserts it.
    pub fn admits(&self, chunk: &ActivityChunk) -> bool {
        if !self.origins.is_empty() && !self.origins.contains(chunk.origin) {
            return false;
        }
        match self.window {
            Some(w) => {
                let point = TimeRange::new(chunk.occurred_at, chunk.occurred_at);
                w.overlaps(&point)
            }
            None => true,
        }
    }
}

/// A chunk with its relevance score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    /// The matched chunk, shared with the index that produced it.
    pub chunk: Arc<ActivityChunk>,
    /// Relevance score. Higher is better; the scale is adapter-defined, but
    /// results must be returned in non-increasing order.
    pub score: f32,
}

/// Storage and retrieval over [`ActivityChunk`]s.
///
/// Chunks are held as [`Arc`] so several indexes can serve the same corpus
/// without copying it; a transcript chunk owns its text, and deep-copying a
/// whole corpus once per index is the single most expensive thing an index
/// build can do.
///
/// # Contract
///
/// Implementations must uphold all of the following. The conformance suite in
/// [`conformance`] asserts each one.
///
/// - [`ActivityIndex::upsert`] is **idempotent** on `chunk.id`: writing the
///   same id twice leaves exactly one chunk, holding the second write.
/// - [`ActivityIndex::get`] returns [`LtmxError::NotFound`] for an unknown id.
/// - [`ActivityIndex::search`] returns results in **non-increasing score
///   order**, truncated to at most `params.top_k` entries.
/// - [`ActivityIndex::search`] never returns a chunk that
///   [`SearchParams::admits`] rejects.
/// - [`ActivityIndex::search`] returns an empty list -- never an error -- for
///   a `top_k` of zero or a query with no lexical signal.
/// - [`ActivityIndex::len`] counts distinct chunk ids.
pub trait ActivityIndex {
    /// Adds or replaces a chunk.
    fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError>;

    /// Adds or replaces many chunks, equivalent to repeated [`Self::upsert`].
    ///
    /// Takes a slice rather than `Vec` so callers can hand over any contiguous
    /// collection without a conversion, and rather than `impl IntoIterator`
    /// because a generic method would make this trait **not object safe** --
    /// and `&dyn ActivityIndex` is what lets several indexes be swapped at
    /// runtime, which is the entire point of the port.
    fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError>;

    /// Returns the chunk stored under `id`.
    fn get(&self, id: &ChunkId) -> Result<Arc<ActivityChunk>, LtmxError>;

    /// Number of distinct indexed chunks.
    fn len(&self) -> usize;

    /// Reports whether the index holds no chunks.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns chunks matching `query`, best first.
    fn search(&self, query: &str, params: &SearchParams) -> Result<Vec<ScoredChunk>, LtmxError>;
}

/// A text-to-vector encoder.
///
/// Implementations are expected to be cheap to clone and safe to share: the
/// retrieval layer holds one for the process lifetime so the first query after
/// startup is not charged a model load.
///
/// # Contract
///
/// - Every vector returned by [`Embedder::embed`] and [`Embedder::embed_batch`]
///   has exactly [`Embedder::dimensions`] elements.
/// - [`Embedder::embed_batch`] preserves input order and returns one vector per
///   input, or an error if any item fails.
/// - [`Embedder::model_id`] changes whenever the produced vectors would change.
///   It is recorded with the index so a model swap is detected rather than
///   silently degrading retrieval quality.
pub trait Embedder: Send + Sync {
    /// Embeds a single string.
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError>;

    /// Embeds a batch, preserving order.
    ///
    /// The default calls [`Embedder::embed`] per item, which is correct but
    /// slow; backends that can batch server-side should override.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        texts.iter().map(|t| self.embed(t)).collect()
    }

    /// Width of the vectors this embedder returns.
    fn dimensions(&self) -> usize;

    /// Stable identifier recorded alongside the index.
    fn model_id(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OriginSet, SourceRef};
    use chrono::{DateTime, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn chunk(origin: Origin, secs: i64) -> ActivityChunk {
        ActivityChunk::new(
            ChunkId::new("c1"),
            "text",
            origin,
            SourceRef::file("/x"),
            at(secs),
        )
    }

    #[test]
    fn default_params_return_five() {
        assert_eq!(SearchParams::default().top_k, 5);
        assert!(SearchParams::default().origins.is_empty());
        assert!(SearchParams::default().window.is_none());
    }

    #[test]
    fn admits_every_origin_when_unfiltered() {
        let params = SearchParams::default();
        assert!(params.admits(&chunk(Origin::Git, 0)));
        assert!(params.admits(&chunk(Origin::Note, 0)));
    }

    #[test]
    fn admits_respects_origin_filter() {
        let params = SearchParams::default().with_origin(Origin::Git);
        assert!(params.admits(&chunk(Origin::Git, 0)));
        assert!(!params.admits(&chunk(Origin::Note, 0)));
    }

    #[test]
    fn admits_respects_window_boundaries() {
        let params = SearchParams::default().with_window(TimeRange::new(at(100), at(200)));
        assert!(params.admits(&chunk(Origin::Git, 100)));
        assert!(params.admits(&chunk(Origin::Git, 200)));
        assert!(!params.admits(&chunk(Origin::Git, 99)));
        assert!(!params.admits(&chunk(Origin::Git, 201)));
    }

    #[test]
    fn admits_uses_any_origin_when_several_are_listed() {
        let params = SearchParams::default()
            .with_origin(Origin::Git)
            .with_origin(Origin::Note);
        assert!(params.admits(&chunk(Origin::Git, 0)));
        assert!(params.admits(&chunk(Origin::Note, 0)));
        assert!(!params.admits(&chunk(Origin::Screen, 0)));
    }

    #[test]
    fn with_top_k_does_not_clear_filters() {
        let params = SearchParams::default()
            .with_origin(Origin::Note)
            .with_top_k(3);
        assert_eq!(params.top_k, 3);
        assert_eq!(params.origins, OriginSet::new([Origin::Note]));
    }
}
