//! Spike harness.
//!
//! The question this exists to answer: **can local traces alone -- agent
//! transcripts, git, notes -- answer the questions Pieces LTM used to answer?**
//!
//! The four query categories below are lifted from the `pieces-ltm` skill, so
//! the probe measures against the acceptance criteria rather than a guess.
//!
//! `eval` compares three retrieval strategies on the same corpus:
//!
//! - `lexical` -- BM25, no model, instant.
//! - `dense`   -- `nomic-embed-text` cosine, requires Ollama.
//! - `hybrid`  -- reciprocal rank fusion of both.
//!
//! Embedding a full corpus takes minutes, so the dense index is built **once**
//! and shared by every strategy. `HybridRetriever` borrows its children rather
//! than owning them specifically so this is possible.

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use ltmx_domain::{ActivityChunk, Origin};
#[cfg(feature = "bm25")]
use ltmx_domain::{ActivityIndex, ScoredChunk, SearchParams};
// `TimeRange` is needed by `Command::Brief`'s window derivation even when
// `bm25` is off (`brief` no longer pulls in `hybrid`/`bm25`), so it is gated
// on either feature rather than `bm25` alone.
#[cfg(any(feature = "bm25", feature = "brief"))]
use ltmx_domain::TimeRange;
#[cfg(feature = "pieces-archive")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

// Errors are rendered, not just printed: this binary reads files, parses
// append-only JSONL, and talks to a local service, and a rendered diagnostic
// with a source span is worth more than a stringified error.
use miette::Result;
#[cfg(any(
    all(feature = "claude-transcripts", feature = "bm25"),
    feature = "pieces-archive"
))]
use miette::{IntoDiagnostic, WrapErr};

/// Shorthand for building a diagnostic at a call site.
///
/// Exists because `miette!` only resolves as a bare macro path once
/// `miette::Result` is in scope; spelling out the full path at every call is
/// noisy.
#[cfg(feature = "bm25")]
macro_rules! report_error {
    ($($arg:tt)*) => {
        miette::miette!($($arg)*)
    };
}

#[cfg(feature = "claude-transcripts")]
use ltmx_sources::ClaudeTranscriptReader;

#[cfg(feature = "pieces-archive")]
use ltmx_sources::PiecesArchiveReader;

/// Converts a library error into a renderable diagnostic.
///
/// The domain deliberately does not depend on miette: a `Diagnostic` impl in a
/// published crate leaks into every consumer and couples them to the renderer.
/// The binary is the right place to make that choice.
#[cfg(feature = "bm25")]
fn report<T>(result: std::result::Result<T, ltmx_domain::LtmxError>) -> Result<T> {
    result.map_err(|error| report_error!("{error}"))
}

/// The four query categories from the `pieces-ltm` skill.
///
/// These are the acceptance criteria for the whole project.
#[cfg(feature = "bm25")]
const EVAL_QUERIES: &[(&str, &str)] = &[
    ("temporal", "what was I working on in the devloop project"),
    ("decision archaeology", "why did we switch to colima"),
    ("prior art", "parse op env vars"),
    ("current project", "minibox ci pipeline"),
];

/// Which retrieval strategy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Strategy {
    /// BM25 only. No model, no network.
    Lexical,
    /// Dense cosine only. Needs Ollama.
    Dense,
    /// Reciprocal rank fusion of both.
    Hybrid,
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Lexical => "lexical",
            Self::Dense => "dense",
            Self::Hybrid => "hybrid",
        };
        f.write_str(s)
    }
}

/// CLI spelling of [`ltmx_brief::BriefKind`].
#[cfg(feature = "brief")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BriefKindArg {
    /// Last 24h, every origin, no path scoping.
    Morning,
    /// Git history and agent-transcript chunks, scoped to work paths.
    Professional,
    /// Note/manual chunks, and transcript chunks outside work paths.
    Personal,
}

#[cfg(feature = "brief")]
impl From<BriefKindArg> for ltmx_brief::BriefKind {
    fn from(kind: BriefKindArg) -> Self {
        match kind {
            BriefKindArg::Morning => Self::Morning,
            BriefKindArg::Professional => Self::Professional,
            BriefKindArg::Personal => Self::Personal,
        }
    }
}

