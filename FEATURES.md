# Features

What ltmx does today, and what it deliberately does not. Status is measured,
not intended — anything marked _planned_ has not been built, and anything marked
_blocked_ is not buildable in the current environment.

## Shipped

### Ingestion — Claude Code transcripts

`claude-transcripts` · `ltmx-sources`

Reads `~/.claude/projects/**/*.jsonl` and produces two kinds of chunk:

- **Prompts** — what you asked. High signal for intent and decision archaeology.
- **Actions** — tool invocations rendered as text (`Bash: cargo xtask test`,
  `Read: /repo/src/store.rs`). High signal for what you were actually doing.

Command-injection echoes and bookkeeping entries are filtered. Chunk ids derive
from the entry `uuid`, so re-reading an appended transcript is idempotent.
Every chunk carries a `path:line` citation.

Measured: **57,766 chunks across 805 sessions**, 2026-07-14 → 2026-10-14, zero
malformed lines skipped.

### Ingestion — the archived Pieces corpus

`pieces-archive` · `ltmx-sources`

Reads the Pieces OS database that recorded roughly seven months of working
activity before its LTM stack was switched off. Text comes from the document
store's SQLite FTS tables; timestamps are recovered from a validated byte
anchor in the document bodies. Nothing is written to the Pieces database —
every query opens `immutable=1`.

| Field      | Records | Chunks  | Volume      |
| ---------- | ------- | ------- | ----------- |
| `readable` | 145,534 | 363,791 | 332 M chars |
| `ocr`      | 186,446 | —       | 889 M chars |

Measured span **2026-03-15 → 2026-10-06**. A full import of the `readable`
field takes **5.7 seconds**. 5,600 records were skipped as under the
80-character floor; 4 lacked a usable timestamp. See
[ADR-013](docs/adrs/ADR-013-legacy-data-is-recoverable.md).

### Retrieval — lexical

`bm25` · `ltmx-lexical`

Dependency-free BM25 over chunk text. No model, no network, sub-second over the
full corpus. Honors origin filters and time windows.

### Retrieval — dense

`ollama` · `ltmx-retrieve`, `ltmx-embed`

`nomic-embed-text` (768-d) via a local Ollama server, over a hand-rolled HTTP
client. Exact cosine scan with vectors normalized on write and query.

Embedding a full corpus takes **~366 seconds**; batching is mandatory.

### Caching

`ltmx-embed` · no feature flag

`CachingEmbedder` decorates any `Embedder` and serves repeats from disk, keyed
by a hash of the model id and the text. Measured on the transcript corpus:

| Run  | Time      | Cached | Embedded |
| ---- | --------- | ------ | -------- |
| Cold | 562.3 s   | 0      | 57,875   |
| Warm | **0.3 s** | 57,885 | 0        |

118 MB for 38,266 distinct vectors — keying on text collapses 57,885 chunks
onto 38,266 entries, a 34% reduction that comes from the corpus's own
self-similarity. A model swap invalidates the cache rather than mixing vector
spaces. See
[ADR-014](docs/adrs/ADR-014-embedding-cache.md).

### Retrieval — hybrid

`hybrid` · `ltmx-retrieve`

Reciprocal rank fusion of lexical and dense. Fuses by rank because BM25 scores
and cosine similarities are not comparable scales.

`HybridRetriever` borrows its children so the embedding pass happens once.

### The probe

`ltmx-probe`

`stats`, `ask --strategy`, and `eval` — the instrument that answers whether local
traces can replace Pieces LTM. `eval` runs the four query categories from the
`pieces-ltm` skill under every compiled strategy.

### Diagnostics

`ParseReport` surfaces skipped transcript lines with file and line rather than
swallowing them. `miette` renders binary errors; libraries stay on `thiserror`.

## Measured retrieval quality

Against 57,766 chunks / 805 sessions. Read the hits, not the coverage count —
that number measures only whether results were returned.

| Category             | Lexical  | Dense    | Hybrid   |
| -------------------- | -------- | -------- | -------- |
| prior art            | good     | weak     | **good** |
| current project      | good     | **good** | good     |
| temporal             | mediocre | weak     | mediocre |
| decision archaeology | **fail** | partial  | partial  |

Lexical fails decision archaeology for a diagnosed reason: `colima` appears in
485 transcripts, but `why`, `we` and `switch` carry high IDF and bury the
subject. Content-word queries find it instantly. This is the concrete
justification for the embedding tier.

## Not built

| Capability              | Status  | Why                                                                          |
| ----------------------- | ------- | ---------------------------------------------------------------------------- |
| MCP server              | planned | The tool contract depends on a retrieval shape that is not settled yet.      |
| git history ingestion   | planned | Tier 0, same shape as transcripts. Not started.                              |
| notes ingestion         | planned | Tier 0, same shape. Not started.                                             |
| Incremental / watcher   | planned | Deliberately after retrieval quality is good enough to observe.              |
| Persistence across runs | planned | The 366s embedding pass is paid every run. Caching is the obvious next step. |
| Cross-encoder reranking | planned | Third retriever; `rrf` already fuses two.                                    |
| Code-aware chunking     | planned | Only if the code queries in the eval set demand it.                          |

## Blocked or declined

| Capability                    | Status      | Why                                                                                                                                                                             |
| ----------------------------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Legacy Pieces LTM reader      | **shipped** | `pieces-archive` reads the 4.8 GB archive: **145,534 records → 363,791 chunks in 5.7 s**, timestamps recovered. See [ADR-013](docs/adrs/ADR-013-legacy-data-is-recoverable.md). |
| Screen capture (tier 1)       | deferred    | Requires demonstrating tier 0 falls short.                                                                                                                                      |
| Vision.framework OCR (tier 2) | deferred    | Objective-C; no pure-Rust path exists.                                                                                                                                          |
| Accessibility tree            | deferred    | Objective-C; no pure-Rust path exists.                                                                                                                                          |
| Network embeddings            | declined    | Would ship the user's transcripts off-machine on every index build.                                                                                                             |

## Explicit non-goals

- **Being a screen recorder.** ltmx reconstructs activity from traces. See
  [ADR-001](docs/adrs/ADR-001-tiered-activity-sourcing.md).
- **Generating text.** ltmx retrieves. It does not summarize or answer; callers
  decide what to do with evidence. (Note: `pieces-ob/scripts/standup.py`
  depends on Pieces' `pieces ask` generation surface and will **not** be
  satisfied by an ltmx retrieval adapter.)
- **Cloud anything.** No remote embedding, no remote index, no telemetry.
