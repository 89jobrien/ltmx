# ltmx HANDOFF

## Status

| Check                                           | Result                                                                                       |
| ----------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Tests                                           | 125 passing, 0 failing (5 ignored) under default features; **17 more** with `pieces-archive` |
| Clippy                                          | clean, `-D warnings`                                                                         |
| `cargo fmt --all --check`                       | clean                                                                                        |
| `cargo check --workspace --no-default-features` | clean                                                                                        |
| `cargo deny check`                              | advisories ok, bans ok, licenses ok, sources ok                                              |
| `rustqual .`                                    | 84.0%, 38 findings                                                                           |
| Ollama integration (live server)                | 4/4 passing                                                                                  |
| Fuzz (`parse_transcript`)                       | 389,506 executions, 0 crashes                                                                |
| Git                                             | **repository initialized, nothing committed**                                                |

Measured corpus: **57,766 chunks / 805 sessions**, 2026-07-14 → 2026-10-14, 0
skipped lines.

## What this project is

A local-first replacement for Pieces LTM, built to answer one empirical question:
**can local traces alone answer the questions Pieces LTM used to answer?**

The probe answers it partially — two of four query categories are solved well,
one is partial, one is partial-but-improved. See **Findings** below.

## Do this before anything else

**Copy `~/Library/com.pieces.os/production/` somewhere durable.** It is a 4.8 GB
Couchbase Lite database plus a 3.2 GB export, living inside an application
support directory that reinstalling or uninstalling Pieces OS could remove. The
export is older and smaller, so it is not a substitute.

Verified read-only on 2026-10-14:

| What                 | Volume                                                   |
| -------------------- | -------------------------------------------------------- |
| Workstream events    | 194,431 rows                                             |
| Hints                | 409,472 rows                                             |
| OCR screen text      | 186,446 rows, **889,095,820 characters**                 |
| Readable text        | 145,534 rows, 332,424,255 characters                     |
| Workstream summaries | 3,261 rows                                               |
| Vector index         | ~1.33 GB of 512-d vectors (hints, events, tags, signals) |

Always open read-only with `immutable=1`. SQLite refuses read-only access to a
WAL database without it, and the alternative is writing to another
application's data.

## Architecture decisions

All in `docs/adrs/`, with an index and open questions in
`docs/adrs/README.md`. The ones that most shape the code:

- **001** — sourcing is tiered by cost; ltmx is not a screen recorder
- **004** — the dependency budget, and what was deferred and why
- **005** — retrieval decisions are measured, not assumed
- **008** — `Arc`-shared chunks, slice-typed bulk writes for object safety
- **010** — ~~legacy Pieces data is unrecoverable~~ — **wrong, retracted**
- **013** — the legacy corpus was never missing; 889 M chars of OCR text, readable now

## Findings worth carrying forward

1. **Lexical retrieval fails decision archaeology for a diagnosable reason.**
   `colima` is in 485 transcripts, but `why`/`we`/`switch` carry high IDF and
   bury the subject. This is why the embedding tier exists — it was not
   speculative.
2. **Ollama answers `Transfer-Encoding: chunked`** and returns `nomic-embed-text`
   vectors **un-normalized** (raw dot product ≈ 223.8). Both were found by
   integration tests against a live server, not by unit tests.
3. **`localhost` fails for Ollama on this machine** — it resolves to `::1` first
   and Ollama binds IPv4. Use `127.0.0.1`.
4. **Batching is the difference between minutes and hours.** `/api/embed` runs at
   ~5 ms/text; the full corpus embeds in 366s.
5. **Neither index deduplicates.** Dense returns the same chunk from three
   transcripts at an identical score, consuming a third of `top_k`.

## Bugs this project caught in its own code

Recorded because each one has a general lesson:

| Defect                                                                                                                                                                                                                                         | Found by                                                    |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| `HybridRetriever` had two `len()` methods with different meanings; the inherent one shadowed the trait's                                                                                                                                       | a unit test                                                 |
| Markdown `---` separators became index terms                                                                                                                                                                                                   | a unit test (tokenizer kept punctuation-only tokens)        |
| `with_origin` replaced the filter set instead of accumulating, silently dropping the first                                                                                                                                                     | a unit test                                                 |
| The `FakeEmbedder` put all energy in one coordinate, so normalization made every document identical                                                                                                                                            | a ranking test                                              |
| Chunked transfer decoding assumed `Content-Length`                                                                                                                                                                                             | a live-server integration test                              |
| Three `?` sites on `LtmxError` assumed `LtmxError: Diagnostic`, which was never implemented. They lived inside `#[cfg(not(feature = "hybrid"))]` functions, so they only compiled when `hybrid` was **off** — invisible under default features | the feature matrix, `--no-default-features --features bm25` |

## The Pieces archive importer — built