/// CLI spelling of [`Origin`], so the filter can be parsed from a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OriginFilter {
    /// Coding-agent transcript chunks.
    AgentTranscript,
    /// Git history.
    Git,
    /// Note chunks.
    Note,
    /// Captured screen content, including the Pieces archive.
    Screen,
    /// Caller-supplied chunks.
    Manual,
}

impl From<OriginFilter> for Origin {
    fn from(filter: OriginFilter) -> Self {
        match filter {
            OriginFilter::AgentTranscript => Self::AgentTranscript,
            OriginFilter::Git => Self::Git,
            OriginFilter::Note => Self::Note,
            OriginFilter::Screen => Self::Screen,
            OriginFilter::Manual => Self::Manual,
        }
    }
}

/// Parses `--since`/`--until` into a UTC instant.
///
/// Accepts `YYYY-MM`, `YYYY-MM-DD` or a full RFC 3339 timestamp, so the common
/// question -- "around March" -- needs no timestamp fiddling, while a precise
/// bound is still available when one is wanted.
#[cfg(any(feature = "bm25", feature = "brief"))]
fn parse_bound(text: &str, end_of_period: bool) -> Result<DateTime<Utc>, String> {
    let trimmed = text.trim();
    let candidates = if end_of_period && trimmed.len() == 7 {
        // "2026-03" as an upper bound means the whole of March, so add a month.
        vec![format!("{trimmed}-01"), format!("{trimmed}-28")]
    } else if end_of_period && trimmed.len() == 10 {
        vec![format!("{trimmed}T23:59:59Z")]
    } else {
        vec![trimmed.to_string(), format!("{trimmed}-01")]
    };
    for candidate in candidates {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&candidate) {
            return Ok(dt.with_timezone(&Utc));
        }
        if let Ok(naive) = chrono::NaiveDate::parse_from_str(&candidate, "%Y-%m-%d") {
            return Ok(naive.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
        }
        if let Ok(ym) = chrono::NaiveDate::parse_from_str(&candidate, "%Y-%m") {
            return Ok(ym.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
        }
    }
    Err(format!(
        "could not parse '{text}' as YYYY-MM, YYYY-MM-DD or an RFC 3339 timestamp"
    ))
}

#[derive(Debug, Parser)]
#[command(about = "ltmx spike harness", version)]
struct Cli {
    /// Root of the Claude Code projects directory.
    #[arg(long, default_value = "~/.claude/projects")]
    root: PathBuf,

    /// Embedding model passed to Ollama.
    ///
    /// Changing this invalidates the embedding cache, because the port
    /// requires `model_id` to change whenever produced vectors would change.
    #[cfg(feature = "hybrid")]
    #[arg(long, default_value = "nomic-embed-text", global = true)]
    model: String,

    /// Where the embedding cache is kept.
    ///
    /// The cache is keyed by model and text, so one file per model is the
    /// natural layout: `--cache ~/.cache/ltmx/nomic-embed-text.bin`.
    #[cfg(feature = "hybrid")]
    #[arg(long, default_value = "~/.cache/ltmx/embeddings.bin", global = true)]
    cache: PathBuf,

    /// Skip the embedding cache and embed everything.
    #[cfg(feature = "hybrid")]
    #[arg(long, global = true)]
    no_cache: bool,

    /// Also index the archived Pieces corpus alongside local transcripts.
    #[cfg(feature = "pieces-archive")]
    #[arg(long, global = true)]
    archive: bool,

    /// Which archived text field to index.
    #[cfg(feature = "pieces-archive")]
    #[arg(long, value_enum, default_value_t = ArchiveField::Readable, global = true)]
    archive_field: ArchiveField,

    /// Path to the Pieces document store.
    #[cfg(feature = "pieces-archive")]
    #[arg(long, default_value = ltmx_sources::pieces::DEFAULT_DB, global = true)]
    archive_db: String,

    /// Read only this many archived records. For smoke tests.
    #[cfg(feature = "pieces-archive")]
    #[arg(long, global = true)]
    archive_limit: Option<usize>,

    /// Restrict results to these origins. Repeatable.
    ///
    /// Exists because `SearchParams` could always filter by origin and nothing
    /// exposed it. With the archive loaded the corpus is no longer one thing,
    /// and "is this answer coming from screen noise or from what you actually
    /// typed?" is not answerable without it.
    #[arg(long, value_enum, global = true)]
    origins: Vec<OriginFilter>,

    /// Narrow a query to a time window.
    #[arg(long, global = true)]
    since: Option<String>,

    /// End of the `--since` window. Accepts `YYYY-MM` or `YYYY-MM-DD`.
    #[arg(long, global = true)]
    until: Option<String>,

    #[command(subcommand)]
    command: Command,
}

/// Default for `--work-prefix`: the current working directory, not a
/// personal machine path, since this is a published, public CLI.
#[cfg(feature = "brief")]
fn default_work_prefix() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_else(|| ".".to_owned())
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report how much material is available.
    Stats,
    /// Ask one question.
    Ask {
        query: String,
        #[arg(long, default_value_t = 5)]
        top_k: usize,
        #[arg(long, value_enum, default_value_t = Strategy::Lexical)]
        strategy: Strategy,
    },
    /// Run the skill's four query categories under every strategy.
    Eval,
    /// Read the archived Pieces LTM corpus and report what it yields.
    ///
    /// Separate from `stats` because the archive is a different source with a
    /// different cost profile: it reads another application's 4.8 GB database
    /// rather than walking local transcripts.
    #[cfg(feature = "pieces-archive")]
    Archive {
        /// Which text field to import.
        #[arg(long, value_enum, default_value_t = ArchiveField::Readable)]
        field: ArchiveField,
        /// Path to the Pieces document store.
        #[arg(long, default_value = ltmx_sources::pieces::DEFAULT_DB)]
        db: String,
        /// Stop after this many records.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Generate a morning/professional/personal brief over recent activity.
    ///
    /// Evidence is selected directly from the corpus by origin and time
    /// window -- a brief has no query, so it never goes through
    /// `ActivityIndex::search`. Generation itself calls a local Ollama
    /// model over that evidence.
    #[cfg(feature = "brief")]
    Brief {
        #[arg(long, value_enum, default_value_t = BriefKindArg::Morning)]
        kind: BriefKindArg,
        /// Maximum pieces of evidence to hand to the generator.
        #[arg(long, default_value_t = 20)]
        top_k: usize,
        /// Generation model passed to Ollama. Independent of the embedding
        /// `--model` flag -- a brief does not require the dense/hybrid path.
        #[arg(long, default_value = "llama3.2")]
        gen_model: String,
        /// Path prefix distinguishing "work" from "personal" chunks for the
        /// `professional`/`personal` kinds. Ignored for `morning`. Defaults to
        /// the current working directory rather than a personal path, since
        /// this is a published, public CLI.
        #[arg(long, default_value_t = default_work_prefix())]
        work_prefix: String,
    },
}

/// Which archived text field to import.
///
/// Kept separate from `ltmx_sources::pieces::Field` so the source crate does
/// not depend on `clap`; a CLI concern should not leak into a library.
#[cfg(feature = "pieces-archive")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ArchiveField {
    /// Application-readable text: structured and smaller.
    Readable,
    /// Raw OCR of screen pixels: larger and noisier.
    Ocr,
}

