# ADR-013: The Legacy Pieces Data Was Never Missing

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien
**Supersedes:** [ADR-010](ADR-010-legacy-ltm-data-is-unrecoverable.md)

## Context

[ADR-010](ADR-010-legacy-ltm-data-is-unrecoverable.md) concluded that the
Pieces LTM corpus was unrecoverable, and closed the question so it would not be
re-investigated. That conclusion was wrong. The data is present, current, and
large.

ADR-010 looked in two places:

- `~/Library/Application Support/com.pieces.os`
- `~/Library/Containers/com.pieces.os`

Both are absent, and both absences were read as "the application has written no
local state". The database is not in either. It is at:

```text
~/Library/com.pieces.os/production/
```

— directly under `~/Library`, with no `Application Support` component. The
negative result was a path mistake, not an absence, and it was recorded as an
absence because the conclusion was already formed.

## Findings

All counts read from `~/Library/com.pieces.os/production/Pieces/couchbase.cblite2/db.sqlite3`,
a 4.8 GB Couchbase Lite database, opened with `immutable=1` so that SQLite could
not write to it. `immutable=1` skips WAL replay, so these are **lower bounds**.

### Documents

| Collection                                 | Rows    |
| ------------------------------------------ | ------- |
| `workstream\Events`                        | 194,431 |
| `hints`                                    | 409,472 |
| `workstream\Summaries`                     | 3,261   |
| `workstream\Pattern\Engine\Sources`        | 93      |
| `workstream\Pattern\Engine\Source\Windows` | 1,182   |
| `annotations`                              | 12,216  |
| `persons`                                  | 900     |
| `activities`                               | 699     |

Empty and notable: `workflow\Mapping\Time\Based\Clusters` (0),
`conversation\Memories` (1), `entities` (0), `files` (0), `ocr\Analyses` (0),
`code\Analyses` (18).

### Recoverable text

Each collection has a companion SQLite FTS5 table ending `::Full\Text\Search\Index_content`
whose columns hold the indexed field values. **No format reverse-engineering is
required to read this** — it is ordinary SQL over a text column.

| Field                                     | Rows    | Characters  |
| ----------------------------------------- | ------- | ----------- |
| `c10context.native_ocr.ocrText`           | 186,446 | 889,095,820 |
| `c0readable`                              | 145,534 | 332,424,255 |
| `c6context.native_clipboard.content.text` | 6,343   | 15,411,888  |
| `hints.c0text`                            | 409,472 | 16,195,131  |

**889 million characters of OCR'd screen text** and 332 million of
application-readable text, covering terminal buffers, editor contents, browser
windows and system dialogs. Sampled rows are coherent prose, not fragments:

```text
[System Settings]
Screen & System Audio Recording
Allow the applications below to record the content of your scree...
```

Top window titles by frequency: `Claude Code` (8,251), `minibox` (4,820),
`dev` (596), `devloop` (572), `langchainx — mod.rs` (468).

### Vector index

`~/Library/com.pieces.os/production/Pieces/vector_db/` holds a parallel index of
512-dimensional vectors, `stride_bytes = 2048`, `schema_version = 8`:

| Collection                       | Vectors | `.vec` size |
| -------------------------------- | ------- | ----------- |
| `hints`                          | 406,583 | 832.6 MB    |
| `workstreamEvents`               | 194,484 | 398.3 MB    |
| `tags`                           | 42,246  | 86.5 MB     |
| `signals`                        | 4,263   | 8.7 MB      |
| `workstreamSummaries`            | 3,283   | 6.7 MB      |
| `workstreamPatternEngineSources` | **0**   | 0 B         |

About 1.33 GB of vectors. Note the last row: 93 source documents exist but their
vector index is empty, so that collection was never embedded.

### Export copy

`~/Library/com.pieces.os/production/Pieces_1784711611646_93e2e65d-….zip`
(3.2 GB, generated 2026-07-22) contains `couchbase.cblite2/db.sqlite3` at
3.78 GB with **fewer** rows than the live database (146,533 events, 297,809
hints). It is a backup, not the primary, and not the better source.

### The body format

Document bodies in the `kv_` tables are a tagged binary envelope beginning
`GBLENDED` (gzip + `BLENDED`), containing an `O$<uuid>` origin block followed by
tagged fields — for example `GUNKNOWNIOS_SERVEREMACOSDOPEN` on a short record.
This is where **timestamps, application identity and structured context live**.

