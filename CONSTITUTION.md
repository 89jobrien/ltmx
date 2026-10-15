# Constitution — ltmx

Project-specific principles for this repo, subordinate to the global
`~/.claude/CONSTITUTION.md`. Where `AGENTS.md` is silent on a case, fall back to
these.

## I. Measure, Do Not Assume

This project replaces a product that can no longer be observed, so intuition is
the one thing it cannot afford. Every retrieval change is re-measured with
`ltmx-probe eval` and the comparison recorded. The coverage number it prints
measures only whether results were _returned_, never whether they were
_relevant_ — read the hits.

The corollary: **reach for retrieval before sourcing.** When a query category
fails, try a different retriever before adding a source tier.

## II. Prove a Tier Is Insufficient Before Building the Next

Activity sourcing is tiered by cost: local traces first, external binaries
second, Objective-C bindings last. Do not build tier N+1 because it seems
obviously better — build it because the probe demonstrated tier N falling short.
This is why ltmx is not a screen recorder.

## III. Ports Live in the Domain

Every trait the domain depends on belongs in `ltmx-domain`, never beside an
adapter. A port defined next to one of its implementations makes the next
implementation import an adapter crate, and inverts the dependency direction.

Write the contract as prose the conformance suite asserts verbatim, so the
documentation and the check cannot drift. Every adapter calls the suite.

## IV. Skipped Input Is Reported, Never Swallowed

A truncated trailing line in an append-only log is normal. A schema change that
silently empties an index is not, and the two are indistinguishable when both
are invisible. Parsers return a report of what they skipped, with enough detail
to locate it.

Corollary: a coverage number that only counts non-empty results is not a result.
Say what was measured.

## V. Dependencies Answer One Question

Every entry in `Cargo.toml` must answer **"why is this not the standard
library?"** When a library is declined for cost, record the reason and the
condition that would reverse it. `nom` and `bytes` are deferred, not rejected;
ADR-004 says what would change that.

## VI. The Domain Does Not Render

Libraries use plain `thiserror` enums. Diagnostics and rendering belong to
binaries. A `Diagnostic` impl in a published crate couples every consumer to the
renderer and pulls backtrace capture into a library that has no business
rendering anything.

## VII. Retrieval Scores Are Not Probabilities

BM25 scores, cosine similarities and fused RRF scores are three different,
incomparable scales. Never blend them without normalizing, and never present
any of them to a caller as a confidence. `HybridRetriever` fuses by _rank_
precisely because the scales do not agree.

## VIII. Environment Facts Are Not Decisions

Facts about the machine — the Ollama endpoint, chunked transfer encoding,
un-normalized vectors, the `::1` resolution trap — belong in `AGENTS.md`, not
in an ADR. They change without anyone deciding anything.

A decision belongs in `docs/adrs/`, and an ADR is written when a decision is
made, not after. The reasoning is the value; the conclusion is cheap.