#[cfg(feature = "pieces-archive")]
impl From<ArchiveField> for ltmx_sources::pieces::Field {
    fn from(field: ArchiveField) -> Self {
        match field {
            ArchiveField::Readable => Self::Readable,
            ArchiveField::Ocr => Self::Ocr,
        }
    }
}

#[cfg(feature = "bm25")]
fn print_hits(hits: &[ScoredChunk]) {
    if hits.is_empty() {
        println!("    (no matches)");
        return;
    }
    for (rank, hit) in hits.iter().enumerate() {
        let preview: String = hit.chunk.text.chars().take(140).collect();
        let preview = preview.replace('\n', " / ");
        println!("  {}. [{:.4}] {}", rank + 1, hit.score, preview);
        println!("      {}", hit.chunk.citation());
    }
}

/// The filters a query runs under, built from the CLI flags.
///
/// Shared by `ask` and `eval` so both honour `--origin` and the time window.
#[cfg(feature = "bm25")]
#[derive(Debug, Clone, Default)]
struct Filters {
    origins: Vec<Origin>,
    window: Option<TimeRange>,
}

#[cfg(feature = "bm25")]
impl Filters {
    /// Applies these filters to a params set.
    fn apply(&self, params: SearchParams) -> SearchParams {
        let params = params.with_origins(self.origins.iter().copied());
        match self.window {
            Some(window) => params.with_window(window),
            None => params,
        }
    }
}

