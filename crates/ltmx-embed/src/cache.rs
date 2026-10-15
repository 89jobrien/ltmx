//! On-disk cache for embeddings.
//!
//! # Why this exists
//!
//! Embedding the corpus is the dominant cost of every dense run. Measured on
//! this machine, `nomic-embed-text` sustains ~5 ms/text batched, which puts the
//! 57,766 transcript chunks at roughly 366 seconds and the 363,791 archived
//! Pieces chunks at roughly half an hour. That price makes it impossible to
//! A/B two embedding models, because each arm costs a full rebuild and you end
//! up rationalising whichever you ran last.
//!
//! Caching turns that cost into a one-off. The second run reads from disk and
//! embeds only what it has not seen.
//!
//! # Design
//!
//! This is a **decorator over the [`Embedder`] port**, not a separate
//! subsystem. `DenseIndex` only ever calls `embed_batch`, so wrapping the
//! embedder is transparent to the retrieval layer and no index changes.
//!
//! ```text
//! DenseIndex<CachingEmbedder<OllamaEmbedder>>
//! ```
//!
//! # Correctness
//!
//! A cache that returns a stale vector is worse than no cache: retrieval
//! silently degrades and nothing reports an error. Three things guard that.
//!
//! - **The key is the text, not the chunk id.** Chunk ids derive from document
//!   uuids, so an edited transcript keeps its id while its text changes.
//!   Keying on id would serve a stale vector forever.
//! - **The key includes `model_id`.** The port requires `model_id` to change
//!   whenever produced vectors would change. Swapping `nomic-embed-text` for
//!   `nomic-embed-text-v2-moe` therefore invalidates every entry instead of
//!   mixing two models in one index.
//! - **The header is checked before anything is read.** A cache written by a
//!   different model, dimension, or format version is treated as absent rather
//!   than partially trusted.
//!
//! ## Hash collisions
//!
//! Entries are keyed by a 128-bit hash of `model_id || text` rather than the
//! text itself, which keeps the file small and lookups O(1). Holding the text
//! would eliminate the concern at the cost of roughly 889 MB for the archive
//! corpus. The residual risk is about one in 10^28 for 421k entries, and the
//! stored text *length* is compared alongside the hash as a cheap secondary
//! check. That is a deliberate trade, not an oversight; see
//! `docs/adrs/ADR-014`.
//!
//! # Durability
//!
//! Writes go to a temporary file which is flushed, `fsync`ed, and then renamed
//! over the target. A crash mid-write therefore leaves the previous cache
//! intact rather than a truncated file that would fail to parse on next load.

use ltmx_domain::{EmbedError, Embedder};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
use std::sync::Arc;

/// Magic bytes at the head of a cache file.
const MAGIC: &[u8; 8] = b"LTMXEMBC";

/// Format version. Bump when the record layout changes.
const VERSION: u32 = 1;

/// A 128-bit content key.
///
/// Two independent 64-bit FNV-1a passes with different offset bases. The seeds
/// are what separate them; a single 128-bit multiply-shift would be no better
/// than the 64-bit version it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key([u64; 2]);

const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const FNV_OFFSET_A: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_OFFSET_B: u64 = 0x9e37_79b9_7f4a_7c15;

/// Derives a 128-bit key from the model identity and the text.
fn key_of(model_id: &str, text: &str) -> Key {
    let mut a = FNV_OFFSET_A;
    let mut b = FNV_OFFSET_B;
    for byte in model_id.as_bytes().iter().chain(text.as_bytes()) {
        a ^= u64::from(*byte);
        a = a.wrapping_mul(FNV_PRIME);
        b ^= u64::from(*byte);
        b = b.wrapping_mul(FNV_PRIME).rotate_left(7);
    }
    Key([a, b])
}

/// One cached embedding, with enough metadata to detect a stale entry.
#[derive(Debug, Clone)]
struct Entry {
    /// Byte length of the source text, a cheap secondary check on the hash.
    text_len: u64,
    /// The vector, exactly `dim` wide.
    vector: Vec<f32>,
}

/// Counts describing what a cache load did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Entries already on disk when the cache was opened.
    pub loaded: usize,
    /// Lookups satisfied from disk.
    pub hits: usize,
    /// Lookups that had to be embedded.
    pub misses: usize,
}

/// Why a cache could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheProblem {
    /// No cache file exists yet. Not an error.
    Missing,
    /// The file exists but was written by an incompatible cache.
    Incompatible(String),
}

impl std::fmt::Display for CacheProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("no cache file"),
            Self::Incompatible(why) => write!(f, "incompatible cache: {why}"),
        }
    }
}

