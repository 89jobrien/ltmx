//! Dense retrieval and hybrid fusion.
//!
//! Two adapters over [`ltmx_domain::ActivityIndex`] -- brute-force cosine and
//! reciprocal rank fusion -- plus a [`HybridRetriever`] that combines any
//! number of them. Depends on the domain only: which embedder backs the dense
//! index is the composition root's decision.

mod dense;
mod fusion;

pub use dense::{DenseIndex, cosine_similarity, normalize};
pub use fusion::{HybridRetriever, RrfParams, rrf};
