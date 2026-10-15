# Development Guide

Canonical developer workflow for ltmx. See `AGENTS.md` for architecture and
environment rules, `docs/adrs/` for why the code is shaped this way, and
`FEATURES.md` for what exists and what does not.

## Quick start

```sh
# Fast feedback: format, lint, test
cargo xtask quick

# Full local gate with summary
cargo xtask ci

# Install git hooks (once)
cargo xtask install-hooks
```

## Prerequisites

- Rust stable, edition 2024. Pinned via `rust-toolchain.toml`, which also
  declares the `clippy` and `rustfmt` components so a fresh checkout needs no
  `rustup component add`. The MSRV is **1.85**, declared in
  `[workspace.package] rust-version` and mirrored in `clippy.toml`.
- [`taskit`](https://crates.io/crates/taskit), invoked through `cargo xtask`
  (the shim auto-installs it if missing)
- **Ollama** with `nomic-embed-text` pulled — required only for the dense and
  hybrid retrieval paths

For the audit gate: `cargo deny` must be installed separately
(`cargo install cargo-deny`). Everything else comes from the toolchain file.

There are no runtime secrets and no network dependency beyond loopback.

## Commands

| Command             | Covers                                    |
| ------------------- | ----------------------------------------- |
| `cargo xtask fmt`   | format all Rust                           |
| `cargo xtask lint`  | clippy with `-D warnings`                 |
| `cargo xtask test`  | workspace tests                           |
| `cargo xtask quick` | fmt-check + lint + test                   |
| `cargo xtask ci`    | full local gate with summary table        |
| `cargo xtask audit` | `cargo deny` — advisories, licenses, bans |

### Gates that must pass before committing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --no-default-features
cargo deny check
```

The last one is not optional. Feature-gated code that compiles under default
features routinely warns under `--no-default-features`; that check has caught
every such warning in this project so far.

Verify individual feature combinations when touching a feature:

```sh
cargo check -p ltmx-probe --no-default-features
cargo check -p ltmx-probe --no-default-features --features bm25
cargo check -p ltmx-probe --no-default-features --features hybrid
cargo check -p ltmx-probe --no-default-features --features pieces-archive
cargo check -p ltmx-probe --no-default-features --features claude-transcripts,pieces-archive
```

The `pieces-archive` combos matter more than they look. Its `archive`
subcommand returns before the transcript corpus loads, and the `miette` error
imports are gated on feature combinations — so a new source tier can break in a
configuration the default build never exercises. That has already happened
twice in this project.

## The archived Pieces corpus

```sh
# Import the readable-text field and report what it yields
cargo run --release -p ltmx-probe --features pieces-archive -- archive

# Raw OCR of screen pixels -- larger and noisier
cargo run --release -p ltmx-probe --features pieces-archive -- archive --field ocr

# Smoke test against a small slice
cargo run --release -p ltmx-probe --features pieces-archive -- archive --limit 200

# Point at a copy rather than the live database
cargo run --release -p ltmx-probe --features pieces-archive -- archive --db /path/to/db.sqlite3
```

Reads `~/Library/com.pieces.os/production/Pieces/couchbase.cblite2/db.sqlite3`
unless `--archive-db` says otherwise. Full `readable` import: **145,534 records →
363,791 chunks in 5.7 s**.

Requires the system `sqlite3` binary, which macOS ships. The database is only
ever opened `immutable=1`; that is not optional and not configurable.

### Indexing the archive alongside transcripts

`--archive` adds the archived corpus to every command. It is opt-in because it
swings the corpus from ~58k chunks to ~420k, which changes the character of any
measurement.

```sh
# Corpus mix, with the archive included
ltmx-probe stats --archive

# A query over both, limited to a slice while iterating
ltmx-probe ask "what was I working on in devloop" --strategy lexical --archive --archive-limit 20000

# Full eval across all three strategies
ltmx-probe eval --archive
```

`stats` breaks the corpus down by origin, which is the first thing to check
when adding the archive changes an answer.

**Known result:** adding the archive does _not_ fix decision archaeology. It
changes lexical failure from "generic prompts" to "application chrome". See
[ADR-013](docs/adrs/ADR-013-legacy-data-is-recoverable.md) for the measurement.

### Filtering

`--origins` and `--since`/`--until` are global and work before or after the
subcommand. `--origins` is repeatable; `--since` accepts `YYYY-MM`,
`YYYY-MM-DD` or a full RFC 3339 timestamp, and `--until 2026-03` means the
whole of March.

```sh
# Is the answer in the screen history at all?
ltmx-probe ask "colima" --archive --origins screen

# What was I doing in April?
ltmx-probe ask "ci pipeline" --archive --since 2026-04 --until 2026-05

# Transcripts only, regardless of what else is loaded
ltmx-probe eval --origins agent-transcript
```

`--origins` is also the fastest way to tell a ranking problem from an
availability problem. See the measured result in
[ADR-013](docs/adrs/ADR-013-legacy-data-is-recoverable.md).

### Embedding cache

```sh
# Warm cache: the second run embeds nothing
ltmx-probe eval

# Compare two models at the same dimension, each with its own cache file
ltmx-probe eval --model nomic-embed-text       --cache ~/.cache/ltmx/nomic.bin
ltmx-probe eval --model nomic-embed-text-v2-moe --cache ~/.cache/ltmx/nomic-v2.bin

# Force a full rebuild
ltmx-probe eval --no-cache
```

Changing `--model` invalidates the cache rather than mixing vector spaces, so
the two runs above are independent and each is warm after its first pass.

## Running the probe

`ltmx-probe` is the instrument that answers the project's central question.
See [adr-005](docs/adrs/ADR-005-spike-first-retrieval-decisions.md).

```sh
# What is available
cargo run -p ltmx-probe --release -- stats

# One question
cargo run -p ltmx-probe --release -- ask "why did we switch to colima"
cargo run -p ltmx-probe --release -- ask "minibox ci pipeline" --strategy hybrid

# The four acceptance queries, under every compiled strategy
cargo run -p ltmx-probe --release -- eval
```

**Build with `--release` for anything using embeddings.** A debug build spends
more time scanning than Ollama spends embedding, which makes query latency look
like a retrieval problem when it is not.

`eval` and any dense strategy pay a **~366 second** embedding pass over the full
corpus on first run. `HybridRetriever` borrows its children so that pass happens
once per process, not once per strategy.

## Tests

```sh
# Default suite
cargo test --workspace

# Ollama integration (ignored by default so CI needs no model server)
cargo test -p ltmx-embed --features ollama --test ollama_integration -- --ignored

# Fuzz the transcript parser (needs nightly)
cargo +nightly fuzz build parse_transcript
cargo +nightly fuzz run parse_transcript fuzz/seeds/parse_transcript -- -max_total_time=60
```

Test strategy and the reasoning behind each dimension are in `AGENTS.md` under
**Testing**. The short version: unit tests are always present, every port has a
conformance suite in `ltmx_domain::ports::conformance`, the transcript parser
is fuzzed, and the conformance suite has a negative test proving it can fail.

Committed fuzz seeds live in `fuzz/seeds/`. They are permanent — adding them
raised the parser's coverage from 763 to 979 edges. The libFuzzer-generated
corpus is reproducible from them and stays gitignored.

## Layout

```text
ltmx/
├── crates/
│   ├── ltmx-domain/      types, ports, conformance suites  (leaf, no infra deps)
│   ├── ltmx-sources/     adapters: transcript parsers
│   ├── ltmx-lexical/     adapter: BM25 ActivityIndex
│   ├── ltmx-retrieve/    adapter: cosine DenseIndex, HybridRetriever (RRF)
│   ├── ltmx-embed/       adapter: Ollama Embedder
│   └── ltmx-probe/       composition root and spike harness
├── docs/adrs/            architecture decision records
├── fuzz/                 fuzz targets and committed seeds
├── taskit.toml           workspace + CI step definitions
├── deny.toml             license / advisory / source policy
├── rustqual.toml         code-quality thresholds
├── rust-toolchain.toml   pinned channel and components
├── rustfmt.toml          max_width = 100, edition 2024
├── clippy.toml           msrv
├── CONSTITUTION.md       project principles
└── xtask/                taskit shim
```

Dependencies point inward: `ltmx-domain` depends on nothing; every adapter
depends on the domain.

## Adding a retrieval backend

1. Implement `ActivityIndex` in a new crate under `crates/`.
2. Call `assert_activity_index_contract(|| YourIndex::new(...))` from its test
   module. Do not skip this — it is what catches contract violations.
3. Write the port contract prose in the impl's docs if it narrows anything.
4. Add the crate to `Cargo.toml` members, `taskit.toml` crates and propagation,
   and the workspace dependency list.
5. Register a `Strategy` variant in `ltmx-probe` and a feature flag behind it.

The caller-facing API does not change. That is the point.

## Troubleshooting

| Symptom                                               | Cause and fix                                                                                                           |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `embedding backend unreachable`                       | Ollama not running, or bound to a different port. Verify with `curl 127.0.0.1:11434/api/tags` — **not** `localhost`.    |
| `malformed JSON` / `expected struct`                  | Ollama answered chunked and the framing was mis-decoded. Check the `Transfer-Encoding` branch in `post_json`.           |
| `dimension mismatch`                                  | The embedder's `dimensions()` disagrees with the stored index width. Rebuild the index after an intentional model swap. |
| `invalid query: HybridRetriever borrows its children` | Expected. Write to the underlying indexes, then construct the retriever.                                                |
| Dense queries feel slow                               | You are on a debug build. Use `--release`.                                                                              |
| `expected struct EmbedResponse`                       | Response was truncated; check the chunked-body decoder decodes every chunk before the terminating zero-length one.      |

## Git

Feature branches; never commit directly to `main`. Conventional Commits:

```text
<type>(<scope>): <description>
```

Never use `--no-verify`.

## License

`MIT OR Apache-2.0`, declared in `[workspace.package]` and inherited by every
crate. Texts are at `LICENSE-MIT` and `LICENSE-APACHE`.

Adding a dependency with a different license class requires updating both the
Cargo metadata and the audit policy before it merges.
