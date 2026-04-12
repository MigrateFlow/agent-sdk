//! In-memory session cost tracker with USD estimation.
//!
//! [`SessionCostTracker`] accumulates token usage from each LLM call within a
//! REPL session, estimating USD cost using the price table from
//! [`sdk_core::cost`]. It powers the enhanced `/cost` command and the inline
//! cost display shown after each turn.

use std::collections::HashMap;

use console::style;
use sdk_core::cost::{estimate_usd, price_for};

/// Tracks cumulative cost across a CLI session.
pub struct SessionCostTracker {
    entries: Vec<CostEntry>,
}

/// One recorded LLM call.
struct CostEntry {
    model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
}

/// Aggregated per-model stats returned by [`SessionCostTracker::per_model_stats`].
pub struct ModelStats {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
}

impl SessionCostTracker {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record token usage from an LLM call.
    ///
    /// When full [`sdk_core::types::usage::TokenUsage`] is available, use
    /// [`record_usage`](Self::record_usage). Otherwise, use
    /// [`record_simple`](Self::record_simple) with just total tokens and a
    /// model name.
    pub fn record_usage(&mut self, usage: &sdk_core::types::usage::TokenUsage) {
        self.entries.push(CostEntry {
            model: usage.model.clone(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
        });
    }

    /// Record a simple call where only total tokens and model name are known.
    /// Splits tokens 60/40 input/output as a rough estimate.
    pub fn record_simple(&mut self, model: &str, total_tokens: u64) {
        let input = (total_tokens as f64 * 0.6) as u64;
        let output = total_tokens - input;
        self.entries.push(CostEntry {
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });
    }

    /// Total estimated USD cost across all recorded calls.
    pub fn total_cost(&self) -> f64 {
        self.entries
            .iter()
            .map(|e| {
                estimate_usd(
                    &e.model,
                    e.input_tokens,
                    e.output_tokens,
                    e.cache_write_tokens,
                    e.cache_read_tokens,
                )
            })
            .sum()
    }

    /// Total tokens (all types combined) across all recorded calls.
    pub fn total_tokens(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| e.input_tokens + e.output_tokens + e.cache_read_tokens + e.cache_write_tokens)
            .sum()
    }

    /// Estimated cache savings in USD: the difference between what cache-read
    /// tokens would have cost at full input price vs. the discounted cache-read
    /// price.
    pub fn cache_savings(&self) -> f64 {
        self.entries
            .iter()
            .filter_map(|e| {
                let p = price_for(&e.model)?;
                let full_cost = e.cache_read_tokens as f64 * p.input_per_1k / 1000.0;
                let cache_cost = e.cache_read_tokens as f64 * p.cache_read_per_1k / 1000.0;
                Some(full_cost - cache_cost)
            })
            .sum()
    }

    /// Number of recorded LLM calls.
    pub fn call_count(&self) -> usize {
        self.entries.len()
    }