A full decoder was judged unnecessary, and that turned out to be right. Bodies
encode timestamps as `0x4F` + a length byte + a 27-byte ISO-8601 string. Across
the span this archive covers, that length byte is always `0x1B`, so the byte
sequence `4F 1B 32 30 32 36 2D` ("`O`", 27, "`2026-`") marks every timestamp in
the corpus. SQLite's `hex()` and `instr()` locate and slice it inside the query,
so the import never ships a multi-kilobyte body across a pipe.

Measured: **194,428 of 194,431** bodies contain the anchor, and **194,425** of
those decode to a valid ISO-8601 string spanning **2026-03-15 → 2026-10-06**.
The corpus's time range is therefore known without a decoder. A real one is
still worth having eventually, but nothing waits on it.

## Decision

The legacy corpus is recoverable. Build a read-only importer for it.

This reverses ADR-010's conclusion while keeping its reasoning about _not_
building speculative readers: that concern was correct and is now discharged by
the opposite means — the schema was inspected and measured, not guessed at.

Concretely:

1. **Preserve the data before anything else.** See Consequences.
2. Add a `pieces-archive` source tier that reads the FTS content tables. This is
   a small, well-specified SQL surface and needs no format work. **Done.**
3. Treat a full body decoder as a separate, later increment. The timestamp
   anchor removes the reason to wait for it.
4. Do **not** reuse Pieces' 512-dimensional vectors. The dimension, the model
   that produced them, and the text they were derived from are all recoverable,
   but re-embedding ltmx's own corpus with `nomic-embed-text` is cheaper and
   keeps one embedding space.

## What was built

`crates/ltmx-sources/src/pieces.rs` behind the `pieces-archive` feature, plus an
`ltmx-probe archive` subcommand.

```sh
cargo run --release -p ltmx-probe --features pieces-archive -- archive
cargo run --release -p ltmx-probe --features pieces-archive -- archive --field ocr
cargo run --release -p ltmx-probe --features pieces-archive -- archive --limit 200
```

Measured against the live 4.8 GB database:

| Field      | Records scanned | Chunks  | Elapsed          |
| ---------- | --------------- | ------- | ---------------- |
| `readable` | 145,534         | 363,791 | 5.7 s            |
| `ocr`      | 186,446         | —       | ~0.2 s per 5,000 |

Skips are counted and broken down by reason, because unread input is reported
rather than swallowed: of the `readable` records, 5,600 fell under the
80-character floor (boilerplate dialogs), 3 had no timestamp and 1 was
malformed. That 0.004% timestamp failure rate matches the SQL validation above.

### Decisions worth recording

- **It shells out to `sqlite3`.** Reading SQLite from Rust means `rusqlite` and
  a vendored SQLite C build. ADR-004 requires every dependency to justify
  itself, and for a one-shot archival import a system binary does the job with
  nothing added to the graph. The cost is a runtime requirement, which is
  checked and reported rather than assumed.
- **Every query opens with `immutable=1`.** SQLite will not open a WAL database
  read-only without it, and the alternative risks writing to another
  application's data. This is not configurable.
- **`json_object` per row, not TSV.** The text contains newlines, so
  column-separated output cannot be split by line. Wrapping each record in a
  JSON object escapes them, so one record is one physical line and the stream
  parses incrementally instead of buffering 889 M characters.
- **`Origin::Screen`, not a new variant.** The archive _is_ captured screen
  content, so it flows through the existing origin filter for free.
- **The window title prefixes every chunk.** "What was I doing in minibox" is
  answered by the title far more often than by the body text, and a chunk is
  indexed independently.
- **Sessions are grouped by day.** The archive carries no session identifier.
  A day is a defensible proxy for a continuous stretch of work, and it is
  labelled as a proxy rather than presented as something Pieces recorded.
- **A test pins the Rust and SQL paths together.** SQLite's `hex()` emits
  uppercase; an early version of the Rust mirror emitted lowercase and the two
  disagreed silently. `anchor_agrees_with_sql` exists so that cannot recur.

### Measured: the archive does not yet help the failing category

Wiring the archive into retrieval (`ltmx-probe --archive`) and asking the query
that lexical retrieval is known to fail — _"why did we switch to colima"_:

| Corpus                | Top hit                                                     |
| --------------------- | ----------------------------------------------------------- |
| Transcripts only      | `did we run evals?` — a generic prompt matching on "did we" |
| Transcripts + archive | Slack threads and editor chrome from captured screen        |

