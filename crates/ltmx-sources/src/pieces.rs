//! Reader for the archived Pieces LTM corpus.
//!
//! Pieces OS recorded roughly seven months of working activity before its LTM
//! stack was switched off. That archive is still on disk, and this module reads
//! it. See `docs/adrs/ADR-013-legacy-data-is-recoverable.md` for the
//! measurements that motivated it.
//!
//! ## What is read, and how
//!
//! Pieces stores documents in a Couchbase Lite database and indexes their text
//! in companion FTS5 tables. The indexed values sit in ordinary text columns
//! named after the fields they hold:
//!
//! | Table suffix                                            | Column                              |
//! | ------------------------------------------------------- | ----------------------------------- |
//! | `workstream\Events::…\Full\Text\Search\Index_content`   | `c0readable`, `c2windowTitle`, `c10context.native_ocr.ocrText` |
//! | `hints::hints\Full\Text\Search\Index_content`           | `c0text`                            |
//!
//! Reading those columns is plain SQL over text. **No format reverse
//! engineering is required** for the text itself, which is the main reason
//! this reader is small.
//!
//! ## Timestamps
//!
//! The FTS columns carry no timestamp, but `ActivityChunk` requires one and the
//! retrieval layer filters on time. Timestamps live in the document body,
//! which is a tagged binary envelope (`GBLENDED`) that has not been fully
//! decoded. Rather than decode it, this reader uses a validated anchor:
//! bodies encode timestamps as `0x4F` + length byte + a 27-byte ISO-8601
//! string, and for the years this archive spans that length byte is always
//! `0x1B` — so the byte sequence `4F 1B 32 30 32 36 2D` ("`O`", 27, "`2026-`")
//! marks the start of every timestamp in the corpus.
//!
//! That anchor was measured, not assumed. Of 194,431 event bodies:
//!
//! - 194,428 contain the anchor (3 do not, and are reported as skipped);
//! - 194,425 of those decode to a string that parses as ISO-8601;
//! - the decoded range is 2026-03-15 → 2026-10-06.
//!
//! The first timestamp in a body is used. Bodies usually contain two (when the
//! record was created and when it was last updated); the first is the event
//! time. The anchor is the only format knowledge in this module, and it is
//! isolated in [`extract_timestamp`] so it can be replaced by a real decoder
//! without touching anything else.
//!
//! ## Why this shells out to `sqlite3`
//!
//! Reading SQLite from Rust means linking `rusqlite`, which vendors the SQLite
//! C amalgamation and its build complexity. ADR-004 requires every dependency
//! to answer "why is this not the standard library?", and for a one-shot
//! archival import the honest answer is that a system `sqlite3` binary —
//! present on macOS by default — does the job with nothing added to the
//! dependency graph. The cost is a runtime requirement on that binary, which is
//! checked and reported rather than assumed.
//!
//! ## Read-only guarantee
//!
//! Every query runs against the database with `immutable=1`. SQLite will not
//! open a WAL database read-only without it, and the alternative — opening the
//! database normally — risks writing to another application's data. This
//! module never opens the database any other way.

use ltmx_domain::{ActivityChunk, ChunkId, LtmxError, Origin, SessionId, SourceRef};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Default location of the Pieces document store.
pub const DEFAULT_DB: &str =
    "~/Library/com.pieces.os/production/Pieces/couchbase.cblite2/db.sqlite3";

/// Hex anchor for an encoded ISO-8601 timestamp.
///
/// `4F` is the envelope's string tag, `1B` is the length of a
/// `YYYY-MM-DDTHH:MM:SS.ffffffZ` string (27 bytes), and `323032362D` is
/// `"2026-"`. Measured against the whole corpus; see the module docs.
const TS_ANCHOR: &str = "4F1B323032362D";

/// Characters of hex per timestamp: 27 bytes.
const TS_HEX_LEN: usize = 54;

/// Hex characters from the anchor to the first timestamp byte: the two tag
/// bytes (`4F`, `1B`).
const TS_HEX_OFFSET: usize = 4;

/// Target characters per emitted chunk.
///
/// The corpus averages ~2,300 characters per event, which is too coarse for
/// retrieval and fine enough to split on blank lines without cutting mid-thought.
const CHUNK_CHARS: usize = 1_200;

/// Text shorter than this is boilerplate (a dialog, an empty window) and is
/// dropped. Measured: the shortest useful events are a few hundred characters.
const MIN_CHARS: usize = 80;