    /// Aggregate stats grouped by model.
    pub fn per_model_stats(&self) -> Vec<ModelStats> {
        let mut map: HashMap<String, ModelStats> = HashMap::new();
        for e in &self.entries {
            let stat = map.entry(e.model.clone()).or_insert_with(|| ModelStats {
                model: e.model.clone(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.0,
            });
            stat.input_tokens += e.input_tokens;
            stat.output_tokens += e.output_tokens;
            stat.cache_read_tokens += e.cache_read_tokens;
            stat.cache_write_tokens += e.cache_write_tokens;
            stat.cost_usd += estimate_usd(
                &e.model,
                e.input_tokens,
                e.output_tokens,
                e.cache_write_tokens,
                e.cache_read_tokens,
            );
        }
        let mut stats: Vec<_> = map.into_values().collect();
        stats.sort_by(|a, b| b.cost_usd.partial_cmp(&a.cost_usd).unwrap_or(std::cmp::Ordering::Equal));
        stats
    }

    /// Format a detailed cost breakdown string for the `/cost` command.
    pub fn format_breakdown(&self, session_turns: usize, session_duration: Option<std::time::Duration>) -> Vec<String> {
        let mut lines = Vec::new();

        let total = self.total_cost();
        let total_tok = self.total_tokens();
        lines.push(format!(
            "{} {} ({})",
            style("Total:").bold(),
            style(format_cost(total)).white().bold(),
            style(format_token_count_with_commas(total_tok)).dim(),
        ));

        let models = self.per_model_stats();
        for m in &models {
            lines.push(String::new());
            lines.push(format!("{}:", style(&m.model).cyan()));

            lines.push(format!(
                "  {} {:>9}  {}",
                style("Input:").dim(),
                format_token_count_with_commas(m.input_tokens),
                style(format_cost(estimate_usd(&m.model, m.input_tokens, 0, 0, 0))).dim(),
            ));
            lines.push(format!(
                "  {} {:>8}  {}",
                style("Output:").dim(),
                format_token_count_with_commas(m.output_tokens),
                style(format_cost(estimate_usd(&m.model, 0, m.output_tokens, 0, 0))).dim(),
            ));

            if m.cache_read_tokens > 0 {
                lines.push(format!(
                    "  {} {:>4}  {}",
                    style("Cache read:").dim(),
                    format_token_count_with_commas(m.cache_read_tokens),
                    style(format_cost(estimate_usd(&m.model, 0, 0, 0, m.cache_read_tokens))).dim(),
                ));
            }
            if m.cache_write_tokens > 0 {
                lines.push(format!(
                    "  {} {:>3}  {}",
                    style("Cache write:").dim(),
                    format_token_count_with_commas(m.cache_write_tokens),
                    style(format_cost(estimate_usd(&m.model, 0, 0, m.cache_write_tokens, 0))).dim(),
                ));
            }

            // Per-model cache savings
            if m.cache_read_tokens > 0 {
                if let Some(p) = price_for(&m.model) {
                    let savings = m.cache_read_tokens as f64 * (p.input_per_1k - p.cache_read_per_1k) / 1000.0;
                    if savings > 0.0 {
                        lines.push(format!(
                            "  {} ~{}",
                            style("Cache savings:").dim(),
                            style(format_cost(savings)).green(),
                        ));
                    }
                }
            }
        }

        // Session summary line
        lines.push(String::new());
        let mut session_parts = vec![format!("{} turns", session_turns)];
        if let Some(dur) = session_duration {
            session_parts.push(crate::format::format_duration(dur));
        }
        let savings = self.cache_savings();
        if savings > 0.001 {
            session_parts.push(format!("~{} saved from cache", format_cost(savings)));
        }
        lines.push(format!(
            "{} {}",
            style("Session:").dim(),
            style(session_parts.join(" \u{00b7} ")).dim(),
        ));

        lines
    }

    /// Format a short inline cost string for status display (e.g. after each turn).
    pub fn format_inline(&self) -> String {
        let cost = self.total_cost();
        if cost > 0.0001 {
            format_cost(cost)
        } else {
            String::new()
        }
    }

    /// Reset all tracked entries (called on /clear).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for SessionCostTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Format an inline cost suffix for appending to status lines.
/// Returns an empty string when there is no meaningful cost to show,
/// or " · $X.XX" (with styled green text) otherwise.
pub fn format_cost_suffix(tracker: &SessionCostTracker) -> String {
    let s = tracker.format_inline();
    if s.is_empty() {
        String::new()
    } else {
        format!(" \u{00b7} {}", console::style(&s).green())
    }
}

/// Format a USD cost value with adaptive precision.
pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        format!("${:.4}", usd)
    } else if usd < 1.0 {
        format!("${:.3}", usd)
    } else {
        format!("${:.2}", usd)
    }
}

