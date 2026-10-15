# Roadmap

Where ltmx is going, and the evidence each stage requires before the next one
starts. Dates are deliberately absent: the gating condition is a measurement, not
a deadline.

## Current state

A working retrieval system over local traces, and an instrument that measures
whether it answers real questions. Two of four query categories are solved well;
two are partial. See `FEATURES.md`.

The question ltmx was built to answer is **partially answered**, which is enough
to justify continuing and not enough to declare victory.

---

## Phase 1 — Retrieval Quality

The highest-value work, because it changes what every later phase is built on.

### 1.1 Deduplicate results

**Why first:** dense retrieval returns the same chunk from three transcripts at
an identical score. A third of `top_k` is currently spent on one answer. No
other improvement can be measured cleanly until this is fixed.

**Measured 2026-10-15.** Counted directly with `COUNT(DISTINCT)` on the source
tables, not inferred from a cache:

| Collection            | Rows    | Distinct | Duplicate rate |
| --------------------- | ------- | -------- | -------------- |
| Agent transcripts     | 57,890  | 38,266   | **34%**        |
| Pieces archive (text) | 145,534 | 144,481  | **0.7%**       |

The transcripts are the duplicated corpus. The archive is not — each captured
screen yields genuinely distinct text, so adding it does not worsen this. Across
the whole combined corpus, 421,805 chunks hold 397,676 distinct texts: **5.7%**.
Dedup is a transcript problem, not a general one, which narrows where it is
worth fixing.

Worth separating **records** from **chunks**: 145,534 archive records become
363,791 chunks because each is split at ~1,200 characters. That is chunking,
not repetition, and conflating the two overstates duplication by an order of
magnitude. An earlier draft of this file made exactly that mistake, inferring a
duplicate rate from a cache that was still filling.

**Open decision:** dedup in the retriever, or in the fusion stage. Recorded as
open in `docs/adrs/README.md`.

**Exit:** no query returns two chunks with identical text in the top `k`.

### 1.2 Rebalance chunks

**Why:** action chunks are **72% of the corpus** and crowd out the prompts that
carry intent. The prompts are the high-signal content and they are the minority.

**Open decision:** rebalance at chunking time, or weight at scoring time.

**Measured 2026-10-14 — no longer open.** With the Pieces archive loaded,
lexical retrieval returns **4/4 categories** where it previously failed
decision archaeology, and every hit is still wrong: the colima query returns
`dotfiles — SKILL.md`. `--origins screen` returns the correct answer in three
hits. The material is present and the ranking is wrong, so the fix is **weighting
at scoring time**, not re-chunking.

That also moves this ahead of 1.3. A chunk-balance fix is now a _prerequisite_
for measuring anything downstream: without it, coverage rises while relevance
does not, and the eval cannot distinguish a real improvement from noise.

**Exit:** the `temporal` and `decision archaeology` categories improve on the
measured baseline in `docs/adrs/ADR-005`, judged on the **hits**, not the count.

### 1.3 Cross-encoder reranking

**Why:** only after 1.1 and 1.2, because reranking a result set containing
duplicates and wrong-origin chunks optimizes the wrong thing. `ragrs` already has
a Candle cross-encoder adapter to borrow from.

**Exit:** reranking improves a measured category without regressing another.

---

## Phase 2 — Persistence

### 2.1 Cache embeddings

**Why:** the dense path pays a **366-second** pass over 57,766 chunks on every
run. Every subsequent `eval` iteration pays it again. This is the single change
that makes all other measurement cheaper, so it compounds.

**Exit:** a second `eval` run embeds nothing.

### 2.2 File-backed index

**Why:** the index is in-memory and dies with the process. A long-lived service
needs one that survives restart.

**Candidate:** `sqlite-vec` `vec0`. `memx` already uses it for exactly this, and
`HybridRetriever`'s borrowing design was chosen partly to keep that option open.
Deferred from the current design because it was not needed yet — see
[ADR-012](docs/adrs/ADR-012-exact-cosine-search.md).

