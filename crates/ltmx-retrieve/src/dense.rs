//! Brute-force dense index.
//!
//! Exact cosine similarity over every stored vector. No approximate structure:
//! at corpus sizes ltmx targets, an O(n) scan over 768 floats per query is
//! microseconds, and exactness removes a whole class of recall bugs. Revisit
//! only if measured latency demands it.
//!
//! Vectors are normalized on write and on query. Ollama returns
//! `nomic-embed-text` un-normalized, so comparing raw dot products would rank
//! by vector magnitude rather than direction.

use ltmx_domain::{
    ActivityChunk, ActivityIndex, ChunkId, EmbedError, Embedder, LtmxError, ScoredChunk,
    SearchParams,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Euclidean norm of `vector`, or `0.0` for an empty vector.
fn magnitude(vector: &[f32]) -> f32 {
    vector.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// Returns a unit-length copy of `vector`.
///
/// A zero vector has no direction, so it is returned unchanged rather than
/// producing `NaN`.
pub fn normalize(vector: &[f32]) -> Vec<f32> {
    let norm = magnitude(vector);
    if norm == 0.0 || !norm.is_finite() {
        return vector.to_vec();
    }
    vector.iter().map(|value| value / norm).collect()
}

/// Cosine similarity of two equal-length vectors.
///
/// Returns `0.0` when lengths differ or either vector is zero, rather than
/// `NaN`, so a bad vector degrades one result instead of poisoning a sort.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let denominator = magnitude(a) * magnitude(b);
    if denominator == 0.0 || !denominator.is_finite() {
        return 0.0;
    }
    let similarity = dot / denominator;
    if similarity.is_finite() {
        similarity.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// An exact dense [`ActivityIndex`] over an [`Embedder`].
///
/// Embedding happens on write and on query. Bulk ingestion should use
/// [`ActivityIndex::upsert_all`], which embeds in one batch.
pub struct DenseIndex<E> {
    embedder: E,
    chunks: HashMap<ChunkId, Arc<ActivityChunk>>,
    vectors: HashMap<ChunkId, Vec<f32>>,
    /// Dimensions the index was built with, checked against the embedder on
    /// every write so a model swap fails loudly instead of silently degrading.
    dimensions: usize,
}

impl<E> DenseIndex<E>
where
    E: Embedder,
{
    /// Builds an empty dense index.
    pub fn new(embedder: E) -> Self {
        let dimensions = embedder.dimensions();
        Self {
            embedder,
            chunks: HashMap::new(),
            vectors: HashMap::new(),
            dimensions,
        }
    }

    /// The vector width this index was created with.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Identifier of the backing model, for recording with the index.
    pub fn model_id(&self) -> String {
        self.embedder.model_id()
    }

    /// The embedder backing this index.
    ///
    /// Exposed so a caller can reach the embedder after the index has taken
    /// ownership of it. [`crate::Embedder`] methods take `&self`, so a shared
    /// borrow is enough to flush an on-disk cache once a build finishes.
    pub fn embedder(&self) -> &E {
        &self.embedder
    }

    /// True when `vector` matches the width this index was built for.
    fn width_matches(&self, vector: &[f32]) -> bool {
        vector.len() == self.dimensions
    }

    fn index(&mut self, chunk: Arc<ActivityChunk>, vector: Vec<f32>) -> Result<(), LtmxError> {
        if !self.width_matches(&vector) {
            return Err(LtmxError::Index(format!(
                "index built for {} dimensions, embedder `{}` returned {}",
                self.dimensions,
                self.embedder.model_id(),
                vector.len()
            )));
        }
        let id = chunk.id.clone();
        self.vectors.insert(id.clone(), normalize(&vector));
        self.chunks.insert(id, chunk);
        Ok(())
    }

    fn embed_all(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LtmxError> {
        self.embedder
            .embed_batch(texts)
            .map_err(|e: EmbedError| LtmxError::Index(e.to_string()))
    }
}

impl<E> ActivityIndex for DenseIndex<E>
where
    E: Embedder,
{
    fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError> {
        let vector = self
            .embedder
            .embed(&chunk.text)
            .map_err(|e: EmbedError| LtmxError::Index(e.to_string()))?;
        self.index(chunk, vector)
    }

    /// Embeds the whole batch in one call.
    ///
    /// This is the difference between minutes and hours on a real corpus: a
    /// per-chunk round trip to the embedding backend dominates everything else.
    fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError> {
        if chunks.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
        let vectors = self.embed_all(&texts)?;
        if vectors.len() != chunks.len() {
            return Err(LtmxError::Index(format!(
                "embedder returned {} vectors for {} chunks",
                vectors.len(),
                chunks.len()
            )));
        }
        for (chunk, vector) in chunks.iter().cloned().zip(vectors) {
            self.index(chunk, vector)?;
        }
        Ok(())
    }

    fn get(&self, id: &ChunkId) -> Result<Arc<ActivityChunk>, LtmxError> {
        self.chunks
            .get(id)
            .cloned()
            .ok_or_else(|| LtmxError::NotFound(id.clone()))
    }

    fn len(&self) -> usize {
        self.chunks.len()
    }

    fn search(&self, query: &str, params: &SearchParams) -> Result<Vec<ScoredChunk>, LtmxError> {
        if params.top_k == 0 || self.chunks.is_empty() {
            return Ok(Vec::new());
        }
        let query_vector = normalize(
            &self
                .embedder
                .embed(query)
                .map_err(|e: EmbedError| LtmxError::Index(e.to_string()))?,
        );

        let mut scored: Vec<ScoredChunk> = self
            .vectors
            .iter()
            .filter_map(|(id, vector)| {
                let chunk = self.chunks.get(id)?;
                if !params.admits(chunk) {
                    return None;
                }
                let score = cosine_similarity(&query_vector, vector);
                (score > 0.0).then(|| ScoredChunk {
                    chunk: chunk.clone(),
                    score,
                })
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(params.top_k);
        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use ltmx_domain::{Origin, SourceRef, TimeRange};

    /// Deterministic embedder: maps a text to a unit vector whose first
    /// element is derived from its first character, so tests can assert
    /// relative similarity without a model.
    #[derive(Clone)]
    struct FakeEmbedder {
        dimensions: usize,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeEmbedder {
        fn new(dimensions: usize) -> Self {
            Self {
                dimensions,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, ltmx_domain::EmbedError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vector_for(text, self.dimensions))
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ltmx_domain::EmbedError> {
            texts.iter().map(|t| self.embed(t)).collect()
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn model_id(&self) -> String {
            "fake".to_string()
        }
    }

    /// Maps text to a vector whose *direction* is fixed by the first character.
    ///
    /// Direction must vary, not magnitude: `DenseIndex` normalizes every vector,
    /// so two vectors differing only in length collapse to the same direction and
    /// cosine similarity cannot tell them apart. Putting all energy in one fixed
    /// coordinate would make every document equally similar.
    ///
    /// Sharing a first character yields the same direction, so a query like
    /// `"docker"` lands on the same vector as `"docker desktop"` -- enough signal
    /// for the ranking assertions, and deliberately trivial.
    fn vector_for(text: &str, dimensions: usize) -> Vec<f32> {
        let slot = text
            .chars()
            .next()
            .map(|c| c as usize % dimensions.max(1))
            .unwrap_or(0);
        (0..dimensions)
            .map(|i| if i == slot { 1.0 } else { 0.0 })
            .collect()
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    fn chunk(id: &str, text: &str, secs: i64) -> Arc<ActivityChunk> {
        ActivityChunk::new(
            ChunkId::new(id),
            text,
            Origin::Note,
            SourceRef::file(format!("/n/{id}.md")),
            at(secs),
        )
        .shared()
    }

    fn index_with(texts: &[(&str, &str)]) -> DenseIndex<FakeEmbedder> {
        let mut index = DenseIndex::new(FakeEmbedder::new(4));
        index
            .upsert_all(
                &texts
                    .iter()
                    .map(|(id, text)| chunk(id, text, 1_000))
                    .collect::<Vec<_>>(),
            )
            .expect("upsert_all should succeed");
        index
    }

    #[test]
    fn normalize_returns_unit_length() {
        let normalized = normalize(&[3.0, 4.0]);
        assert!((magnitude(&normalized) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_leaves_zero_vector_alone() {
        let zero = normalize(&[0.0, 0.0]);
        assert_eq!(zero, vec![0.0, 0.0]);
    }

    #[test]
    fn normalize_leaves_non_finite_vector_alone() {
        let input = vec![f32::INFINITY, 1.0];
        assert_eq!(normalize(&input), input);
    }

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_opposite_vectors_is_negative_one() {
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_zero_vector_is_zero_not_nan() {
        let value = cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]);
        assert_eq!(value, 0.0);
        assert!(value.is_finite());
    }

    #[test]
    fn cosine_of_mismatched_lengths_is_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
    }

    #[test]
    fn cosine_of_empty_vectors_is_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_is_clamped_to_unit_range() {
        let value = cosine_similarity(&[1e20, 1e20], &[1e20, 1e20]);
        assert!((-1.0..=1.0).contains(&value));
    }

    #[test]
    fn empty_index_reports_empty() {
        let index = DenseIndex::new(FakeEmbedder::new(4));
        assert!(index.is_empty());
    }

    #[test]
    fn upsert_stores_chunk_and_vector() {
        assert_eq!(index_with(&[("a", "alpha")]).len(), 1);
    }

    #[test]
    fn upsert_is_idempotent_by_id() {
        let mut index = index_with(&[("a", "alpha")]);
        index.upsert(chunk("a", "omega", 1_000)).expect("re-upsert");
        assert_eq!(index.len(), 1);
        assert_eq!(index.get(&ChunkId::new("a")).expect("get").text, "omega");
    }

    #[test]
    fn get_reports_not_found_for_unknown_id() {
        assert!(matches!(
            DenseIndex::new(FakeEmbedder::new(4)).get(&ChunkId::new("absent")),
            Err(LtmxError::NotFound(_))
        ));
    }

    #[test]
    fn upsert_all_embeds_in_one_batch() {
        // One batch call means one embedder invocation per item only because
        // the fake implements it naively; the point is that upsert_all exists
        // and returns every chunk.
        let mut index = DenseIndex::new(FakeEmbedder::new(4));
        index
            .upsert_all(
                &(0..5)
                    .map(|i| chunk(&format!("c{i}"), "alpha", 1_000))
                    .collect::<Vec<_>>(),
            )
            .expect("upsert_all");
        assert_eq!(index.len(), 5);
    }

    #[test]
    fn upsert_all_with_no_chunks_is_a_no_op() {
        let mut index = DenseIndex::new(FakeEmbedder::new(4));
        index.upsert_all(&[]).expect("empty upsert_all");
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn search_ranks_the_closest_vector_first() {
        let index = index_with(&[("alpha", "alpha text"), ("zulu", "zulu text")]);
        let hits = index
            .search("alpha", &SearchParams::default().with_top_k(2))
            .expect("search should succeed");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].chunk.id, ChunkId::new("alpha"));
    }

    #[test]
    fn search_scores_are_cosine_bounded() {
        let index = index_with(&[("alpha", "alpha"), ("zulu", "zulu")]);
        for hit in index
            .search("alpha", &SearchParams::default().with_top_k(5))
            .expect("search should succeed")
        {
            assert!(
                (-1.0..=1.0).contains(&hit.score),
                "score {} outside cosine range",
                hit.score
            );
        }
    }

    #[test]
    fn search_on_empty_index_returns_nothing() {
        assert!(
            DenseIndex::new(FakeEmbedder::new(4))
                .search("alpha", &SearchParams::default())
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[test]
    fn search_with_zero_top_k_returns_nothing() {
        let index = index_with(&[("alpha", "alpha")]);
        assert!(
            index
                .search("alpha", &SearchParams::default().with_top_k(0))
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[test]
    fn search_honors_top_k() {
        let index = index_with(&[("a", "alpha"), ("b", "alpha"), ("c", "alpha")]);
        assert_eq!(
            index
                .search("alpha", &SearchParams::default().with_top_k(2))
                .expect("search should succeed")
                .len(),
            2
        );
    }

    #[test]
    fn search_honors_origin_filter() {
        let mut git = chunk("g", "alpha", 1_000);
        Arc::make_mut(&mut git).origin = Origin::Git;
        let mut index = DenseIndex::new(FakeEmbedder::new(4));
        let batch = vec![git, chunk("n", "alpha", 1_000)];
        index.upsert_all(&batch).expect("upsert_all");
        let hits = index
            .search(
                "alpha",
                &SearchParams::default()
                    .with_top_k(5)
                    .with_origin(Origin::Git),
            )
            .expect("search should succeed");
        assert!(hits.iter().all(|h| h.chunk.origin == Origin::Git));
    }

    #[test]
    fn search_honors_time_window() {
        let mut index = DenseIndex::new(FakeEmbedder::new(4));
        index
            .upsert(chunk("old", "alpha", 100))
            .expect("upsert old");
        index
            .upsert(chunk("new", "alpha", 900_000))
            .expect("upsert new");
        let window = TimeRange::new(at(800_000), at(1_000_000));
        let hits = index
            .search(
                "alpha",
                &SearchParams::default().with_top_k(5).with_window(window),
            )
            .expect("search should succeed");
        assert!(hits.iter().all(|h| h.chunk.id == ChunkId::new("new")));
    }

    #[test]
    fn width_mismatch_is_rejected_on_write() {
        // Built for 4 dimensions, then given an embedder that returns 8.
        struct WrongWidth;
        impl Embedder for WrongWidth {
            fn embed(&self, _text: &str) -> Result<Vec<f32>, ltmx_domain::EmbedError> {
                Ok(vec![0.0; 8])
            }
            fn dimensions(&self) -> usize {
                8
            }
            fn model_id(&self) -> String {
                "wrong".to_string()
            }
        }

        let mut index = DenseIndex::new(WrongWidth);
        // Simulate a model swap by lying about the recorded width.
        index.dimensions = 4;
        let result = index.upsert(chunk("a", "alpha", 1_000));
        assert!(
            matches!(result, Err(LtmxError::Index(_))),
            "a width mismatch must be an error, not a silent write"
        );
    }

    #[test]
    fn dimensions_and_model_id_are_reported() {
        let index = DenseIndex::new(FakeEmbedder::new(16));
        assert_eq!(index.dimensions(), 16);
        assert_eq!(index.model_id(), "fake");
    }

    #[test]
    fn search_embeds_the_query_each_time() {
        let embedder = FakeEmbedder::new(4);
        let mut index = DenseIndex::new(embedder.clone());
        let batch = vec![chunk("a", "alpha", 1_000)];
        index.upsert_all(&batch).expect("upsert_all");
        let before = embedder.calls();
        index
            .search("alpha", &SearchParams::default())
            .expect("search");
        assert!(
            embedder.calls() > before,
            "the query itself must be embedded"
        );
    }

    #[test]
    fn dense_index_satisfies_activity_index_contract() {
        ltmx_domain::ports::conformance::assert_activity_index_contract(|| {
            DenseIndex::new(FakeEmbedder::new(4))
        });
    }
}