**Both fail.** The archive does not fix decision archaeology; it changes the
failure mode from _generic question prompts_ to _application noise_. `colima`
appears in 485 transcripts, and it now also appears across months of screen
history — but so does Slack, Chrome, and every IDE panel, and those carry more
high-IDF query terms (`we`, `did`, `why`) than the decision itself does.

This is a negative result and it is the useful kind: it rules out "just add the
data" as a fix for retrieval quality. The archive raises coverage without
raising relevance, which is exactly the failure
[ADR-005](ADR-005-spike-first-retrieval-decisions.md) was designed to detect,
because a coverage number alone would have scored this as a success.

### The diagnosis: present but buried, not absent

Adding `--origins` — the filter `SearchParams` always supported and nothing
exposed — settles what kind of problem this is. Querying `colima` against
**screen origin only**:

```text
1. [3.4509] linuxbox adapters:: colima:: tests:: lima_home_respects
2. [3.3905] minibox/docs/diagrams/platform-adapter-selection.md
3. [3.3763] platform-adapter-selection.md -> detect{Host platform?} -> Linux
```

That is the decision, in three hits, from months of screen history. **The
archive contains the answer.** It is out-ranked by Slack threads and editor
chrome because chrome carries more high-IDF query terms than the decision
itself does.

So the problem is ranking, not sourcing and not availability:

- Screen text is mostly chrome, and chrome is _lexically_ competitive with
  content because it is saturated with the same high-frequency words a question
  uses.
- `Origin::Screen` is now reachable from the CLI, which makes the hypothesis
  testable per-query rather than per-corpus.
- The fix is therefore not a new source and not a new embedding model. It is
  scoring: demote screen content, or restrict it to windows carrying signal.

What the archive does establish:

- The corpus now spans **2026-03-15 → 2026-10-14** rather than 2026-07-14, and
  retrieval can reach seven months of terminal output, editor content and
  command results.
- The same 34% self-similarity the embedding cache measured on transcripts is
  presumably worse here, which is what makes
  [Roadmap](../../ROADMAP.md) Phase 1.1 (deduplication) a prerequisite for
  measuring any scoring change, not merely the next improvement.

## Consequences

- **ADR-010's premise is retracted.** It is marked superseded rather than
  deleted, because a confidently-wrong negative result that closed a question is
  worth keeping as a record of how it was wrong.
- **The fragility is real and immediate.** The live 4.8 GB database sits inside
  an application support directory that a reinstall or uninstall of Pieces OS
  could remove. There is no second copy: the export is smaller and older. Copy
  the directory somewhere durable **before** writing any importer code.
- **This changes the tiering rationale, not the tiering itself.** The corpus
  proves the value of screen-derived text — 889 M characters of it — which is
  evidence _for_ ltmx's problem existing. It is not evidence that ltmx should
  build a screen recorder. Reading a historical archive is categorically
  different from capturing the user's screen going forward, and the second still
  requires demonstrating that tier 0 falls short.
- **A capability comparison becomes possible.** ltmx can now be measured against
  the real corpus rather than against a description of it.
- **Retrieval quality questions change shape.** Decision archaeology, which
  lexical retrieval currently fails, has 194k events and 3,261 workstream
  summaries to draw on. That is a much better test set than what exists now.

## Alternatives Considered

- **Keep ADR-010 and re-investigate later.** Rejected: it was investigated, the
  answer was wrong, and leaving it in place means the next session either
  repeats the work or inherits the false conclusion.
- **Reverse-engineer the `GBLENDED` body format first.** Rejected as the
  starting point: the FTS tables give most of the text with no decoding at all.
  The decoder is worth doing, but only once there is evidence the FTS-only
  corpus is insufficient.
- **Import Pieces' existing vectors to skip re-embedding.** Rejected: the model
  is unidentified, the dimension (512) does not match ltmx's 768, and mixing
  embedding spaces in one index would corrupt retrieval rather than accelerate
  it.
- **Point ltmx at the live database directly at query time.** Rejected: it makes
  a third-party app's internal schema a runtime dependency of a local tool, and
  any schema change breaks retrieval with no compile-time signal. Import to
  ltmx's own format once, then be independent.

## Verification

Every count above was read live, read-only, on 2026-10-14:

```sh
DB="~/Library/com.pieces.os/production/Pieces/couchbase.cblite2/db.sqlite3"
sqlite3 "file:$DB?immutable=1" "select count(*) from \"kv_.workstream\\Events\";"
```

`immutable=1` is mandatory. Without it SQLite refuses to open a WAL database
read-only, and without read-only mode there is a real risk of corrupting another
application's data.
