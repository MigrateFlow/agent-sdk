use serde::{Deserialize, Serialize};

/// Well-known project-local config directory name, analogous to `.claude/`.
/// Mutable runtime state is stored separately under `~/.agent/projects/...`.
pub const AGENT_DIR: &str = ".agent";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    Claude,
    #[default]
    OpenAi,
}

impl LlmProvider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" | "anthropic" => Some(Self::Claude),
            "openai" | "open_ai" => Some(Self::OpenAi),
            _ => None,
        }
    }

    pub fn detect() -> Self {
        if let Ok(raw) = std::env::var("LLM_PROVIDER") {
            if let Some(provider) = Self::parse(&raw) {
                return provider;
            }
        }

        let has_openai = std::env::var("OPENAI_API_KEY")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        let has_anthropic = std::env::var("ANTHROPIC_API_KEY")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);

        match (has_openai, has_anthropic) {
            (true, false) => Self::OpenAi,
            (false, true) => Self::Claude,
            _ => Self::default(),
        }
    }

    pub fn api_key_env_var(&self) -> &'static str {
        match self {
            LlmProvider::Claude => "ANTHROPIC_API_KEY",
            LlmProvider::OpenAi => "OPENAI_API_KEY",
        }
    }

    pub fn base_url_env_var(&self) -> &'static str {
        match self {
            LlmProvider::Claude => "ANTHROPIC_API_BASE_URL",
            LlmProvider::OpenAi => "OPENAI_API_BASE_URL",
        }
    }

    pub fn model_env_var(&self) -> &'static str {
        match self {
            LlmProvider::Claude => "ANTHROPIC_MODEL",
            LlmProvider::OpenAi => "OPENAI_MODEL",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            LlmProvider::Claude => "https://api.anthropic.com",
            LlmProvider::OpenAi => "https://api.openai.com",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            LlmProvider::Claude => "claude-sonnet-4-20250514",
            LlmProvider::OpenAi => "gpt-4o",
        }
    }
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

    /// HTTP request timeout in seconds (applies to each LLM API call).
    #[serde(default = "default_http_timeout_secs")]
    pub http_timeout_secs: u64,
    /// Maximum number of automatic retries on transient errors (429, 5xx).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Base delay in milliseconds for exponential back-off on retries.
    #[serde(default = "default_retry_base_delay_ms")]
    pub retry_base_delay_ms: u64,
}

