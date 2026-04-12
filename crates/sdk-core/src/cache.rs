//! Caching primitives for file state, tool results, usage statistics, and
//! cache-break detection.
//!
//! The main types are:
//!
//! - [`FileStateCache`] — LRU in-memory cache for file contents keyed by path
//!   and mtime.
//! - [`ToolResultStore`] — disk-backed storage for large tool results so they
//!   can be evicted from conversation context and reloaded on demand.
//! - [`StatsCache`] — append-only JSONL persistence for per-day usage
//!   statistics.
//! - [`CacheBreakDetector`] — heuristic that flags large drops in cache-read
//!   tokens between consecutive API calls.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Find the largest byte index `<= index` that is a valid char boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ─── FileStateCache ────────────────────────────────────────────────────────

use crate::config::CacheConfig;

/// A single cached file entry.
#[derive(Debug, Clone)]
pub struct CachedFile {
    pub content: String,
    pub mtime: u64,
    pub byte_size: usize,
}

/// Summary stats for the file cache.
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub entries: usize,
    pub total_bytes: usize,
    pub max_entries: usize,
    pub max_bytes: usize,
}

/// In-memory LRU cache for file contents, keyed by canonical path.
///
/// Lookups are validated against the current mtime so stale entries are
/// automatically discarded.  When the cache exceeds the configured
/// `max_entries` or `max_size_bytes` the least-recently-inserted entry
/// is evicted.
pub struct FileStateCache {
    entries: Mutex<HashMap<PathBuf, CachedFile>>,
    order: Mutex<Vec<PathBuf>>,
    current_size: Mutex<usize>,
    max_entries: usize,
    max_size_bytes: usize,
}

impl FileStateCache {
    /// Create an empty cache with default limits.
    pub fn new() -> Self {
        Self::with_config(CacheConfig::default())
    }

    /// Create an empty cache with explicit limits.
    pub fn with_config(config: CacheConfig) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            order: Mutex::new(Vec::new()),
            current_size: Mutex::new(0),
            max_entries: config.max_entries,
            max_size_bytes: config.max_size_bytes,
        }
    }

    /// Return the cached content for `path` if the entry exists **and** its
    /// recorded mtime matches `current_mtime`.
    pub fn get(&self, path: &Path, current_mtime: u64) -> Option<String> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(path)?;
        if entry.mtime == current_mtime {
            Some(entry.content.clone())
        } else {
            None
        }
    }

    /// Store `content` for `path` with the given `mtime`.
    ///
    /// If the path is already cached the old entry is replaced.  After
    /// insertion the cache is trimmed to stay within size and count limits.
    pub fn put(&self, path: &Path, content: String, mtime: u64) {
        let byte_size = content.len();
        let entry = CachedFile {
            content,
            mtime,
            byte_size,
        };

        let mut entries = match self.entries.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let mut order = match self.order.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let mut current_size = match self.current_size.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        // Remove old entry for this path if present.
        if let Some(old) = entries.remove(path) {
            *current_size = current_size.saturating_sub(old.byte_size);
            order.retain(|p| p != path);
        }

        // Insert new entry.
        *current_size += byte_size;
        entries.insert(path.to_path_buf(), entry);
        order.push(path.to_path_buf());

        // Evict oldest entries while over limits.
        while (entries.len() > self.max_entries || *current_size > self.max_size_bytes)
            && !order.is_empty()
        {
            let oldest = order.remove(0);
            if let Some(removed) = entries.remove(&oldest) {
                *current_size = current_size.saturating_sub(removed.byte_size);
            }
        }
    }

    /// Remove the entry for `path`, e.g. after a write that invalidates the
    /// cached content.
    pub fn invalidate(&self, path: &Path) {
        let mut entries = match self.entries.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(removed) = entries.remove(path) {
            if let Ok(mut sz) = self.current_size.lock() {
                *sz = sz.saturating_sub(removed.byte_size);
            }
            if let Ok(mut order) = self.order.lock() {
                order.retain(|p| p != path);
            }
        }
    }

    /// Drop all cached entries.
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
        if let Ok(mut order) = self.order.lock() {
            order.clear();
        }
        if let Ok(mut sz) = self.current_size.lock() {
            *sz = 0;
        }
    }

    /// Return a snapshot of current cache utilization.
    pub fn stats(&self) -> CacheStats {
        let entries = self.entries.lock().map(|g| g.len()).unwrap_or(0);
        let total_bytes = self.current_size.lock().map(|g| *g).unwrap_or(0);
        CacheStats {
            entries,
            total_bytes,
            max_entries: self.max_entries,
            max_bytes: self.max_size_bytes,
        }
    }
}

impl Default for FileStateCache {
    fn default() -> Self {
        Self::new()
    }
}

// ─── ToolResultStore ───────────────────────────────────────────────────────

/// Disk-backed store for large tool results.
///
/// Results are written as plain-text files under
/// `<base_dir>/<session_id>/<tool_use_id>.txt`.
pub struct ToolResultStore {
    base_dir: PathBuf,
}