**Exit:** the service restarts and answers without a full rebuild.

---

## Phase 3 — Sourcing

Tier 0 only, per [ADR-001](docs/adrs/ADR-001-tiered-activity-sourcing.md).

### 3.1 Git history

Commit messages carry decisions with their context. Same chunk shape as
transcripts, so this is mostly a parser.

### 3.2 Notes

The Obsidian vault is already indexed by Obsidian's own search; what ltmx adds
is joining notes to the transcript and git activity around the same time.

**Exit:** a query spans sources — a decision found in a note links to the session
where it was made.

### 3.3 Tiers 1 and 2 — deferred, not planned

Screen capture (`screencapture` + `tesseract`) and then Objective-C bindings
(Vision.framework OCR, accessibility tree) are **not scheduled**. They start
only when the probe demonstrates that a _sourcing_ gap exists — not a retrieval
gap. Constitution II.

Worth restating what tier 2 costs: macOS screen-recording and accessibility
permissions, and a permanent recording indicator with no supported bypass.

---

## Phase 4 — Serving

### 4.1 MCP server

**Blocked on:** the retrieval response shape settling. Phases 1 and 2 change what
a result contains, and the tool contract should be designed against the final
shape rather than revised twice.

When it happens, the contract is already constrained by existing consumers:

- **`ask_pieces_ltm`** must keep its name and intent. Client skills reference the
  tool name, not the server name, so the name _is_ the migration surface.
- **HTTP on `127.0.0.1:39300`** with `/.well-known/health`. The `pieces-ltm` skill
  classifies readiness with an HTTP health probe and a 15-second `ask_pieces_ltm`
  canary. A stdio server has no port to probe and would require rewriting the
  skill.
- **Both transports and both protocol versions**: Claude Code uses streamable
  HTTP at `/model_context_protocol/2025-03-26/mcp`; Zed and OpenCode use SSE at
  `/model_context_protocol/2024-11-05/sse`.
- **Eager embedder warm-up.** The canary budget is 15 seconds. A lazy model load
  blows it, every session gets classified `live-not-ready`, and the failure looks
  like a broken memory system rather than a cold start.
- **Bounded context.** `top_k` and a character budget, so the tool cannot flood
  an agent's window.

### 4.2 Repoint existing clients

`my-pieces-memory` in `~/.claude.json`, the Zed and OpenCode configs, and the
`pieces-ltm` skill's readiness protocol. See
`~/.claude/skills/pieces/references/mcp-platforms.md`.

---

## Phase 5 — Retrieval Depth

Only if the evaluation set says so.

- Code-aware chunking (tree-sitter symbol boundaries with path, line range and
  symbol on every chunk) — the main known weakness of any text-search approach
  over source.
- Session clustering: reconstruct work ranges from activity and label each with
  goal, decisions and open threads, which is what `memx::SessionLog` already
  models.
- A third retriever, if one earns its place in the fusion.

---

## Explicitly Not Planned

| Item                       | Why                                                                                                                                            |
| -------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Legacy Pieces LTM reader   | **Revived — see [ADR-013](docs/adrs/ADR-013-legacy-data-is-recoverable.md).** Was declined on a wrong premise; the corpus exists.              |
| Generation / summarization | ltmx retrieves. `pieces-ob/scripts/standup.py` needs Pieces' `pieces ask` generation surface and will not be satisfied by a retrieval adapter. |
| Cloud anything             | No remote embedding, index, or telemetry.                                                                                                      |
| GUI or TUI                 | The probe is the interface.                                                                                                                    |
| Publishing to crates.io    | Not yet. No `repository`/`keywords` metadata, no `deny.toml`, no CHANGELOG discipline.                                                         |

---

## How to Read This

The gates above are **measurements**, not milestones. A phase is done when its
exit condition is observed in `ltmx-probe eval`, not when its code is written.
If an exit condition turns out to be unachievable, that is information worth
having early — which is the entire reason this project started with a spike
instead of an architecture.