/// Texts processed between progress reports and flush checks.
///
/// Bounds both memory and loss. Holding every pending text in one `Vec` meant a
/// 421k-chunk corpus built a ~435 MB duplicate of the corpus before a single
/// vector came back; flushing only at the end meant a crash at minute 25 of a
/// 30-minute run discarded all of it.
const SLICE: usize = 4_096;

/// Embeddings accumulated before the cache is flushed to disk.
///
/// Bounds the loss from a crash to this many embeddings. At ~1.3 KB per vector
/// that is a minute of work per flush: frequent enough that a crash is cheap to
/// recover from, rare enough that the write is noise.
const FLUSH_EVERY: usize = 50_000;

/// A progress report from a long embedding run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Texts looked up so far in this batch.
    pub processed: usize,
    /// Total texts in this batch.
    pub total: usize,
    /// Texts served from disk in this batch.
    pub hits: usize,
    /// Texts sent to the backend in this batch.
    pub misses: usize,
    /// Entries currently held in memory.
    pub cached: usize,
    /// Wall time since the batch started.
    pub elapsed: std::time::Duration,
}

/// A callback invoked as a batch advances.
pub type ProgressFn = Box<dyn Fn(Progress) + Send + Sync>;

/// An [`Embedder`] that serves repeats from a file on disk.
///
/// Wrapping rather than modifying a backend keeps every backend cacheable
/// without opt-in, and keeps the cache swappable for tests.
pub struct CachingEmbedder<E> {
    inner: E,
    path: PathBuf,
    model_id: String,
    dimensions: usize,
    entries: Mutex<HashMap<Key, Entry>>,
    /// Lookups served from disk, and lookups sent to the backend.
    ///
    /// Atomics rather than fields behind the entries mutex: `stats` is read
    /// while the embedder may be shared across threads, and contention on the
    /// hot path should not be the price of a counter.
    hits: AtomicUsize,
    misses: AtomicUsize,
    /// Embeddings embedded since the last flush, driving periodic saves.
    since_flush: AtomicUsize,
    /// Called as a batch advances. Deliberately not on the `Embedder` port:
    /// reporting progress is not something every backend can or should do.
    on_progress: Option<ProgressFn>,
    /// What happened when the cache was opened.
    loaded: Mutex<Option<Result<CacheStats, CacheProblem>>>,
}

/// Hand-written because a boxed closure is not printable and the derive would
/// otherwise force the field out. Reports the shape that is useful in a log —
/// where the cache lives, what model it holds, and whether it reports progress.
impl<E> std::fmt::Debug for CachingEmbedder<E>
where
    E: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachingEmbedder")
            .field("inner", &self.inner)
            .field("path", &self.path)
            .field("model_id", &self.model_id)
            .field("dimensions", &self.dimensions)
            .field("entries", &self.entries.lock().map(|e| e.len()).ok())
            .field("reports_progress", &self.on_progress.is_some())
            .finish()
    }
}

