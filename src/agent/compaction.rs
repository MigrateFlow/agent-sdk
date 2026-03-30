use std::collections::HashMap;

use crate::types::chat::ChatMessage;

/// Metrics collected during compaction operations
#[derive(Debug, Clone, Default)]
pub struct CompactionMetrics {
    pub total_compactions: u64,
    pub total_messages_before: u64,
    pub total_messages_after: u64,
    pub total_chars_saved: u64,
    pub total_time_spent: std::time::Duration,
    pub strategy_usage: HashMap<String, u64>,
}

impl CompactionMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_compaction(
        &mut self,
        messages_before: usize,
        messages_after: usize,
        chars_before: usize,
        chars_after: usize,
        time_spent: std::time::Duration,
        strategy: &str,
    ) {
        self.total_compactions += 1;
        self.total_messages_before += messages_before as u64;
        self.total_messages_after += messages_after as u64;
        self.total_chars_saved += (chars_before - chars_after) as u64;
        self.total_time_spent += time_spent;
        
        *self.strategy_usage.entry(strategy.to_string()).or_insert(0) += 1;
    }

    pub fn avg_messages_before(&self) -> f64 {
        if self.total_compactions == 0 {
            0.0
        } else {
            self.total_messages_before as f64 / self.total_compactions as f64
        }
    }

    pub fn avg_messages_after(&self) -> f64 {
        if self.total_compactions == 0 {
            0.0
        } else {
            self.total_messages_after as f64 / self.total_compactions as f64
        }
    }

    pub fn avg_chars_saved(&self) -> f64 {
        if self.total_compactions == 0 {
            0.0
        } else {
            self.total_chars_saved as f64 / self.total_compactions as f64
        }
    }

    pub fn avg_time_per_compaction(&self) -> std::time::Duration {
        if self.total_compactions == 0 {
            std::time::Duration::from_nanos(0)
        } else {
            std::time::Duration::from_nanos((self.total_time_spent.as_nanos() / self.total_compactions as u128) as u64)
        }
    }
}

/// A trait for defining custom compaction rules
pub trait CompactionRule: Send + Sync {
    /// Determine if compaction should be performed on the given messages
    fn should_compact(&self, messages: &[ChatMessage]) -> bool;
    
    /// Select which message indices should be targeted for compaction
    fn select_targets(&self, messages: &[ChatMessage]) -> Vec<usize>;
    
    /// Apply the compaction to the selected messages
    fn apply_compaction(&self, messages: &mut [ChatMessage], targets: &[usize]);
}

/// A basic compaction rule that compresses large tool results and assistant messages
pub struct BasicCompactionRule {
    pub tool_result_limit: usize,
    pub assistant_content_limit: usize,
    pub keep_recent: usize,
}

impl Default for BasicCompactionRule {
    fn default() -> Self {
        Self {
            tool_result_limit: 200,
            assistant_content_limit: 500,
            keep_recent: 10,
        }
    }
}

impl CompactionRule for BasicCompactionRule {
    fn should_compact(&self, messages: &[ChatMessage]) -> bool {
        // Should compact if we have more messages than our keep_recent threshold
        messages.len() > self.keep_recent
    }

    fn select_targets(&self, messages: &[ChatMessage]) -> Vec<usize> {
        let total = messages.len();
        if total <= self.keep_recent {
            return vec![];
        }

        let mut targets = Vec::new();
        // Don't compact the most recent messages
        let keep_after = total - self.keep_recent;

        for i in 1..keep_after {
            match &messages[i] {
                ChatMessage::Tool { content, .. } => {
                    if content.len() > self.tool_result_limit {
                        targets.push(i);
                    }
                }
                ChatMessage::Assistant { content, .. } => {
                    if content.as_ref().is_some_and(|c| c.len() > self.assistant_content_limit) {
                        targets.push(i);
                    }
                }
                _ => {}
            }
        }

        targets
    }

