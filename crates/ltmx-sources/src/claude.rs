//! Parser for Claude Code session transcripts.
//!
//! Transcripts are JSONL: one JSON object per line, each either a conversation
//! entry (`type` of `user` or `assistant`) or bookkeeping (`attachment`,
//! `mode`, `file-history-snapshot`, ...). Only conversation entries become
//! chunks.
//!
//! Two kinds of chunk are produced, because they answer different questions:
//!
//! - **Prompts** -- what you asked. High signal for intent and decision
//!   archaeology ("why did we choose X?").
//! - **Actions** -- tool invocations rendered as text. High signal for "what
//!   was I actually doing", and the only thing that ties a question to a file.

use crate::walk;
use ltmx_domain::{ActivityChunk, ChunkId, Origin, SessionId, SourceRef};
use serde_json::Value;
use std::path::Path;

/// Renders a tool invocation as a single readable line.
///
/// Long inputs are truncated: a chunk is an index entry, not a transcript, and
/// embedding a 400-line `cat` output helps nobody.
fn render_tool_use(name: &str, input: &Value) -> String {
    let arg = match name {
        "Bash" | "Shell" => input.get("command").and_then(Value::as_str),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            input.get("file_path").and_then(Value::as_str)
        }
        "Grep" => input.get("pattern").and_then(Value::as_str),
        "Glob" => input.get("pattern").and_then(Value::as_str),
        "WebFetch" | "WebSearch" => input.get("url").and_then(Value::as_str),
        "Task" => input.get("description").and_then(Value::as_str),
        _ => None,
    };

    let Some(arg) = arg else {
        return name.to_string();
    };

    const MAX: usize = 200;
    let condensed = arg.split_whitespace().collect::<Vec<_>>().join(" ");
    if condensed.chars().count() > MAX {
        let truncated: String = condensed.chars().take(MAX).collect();
        format!("{name}: {truncated}…")
    } else {
        format!("{name}: {condensed}")
    }
}

/// Collects the plain text of a `content` value.
///
/// `content` is either a bare string or a list of blocks; text blocks carry a
/// `text` field, while `tool_use` blocks are handled separately by the caller.
fn plain_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.trim().to_string(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// True when the entry is a command-injection echo rather than something you
/// actually typed.
///
/// Transcripts record slash commands and hook output as user entries; indexing
/// them produces noise that crowds out real prompts.
fn is_noise(text: &str) -> bool {
    // `<command-name>`, `<command-message>`, `<system-reminder>` and
    // `<local-command-stdout>` all open with an angle bracket; the caveat
    // banner precedes replayed transcripts.
    text.is_empty()
        || text.starts_with('<')
        || text.starts_with("Caveat: The messages below")
        || text.contains("<local-command-stdout>")
}

/// Parses one transcript into chunks.
///
/// Chunk ids derive from the entry `uuid`, so re-reading an appended
/// transcript yields identical ids and indexing stays idempotent.
/// A transcript line that produced no chunk.
///
/// Kept rather than swallowed: a truncated trailing line is normal for an
/// append-only log, but a schema change silently emptying an index is not --
/// and the two are indistinguishable unless the skips are reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLine {
    /// Transcript the line came from.
    pub path: std::path::PathBuf,
    /// 1-based line number within that transcript.
    pub line: usize,
    /// Why the line produced nothing.
    pub reason: SkipReason,
}

/// Why a line produced no chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Not valid JSON -- usually a partially-written trailing line.
    Malformed,
}

/// Chunks and skip diagnostics from parsing transcripts.
#[derive(Debug, Clone, Default)]
pub struct ParseReport {
    /// Every chunk that was extracted.
    pub chunks: Vec<ActivityChunk>,
    /// Lines that produced nothing, with reasons.
    pub skipped: Vec<SkippedLine>,
}

impl ParseReport {
    /// Merges another report into this one.
    pub fn absorb(&mut self, other: ParseReport) {
        self.chunks.extend(other.chunks);
        self.skipped.extend(other.skipped);
    }

    /// Reports whether nothing was skipped.
    pub fn is_clean(&self) -> bool {
        self.skipped.is_empty()
    }

    /// Writes skipped lines to stderr, capped so a systematically mis-parsed
    /// corpus cannot flood the terminal. The total count is always shown.
    pub fn warn_skipped(&self) {
        if self.skipped.is_empty() {
            return;
        }
        const SHOW: usize = 10;
        eprintln!("warning: skipped {} transcript lines", self.skipped.len());
        for skip in self.skipped.iter().take(SHOW) {
            eprintln!("  {}:{}: malformed JSON", skip.path.display(), skip.line);
        }
        if self.skipped.len() > SHOW {
            eprintln!("  ... and {} more", self.skipped.len() - SHOW);
        }
    }
}

