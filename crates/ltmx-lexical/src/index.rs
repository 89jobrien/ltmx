//! In-memory BM25 [`ActivityIndex`] adapter.

use super::scoring::{Bm25Params, bm25};
use super::tokenize::tokenize;
use ltmx_domain::{ActivityChunk, ActivityIndex, ChunkId, LtmxError, ScoredChunk, SearchParams};
use std::collections::HashMap;
use std::sync::Arc;

/// An in-memory BM25 index over activity chunks.
#[derive(Debug, Clone)]
pub struct LexicalIndex {
    /// Chunk id to the chunk itself.
    chunks: HashMap<ChunkId, Arc<ActivityChunk>>,
    /// Chunk id to its term-frequency map.
    postings: HashMap<ChunkId, HashMap<String, u32>>,
    /// Chunk id to token count.
    lengths: HashMap<ChunkId, u32>,
    params: Bm25Params,
}

impl LexicalIndex {
    /// Builds an empty index with default scoring parameters.
    pub fn new() -> Self {
        Self::with_params(Bm25Params::default())
    }

    /// Builds an empty index with explicit scoring parameters.
    pub fn with_params(params: Bm25Params) -> Self {
        Self {
            chunks: HashMap::new(),
            postings: HashMap::new(),
            lengths: HashMap::new(),
            params,
        }
    }

    /// Mean token count across the corpus, or `0.0` when empty.
    fn average_length(&self) -> f32 {
        if self.lengths.is_empty() {
            return 0.0;
        }
        let total: u32 = self.lengths.values().sum();
        total as f32 / self.lengths.len() as f32
    }

    /// Number of indexed chunks containing `term`.
    fn document_frequency(&self, term: &str) -> u32 {
        self.postings
            .values()
            .filter(|tokens| tokens.contains_key(term))
            .count() as u32
    }

    fn index_chunk(&mut self, chunk: Arc<ActivityChunk>) {
        let mut tokens: HashMap<String, u32> = HashMap::new();
        let mut length = 0u32;
        for term in tokenize(&chunk.text) {
            *tokens.entry(term).or_insert(0) += 1;
            length += 1;
        }
        self.lengths.insert(chunk.id.clone(), length);
        self.postings.insert(chunk.id.clone(), tokens);
        self.chunks.insert(chunk.id.clone(), chunk);
    }
}

impl Default for LexicalIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityIndex for LexicalIndex {
    fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError> {
        self.index_chunk(chunk);
        Ok(())
    }

    fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError> {
        for chunk in chunks {
            self.index_chunk(Arc::clone(chunk));
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
        if params.top_k == 0 {
            return Ok(Vec::new());
        }
        let query_terms = tokenize(query);
        if query_terms.is_empty() || self.chunks.is_empty() {
            return Ok(Vec::new());
        }

        let corpus_size = self.chunks.len() as f32;
        let avg_len = self.average_length();
        if avg_len == 0.0 {
            return Ok(Vec::new());
        }

        // Resolve document frequencies once rather than per scored chunk.
        let dfs: HashMap<&str, u32> = query_terms
            .iter()
            .map(|term| (term.as_str(), self.document_frequency(term)))
            .collect();
        let doc_freq = |term: &str| *dfs.get(term).unwrap_or(&0);

        let mut scored: Vec<ScoredChunk> = self
            .chunks
            .values()
            .filter(|chunk| params.admits(chunk))
            .filter_map(|chunk| {
                let tokens = self.postings.get(&chunk.id)?;
                let length = *self.lengths.get(&chunk.id)?;
                let score = bm25(
                    &query_terms,
                    tokens,
                    length,
                    avg_len,
                    corpus_size,
                    doc_freq,
                    self.params,
                );
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
    use chrono::{DateTime, Utc};
    use ltmx_domain::{Origin, SourceRef, TimeRange};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn chunk(id: &str, text: &str, secs: i64) -> Arc<ActivityChunk> {
        ActivityChunk::new(
            ChunkId::new(id),
            text,
            Origin::Note,
            SourceRef::file(format!("/notes/{id}.md")),
            at(secs),
        )
        .shared()
    }

    fn index_with(chunks: Vec<Arc<ActivityChunk>>) -> LexicalIndex {
        let mut index = LexicalIndex::new();
        index.upsert_all(&chunks).expect("upsert should succeed");
        index
    }

    #[test]
    fn empty_index_reports_empty() {
        let index = LexicalIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn upsert_populates_the_index() {
        let index = index_with(vec![chunk("a", "vec0 storage", 0)]);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn upsert_is_idempotent_by_id() {
        let mut index = index_with(vec![chunk("a", "vec0 storage", 0)]);
        index
            .upsert(chunk("a", "vec0 storage revised", 0))
            .expect("re-upsert");
        assert_eq!(index.len(), 1);
        assert_eq!(
            index.get(&ChunkId::new("a")).expect("get").text,
            "vec0 storage revised"
        );
    }

    #[test]
    fn repeated_upsert_does_not_double_count_terms() {
        let mut index = LexicalIndex::new();
        for _ in 0..3 {
            index
                .upsert(chunk("a", "docker docker docker", 0))
                .expect("upsert");
        }
        assert_eq!(index.len(), 1);
        let hits = index
            .search("docker", &SearchParams::default())
            .expect("search should succeed");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn get_returns_not_found_for_unknown_id() {
        assert!(matches!(
            LexicalIndex::new().get(&ChunkId::new("missing")),
            Err(LtmxError::NotFound(_))
        ));
    }

    #[test]
    fn search_ranks_the_matching_chunk_first() {
        let index = index_with(vec![
            chunk("a", "colima replaced docker desktop", 0),
            chunk("b", "we configured the gitea runner", 0),
        ]);
        let hits = index
            .search("docker desktop", &SearchParams::default())
            .expect("search should succeed");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].chunk.id, ChunkId::new("a"));
    }

    #[test]
    fn search_returns_nothing_when_no_term_matches() {
        let index = index_with(vec![chunk("a", "colima replaced docker", 0)]);
        let hits = index
            .search("kubernetes ingress", &SearchParams::default())
            .expect("search should succeed");
        assert!(hits.is_empty());
    }

    #[test]
    fn search_respects_top_k() {
        let index = index_with(vec![
            chunk("a", "docker desktop colima", 0),
            chunk("b", "docker desktop podman", 0),
            chunk("c", "docker desktop rancher", 0),
        ]);
        let hits = index
            .search("docker", &SearchParams::default().with_top_k(2))
            .expect("search should succeed");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn search_with_top_k_zero_returns_nothing() {
        let index = index_with(vec![chunk("a", "docker", 0)]);
        assert!(
            index
                .search("docker", &SearchParams::default().with_top_k(0))
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[test]
    fn search_on_empty_index_returns_nothing() {
        assert!(
            LexicalIndex::new()
                .search("docker", &SearchParams::default())
                .expect("search should succeed")
                .is_empty()
        );
    }

    #[test]
    fn search_honors_origin_filter() {
        let mut git = chunk("a", "docker desktop migration", 0);
        Arc::make_mut(&mut git).origin = Origin::Git;
        let index = index_with(vec![chunk("b", "docker desktop notes", 0), git]);
        let hits = index
            .search("docker", &SearchParams::default().with_origin(Origin::Git))
            .expect("search should succeed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.origin, Origin::Git);
    }

    #[test]
    fn search_honors_time_window() {
        let index = index_with(vec![
            chunk("old", "docker desktop migration", 100),
            chunk("new", "docker desktop migration", 9_000),
        ]);
        let params = SearchParams::default().with_window(TimeRange::new(at(8_000), at(10_000)));
        let hits = index
            .search("docker", &params)
            .expect("search should succeed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.id, ChunkId::new("new"));
    }

    // ── Conformance: the ActivityIndex port contract ─────────────

    #[test]
    fn lexical_index_satisfies_activity_index_contract() {
        ltmx_domain::ports::conformance::assert_activity_index_contract(LexicalIndex::new);
    }

    #[test]
    fn scores_are_sorted_descending() {
        let index = index_with(vec![
            chunk("a", "docker", 0),
            chunk("b", "docker docker docker docker", 0),
        ]);
        let hits = index
            .search("docker", &SearchParams::default())
            .expect("search should succeed");
        assert!(hits[0].score >= hits[1].score);
        assert_eq!(hits[0].chunk.id, ChunkId::new("b"));
    }
}
