# ADR-005: Retrieval Decisions Are Spike-Driven, Not Assumed

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

ltmx is a replacement for a product it can no longer observe. Pieces is
disabled, so there is no way to compare behaviour directly. The only honest
way to know whether a retrieval change helps is to measure it.

The risk this guards against is building a retrieval architecture on assumed
properties and discovering the assumptions were wrong.

## Decision

Maintain a fixed evaluation set — the four query categories from the `pieces-ltm`
skill — and re-run it against every retrieval change:

| Category             | Example query                                  |
| -------------------- | ---------------------------------------------- |
| temporal             | "what was I working on in the devloop project" |
| decision archaeology | "why did we switch to colima"                  |
| prior art            | "parse op env vars"                            |
| current project      | "minibox ci pipeline"                          |

These are the skill's own documented query patterns, which makes them the
acceptance criteria rather than a guess. `ltmx-probe eval` runs them under
every compiled strategy and prints the hits.

Measured results on 57,766 chunks / 805 sessions:

| Category             | Lexical  | Dense   | Hybrid   |
| -------------------- | -------- | ------- | -------- |
| prior art            | good     | weak    | good     |
| current project      | good     | good    | good     |
| temporal             | mediocre | weak    | mediocre |
| decision archaeology | **fail** | partial | partial  |

Rules:

- **Every retrieval change is re-measured**, and the comparison is recorded.
- **The coverage number is not a quality claim.** It measures only whether
  results were _returned_, not whether they were _relevant_. This caveat is
  recorded in `AGENTS.md` so it is not later quoted as a result.
- **Reach for retrieval before sourcing.** If a category fails, try a different
  retriever before adding a new source tier.

## Consequences

- **One predicted failure was confirmed.** Lexical retrieval fails decision
  archaeology — not from a corpus gap (`colima` appears in 485 transcripts and
  is found instantly by a content-word query) but because `why`, `we` and
  `switch` carry high IDF and bury the subject. This is the concrete
  justification for the embedding tier.
- **The metric exposed its own weakness.** The first version reported `4/4` for
  a run where one category was clearly wrong, which is why the caveat above
  exists.
- **Known gaps are recorded rather than hidden.** Neither index deduplicates —
  dense returned the same chunk from three files at an identical score — and
  action chunks are 72% of the corpus and crowd out prompts that carry intent.
- **`eval` is not a CI gate.** It needs a live model server and minutes of
  runtime. The gate is the conformance suite; `eval` is an instrument.

## Alternatives Considered

- **Ship an index and tune it by intuition.** Rejected: intuition is exactly
  what this project cannot afford, having lost the reference implementation.
- **Benchmark against synthetic queries.** Rejected: synthetic phrasing would
  measure the synthetic generator. The real queries are the ones the skill
  documents.