/// Parses one transcript into chunks.
///
/// Chunk ids derive from the entry `uuid`, so re-reading an appended
/// transcript yields identical ids and indexing stays idempotent.
pub fn parse_transcript(path: &Path) -> Result<ParseReport, ParseError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ParseError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let file = path.to_string_lossy().into_owned();
    let mut chunks = Vec::new();
    let mut skipped = Vec::new();

    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let line_number = index + 1;
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            skipped.push(SkippedLine {
                path: path.to_path_buf(),
                line: line_number,
                reason: SkipReason::Malformed,
            });
            continue;
        };

        let entry_type = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let message = match entry.get("message") {
            Some(m) => m,
            None => continue,
        };

        let Some(uuid) = entry.get("uuid").and_then(Value::as_str) else {
            continue;
        };
        let session = entry
            .get("sessionId")
            .and_then(Value::as_str)
            .map(SessionId::new);
        let occurred_at = match entry.get("timestamp").and_then(Value::as_str) {
            Some(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(_) => continue,
            },
            None => continue,
        };

        let source = SourceRef::lines(file.clone(), index as u32 + 1, index as u32 + 1);
        let content = message.get("content").unwrap_or(&Value::Null);

        match entry_type {
            "user" => {
                let text = plain_text(content);
                if is_noise(&text) {
                    continue;
                }
                let mut chunk = ActivityChunk::new(
                    ChunkId::new(format!("{file}#{uuid}#prompt")),
                    text,
                    Origin::AgentTranscript,
                    source,
                    occurred_at,
                );
                if let Some(session) = session {
                    chunk = chunk.with_session(session);
                }
                chunks.push(chunk);
            }
            "assistant" => {
                let Value::Array(blocks) = content else {
                    continue;
                };
                // One chunk per turn keeps commands from a single response
                // together, which is how you actually remember the activity.
                let actions: Vec<String> = blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .filter_map(|b| {
                        let name = b.get("name").and_then(Value::as_str)?;
                        let input = b.get("input").unwrap_or(&Value::Null);
                        Some(render_tool_use(name, input))
                    })
                    .collect();
                if actions.is_empty() {
                    continue;
                }
                let mut chunk = ActivityChunk::new(
                    ChunkId::new(format!("{file}#{uuid}#actions")),
                    actions.join("\n"),
                    Origin::AgentTranscript,
                    source,
                    occurred_at,
                );
                if let Some(session) = session {
                    chunk = chunk.with_session(session);
                }
                chunks.push(chunk);
            }
            _ => {}
        }
    }

    Ok(ParseReport { chunks, skipped })
}

/// Errors raised while reading transcripts.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Walks a Claude Code project root and parses every transcript beneath it.
#[derive(Debug, Clone)]
pub struct ClaudeTranscriptReader {
    root: std::path::PathBuf,
}