/// Builds filters from the parsed CLI values.
#[cfg(feature = "bm25")]
fn build_filters(
    origins: &[OriginFilter],
    since: &Option<String>,
    until: &Option<String>,
) -> Result<Filters> {
    let window = match (since, until) {
        (None, None) => None,
        (Some(from), None) => {
            let start = parse_bound(from, false).map_err(|e| miette::miette!("{e}"))?;
            Some(TimeRange::new(start, DateTime::<Utc>::MAX_UTC))
        }
        (None, Some(to)) => {
            let end = parse_bound(to, true).map_err(|e| miette::miette!("{e}"))?;
            Some(TimeRange::new(DateTime::<Utc>::MIN_UTC, end))
        }
        (Some(from), Some(to)) => {
            let start = parse_bound(from, false).map_err(|e| miette::miette!("{e}"))?;
            let end = parse_bound(to, true).map_err(|e| miette::miette!("{e}"))?;
            Some(TimeRange::new(start, end))
        }
    };
    Ok(Filters {
        origins: origins.iter().map(|o| (*o).into()).collect(),
        window,
    })
}

/// Runs the four skill query categories against one retriever.
#[cfg(feature = "bm25")]
fn run_eval(name: &str, index: &dyn ActivityIndex, top_k: usize, filters: &Filters) {
    println!("═══ {name} ═══");
    let mut covered = 0;
    for (category, query) in EVAL_QUERIES {
        let hits = index
            .search(
                query,
                &filters.apply(SearchParams::default().with_top_k(top_k)),
            )
            .expect("search should succeed");
        let hit = !hits.is_empty();
        covered += usize::from(hit);
        println!(
            "[{}] {category}: {query}",
            if hit { "HIT " } else { "MISS" }
        );
        print_hits(&hits);
        println!();
    }
    println!(
        "{name}: {covered}/{} categories returned any result\n",
        EVAL_QUERIES.len()
    );
}

/// Builds the lexical index over `chunks`.
///
/// The corpus is built once as `Arc` and only ever cloned by reference
/// afterwards, so building a second index costs a pointer copy rather than a
/// deep copy of every chunk's text.
#[cfg(feature = "hybrid")]
fn lexical_index(chunks: &[Arc<ActivityChunk>]) -> Result<ltmx_lexical::LexicalIndex> {
    let mut index = ltmx_lexical::LexicalIndex::new();
    report(index.upsert_all(chunks))?;
    Ok(index)
}

/// How the dense path should reach Ollama.
#[cfg(feature = "hybrid")]
#[derive(Debug, Clone)]
struct DenseConfig {
    /// Model name passed to Ollama. Changing it invalidates the cache, because
    /// the port requires `model_id` to change whenever vectors would change.
    model: String,
    /// Where the embedding cache lives.
    cache: PathBuf,
    /// Set to skip caching entirely.
    no_cache: bool,
}

