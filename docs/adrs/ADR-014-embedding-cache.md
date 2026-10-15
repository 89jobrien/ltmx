# ADR-014: Embeddings Are Cached on Disk, Keyed by Text

**Status:** Accepted
**Date:** 2026-10-14
**Deciders:** Joseph O'Brien

## Context

Every dense run re-embeds the whole corpus. Measured on this machine,
`nomic-embed-text` sustains roughly 5 ms per text batched, which put the 57,766
transcript chunks at **562 seconds** and the 363,791 archived Pieces chunks at
around half an hour.

That is not merely slow. It made a specific question unanswerable: _does
`nomic-embed-text-v2-moe` retrieve better than `nomic-embed-text` for this
corpus?_ Each arm costs a full rebuild, so at most one arm gets run, and the
result is whatever the last arm said. Measuring a choice requires paying for it
twice, which is a good way to never measure anything.

## Decision

Cache embeddings to disk. `CachingEmbedder` in `ltmx-embed` decorates any
[`Embedder`](../../crates/ltmx-domain/src/ports/mod.rs) and serves repeats from
a file.

### It is a decorator, not a subsystem

`DenseIndex` only calls `embed_batch`, so wrapping the embedder is transparent
to the retrieval layer and no index changes:

```text
DenseIndex<CachingEmbedder<OllamaEmbedder>>
```

Any future backend becomes cacheable without opting in.

### The key is the text, not the chunk id

Chunk ids derive from document uuids, so editing a transcript keeps its id
while its text changes. Keying on id would serve a stale vector forever and
degrade retrieval silently — the failure mode a cache exists to avoid.

The key is a 128-bit hash of `model_id || text`, plus the text length as a
secondary check.

### The model id is part of the key

The port requires `model_id` to change whenever produced vectors would change.
Swapping models therefore invalidates every entry instead of quietly mixing two
models' vectors in one index. This is what makes the comparison the cache
exists to enable actually safe to run.

### A rejected cache is reported, not silently discarded

A missing cache is normal. A cache written by a different model, a different
width, or a different format version is treated as absent **and said out loud**,
because that usually means the next run is about to pay the full cost again and
the operator should know before waiting.

### Work is done in bounded slices

A batch is processed `SLICE` (4,096) texts at a time: look up the slice, embed
only its misses, record the results, report, maybe flush, move on.

This is the fix for a problem the first version had. It built **one** `Vec` of
every pending text and called the backend once. On the combined 421k-chunk
corpus that meant:

- a ~435 MB duplicate of the corpus held resident before a single vector came
  back, on top of the 1.1 GB the index needs anyway;
- **no progress point anywhere** — a 30-minute run printed one line before it
  started and nothing after, which is indistinguishable from a hang;
- **no durability until the end** — a crash at minute 25 of 30 discarded
  everything, and the cache existed precisely to prevent that.

Slicing fixes all three from one change, because bounded work is what gives
you somewhere to report and somewhere to flush.

- **Progress.** `with_progress` registers a callback receiving processed/total,
  hits, misses, entries held, and elapsed. Not on the `Embedder` port —
  reporting progress is not something every backend can or should do.
- **Periodic flush.** Every `FLUSH_EVERY` (50,000) embeddings the cache is
  written out, bounding crash loss to roughly a minute of work. Failures are
  logged, not raised: losing durability part-way is strictly better than
  aborting the run, and the final `save` still surfaces a real problem.

Tests pin both, plus the invariant that would regress first:
`backend_never_sees_more_than_one_slice`.

### Writes are atomic

Temp file, flush, `fsync`, then rename. A crash mid-write leaves the previous
cache intact rather than a truncated file that fails to parse on next load.

### Hash collisions are a deliberate, bounded trade

Storing the text would eliminate collisions entirely, at a cost of roughly
889 MB for the archive corpus. The residual risk of a 128-bit key is about one
in 10^28 across 421k entries, and the stored length is checked alongside. Chosen
because the file is read on every run and a smaller one is materially cheaper to
load. Revisit if a collision is ever suspected — the symptom would be one
document returning another's neighbours.

## Consequences

- **Cold 562.3s → warm 0.3s** on the transcript corpus, measured. The
  comparison that motivated this is now affordable.
