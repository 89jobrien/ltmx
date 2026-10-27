//! Value types describing reconstructed activity.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Stable identifier for a chunk.
///
/// Derived from the source location rather than generated randomly, so that
/// re-ingesting the same source produces the same id and indexing is
/// idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkId(String);

impl ChunkId {
    /// Builds a chunk id from a stable string key.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChunkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a chunk came from.
///
/// This is the axis that lets one query span code notes and a decision made in
/// a terminal three weeks later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A coding-agent transcript (`~/.claude/projects/*.jsonl`, Codex, etc).
    AgentTranscript,
    /// Git history in a local repository.
    Git,
    /// A markdown note, typically in an Obsidian vault.
    Note,
    /// Extracted from captured screen content.
    Screen,
    /// Anything the caller supplies directly.
    Manual,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Origin {
    /// Every origin, in declaration order.
    pub const ALL: [Origin; 5] = [
        Origin::AgentTranscript,
        Origin::Git,
        Origin::Note,
        Origin::Screen,
        Origin::Manual,
    ];

    /// Stable storage name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AgentTranscript => "agent_transcript",
            Self::Git => "git",
            Self::Note => "note",
            Self::Screen => "screen",
            Self::Manual => "manual",
        }
    }

    /// Bit position in an [`OriginSet`].
    const fn bit(&self) -> u8 {
        match self {
            Self::AgentTranscript => 1 << 0,
            Self::Git => 1 << 1,
            Self::Note => 1 << 2,
            Self::Screen => 1 << 3,
            Self::Manual => 1 << 4,
        }
    }
}

/// A set of origins.
///
/// A bitmask rather than a `Vec<Origin>`: the type has a fixed, small number of
/// variants, so membership is one bit test with no allocation on the query
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct OriginSet(u8);

impl OriginSet {
    /// The empty set, which admits every origin.
    pub const EMPTY: OriginSet = OriginSet(0);

    /// The set containing every origin.
    pub const ALL: OriginSet = OriginSet(0b11111);

    /// Builds a set from anything iterable of origins.
    pub fn new<I: IntoIterator<Item = Origin>>(origins: I) -> Self {
        let mut bits = 0u8;
        for origin in origins {
            bits |= origin.bit();
        }
        OriginSet(bits)
    }

    /// Adds `origin`, returning the updated set.
    pub fn with(mut self, origin: Origin) -> Self {
        self.0 |= origin.bit();
        self
    }

    /// Reports whether `origin` is a member.
    pub fn contains(&self, origin: Origin) -> bool {
        self.0 & origin.bit() != 0
    }

    /// Reports whether the set is empty.
    ///
    /// An empty set is treated as "no filter", which is why
    /// [`SearchParams::admits`] allows everything in that case.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Returns the members in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = Origin> {
        Origin::ALL.into_iter().filter(|o| self.contains(*o))
    }
}

/// The durable artifact a chunk was derived from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// Path on disk, or a synthetic path such as a git `HEAD` ref.
    pub path: String,
    /// Line span within `path`, when the source is line-addressable.
    pub line_range: Option<(u32, u32)>,
}

impl SourceRef {
    /// Builds a source reference for a whole file.
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line_range: None,
        }
    }

    /// Builds a source reference for a line span within a file.
    pub fn lines(path: impl Into<String>, start: u32, end: u32) -> Self {
        Self {
            path: path.into(),
            line_range: Some((start, end)),
        }
    }

    /// Returns a `path:start-end` citation, or just the path when the source
    /// has no line span.
    pub fn citation(&self) -> String {
        match self.line_range {
            Some((start, end)) => format!("{}:{}-{}", self.path, start, end),
            None => self.path.clone(),
        }
    }
}

/// Groups chunks that belong to one continuous stretch of work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Builds a session id from a stable string key.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An inclusive time span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeRange {
    /// Builds a time range, normalizing endpoints so `start <= end`.
    pub fn new(a: DateTime<Utc>, b: DateTime<Utc>) -> Self {
        if a <= b {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    /// Reports whether `other` overlaps this range.
    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// Reports whether `at` falls inside this range.
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.start <= at && at <= self.end
    }
}

