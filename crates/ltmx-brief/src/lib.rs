//! Generates evidence-backed periodic briefs over an ltmx corpus.
//!
//! This crate is deliberately *not* a retrieval adapter. A brief has no
//! query — "what happened since yesterday" is not a lexical or semantic
//! match problem, it is a filter-and-summarize problem — so evidence here is
//! selected directly from a chunk slice via [`SearchParams::admits`], never
//! through [`ltmx_domain::ActivityIndex::search`]. Routing an empty query
//! through BM25 or dense cosine would hit the "no lexical signal" contract
//! clause and silently return nothing, or score an unrelated embedding of
//! `""` as if it meant something.
//!
//! Keeping generation in its own crate, rather than in `ltmx-domain` or a
//! retrieval crate, is what lets the "evidence, not generation" principle
//! hold for the memory engine itself: `ltmx-domain` and its adapters stay
//! retrieval-only, and summarization is a consumer built on top, same as any
//! other caller of the ports.

pub mod ollama;

pub use ollama::OllamaGenerator;

use chrono::{DateTime, Duration, Utc};
use ltmx_domain::{ActivityChunk, Origin, SearchParams, TimeRange};
use std::sync::Arc;

/// Which kind of brief to produce.
///
/// Each kind is a different evidence-selection preset over the *same*
/// generation pipeline — there is no per-kind code path, only a different
/// scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BriefKind {
    /// Last 24h, every origin, no path scoping — "what happened since
    /// yesterday."
    Morning,
    /// Git history and agent-transcript chunks, scoped to work project
    /// paths.
    Professional,
    /// Note/manual chunks (and transcript chunks outside work paths).
    Personal,
}

impl BriefKind {
    /// A short label for prompts and output headers.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Morning => "Morning Brief",
            Self::Professional => "Professional Brief",
            Self::Personal => "Personal Brief",
        }
    }
}

/// Builds the origin+window filter for `kind` over `window`.
///
/// This `SearchParams` is never handed to an `ActivityIndex` — only its
/// [`SearchParams::admits`] is used, by [`select_evidence`], to filter a
/// chunk slice directly.
pub fn filter_for(kind: BriefKind, window: TimeRange) -> SearchParams {
    let base = SearchParams::default().with_window(window);
    match kind {
        BriefKind::Morning => base,
        BriefKind::Professional => base.with_origins([Origin::Git, Origin::AgentTranscript]),
        BriefKind::Personal => base.with_origins([Origin::Note, Origin::Manual]),
    }
}

/// The default window for a brief: the last 24 hours up to `now`.
pub fn last_24h(now: DateTime<Utc>) -> TimeRange {
    TimeRange::new(now - Duration::hours(24), now)
}

/// Selects the evidence for a brief: every chunk `filter` admits, newest
/// first, truncated to `top_k`.
///
/// Recency, not relevance, is the ranking — there is no query to score
/// against.
pub fn select_evidence(
    chunks: &[Arc<ActivityChunk>],
    filter: &SearchParams,
    top_k: usize,
) -> Vec<Arc<ActivityChunk>> {
    let mut admitted: Vec<Arc<ActivityChunk>> = chunks
        .iter()
        .filter(|chunk| filter.admits(chunk))
        .cloned()
        .collect();
    admitted.sort_by_key(|chunk| std::cmp::Reverse(chunk.occurred_at));
    admitted.truncate(top_k);
    admitted
}

/// Narrows `chunks` to paths under (or outside) `work_prefix`.
///
/// `SearchParams` has no path filter — only origin and time window — so this
/// is a second pass in the caller, not a domain-port change.
/// [`BriefKind::Morning`] is unscoped and returns `chunks` unchanged.
pub fn filter_by_path(
    chunks: Vec<Arc<ActivityChunk>>,
    kind: BriefKind,
    work_prefix: &str,
) -> Vec<Arc<ActivityChunk>> {
    match kind {
        BriefKind::Morning => chunks,
        BriefKind::Professional => chunks
            .into_iter()
            .filter(|chunk| chunk.source.path.starts_with(work_prefix))
            .collect(),
        BriefKind::Personal => chunks
            .into_iter()
            .filter(|chunk| !chunk.source.path.starts_with(work_prefix))
            .collect(),
    }
}

