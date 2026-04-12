use serde::{Deserialize, Serialize};

use sdk_core::types::usage::TokenUsage;

/// Controls which parts of the API request get cache_control breakpoints.
#[derive(Debug, Clone)]
pub struct CachePolicy {
    pub cache_system_prompt: bool,
    pub cache_tools: bool,
    pub cache_conversation_prefix: bool,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            cache_system_prompt: true,
            cache_tools: true,
            cache_conversation_prefix: true,
        }
    }
}

/// Accumulated cache hit/miss metrics across turns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheMetrics {
    pub total_cache_writes: u64,
    pub total_cache_reads: u64,
    pub total_requests: u64,
    pub estimated_tokens_saved: u64,
}

impl CacheMetrics {
    pub fn cache_hit_rate(&self) -> f64 {
        if self.total_requests == 0 {
            return 0.0;
        }
        let reads = self.total_cache_reads as f64;
        let writes = self.total_cache_writes as f64;
        if reads + writes == 0.0 {
            0.0
        } else {
            reads / (reads + writes)
        }
    }

    pub fn update(&mut self, usage: &TokenUsage) {
        self.total_requests += 1;
        self.total_cache_writes += usage.cache_creation_input_tokens;
        self.total_cache_reads += usage.cache_read_input_tokens;
        // Cache reads save tokens vs. re-processing
        self.estimated_tokens_saved += usage.cache_read_input_tokens;
    }
}

pub fn format_cache_stats(metrics: &CacheMetrics) -> String {
    format!(
        "Cache: {:.1}% hit rate | {} reads, {} writes | ~{} tokens saved | {} requests",
        metrics.cache_hit_rate() * 100.0,
        metrics.total_cache_reads,
        metrics.total_cache_writes,
        metrics.estimated_tokens_saved,
        metrics.total_requests,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_cache_policy() {
        let policy = CachePolicy::default();
        assert!(policy.cache_system_prompt);
        assert!(policy.cache_tools);
        assert!(policy.cache_conversation_prefix);
    }

    #[test]
    fn test_cache_metrics_default() {
        let metrics = CacheMetrics::default();
        assert_eq!(metrics.total_requests, 0);
        assert_eq!(metrics.cache_hit_rate(), 0.0);
    }

    #[test]
    fn test_cache_metrics_update() {
        let mut metrics = CacheMetrics::default();
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 80,
            cache_read_input_tokens: 0,
            model: "test".to_string(),
        };
        metrics.update(&usage);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.total_cache_writes, 80);
        assert_eq!(metrics.total_cache_reads, 0);
        assert_eq!(metrics.cache_hit_rate(), 0.0);

        // Second request with cache hit
        let usage2 = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 80,
            model: "test".to_string(),
        };
        metrics.update(&usage2);
        assert_eq!(metrics.total_requests, 2);
        assert_eq!(metrics.total_cache_reads, 80);
        assert_eq!(metrics.estimated_tokens_saved, 80);
        assert!((metrics.cache_hit_rate() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_format_cache_stats() {
        let metrics = CacheMetrics {
            total_cache_writes: 100,
            total_cache_reads: 300,
            total_requests: 4,
            estimated_tokens_saved: 300,
        };
        let s = format_cache_stats(&metrics);
        assert!(s.contains("75.0% hit rate"));
        assert!(s.contains("300 reads"));
        assert!(s.contains("100 writes"));
    }
}
