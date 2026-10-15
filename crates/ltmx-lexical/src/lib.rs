//! A small BM25 index over [`ActivityChunk`]s.
//!
//! # Why this is behind the `bm25` feature
//!
//! BM25 is deliberately dependency-free: the probe needs to answer "is lexical
//! retrieval enough?" without pulling in Tantivy or any other index crate. The
//! moment that question is answered -- and if the answer is no -- a different
//! adapter implements [`ActivityIndex`] behind the same port and this module is
//! switched off. Callers do not change either way, so the choice stays a
//! build-time decision.
//!
//! Disable with `default-features = false`.

mod scoring;
mod tokenize;

#[cfg(feature = "bm25")]
mod index;

#[cfg(feature = "bm25")]
pub use index::LexicalIndex;

pub use scoring::{Bm25Params, bm25};
pub use tokenize::tokenize;