impl<E> CachingEmbedder<E>
where
    E: Embedder,
{
    /// Opens (or creates) a cache at `path`, wrapping `inner`.
    ///
    /// A missing cache is normal on first run. An *incompatible* one is also
    /// tolerated -- it is rebuilt from scratch -- but is reported through
    /// [`Self::problem`] so the caller can say so out loud rather than
    /// silently discarding someone's model configuration.
    pub fn open(inner: E, path: impl Into<PathBuf>) -> Self {
        let model_id = inner.model_id();
        let dimensions = inner.dimensions();
        let path = path.into();

        let (entries, loaded) = match read_cache(&path, &model_id, dimensions) {
            Ok((entries, stats)) => (entries, Ok(stats)),
            Err(problem) => (HashMap::new(), Err(problem)),
        };

        Self {
            inner,
            path,
            model_id,
            dimensions,
            entries: Mutex::new(entries),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
            since_flush: AtomicUsize::new(0),
            on_progress: None,
            loaded: Mutex::new(Some(loaded)),
        }
    }

    /// Registers a progress callback, called as a batch advances.
    ///
    /// Returns `self` so it chains onto [`Self::open`]. A 30-minute index run
    /// that prints nothing is indistinguishable from a hung process, which is
    /// the problem this exists to solve.
    pub fn with_progress<F>(mut self, callback: F) -> Self
    where
        F: Fn(Progress) + Send + Sync + 'static,
    {
        self.on_progress = Some(Box::new(callback));
        self
    }

    /// The cache file backing this embedder.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What happened when the cache was opened.
    ///
    /// [`None`] once [`Self::save`] has consumed it.
    pub fn problem(&self) -> Option<CacheProblem> {
        self.loaded.lock().ok().and_then(|g| match &*g {
            Some(Err(problem)) => Some(problem.clone()),
            _ => None,
        })
    }

    /// Counters for this process's lookups.
    ///
    /// `hits + misses` is the number of *texts* looked up. The backend is
    /// called once per batch containing at least one miss, not once per miss,
    /// so this is not a count of round trips.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            loaded: self
                .loaded
                .lock()
                .ok()
                .and_then(|g| match &*g {
                    Some(Ok(stats)) => Some(stats.loaded),
                    _ => None,
                })
                .unwrap_or_default(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// Writes the cache to disk atomically.
    ///
    /// Returns the number of entries written. Safe to call when nothing was
    /// embedded: it rewrites what was loaded plus whatever was added.
    pub fn save(&self) -> std::io::Result<usize> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| std::io::Error::other("embedding cache lock poisoned"))?;
        write_cache(&self.path, &self.model_id, self.dimensions, &entries)?;
        Ok(entries.len())
    }

    /// Records freshly produced vectors, ignoring any already held.
    fn record(&self, texts: &[String], vectors: &[Vec<f32>]) {
        if let Ok(mut entries) = self.entries.lock() {
            for (text, vector) in texts.iter().zip(vectors) {
                let key = key_of(&self.model_id, text);
                entries.entry(key).or_insert_with(|| Entry {
                    text_len: text.len() as u64,
                    vector: vector.clone(),
                });
            }
        }
    }

    /// Writes the cache when enough has accumulated since the last flush.
    ///
    /// Failures are logged rather than raised: losing durability part-way
    /// through a long run is strictly better than aborting it, and the final
    /// `save` will still surface a genuine problem with the cache file.
    fn flush_if_due(&self) {
        let since = self.since_flush.fetch_add(SLICE, Ordering::Relaxed);
        if since + SLICE < FLUSH_EVERY {
            return;
        }
        self.since_flush.store(0, Ordering::Relaxed);
        match self.save() {
            Ok(count) => eprintln!("  cache flushed: {count} entries"),
            Err(e) => eprintln!("  cache flush failed: {e}"),
        }
    }
}

impl<E> Embedder for CachingEmbedder<E>
where
    E: Embedder,
{
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let owned = text.to_string();
        self.embed_batch(std::slice::from_ref(&owned))
            .map(|mut v| v.pop().unwrap_or_default())
    }

    /// Embeds only the items not already cached, in bounded slices.
    ///
    /// Every item is looked up individually and only genuine misses reach the
    /// backend, still in one call per slice. Work is done a [`SLICE`] at a time
    /// so memory stays bounded regardless of corpus size, which is also what
    /// gives the progress callback and the periodic flush somewhere to fire.
    ///
    /// An earlier version built one `Vec` of every pending text and called the
    /// backend once. On the 421k-chunk corpus that meant a ~435 MB duplicate of
    /// the corpus held before the first vector returned, and a crash lost the
    /// whole run.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let started = std::time::Instant::now();
        let total = texts.len();
        let mut out = Vec::with_capacity(total);
        let mut done_hits = 0usize;
        let mut done_misses = 0usize;

        for slice in texts.chunks(SLICE) {
            let (mut slots, miss_texts) = {
                let entries = self
                    .entries
                    .lock()
                    .map_err(|_| EmbedError::Decode("embedding cache lock poisoned".into()))?;
                let mut slots = Vec::with_capacity(slice.len());
                let mut miss_texts = Vec::new();
                for text in slice {
                    let key = key_of(&self.model_id, text);
                    match entries
                        .get(&key)
                        .filter(|entry| entry.text_len == text.len() as u64)
                    {
                        Some(entry) => slots.push(Some(entry.vector.clone())),
                        None => {
                            slots.push(None);
                            miss_texts.push(text.clone());
                        }
                    }
                }
                (slots, miss_texts)
            };

            let slice_hits = slice.len() - miss_texts.len();
            let slice_misses = miss_texts.len();
            done_hits += slice_hits;
            done_misses += slice_misses;

            if !miss_texts.is_empty() {
                let fresh = self.inner.embed_batch(&miss_texts)?;
                if fresh.len() != slice_misses {
                    return Err(EmbedError::Decode(format!(
                        "backend returned {} vectors for {} inputs",
                        fresh.len(),
                        slice_misses
                    )));
                }
                for vector in &fresh {
                    if vector.len() != self.dimensions {
                        return Err(EmbedError::DimensionMismatch {
                            expected: self.dimensions,
                            actual: vector.len(),
                        });
                    }
                }
                self.record(&miss_texts, &fresh);

                let mut produced = fresh.into_iter();
                for slot in &mut slots {
                    if slot.is_none() {
                        *slot = Some(produced.next().ok_or_else(|| {
                            EmbedError::Decode("backend produced too few vectors".into())
                        })?);
                    }
                }
            }

            out.extend(
                slots
                    .into_iter()
                    .map(|slot| slot.unwrap_or_else(|| vec![0.0; self.dimensions])),
            );

            self.hits.fetch_add(slice_hits, Ordering::Relaxed);
            self.misses.fetch_add(slice_misses, Ordering::Relaxed);

            if let Some(callback) = &self.on_progress {
                callback(Progress {
                    processed: out.len(),
                    total,
                    hits: done_hits,
                    misses: done_misses,
                    cached: self.entries.lock().map(|e| e.len()).unwrap_or(0),
                    elapsed: started.elapsed(),
                });
            }

            self.flush_if_due();
        }
        Ok(out)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn model_id(&self) -> String {
        self.model_id.clone()
    }
}

