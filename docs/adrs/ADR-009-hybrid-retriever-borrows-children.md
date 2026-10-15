# ADR-009: HybridRetriever Borrows Its Children

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

A dense index costs a full embedding pass — measured at **366 seconds for 57,766
chunks** against a local Ollama server.

`eval` needs to run three strategies over the same corpus: lexical, dense, and
their fusion. With an owning `HybridRetriever(Vec<Box<dyn ActivityIndex>>)`,
every construction needs its own dense index, so a naive implementation embeds
the corpus once per strategy. The first `eval` run took **733 seconds and
embedded the same corpus twice**, once for `dense` and again for `hybrid`.

## Decision

`HybridRetriever<'a>` holds `Vec<&'a dyn ActivityIndex>` and borrows its
children.

Because it cannot own writes, `upsert` and `upsert_all` return
`LtmxError::InvalidQuery` with an explanatory message rather than silently
discarding the write. Callers write to the underlying indexes, then construct
the retriever.

## Consequences

- **One embedding pass serves every strategy.** `eval` now embeds once and runs
  all three rankings against the same indexes — **366 seconds total instead of
  733**.
- **The retriever is read-only.** This is a real constraint, not a temporary
  limitation: a borrow-checked design should not pretend to accept writes.
- **`HybridRetriever` cannot pass the general conformance suite**, which
  exercises writes. Its read-side guarantees are tested directly instead. This
  is a known gap in coverage, recorded rather than papered over.
- **Children must outlive the retriever.** In practice they are bound in the
  same scope, which is exactly the intent.

## Alternatives Considered

- **Keep owning children, accept duplicate embedding.** Rejected: doubles the
  slowest operation in the project for no benefit.
- **Share a dense index behind `Arc`.** Rejected: it would have worked for this
  case, but borrowing expresses the actual constraint — the retriever does not
  own its children — and prevents mutation through the trait object.
- **Cache vectors to disk between runs.** Worth revisiting independently: it
  would eliminate the pass entirely on repeat runs.