/// Embeds the corpus, reporting elapsed time and cache behaviour.
///
/// Returns a boxed `dyn ActivityIndex` rather than a concrete type because
/// `--no-cache` swaps the embedder for a different concrete type, and the
/// retrieval layer is already written against the port.
///
/// This is the only expensive step in the whole probe, so it happens once per
/// process regardless of how many strategies need it.
#[cfg(feature = "hybrid")]
fn dense_index(
    chunks: &[Arc<ActivityChunk>],
    config: &DenseConfig,
) -> Result<Box<dyn ActivityIndex>> {
    let started = std::time::Instant::now();

    if config.no_cache {
        eprintln!(
            "embedding {} chunks via Ollama (cache disabled), this takes minutes...",
            chunks.len()
        );
        let mut index =
            ltmx_retrieve::DenseIndex::new(ltmx_embed::OllamaEmbedder::new(&config.model));
        report(index.upsert_all(chunks))?;
        eprintln!("embedded in {:.1}s", started.elapsed().as_secs_f64());
        return Ok(Box::new(index));
    }

    let embedder = ltmx_embed::CachingEmbedder::open(
        ltmx_embed::OllamaEmbedder::new(&config.model),
        &config.cache,
    )
    .with_progress(|p| {
        // A line per slice: ~106 lines across the archive corpus, so it reads
        // as progress rather than a log flood. Elapsed and rate are the numbers
        // that matter when deciding whether to wait or kill it.
        let secs = p.elapsed.as_secs_f64();
        let rate = if secs > 0.0 {
            p.processed as f64 / secs
        } else {
            0.0
        };
        let eta = if rate > 0.0 {
            let remaining = p.total.saturating_sub(p.processed) as f64 / rate;
            format!("{:.0}s left", remaining)
        } else {
            "…".to_string()
        };
        eprintln!(
            "  {} / {}  {:.0}%  {:.1}s  {:.0}/s  {} cached, {} embedded  {}",
            p.processed,
            p.total,
            (p.processed as f64 / p.total.max(1) as f64) * 100.0,
            secs,
            rate,
            p.hits,
            p.misses,
            eta
        );
    });
    match embedder.problem() {
        Some(ltmx_embed::CacheProblem::Missing) => {}
        Some(ltmx_embed::CacheProblem::Incompatible(why)) => {
            // Loud, not silent: discarding a cache usually means the model or
            // the format changed, and the next run is about to pay for it again.
            eprintln!("embedding cache discarded ({why}); rebuilding");
        }
        None => {}
    }

    let before = embedder.stats();
    if before.loaded > 0 {
        eprintln!(
            "embedding cache: {} entries loaded from {}",
            before.loaded,
            config.cache.display()
        );
    }

    let mut index = ltmx_retrieve::DenseIndex::new(embedder);
    report(index.upsert_all(chunks))?;

    let after = index.embedder().stats();
    let written = index
        .embedder()
        .save()
        .map_err(|e| miette::miette!("writing embedding cache: {e}"))?;

    eprintln!(
        "embedded in {:.1}s — {} cached, {} embedded, {} entries written to {}",
        started.elapsed().as_secs_f64(),
        after.hits,
        after.misses,
        written,
        config.cache.display()
    );
    Ok(Box::new(index))
}

/// Handles `ask`, building only the indexes the chosen strategy needs.
#[cfg(feature = "hybrid")]
fn ask(
    query: &str,
    top_k: usize,
    strategy: Strategy,
    chunks: &[Arc<ActivityChunk>],
    config: &DenseConfig,
    filters: &Filters,
) -> Result<()> {
    let params = filters.apply(SearchParams::default().with_top_k(top_k));

    let hits = match strategy {
        Strategy::Lexical => report(lexical_index(chunks)?.search(query, &params))?,
        Strategy::Dense => report(dense_index(chunks, config)?.search(query, &params))?,
        Strategy::Hybrid => {
            // Both children must outlive the retriever, so they are bound here
            // rather than handed to a helper that would return a borrow.
            let lexical = lexical_index(chunks)?;
            let dense = dense_index(chunks, config)?;
            let retriever = ltmx_retrieve::HybridRetriever::new(vec![
                &lexical as &dyn ActivityIndex,
                &*dense as &dyn ActivityIndex,
            ]);
            report(retriever.search(query, &params))?
        }
    };

    println!("query:    {query}");
    println!("strategy: {strategy}\n");
    print_hits(&hits);
    Ok(())
}

