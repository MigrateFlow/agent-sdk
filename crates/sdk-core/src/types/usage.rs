use serde::{Deserialize, Serialize};

/// Detailed token usage for a single LLM response.
///
/// Fields cover standard input/output tokens plus Anthropic's prompt-caching
/// fields. For providers/models that do not expose cache metrics, the cache
/// fields are reported as `0`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub model: String,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero() {
        let u = TokenUsage::default();
        assert_eq!(u.total(), 0);
        assert_eq!(u.cache_creation_input_tokens, 0);
        assert_eq!(u.cache_read_input_tokens, 0);
        assert!(u.model.is_empty());
    }

    #[test]
    fn total_sums_input_and_output() {
        let u = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        assert_eq!(u.total(), 15);
    }

    #[test]
    fn serde_defaults_missing_cache_fields() {
        let u: TokenUsage = serde_json::from_value(serde_json::json!({
            "input_tokens": 1,
            "output_tokens": 2
        }))
        .unwrap();
        assert_eq!(u.input_tokens, 1);
        assert_eq!(u.cache_creation_input_tokens, 0);
        assert_eq!(u.cache_read_input_tokens, 0);
        assert_eq!(u.model, "");
    }
}