fn default_http_timeout_secs() -> u64 {
    300
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_base_delay_ms() -> u64 {
    1000
}

impl LlmConfig {
    pub fn resolved_provider(&self) -> LlmProvider {
        self.provider.clone()
    }

    pub fn resolve_model(&self) -> String {
        if !self.model.trim().is_empty() {
            return self.model.clone();
        }

        let provider = self.resolved_provider();
        std::env::var(provider.model_env_var())
            .or_else(|_| std::env::var("LLM_MODEL"))
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| provider.default_model().to_string())
    }

    pub fn resolve_api_key(&self) -> Option<String> {
        if let Some(ref key) = self.api_key {
            if !key.is_empty() {
                return Some(key.clone());
            }
        }
        std::env::var(self.resolved_provider().api_key_env_var())
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    pub fn resolve_base_url(&self) -> String {
        if let Some(ref url) = self.api_base_url {
            if !url.is_empty() {
                return url.clone();
            }
        }
        let provider = self.resolved_provider();
        if let Ok(url) = std::env::var(provider.base_url_env_var()) {
            if !url.is_empty() {
                return url;
            }
        }
        provider.default_base_url().to_string()
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        let provider = LlmProvider::detect();
        Self {
            provider: provider.clone(),
            model: std::env::var(provider.model_env_var())
                .or_else(|_| std::env::var("LLM_MODEL"))
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| provider.default_model().to_string()),
            max_tokens: 4096,
            requests_per_minute: 50,
            tokens_per_minute: 80_000,
            api_key: None,
            api_base_url: None,
            http_timeout_secs: default_http_timeout_secs(),
            max_retries: default_max_retries(),
            retry_base_delay_ms: default_retry_base_delay_ms(),
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
    /// How many consecutive idle polling cycles before a teammate exits.
    #[serde(default = "default_max_idle_cycles")]
    pub max_idle_cycles: u32,
    /// Seconds a teammate will wait for plan approval before proceeding.
    #[serde(default = "default_plan_approval_timeout_secs")]
    pub plan_approval_timeout_secs: u64,
    /// Default timeout in seconds for `run_command` tool invocations.
    #[serde(default = "default_command_timeout_secs")]
    pub command_timeout_secs: u64,
}

fn default_max_loop_iterations() -> usize {
    50
}

fn default_max_context_tokens() -> usize {
    200_000
}

fn default_max_idle_cycles() -> u32 {
    50
}

fn default_plan_approval_timeout_secs() -> u64 {
    300
}

fn default_command_timeout_secs() -> u64 {
    30
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_parallel_agents: 4,
            poll_interval_ms: 200,
            max_task_retries: 3,
            max_loop_iterations: default_max_loop_iterations(),
            max_context_tokens: default_max_context_tokens(),
            max_idle_cycles: default_max_idle_cycles(),
            plan_approval_timeout_secs: default_plan_approval_timeout_secs(),
            command_timeout_secs: default_command_timeout_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize env-dependent tests. Provider detection / resolution read process-wide
    // env vars which would race if tests ran concurrently on the same threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Scoped env-var guard that snapshots the previous value and restores it on drop.
    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn clear_all_provider_envs() -> Vec<EnvGuard> {
        vec![
            EnvGuard::unset("LLM_PROVIDER"),
            EnvGuard::unset("LLM_MODEL"),
            EnvGuard::unset("OPENAI_API_KEY"),
            EnvGuard::unset("ANTHROPIC_API_KEY"),
            EnvGuard::unset("OPENAI_API_BASE_URL"),
            EnvGuard::unset("ANTHROPIC_API_BASE_URL"),
            EnvGuard::unset("OPENAI_MODEL"),
            EnvGuard::unset("ANTHROPIC_MODEL"),
        ]
    }

    #[test]
    fn agent_dir_constant() {
        assert_eq!(AGENT_DIR, ".agent");
    }

    #[test]
    fn provider_default_is_openai() {
        assert_eq!(LlmProvider::default(), LlmProvider::OpenAi);
    }

    #[test]
    fn provider_parse_claude_variants() {
        assert_eq!(LlmProvider::parse("claude"), Some(LlmProvider::Claude));
        assert_eq!(LlmProvider::parse("Claude"), Some(LlmProvider::Claude));
        assert_eq!(LlmProvider::parse("ANTHROPIC"), Some(LlmProvider::Claude));
        assert_eq!(
            LlmProvider::parse("  anthropic  "),
            Some(LlmProvider::Claude)
        );
    }

    #[test]
    fn provider_parse_openai_variants() {
        assert_eq!(LlmProvider::parse("openai"), Some(LlmProvider::OpenAi));
        assert_eq!(LlmProvider::parse("OpenAI"), Some(LlmProvider::OpenAi));
        assert_eq!(LlmProvider::parse("open_ai"), Some(LlmProvider::OpenAi));
    }

    #[test]
    fn provider_parse_unknown_returns_none() {
        assert_eq!(LlmProvider::parse("gemini"), None);
        assert_eq!(LlmProvider::parse(""), None);
    }

    #[test]
    fn provider_env_accessors_cover_all_variants() {
        for provider in [LlmProvider::Claude, LlmProvider::OpenAi] {
            // Each accessor returns a non-empty constant and varies per provider.
            assert!(!provider.api_key_env_var().is_empty());
            assert!(!provider.base_url_env_var().is_empty());
            assert!(!provider.model_env_var().is_empty());
            assert!(!provider.default_base_url().is_empty());
            assert!(!provider.default_model().is_empty());
        }

        assert_eq!(LlmProvider::Claude.api_key_env_var(), "ANTHROPIC_API_KEY");
        assert_eq!(LlmProvider::OpenAi.api_key_env_var(), "OPENAI_API_KEY");
        assert_eq!(
            LlmProvider::Claude.base_url_env_var(),
            "ANTHROPIC_API_BASE_URL"
        );
        assert_eq!(
            LlmProvider::OpenAi.base_url_env_var(),
            "OPENAI_API_BASE_URL"
        );
        assert_eq!(LlmProvider::Claude.model_env_var(), "ANTHROPIC_MODEL");
        assert_eq!(LlmProvider::OpenAi.model_env_var(), "OPENAI_MODEL");
        assert_eq!(
            LlmProvider::Claude.default_base_url(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            LlmProvider::OpenAi.default_base_url(),
            "https://api.openai.com"
        );
        assert_eq!(
            LlmProvider::Claude.default_model(),
            "claude-sonnet-4-20250514"
        );
        assert_eq!(LlmProvider::OpenAi.default_model(), "gpt-4o");
    }

    #[test]
    fn provider_detect_honors_explicit_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _override = EnvGuard::set("LLM_PROVIDER", "claude");
        assert_eq!(LlmProvider::detect(), LlmProvider::Claude);
    }

    #[test]
    fn provider_detect_invalid_env_falls_through_to_keys() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _bogus = EnvGuard::set("LLM_PROVIDER", "not-a-provider");
        let _anthropic = EnvGuard::set("ANTHROPIC_API_KEY", "sk-real");
        assert_eq!(LlmProvider::detect(), LlmProvider::Claude);
    }

    #[test]
    fn provider_detect_picks_openai_when_only_openai_key_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _openai = EnvGuard::set("OPENAI_API_KEY", "sk-openai");
        assert_eq!(LlmProvider::detect(), LlmProvider::OpenAi);
    }

    #[test]
    fn provider_detect_blank_key_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _blank = EnvGuard::set("OPENAI_API_KEY", "   ");
        // Both keys effectively absent -> default (OpenAi).
        assert_eq!(LlmProvider::detect(), LlmProvider::OpenAi);
    }

    #[test]
    fn provider_detect_defaults_when_both_keys_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _a = EnvGuard::set("OPENAI_API_KEY", "sk-openai");
        let _b = EnvGuard::set("ANTHROPIC_API_KEY", "sk-anthropic");
        assert_eq!(LlmProvider::detect(), LlmProvider::default());
    }

    #[test]
    fn llm_config_default_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let cfg = LlmConfig::default();
        assert_eq!(cfg.provider, LlmProvider::OpenAi);
        assert_eq!(cfg.model, LlmProvider::OpenAi.default_model());
        assert_eq!(cfg.max_tokens, 4096);
        assert_eq!(cfg.requests_per_minute, 50);
        assert_eq!(cfg.tokens_per_minute, 80_000);
        assert!(cfg.api_key.is_none());
        assert!(cfg.api_base_url.is_none());
        assert_eq!(cfg.http_timeout_secs, 300);
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.retry_base_delay_ms, 1000);
    }

    #[test]
    fn llm_config_default_respects_llm_model_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _model = EnvGuard::set("LLM_MODEL", "override-model");
        let cfg = LlmConfig::default();
        assert_eq!(cfg.model, "override-model");
    }

    #[test]
    fn llm_config_default_respects_provider_specific_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _provider = EnvGuard::set("LLM_PROVIDER", "openai");
        let _model = EnvGuard::set("OPENAI_MODEL", "gpt-from-env");
        let cfg = LlmConfig::default();
        assert_eq!(cfg.provider, LlmProvider::OpenAi);
        assert_eq!(cfg.model, "gpt-from-env");
    }

    #[test]
    fn resolved_provider_returns_clone() {
        let cfg = LlmConfig {
            provider: LlmProvider::Claude,
            model: "m".into(),
            max_tokens: 1,
            requests_per_minute: 1,
            tokens_per_minute: 1,
            api_key: None,
            api_base_url: None,
            http_timeout_secs: 1,
            max_retries: 0,
            retry_base_delay_ms: 0,
        };
        assert_eq!(cfg.resolved_provider(), LlmProvider::Claude);
    }

    #[test]
    fn resolve_model_prefers_configured_value() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _env = EnvGuard::set("OPENAI_MODEL", "should-be-ignored");
        let cfg = LlmConfig {
            model: "configured".into(),
            ..LlmConfig::default()
        };
        assert_eq!(cfg.resolve_model(), "configured");
    }

    #[test]
    fn resolve_model_falls_back_to_provider_env_then_llm_model() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        // Empty-string model triggers the fallback path.
        let cfg = LlmConfig {
            provider: LlmProvider::Claude,
            model: "   ".into(),
            ..LlmConfig::default()
        };

        // No env -> default model for provider.
        assert_eq!(cfg.resolve_model(), LlmProvider::Claude.default_model());

        // ANTHROPIC_MODEL takes precedence over LLM_MODEL.
        let _anthropic = EnvGuard::set("ANTHROPIC_MODEL", "anthro-model");
        let _llm = EnvGuard::set("LLM_MODEL", "llm-model");
        assert_eq!(cfg.resolve_model(), "anthro-model");
        drop(_anthropic);

        // Only LLM_MODEL present.
        assert_eq!(cfg.resolve_model(), "llm-model");
    }

    #[test]
    fn resolve_model_ignores_blank_envs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();
        let _blank_specific = EnvGuard::set("OPENAI_MODEL", "");
        let _blank_generic = EnvGuard::set("LLM_MODEL", "   ");
        let cfg = LlmConfig {
            provider: LlmProvider::OpenAi,
            model: "".into(),
            ..LlmConfig::default()
        };
        assert_eq!(cfg.resolve_model(), LlmProvider::OpenAi.default_model());
    }

    #[test]
    fn resolve_api_key_prefers_config_then_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();

        let cfg_with_key = LlmConfig {
            api_key: Some("cfg-key".into()),
            ..LlmConfig::default()
        };
        assert_eq!(cfg_with_key.resolve_api_key().as_deref(), Some("cfg-key"));

        // Empty string in api_key should fall through to env.
        let cfg_empty = LlmConfig {
            provider: LlmProvider::OpenAi,
            api_key: Some("".into()),
            ..LlmConfig::default()
        };
        let _env = EnvGuard::set("OPENAI_API_KEY", "env-key");
        assert_eq!(cfg_empty.resolve_api_key().as_deref(), Some("env-key"));

        // No config key, no env -> None.
        drop(_env);
        let cfg_none = LlmConfig {
            provider: LlmProvider::OpenAi,
            api_key: None,
            ..LlmConfig::default()
        };
        assert!(cfg_none.resolve_api_key().is_none());

        // Blank env value treated as absent.
        let _blank = EnvGuard::set("OPENAI_API_KEY", "   ");
        assert!(cfg_none.resolve_api_key().is_none());
    }

    #[test]
    fn resolve_base_url_prefers_config_then_env_then_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _cleanup = clear_all_provider_envs();

        let cfg_with_url = LlmConfig {
            provider: LlmProvider::Claude,
            api_base_url: Some("https://proxy.example".into()),
            ..LlmConfig::default()
        };
        assert_eq!(cfg_with_url.resolve_base_url(), "https://proxy.example");

        // Empty api_base_url should fall through to env.
        let cfg_empty = LlmConfig {
            provider: LlmProvider::Claude,
            api_base_url: Some("".into()),
            ..LlmConfig::default()
        };
        let _env = EnvGuard::set("ANTHROPIC_API_BASE_URL", "https://env.example");
        assert_eq!(cfg_empty.resolve_base_url(), "https://env.example");

        drop(_env);

        // No config, no env -> provider default.
        let cfg_none = LlmConfig {
            provider: LlmProvider::Claude,
            api_base_url: None,
            ..LlmConfig::default()
        };
        assert_eq!(
            cfg_none.resolve_base_url(),
            LlmProvider::Claude.default_base_url()
        );
    }

    #[test]
    fn llm_provider_serde_roundtrip() {
        let json = serde_json::to_string(&LlmProvider::Claude).unwrap();
        assert_eq!(json, "\"claude\"");
        let parsed: LlmProvider = serde_json::from_str("\"open_ai\"").unwrap();
        assert_eq!(parsed, LlmProvider::OpenAi);
    }

    #[test]
    fn llm_config_deserialize_applies_defaults() {
        let json = r#"{
            "model": "m",
            "max_tokens": 10,
            "requests_per_minute": 1,
            "tokens_per_minute": 2
        }"#;
        let cfg: LlmConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.provider, LlmProvider::OpenAi);
        assert_eq!(cfg.http_timeout_secs, default_http_timeout_secs());
        assert_eq!(cfg.max_retries, default_max_retries());
        assert_eq!(cfg.retry_base_delay_ms, default_retry_base_delay_ms());
        assert!(cfg.api_key.is_none());
        assert!(cfg.api_base_url.is_none());
    }

    #[test]
    fn agent_config_default_values() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.max_parallel_agents, 4);
        assert_eq!(cfg.poll_interval_ms, 200);
        assert_eq!(cfg.max_task_retries, 3);
        assert_eq!(cfg.max_loop_iterations, 50);
        assert_eq!(cfg.max_context_tokens, 200_000);
        assert_eq!(cfg.max_idle_cycles, 50);
        assert_eq!(cfg.plan_approval_timeout_secs, 300);
        assert_eq!(cfg.command_timeout_secs, 30);
    }

    #[test]
    fn agent_config_default_fns() {
        // Hit each `default_*` helper directly.
        assert_eq!(default_max_loop_iterations(), 50);
        assert_eq!(default_max_context_tokens(), 200_000);
        assert_eq!(default_max_idle_cycles(), 50);
        assert_eq!(default_plan_approval_timeout_secs(), 300);
        assert_eq!(default_command_timeout_secs(), 30);
    }

    #[test]
    fn agent_config_deserialize_applies_defaults_for_missing_fields() {
        let json = r#"{
            "max_parallel_agents": 2,
            "poll_interval_ms": 100,
            "max_task_retries": 1
        }"#;
        let cfg: AgentConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.max_parallel_agents, 2);
        assert_eq!(cfg.max_loop_iterations, default_max_loop_iterations());
        assert_eq!(cfg.max_context_tokens, default_max_context_tokens());
        assert_eq!(cfg.max_idle_cycles, default_max_idle_cycles());
        assert_eq!(
            cfg.plan_approval_timeout_secs,
            default_plan_approval_timeout_secs()
        );
        assert_eq!(cfg.command_timeout_secs, default_command_timeout_secs());
    }
}
