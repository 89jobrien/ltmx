//! Reusable contract suites for domain ports.
//!
//! These live with the traits rather than with any adapter, so a new adapter
//! gets its contract checked by adding one call in its own test module. That
//! is the point: an adapter that compiles is not necessarily correct.

use super::{ActivityIndex, SearchParams};
use crate::LtmxError;
use crate::types::{ActivityChunk, ChunkId, Origin, SourceRef};
use std::sync::Arc;

/// Asserts that `index` upholds every guarantee on the [`ActivityIndex`] docs.
///
/// A conforming adapter calls this from its own test module:
///
/// ```ignore
/// #[cfg(test)]
/// mod tests {
///     use ltmx_domain::ports::conformance::assert_activity_index_contract;
///
///     #[test]
///     fn lexical_index_satisfies_contract() {
///         assert_activity_index_contract(|| ltmx_lexical::LexicalIndex::new());
///     }
/// }
/// ```
///
/// The factory takes a closure so each check runs against a fresh, empty index
/// and cannot leak state between assertions.
pub fn assert_activity_index_contract<F, I>(new_index: F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    upsert_then_get_roundtrips(&new_index);
    upsert_is_idempotent_on_id(&new_index);
    get_reports_not_found_for_unknown_id(&new_index);
    len_counts_distinct_ids(&new_index);
    search_returns_nothing_when_empty(&new_index);
    search_truncates_to_top_k(&new_index);
    search_never_exceeds_top_k(&new_index);
    search_is_sorted_descending(&new_index);
    search_returns_nothing_for_zero_top_k(&new_index);
    search_excludes_filtered_origins(&new_index);
    search_excludes_out_of_window_chunks(&new_index);
}

fn sample(id: &str, text: &str, secs: i64, origin: Origin) -> Arc<ActivityChunk> {
    let at = chrono::DateTime::from_timestamp(secs, 0).expect("valid timestamp");
    ActivityChunk::new(
        ChunkId::new(id),
        text,
        origin,
        SourceRef::file(format!("/src/{id}.md")),
        at,
    )
    .shared()
}

fn upsert_then_get_roundtrips<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    let chunk = sample("a", "colima replaced docker desktop", 1_000, Origin::Note);
    index.upsert(chunk.clone()).expect("upsert should succeed");
    let stored = index.get(&chunk.id).expect("get should succeed");
    assert_eq!(stored.text, chunk.text, "roundtrip must preserve text");
    assert_eq!(
        stored.origin, chunk.origin,
        "roundtrip must preserve origin"
    );
    assert_eq!(
        stored.occurred_at, chunk.occurred_at,
        "roundtrip must preserve timestamp"
    );
}

fn upsert_is_idempotent_on_id<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    index
        .upsert(sample("a", "original text", 1_000, Origin::Note))
        .expect("first upsert");
    index
        .upsert(sample("a", "replacement text", 2_000, Origin::Note))
        .expect("second upsert");
    assert_eq!(index.len(), 1, "rewriting an id must not add a chunk");
    assert_eq!(
        index
            .get(&ChunkId::new("a"))
            .expect("get should succeed")
            .text,
        "replacement text",
        "the second write must win"
    );
}

fn get_reports_not_found_for_unknown_id<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let index = new_index();
    assert!(
        matches!(
            index.get(&ChunkId::new("absent")),
            Err(LtmxError::NotFound(_))
        ),
        "unknown id must yield NotFound"
    );
}

fn len_counts_distinct_ids<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    assert!(index.is_empty(), "a new index must be empty");
    let batch = vec![
        sample("a", "docker desktop", 1_000, Origin::Note),
        sample("b", "colima migration", 2_000, Origin::Note),
        sample("a", "docker desktop again", 3_000, Origin::Note),
    ];
    index.upsert_all(&batch).expect("upsert_all should succeed");
    assert_eq!(index.len(), 2, "len must count distinct ids");
}

fn search_returns_nothing_when_empty<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let index = new_index();
    let hits = index
        .search("docker", &SearchParams::default())
        .expect("search on an empty index must succeed");
    assert!(hits.is_empty(), "an empty index cannot match");
}

fn search_truncates_to_top_k<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    let batch = vec![
        sample("a", "docker desktop", 1_000, Origin::Note),
        sample("b", "docker colima", 1_000, Origin::Note),
        sample("c", "docker podman", 1_000, Origin::Note),
    ];
    index.upsert_all(&batch).expect("upsert_all should succeed");
    let hits = index
        .search("docker", &SearchParams::default().with_top_k(2))
        .expect("search should succeed");
    assert_eq!(hits.len(), 2, "search must honor top_k");
}

fn search_never_exceeds_top_k<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    let batch: Vec<_> = (0..10)
        .map(|n| {
            sample(
                &format!("c{n}"),
                "docker desktop colima",
                1_000,
                Origin::Note,
            )
        })
        .collect();
    index.upsert_all(&batch).expect("upsert_all should succeed");
    for top_k in [1, 3, 7] {
        let hits = index
            .search("docker", &SearchParams::default().with_top_k(top_k))
            .expect("search should succeed");
        assert!(hits.len() <= top_k, "top_k {top_k} returned {}", hits.len());
    }
}

