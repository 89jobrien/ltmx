# ADR-008: Shared Chunks and Object-Safe Bulk Writes

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

Several indexes serve the same corpus. `HybridRetriever` combines a lexical and
a dense index over identical chunks, and `eval` builds both.

Two problems followed from the obvious signatures:

1. **Deep copies.** `upsert_all(chunks: Vec<ActivityChunk>)` forced
   `chunks.to_vec()` at every call site — a full copy of all 57,766 chunks,
   including every chunk's `String`, once per index.
2. **Object safety.** Changing the parameter to
   `impl IntoIterator<Item = ActivityChunk>` is the idiomatic Rust answer, and
   it silently makes `ActivityIndex` **not dyn compatible**. `&dyn
ActivityIndex` is what lets indexes be swapped at runtime, which is the entire
   point of the port.

## Decision

Hold chunks as `Arc<ActivityChunk>` throughout the port, and type bulk writes as
a **slice**:

```rust
fn upsert(&mut self, chunk: Arc<ActivityChunk>) -> Result<(), LtmxError>;
fn upsert_all(&mut self, chunks: &[Arc<ActivityChunk>]) -> Result<(), LtmxError>;
fn get(&self, id: &ChunkId) -> Result<Arc<ActivityChunk>, LtmxError>;
```

`SearchParams::origins` became an `OriginSet` bitmask over the five `Origin`
variants rather than a `Vec<Origin>`, removing an allocation from the query
path.

A builder on `OriginSet` **accumulates** rather than replaces — chaining
`with_origin(A).with_origin(B)` admits either. An earlier version replaced,
which silently dropped the first filter; a test caught it.

## Consequences

- **A second index costs a pointer copy**, not a corpus copy.
- **`ActivityIndex` stays object safe**, so `&dyn ActivityIndex` works and
  backends remain swappable.
- **Callers with an owned `Vec` pass `&vec`.** Slightly more ceremony, accepted
  deliberately.
- **Callers cannot pass an arbitrary iterator to `upsert_all`.** The obvious
  ergonomic win is the one thing ruled out by object safety; the port contract
  documents this so it is not "fixed" later.

## Alternatives Considered

- **`impl IntoIterator<Item = Arc<ActivityChunk>>`.** Rejected: breaks object
  safety, which costs more than the ergonomic gain.
- **`Vec<ActivityChunk>`.** Rejected: forces a conversion at every call site and
  makes the deep copy the caller's problem rather than the type's.
- **Index stores `&'a ActivityChunk` with a lifetime parameter.** Rejected: it
  threads lifetimes through every adapter and prevents building two indexes from
  one owned corpus without awkward borrow splitting.
