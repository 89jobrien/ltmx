# ADR-004: Minimize Dependencies

**Status:** Accepted, amended 2026-10-14 (see _Amendment_ below)
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

ltmx is intended to run as a long-lived local agent service. Its retrieval
quality depends on an embedding model that is already installed, so almost
nothing requires a third-party crate to be useful. Meanwhile the obvious
libraries for each job are heavy: `reqwest` alone pulls roughly eighty
transitive crates, Tantivy is a full search engine, and Objective-C bindings
exist only to reach system frameworks that have no pure-Rust equivalent.

Every dependency is a supply-chain surface, an upgrade liability, and a
constraint on what the project can ever be built as.

## Decision

Keep the dependency list short, pure Rust, and individually justified. Every
entry must answer **"why is this not the standard library?"**

Accepted non-obvious choices:

- **No HTTP client.** The Ollama adapter hand-rolls HTTP/1.1 over
  `std::net::TcpStream`, including chunked transfer decoding, for one loopback
  POST.
- **No search crate.** BM25 is implemented directly so the probe can answer
  "is lexical retrieval enough?" without committing to Tantivy. It is an
  `ActivityIndex` adapter, swappable when the answer is no.
- **No Objective-C bindings.** Vision.framework and the accessibility API are
  tier 2 and stay optional.
- **`serde_json`, not a parser-combinator library,** for transcript JSON.
  A combinator parser would be strictly worse on escapes, unicode and nesting.
- **`ignore` over `walkdir`**, because `.gitignore` handling matters when
  walking a home directory.
- **`miette` in binaries only.** Libraries stay on plain `thiserror` enums.

Full dependency list: `serde`, `serde_json`, `chrono`, `thiserror`, `clap`,
`ignore`, `tracing`, `tracing-subscriber`, `tempfile`, `miette` (binary only).

Licensing: the project is `MIT OR Apache-2.0`, the most permissive practical
choice, which imposes no conditions on users. Every current dependency is MIT,
Apache-2.0, or dual-licensed under the same terms, so no exceptions are needed —
and `deny.toml` now enforces that rather than asserting it, with
`cargo deny check` passing on advisories, bans, licenses and sources.

## Amendment: "minimize" is not "minimize at any cost"

The rule above was read as absolute, and it was costing the project real
correctness rather than saving it. Correcting that.

The standard of justification stays, but it is **not** "why is this not the
standard library?" any more. It is:

> Would hand-rolling this be a bug farm? If the hand-rolled version is
> something you would write once and then never look at again, take the
> dependency. If it is small, central, and worth understanding in full, keep
> it.

This does **not** mean adding crates on principle. It removes a bias that made
the project re-implement things it should not have.

What changed in practice, and what did not:

- **Still no HTTP client, no search engine, no Objective-C bindings.** Those are
  large subsystems, not conveniences, and the reasons above stand.
- **Still no `nom`.** The HTTP framing parser is small and central. It is
  deferred for a concrete reason (unit-testability on byte fixtures), not
  because parsers are verboten.
- **Hand-rolled correctness helpers stay hand-rolled where they are load-bearing.**
  The hex codec in the embedding cache, the BM25 scorer, and the HTTP chunk
  decoder are each small, central, and worth owning.

`itertools` is the obvious first candidate under the relaxed rule and is
**not yet added** — the embedding cache did not need it. It is expected to earn
its place in the next two pieces of work, both of which are about comparing and
grouping large result sets rather than producing them:

- the retrieval A/B harness, which joins two models' ranked outputs per query
  and reports agreement;
- result deduplication, which groups hits by content before fusion.

Recording this now so the next session does not re-derive a stricter policy from
this ADR's original wording.

## Consequences

- **Protocol handling is ours to get right.** This produced one real bug:
  Ollama answers with `Transfer-Encoding: chunked`, and the initial client
  assumed `Content-Length`. Unit tests could not have caught it; the
  `#[ignore]`d integration test against a live server did, immediately.
- **More code to maintain.** ~60 lines of HTTP framing and ~120 lines of BM25
  are code this project now owns.
- **A parser-combinator library is still justified for HTTP framing.** nom was
  considered and deferred: it would make the framing parser a pure
  `fn(&[u8])` unit-testable on byte fixtures rather than only against a live
  socket. That is a real gain, and the decision is deferred rather than
  rejected.
- **`bytes` was considered and declined.** At 64 texts per loopback request
  against a ~5 ms model, buffer churn is unmeasurable, and the dependency is not
  worth it. Revisit if a real HTTP server with SSE and concurrent clients lands.

## Alternatives Considered

- **`reqwest` for the Ollama call.** Rejected: eighty transitive crates to reach
  a process on loopback. The trade flips the moment an outbound HTTP server is
  needed, at which point this ADR should be revisited rather than worked around.
- **`tantivy` for indexing.** Deferred, not rejected: it is the intended
  successor to the in-house BM25 if lexical retrieval proves insufficient.
- **`fastembed` for local embeddings.** Rejected: onnxruntime is a large
  native dependency. Ollama already runs the model; see
  [ADR-006](ADR-006-local-embeddings-via-ollama.md).
