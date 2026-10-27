//! Uniform context, reduction logging, and normalization for activity records.

use serde::{Deserialize, Serialize};

/// Window/app context for a chunk, attached uniformly across all origins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Context {
    /// The application that produced the content (e.g. "VS Code", "Terminal").
    pub app: Option<String>,
    /// Window title or tool name.
    pub title: Option<String>,
    /// A locator for the content (file path, command, etc.).
    pub locator: Option<String>,
}

impl Context {
    /// Builds an empty context.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// A single named transformation applied during normalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reduction {
    /// Namespaced name like `drop:too-short` or `normalize:whitespace`.
    pub rule: String,
    /// Characters in before this rule applied (or for the record).
    pub chars_in: usize,
    /// Characters out after this rule applied.
    pub chars_out: usize,
}

impl Reduction {
    /// Creates a reduction entry.
    #[must_use]
    pub fn new(rule: impl Into<String>, chars_in: usize, chars_out: usize) -> Self {
        Self {
            rule: rule.into(),
            chars_in,
            chars_out,
        }
    }
}

/// Source/extractor provenance for a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceProvenance {
    /// When this record was extracted into the corpus store.
    pub extracted_at: chrono::DateTime<chrono::Utc>,
    /// Version of the extractor that produced it.
    pub extractor: String,
    /// Digest of the source artifact at extraction time.
    pub source_digest: Option<String>,
}

impl SourceProvenance {
    /// Builds source provenance.
    #[must_use]
    pub fn new(
        extracted_at: chrono::DateTime<chrono::Utc>,
        extractor: impl Into<String>,
        source_digest: Option<String>,
    ) -> Self {
        Self {
            extracted_at,
            extractor: extractor.into(),
            source_digest,
        }
    }
}

/// A normalized activity record before chunking/embedding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityRecord {
    /// Stable identifier for this record.
    pub id: String,
    /// Normalized text content.
    pub text: String,
    /// Origin of the content.
    pub origin: crate::types::Origin,
    /// Source locator.
    pub source: crate::types::SourceRef,
    /// When the activity occurred.
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// Session identifier if known.
    pub session: Option<crate::types::SessionId>,
    /// Uniform context across all origins.
    pub context: Context,
    /// Reduction log (empty by default).
    pub reductions: Vec<Reduction>,
    /// Source/extractor provenance.
    pub provenance: SourceProvenance,
}

impl ActivityRecord {
    /// Builds a record.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        origin: crate::types::Origin,
        source: crate::types::SourceRef,
        occurred_at: chrono::DateTime<chrono::Utc>,
        session: Option<crate::types::SessionId>,
        context: Context,
        provenance: SourceProvenance,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            origin,
            source,
            occurred_at,
            session,
            context,
            reductions: Vec::new(),
            provenance,
        }
    }

    /// Attaches a reduction entry.
    #[must_use]
    pub fn with_reduction(mut self, reduction: Reduction) -> Self {
        self.reductions.push(reduction);
        self
    }
}