/// Which text field to import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Application-readable text. Structured, cleaner, smaller.
    ///
    /// 145,534 non-empty values, 332 M characters, averaging ~2,300 each.
    Readable,
    /// Raw OCR of screen pixels. Larger and noisier.
    ///
    /// 186,446 non-empty values, 889 M characters. Use when a window exposed
    /// no readable content, e.g. video or a rendered canvas.
    Ocr,
}

impl Field {
    /// Returns the FTS column holding this field's text.
    fn column(self) -> &'static str {
        match self {
            Self::Readable => "c0readable",
            Self::Ocr => "c10context.native_ocr.ocrText",
        }
    }
}

/// Why an archived record produced no chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The body carried no decodable timestamp.
    NoTimestamp,
    /// The timestamp was present but not valid ISO-8601.
    MalformedTimestamp,
    /// The text was empty or shorter than [`MIN_CHARS`].
    TooShort,
}

/// An archived record that produced no chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRecord {
    /// Document uuid.
    pub uuid: String,
    /// Why it produced nothing.
    pub reason: SkipReason,
}

/// Chunks and skip diagnostics from reading the archive.
#[derive(Debug, Clone, Default)]
pub struct ArchiveReport {
    /// Every chunk extracted.
    pub chunks: Vec<ActivityChunk>,
    /// Records that produced nothing, with reasons.
    pub skipped: Vec<SkippedRecord>,
    /// Records the archive reported as readable but that yielded no chunk.
    pub scanned: usize,
}

impl ArchiveReport {
    /// Reports whether nothing was skipped.
    pub fn is_clean(&self) -> bool {
        self.skipped.is_empty()
    }

    /// Summarises counts for a human, including what was skipped and why.
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} records scanned, {} chunks, {} skipped",
            self.scanned,
            self.chunks.len(),
            self.skipped.len()
        );
        if !self.is_clean() {
            const SHOW: usize = 5;
            let mut by_reason: std::collections::BTreeMap<&str, usize> = Default::default();
            for skip in &self.skipped {
                *by_reason.entry(reason_label(skip.reason)).or_default() += 1;
            }
            for (reason, count) in &by_reason {
                out.push_str(&format!("\n  {count} {reason}"));
            }
            for skip in self.skipped.iter().take(SHOW) {
                out.push_str(&format!(
                    "\n  {} ({})",
                    skip.uuid,
                    reason_label(skip.reason)
                ));
            }
            if self.skipped.len() > SHOW {
                out.push_str(&format!("\n  ... and {} more", self.skipped.len() - SHOW));
            }
        }
        out
    }
}

/// A short human label for a skip reason.
fn reason_label(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::NoTimestamp => "no timestamp in body",
        SkipReason::MalformedTimestamp => "malformed timestamp",
        SkipReason::TooShort => "text too short to be meaningful",
    }
}

/// Recovers the first ISO-8601 timestamp encoded in a document body.
///
/// Returns the timestamp string, or `None` when the body carries no anchor.
///
/// This mirrors, in Rust, the extraction the query performs in SQL. It is not
/// on the import path -- SQLite does the work so multi-kilobyte bodies never
/// cross the pipe. It exists so the format assumption has a test that needs no
/// database, and so a future real decoder has an obvious place to replace it.
/// The two must agree; [`anchor_agrees_with_sql`] pins that.
#[cfg(test)]
fn extract_timestamp(body: &[u8]) -> Option<String> {
    let hex = to_hex(body);
    let at = hex.find(TS_ANCHOR)? + TS_HEX_OFFSET;
    let slice = hex.get(at..at + TS_HEX_LEN)?;
    let bytes = from_hex(slice)?;
    String::from_utf8(bytes).ok()
}

/// Hex encoding matching SQLite's `hex()`, which emits **uppercase**.
///
/// The case matters: `TS_ANCHOR` is uppercase because that is what the query
/// matches against. A lowercase encoder here would silently disagree with the
/// SQL and every Rust-side test would pass against the wrong bytes.
#[cfg(test)]
fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Decodes a hex string into bytes, case-insensitively.
#[cfg(test)]
fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// Splits an event's text into chunks of at most [`CHUNK_CHARS`] characters.
///
/// Breaks on blank lines where possible so a chunk tends to hold one coherent
/// block -- a terminal buffer, a dialog, a document section -- rather than an
/// arbitrary slice. Falls back to a hard split for a single oversized block.
fn split_into_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for block in text.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if block.chars().count() > CHUNK_CHARS {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            chunks.extend(hard_split(block));
            continue;
        }
        if current.chars().count() + block.chars().count() + 2 > CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(block);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Splits a single oversized block on line boundaries, falling back to chars.
