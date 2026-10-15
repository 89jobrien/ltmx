//! Reciprocal rank fusion.
//!
//! Fuses ranked lists that do not share a score scale. BM25 scores and cosine
//! similarities are not comparable -- a BM25 score of 7.6 means nothing next to
//! a cosine of 0.71 -- so fusion combines *ranks* instead, which is what
//! Reciprocal Rank Fusion does and why it needs no score calibration.

use ltmx_domain::{ActivityIndex, ChunkId, LtmxError, ScoredChunk, SearchParams};
use std::collections::HashMap;
use std::sync::Arc;

/// RRF tuning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RrfParams {
    /// Rank damping constant. The standard value is 60.
    pub k: f32,
}

impl Default for RrfParams {
    fn default() -> Self {
        Self { k: 60.0 }
    }
}

/// Fuses ranked lists into one ranking.
///
/// Each input is one retriever's ranking, ordered best-first. A chunk appearing
/// in several lists accumulates `1 / (k + rank)` per list, so a chunk both
/// retrievers agree on outranks one that only a single retriever liked.
///
/// Ties break on first-seen order, which makes the result deterministic.
pub fn rrf(lists: &[&[ChunkId]], params: RrfParams) -> Vec<(ChunkId, f32)> {
    let mut fused: HashMap<ChunkId, f32> = HashMap::new();
    // First-seen order, so ties resolve the same way on every run.
    let mut order: Vec<ChunkId> = Vec::new();

    for ranked in lists {
        for (position, id) in ranked.iter().enumerate() {
            // Rank is 1-based; a 0-based rank would make position 0 score as if
            // it were the worst possible hit.
            let rank = position as f32 + 1.0;
            *fused.entry(id.clone()).or_insert_with(|| {
                order.push(id.clone());
                0.0
            }) += 1.0 / (params.k + rank);
        }
    }

    let mut scored: Vec<(ChunkId, f32)> = fused.into_iter().collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let position = |id: &ChunkId| {
                    order
                        .iter()
                        .position(|candidate| candidate == id)
                        .unwrap_or(usize::MAX)
                };
                position(&a.0).cmp(&position(&b.0))
            })
    });
    scored
}

/// Combines several [`ActivityIndex`] adapters into one retriever.
///
/// Borrows rather than owns its children, so an expensive index -- a dense
/// index needs a full embedding pass -- can be built once and shared across
/// several retrievers instead of rebuilt per query.
///
/// # Contract
///
/// - Results are ranked by [`rrf`] over each child's own ranking, so a child
///   that returns nothing contributes nothing rather than failing the query.
/// - Never returns a chunk that [`SearchParams::admits`] rejects.
/// - Returns at most `params.top_k` results, in non-increasing score order.
pub struct HybridRetriever<'a> {
    children: Vec<&'a dyn ActivityIndex>,
    params: RrfParams,
}

impl<'a> HybridRetriever<'a> {
    /// Builds a retriever over `children`.
    pub fn new<I>(children: I) -> Self
    where
        I: IntoIterator<Item = &'a dyn ActivityIndex>,
    {
        Self {
            children: children.into_iter().collect(),
            params: RrfParams::default(),
        }
    }

    /// Builds a retriever with explicit fusion parameters.
    pub fn with_params<I>(children: I, params: RrfParams) -> Self
    where
        I: IntoIterator<Item = &'a dyn ActivityIndex>,
    {
        Self {
            children: children.into_iter().collect(),
            params,
        }
    }

    /// Returns the number of participating indexes.
    ///
    /// Deliberately not named `len`: [`ActivityIndex::len`] means chunk count,
    /// and two methods called `len` with different meanings is a trap.
    pub fn index_count(&self) -> usize {
        self.children.len()
    }

    /// Reports whether no index participates.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Fuses the children and resolves ids to chunks.
    ///
    /// Each child is asked for more than `top_k` results, because a chunk that
    /// only the fourth-ranked result of both lists would never surface
    /// otherwise. Fusion needs overlap to work with.
    fn search(&self, query: &str, params: &SearchParams) -> Result<Vec<ScoredChunk>, LtmxError> {
        if params.top_k == 0 || self.children.is_empty() {
            return Ok(Vec::new());
        }

        // Overfetch: fusion rewards agreement, so a truncated candidate list
        // hides chunks that two retrievers both ranked moderately.
        let depth = (params.top_k * 4).max(params.top_k);
        let child_params = SearchParams {
            top_k: depth,
            origins: params.origins,
            window: params.window,
        };

        let mut lists: Vec<Vec<ChunkId>> = Vec::with_capacity(self.children.len());
        let mut chunks: HashMap<ChunkId, Arc<ltmx_domain::ActivityChunk>> = HashMap::new();

        for child in self.children.iter() {
            let hits = child.search(query, &child_params)?;
            let mut ids = Vec::with_capacity(hits.len());
            for hit in hits {
                ids.push(hit.chunk.id.clone());
                chunks.insert(hit.chunk.id.clone(), hit.chunk);
            }
            lists.push(ids);
        }

        let borrowed: Vec<&[ChunkId]> = lists.iter().map(Vec::as_slice).collect();
        Ok(rrf(&borrowed, self.params)
            .into_iter()
            .filter_map(|(id, score)| chunks.remove(&id).map(|chunk| ScoredChunk { chunk, score }))
            .take(params.top_k)
            .collect())
    }
}

