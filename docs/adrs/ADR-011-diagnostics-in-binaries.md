# ADR-011: Diagnostics in Binaries, Plain Errors in Libraries

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien
**Related:** [ADR-004](ADR-004-minimize-dependencies.md)

## Context

ltmx uses two error mechanisms. The naive approach is to pick one and use it
everywhere.

miette's `Result` requires errors to implement `Diagnostic`, and that renders
backtraces, source spans and help text. That is exactly right for `ltmx-probe`,
which reads files, parses append-only JSONL and talks to a local model server.

It is exactly wrong for `ltmx-domain`. A `Diagnostic` impl on a published
library type couples every consumer to miette as a dependency, and pulls
backtrace capture into a library that has no business rendering anything.

A second problem surfaced alongside: **the transcript parser silently skipped
malformed lines.** A truncated trailing line is normal for an append-only log,
but a schema change quietly emptying an index is not — and the two are
indistinguishable when both are invisible.

## Decision

- **Libraries** use plain `thiserror` enums (`LtmxError`, `EmbedError`,
  `ParseError`).
- **Binaries** use `miette::{Result, WrapErr, IntoDiagnostic}` and convert at
  the boundary via a small `report` helper.
- **Skipped input is reported, never swallowed.** `parse_transcript` returns a
  `ParseReport { chunks, skipped }` where each `SkippedLine` carries the
  transcript path, 1-based line number, and a `SkipReason`. The probe prints
  them, capped so a systematically mis-parsed corpus cannot flood the terminal.

Bookkeeping entries (lines with no `message`) are expected and are **not**
counted as skips — otherwise the report would be noise.

## Consequences

- **Domain crates stay dependency-light and renderer-agnostic.**
- **A schema change becomes visible.** Currently the parser reports **zero**
  skips across 805 real sessions, which is the result worth having: if that
  number moves, something changed upstream.
- **The cost is one conversion helper** in the binary, which is the correct place
  for the choice to live.
- **The probe gains no source-span rendering yet.** `miette` is wired and
  available; spans would come from `NamedSource` if a diagnostic later needs to
  point into a transcript.

## Alternatives Considered

- **miette everywhere.** Rejected: couples every consumer and puts backtrace
  capture into a domain library.
- **anyhow everywhere.** Rejected: loses type information at exactly the point
  where the binary most benefits from structured context.
- **Keep silently skipping malformed lines.** Rejected: this is the failure mode
  where an index is quietly wrong and nothing says so.
