# ADR-007: Fuse Retrievers by Reciprocal Rank Fusion

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

Lexical and dense retrieval fail differently and excel at different things. On
the measured evaluation set, lexical is right for prior art and current project;
dense finds files the lexical index ranks below low-information action chunks.
Neither dominates, so both should contribute.

Combining them is not straightforward: BM25 scores and cosine similarities are
**not on a comparable scale**. A BM25 score of 7.6 and a cosine of 0.71 carry no
relative meaning against each other. Any scheme that blends raw scores needs
per-query score normalization, which is fragile and hard to tune.

## Decision

Combine by **rank**, not by score, using Reciprocal Rank Fusion.

```rust
score(chunk) = Σ_over_lists  1 / (k + rank_in_list)
```

with the standard `k = 60`. A chunk both retrievers rank highly outranks one
that only a single retriever liked.

`HybridRetriever` overfetches — each child is asked for `4 × top_k` — because
fusion rewards agreement, and a truncated candidate list hides chunks that two
retrievers both ranked moderately.

## Consequences

- **No score calibration is needed**, which removes a whole class of tuning.
- **Fusion is deterministic.** Ties break on first-seen order rather than hash
  iteration order, so the same query returns the same ranking every run.
- **`k` is the only tuning knob.** Larger values flatten the contribution of
  top ranks; `k = 60` is the conventional default.
- **Result scores are not interpretable.** An RRF score is a ranking signal,
  not a relevance probability. Callers must not treat it as a confidence.

## Alternatives Considered

- **Normalize scores, then blend.** Rejected: per-query normalization is
  sensitive to the shape of a single result set, and a query with one strong hit
  rescales everything else differently than a flat result set.
- **Concatenate both result lists.** Rejected: ordering would depend on which
  retriever is listed first rather than on agreement.
- **Dense only.** Rejected by measurement — dense ranked prior art weak.