/// Format a token count with comma separators for readability.
fn format_token_count_with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdk_core::types::usage::TokenUsage;

    #[test]
    fn new_tracker_is_empty() {
        let t = SessionCostTracker::new();
        assert_eq!(t.total_tokens(), 0);
        assert_eq!(t.total_cost(), 0.0);
        assert_eq!(t.call_count(), 0);
        assert_eq!(t.cache_savings(), 0.0);
    }

    #[test]
    fn record_usage_accumulates() {
        let mut t = SessionCostTracker::new();
        t.record_usage(&TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            model: "claude-sonnet-4-5-20250514".to_string(),
        });
        assert_eq!(t.call_count(), 1);
        assert_eq!(t.total_tokens(), 1500);
        // Sonnet: 1K input * $3/1K + 0.5K output * $15/1K = $3 + $7.5 = $10.5
        assert!((t.total_cost() - 10.5).abs() < 1e-6);
    }

    #[test]
    fn record_simple_splits_tokens() {
        let mut t = SessionCostTracker::new();
        t.record_simple("claude-sonnet-4-5", 1000);
        assert_eq!(t.total_tokens(), 1000);
        assert_eq!(t.call_count(), 1);
        // 600 input * $3/1K + 400 output * $15/1K = $1.8 + $6.0 = $7.8
        assert!((t.total_cost() - 7.8).abs() < 1e-6);
    }

    #[test]
    fn cache_savings_computed() {
        let mut t = SessionCostTracker::new();
        t.record_usage(&TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 10_000,
            model: "claude-sonnet-4-5".to_string(),
        });
        // Sonnet: full input = $3/1K, cache_read = $0.30/1K
        // Savings = 10K * ($3 - $0.30) / 1K = $27.0
        assert!((t.cache_savings() - 27.0).abs() < 1e-6);
    }

    #[test]
    fn unknown_model_zero_cost() {
        let mut t = SessionCostTracker::new();
        t.record_simple("unknown-model", 5000);
        assert_eq!(t.total_tokens(), 5000);
        assert_eq!(t.total_cost(), 0.0);
    }

    #[test]
    fn per_model_stats_groups() {
        let mut t = SessionCostTracker::new();
        t.record_simple("claude-sonnet-4-5", 1000);
        t.record_simple("claude-sonnet-4-5", 2000);
        t.record_simple("claude-opus-4-6", 5000);
        let stats = t.per_model_stats();
        assert_eq!(stats.len(), 2);
        // Opus has higher cost per token, so with 5K tokens it should come first
        assert!(stats[0].model.starts_with("claude-opus"));
    }

    #[test]
    fn clear_resets_tracker() {
        let mut t = SessionCostTracker::new();
        t.record_simple("claude-sonnet-4-5", 1000);
        assert!(t.call_count() > 0);
        t.clear();
        assert_eq!(t.call_count(), 0);
        assert_eq!(t.total_tokens(), 0);
        assert_eq!(t.total_cost(), 0.0);
    }

    #[test]
    fn format_cost_adaptive_precision() {
        assert_eq!(format_cost(0.001), "$0.0010");
        assert_eq!(format_cost(0.05), "$0.050");
        assert_eq!(format_cost(1.5), "$1.50");
        assert_eq!(format_cost(42.123), "$42.12");
    }

    #[test]
    fn format_token_count_commas() {
        assert_eq!(format_token_count_with_commas(0), "0");
        assert_eq!(format_token_count_with_commas(999), "999");
        assert_eq!(format_token_count_with_commas(1_000), "1,000");
        assert_eq!(format_token_count_with_commas(12_345), "12,345");
        assert_eq!(format_token_count_with_commas(1_234_567), "1,234,567");
    }

    #[test]
    fn format_inline_empty_for_zero() {
        let t = SessionCostTracker::new();
        assert!(t.format_inline().is_empty());
    }

    #[test]
    fn format_inline_shows_cost() {
        let mut t = SessionCostTracker::new();
        t.record_simple("claude-sonnet-4-5", 10_000);
        let inline = t.format_inline();
        assert!(inline.starts_with('$'));
    }
}