impl ClaudeTranscriptReader {
    /// Builds a reader rooted at a `~/.claude/projects` directory.
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the root directory this reader walks.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Parses every transcript beneath the root.
    ///
    /// Individual unreadable transcripts are skipped: one corrupt file must
    /// not abort an index run. Lines that fail to parse are reported rather
    /// than silently dropped, so a schema change is visible instead of showing
    /// up as a mysteriously smaller index.
    pub fn read_all(&self) -> Result<ParseReport, ltmx_domain::LtmxError> {
        let files = walk(&self.root, "jsonl")?;
        let mut report = ParseReport::default();
        for file in files {
            match parse_transcript(&file) {
                Ok(parsed) => report.absorb(parsed),
                Err(ParseError::Io { .. }) => continue,
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are written to a real temp dir so the parser is exercised
    // through the same path production uses.
    fn fixture(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, lines.join("\n")).expect("write fixture");
        (dir, path)
    }

    #[test]
    fn parses_user_prompt_as_chunk() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-08-23T08:04:37.247Z","cwd":"/x","message":{"role":"user","content":"why did we switch to colima?"}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "why did we switch to colima?");
        assert_eq!(chunks[0].origin, Origin::AgentTranscript);
        assert_eq!(
            chunks[0].session.as_ref().map(SessionId::as_str),
            Some("s1")
        );
        assert_eq!(chunks[0].source.line_range, Some((1, 1)));
    }

    #[test]
    fn renders_tool_use_blocks_as_action_chunk() {
        let (_d, path) = fixture(&[
            r#"{"type":"assistant","uuid":"u2","timestamp":"2026-08-23T08:05:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo xtask test"}}]}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Bash: cargo xtask test");
    }

    #[test]
    fn joins_multiple_actions_in_one_chunk() {
        let (_d, path) = fixture(&[
            r#"{"type":"assistant","uuid":"u3","timestamp":"2026-08-23T08:05:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/x/main.rs"}},{"type":"tool_use","name":"Grep","input":{"pattern":"vec0"}}]}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Read: /x/main.rs\nGrep: vec0");
    }

    #[test]
    fn skips_bookkeeping_entries() {
        let (_d, path) = fixture(&[
            r#"{"type":"mode","mode":"normal","sessionId":"s1"}"#,
            r#"{"type":"attachment","uuid":"x","timestamp":"2026-08-23T08:04:27Z"}"#,
            r#"{"type":"last-prompt","leafUuid":"y"}"#,
        ]);
        assert!(parse_transcript(&path).expect("parse").chunks.is_empty());
    }

    #[test]
    fn skips_command_injection_noise() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"n1","timestamp":"2026-08-23T08:00:00Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","uuid":"n2","timestamp":"2026-08-23T08:00:01Z","message":{"role":"user","content":""}}"#,
        ]);
        assert!(parse_transcript(&path).expect("parse").chunks.is_empty());
    }

    #[test]
    fn skips_assistant_turn_without_tool_use() {
        let (_d, path) = fixture(&[
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-23T08:05:00Z","message":{"role":"assistant","content":[{"type":"text","text":"here is the answer"}]}}"#,
        ]);
        assert!(parse_transcript(&path).expect("parse").chunks.is_empty());
    }

    #[test]
    fn skips_malformed_lines_without_failing() {
        let (_d, path) = fixture(&[
            "{not json",
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-23T08:04:37Z","message":{"role":"user","content":"kept"}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "kept");
    }

    #[test]
    fn skips_entries_without_timestamp() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"u1","message":{"role":"user","content":"no time"}}"#,
        ]);
        assert!(parse_transcript(&path).expect("parse").chunks.is_empty());
    }

    #[test]
    fn parses_text_block_list_content() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-23T08:00:00Z","message":{"role":"user","content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks[0].text, "first\nsecond");
    }

    #[test]
    fn chunk_ids_are_stable_across_reparse() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-23T08:00:00Z","message":{"role":"user","content":"stable"}}"#,
        ]);
        let first = parse_transcript(&path).expect("parse").chunks;
        let second = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(first[0].id, second[0].id);
    }

    #[test]
    fn records_line_numbers_for_citations() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"skip","timestamp":"2026-08-23T08:00:00Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","uuid":"keep","timestamp":"2026-08-23T08:00:01Z","message":{"role":"user","content":"kept"}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks[0].source.line_range, Some((2, 2)));
        assert!(chunks[0].citation().ends_with(":2-2"));
    }

    #[test]
    fn truncates_very_long_tool_arguments() {
        let long = "x".repeat(500);
        let (_d, path) = fixture(&[&format!(
            r#"{{"type":"assistant","uuid":"u1","timestamp":"2026-08-23T08:05:00Z","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{}"}}}}]}}}}"#,
            long
        )]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert!(chunks[0].text.chars().count() < 260, "should truncate");
        assert!(chunks[0].text.ends_with('…'));
    }

    #[test]
    fn unknown_tool_without_recognized_arg_renders_bare_name() {
        let (_d, path) = fixture(&[
            r#"{"type":"assistant","uuid":"u1","timestamp":"2026-08-23T08:05:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Workflow","input":{"weird":1}}]}}"#,
        ]);
        let chunks = parse_transcript(&path).expect("parse").chunks;
        assert_eq!(chunks[0].text, "Workflow");
    }

    #[test]
    fn malformed_lines_are_reported_not_swallowed() {
        let (_d, path) = fixture(&[
            "definitely not json",
            r#"{"type":"user","uuid":"ok","timestamp":"2026-08-23T08:00:00Z","message":{"role":"user","content":"kept"}}"#,
            "{",
        ]);
        let report = parse_transcript(&path).expect("parse");
        assert_eq!(report.chunks.len(), 1, "the good line still parses");
        assert_eq!(report.skipped.len(), 2, "both bad lines are reported");
        assert_eq!(report.skipped[0].line, 1);
        assert_eq!(report.skipped[1].line, 3);
        assert!(
            report
                .skipped
                .iter()
                .all(|s| s.reason == SkipReason::Malformed)
        );
    }

    #[test]
    fn a_clean_transcript_reports_no_skips() {
        let (_d, path) = fixture(&[
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-23T08:00:00Z","message":{"role":"user","content":"clean"}}"#,
        ]);
        assert!(parse_transcript(&path).expect("parse").is_clean());
    }

    #[test]
    fn bookkeeping_entries_are_not_counted_as_skips() {
        let (_d, path) = fixture(&[
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"attachment","uuid":"a","timestamp":"2026-08-23T08:00:00Z"}"#,
        ]);
        assert!(
            parse_transcript(&path).expect("parse").is_clean(),
            "entries without a message are expected, not anomalies"
        );
    }

    #[test]
    fn reports_absorb_into_one_summary() {
        let mut a = ParseReport {
            chunks: vec![sample("one")],
            skipped: vec![],
        };
        let b = ParseReport {
            chunks: vec![sample("two")],
            skipped: vec![],
        };
        a.absorb(b);
        assert_eq!(a.chunks.len(), 2);
        assert!(a.is_clean());
    }

    fn sample(id: &str) -> ActivityChunk {
        ActivityChunk::new(
            ltmx_domain::ChunkId::new(id),
            "text",
            ltmx_domain::Origin::AgentTranscript,
            ltmx_domain::SourceRef::file("/x"),
            chrono::DateTime::from_timestamp(0, 0).expect("valid timestamp"),
        )
    }

    #[test]
    fn missing_file_is_an_error() {
        let result = parse_transcript(Path::new("/nonexistent/path/session.jsonl"));
        assert!(matches!(result, Err(ParseError::Io { .. })));
    }
}
