//! Errors surfaced by domain operations.

use crate::types::ChunkId;

/// Errors raised by index and retrieval operations.
#[derive(Debug, thiserror::Error)]
pub enum LtmxError {
    /// The index rejected a write or a query.
    #[error("index error: {0}")]
    Index(String),
    /// A chunk was not found by the given id.
    #[error("chunk not found: {0}")]
    NotFound(ChunkId),
    /// A caller passed arguments the index cannot honor.
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}

/// Errors raised while embedding text.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The embedding backend could not be reached.
    #[error("embedding backend unreachable at {endpoint}: {reason}")]
    Unreachable {
        /// `host:port` or URL that was attempted.
        endpoint: String,
        /// Underlying transport failure.
        reason: String,
    },
    /// The backend replied with a non-success status.
    #[error("embedding backend returned {status}: {body}")]
    Status {
        /// HTTP status code returned by the backend.
        status: u16,
        /// Response body, truncated by the adapter.
        body: String,
    },
    /// The reply could not be decoded.
    #[error("malformed embedding response: {0}")]
    Decode(String),
    /// The backend returned a vector of unexpected width.
    #[error("expected {expected} dimensions, backend returned {actual}")]
    DimensionMismatch {
        /// Width declared by [`crate::Embedder::dimensions`].
        expected: usize,
        /// Width actually returned by the backend.
        actual: usize,
    },
}