/// Extra signals that improve ranking without being part of the text.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoringSignals {
    /// Commit recency score in `0.0..=1.0`, where newer is higher.
    pub recency: f32,
    /// Emphasized term matches, e.g. a path hit inside a code chunk.
    pub term_overlap: u32,
}

/// Backwards-compatible alias for [`ScoringSignals`].
pub type Provenance = ScoringSignals;

/// One retrievable unit of reconstructed work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityChunk {
    pub id: ChunkId,
    pub text: String,
    pub origin: Origin,
    pub source: SourceRef,
    pub occurred_at: DateTime<Utc>,
    pub session: Option<SessionId>,
    pub provenance: ScoringSignals,
}

impl ActivityChunk {
    /// Builds a chunk with zeroed provenance signals.
    pub fn new(
        id: ChunkId,
        text: impl Into<String>,
        origin: Origin,
        source: SourceRef,
        occurred_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            text: text.into(),
            origin,
            source,
            occurred_at,
            session: None,
            provenance: ScoringSignals {
                recency: 0.0,
                term_overlap: 0,
            },
        }
    }

    /// Wraps this chunk for sharing between indexes.
    ///
    /// Several indexes are built over the same corpus, and a transcript chunk
    /// owns its text. Cloning the corpus per index is expensive; sharing the
    /// allocation is not.
    pub fn shared(self) -> Arc<ActivityChunk> {
        Arc::new(self)
    }

    /// Attaches a session to the chunk.
    pub fn with_session(mut self, session: SessionId) -> Self {
        self.session = Some(session);
        self
    }

    /// Returns a `path:line` citation for this chunk.
    pub fn citation(&self) -> String {
        self.source.citation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn chunk_id_renders() {
        assert_eq!(ChunkId::new("a/b#12").to_string(), "a/b#12");
        assert_eq!(ChunkId::new("a/b#12").as_str(), "a/b#12");
    }

    #[test]
    fn origin_renders_as_snake_case() {
        assert_eq!(Origin::AgentTranscript.to_string(), "agent_transcript");
        assert_eq!(Origin::Screen.to_string(), "screen");
    }

    #[test]
    fn source_ref_citation_formats_line_span() {
        let src = SourceRef::lines("/repo/main.rs", 10, 42);
        assert_eq!(src.citation(), "/repo/main.rs:10-42");
    }

    #[test]
    fn source_ref_citation_omits_absent_line_span() {
        let src = SourceRef::file("/repo/README.md");
        assert_eq!(src.citation(), "/repo/README.md");
    }

    #[test]
    fn chunk_citation_delegates_to_source() {
        let chunk = ActivityChunk::new(
            ChunkId::new("c1"),
            "decided to use vec0",
            Origin::Note,
            SourceRef::lines("/vault/DECISIONS.md", 3, 7),
            at(0),
        );
        assert_eq!(chunk.citation(), "/vault/DECISIONS.md:3-7");
    }

    #[test]
    fn chunk_defaults_provenance_to_zero() {
        let chunk = ActivityChunk::new(
            ChunkId::new("c1"),
            "text",
            Origin::Git,
            SourceRef::file("/repo"),
            at(0),
        );
        assert_eq!(chunk.provenance.recency, 0.0);
        assert_eq!(chunk.provenance.term_overlap, 0);
        assert!(chunk.session.is_none());
    }

    #[test]
    fn time_range_normalizes_reversed_endpoints() {
        let r = TimeRange::new(at(200), at(100));
        assert_eq!(r.start, at(100));
        assert_eq!(r.end, at(200));
    }

    #[test]
    fn time_range_overlaps_on_intersection_only() {
        let a = TimeRange::new(at(0), at(100));
        assert!(a.overlaps(&TimeRange::new(at(50), at(150))));
        assert!(a.overlaps(&TimeRange::new(at(50), at(60))));
        assert!(!a.overlaps(&TimeRange::new(at(101), at(200))));
    }

    #[test]
    fn time_range_contains_is_inclusive() {
        let r = TimeRange::new(at(0), at(100));
        assert!(r.contains(at(0)));
        assert!(r.contains(at(100)));
        assert!(!r.contains(at(101)));
    }
}
