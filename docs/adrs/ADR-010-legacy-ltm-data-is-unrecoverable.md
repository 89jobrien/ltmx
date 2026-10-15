# ADR-010: Legacy Pieces LTM Data Is Unrecoverable

**Status:** Superseded by [ADR-013](ADR-013-legacy-data-is-recoverable.md)
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

> **This ADR is wrong.** Its conclusion is retracted. The database was never
> missing — the search looked in `~/Library/Application Support/com.pieces.os`
> and `~/Library/Containers/com.pieces.os`, but Pieces writes to
> `~/Library/com.pieces.os/`, with no `Application Support` component. Both
> negative results were path mistakes read as absences.
>
> The corpus is 194,431 workstream events carrying **889,095,820 characters of
> OCR'd screen text**. See
> [ADR-013](ADR-013-legacy-data-is-recoverable.md).
>
> Kept unedited below as a record of how a confident negative result was
> manufactured. The mistake worth remembering is not the wrong path — it is
> writing up a conclusion that was already formed, in language ("This closes the
> question permanently") that stopped the next session from looking.

## Context

ltmx began as an attempt to read the existing Pieces LTM index so prior captured
activity stayed searchable. That option was investigated and closed.

Findings:

- `/Applications/Pieces OS.app` (`com.pieces.os`) is installed, but there is no
  `~/Library/Application Support/com.pieces.os` and no
  `~/Library/Containers/com.pieces.os`. The application has written no local
  state.
- `~/Library/Application Support/pieces-cli/` holds only `cli.json`, `logs/`,
  `mcp.json`, `model.json` and `user.json` — **no database**.
- `model.json` lists only chat models (Claude Opus 5 Fast, Gemini-2.0 Flash
  Lite). There is **no embedding model entry**, consistent with the LTM stack
  having been removed from the client rather than merely hidden.
- The vendored PiecesOS Python client contains `LTMNotEnabledError` and
  `LTM_NOT_ENABLED`: LTM is a gated feature that is now off.
- The mac-mini, which held a persistent SSH forward to a PiecesOS instance, is
  **offline for good**.

## Decision

Treat legacy data as unrecoverable. Do not build a legacy reader.

Specifically rejected: reverse-engineering the Pieces LTM SQLite schema to
support a read-only adapter.

## Consequences

- **No bridge mode.** ltmx starts clean and accumulates from the point it is
  installed.
- **No speculative adapter work.** A reader written against an uninspectable
  schema would look finished and be unverified, which is worse than absent.
- **The capability comparison stays qualitative.** Behaviour is measured against
  the query categories in [ADR-005](ADR-005-spike-first-retrieval-decisions.md),
  not against the original product.
- **This closes the question permanently.** Recorded so it is not re-investigated
  on a future session.

## Alternatives Considered

- **Write the legacy reader against the documented API.** This was the most
  promising option — a vendored, generated, typed client exists and would have
  avoided schema archaeology entirely. Closed when the only instance carrying
  that data became unreachable.
- **Rebuild the schema from the vendored models and hope.** Rejected: the client
  describes the API surface, not the storage layout, and there was no instance to
  validate against.
- **Poll periodically in case data reappears.** Rejected: the data is gone, not
  transient.
