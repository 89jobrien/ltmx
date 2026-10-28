//! Parsers turning durable local traces into [`ltmx_domain::ActivityChunk`]s.
//!
//! Tier 0: everything here reads state you already have on disk. No screen
//! capture, no Objective-C, no network. See `AGENTS.md` for the tier policy.
//!
//! `pieces` is a different kind of source: it reads an *archive* of screen
//! activity that a previous product recorded, rather than capturing the screen
//! now. It is behind its own feature flag because it reads another
//! application's private database, and that is a heavier commitment than
//! parsing your own transcripts.

#[cfg(feature = "claude-transcripts")]
use ltmx_domain::LtmxError;

#[cfg(feature = "claude-transcripts")]
pub mod claude;

#[cfg(feature = "claude-transcripts")]
pub use claude::ClaudeTranscriptReader;

#[cfg(feature = "pieces-archive")]
pub mod pieces;

#[cfg(feature = "pieces-archive")]
pub use pieces::{ArchiveReport, Field, PiecesArchiveReader, SkipReason};

#[cfg(feature = "claude-transcripts")]
/// Recursively finds files with `extension` beneath `root`.
///
/// Uses the `ignore` crate so `.gitignore` and hidden-file rules apply, which
/// keeps index runs fast and predictable across a large home directory.
#[cfg(feature = "claude-transcripts")]
pub fn walk(root: &std::path::Path, extension: &str) -> Result<Vec<std::path::PathBuf>, LtmxError> {
    let mut found = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .build()
        .flatten()
    {
        if entry.path().extension().is_some_and(|ext| ext == extension) {
            found.push(entry.into_path());
        }
    }
    found.sort();
    Ok(found)
}
