//! Ingests a couple of activity records into a SQLite corpus store and
//! queries them back by origin and time window.
//!
//! Requires the `sqlite3` CLI on `PATH`.
//!
//! ```sh
//! cargo run -p ltmx-store --example corpus
//! ```

use chrono::{DateTime, Utc};
use ltmx_domain::{ActivityRecord, Context, Origin, SourceProvenance, SourceRef};
use ltmx_store::{CorpusStore, RecordQuery, SqliteStore};

fn at(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).expect("valid timestamp")
}

fn record(id: &str, text: &str, origin: Origin, secs: i64) -> ActivityRecord {
    ActivityRecord::new(
        id,
        text,
        origin,
        SourceRef::file(format!("/src/{id}")),
        at(secs),
        None,
        Context::empty(),
        SourceProvenance::new(at(secs), "examples/corpus", None),
    )
}

fn main() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(dir.path().join("corpus.db")).expect("open store");

    let report = store
        .ingest(&[
            record(
                "a",
                "migrated the embedding cache to colima",
                Origin::Note,
                1_700_000_000,
            ),
            record("b", "wired up the gitea runner", Origin::Git, 1_700_000_100),
        ])
        .expect("ingest should succeed");
    println!(
        "ingested {} record(s), skipped {}",
        report.inserted, report.skipped
    );

    let git_only = store
        .query(&RecordQuery {
            origin: Some(Origin::Git),
            ..Default::default()
        })
        .expect("query should succeed");

    println!("records with origin=git:");
    for record in git_only {
        println!("  {} — {}", record.id, record.text);
    }
}