/// Handles `ask` when only lexical retrieval is compiled in.
#[cfg(not(feature = "hybrid"))]
fn ask(
    query: &str,
    top_k: usize,
    _strategy: Strategy,
    chunks: &[Arc<ActivityChunk>],
    #[cfg(feature = "bm25")] filters: &Filters,
) -> Result<()> {
    #[cfg(feature = "bm25")]
    {
        let mut index = ltmx_lexical::LexicalIndex::new();
        report(index.upsert_all(chunks))?;
        let hits = report(index.search(
            query,
            &filters.apply(SearchParams::default().with_top_k(top_k)),
        ))?;
        println!("query:    {query}");
        println!("strategy: lexical (only strategy compiled in)\n");
        print_hits(&hits);
        return Ok(());
    }
    #[cfg(not(feature = "bm25"))]
    {
        let _ = (query, top_k, chunks);
        miette::bail!("built without the `bm25` feature: no index adapter available");
    }
}

/// Runs the four query categories under every available strategy.
#[cfg(feature = "hybrid")]
fn eval(
    chunks: &[Arc<ActivityChunk>],
    top_k: usize,
    config: &DenseConfig,
    filters: &Filters,
) -> Result<()> {
    let lexical = lexical_index(chunks)?;
    run_eval("lexical", &lexical, top_k, filters);

    let dense = dense_index(chunks, config)?;
    run_eval("dense", &*dense, top_k, filters);

    let hybrid = ltmx_retrieve::HybridRetriever::new(vec![
        &lexical as &dyn ActivityIndex,
        &*dense as &dyn ActivityIndex,
    ]);
    run_eval("hybrid", &hybrid, top_k, filters);
    Ok(())
}

