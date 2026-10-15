# ADR-006: Local Embeddings via Ollama

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

Dense retrieval measurably helps where lexical fails — decision archaeology on
the colima query is the confirmed case. That requires an embedding model.

The candidates were a cloud embedding API, `fastembed` with ONNX Runtime, or a
local model server. The project is local-first: the corpus is the user's own
transcripts, and a long-lived agent service that phones a cloud endpoint for
every index build is a privacy regression.

## Decision

Use `nomic-embed-text` (768 dimensions) through a local Ollama server on
`127.0.0.1:11434`, behind the `ollama` feature and the `Embedder` port.

Three environment properties were established empirically and are recorded in
`AGENTS.md`:

- **Use `127.0.0.1`, never `localhost`.** On this machine `localhost` resolves
  to `::1` first, and Ollama binds IPv4 only. `localhost` fails where
  `127.0.0.1` succeeds.
- **The model is already installed.** `nomic-embed-text` and
  `nomic-embed-text-v2-moe` are present locally, so there is no download step
  and no cold-start failure mode for a first query.
- **Batching is mandatory, not an optimization.** `/api/embed` runs at
  roughly 5 ms per text; a per-request round trip is far slower. Measured:
  **57,766 chunks in 366 seconds**, versus hours per-chunk. `Embedder::embed_batch`
  overrides the default and every bulk path uses it.

## Consequences

- **Vectors are un-normalized.** A raw dot product of two `nomic-embed-text`
  vectors comes back around **223.8**, not ≤ 1. `DenseIndex` normalizes on write
  and on query; comparing raw dot products would rank by magnitude, not
  direction. Found by the integration test, not by unit tests.
- **Ollama must be running for the dense path.** Failures surface as
  `EmbedError::Unreachable`, which is why lexical remains available with
  `--no-default-features`.
- **The embedder must report a stable `model_id`.** It is recorded alongside the
  index so a model swap is detected rather than silently degrading retrieval.
- **Vector width is guarded on write.** `DenseIndex` records the width at
  construction and rejects a mismatched vector with an error rather than storing
  it.

## Alternatives Considered

- **Cloud embeddings.** Rejected: sends the user's transcripts off-machine for
  every index build, for no local-first benefit.
- **`fastembed` + ONNX Runtime.** Rejected: a large native dependency
  (see [ADR-004](ADR-004-minimize-dependencies.md)) for a model that is already
  installed and running.
- **`nomic-embed-text-v2-moe`.** Not selected: the newer variant is ~3.5× the
  size. Both are 768-dimensional, so switching is a config change behind the
  port.
