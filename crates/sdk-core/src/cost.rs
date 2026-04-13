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
        model: "claude-opus-4",
        input_per_1k: 15.0,
        output_per_1k: 75.0,
        cache_write_per_1k: 18.75,
        cache_read_per_1k: 1.875,
    },
    ModelPrice {
        model: "claude-sonnet-4",
        input_per_1k: 3.0,
        output_per_1k: 15.0,
        cache_write_per_1k: 3.75,
        cache_read_per_1k: 0.30,
    },
    ModelPrice {
        model: "claude-haiku-4",
        input_per_1k: 0.80,
        output_per_1k: 4.0,
        cache_write_per_1k: 1.0,
        cache_read_per_1k: 0.08,
    },
    // gpt-4o-mini must appear before gpt-4o so the more-specific prefix matches first.
    ModelPrice {
        model: "gpt-4o-mini",
        input_per_1k: 0.15,
        output_per_1k: 0.60,
        cache_write_per_1k: 0.0,
        cache_read_per_1k: 0.0,
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
    fn price_lookup_matches_haiku() {
        let p = price_for("claude-haiku-4-0-20260101").expect("haiku prefix match");
        assert!((p.input_per_1k - 0.80).abs() < 1e-9);
    }

    #[test]
    fn price_lookup_matches_gpt4o_mini_before_gpt4o() {
        let p = price_for("gpt-4o-mini-2025").expect("gpt-4o-mini prefix match");
        assert!((p.input_per_1k - 0.15).abs() < 1e-9);
        let p2 = price_for("gpt-4o-2025").expect("gpt-4o prefix match");
        assert!((p2.input_per_1k - 2.50).abs() < 1e-9);
    }

    #[test]
    fn estimate_opus_with_cache() {
        // 1K input + 1K output on opus = $15 + $75 = $90; no cache.
        let usd = estimate_usd("claude-opus-4-6", 1000, 1000, 0, 0);
        assert!((usd - 90.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_usd_includes_cache_terms() {
        // Opus: cache_write $18.75/1K, cache_read $1.875/1K
        let usd = estimate_usd("claude-opus-4-6", 0, 0, 2_000, 10_000);
        let expected = 2.0 * 18.75 + 10.0 * 1.875;
        assert!((usd - expected).abs() < 1e-9);
    }

    #[test]
    fn estimate_usd_for_unknown_model_is_zero() {
        assert_eq!(estimate_usd("nothing-like-this", 10_000, 10_000, 0, 0), 0.0);
    }

    #[test]
    fn estimate_usd_matches_prefix_with_date_suffix() {
        // Sonnet input $3/1K, output $15/1K → 1K+1K = $18
        let usd = estimate_usd("claude-sonnet-4-5-20250929", 1000, 1000, 0, 0);
        assert!((usd - 18.0).abs() < 1e-9);
    }

    #[test]
    fn cost_tracker_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/sessions");
        let tracker = CostTracker::new(&target).unwrap();
        assert!(target.exists());
        assert_eq!(tracker.path(), target.join("cost.jsonl"));
    }

    #[test]
    fn cost_tracker_records_post_llm_requests_as_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = CostTracker::new(dir.path()).unwrap();
        let event = HookEvent::PostLlmRequest {
            tokens_in: 1000,
            tokens_out: 500,
            cache_in: 0,
            cache_read: 0,
            duration_ms: 12,
            model: "claude-opus-4-6".into(),
        };
        // Hook must return Continue.
        matches!(tracker.on_event(&event), HookResult::Continue);
        // And append a valid JSON row.
        let rows = CostTracker::read_all(tracker.path()).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.model, "claude-opus-4-6");
        assert_eq!(row.tokens_in, 1000);
        assert_eq!(row.tokens_out, 500);
        // 1K input + 0.5K output on opus = 15 + 37.5 = 52.5
        assert!((row.estimated_usd - 52.5).abs() < 1e-9);
        // Timestamp is RFC 3339: parsing must succeed.
        chrono::DateTime::parse_from_rfc3339(&row.timestamp).expect("rfc3339");
    }

    #[test]
    fn cost_tracker_ignores_other_events() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = CostTracker::new(dir.path()).unwrap();
        let event = HookEvent::PreLlmRequest { message_count: 1 };
        matches!(tracker.on_event(&event), HookResult::Continue);
        let rows = CostTracker::read_all(tracker.path()).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn read_all_returns_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let rows = CostTracker::read_all(dir.path().join("missing.jsonl")).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn read_all_skips_malformed_and_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cost.jsonl");
        let good = CostRecord {
            timestamp: "2026-04-13T00:00:00Z".into(),
            model: "gpt-4o".into(),
            tokens_in: 1,
            tokens_out: 2,
            cache_in: 0,
            cache_read: 0,
            estimated_usd: 0.01,
        };
        let body = format!(
            "{}\n\nnot-json garbage\n{}\n",
            serde_json::to_string(&good).unwrap(),
            serde_json::to_string(&good).unwrap()
        );
        std::fs::write(&path, body).unwrap();
        let rows = CostTracker::read_all(&path).unwrap();
        assert_eq!(rows.len(), 2);
    }
}