`crates/ltmx-sources/src/pieces.rs`, behind `pieces-archive`, plus
`ltmx-probe archive`.

```sh
cargo run --release -p ltmx-probe --features pieces-archive -- archive
cargo run --release -p ltmx-probe --features pieces-archive -- archive --field ocr
cargo run --release -p ltmx-probe --features pieces-archive -- archive --limit 200
```

| Field      | Records | Chunks  | Elapsed        |
| ---------- | ------- | ------- | -------------- |
| `readable` | 145,534 | 363,791 | 5.7 s          |
| `ocr`      | 186,446 | —       | ~0.2 s / 5,000 |

Span **2026-03-15 → 2026-10-06**. Skips: 5,600 under the 80-char floor, 3 with
no timestamp, 1 malformed — 0.004% timestamp failures, reported not swallowed.

Four things worth carrying forward:

1. **Timestamps came from a byte anchor, not a decoder.** Bodies encode them as
   `0x4F` + length + 27-byte ISO string; the length is always `0x1B`, so
   `4F1B323032362D` marks every one. SQLite's `hex()`/`instr()` slice it inside
   the query, so multi-kB bodies never cross the pipe. The `GBLENDED` decoder
   is still unbuilt and nothing needs it.
2. **`json_object` per row, not TSV.** The text contains newlines, so
   column-separated output cannot be split by line. JSON escapes them, making
   one record one physical line.
3. **SQLite's `hex()` is uppercase.** The Rust mirror was lowercase and the two
   disagreed silently until a test caught it. `anchor_agrees_with_sql` now pins
   them together.
4. **It shells out to `sqlite3`** rather than taking a `rusqlite` dependency,
   per ADR-004. The cost is a runtime requirement on that binary.

**Not yet done:** the importer produces chunks but they are not fed into
retrieval. There is no command that indexes the archive and answers a query
against it, and no embedding cache — 363,791 chunks at ~5 ms each is roughly
30 minutes for a dense pass.

## The embedding cache — built

`ltmx-embed/src/cache.rs`, a decorator over the `Embedder` port. Wired into
`ltmx-probe` with `--model`, `--cache` and `--no-cache`.

| Run  | Time      | Cached | Embedded | Cache          |
| ---- | --------- | ------ | -------- | -------------- |
| Cold | 562.3 s   | 0      | 57,875   | 38,263 entries |
| Warm | **0.3 s** | 57,885 | 0        | 38,266 entries |

118 MB. Keyed on `hash(model_id || text)` plus text length, so an edited
transcript can never be served a stale vector, and a model swap invalidates
rather than mixing vector spaces. Writes are atomic (temp + fsync + rename).

**The bug worth remembering:** the first version split a batch at its first
cache miss and embedded everything after it. Measured wrong — a warm run
embedded **25,556** items when ~3 were new. Transcripts are append-only, so the
corpus grew _during_ the measuring session, the first new chunk landed early,
and everything behind it was re-embedded. Replaced with per-item lookup. The
corpus is never static, so any cache heuristic that assumes it is will be wrong.

Two consequences now affordable:

1. **The model comparison can actually be run.**
   `nomic-embed-text` vs `nomic-embed-text-v2-moe`, both 768-d, separate cache
   files per model.
2. **Dedup got a measurement.** 57,885 chunks → 38,266 distinct texts. A third
   of the transcript corpus is duplicated content, which is the same property
   that makes Roadmap Phase 1.1 worth doing.

## The archive is wired into retrieval — and it does not help yet

`ltmx-probe --archive` adds the Pieces corpus to every command. Corpus is now
~420k chunks spanning **2026-03-15 → 2026-10-14** (was 2026-07-14).
`stats` breaks it down by origin:

```text
  agent_transcript   57887
  screen            363791
```

**The negative result worth carrying forward.** Asking the query lexical
retrieval is known to fail, _"why did we switch to colima"_:

| Corpus                | Top hit                                          |
| --------------------- | ------------------------------------------------ |
| Transcripts only      | `did we run evals?` — generic, matching "did we" |
| Transcripts + archive | Slack threads and editor chrome                  |

Both fail. The archive **changes the failure mode rather than fixing it**: from
generic prompts to application noise. `colima` is in 485 transcripts and now
across months of screen history, but so is Slack, Chrome and every IDE panel,
and those carry more high-IDF query terms than the decision does.

This is the ADR-005 trap firing in the act. Over the combined corpus lexical
reports **4/4 categories returned a result**, up from a failure — and every hit
is still wrong: decision archaeology returns `dotfiles — SKILL.md`. A metric
that counts "did anything come back" scores a confidently wrong answer as a
perfect one.

**The diagnosis, from the new `--origins` filter.** Querying `colima` against
screen origin only returns, in three hits:

```text
linuxbox adapters:: colima:: tests:: lima_home_respects
minibox/docs/diagrams/platform-adapter-selection.md
platform-adapter-selection.md -> detect{Host platform?} -> Linux
```