fn hard_split(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in block.lines() {
        if current.chars().count() + line.chars().count() + 1 > CHUNK_CHARS && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if current.chars().count() > CHUNK_CHARS {
        // A single line longer than the budget: hard-split it.
        let chars: Vec<char> = current.chars().collect();
        for window in chars.chunks(CHUNK_CHARS) {
            out.push(window.iter().collect());
        }
    } else if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Expands a leading `~` so the default path works without shell quoting.
fn expand_home(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = text.strip_prefix("~/") else {
        return path.to_path_buf();
    };
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_path_buf();
    };
    PathBuf::from(home).join(rest)
}

/// Reader over an archived Pieces LTM database.
#[derive(Debug, Clone)]
pub struct PiecesArchiveReader {
    db: PathBuf,
    field: Field,
}

impl PiecesArchiveReader {
    /// Builds a reader over the database at `db`.
    pub fn new(db: impl Into<PathBuf>, field: Field) -> Self {
        Self {
            db: db.into(),
            field,
        }
    }

    /// Builds a reader over the default Pieces location.
    pub fn with_default_path(field: Field) -> Self {
        Self::new(DEFAULT_DB, field)
    }

    /// Reads the whole archive.
    ///
    /// `limit` caps the number of records read, for smoke tests. `None` reads
    /// everything.
    pub fn read(&self, limit: Option<usize>) -> Result<ArchiveReport, LtmxError> {
        let db = expand_home(&self.db);
        if !db.exists() {
            return Err(LtmxError::Index(format!(
                "Pieces archive not found at {} -- see docs/adrs/ADR-013",
                db.display()
            )));
        }

        let column = self.field.column();
        let limit_clause = limit_clause(limit);
        // One JSON object per line. `json_object` escapes newlines inside the
        // values, so each record is exactly one physical line and the stream can
        // be parsed incrementally rather than buffered. The body is reduced to
        // its timestamp in SQL so multi-kilobyte blobs never cross the pipe.
        let sql = format!(
            r#"
.mode list
.separator "\x1f"
SELECT json_object(
  'k', e.key,
  'r', f."{column}",
  'w', f."c2windowTitle",
  't', CASE WHEN instr(hex(e.body), '{TS_ANCHOR}') > 0
            THEN CAST(unhex(substr(hex(e.body),
                 instr(hex(e.body), '{TS_ANCHOR}') + {TS_HEX_OFFSET}, {TS_HEX_LEN})) AS TEXT)
            ELSE NULL END
)
FROM "kv_.workstream\Events" e
JOIN "kv_.workstream\Events::workstream\Events\Full\Text\Search\Index_content" f
  ON f.docid = e.rowid
WHERE f."{column}" IS NOT NULL AND f."{column}" <> ''
ORDER BY e.rowid
{limit_clause}
"#
        );

        let mut report = ArchiveReport::default();
        let stdout = spawn_sqlite(&db, &sql)?;
        for (index, line) in stdout.lines().enumerate() {
            let line =
                line.map_err(|e| LtmxError::Index(format!("reading sqlite3 output: {e}")))?;
            if line.is_empty() {
                continue;
            }
            report.scanned += 1;
            match parse_record(&line) {
                Ok((uuid, text, title, timestamp)) => {
                    let occurred_at = match timestamp {
                        Some(ts) => match chrono::DateTime::parse_from_rfc3339(&ts) {
                            Ok(dt) => dt.with_timezone(&chrono::Utc),
                            Err(_) => {
                                report.skipped.push(SkippedRecord {
                                    uuid,
                                    reason: SkipReason::MalformedTimestamp,
                                });
                                continue;
                            }
                        },
                        None => {
                            report.skipped.push(SkippedRecord {
                                uuid,
                                reason: SkipReason::NoTimestamp,
                            });
                            continue;
                        }
                    };

                    let body = text.trim();
                    if body.chars().count() < MIN_CHARS {
                        report.skipped.push(SkippedRecord {
                            uuid,
                            reason: SkipReason::TooShort,
                        });
                        continue;
                    }

                    // The window title is retrieval signal in its own right --
                    // "what was I doing in minibox" is answered by the title
                    // far more often than by the body text. It rides on every
                    // chunk because a chunk is indexed independently.
                    let prefix = title.trim();
                    let session = SessionId::new(occurred_at.format("%Y-%m-%d").to_string());

                    for (part, chunk_text) in split_into_chunks(body).into_iter().enumerate() {
                        let text = if prefix.is_empty() {
                            chunk_text
                        } else {
                            format!("[{prefix}]\n{chunk_text}")
                        };
                        report.chunks.push(
                            ActivityChunk::new(
                                ChunkId::new(format!("{uuid}#{part}")),
                                text,
                                Origin::Screen,
                                SourceRef::file(format!("pieces://workstream-events/{uuid}")),
                                occurred_at,
                            )
                            .with_session(session.clone()),
                        );
                    }
                }
                Err(_) => {
                    tracing::warn!("pieces archive: unparseable record at row {index}; skipped");
                }
            }
            if let Some(cap) = limit
                && index + 1 >= cap
            {
                break;
            }
        }
        Ok(report)
    }
}

/// Builds the `LIMIT` clause, or an empty string.
fn limit_clause(limit: Option<usize>) -> String {
    match limit {
        Some(n) => format!("LIMIT {n}"),
        None => String::new(),
    }
}

/// Spawns `sqlite3` in immutable read-only mode and returns its stdout.
///
/// `immutable=1` is not optional. SQLite cannot open a WAL database read-only
/// without it, and opening it normally would let SQLite write to another
/// application's data.
fn spawn_sqlite(db: &Path, sql: &str) -> Result<BufReader<std::process::ChildStdout>, LtmxError> {
    let uri = format!("file:{}?immutable=1", db.display());
    let mut child = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(&uri)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| LtmxError::Index(format!("could not run sqlite3: {e}")))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| LtmxError::Index("sqlite3 stdin unavailable".into()))?;
        use std::io::Write;
        stdin
            .write_all(sql.as_bytes())
            .map_err(|e| LtmxError::Index(format!("could not write query to sqlite3: {e}")))?;
    }
    // Drop stdin so sqlite3 sees EOF and starts producing.
    drop(child.stdin.take());

    Ok(BufReader::new(child.stdout.take().ok_or_else(|| {
        LtmxError::Index("sqlite3 produced no stdout".into())
    })?))
}