fn search_is_sorted_descending<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    let batch = vec![
        sample("a", "docker", 1_000, Origin::Note),
        sample("b", "docker docker docker", 1_000, Origin::Note),
        sample("c", "docker desktop", 1_000, Origin::Note),
    ];
    index.upsert_all(&batch).expect("upsert_all should succeed");
    let hits = index
        .search("docker", &SearchParams::default().with_top_k(3))
        .expect("search should succeed");
    for pair in hits.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "results must be non-increasing: {} then {}",
            pair[0].score,
            pair[1].score
        );
    }
}

fn search_returns_nothing_for_zero_top_k<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    index
        .upsert(sample("a", "docker desktop", 1_000, Origin::Note))
        .expect("upsert should succeed");
    let hits = index
        .search("docker", &SearchParams::default().with_top_k(0))
        .expect("search should succeed");
    assert!(hits.is_empty(), "top_k of zero must return nothing");
}

fn search_excludes_filtered_origins<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    let batch = vec![
        sample("note", "docker desktop", 1_000, Origin::Note),
        sample("git", "docker desktop", 1_000, Origin::Git),
    ];
    index.upsert_all(&batch).expect("upsert_all should succeed");
    let params = SearchParams::default().with_origin(Origin::Git);
    let hits = index
        .search("docker", &params)
        .expect("search should succeed");
    assert!(!hits.is_empty(), "the git chunk must be reachable");
    assert!(
        hits.iter().all(|hit| hit.chunk.origin == Origin::Git),
        "a filtered search must not leak other origins"
    );
}

fn search_excludes_out_of_window_chunks<F, I>(new_index: &F)
where
    F: Fn() -> I,
    I: ActivityIndex,
{
    let mut index = new_index();
    let batch = vec![
        sample("old", "docker desktop", 1_000, Origin::Note),
        sample("new", "docker desktop", 900_000, Origin::Note),
    ];
    index.upsert_all(&batch).expect("upsert_all should succeed");
    let window = crate::types::TimeRange::new(
        chrono::DateTime::from_timestamp(800_000, 0).expect("valid timestamp"),
        chrono::DateTime::from_timestamp(1_000_000, 0).expect("valid timestamp"),
    );
    let hits = index
        .search("docker", &SearchParams::default().with_window(window))
        .expect("search should succeed");
    assert!(!hits.is_empty(), "the in-window chunk must be reachable");
    assert!(
        hits.iter().all(|hit| hit.chunk.id == ChunkId::new("new")),
        "a windowed search must not leak out-of-window chunks"
    );
}

#[cfg(test)]
mod tests {
    use super::super::{ActivityIndex, ScoredChunk, SearchParams};
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// A deliberately trivial in-memory adapter used to prove the suite itself
    /// detects violations -- a suite that cannot fail is worthless.
    #[derive(Default, Clone)]
    struct TrivialIndex {
        chunks: Rc<RefCell<HashMap<ChunkId, Arc<ActivityChunk>>>>,
    }

    impl TrivialIndex {
        fn new() -> Self {
            Self::default()
        }
    }

    impl ActivityIndex for TrivialIndex {
        fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError> {
            self.chunks.borrow_mut().insert(chunk.id.clone(), chunk);
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
                .borrow()
                .get(id)
                .cloned()
                .ok_or_else(|| LtmxError::NotFound(id.clone()))
        }

        fn len(&self) -> usize {
            self.chunks.borrow().len()
        }

        fn search(
            &self,
            query: &str,
            params: &SearchParams,
        ) -> Result<Vec<ScoredChunk>, LtmxError> {
            let needle = query.to_lowercase();
            let mut hits: Vec<ScoredChunk> = self
                .chunks
                .borrow()
                .values()
                .filter(|chunk| params.admits(chunk))
                .filter(|chunk| chunk.text.to_lowercase().contains(&needle))
                .map(|chunk| ScoredChunk {
                    chunk: chunk.clone(),
                    score: 1.0,
                })
                .collect();
            hits.truncate(params.top_k);
            Ok(hits)
        }
    }

    /// An adapter that deliberately breaks the truncation contract.
    #[derive(Default)]
    struct IgnoresTopK(TrivialIndex);

    impl ActivityIndex for IgnoresTopK {
        fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError> {
            self.0.upsert(chunk)
        }
        fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError> {
            self.0.upsert_all(chunks)
        }
        fn get(&self, id: &ChunkId) -> Result<Arc<ActivityChunk>, LtmxError> {
            self.0.get(id)
        }
        fn len(&self) -> usize {
            self.0.len()
        }
        fn search(
            &self,
            query: &str,
            _params: &SearchParams,
        ) -> Result<Vec<ScoredChunk>, LtmxError> {
            self.0.search(query, &SearchParams::default())
        }
    }

    #[test]
    fn conforming_adapter_passes_the_suite() {
        assert_activity_index_contract(TrivialIndex::new);
    }

    #[test]
    #[should_panic(expected = "top_k")]
    fn suite_detects_an_adapter_that_ignores_top_k() {
        assert_activity_index_contract(IgnoresTopK::default);
    }
}