    fn apply_compaction(&self, messages: &mut [ChatMessage], targets: &[usize]) {
        for &index in targets {
            if index >= messages.len() {
                continue;
            }

            match &mut messages[index] {
                ChatMessage::Tool {
                    tool_call_id: _,
                    content,
                } => {
                    if content.len() > self.tool_result_limit {
                        let summary = format!(
                            "[compacted: {} chars] {}",
                            content.len(),
                            safe_prefix(content, self.tool_result_limit.saturating_sub(50))
                        );
                        *content = summary;
                    }
                }
                ChatMessage::Assistant {
                    content,
                    tool_calls: _,
                } => {
                    if content.as_ref().is_some_and(|c| c.len() > self.assistant_content_limit) {
                        *content = Some(truncate(
                            content.as_ref().unwrap(),
                            self.assistant_content_limit.saturating_sub(100),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Helper function to safely truncate strings without breaking UTF-8 boundaries
pub fn safe_prefix(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }

    match s.char_indices().map(|(idx, _)| idx).take_while(|&idx| idx <= max_len).last() {
        Some(0) | None => "",
        Some(idx) => &s[..idx],
    }
}

/// Helper function to truncate a string with ellipsis
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", safe_prefix(s, max_len.saturating_sub(3)))
    }
}

/// Helper function to calculate the estimated token count for a message
pub fn estimate_token_count(message: &ChatMessage) -> usize {
    const CHARS_PER_TOKEN: usize = 4;
    message.char_len() / CHARS_PER_TOKEN
}

/// Helper function to calculate the estimated token count for a set of messages
pub fn estimate_total_token_count(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_token_count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_prefix() {
        let s = "Hello, world!";
        assert_eq!(safe_prefix(s, 5), "Hello");
        assert_eq!(safe_prefix(s, 13), s); // Length of "Hello, world!" is 13
        assert_eq!(safe_prefix(s, 0), "");
    }

    #[test]
    fn test_truncate() {
        let s = "This is a long string";
        assert_eq!(truncate(s, 10), "This is...");
        assert_eq!(truncate(s, 100), s);
    }

    #[test]
    fn test_basic_compaction_rule() {
        let rule = BasicCompactionRule {
            tool_result_limit: 50,
            assistant_content_limit: 100,
            keep_recent: 1, // Keep only 1 recent message
        };

        let mut messages = vec![
            ChatMessage::system("system".to_string()),
            ChatMessage::assistant("This is a short assistant message".to_string()),
            ChatMessage::tool_result("call1", &"x".repeat(60)), // This should be compacted
            ChatMessage::assistant(&"x".repeat(120)), // This should be kept (most recent)
        ];

        assert!(rule.should_compact(&messages));
        let targets = rule.select_targets(&messages);
        // With 4 messages and keep_recent=1, we keep the last message (index 3) and consider indices 0,1,2
        // Index 0 is system, so not targeted
        // Index 1 is assistant but under limit, so not targeted
        // Index 2 is tool result over limit, so targeted
        assert_eq!(targets, vec![2]);
        
        rule.apply_compaction(&mut messages, &targets);
        
        // Check that the large tool result was compressed
        if let ChatMessage::Tool { content, .. } = &messages[2] {
            assert!(content.starts_with("[compacted:"));
        } else {
            panic!("Expected tool message at index 2");
        }
    }

    #[test]
    fn test_metrics() {
        let mut metrics = CompactionMetrics::new();
        metrics.record_compaction(20, 10, 5000, 3000, std::time::Duration::from_millis(100), "default");
        metrics.record_compaction(15, 8, 4000, 2500, std::time::Duration::from_millis(80), "conservative");

        assert_eq!(metrics.total_compactions, 2);
        assert_eq!(metrics.avg_chars_saved(), 1750.0); // (2000 + 1500) / 2
        assert_eq!(metrics.avg_time_per_compaction(), std::time::Duration::from_millis(90));
    }
}