/// Reads a cache file, rejecting anything written by a different configuration.
fn read_cache(
    path: &Path,
    model_id: &str,
    dimensions: usize,
) -> Result<(HashMap<Key, Entry>, CacheStats), CacheProblem> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CacheProblem::Missing);
        }
        Err(e) => return Err(CacheProblem::Incompatible(format!("cannot open: {e}"))),
    };
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 8];
    reader
        .read_exact(&mut magic)
        .map_err(|e| CacheProblem::Incompatible(format!("short header: {e}")))?;
    if &magic != MAGIC {
        return Err(CacheProblem::Incompatible("bad magic".into()));
    }

    let version = read_u32(&mut reader)?;
    if version != VERSION {
        return Err(CacheProblem::Incompatible(format!(
            "format version {version}, expected {VERSION}"
        )));
    }

    let dim = read_u32(&mut reader)? as usize;
    if dim != dimensions {
        return Err(CacheProblem::Incompatible(format!(
            "cached width {dim}, embedder width {dimensions}"
        )));
    }

    let stored_model = read_string(&mut reader)?;
    if stored_model != model_id {
        return Err(CacheProblem::Incompatible(format!(
            "cached model `{stored_model}`, current `{model_id}`"
        )));
    }

    let count = read_u64(&mut reader)? as usize;
    let mut entries = HashMap::with_capacity(count);
    for _ in 0..count {
        let mut key = [0u8; 16];
        reader
            .read_exact(&mut key)
            .map_err(|e| CacheProblem::Incompatible(format!("truncated key: {e}")))?;
        let text_len = read_u64(&mut reader)?;
        let mut vector = vec![0.0f32; dim];
        unsafe_free_read_f32s(&mut reader, &mut vector)?;
        entries.insert(
            Key([
                u64::from_le_bytes(key[0..8].try_into().expect("8 bytes")),
                u64::from_le_bytes(key[8..16].try_into().expect("8 bytes")),
            ]),
            Entry { text_len, vector },
        );
    }

    Ok((
        entries,
        CacheStats {
            loaded: count,
            ..CacheStats::default()
        },
    ))
}

/// Reads `vector.len()` little-endian `f32`s without unsafe.
fn unsafe_free_read_f32s(reader: &mut impl Read, vector: &mut [f32]) -> Result<(), CacheProblem> {
    let mut bytes = vec![0u8; vector.len() * 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| CacheProblem::Incompatible(format!("truncated vector: {e}")))?;
    for (slot, raw) in vector.iter_mut().zip(bytes.chunks_exact(4)) {
        *slot = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    }
    Ok(())
}

