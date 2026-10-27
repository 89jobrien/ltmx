//! Core types for ltmx.
//!
//! ltmx reconstructs what you actually worked on from durable traces --
//! agent transcripts, git history, notes -- and answers questions about it.
//!
//! The central value is [`ActivityChunk`]: one retrievable unit of work with
//! enough provenance that a caller can cite it without parsing prose.

mod error;
pub mod normalize;
mod types;

pub mod ports;

pub use error::{EmbedError, LtmxError};
pub use normalize::{ActivityRecord, Context, Reduction, SourceProvenance};
pub use ports::{ActivityIndex, Embedder, ScoredChunk, SearchParams};
pub use types::{
    ActivityChunk, ChunkId, Origin, Provenance, ScoringSignals, SessionId, SourceRef, TimeRange,
};