/// Parses one JSON record into `(uuid, text, window title, timestamp)`.
///
/// The timestamp is optional because the SQL yields NULL when a body carries
/// no anchor, and that case is reported rather than guessed around.
fn parse_record(line: &str) -> Result<(String, String, String, Option<String>), serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    let uuid = value
        .get("k")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let text = value
        .get("r")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let title = value
        .get("w")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let timestamp = value.get("t").and_then(Value::as_str).map(str::to_string);
    Ok((uuid, text, title, timestamp))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = b"Pieces OS \x00\xff";
        assert_eq!(from_hex(&to_hex(bytes)).as_deref(), Some(&bytes[..]));
    }

    #[test]
    fn hex_rejects_odd_length() {
        assert!(from_hex("abc").is_none());
    }

    #[test]
    fn hex_rejects_non_hex() {
        assert!(from_hex("zz").is_none());
    }

    /// The measured corpus shape: a body with the anchor at a known offset
    /// yields the exact ISO string the database contains.
    #[test]
    fn extracts_timestamp_from_anchor() {
        let ts = "2026-04-02T05:06:43.725180Z";
        assert_eq!(ts.len(), 27, "anchor assumes a 27-byte timestamp");
        let mut body = b"GBLENDEDO$f51e7870-ca49-465a-9c94-b232fa330b2dFMANUAL\x00".to_vec();
        body.push(0x4F);
        body.push(0x1B);
        body.extend_from_slice(ts.as_bytes());
        body.push(0x00);
        assert_eq!(extract_timestamp(&body).as_deref(), Some(ts));
    }

    #[test]
    fn timestamp_absent_without_anchor() {
        assert!(extract_timestamp(b"GBLENDEDno-timestamp-here").is_none());
    }

    /// Guards the anchor constant itself. If Pieces changes its envelope or the
    /// archive rolls into a different year, this fails before the importer
    /// silently yields nothing.
    #[test]
    fn anchor_encodes_the_expected_shape() {
        assert_eq!(TS_ANCHOR, "4F1B323032362D");
        assert_eq!(TS_HEX_LEN, 27 * 2);
        assert_eq!(TS_HEX_OFFSET, 2 * 2);
    }

    /// The SQL and the Rust mirror must extract the same bytes.
    ///
    /// The query is built by string interpolation, so an off-by-one in
    /// `TS_HEX_OFFSET` or `TS_HEX_LEN` would produce garbage timestamps that
    /// still parse as *some* ISO string. This pins the two implementations
    /// against each other without needing the 4.8 GB database.
    #[test]
    fn anchor_agrees_with_sql() {
        let ts = "2026-07-19T14:03:21.114820Z";
        let mut body = b"GBLENDEDprefix".to_vec();
        body.push(0x4F);
        body.push(0x1B);
        body.extend_from_slice(ts.as_bytes());
        body.push(0x00);

        let hex = to_hex(&body);
        let at = hex.find(TS_ANCHOR).expect("anchor present") + TS_HEX_OFFSET;
        // Exactly what the query evaluates: substr(hex, at, 27) then unhex().
        let slice = &hex[at..at + TS_HEX_LEN];
        assert_eq!(slice.len(), 54);
        assert_eq!(
            from_hex(slice).as_deref(),
            Some(ts.as_bytes()),
            "SQL slice must decode to the timestamp"
        );
        assert_eq!(extract_timestamp(&body).as_deref(), Some(ts));
    }

    #[test]
    fn splits_on_blank_lines_and_respects_budget() {
        let block = "a".repeat(CHUNK_CHARS - 10);
        let text = format!("{block}\n\n{block}\n\n{block}");
        let chunks = split_into_chunks(&text);
        assert_eq!(chunks.len(), 3, "each block exceeds half the budget");
        for chunk in &chunks {
            assert!(chunk.chars().count() <= CHUNK_CHARS);
        }
    }

    #[test]
    fn small_blocks_share_one_chunk() {
        let text = "first block\n\nsecond block";
        let chunks = split_into_chunks(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "first block\n\nsecond block");
    }

    #[test]
    fn oversized_single_block_is_hard_split() {
        let text = "b".repeat(CHUNK_CHARS * 2 + 50);
        let chunks = split_into_chunks(&text);
        assert!(chunks.len() >= 3);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= CHUNK_CHARS);
        }
    }

    #[test]
    fn splitting_never_loses_characters() {
        let text = "one\n\ntwo\n\nthree\n\nfour";
        let rejoined = split_into_chunks(text).join("\n\n");
        assert_eq!(rejoined, text);
    }

    #[test]
    fn parse_record_handles_embedded_newlines() {
        let line = r#"{"k":"u1","r":"line one\nline two","w":"minibox","t":"2026-04-02T05:06:43.725180Z"}"#;
        let (uuid, text, title, ts) = parse_record(line).expect("valid record");
        assert_eq!(uuid, "u1");
        assert_eq!(text, "line one\nline two");
        assert_eq!(title, "minibox");
        assert_eq!(ts.as_deref(), Some("2026-04-02T05:06:43.725180Z"));
    }

    /// A record with no timestamp must surface as `None` so the caller reports
    /// a skip -- never a fabricated time.
    #[test]
    fn parse_record_tolerates_missing_timestamp() {
        let line = r#"{"k":"u2","r":"body","w":"","t":null}"#;
        let (_, _, _, ts) = parse_record(line).expect("valid record");
        assert!(ts.is_none());
    }

    #[test]
    fn report_counts_skips_by_reason() {
        let report = ArchiveReport {
            chunks: Vec::new(),
            skipped: vec![
                SkippedRecord {
                    uuid: "a".into(),
                    reason: SkipReason::NoTimestamp,
                },
                SkippedRecord {
                    uuid: "b".into(),
                    reason: SkipReason::NoTimestamp,
                },
                SkippedRecord {
                    uuid: "c".into(),
                    reason: SkipReason::TooShort,
                },
            ],
            scanned: 3,
        };
        let summary = report.summary();
        assert!(summary.contains("2 no timestamp in body"));
        assert!(summary.contains("1 text too short"));
        assert!(!report.is_clean());
    }

    #[test]
    fn missing_database_is_an_error_not_an_empty_report() {
        let reader = PiecesArchiveReader::new("/nonexistent/pieces.sqlite3", Field::Readable);
        let err = reader.read(Some(1)).expect_err("missing db must fail");
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn expands_home_prefix() {
        let expanded = expand_home(Path::new("~/some/where.db"));
        assert!(!expanded.starts_with("~"));
        assert!(expanded.ends_with("some/where.db"));
    }

    #[test]
    fn leaves_absolute_paths_alone() {
        let absolute = PathBuf::from("/tmp/where.db");
        assert_eq!(expand_home(&absolute), absolute);
    }
}
