//! Token cost tracking hook.
//!
//! `CostTracker` is a [`Hook`] that listens for `HookEvent::PostLlmRequest`
//! and appends a JSONL row per request to `<project_sessions_dir>/cost.jsonl`.
//! It also estimates a USD cost using a small built-in price table.
//!
//! The price table lives in [`MODEL_PRICES`]. Prices are per 1K tokens.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::hooks::{Hook, HookEvent, HookResult};

/// Per-1K-token USD price entry for one model.
#[derive(Debug, Clone, Copy)]
pub struct ModelPrice {
    pub model: &'static str,
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub cache_write_per_1k: f64,
    pub cache_read_per_1k: f64,
}

/// Per-1K-token USD prices. Keep entries sorted by most-specific prefix first;
/// lookup uses `str::starts_with` so a model id returned by the API (which may
/// include a date suffix, e.g. `claude-sonnet-4-5-20250929`) still matches.
pub static MODEL_PRICES: &[ModelPrice] = &[
    ModelPrice {
        model: "claude-opus-4-6",
        input_per_1k: 15.0,
        output_per_1k: 75.0,
        cache_write_per_1k: 18.75,
        cache_read_per_1k: 1.50,
    },
    ModelPrice {
        model: "claude-sonnet-4-5",
        input_per_1k: 3.0,
        output_per_1k: 15.0,
        cache_write_per_1k: 3.75,
        cache_read_per_1k: 0.30,
    },
    ModelPrice {
        model: "gpt-4o",
        input_per_1k: 2.50,
        output_per_1k: 10.0,
        cache_write_per_1k: 0.0,
        cache_read_per_1k: 0.0,
    },
];

pub fn price_for(model: &str) -> Option<ModelPrice> {
    MODEL_PRICES
        .iter()
        .find(|p| model.starts_with(p.model))
        .copied()
}

/// Estimate cost (USD) for one LLM response, given the token breakdown.
pub fn estimate_usd(
    model: &str,
    tokens_in: u64,
    tokens_out: u64,
    cache_in: u64,
    cache_read: u64,
) -> f64 {
    let Some(price) = price_for(model) else {
        return 0.0;
    };
    let k = 1000.0;
    (tokens_in as f64) * price.input_per_1k / k
        + (tokens_out as f64) * price.output_per_1k / k
        + (cache_in as f64) * price.cache_write_per_1k / k
        + (cache_read as f64) * price.cache_read_per_1k / k
}

/// A single JSONL row written by [`CostTracker`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    pub timestamp: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_in: u64,
    pub cache_read: u64,
    pub estimated_usd: f64,
}

/// Hook that appends `PostLlmRequest` events to a JSONL file.
pub struct CostTracker {
    path: PathBuf,
    // Serialize writes across concurrent agents in the same process.
    lock: Mutex<()>,
}

impl CostTracker {
    /// Create a tracker that writes to `<dir>/cost.jsonl`. Ensures the parent
    /// directory exists.
    pub fn new(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            path: dir.join("cost.jsonl"),
            lock: Mutex::new(()),
        })
    }

    /// Full path to the JSONL file this tracker writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, record: &CostRecord) {
        let line = match serde_json::to_string(record) {
            Ok(s) => s,
            Err(_) => return,
        };
        let _guard = self.lock.lock().ok();
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{}", line);
        }
    }

    /// Read every `CostRecord` from `<dir>/cost.jsonl`. Unknown lines are skipped.
    pub fn read_all(path: impl AsRef<Path>) -> std::io::Result<Vec<CostRecord>> {
        let path = path.as_ref();
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<CostRecord>(l).ok())
            .collect())
    }
}

impl Hook for CostTracker {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        if let HookEvent::PostLlmRequest {
            tokens_in,
            tokens_out,
            cache_in,
            cache_read,
            model,
            ..
        } = event
        {
            let record = CostRecord {
                timestamp: Utc::now().to_rfc3339(),
                model: model.clone(),
                tokens_in: *tokens_in,
                tokens_out: *tokens_out,
                cache_in: *cache_in,
                cache_read: *cache_read,
                estimated_usd: estimate_usd(
                    model,
                    *tokens_in,
                    *tokens_out,
                    *cache_in,
                    *cache_read,
                ),
            };
            self.append(&record);
        }
        HookResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_lookup_matches_prefix() {
        let p = price_for("claude-sonnet-4-5-20250929").expect("prefix match");
        assert!((p.input_per_1k - 3.0).abs() < 1e-9);

        assert!(price_for("totally-unknown-model").is_none());
    }

    #[test]
    fn estimate_opus_with_cache() {
        // 1K input + 1K output on opus = $15 + $75 = $90; no cache.
        let usd = estimate_usd("claude-opus-4-6", 1000, 1000, 0, 0);
        assert!((usd - 90.0).abs() < 1e-9);
    }
}
