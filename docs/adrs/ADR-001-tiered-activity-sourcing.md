# ADR-001: Source Activity From Durable Traces, Not Screen Capture

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

Pieces shipped "Long-Term Memory" as the **Workstream Pattern Engine**: a
continuous macOS capture pipeline gated behind screen-recording and
accessibility permissions, with a QR-code calibration step used to teach the
engine to ignore the window it is calibrating against. It surfaced work from
conversations, code snippets, browser activity, clipboard history and git
activity. Pieces disabled LTM for users, and the project exists to replace it.

The intuitive replacement is to rebuild the capture pipeline. The obvious
reading of "LTM recorded what was on my screen" is "therefore I must record what
is on my screen".

That reading is wrong, and expensive to get wrong.

## Decision

Source activity in **cost-ordered tiers**, each behind a feature flag, and prove
each tier insufficient before building the next.

| Tier | Sources                           | Cost                              | Feature              |
| ---- | --------------------------------- | --------------------------------- | -------------------- |
| 0    | Agent transcripts, git, notes     | none — read state already on disk | `claude-transcripts` |
| 1    | `screencapture` + `tesseract`     | one `brew install`, no FFI        | _planned_            |
| 2    | Vision.framework OCR, AXUIElement | Objective-C bindings              | _planned_            |

Tier 0 ships first. Tiers 1 and 2 are not built until `ltmx-probe eval`
demonstrates a failure that sourcing -- rather than retrieval -- causes.

The reasoning: **text that would be OCR'd off a screen mostly came from a file
or a transcript that is already indexed at tier 0.** Code on screen came from a
repo. Docs came from a file. Terminal output came from a shell. Tier 1 and 2
uniquely add browser rendering, chat UIs and PDFs.

Tier 2 cannot be done in pure Rust at all. Vision.framework and the
accessibility API are Objective-C, which is why it is a separate, optional
tier rather than the default path.

## Consequences

- **The project is answerable before it is speculative.** Tier 0 is a parser
  over `~/.claude/projects/**/*.jsonl`, git and notes.
- **Build cost is deferred, not avoided.** Tiers 1 and 2 remain real work with
  macOS permission gating and a mandatory screen-recording indicator.
- **Historical screen activity _is_ recoverable.** This ADR is about capturing
  activity going forward; the retrospective corpus is a separate source tier,
  and it turns out to exist. See
  [ADR-013](ADR-013-legacy-data-is-recoverable.md). This does not weaken the
  tiering — importing a 2026 archive and recording the user's screen from today
  are different acts, and only the second needs permissions.
- **A screen capture daemon is a different and larger project** than the
  retrieval system being built now. The two must not be conflated.

## Alternatives Considered

- **Rebuild the capture pipeline first.** Rejected: it is the highest-cost
  path, gated behind OS permissions, and it re-derives text that tier 0 already
  holds.
- **OCR the screen once and index the results.** Rejected for the same reason,
  and additionally because screenshots are large, unsearchable, and a standing
  liability if stored.
- **Tier 0 only, permanently.** Not rejected so much as deferred — if tier 0
  turns out to cover the four query categories, tiers 1 and 2 may never be
  built. See [ADR-005](ADR-005-spike-first-retrieval-decisions.md).