/// Errors raised while generating a brief.
///
/// Distinct from [`ltmx_domain::EmbedError`]: that type's messages say
/// "embedding backend", which would misdescribe a generation failure even
/// though the transport (`post_json`) is shared.
#[derive(Debug, thiserror::Error)]
pub enum BriefError {
    /// The generation backend could not be reached.
    #[error("generation backend unreachable at {endpoint}: {reason}")]
    Unreachable {
        /// `host:port` or URL that was attempted.
        endpoint: String,
        /// Underlying transport failure.
        reason: String,
    },
    /// The backend replied with a non-success status.
    #[error("generation backend returned {status}: {body}")]
    Status {
        /// HTTP status code returned by the backend.
        status: u16,
        /// Response body, truncated by the adapter.
        body: String,
    },
    /// The reply could not be decoded.
    #[error("malformed generation response: {0}")]
    Decode(String),
}

impl From<ltmx_domain::EmbedError> for BriefError {
    fn from(error: ltmx_domain::EmbedError) -> Self {
        match error {
            ltmx_domain::EmbedError::Unreachable { endpoint, reason } => {
                Self::Unreachable { endpoint, reason }
            }
            ltmx_domain::EmbedError::Status { status, body } => Self::Status { status, body },
            ltmx_domain::EmbedError::Decode(message) => Self::Decode(message),
            // `post_json` (the transport shared with the embedding adapter)
            // only ever returns the three variants matched above for a
            // `/api/generate` call, so this arm cannot fire today. It is kept
            // as a catch-all rather than an exhaustive match so a future
            // `EmbedError` variant doesn't break this `From` impl; if
            // `post_json`'s error surface grows for a reason unrelated to
            // decoding, this will mislabel it as `Decode` until revisited.
            other => Self::Decode(other.to_string()),
        }
    }
}

/// Builds the prompt sent to the generator: a brief label, the window, and
/// one citation + text excerpt per piece of evidence.
///
/// Evidence, not instruction: the model is asked to summarize what is given,
/// not to recall anything on its own, which is what keeps the brief
/// traceable back to `path:line` citations.
pub fn build_prompt(kind: BriefKind, window: TimeRange, evidence: &[Arc<ActivityChunk>]) -> String {
    let mut prompt = format!(
        "Write a {label} summarizing the activity below, which occurred between \
         {start} and {end}. Use only the evidence given; cite the source path for \
         each point you make. If the evidence is sparse, say so rather than inventing \
         detail.\n\n",
        label = kind.label(),
        start = window.start.to_rfc3339(),
        end = window.end.to_rfc3339(),
    );
    if evidence.is_empty() {
        prompt.push_str("(no evidence found in this window)\n");
        return prompt;
    }
    for chunk in evidence {
        let excerpt: String = chunk.text.chars().take(400).collect();
        prompt.push_str(&format!(
            "- [{}] {}\n",
            chunk.citation(),
            excerpt.replace('\n', " ")
        ));
    }
    prompt
}

