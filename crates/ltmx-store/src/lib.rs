//! SQLite-backed corpus store for normalized activity records.

use std::path::Path;

use chrono::{DateTime, Utc};
use ltmx_domain::{ActivityRecord, Context, LtmxError, Reduction, SourceProvenance, TimeRange};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(String),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("domain error: {0}")]
    Domain(#[from] LtmxError),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Report from an ingest operation.
#[derive(Debug, Clone, Default)]
pub struct IngestReport {
    pub inserted: usize,
    pub updated: usize,
    pub skipped: usize,
}

/// Query parameters for the corpus store.
#[derive(Debug, Clone, Default)]
pub struct RecordQuery {
    pub range: Option<TimeRange>,
    pub origin: Option<ltmx_domain::Origin>,
    pub limit: Option<usize>,
}

/// Trait for corpus storage.
pub trait CorpusStore {
    fn upsert(&self, record: &ActivityRecord) -> Result<()>;
    fn ingest(&self, batch: &[ActivityRecord]) -> Result<IngestReport>;
    fn query(&self, q: &RecordQuery) -> Result<Vec<ActivityRecord>>;
}

/// SQLite implementation of [`CorpusStore`].
#[derive(Debug)]
pub struct SqliteStore {
    path: std::path::PathBuf,
}

impl SqliteStore {
    /// Opens or creates a store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let store = Self {
            path: path.as_ref().to_path_buf(),
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        let sql = r#"
CREATE TABLE IF NOT EXISTS activity_records (
  id TEXT PRIMARY KEY,
  text TEXT NOT NULL,
  origin TEXT NOT NULL,
  source_json TEXT NOT NULL,
  occurred_at TEXT NOT NULL,
  session_json TEXT,
  context_json TEXT NOT NULL,
  reductions_json TEXT NOT NULL,
  provenance_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_activity_records_occurred_at ON activity_records(occurred_at);
CREATE INDEX IF NOT EXISTS idx_activity_records_origin ON activity_records(origin);
"#;
        run_sqlite(&self.path, sql)
    }
}

impl CorpusStore for SqliteStore {
    fn upsert(&self, record: &ActivityRecord) -> Result<()> {
        let source_json = serde_json::to_string(&record.source)?;
        let session_json = record
            .session
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let context_json = serde_json::to_string(&record.context)?;
        let reductions_json = serde_json::to_string(&record.reductions)?;
        let provenance_json = serde_json::to_string(&record.provenance)?;
        let occurred_at = record.occurred_at.to_rfc3339();
        let origin = record.origin.as_str();

        let sql = format!(
            r#"
INSERT INTO activity_records(id, text, origin, source_json, occurred_at, session_json, context_json, reductions_json, provenance_json)
VALUES({id}, {text}, {origin}, {source_json}, {occurred_at}, {session_json}, {context_json}, {reductions_json}, {provenance_json})
ON CONFLICT(id) DO UPDATE SET
  text=excluded.text,
  origin=excluded.origin,
  source_json=excluded.source_json,
  occurred_at=excluded.occurred_at,
  session_json=excluded.session_json,
  context_json=excluded.context_json,
  reductions_json=excluded.reductions_json,
  provenance_json=excluded.provenance_json;
"#,
            id = sql_str(&record.id),
            text = sql_str(&record.text),
            origin = sql_str(origin),
            source_json = sql_str(&source_json),
            occurred_at = sql_str(&occurred_at),
            session_json = session_json
                .as_deref()
                .map(sql_str)
                .unwrap_or_else(|| "NULL".into()),
            context_json = sql_str(&context_json),
            reductions_json = sql_str(&reductions_json),
            provenance_json = sql_str(&provenance_json),
        );
        run_sqlite(&self.path, &sql)
    }

    fn ingest(&self, batch: &[ActivityRecord]) -> Result<IngestReport> {
        let mut report = IngestReport::default();
        for record in batch {
            self.upsert(record)?;
            report.inserted += 1;
        }
        Ok(report)
    }

    fn query(&self, q: &RecordQuery) -> Result<Vec<ActivityRecord>> {
        let mut where_clauses = Vec::new();
        if let Some(ref range) = q.range {
            where_clauses.push(format!(
                "occurred_at >= {} AND occurred_at <= {}",
                sql_str(&range.start.to_rfc3339()),
                sql_str(&range.end.to_rfc3339())
            ));
        }
        if let Some(origin) = q.origin {
            where_clauses.push(format!("origin = {}", sql_str(origin.as_str())));
        }
        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };
        let limit_sql = q.limit.map(|n| format!("LIMIT {n}")).unwrap_or_default();
        let sql = format!(
            "SELECT id, text, origin, source_json, occurred_at, session_json, context_json, reductions_json, provenance_json FROM activity_records {where_sql} ORDER BY occurred_at {limit_sql};"
        );
        let output = run_sqlite_output(&self.path, &sql)?;
        let mut records = Vec::new();
        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(9, '\x1f');
            let id = parts.next().unwrap_or("").to_string();
            let text = parts.next().unwrap_or("").to_string();
            let origin_str = parts.next().unwrap_or("");
            let source_json = parts.next().unwrap_or("{}");
            let occurred_at_str = parts.next().unwrap_or("");
            let session_json = parts.next().unwrap_or("");
            let context_json = parts.next().unwrap_or("{}");
            let reductions_json = parts.next().unwrap_or("[]");
            let provenance_json = parts.next().unwrap_or("{}");

            let origin = parse_origin(origin_str);
            let source: ltmx_domain::SourceRef = serde_json::from_str(source_json)?;
            let occurred_at = DateTime::parse_from_rfc3339(occurred_at_str)
                .map_err(|e| StoreError::Sqlite(e.to_string()))?
                .with_timezone(&Utc);
            let session = if session_json.is_empty() || session_json == "NULL" {
                None
            } else {
                Some(serde_json::from_str::<ltmx_domain::SessionId>(
                    session_json,
                )?)
            };
            let context: Context = serde_json::from_str(context_json)?;
            let reductions: Vec<Reduction> = serde_json::from_str(reductions_json)?;
            let provenance: SourceProvenance = serde_json::from_str(provenance_json)?;

            records.push(ActivityRecord {
                id,
                text,
                origin,
                source,
                occurred_at,
                session,
                context,
                reductions,
                provenance,
            });
        }
        Ok(records)
    }
}

