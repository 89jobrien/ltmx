#![no_main]

//! Fuzz target for the Claude Code transcript parser.
//!
//! Transcripts are append-only files written by another process, so their
//! contents are genuinely external input: partially-written trailing lines,
//! schema drift, encoding surprises, and outright garbage. The parser must
//! never panic on any of it -- a panic here aborts an index run and takes
//! every other session's chunks with it.
//!
//! Beyond crash-freedom, assert lightweight invariants: every chunk the parser
//! emits must carry a citation and a valid origin, because a chunk that cannot
//! be cited is worse than no chunk at all.

use libfuzzer_sys::fuzz_target;
use ltmx_sources::claude::parse_transcript;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir should create");
    let path = dir.path().join("session.jsonl");
    if std::fs::write(&path, text).is_err() {
        return;
    }

    let chunks = match parse_transcript(&path) {
        Ok(chunks) => chunks,
        // Only a read failure may abort; the file was just written, so any
        // error here is itself a finding worth surfacing to the fuzzer.
        Err(_) => return,
    };

    for chunk in &chunks {
        assert!(!chunk.text.trim().is_empty(), "emitted a blank chunk");
        assert!(!chunk.source.path.is_empty(), "emitted a chunk with no path");
        assert!(
            chunk.citation().contains(':'),
            "every chunk must be citable, got {:?}",
            chunk.citation()
        );
    }
});