The archive **contains the answer**. It is out-ranked, not missing. So this is
a ranking problem, not a sourcing or availability problem — which rules out
both "more sources" and "a bigger embedding model" as the fix. It is scoring.

**How self-similar, measured.** The embedding cache is keyed by text, so its
entry count _is_ the distinct-text count. **421,681 chunks collapse to 86,427
distinct texts.** Subtracting the 38,266 transcript entries, the 363,791 archive
chunks hold only ~48,000 distinct strings — an **~87% duplication rate**, far
above the 34% measured on transcripts.

Screen capture repeats itself: menus, toolbars, file trees, the same Slack
sidebar, the same editor chrome, hundreds of thousands of times. This is the
_mechanism_ behind the bad ranking, not just a contributor — the top of the
BM25 results is not merely noisy, it is **the same text over and over**.

Corollary for anyone tempted to add sources: more capture makes this worse
before better.

## Long-run observability — shipped

A 30-minute index run that prints nothing is indistinguishable from a hang,
and one that only writes at the end loses everything on a crash. Both fixed in
`CachingEmbedder` by doing work in **4,096-text slices**:

```text
  4096 / 421681  1%  0.0s  1041136/s  4096 cached, 0 embedded  0s left
 32768 / 421681  8%  0.3s  104861/s  32767 cached, 1 embedded  4s left
  cache flushed: 86428 entries
```

- **Progress** via `with_progress` — processed/total, hits, misses, rate, ETA.
- **Periodic flush** every 50,000 embeddings, so a crash costs a minute of work
  rather than the whole run. Failures are logged, not raised.
- **Bounded memory.** The previous version built one `Vec` of every pending
  text — a ~435 MB duplicate of the corpus held before a single vector returned.

## Known gaps

- **No deduplication.** Decided _where_ to fix it — retriever or fusion stage —
  is still open.
- **Chunk imbalance.** Action chunks are 72% of the corpus and crowd out prompts
  that carry intent.
- **No persistence across runs** — **resolved**, see ADR-014.
- **`HybridRetriever` cannot pass the general conformance suite** because it
  cannot accept writes. Its read-side guarantees are tested directly instead.
- **License texts now present**, and `deny.toml` now derives its allow-list from
  the real resolved graph. `cargo deny check` passes on advisories, bans,
  licenses and sources. Adding a dependency with a new license class will now
  fail the gate instead of passing unnoticed.
- **`repository` metadata is still absent**, because the repo has no git remote.
  A guessed URL would be worse than none. `keywords` and `categories` are set.
- **`[workspace.lints]` is unusable on this toolchain.** `lints.workspace = true`
  is accepted and then ignored — members report `unused manifest key:
package.lints` and no lint flags reach rustc. Verified against a minimal
  scratch workspace and via `cargo clippy -v`. Lint policy therefore lives in
  the gate command. See AGENTS.md.
- **`clippy::pedantic` is measured, not enabled.** ~60 findings across four
  adapter crates, zero in `ltmx-domain` and `ltmx-probe`. About half are noise
  from `assert_is_empty` in tests and `f32` casts in BM25/cosine math. Enabling
  it needs a per-lint allow decision, so it is Joe's call. Breakdown in
  AGENTS.md.
- **`rustqual` scores 84.0%** with 38 findings. 22 of the 38 are in
  `ports/conformance.rs`, where rustqual cannot tell test-helper code from
  production code. The genuinely actionable ones: `parse_transcript` at
  complexity 24 / 106 lines, `post_json` at 80 lines, and `bm25` taking 7
  parameters.

## Next steps, in order

1. **Deduplicate results** and re-run `eval`. Highest-value next change: it is
   currently wasting a third of `top_k` on a single answer.
2. **Persist embeddings** so the probe does not pay 366s per run. This makes every
   subsequent `eval` iteration cheap, which compounds.
3. **Rebalance chunks** — decide between rebalancing at chunk time and weighting
   at scoring time, then measure.
4. **Define the MCP tool contract** and build `ltmx-mcp`. Blocked on the retrieval
   response shape settling, which (1)–(3) will inform.

Do **not** start screen capture. Constitution II: prove tier 0 insufficient
first.

## Environment reminders

- `127.0.0.1:11434`, never `localhost`
- `nomic-embed-text` is installed; `nomic-embed-text-v2-moe` also available
- Use `--release` for anything embedding — a debug build makes query latency
  look like a retrieval problem
- `cargo +nightly fuzz run parse_transcript fuzz/seeds/parse_transcript`

## State of the working tree

Uncommitted. `git init` has run and files were staged early on; the working tree
has moved well past that staging. Nothing has been committed, so `git log` is
empty. Review `git status` before the first commit — nothing has been verified
against a clean checkout yet.
