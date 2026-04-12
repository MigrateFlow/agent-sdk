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