impl ToolResultStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Persist `content` and return the path it was written to.
    pub fn store(
        &self,
        session_id: &str,
        tool_use_id: &str,
        content: &str,
    ) -> std::io::Result<PathBuf> {
        let dir = self.base_dir.join(session_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.txt", tool_use_id));
        std::fs::write(&path, content)?;
        Ok(path)
    }

    /// Load a previously stored result, returning `None` if the file does not
    /// exist or cannot be read.
    pub fn load(&self, session_id: &str, tool_use_id: &str) -> Option<String> {
        let path = self
            .base_dir
            .join(session_id)
            .join(format!("{}.txt", tool_use_id));
        std::fs::read_to_string(path).ok()
    }

    /// Return the first `max_bytes` of `content`, appending a truncation
    /// marker when the content is longer.
    pub fn preview(content: &str, max_bytes: usize) -> String {
        if content.len() <= max_bytes {
            return content.to_string();
        }
        let end = floor_char_boundary(content, max_bytes);
        let remaining = content.len() - end;
        let mut preview = content[..end].to_string();
        preview.push_str(&format!("\n... ({} more bytes)", remaining));
        preview
    }

    /// Total bytes used on disk by all stored results under `base_dir`.
    pub fn disk_usage(&self) -> std::io::Result<u64> {
        let mut total: u64 = 0;
        if !self.base_dir.exists() {
            return Ok(0);
        }
        for entry in std::fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                for inner in std::fs::read_dir(&path)? {
                    let inner = inner?;
                    total += inner.metadata().map(|m| m.len()).unwrap_or(0);
                }
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        Ok(total)
    }
}

// ─── StatsCache ────────────────────────────────────────────────────────────

/// A single row in the stats JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsEntry {
    pub date: String,
    pub requests: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_reads: u64,
    pub cache_writes: u64,
    pub estimated_usd: f64,
}

/// Aggregate view across multiple [`StatsEntry`] rows.
#[derive(Debug, Default)]
pub struct StatsSummary {
    pub total_requests: u64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_cache_reads: u64,
    pub total_cache_writes: u64,
    pub total_usd: f64,
    pub days_tracked: usize,
}

/// Helpers for reading and appending the stats JSONL file.
pub struct StatsCache;

impl StatsCache {
    /// Load all entries from a JSONL file, silently skipping malformed lines.
    pub fn load(path: &Path) -> Vec<StatsEntry> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let reader = std::io::BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            if let Ok(line) = line {
                if let Ok(entry) = serde_json::from_str::<StatsEntry>(&line) {
                    out.push(entry);
                }
            }
        }
        out
    }

    /// Atomically append a single entry to the JSONL file.
    pub fn append(path: &Path, entry: &StatsEntry) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let json = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writeln!(file, "{}", json)?;
        Ok(())
    }

    /// Compute an aggregate summary across the given entries.
    pub fn summary(entries: &[StatsEntry]) -> StatsSummary {
        let mut s = StatsSummary::default();
        let mut dates = std::collections::HashSet::new();
        for e in entries {
            s.total_requests += e.requests;
            s.total_tokens_in += e.tokens_in;
            s.total_tokens_out += e.tokens_out;
            s.total_cache_reads += e.cache_reads;
            s.total_cache_writes += e.cache_writes;
            s.total_usd += e.estimated_usd;
            dates.insert(e.date.clone());
        }
        s.days_tracked = dates.len();
        s
    }
}

// ─── CacheBreakDetector ────────────────────────────────────────────────────

/// Diagnostic details about why a cache break was detected.
#[derive(Debug)]
pub struct CacheBreakReason {
    pub system_changed: bool,
    pub tools_changed: bool,
    pub model_changed: bool,
    pub cache_read_drop_pct: f64,
}

/// Tracks state across consecutive API calls and flags probable cache breaks.
///
/// A cache break is reported when cache-read tokens drop by more than 5 %
/// **and** more than 2 000 tokens compared to the previous call.
pub struct CacheBreakDetector {
    last_system_hash: Option<u64>,
    last_tools_hash: Option<u64>,
    last_model: Option<String>,
    last_cache_reads: u64,
    // Snapshots of the *previous* call's state, used to detect what changed.
    pending_system_hash: Option<u64>,
    pending_tools_hash: Option<u64>,
    pending_model: Option<String>,
}

impl CacheBreakDetector {
    pub fn new() -> Self {
        Self {
            last_system_hash: None,
            last_tools_hash: None,
            last_model: None,
            last_cache_reads: 0,
            pending_system_hash: None,
            pending_tools_hash: None,
            pending_model: None,
        }
    }

    /// Snapshot the hashes and model name **before** an API call so that
    /// [`check_break`](Self::check_break) can detect changes.
    pub fn record_state(&mut self, system_hash: u64, tools_hash: u64, model: &str) {
        self.pending_system_hash = self.last_system_hash;
        self.pending_tools_hash = self.last_tools_hash;
        self.pending_model = self.last_model.clone();

        self.last_system_hash = Some(system_hash);
        self.last_tools_hash = Some(tools_hash);
        self.last_model = Some(model.to_string());
    }