/// Runs the four query categories with lexical retrieval only.
#[cfg(not(feature = "hybrid"))]
fn eval(
    chunks: &[Arc<ActivityChunk>],
    top_k: usize,
    #[cfg(feature = "bm25")] filters: &Filters,
) -> Result<()> {
    #[cfg(feature = "bm25")]
    {
        let mut index = ltmx_lexical::LexicalIndex::new();
        report(index.upsert_all(chunks))?;
        run_eval("lexical", &index, top_k, filters);
        return Ok(());
    }
    #[cfg(not(feature = "bm25"))]
    {
        let _ = (chunks, top_k);
        miette::bail!("built without the `bm25` feature: no index adapter available");
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let root = sh_expand(&cli.root);

    #[cfg(feature = "hybrid")]
    let dense_config = DenseConfig {
        model: cli.model.clone(),
        cache: sh_expand(&cli.cache),
        no_cache: cli.no_cache,
    };

    #[cfg(feature = "bm25")]
    let filters = build_filters(&cli.origins, &cli.since, &cli.until)?;
    #[cfg(not(feature = "bm25"))]
    let _ = (&cli.origins, &cli.since, &cli.until);

    // The archive reader is self-contained: it does not need the transcript
    // corpus loaded, and loading that takes far longer than the import itself.
    #[cfg(feature = "pieces-archive")]
    if let Command::Archive { field, db, limit } = cli.command {
        let reader = PiecesArchiveReader::new(&db, field.into());
        let started = std::time::Instant::now();
        let report = reader
            .read(limit)
            .into_diagnostic()
            .wrap_err_with(|| format!("reading the Pieces archive at {db}"))?;
        eprintln!("{}", report.summary());
        eprintln!("elapsed: {:?}", started.elapsed());

        // Show a sample so the output is inspectable rather than a bare count.
        for chunk in report.chunks.iter().take(3) {
            let head: String = chunk.text.chars().take(160).collect();
            println!(
                "\n{} {}\n{}",
                chunk.occurred_at.to_rfc3339(),
                chunk.citation(),
                head.replace('\n', " ⏎ ")
            );
        }
        return Ok(());
    }

    #[cfg(feature = "claude-transcripts")]
    let transcript_chunks: Vec<Arc<ActivityChunk>> = {
        let reader = ClaudeTranscriptReader::new(&root);
        let report = reader
            .read_all()
            .into_diagnostic()
            .wrap_err_with(|| format!("reading transcripts under {}", root.display()))?;
        eprintln!(
            "ingested {} chunks from {}",
            report.chunks.len(),
            root.display()
        );
        report.warn_skipped();
        report
            .chunks
            .into_iter()
            .map(ActivityChunk::shared)
            .collect()
    };

    #[cfg(not(feature = "claude-transcripts"))]
    let transcript_chunks: Vec<Arc<ActivityChunk>> = {
        eprintln!("built without `claude-transcripts`: nothing to index");
        Vec::new()
    };

    // The Pieces archive is opt-in. It is 340k+ chunks of screen history, which
    // changes the character of every measurement, so it is never added by
    // accident.
    #[cfg(feature = "pieces-archive")]
    let archive_chunks: Vec<Arc<ActivityChunk>> = if cli.archive {
        let reader = PiecesArchiveReader::new(
            sh_expand(Path::new(&cli.archive_db)),
            cli.archive_field.into(),
        );
        let started = std::time::Instant::now();
        let report = reader
            .read(cli.archive_limit)
            .into_diagnostic()
            .wrap_err_with(|| format!("reading the Pieces archive at {}", cli.archive_db))?;
        eprintln!("{}", report.summary());
        eprintln!(
            "archive ingested in {:.1}s",
            started.elapsed().as_secs_f64()
        );
        report
            .chunks
            .into_iter()
            .map(ActivityChunk::shared)
            .collect()
    } else {
        Vec::new()
    };

    #[cfg(not(feature = "pieces-archive"))]
    let archive_chunks: Vec<Arc<ActivityChunk>> = Vec::new();

    let mut chunks: Vec<Arc<ActivityChunk>> = transcript_chunks;
    chunks.extend(archive_chunks);

    match cli.command {
        Command::Stats => {
            let sessions: std::collections::HashSet<_> =
                chunks.iter().filter_map(|c| c.session.as_ref()).collect();
            println!("root:          {}", root.display());
            println!("total chunks:  {}", chunks.len());
            println!("sessions:      {}", sessions.len());
            // Per-origin counts, because the corpus is no longer one thing.
            // A query that gets worse after adding the archive is usually a
            // mix problem, and this is where you would see it.
            for origin in Origin::ALL {
                let count = chunks.iter().filter(|c| c.origin == origin).count();
                if count > 0 {
                    println!("  {:<18} {}", origin.as_str(), count);
                }
            }
            if let (Some(oldest), Some(newest)) = (oldest(&chunks), newest(&chunks)) {
                println!("span:          {oldest} → {newest}");
            }
        }
        Command::Ask {
            query,
            top_k,
            strategy,
        } => {
            #[cfg(feature = "hybrid")]
            ask(&query, top_k, strategy, &chunks, &dense_config, &filters)?;
            #[cfg(not(feature = "hybrid"))]
            ask(
                &query,
                top_k,
                strategy,
                &chunks,
                #[cfg(feature = "bm25")]
                &filters,
            )?;
        }
        Command::Eval => {
            #[cfg(feature = "hybrid")]
            eval(&chunks, 3, &dense_config, &filters)?;
            #[cfg(not(feature = "hybrid"))]
            eval(
                &chunks,
                3,
                #[cfg(feature = "bm25")]
                &filters,
            )?;
            println!(
                "Note: this metric only measures whether results were returned, not\n\
                 whether they were relevant. Read the hits."
            );
        }
        // Handled above, before the transcript corpus is loaded. Listed
        // explicitly so adding a command cannot silently fall through here.
        #[cfg(feature = "pieces-archive")]
        Command::Archive { .. } => unreachable!("archive returns before chunk loading"),
        #[cfg(feature = "brief")]
        Command::Brief {
            kind,
            top_k,
            gen_model,
            work_prefix,
        } => {
            let kind: ltmx_brief::BriefKind = kind.into();
            // `filters` is only bound under `#[cfg(feature = "bm25")]`
            // (see above); `brief` no longer pulls in `bm25`, so the
            // non-bm25 path builds the window directly from `cli.since`/
            // `cli.until`, mirroring `build_filters`'s window logic rather
            // than requiring `bm25` just for this CLI surface.
            #[cfg(feature = "bm25")]
            let window = filters
                .window
                .unwrap_or_else(|| ltmx_brief::last_24h(Utc::now()));
            #[cfg(not(feature = "bm25"))]
            let window = {
                let bound = match (&cli.since, &cli.until) {
                    (None, None) => None,
                    (Some(from), None) => {
                        let start = parse_bound(from, false).map_err(|e| miette::miette!("{e}"))?;
                        Some(TimeRange::new(start, DateTime::<Utc>::MAX_UTC))
                    }
                    (None, Some(to)) => {
                        let end = parse_bound(to, true).map_err(|e| miette::miette!("{e}"))?;
                        Some(TimeRange::new(DateTime::<Utc>::MIN_UTC, end))
                    }
                    (Some(from), Some(to)) => {
                        let start = parse_bound(from, false).map_err(|e| miette::miette!("{e}"))?;
                        let end = parse_bound(to, true).map_err(|e| miette::miette!("{e}"))?;
                        Some(TimeRange::new(start, end))
                    }
                };
                bound.unwrap_or_else(|| ltmx_brief::last_24h(Utc::now()))
            };
            let filter = ltmx_brief::filter_for(kind, window);
            let scoped = ltmx_brief::filter_by_path(chunks.clone(), kind, &work_prefix);
            let evidence = ltmx_brief::select_evidence(&scoped, &filter, top_k);

            eprintln!(
                "{}: {} pieces of evidence, {} -> {}",
                kind.label(),
                evidence.len(),
                window.start.to_rfc3339(),
                window.end.to_rfc3339()
            );

            let generator = ltmx_brief::OllamaGenerator::new(gen_model);
            let text = ltmx_brief::generate_brief(&generator, kind, window, &evidence)
                .map_err(|e| miette::miette!("{e}"))?;
            println!("{text}");
        }
    }

    Ok(())
}

fn oldest(chunks: &[Arc<ActivityChunk>]) -> Option<DateTime<Utc>> {
    chunks.iter().map(|c| c.occurred_at).min()
}

fn newest(chunks: &[Arc<ActivityChunk>]) -> Option<DateTime<Utc>> {
    chunks.iter().map(|c| c.occurred_at).max()
}

/// Expands a leading `~`.
///
/// The default root is a tilde path and no crate in the dependency budget does
/// this.
fn sh_expand(path: &std::path::Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ltmx_domain::{ChunkId, Origin, SourceRef};

    #[test]
    fn expands_home_prefixed_paths() {
        // SAFETY: single-threaded test process; no other reader of HOME.
        unsafe { std::env::set_var("HOME", "/tmp/fake-home") };
        let expanded = sh_expand(&PathBuf::from("~/.claude/projects"));
        assert_eq!(expanded, PathBuf::from("/tmp/fake-home/.claude/projects"));
    }

    #[test]
    fn leaves_absolute_paths_untouched() {
        let absolute = PathBuf::from("/Users/joe/.claude/projects");
        assert_eq!(sh_expand(&absolute), absolute);
    }

    #[test]
    fn eval_queries_cover_all_four_categories() {
        let categories: std::collections::HashSet<_> =
            EVAL_QUERIES.iter().map(|(c, _)| *c).collect();
        assert_eq!(categories.len(), 4, "categories should be distinct");
    }

    #[test]
    fn eval_queries_are_unique() {
        let queries: std::collections::HashSet<_> = EVAL_QUERIES.iter().map(|(_, q)| *q).collect();
        assert_eq!(queries.len(), EVAL_QUERIES.len());
    }

    #[test]
    fn strategy_renders_as_lowercase_name() {
        assert_eq!(Strategy::Lexical.to_string(), "lexical");
        assert_eq!(Strategy::Hybrid.to_string(), "hybrid");
    }

    #[test]
    fn oldest_and_newest_span_the_chunks() {
        let at = |s: i64| Utc.timestamp_opt(s, 0).single().expect("valid");
        let chunk = |id: &str, text: &str, secs: i64| {
            ActivityChunk::new(
                ChunkId::new(id),
                text,
                Origin::Note,
                SourceRef::file("/x"),
                at(secs),
            )
        };
        let chunks = vec![
            chunk("a", "x", 300),
            chunk("b", "y", 100),
            chunk("c", "z", 200),
        ]
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<_>>();
        assert_eq!(oldest(&chunks), Some(at(100)));
        assert_eq!(newest(&chunks), Some(at(300)));
    }

    #[test]
    fn oldest_and_newest_handle_empty_input() {
        assert_eq!(oldest(&[]), None);
        assert_eq!(newest(&[]), None);
    }
}
