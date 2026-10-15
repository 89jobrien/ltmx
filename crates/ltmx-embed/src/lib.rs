//! Embedding adapters.
//!
//! The [`ltmx_domain::Embedder`] port lives in the domain; this crate holds only
//! concrete backends. Gated behind the `ollama` feature, so `--no-default-features`
//! builds with no network capability whatsoever.
//!
//! [`cache`] is a decorator over that port rather than a backend: it wraps any
//! `Embedder` and serves repeats from disk, which is what makes comparing two
//! embedding models affordable.

#[cfg(feature = "ollama")]
pub mod ollama;

#[cfg(feature = "ollama")]
pub use ollama::{DEFAULT_HOST, NOMIC_DIMENSIONS, OllamaEmbedder};

pub mod cache;

pub use cache::{CacheProblem, CacheStats, CachingEmbedder};