- **The same holds at full scale.** Measured across transcripts plus the archive,
  421,805 chunks and a 397,676-entry cache:

  | Run                            | Time      | Cached  | Embedded |
  | ------------------------------ | --------- | ------- | -------- |
  | Cold (partly warm, 208k to go) | 4,557.7 s | 213,449 | 208,356  |
  | Warm, all hits                 | **8.2 s** | 421,884 | 0        |

  A 556× difference, and the warm pass is cheap enough to run on every query.

- **The warm pass runs at ~51k chunks/sec end to end** — 421,884 chunks in
  8.2 s with zero misses, covering hashing, map lookup and copying every vector
  out of the cache. That is the whole cost, not a partial one. This retires the
  `rust-hash` candidate dependency: a faster hasher cannot make an 8 s number
  meaningfully smaller, and the number is already invisible next to an Ollama
  round trip.
- **Archive chunks embed ~4× slower than transcript text.** 21.9 ms/chunk against
  a 5 ms/text baseline. The cause is length: archive chunks are ~1,200 characters
  where transcript prompts are short. Not contention, not overhead — worth
  knowing before attributing a slow cold run to the wrong thing.
- **Duplicate text collapses for free.** Keying on text means 57,885 transcript
  chunks map to 38,266 distinct entries — a 34% reduction that comes from
  content, not tuning. The combined corpus is far less duplicated: 421,805
  chunks to 397,676 entries, **5.7%**. The archive barely repeats itself, so
  the 34% is a transcript property, not a general one
  ([Roadmap](../../ROADMAP.md) Phase 1.1).
- **118 MB on disk** for 38,266 × 768 `f32` vectors. About 4× the vector bytes,
  the rest being keys and lengths.
- **A model comparison needs two cache files**, one per model, since the model
  id is part of the key. That is the intended usage:
  `--cache ~/.cache/ltmx/nomic-embed-text.bin`.
- **The cache is unbounded.** Every text ever embedded accumulates, including
  queries. Fine at this scale; revisit if it grows unwieldy.
- **Memory is duplicated between the cache and the index.** Each holds a full
  copy of every vector — ~1.1 GB apiece at 364k chunks, and the archive run
  peaked near 4.6 GB resident. Affordable now, and worth revisiting before the
  MCP server needs a persistent index: the cache would rather stream into the
  index than sit resident beside it.

## The bugs this caught

Two, both from running the thing on a real corpus rather than reasoning about
it.

### Misses after the first one were all re-embedded

The first implementation split a batch at its **first** cache miss and embedded
the entire remainder, reasoning that batches arrive in corpus order and one
round trip beats per-item lookups.

Measured wrong: a warm run embedded **25,556** items when roughly three were
actually new. Transcripts are append-only, so the corpus grew by nine chunks
during the measuring session, the first of those landed early in the order, and
everything behind it was re-embedded. The heuristic optimised for a stable
corpus, which this one never is.

Replaced with per-item lookup, which costs a hash per item and is immune to
where the misses fall. The regression test is
`scattered_misses_do_not_poison_the_rest_of_the_batch`, and it is the more
valuable of the pair because it encodes _why_ the corpus grows mid-run.

### A 30-minute run reported nothing and lost everything on failure

The same first implementation also treated the batch as one indivisible unit:
every pending text materialised up front, one backend call, one save at the end.
On the 421k corpus that produced a ~435 MB resident duplicate, **no progress
output for the whole run**, and total loss of the work on a crash.

Fixed by slicing, which is recorded under _Work is done in bounded slices_
above. Worth stating as a lesson rather than a detail: **a long operation with
no observable progress and no intermediate durability is not just inconvenient,
it is untrustworthy** — you cannot tell it from a hang, and you cannot afford to
let it finish.

## Alternatives Considered

- **Cache the index rather than the vectors.** Rejected: it would pin the
  retrieval layer to one backend's on-disk format, and the index is cheap to
  rebuild once embedding is free.
- **Key on chunk id.** Rejected above; it is the obvious design and it is wrong.
- **Store the text alongside each vector for verification.** Rejected on load
  cost; see the collision trade above.
- **An LRU or bounded cache.** Rejected as premature. The corpus is stable at
  118 MB and a bound would evict entries that are exactly the ones a later run
  wants.
- **`sqlite-vec` instead of a flat file.** Deferred. It becomes attractive once
  the MCP server needs a persistent _index_ across restarts, not just persistent
  embeddings — a different problem from this one.