    /// After receiving the API response, check whether cache-read tokens
    /// dropped significantly.  Returns `Some(reason)` if a break was detected.
    pub fn check_break(&mut self, cache_read_tokens: u64) -> Option<CacheBreakReason> {
        let prev = self.last_cache_reads;
        self.last_cache_reads = cache_read_tokens;

        // Need a previous baseline to compare against.
        if prev == 0 {
            return None;
        }

        let drop = prev.saturating_sub(cache_read_tokens);
        if drop < 2_000 {
            return None;
        }

        let drop_pct = (drop as f64 / prev as f64) * 100.0;
        if drop_pct <= 5.0 {
            return None;
        }

        let system_changed = match (self.pending_system_hash, self.last_system_hash) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let tools_changed = match (self.pending_tools_hash, self.last_tools_hash) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let model_changed = match (&self.pending_model, &self.last_model) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };

        Some(CacheBreakReason {
            system_changed,
            tools_changed,
            model_changed,
            cache_read_drop_pct: drop_pct,
        })
    }
}

impl Default for CacheBreakDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash an arbitrary string for use with [`CacheBreakDetector::record_state`].
pub fn hash_content(content: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_cache_get_put() {
        let cache = FileStateCache::new();
        assert!(cache.get(Path::new("/a.txt"), 1).is_none());

        cache.put(Path::new("/a.txt"), "hello".into(), 1);
        assert_eq!(cache.get(Path::new("/a.txt"), 1).unwrap(), "hello");
        // Stale mtime → miss
        assert!(cache.get(Path::new("/a.txt"), 2).is_none());
    }

    #[test]
    fn file_cache_invalidate() {
        let cache = FileStateCache::new();
        cache.put(Path::new("/b.txt"), "data".into(), 5);
        cache.invalidate(Path::new("/b.txt"));
        assert!(cache.get(Path::new("/b.txt"), 5).is_none());
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn file_cache_eviction() {
        let cache = FileStateCache::new();
        for i in 0..150 {
            let p = PathBuf::from(format!("/file_{}.txt", i));
            cache.put(&p, format!("content_{}", i), 1);
        }
        // Should have evicted down to default max_entries (100).
        assert!(cache.stats().entries <= CacheConfig::default().max_entries);
    }

    #[test]
    fn file_cache_clear() {
        let cache = FileStateCache::new();
        cache.put(Path::new("/x"), "abc".into(), 1);
        cache.clear();
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().total_bytes, 0);
    }

    #[test]
    fn tool_result_preview() {
        let short = "hello";
        assert_eq!(ToolResultStore::preview(short, 100), "hello");

        let long = "a".repeat(200);
        let preview = ToolResultStore::preview(&long, 50);
        assert!(preview.contains("... (150 more bytes)"));
        assert!(preview.len() < long.len());
    }

    #[test]
    fn tool_result_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());
        store.store("sess1", "tool_abc", "big result").unwrap();
        assert_eq!(store.load("sess1", "tool_abc").unwrap(), "big result");
        assert!(store.load("sess1", "missing").is_none());
        assert!(store.disk_usage().unwrap() > 0);
    }

    #[test]
    fn stats_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.jsonl");

        let entry = StatsEntry {
            date: "2026-04-12".into(),
            requests: 5,
            tokens_in: 1000,
            tokens_out: 500,
            cache_reads: 800,
            cache_writes: 200,
            estimated_usd: 0.05,
        };
        StatsCache::append(&path, &entry).unwrap();
        StatsCache::append(&path, &entry).unwrap();

        let loaded = StatsCache::load(&path);
        assert_eq!(loaded.len(), 2);

        let summary = StatsCache::summary(&loaded);
        assert_eq!(summary.total_requests, 10);
        assert_eq!(summary.days_tracked, 1);
    }

    #[test]
    fn cache_break_detector_no_break_on_first_call() {
        let mut d = CacheBreakDetector::new();
        d.record_state(1, 2, "claude");
        assert!(d.check_break(5000).is_none());
    }

    #[test]
    fn cache_break_detector_detects_large_drop() {
        let mut d = CacheBreakDetector::new();
        d.record_state(1, 2, "claude");
        assert!(d.check_break(10_000).is_none()); // baseline

        d.record_state(1, 2, "claude");
        // Drop from 10k to 1k = 90% drop, >2000 tokens
        let reason = d.check_break(1_000);
        assert!(reason.is_some());
        assert!(reason.unwrap().cache_read_drop_pct > 80.0);
    }

    #[test]
    fn cache_break_detector_ignores_small_drop() {
        let mut d = CacheBreakDetector::new();
        d.record_state(1, 2, "claude");
        assert!(d.check_break(10_000).is_none());

        d.record_state(1, 2, "claude");
        // Drop from 10k to 9.8k = 2% drop — below threshold
        assert!(d.check_break(9_800).is_none());
    }

    #[test]
    fn hash_content_deterministic() {
        let a = hash_content("hello world");
        let b = hash_content("hello world");
        let c = hash_content("hello world!");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
