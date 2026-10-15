# ADR-003: Capability Tiers Are Feature Flags

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

ltmx grows along two independent axes: _what it can source_ (transcripts, git,
notes, screen) and _how it can retrieve_ (BM25, dense embeddings, hybrid
fusion). These axes are also very different in cost — a lexical index over
57,766 chunks builds in under a second, while a dense index over the same corpus
takes a ~6 minute embedding pass against a local model server.

An early design defined `Embedder` as a runtime-composed port in a library, so
the embedding backend was a runtime concern rather than a build-time one. That
coupled the library to Ollama's existence.

## Decision

Each optional capability is a Cargo feature. Defaults cover the useful local
setup; nothing is required to compile a narrower build.

| Feature              | Enables                                        |
| -------------------- | ---------------------------------------------- |
| `claude-transcripts` | reads `~/.claude/projects/**/*.jsonl`          |
| `bm25`               | the in-house BM25 `ActivityIndex`              |
| `ollama`             | the Ollama `Embedder`                          |
| `hybrid`             | dense retrieval + RRF; implies `bm25`+`ollama` |

Rules, enforced in review:

- A feature flag selects an **optional backend**, never hides dead code. If a
  feature is on by default and never off, delete it.
- Anything behind a feature **must compile with the feature off**. Verify with
  `cargo check --no-default-features`.
- Adapters implementing a port must pass that port's conformance suite
  regardless of which feature enabled them.

## Consequences

- **`--no-default-features` builds with no network capability at all.** The
  narrowest build links neither an HTTP client nor an embedder.
- **Build configuration mirrors runtime intent.** A machine without Ollama
  builds a lexical-only probe rather than failing at runtime.
- **The cfg matrix is real maintenance.** Every new feature requires checking
  the no-default build; this has caught six compile warnings that default-feature
  builds never surface.
- **Feature combinations must stay coherent.** `hybrid` implying `bm25` and
  `ollama` keeps `eval` from offering a strategy it cannot build.

## Alternatives Considered

- **Runtime-pluggable embedders, no features.** Rejected: it makes Ollama a
  hard dependency of the build graph and pushes cost decisions to runtime, where
  they are harder to test.
- **One crate with everything always on.** Rejected: no cheap build exists, and
  "does this still work without the model server" stops being a question the
  compiler can answer.
