# ltmx

Reconstruct what you actually worked on, and answer questions about it.

ltmx is a local-first replacement for Pieces LTM ("Long-Term Memory"), the
Workstream Pattern Engine that Pieces shipped and then disabled for users. It
reads the durable traces already on your machine and serves them back over an
MCP server.

## The spike question

Before building anything further, this repository answers one question:

> **Can local traces alone -- agent transcripts, git, notes -- answer the
> questions Pieces LTM used to answer?**

Run it against your own data:

```sh
cargo run -p ltmx-probe --release -- stats
cargo run -p ltmx-probe --release -- eval
cargo run -p ltmx-probe --release -- ask "why did we switch to colima"
```

`eval` runs the four query categories from the `pieces-ltm` skill -- temporal,
decision archaeology, prior art, and current-project context -- because those
are the acceptance criteria, not a guess.

## Source tiers

Sourcing capability is layered by cost, and each tier is behind a feature flag.
Build only the tier you need.

| Tier | Sources                                                   | Cost                       | Feature              |
| ---- | --------------------------------------------------------- | -------------------------- | -------------------- |
| 0    | Claude Code transcripts (`~/.claude/projects/**/*.jsonl`) | none                       | `claude-transcripts` |
| 0    | git history                                               | shells out to `git`        | _planned_            |
| 0    | notes (Obsidian vault)                                    | none                       | _planned_            |
| 1    | screen capture + OCR via `screencapture` / `tesseract`    | one `brew install`, no FFI | _planned_            |
| 2    | Vision.framework OCR, accessibility tree                  | `objc2` bindings           | _planned_            |

Tier 2 is Objective-C. There is no pure-Rust path to it, so it stays optional.
Most text that would be OCR'd off a screen came from a file or a transcript
that is already indexed at tier 0.

## Retrieval

Two adapters behind one port, swappable at build time:

- `ltmx-lexical` (`bm25`) -- dependency-free BM25. Fast, exact, no model.
- `ltmx-embed` (`ollama`) -- `nomic-embed-text` (768-d) via a local Ollama
  server, over a hand-rolled HTTP client so no HTTP stack is linked.
- `ltmx-retrieve` (`hybrid`) -- exact cosine `DenseIndex` plus
  `HybridRetriever`, which fuses ranked lists with reciprocal rank fusion.

RRF is used rather than score blending because BM25 scores and cosine
similarities are not on a comparable scale; fusing _ranks_ needs no
calibration.

```sh
cargo run -p ltmx-probe --release -- ask "why did we switch to colima" --strategy hybrid
```

Embedding the full corpus takes minutes, so `eval` builds the dense index only
when a strategy needs it.

## Examples

Each adapter crate has a self-contained `cargo run --example` that exercises its
public API without touching your real corpus:

```sh
# BM25 search over a handful of in-memory chunks. No external dependencies.
cargo run -p ltmx-lexical --example search

# DenseIndex + HybridRetriever fusion, using a toy embedder so the example
# runs offline with no Ollama server required.
cargo run -p ltmx-retrieve --example hybrid_search

# Ingest and query ActivityRecords through SqliteStore. Requires the
# sqlite3 CLI on PATH (already a runtime dependency of ltmx-store).
cargo run -p ltmx-store --example corpus
```

There is no example for `ltmx-embed`'s `OllamaEmbedder`: it needs a running
local Ollama server (see [Notes](#notes)), so it is covered by the
`#[ignore]`d integration tests under `cargo test -p ltmx-embed --features
ollama -- --ignored` instead of a standalone example.

## Architecture

Hexagonal. Ports live in `ltmx-domain`; adapters depend on the domain and never
the reverse.

```text
ltmx-domain      ActivityChunk, Origin, ActivityIndex, Embedder, conformance suites
ltmx-sources     adapters: transcript parsers
ltmx-lexical     adapter: BM25 ActivityIndex
ltmx-retrieve    adapter: cosine DenseIndex, HybridRetriever (RRF)
ltmx-embed       adapter: Ollama Embedder
ltmx-probe       composition root and spike harness
```

Adding an index backend means implementing `ActivityIndex` and calling
`assert_activity_index_contract` from your tests. Callers do not change.

Chunks are shared as `Arc` across indexes, so building a second index over the
same corpus costs a pointer copy rather than deep-copying every chunk's text.
`HybridRetriever` borrows its children for the same reason: the dense index
costs a ~6 minute embedding pass and must be built once.

## Dependency policy

Keep the list short, pure Rust, and justified. See `AGENTS.md`.

## Development

```sh
cargo xtask fmt
cargo xtask lint
cargo xtask test
cargo xtask quick
```

Integration tests that need a running Ollama are `#[ignore]`d:

```sh
cargo test -p ltmx-embed --features ollama -- --ignored
```

Fuzzing the transcript parser needs nightly:

```sh
cargo +nightly fuzz run parse_transcript
```

## Documentation

| File                  | Purpose                                                   |
| --------------------- | --------------------------------------------------------- |
| `AGENTS.md`           | single source of truth for agent guidance                 |
| `CONSTITUTION.md`     | project principles, subordinate to the global one         |
| `DEVELOPMENT.md`      | commands, gates, feature matrix, troubleshooting          |
| `FEATURES.md`         | what exists, what is planned, what is blocked, non-goals  |
| `HANDOFF.md`          | current state, findings, known gaps, next steps           |
| `ROADMAP.md`          | phases, each with the measurement that unlocks the next   |
| `docs/adrs/README.md` | architecture decision records — why the code is shaped so |

## License

`MIT OR Apache-2.0`, matching the `license` field in `Cargo.toml`. License texts
are at [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

The dual license follows the workspace convention: MIT for permissive use,
Apache-2.0 for its explicit patent grant and improved derivative-works terms.

## Notes

- Ollama is reached at `127.0.0.1:11434`, not `localhost`: `localhost`
  resolves to `::1` first on this machine and Ollama binds IPv4 only.
- Ollama answers with `Transfer-Encoding: chunked`, and returns
  `nomic-embed-text` vectors **un-normalized**. Normalize before comparing.
- Batching is not optional: `/api/embed` runs at ~5 ms per text, where a
  per-request round trip is far slower. A full-corpus pass is ~6 minutes.

## Findings so far

Measured against `~/.claude/projects` — 57,766 chunks across 805 sessions.

| Query                           | Lexical  | Dense   | Hybrid   |
| ------------------------------- | -------- | ------- | -------- |
| prior art (`parse op env vars`) | good     | weak    | good     |
| current project (`minibox ci`)  | good     | good    | good     |
| temporal (`devloop project`)    | mediocre | weak    | mediocre |
| decision archaeology (`colima`) | **fail** | partial | partial  |

Two conclusions worth keeping:

1. **Lexical alone fails decision archaeology**, not from a corpus gap --
   `colima` appears in 485 transcripts and is found instantly by a
   content-word query. The failure is that `why`, `we` and `switch` carry high
   IDF and bury the subject. This is the concrete justification for embeddings.
2. **Neither index deduplicates.** Dense returned the same chunk from three
   files at an identical score, filling `top_k` with one answer.

Action chunks (rendered tool calls) are 72% of the corpus and crowd out the
prompts that carry intent. Deduplication and chunk balance are the next two
things to fix.