impl ActivityIndex for HybridRetriever<'_> {
    /// Not supported: a retriever that borrows its children cannot take
    /// ownership of a new chunk. Build the children, write to them, then
    /// construct the retriever.
    fn upsert(&mut self, _chunk: Arc<ltmx_domain::ActivityChunk>) -> Result<(), LtmxError> {
        Err(LtmxError::InvalidQuery(
            "HybridRetriever borrows its children; upsert the underlying indexes instead".into(),
        ))
    }

    fn upsert_all(&mut self, _chunks: &[Arc<ltmx_domain::ActivityChunk>]) -> Result<(), LtmxError> {
        Err(LtmxError::InvalidQuery(
            "HybridRetriever borrows its children; upsert the underlying indexes instead".into(),
        ))
    }

    fn get(&self, id: &ChunkId) -> Result<Arc<ltmx_domain::ActivityChunk>, LtmxError> {
        for child in &self.children {
            if let Ok(chunk) = child.get(id) {
                return Ok(chunk);
            }
        }
        Err(LtmxError::NotFound(id.clone()))
    }

    /// Reports the largest child index.
    ///
    /// Children hold the same chunks, so their counts agree; the maximum is
    /// used so a child that failed to ingest something is still visible rather
    /// than silently hidden.
    fn len(&self) -> usize {
        self.children.iter().map(|c| c.len()).max().unwrap_or(0)
    }

    fn search(&self, query: &str, params: &SearchParams) -> Result<Vec<ScoredChunk>, LtmxError> {
        self.search(query, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ltmx_domain::{ActivityChunk, Origin, SourceRef};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn chunk(id: &str, text: &str) -> Arc<ActivityChunk> {
        ActivityChunk::new(
            ChunkId::new(id),
            text,
            Origin::Note,
            SourceRef::file(format!("/n/{id}.md")),
            at(1_000),
        )
        .shared()
    }

    /// A trivial index that returns a fixed ranking, for testing fusion alone.
    struct RankedIndex {
        chunks: HashMap<ChunkId, Arc<ActivityChunk>>,
        ranking: Vec<ChunkId>,
    }

    impl RankedIndex {
        fn new(ranking: &[&str]) -> Self {
            let mut chunks = HashMap::new();
            for id in ranking {
                chunks.insert(ChunkId::new(*id), chunk(id, &format!("content for {id}")));
            }
            Self {
                chunks,
                ranking: ranking.iter().map(|id| ChunkId::new(*id)).collect(),
            }
        }
    }

    impl ActivityIndex for RankedIndex {
        fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError> {
            self.chunks.insert(chunk.id.clone(), chunk);
            Ok(())
        }
        fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError> {
            for chunk in chunks {
                self.upsert(Arc::clone(chunk))?;
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
        fn search(
            &self,
            _query: &str,
            params: &SearchParams,
        ) -> Result<Vec<ScoredChunk>, LtmxError> {
            Ok(self
                .ranking
                .iter()
                .filter_map(|id| {
                    self.chunks
                        .get(id)
                        .filter(|c| params.admits(c))
                        .map(|c| ScoredChunk {
                            chunk: c.clone(),
                            score: 1.0,
                        })
                })
                .take(params.top_k)
                .collect())
        }
    }

    #[test]
    fn empty_input_fuses_to_nothing() {
        assert!(rrf(&[], RrfParams::default()).is_empty());
    }

    #[test]
    fn single_list_preserves_its_order() {
        let lists: [&[ChunkId]; 1] = [&[ChunkId::new("x"), ChunkId::new("y")]];
        let fused = rrf(&lists, RrfParams::default());
        assert_eq!(fused[0].0, ChunkId::new("x"));
        assert_eq!(fused[1].0, ChunkId::new("y"));
    }

    #[test]
    fn agreement_beats_a_single_strong_list() {
        // "a" is top of the first list only; "c" is second in both.
        let lists: [&[ChunkId]; 2] = [
            &[ChunkId::new("a"), ChunkId::new("c")],
            &[ChunkId::new("b"), ChunkId::new("c")],
        ];
        let fused = rrf(&lists, RrfParams::default());
        assert_eq!(
            fused[0].0,
            ChunkId::new("c"),
            "a chunk both lists like should win"
        );
    }

    #[test]
    fn duplicate_ids_within_one_list_accumulate() {
        let lists: [&[ChunkId]; 1] = [&[ChunkId::new("x"), ChunkId::new("x")]];
        let fused = rrf(&lists, RrfParams::default());
        assert_eq!(fused.len(), 1, "ids must be deduplicated");
    }

    #[test]
    fn larger_k_flattens_rank_differences() {
        let lists: [&[ChunkId]; 1] = [&[ChunkId::new("x"), ChunkId::new("y")]];
        let sharp = rrf(&lists, RrfParams { k: 1.0 });
        let flat = rrf(&lists, RrfParams { k: 1000.0 });
        let sharp_gap = sharp[0].1 - sharp[1].1;
        let flat_gap = flat[0].1 - flat[1].1;
        assert!(flat_gap < sharp_gap, "larger k should compress differences");
    }

    #[test]
    fn retriever_fuses_two_indexes() {
        let first = RankedIndex::new(&["a", "shared"]);
        let second = RankedIndex::new(&["b", "shared"]);
        let retriever = HybridRetriever::new(vec![
            &first as &dyn ActivityIndex,
            &second as &dyn ActivityIndex,
        ]);
        let hits = retriever
            .search("q", &SearchParams::default().with_top_k(3))
            .expect("search should succeed");
        assert_eq!(hits[0].chunk.id, ChunkId::new("shared"));
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn retriever_with_no_children_returns_nothing() {
        let retriever = HybridRetriever::new(vec![]);
        assert!(retriever.is_empty());
        assert_eq!(retriever.index_count(), 0);
        assert!(
            retriever
                .search("q", &SearchParams::default())
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[test]
    fn retriever_index_count_is_not_chunk_count() {
        let first = RankedIndex::new(&["a", "b", "c"]);
        let second = RankedIndex::new(&["a", "b", "c"]);
        let retriever = HybridRetriever::new(vec![
            &first as &dyn ActivityIndex,
            &second as &dyn ActivityIndex,
        ]);
        assert_eq!(retriever.index_count(), 2);
        assert_eq!(
            ActivityIndex::len(&retriever),
            3,
            "len() must count chunks, not indexes"
        );
    }

    #[test]
    fn retriever_honors_top_k() {
        let child = RankedIndex::new(&["a", "b", "c", "d", "e", "f"]);
        let retriever = HybridRetriever::new(vec![&child as &dyn ActivityIndex]);
        let hits = retriever
            .search("q", &SearchParams::default().with_top_k(2))
            .expect("search should succeed");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn retriever_zero_top_k_returns_nothing() {
        let child = RankedIndex::new(&["a"]);
        let retriever = HybridRetriever::new(vec![&child as &dyn ActivityIndex]);
        assert!(
            retriever
                .search("q", &SearchParams::default().with_top_k(0))
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[test]
    fn retriever_scores_are_descending() {
        let first = RankedIndex::new(&["a", "shared", "z"]);
        let second = RankedIndex::new(&["b", "shared", "y"]);
        let retriever = HybridRetriever::new(vec![
            &first as &dyn ActivityIndex,
            &second as &dyn ActivityIndex,
        ]);
        let hits = retriever
            .search("q", &SearchParams::default().with_top_k(5))
            .expect("search should succeed");
        for pair in hits.windows(2) {
            assert!(pair[0].score >= pair[1].score);
        }
    }

    #[test]
    fn retriever_rejects_writes_rather_than_silently_dropping_them() {
        let child = RankedIndex::new(&["a"]);
        let mut retriever = HybridRetriever::new(vec![&child as &dyn ActivityIndex]);
        let result = retriever.upsert(chunk("b", "text"));
        assert!(
            matches!(result, Err(LtmxError::InvalidQuery(_))),
            "a borrowing retriever must refuse writes, not accept and discard them"
        );
    }

    #[test]
    fn retriever_propagates_origin_filter_to_children() {
        let mut git = chunk("g", "docker desktop");
        Arc::make_mut(&mut git).origin = Origin::Git;
        let batch = vec![git, chunk("n", "docker notes")];

        // The retriever borrows, so writes go to the child it wraps.
        let mut child = RankedIndex::new(&["g", "n"]);
        child.upsert_all(&batch).expect("upsert");

        let retriever = HybridRetriever::new(vec![&child as &dyn ActivityIndex]);
        let hits = retriever
            .search(
                "docker",
                &SearchParams::default()
                    .with_top_k(5)
                    .with_origin(Origin::Git),
            )
            .expect("search should succeed");
        assert!(hits.iter().all(|h| h.chunk.origin == Origin::Git));
    }

    #[test]
    fn retriever_get_reports_not_found_across_children() {
        let child = RankedIndex::new(&["a"]);
        let retriever = HybridRetriever::new(vec![&child as &dyn ActivityIndex]);
        assert!(matches!(
            retriever.get(&ChunkId::new("absent")),
            Err(LtmxError::NotFound(_))
        ));
    }
}
