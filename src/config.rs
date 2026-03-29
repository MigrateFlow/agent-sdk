use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    #[default]
    Claude,
    OpenAi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub provider: LlmProvider,
    pub model: String,
    pub max_tokens: usize,
    pub requests_per_minute: u32,
    pub tokens_per_minute: u32,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_base_url: Option<String>,
}

impl LlmConfig {
    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(ref key) = self.api_key {
            if !key.is_empty() {
                return Some(key.clone());
            }
        }
        let env_var = match self.provider {
            LlmProvider::Claude => "ANTHROPIC_API_KEY",
            LlmProvider::OpenAi => "OPENAI_API_KEY",
        };
        std::env::var(env_var).ok()
    }

    pub fn resolve_base_url(&self) -> String {
        if let Some(ref url) = self.api_base_url {
            if !url.is_empty() {
                return url.clone();
            }
        }
        let env_var = match self.provider {
            LlmProvider::Claude => "ANTHROPIC_API_BASE_URL",
            LlmProvider::OpenAi => "OPENAI_API_BASE_URL",
        };
        if let Ok(url) = std::env::var(env_var) {
            if !url.is_empty() {
                return url;
            }
        }
        match self.provider {
            LlmProvider::Claude => "https://api.anthropic.com".to_string(),
            LlmProvider::OpenAi => "https://api.openai.com".to_string(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::Claude,
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            requests_per_minute: 50,
            tokens_per_minute: 80_000,
            api_key: None,
            api_base_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub max_parallel_agents: usize,
    pub poll_interval_ms: u64,
    pub max_task_retries: u32,
    #[serde(default = "default_max_loop_iterations")]
    pub max_loop_iterations: usize,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
}

fn default_max_loop_iterations() -> usize {
    50
}

fn default_max_context_tokens() -> usize {
    100_000
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_parallel_agents: 4,
            poll_interval_ms: 200,
            max_task_retries: 3,
            max_loop_iterations: 50,
            max_context_tokens: 100_000,
        }
    }
}