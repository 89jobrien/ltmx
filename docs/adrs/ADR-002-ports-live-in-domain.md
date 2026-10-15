# ADR-002: Ports Live in ltmx-domain

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

ltmx has several interchangeable implementations of the same capabilities: two
`ActivityIndex` backends (BM25, dense cosine), and potentially several
`Embedder` backends (Ollama today, others later). Callers must not change when
a backend changes.

There is a specific, easy mistake here, and this project made it. An early
version defined the `Embedder` port inside `ltmx-embed`, the crate that also
contained the Ollama adapter. That puts the port next to one of its
implementations, so every future embedder would import an adapter crate just to
obtain the trait — and the dependency direction between domain and
infrastructure inverts.

## Decision

All ports live in `ltmx-domain`. Adapters depend on the domain; the domain
depends on nothing.

```text
ltmx-domain      ActivityChunk, Origin, ActivityIndex, Embedder, conformance suites
ltmx-sources     adapters: transcript parsers
ltmx-lexical     adapter: BM25 ActivityIndex
ltmx-retrieve    adapter: cosine DenseIndex, HybridRetriever
ltmx-embed       adapter: Ollama Embedder
ltmx-probe       composition root and spike harness
```

Each port carries a documented contract, and each contract has a reusable
conformance suite in `ltmx_domain::ports::conformance` that an adapter calls
from its own tests. The suite itself has a negative test proving it can fail —
a suite that cannot fail is worthless.

Trait contracts are written as prose that the conformance suite asserts
verbatim, so the documentation and the check cannot drift.

## Consequences

- **Swapping a backend does not touch callers.** A new index implements
  `ActivityIndex` and calls one function in its tests.
- **A port cannot accidentally depend on infrastructure.** The dependency
  direction is structurally enforced by where the code lives.
- **Contract violations are caught at test time, not review time.** The
  conformance suite is the enforcement mechanism.
- **The domain crate must stay dependency-light.** It carries `serde`, `chrono`
  and `thiserror`, which are treated as universally acceptable rather than
  infrastructure.

## Alternatives Considered

- **Ports colocated with their primary adapter.** Rejected: it is convenient
  for one adapter and wrong for the second. This project made the mistake.
- **Define ports in a dedicated `ltmx-ports` crate, as `braid` does.** Viable
  and defensible. Collapsed into `ltmx-domain` here because the domain is
  already a leaf crate with no infrastructure dependencies, so a separate crate
  would add indirection without adding isolation.
- **Document contracts without a conformance suite.** Rejected: an adapter that
  compiles is not necessarily correct, and the suite is what caught two real
  defects during development.
