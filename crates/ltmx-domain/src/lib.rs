//! Core types for ltmx.
//!
//! ltmx reconstructs what you actually worked on from durable traces --
//! agent transcripts, git history, notes -- and answers questions about it.
//!
//! The central value is [`ActivityChunk`]: one retrievable unit of work with
//! enough provenance that a caller can cite it without parsing prose.

mod error;
mod types;

pub mod ports;

pub use error::{EmbedError, LtmxError};
pub use ports::{ActivityIndex, Embedder, ScoredChunk, SearchParams};
pub use types::{ActivityChunk, ChunkId, Origin, Provenance, SessionId, SourceRef, TimeRange};