fn parse_origin(s: &str) -> ltmx_domain::Origin {
    match s {
        "agent_transcript" => ltmx_domain::Origin::AgentTranscript,
        "git" => ltmx_domain::Origin::Git,
        "note" => ltmx_domain::Origin::Note,
        "screen" => ltmx_domain::Origin::Screen,
        "manual" => ltmx_domain::Origin::Manual,
        _ => ltmx_domain::Origin::Manual,
    }
}

fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn run_sqlite(db: &Path, sql: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| StoreError::Sqlite("stdin unavailable".into()))?;
        stdin
            .write_all(sql.as_bytes())
            .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    }
    let status = child
        .wait()
        .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    if !status.success() {
        return Err(StoreError::Sqlite(format!("sqlite3 exited {status}")));
    }
    Ok(())
}

fn run_sqlite_output(db: &Path, sql: &str) -> Result<String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg("-separator")
        .arg("\x1f")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| StoreError::Sqlite("stdin unavailable".into()))?;
        stdin
            .write_all(sql.as_bytes())
            .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| StoreError::Sqlite("stdout unavailable".into()))?;
    let mut buf = String::new();
    stdout
        .read_to_string(&mut buf)
        .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    let status = child
        .wait()
        .map_err(|e| StoreError::Sqlite(e.to_string()))?;
    if !status.success() {
        return Err(StoreError::Sqlite(format!("sqlite3 exited {status}")));
    }
    Ok(buf)
}
