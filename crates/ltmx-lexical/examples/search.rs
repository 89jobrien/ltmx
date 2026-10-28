//! Indexes a handful of activity chunks with BM25 and runs a search.
//!
//! ```sh
//! cargo run -p ltmx-lexical --example search
//! ```

use chrono::{DateTime, Utc};
use ltmx_domain::{ActivityChunk, ActivityIndex, ChunkId, Origin, SearchParams, SourceRef};
use ltmx_lexical::LexicalIndex;

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid timestamp")
}

fn chunk(id: &str, text: &str, origin: Origin, secs: i64) -> ActivityChunk {
    ActivityChunk::new(
        ChunkId::new(id),
        text,
        origin,
        SourceRef::file(format!("/notes/{id}.md")),
        at(secs),
    )
}

fn main() {
    let mut index = LexicalIndex::new();
    index
        .upsert_all(&[
            chunk(
                "a",
                "migrated the embedding cache to colima, docker desktop stayed idle",
                Origin::Note,
                1_700_000_000,
            )
            .shared(),
            chunk(
                "b",
                "wired up the gitea runner for release builds",
                Origin::Git,
                1_700_000_100,
            )
            .shared(),
            chunk(
                "c",
                "decided to keep docker desktop around for compose-only workflows",
                Origin::Note,
                1_700_000_200,
            )
            .shared(),
        ])
        .expect("upsert_all should succeed");

    let hits = index
        .search("docker desktop", &SearchParams::default().with_top_k(2))
        .expect("search should succeed");

    println!("query: \"docker desktop\" ({} indexed chunks)", index.len());
    for hit in hits {
        println!("  {:>6.3}  {}", hit.score, hit.chunk.citation());
    }
}
