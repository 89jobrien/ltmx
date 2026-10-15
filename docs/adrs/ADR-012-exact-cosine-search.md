# ADR-012: Exact Cosine Search, No Approximate Index

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

`DenseIndex` answers "the k chunks most similar to this query vector". The
standard way to do this at scale is an approximate nearest-neighbour structure —
HNSW, IVF, or a product-quantized index. The `ragrs` workspace contains an HNSW
adapter for exactly this, and its README documents a real defect: the dense
index lives only for the lifetime of one process, so an `index` invocation
followed by a `query` invocation silently degrades to lexical-only results.

ltmx is being built against a corpus of **57,766 chunks of 768 dimensions**.
That is roughly 44 million floats — about 177 MB — small enough to scan exactly.

## Decision

Brute-force exact cosine scan. No approximate structure.

Vectors are normalized on write and on query, because Ollama returns
`nomic-embed-text` un-normalized (a raw dot product comes back around 223.8, not
≤ 1) and ranking by magnitude rather than direction would be meaningless.

Degenerate cases return `0.0` rather than `NaN`: mismatched lengths, empty
vectors, zero vectors, and non-finite inputs. A bad vector should degrade one
result, not poison a sort.

## Consequences

- **No recall loss.** Exact search cannot silently miss the answer, which
  removes an entire class of bug that would be very hard to diagnose in a
  personal memory system.
- **Query cost is O(n).** Acceptable at this scale; a scan over 768 floats per
  chunk is microseconds.
- **The corpus ceiling is where this stops working.** Growth is linear in both
  time and memory, and ltmx will accumulate indefinitely. Revisit when the
  corpus reaches a size where scan latency is measurable.
- **It avoids the persistence defect** that made `ragrs`' HNSW adapter
  unsuitable: this index lives in the same process that queries it, so the
  mismatch cannot arise.

## Alternatives Considered

- **Reuse `ragrs`'s HNSW adapter.** Rejected for the documented persistence
  defect, and unnecessary at this corpus size.
- **`sqlite-vec` (`vec0` virtual table).** Attractive for persistence across
  processes, and `memx` already uses it. Deferred: it requires the whole index
  to become file-backed, which is a larger change than the current problem
  warrants. It remains the most likely successor.
- **Product quantization to cut memory.** Rejected: approximate before it is
  needed, and it would reintroduce recall loss.