/// Generates a brief's prose from already-selected evidence.
///
/// Thin orchestration: build the prompt, call the generator. Kept as a free
/// function (rather than a method on [`OllamaGenerator`]) so a future
/// generator backend only needs to implement `generate(&self, &str)`, not
/// this crate's prompt-building policy.
pub fn generate_brief(
    generator: &OllamaGenerator,
    kind: BriefKind,
    window: TimeRange,
    evidence: &[Arc<ActivityChunk>],
) -> Result<String, BriefError> {
    generator.generate(&build_prompt(kind, window, evidence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ltmx_domain::{ChunkId, SourceRef};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn chunk(path: &str, origin: Origin, secs: i64) -> Arc<ActivityChunk> {
        Arc::new(ActivityChunk::new(
            ChunkId::new(path),
            "text",
            origin,
            SourceRef::file(path),
            at(secs),
        ))
    }

    #[test]
    fn morning_filter_is_unrestricted_by_origin() {
        let filter = filter_for(BriefKind::Morning, last_24h(at(100_000)));
        assert!(filter.origins.is_empty());
    }

    #[test]
    fn professional_filter_restricts_to_work_origins() {
        let filter = filter_for(BriefKind::Professional, last_24h(at(100_000)));
        assert!(filter.origins.contains(Origin::Git));
        assert!(filter.origins.contains(Origin::AgentTranscript));
        assert!(!filter.origins.contains(Origin::Note));
    }

    #[test]
    fn personal_filter_restricts_to_personal_origins() {
        let filter = filter_for(BriefKind::Personal, last_24h(at(100_000)));
        assert!(filter.origins.contains(Origin::Note));
        assert!(filter.origins.contains(Origin::Manual));
        assert!(!filter.origins.contains(Origin::Git));
    }

    #[test]
    fn last_24h_spans_exactly_one_day_ending_at_now() {
        let now = at(100_000);
        let window = last_24h(now);
        assert_eq!(window.end, now);
        assert_eq!((window.end - window.start).num_hours(), 24);
    }

    #[test]
    fn select_evidence_orders_newest_first_and_truncates() {
        let chunks = vec![
            chunk("/a", Origin::Git, 100),
            chunk("/b", Origin::Git, 300),
            chunk("/c", Origin::Git, 200),
        ];
        let filter = SearchParams::default().with_window(TimeRange::new(at(0), at(400)));
        let evidence = select_evidence(&chunks, &filter, 2);
        assert_eq!(evidence.len(), 2);
        assert_eq!(evidence[0].occurred_at, at(300));
        assert_eq!(evidence[1].occurred_at, at(200));
    }

    #[test]
    fn select_evidence_drops_chunks_outside_the_window() {
        let chunks = vec![chunk("/a", Origin::Git, 50)];
        let filter = SearchParams::default().with_window(TimeRange::new(at(100), at(200)));
        assert!(select_evidence(&chunks, &filter, 10).is_empty());
    }

    #[test]
    fn filter_by_path_keeps_work_paths_for_professional() {
        let chunks = vec![
            chunk("/Users/joe/dev/ltmx/src/lib.rs", Origin::Git, 0),
            chunk("/Users/joe/notes/journal.md", Origin::Note, 0),
        ];
        let filtered = filter_by_path(chunks, BriefKind::Professional, "/Users/joe/dev");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source.path, "/Users/joe/dev/ltmx/src/lib.rs");
    }

    #[test]
    fn filter_by_path_excludes_work_paths_for_personal() {
        let chunks = vec![
            chunk("/Users/joe/dev/ltmx/src/lib.rs", Origin::Git, 0),
            chunk("/Users/joe/notes/journal.md", Origin::Note, 0),
        ];
        let filtered = filter_by_path(chunks, BriefKind::Personal, "/Users/joe/dev");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source.path, "/Users/joe/notes/journal.md");
    }

    #[test]
    fn filter_by_path_leaves_morning_untouched() {
        let chunks = vec![
            chunk("/Users/joe/dev/ltmx/src/lib.rs", Origin::Git, 0),
            chunk("/Users/joe/notes/journal.md", Origin::Note, 0),
        ];
        let filtered = filter_by_path(chunks, BriefKind::Morning, "/Users/joe/dev");
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn prompt_cites_every_piece_of_evidence() {
        let evidence = vec![chunk("/x/y.rs", Origin::Git, 0)];
        let prompt = build_prompt(BriefKind::Professional, last_24h(at(100_000)), &evidence);
        assert!(prompt.contains("Professional Brief"));
        assert!(prompt.contains(&evidence[0].citation()));
    }

    #[test]
    fn path_filter_before_truncation_still_fills_top_k() {
        // Regression test for the ordering bug: `main.rs` must apply
        // `filter_by_path` to the full chunk pool *before* `select_evidence`
        // truncates to `top_k`, or a path-admitted chunk sitting behind
        // excluded ones gets discarded before the path filter ever sees it.
        let chunks = vec![
            chunk("/Users/joe/notes/a.md", Origin::Note, 400),
            chunk("/Users/joe/notes/b.md", Origin::Note, 300),
            chunk("/Users/joe/dev/ltmx/src/lib.rs", Origin::Git, 200),
            chunk("/Users/joe/dev/ltmx/src/main.rs", Origin::Git, 100),
        ];
        let window = TimeRange::new(at(0), at(500));
        let kind = BriefKind::Professional;
        let filter = filter_for(kind, window);

        let scoped = filter_by_path(chunks, kind, "/Users/joe/dev");
        let evidence = select_evidence(&scoped, &filter, 2);

        assert_eq!(evidence.len(), 2);
        assert!(
            evidence
                .iter()
                .all(|chunk| chunk.source.path.starts_with("/Users/joe/dev"))
        );
    }

    #[test]
    fn empty_evidence_says_so_rather_than_fabricating_a_prompt_body() {
        let prompt = build_prompt(BriefKind::Morning, last_24h(at(100_000)), &[]);
        assert!(prompt.contains("no evidence found"));
    }
}
