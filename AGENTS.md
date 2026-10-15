<agent-instructions>

<read-first>
- `README.md` — what ltmx is, retrieval strategies, measured results.
- `CONSTITUTION.md` — project principles. Subordinate to the global
  `~/.claude/CONSTITUTION.md`; where this file is silent, those apply.
- `DEVELOPMENT.md` — commands, gates, feature matrix, troubleshooting.
- `FEATURES.md` — what exists, what is planned, what is blocked, non-goals.
- `HANDOFF.md` — current state, findings, known gaps, next steps.
- `ROADMAP.md` — phases, each with the measurement that unlocks the next.
- `docs/adrs/README.md` — why the code is shaped as it is.
</read-first>

<architecture>
Hexagonal. Ports live in `ltmx-domain`; adapters depend on the domain, never
the reverse. See `docs/adrs/` for the reasoning behind each structural choice;
the ADRs explain *why*, this section states *what*.

- `ltmx-domain` -- `ActivityChunk`, `Origin`, `SourceRef`, `TimeRange`, plus the
  `ActivityIndex` and `Embedder` ports and their conformance suites.
- `ltmx-sources` -- adapters that turn durable traces into chunks, including
  the archived Pieces corpus (`pieces-archive`).
- `ltmx-lexical` -- BM25 `ActivityIndex` adapter.
- `ltmx-retrieve` -- exact cosine `ActivityIndex` plus `HybridRetriever` (RRF).
- `ltmx-embed` -- Ollama `Embedder` adapter.
- `ltmx-probe` -- composition root and spike harness.

Never define a port in an adapter crate. If a trait describes a capability the
domain needs, it belongs in `ltmx-domain`.
</architecture>

<dependency-policy>
Keep dependencies few, pure Rust, and individually justified. Every entry in
`Cargo.toml` must answer "why is this not the standard library?"

Current non-obvious choices:

- No HTTP client crate. The Ollama adapter hand-rolls HTTP/1.1 over
  `std::net::TcpStream`. reqwest would add ~80 transitive crates for one
  loopback POST.
- No search crate. BM25 is implemented directly so the probe can answer "is
  lexical enough?" without committing to Tantivy. Swap it behind
  `ActivityIndex` when the answer is no.
- No Objective-C bindings. Vision.framework and the accessibility API are
  Objective-C and cannot be reached from pure Rust; they are tier 2 and stay
  optional.
- `ignore` is used instead of `walkdir` because it applies `.gitignore`, which
  matters when walking a home directory.
- `miette` is used in the **binary only**. A `Diagnostic` impl in a published
  library leaks into every consumer and couples them to the renderer, so
  `ltmx-domain` and friends stay on plain `thiserror` enums and the binary
  converts at the boundary.
- `serde_json` for transcript JSON. A parser combinator library would be
  strictly worse here; it is only worth considering for the HTTP chunk
  framing, where it would make the parser unit-testable on byte fixtures.

If you add a dependency, justify it in the pull request.

## License

`MIT OR Apache-2.0`. Texts are `LICENSE-MIT` and `LICENSE-APACHE`; the
`license` field is inherited from `[workspace.package]`. Keep all three in sync.

## Toolchain configuration

Every config file in this repo is verified, not assumed. If you change one, run
the command that proves it still does something.

| File                  | Governs                                                                 | Verify with                                                            |
| --------------------- | ----------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `rust-toolchain.toml` | pins `channel = "stable"` and the `clippy` + `rustfmt` components       | `cargo --version` in the repo root picks up the file                   |
| `rustfmt.toml`        | `max_width = 100`, `edition = "2024"`                                   | `cargo fmt --all --check`                                              |
| `clippy.toml`         | `msrv = "1.85"` — keeps clippy from suggesting newer-than-MSRV std APIs | `cargo clippy` (an invalid key is a hard error, so a typo cannot hide) |
| `deny.toml`           | license/advisory/source policy                                          | `cargo deny check`                                                     |
| `rustqual.toml`       | quality thresholds and suppression ceiling                              | `rustqual . --no-fail`                                                 |

Two things are deliberately absent, and both are absent for a reason rather
than by omission:

- **`[workspace.lints]` with `lints.workspace = true`.** The toolchain here
  (cargo 1.99.0) accepts the key and then ignores it: every member manifest
  emits `unused manifest key: package.lints` and no lint flags reach rustc.
  Verified by checking `-v` output for `--deny=unsafe_code`, which is absent.
  Do not add it back until `cargo clippy -v` shows the flags.
- **`taplo.toml`.** `taplo` is not installed and no TOML formatter runs in this
  repo. A formatter config for a tool nobody runs is a comment.

### Lint policy lives in the gate, not in a table

`cargo clippy --workspace --all-targets -- -D warnings` is the gate. There is no
workspace lint table, so stricter sets are passed as flags on that command.

Enabling `clippy::pedantic` was measured, not assumed: **~60 findings across
`ltmx-lexical`, `ltmx-sources`, `ltmx-retrieve` and `ltmx-embed`; zero in
`ltmx-domain` and `ltmx-probe`.** The distribution is what decides whether it is
worth it:

