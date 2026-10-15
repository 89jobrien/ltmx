# Architecture Decision Records

Decisions ltmx makes, and why. Written at the point of decision, including the
reasoning that led somewhere other than the obvious place.

Each record is immutable once accepted. A decision that changes gets a new
record with `Status: Superseded by ADR-NNNN`, and the original is left intact —
the reasoning matters more than the conclusion, and the path to a wrong turn is
usually what a later reader needs.

## Index

| ADR                                                  | Status            | Decision                                                         |
| ---------------------------------------------------- | ----------------- | ---------------------------------------------------------------- |
| [001](ADR-001-tiered-activity-sourcing.md)           | Accepted          | Source activity from durable traces, not screen capture          |
| [002](ADR-002-ports-live-in-domain.md)               | Accepted          | Ports live in `ltmx-domain`                                      |
| [003](ADR-003-capability-tiers-are-feature-flags.md) | Accepted          | Capability tiers are feature flags                               |
| [004](ADR-004-minimize-dependencies.md)              | Accepted          | Minimize dependencies                                            |
| [005](ADR-005-spike-first-retrieval-decisions.md)    | Accepted          | Retrieval decisions are spike-driven, not assumed                |
| [006](ADR-006-local-embeddings-via-ollama.md)        | Accepted          | Local embeddings via Ollama                                      |
| [007](ADR-007-fuse-by-reciprocal-rank.md)            | Accepted          | Fuse retrievers by reciprocal rank                               |
| [008](ADR-008-shared-chunks-and-object-safety.md)    | Accepted          | Shared chunks and object-safe bulk writes                        |
| [009](ADR-009-hybrid-retriever-borrows-children.md)  | Accepted          | `HybridRetriever` borrows its children                           |
| [010](ADR-010-legacy-ltm-data-is-unrecoverable.md)   | Superseded by 013 | ~~Legacy Pieces LTM data is unrecoverable~~ — **wrong, see 013** |
| [011](ADR-011-diagnostics-in-binaries.md)            | Accepted          | Diagnostics in binaries, plain errors in libraries               |
| [012](ADR-012-exact-cosine-search.md)                | Accepted          | Exact cosine search, no approximate index                        |
| [013](ADR-013-legacy-data-is-recoverable.md)         | Accepted          | The legacy corpus is present and readable — 889 M chars of OCR   |
| [014](ADR-014-embedding-cache.md)                    | Accepted          | Embeddings are cached on disk, keyed by text                     |

## Reading order

If you are new to the project:

1. **001** explains what ltmx is and why it is not a screen recorder. Everything
   else follows from it.
2. **002** and **004** explain the shape of the code and the constraint on its
   dependencies.
3. **005** explains how decisions about retrieval get validated at all.

## What is deliberately _not_ an ADR

Environment facts — Ollama's endpoint, chunked transfer encoding, un-normalized
vectors, the `::1` resolution trap — are in `AGENTS.md`. They are properties of
the machine, not choices this project made, and they will change without anyone
deciding anything.

Test strategy is in `AGENTS.md` under **Testing**. Architecture layer boundaries
are in the **Architecture** section there.

## Open questions

Recorded so they are not mistaken for settled:

- **The `GBLENDED` body decoder.** ADR-013 recovered 889 M characters of screen
  text from SQLite FTS tables with no decoding. Timestamps, application identity
  and structured context live in the body format and are _not_ yet recovered, so
  the corpus has no known time range. Worth doing only if the FTS-only import
  proves insufficient.
- **Deduplication.** Neither index deduplicates. Dense returns the same chunk
  from three transcripts at an identical score, consuming a third of `top_k` on
  one answer. Fix location undecided: the retriever, or the fusion stage.
  Measured scope: transcripts are 34% duplicated, the Pieces archive 0.7%, the
  combined corpus 5.7%. So the archive neither causes nor worsens this — an
  earlier note here claimed it would, from a bad measurement.
- **Chunk balance.** Action chunks (rendered tool calls) are 72% of the corpus
  and crowd out the prompts that carry intent. Whether to rebalance at chunking
  time or to weight at scoring time is undecided.
- **Which embedding model is better.** Both are installed and both emit 768-d
  vectors, so this is a quality comparison at fixed dimension rather than a
  width one. Affordable now that [ADR-014](ADR-014-embedding-cache.md) makes a
  rebuild cheap; not yet run.
- **Deduplication.** Neither index deduplicates. Dense returns the same chunk
  from three transcripts at an identical score, consuming a third of `top_k` on
  one answer. Fix location undecided: the retriever, or the fusion stage. The
  Pieces corpus will make this worse before it makes it better — 186k OCR rows
  are heavily self-similar by construction, and the embedding cache already
  measures a 34% collapse of the transcript corpus onto distinct texts.
- **Persistent dense index.** Exact scan works at current scale; the corpus
  grows without bound and the ceiling is known (see [ADR-012](ADR-012-exact-cosine-search.md)).