fn read_u32(reader: &mut impl Read) -> Result<u32, CacheProblem> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| CacheProblem::Incompatible(format!("truncated field: {e}")))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, CacheProblem> {
    let mut bytes = [0u8; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| CacheProblem::Incompatible(format!("truncated field: {e}")))?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_string(reader: &mut impl Read) -> Result<String, CacheProblem> {
    let len = read_u32(reader)? as usize;
    let mut bytes = vec![0u8; len];
    reader
        .read_exact(&mut bytes)
        .map_err(|e| CacheProblem::Incompatible(format!("truncated string: {e}")))?;
    String::from_utf8(bytes).map_err(|e| CacheProblem::Incompatible(format!("bad utf-8: {e}")))
}

/// Writes the cache atomically: temp file, flush, `fsync`, rename.
fn write_cache(
    path: &Path,
    model_id: &str,
    dimensions: usize,
    entries: &HashMap<Key, Entry>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = PathBuf::from(path);
    temp.set_extension("tmp");

    let file = File::create(&temp)?;
    let mut writer = BufWriter::new(file);

    writer.write_all(MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&(dimensions as u32).to_le_bytes())?;
    writer.write_all(&(model_id.len() as u32).to_le_bytes())?;
    writer.write_all(model_id.as_bytes())?;
    writer.write_all(&(entries.len() as u64).to_le_bytes())?;

    // Deterministic order makes the file reproducible, which matters when
    // comparing two caches byte-for-byte to prove a model swap changed them.
    let mut ordered: Vec<(&Key, &Entry)> = entries.iter().collect();
    ordered.sort_unstable_by_key(|(key, _)| key.0);

    for (key, entry) in ordered {
        writer.write_all(&key.0[0].to_le_bytes())?;
        writer.write_all(&key.0[1].to_le_bytes())?;
        writer.write_all(&entry.text_len.to_le_bytes())?;
        for value in &entry.vector {
            writer.write_all(&value.to_le_bytes())?;
        }
    }

    writer.flush()?;
    // fsync the data before the rename, so a crash cannot leave a renamed but
    // empty file in place of a good cache.
    writer
        .get_ref()
        .sync_all()
        .map_err(|e| std::io::Error::other(format!("fsync failed: {e}")))?;
    drop(writer);

    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A deterministic stand-in embedder that records how often it was called.
    ///
    /// The counter is a shared `Arc` so a test can still read it after the
    /// embedder has been moved into the cache. That distinction matters: the
    /// claim worth testing is not "the cache counted a hit" but "the backend
    /// was never asked", which is the difference between a 30-minute run and a
    /// 30-second one.
    #[derive(Debug, Clone)]
    struct CountingEmbedder {
        calls: Arc<AtomicUsize>,
        dimensions: usize,
        model: String,
    }

    impl CountingEmbedder {
        fn new(dimensions: usize, model: &str) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                dimensions,
                model: model.to_string(),
            }
        }

        /// Derives a vector from the text so misses are observable.
        fn vector_for(&self, text: &str) -> Vec<f32> {
            let mut out = vec![0.0f32; self.dimensions];
            for (i, byte) in text.bytes().enumerate() {
                out[i % self.dimensions] += f32::from(byte) / 255.0;
            }
            out
        }
    }

    impl Embedder for CountingEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.vector_for(text))
        }

        fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts.iter().map(|t| self.vector_for(t)).collect())
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn model_id(&self) -> String {
            self.model.clone()
        }
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("temp dir");
        // Leak the directory so the file outlives the test handle.
        let path = dir.keep();
        path.join(name)
    }

    #[test]
    fn first_run_misses_then_second_run_hits() {
        let path = temp_path("cache.bin");
        let corpus = texts(&["alpha", "beta", "gamma"]);

        let first = CountingEmbedder::new(4, "m1");
        let cache = CachingEmbedder::open(first, &path);
        let out = cache.embed_batch(&corpus).expect("embed");
        assert_eq!(out.len(), 3);
        cache.save().expect("save");
        assert_eq!(cache.stats().hits, 0);

        let second = CountingEmbedder::new(4, "m1");
        let backend_calls = second.calls.clone();
        let cache = CachingEmbedder::open(second, &path);
        assert_eq!(cache.stats().loaded, 3);
        let out = cache.embed_batch(&corpus).expect("embed from cache");
        assert_eq!(out.len(), 3);
        assert_eq!(cache.stats().hits, 3);
        assert_eq!(
            backend_calls.load(Ordering::SeqCst),
            0,
            "a fully warm cache must not call the backend at all"
        );
    }

    /// The property the whole cache rests on: a warm run produces the same
    /// vectors as a cold one, byte for byte.
    #[test]
    fn warm_cache_reproduces_cold_vectors() {
        let path = temp_path("reproduce.bin");
        let corpus = texts(&["one", "two", "three", "four"]);

        let cold = CountingEmbedder::new(8, "m1");
        let cache = CachingEmbedder::open(cold, &path);
        let expected = cache.embed_batch(&corpus).expect("cold embed");
        cache.save().expect("save");

        let warm = CountingEmbedder::new(8, "m1");
        let cache = CachingEmbedder::open(warm, &path);
        let actual = cache.embed_batch(&corpus).expect("warm embed");
        assert_eq!(expected, actual);
    }

    /// A model swap must invalidate everything. Mixing two models' vectors in
    /// one index is the failure this guards.
    #[test]
    fn model_swap_invalidates_cache() {
        let path = temp_path("swap.bin");
        let corpus = texts(&["alpha"]);

        let first = CountingEmbedder::new(4, "model-a");
        let cache = CachingEmbedder::open(first, &path);
        cache.embed_batch(&corpus).expect("embed");
        cache.save().expect("save");

        let second = CountingEmbedder::new(4, "model-b");
        let cache = CachingEmbedder::open(second, &path);
        assert_eq!(cache.stats().loaded, 0, "entries must not survive a swap");
        assert!(
            matches!(cache.problem(), Some(CacheProblem::Incompatible(_))),
            "the swap must be reported, not silently ignored"
        );
        cache.embed_batch(&corpus).expect("embed after swap");
        assert!(cache.stats().loaded == 0);
    }

    /// A width change must invalidate too, even at the same model id.
    #[test]
    fn dimension_change_invalidates_cache() {
        let path = temp_path("dim.bin");
        let corpus = texts(&["alpha"]);

        let first = CountingEmbedder::new(4, "m1");
        let cache = CachingEmbedder::open(first, &path);
        cache.embed_batch(&corpus).expect("embed");
        cache.save().expect("save");

        let wider = CountingEmbedder::new(8, "m1");
        let cache = CachingEmbedder::open(wider, &path);
        assert_eq!(cache.stats().loaded, 0);
        let out = cache.embed_batch(&corpus).expect("embed");
        assert_eq!(out[0].len(), 8);
    }

    /// The subtle one: chunk ids are stable across transcript edits, so keying
    /// on id would serve a stale vector forever. Keying on text must not.
    #[test]
    fn edited_text_is_not_served_from_cache() {
        let path = temp_path("edit.bin");

        let first = CountingEmbedder::new(4, "m1");
        let cache = CachingEmbedder::open(first, &path);
        cache.embed_batch(&texts(&["original"])).expect("embed");
        cache.save().expect("save");

        let second = CountingEmbedder::new(4, "m1");
        let backend_calls = second.calls.clone();
        let cache = CachingEmbedder::open(second, &path);
        // Same chunk, different content: must miss.
        cache.embed_batch(&texts(&["edited"])).expect("embed");
        assert_eq!(cache.stats().loaded, 1);
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(
            backend_calls.load(Ordering::SeqCst),
            1,
            "edited text must reach the backend"
        );
    }

    /// A partially warm batch embeds only its missing suffix.
    #[test]
    fn partially_warm_batch_embeds_only_the_misses() {
        let path = temp_path("partial.bin");

        let first = CountingEmbedder::new(4, "m1");
        let cache = CachingEmbedder::open(first, &path);
        cache.embed_batch(&texts(&["a", "b"])).expect("embed");
        cache.save().expect("save");

        let second = CountingEmbedder::new(4, "m1");
        let backend_calls = second.calls.clone();
        let cache = CachingEmbedder::open(second, &path);
        let out = cache
            .embed_batch(&texts(&["a", "b", "c", "d"]))
            .expect("embed");
        assert_eq!(out.len(), 4);
        assert_eq!(cache.stats().hits, 2);
        assert_eq!(
            backend_calls.load(Ordering::SeqCst),
            1,
            "a half-warm batch must make exactly one backend call"
        );
    }

    /// Regression: misses must be found wherever they fall, not only at the
    /// front.
    ///
    /// Transcripts are append-only, so a corpus grows by a handful of chunks
    /// between runs and the new chunks appear at arbitrary positions. An
    /// earlier implementation embedded everything after the first miss, which
    /// turned nine new chunks into ~25,000 needless embeddings.
    #[test]
    fn scattered_misses_do_not_poison_the_rest_of_the_batch() {
        let path = temp_path("scattered.bin");

        let first = CountingEmbedder::new(4, "m1");
        let cache = CachingEmbedder::open(first, &path);
        cache
            .embed_batch(&texts(&["a", "b", "c", "d", "e"]))
            .expect("embed");
        cache.save().expect("save");

        // One new item in the middle, everything else already known.
        let second = CountingEmbedder::new(4, "m1");
        let backend_calls = second.calls.clone();
        let cache = CachingEmbedder::open(second, &path);
        let out = cache
            .embed_batch(&texts(&["a", "b", "NEW", "d", "e"]))
            .expect("embed");

        assert_eq!(out.len(), 5);
        assert_eq!(cache.stats().hits, 4, "only the new item should miss");
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(
            backend_calls.load(Ordering::SeqCst),
            1,
            "one backend call, carrying only the new item"
        );
    }

    /// The port requires order preservation. A cache that assembles results in
    /// the wrong order would satisfy every count assertion and still be wrong,
    /// so this pins the mixed cached/embedded case specifically.
    #[test]
    fn mixed_batch_preserves_input_order() {
        let path = temp_path("order.bin");

        let first = CountingEmbedder::new(4, "m1");
        let warm = CachingEmbedder::open(first, &path);
        let corpus = texts(&["one", "two", "three", "four", "five"]);
        let expected = warm.embed_batch(&corpus).expect("embed");
        warm.save().expect("save");

        let second = CountingEmbedder::new(4, "m1");
        let cache = CachingEmbedder::open(second, &path);
        let mixed = texts(&["one", "NEW-A", "three", "NEW-B", "five"]);
        let actual = cache.embed_batch(&mixed).expect("embed");
        assert_eq!(actual.len(), 5);

        // Cached positions must match the cold run.
        assert_eq!(actual[0], expected[0]);
        assert_eq!(actual[2], expected[2]);
        assert_eq!(actual[4], expected[4]);

        // Embedded positions must match what a cold run would have produced.
        let cold = CountingEmbedder::new(4, "m1");
        assert_eq!(actual[1], cold.embed("NEW-A").expect("embed"));
        assert_eq!(actual[3], cold.embed("NEW-B").expect("embed"));
    }

    /// Progress must actually be reported, monotonically, and reach the total.
    ///
    /// A 30-minute index run that prints nothing is indistinguishable from a
    /// hung process, which is the problem the callback exists to solve.
    #[test]
    fn progress_is_reported_and_ends_at_total() {
        use std::sync::atomic::AtomicUsize;
        static SEEN: AtomicUsize = AtomicUsize::new(0);

        let path = temp_path("progress.bin");
        let cache =
            CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path).with_progress(|p| {
                SEEN.fetch_add(1, Ordering::SeqCst);
                assert!(p.processed <= p.total);
                assert_eq!(p.hits + p.misses, p.processed);
            });

        // More than one slice, so the callback must fire repeatedly.
        let corpus: Vec<String> = (0..SLICE * 2 + 7).map(|i| format!("text-{i}")).collect();
        let expected = corpus.len();
        let out = cache.embed_batch(&corpus).expect("embed");

        assert_eq!(out.len(), expected);
        assert!(
            SEEN.load(Ordering::SeqCst) >= 3,
            "expected a report per slice, got {}",
            SEEN.load(Ordering::SeqCst)
        );
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().misses, expected);
    }

    /// A long run must leave durable state behind before it ends, so a crash
    /// costs one flush interval rather than the whole run.
    #[test]
    fn cache_is_flushed_midway_through_a_long_run() {
        let path = temp_path("flush.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);

        // Enough to cross FLUSH_EVERY in slices, but stop short of the end.
        let corpus: Vec<String> = (0..FLUSH_EVERY + SLICE).map(|i| format!("t-{i}")).collect();
        cache.embed_batch(&corpus).expect("embed");

        assert!(
            path.exists(),
            "the cache must hit disk before the batch finishes"
        );
        // And a fresh embedder must be able to read it.
        let reopened = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert!(
            reopened.stats().loaded >= FLUSH_EVERY,
            "expected at least FLUSH_EVERY entries on disk, got {}",
            reopened.stats().loaded
        );
    }

    /// A run shorter than one flush interval must not write prematurely.
    #[test]
    fn short_run_does_not_flush() {
        let path = temp_path("noflush.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        cache.embed_batch(&texts(&["a", "b", "c"])).expect("embed");
        assert!(
            !path.exists(),
            "a three-item batch must not trigger a flush"
        );
        cache.save().expect("explicit save");
        assert!(path.exists());
    }

    /// Slicing must not change results. The backend sees at most `SLICE` texts
    /// per call, so this proves the slicing is invisible above the decorator.
    #[test]
    fn slicing_preserves_results_and_backend_batch_size() {
        let path = temp_path("slicing.bin");
        let corpus: Vec<String> = (0..SLICE * 2 + 3).map(|i| format!("item-{i}")).collect();

        let cold = CachingEmbedder::open(CountingEmbedder::new(6, "m1"), &path);
        let expected = cold.embed_batch(&corpus).expect("cold");
        // Below FLUSH_EVERY, so nothing hit disk on its own -- the test has to.
        cold.save().expect("save");

        let second = CountingEmbedder::new(6, "m1");
        let backend_calls = second.calls.clone();
        let warm = CachingEmbedder::open(second, &path);
        let actual = warm.embed_batch(&corpus).expect("warm");

        assert_eq!(expected, actual, "slicing must not change results");
        assert_eq!(warm.stats().hits, corpus.len());
        assert_eq!(warm.stats().misses, 0);
        assert_eq!(
            backend_calls.load(Ordering::SeqCst),
            0,
            "a fully warm run must not reach the backend"
        );
    }

    /// The backend must never be handed more than `SLICE` texts at once.
    ///
    /// This is the property that bounds memory, and it would regress silently
    /// if someone "optimised" the slice loop into a single call.
    #[test]
    fn backend_never_sees_more_than_one_slice() {
        struct LargestBatch(Arc<AtomicUsize>);
        impl Embedder for LargestBatch {
            fn embed(&self, _: &str) -> Result<Vec<f32>, EmbedError> {
                self.0.fetch_max(1, Ordering::SeqCst);
                Ok(vec![0.0; 4])
            }
            fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
                self.0.fetch_max(texts.len(), Ordering::SeqCst);
                Ok(vec![vec![0.0; 4]; texts.len()])
            }
            fn dimensions(&self) -> usize {
                4
            }
            fn model_id(&self) -> String {
                "m1".into()
            }
        }

        let seen = Arc::new(AtomicUsize::new(0));
        let cache = CachingEmbedder::open(LargestBatch(seen.clone()), temp_path("bound.bin"));
        let corpus: Vec<String> = (0..SLICE * 3).map(|i| format!("b-{i}")).collect();
        cache.embed_batch(&corpus).expect("embed");

        assert_eq!(
            seen.load(Ordering::SeqCst),
            SLICE,
            "backend batch must be bounded by SLICE"
        );
    }

    #[test]
    fn missing_cache_is_not_an_error() {
        let path = temp_path("absent.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert!(matches!(cache.problem(), Some(CacheProblem::Missing)));
        assert_eq!(cache.stats().loaded, 0);
        assert_eq!(cache.embed_batch(&texts(&["x"])).expect("embed").len(), 1);
    }

    #[test]
    fn empty_batch_short_circuits() {
        let path = temp_path("empty.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert!(cache.embed_batch(&[]).expect("empty").is_empty());
    }

    #[test]
    fn single_embed_uses_the_cache() {
        let path = temp_path("single.bin");
        let first = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        let expected = first.embed("hello").expect("embed");
        first.save().expect("save");

        let second = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert_eq!(second.embed("hello").expect("embed"), expected);
        assert_eq!(second.stats().hits, 1);
    }

    /// A truncated file must be rejected, not half-read. A cache that silently
    /// loses entries is worse than no cache.
    #[test]
    fn truncated_cache_is_rejected() {
        let path = temp_path("truncated.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        cache
            .embed_batch(&texts(&["alpha", "beta"]))
            .expect("embed");
        cache.save().expect("save");

        let bytes = std::fs::read(&path).expect("read");
        std::fs::write(&path, &bytes[..bytes.len() / 2]).expect("truncate");

        let reopened = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert_eq!(reopened.stats().loaded, 0);
        assert!(matches!(
            reopened.problem(),
            Some(CacheProblem::Incompatible(_))
        ));
    }

    #[test]
    fn corrupt_magic_is_rejected() {
        let path = temp_path("magic.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        cache.embed_batch(&texts(&["alpha"])).expect("embed");
        cache.save().expect("save");

        let mut bytes = std::fs::read(&path).expect("read");
        bytes[0] = b'X';
        std::fs::write(&path, bytes).expect("write");

        let reopened = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert_eq!(reopened.stats().loaded, 0);
    }

    /// Saving must not leave a `.tmp` behind on success.
    #[test]
    fn save_is_atomic_and_leaves_no_temp() {
        let path = temp_path("atomic.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        cache.embed_batch(&texts(&["alpha"])).expect("embed");
        let written = cache.save().expect("save");
        assert_eq!(written, 1);
        assert!(path.exists());
        let temp = path.with_extension("tmp");
        assert!(!temp.exists(), "temp file must be renamed away");
    }

    /// Saving twice with no new work must be stable, and a second cache must
    /// still read it.
    #[test]
    fn repeated_save_round_trips() {
        let path = temp_path("twice.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        cache.embed_batch(&texts(&["a", "b", "c"])).expect("embed");
        cache.save().expect("save one");
        cache.embed_batch(&texts(&["d"])).expect("embed more");
        assert_eq!(cache.save().expect("save two"), 4);

        let reopened = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        assert_eq!(reopened.stats().loaded, 4);
        assert_eq!(reopened.embed_batch(&texts(&["d"])).expect("warm").len(), 1);
        assert_eq!(reopened.stats().hits, 1);
    }

    #[test]
    fn key_distinguishes_model_and_text() {
        assert_ne!(key_of("a", "x"), key_of("b", "x"));
        assert_ne!(key_of("a", "x"), key_of("a", "y"));
        assert_eq!(key_of("a", "x"), key_of("a", "x"));
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested/deeper/cache.bin");
        let cache = CachingEmbedder::open(CountingEmbedder::new(4, "m1"), &path);
        cache.embed_batch(&texts(&["a"])).expect("embed");
        cache.save().expect("save");
        assert!(path.exists());
    }
}