| Finding                                                                                                                    | Count | Real or noise                                                       |
| -------------------------------------------------------------------------------------------------------------------------- | ----- | ------------------------------------------------------------------- |
| `assert_is_empty` in tests                                                                                                 | ~13   | noise — clippy's suggested `assert_eq!(x, [] as [T; 0])` is worse   |
| `cast_precision_loss`, `cast_possible_truncation`                                                                          | ~10   | noise — BM25 and cosine are `f32` math; `usize as f32` is the point |
| `must_use_candidate`, `return_self_not_must_use`                                                                           | ~10   | real, cheap, worth doing                                            |
| `missing_errors_doc`                                                                                                       | 4     | real — the public fallible entry points should say what fails       |
| `items_after_statements`                                                                                                   | 2     | real — a `const` declared after a statement                         |
| `needless_pass_by_value`, `map_unwrap_or`, `explicit_iter_loop`, `manual_let_else`, `needless_continue`, `match_same_arms` | 7     | real                                                                |

Turning it on means editing test assertions to something less readable unless
those lints are individually allowed. That is a judgement call, not a
config-file change, so it is not pre-made here.

## Data sharing

Chunks are held as `Arc<ActivityChunk>` through the whole port. Several indexes
serve the same corpus, and a transcript chunk owns its text -- deep-copying the
corpus once per index is the most expensive thing an index build can do. Port
methods take `&[Arc<ActivityChunk>]` rather than `Vec` or `impl IntoIterator`:
`Vec` forces a conversion at every call site, and a generic method would make
`ActivityIndex` not object safe, which would break `&dyn ActivityIndex` and with
it the ability to swap indexes at runtime.

`HybridRetriever` **borrows** its children so an expensive dense index can be
built once and shared by several retrievers. A borrowing retriever therefore
cannot accept writes and says so with an error rather than silently discarding
them.
</dependency-policy>

<feature-flags>
Sourcing and retrieval capability is layered by cost. Each tier is a feature.

- `claude-transcripts` -- read `~/.claude/projects/**/*.jsonl`.
- `pieces-archive` -- read the archived Pieces LTM database. Separate from
  `claude-transcripts` because it reads _another application's_ private data
  store. Opens it `immutable=1`; never write to it.
- `bm25` -- the in-house BM25 `ActivityIndex`.
- `ollama` -- the Ollama `Embedder`.
- `hybrid` -- dense retrieval plus RRF fusion. Implies `bm25` and `ollama`.

Rules:

- A feature flag is how an optional backend is selected, not how dead code is
  hidden. If a feature is on by default and never off, delete it.
- Anything behind a feature must compile with the feature **off**. Check with
  `cargo check --no-default-features`.
- Adapters implementing a domain port must pass that port's conformance suite.
  </feature-flags>

<spike-discipline>
This project exists to answer whether local traces can replace Pieces LTM.
That is an empirical question, and `ltmx-probe eval` is how it gets answered.

- Every retrieval change is measured by re-running `eval` against the four
  query categories in the `pieces-ltm` skill.
- `eval` reports whether results were _returned_, not whether they were
  _relevant_. Read the hits. Do not quote the coverage number as a quality
  claim.
- Do not add screen capture (tier 1 or 2) until tier 0 is shown to fall short.
  Text on screen usually came from a file or transcript already indexed.
- If the probe says lexical retrieval fails a category, reach for embeddings
  before reaching for more sources.
  </spike-discipline>

<testing>
Per the workspace testing philosophy:

- **Unit** -- written with every function. Same-file `#[cfg(test)]`.
- **Property** -- invariants over input spaces, not specific values.
- **Fuzz** -- required for anything parsing external bytes. The transcript
  parser has `fuzz/fuzz_targets/parse_transcript.rs`. A fuzz target that finds
  no crash in 60s is table stakes, not done.
- **Conformance** -- every `impl Trait` calls the suite in
  `ltmx_domain::ports::conformance`. The suite itself has a negative test
  proving it can fail; keep it that way.
- **Integration** -- real I/O boundaries, `#[ignore]`d by default so CI does not
  depend on Ollama running. Run them locally.

Integration tests against Ollama have already earned their place: they caught
that Ollama answers with `Transfer-Encoding: chunked` and returns un-normalized
vectors. Neither was visible from unit tests.

- **Fuzz seeds are committed** under `fuzz/seeds/<target>/`. The generated
  corpus is reproducible from them and stays ignored. Adding seeds raised the
  parser's coverage from 763 to 979 edges.
- **Skipped input is reported, never swallowed.** The transcript parser returns
  a `ParseReport`; malformed lines are counted and shown with file and line. A
  truncated trailing line is normal, but a schema change silently emptying the
  index must not be invisible.
  </testing>

<environment>
- Ollama: `127.0.0.1:11434`. Use `127.0.0.1`, never `localhost` -- `localhost`
  resolves to `::1` first here and Ollama binds IPv4 only.
- `nomic-embed-text` returns 768-dimensional, **un-normalized** vectors.
  `DenseIndex` normalizes on write and on query; do not compare raw dot products.
- Ollama replies with `Transfer-Encoding: chunked`. The adapter decodes chunk
  framing.
- Embedding throughput via `/api/embed` is ~5 ms/text batched. Always use
  `Embedder::embed_batch` for bulk work; a per-chunk round trip turns a
  full-corpus pass from minutes into hours.
</environment>

<conventions>
- Rust 2024, edition inherited from the workspace.
- `anyhow::Result` in binaries, `thiserror` in libraries.
- No `unwrap()` in production code. In tests, `expect("why this cannot fail")`.
- No business logic in the composition root.
- Document trait contracts -- preconditions, postconditions, invariants --
  because the conformance suite asserts exactly those words.
</conventions>

</agent-instructions>
