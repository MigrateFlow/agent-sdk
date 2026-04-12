use std::time::Duration;

use tracing::warn;

use sdk_core::error::{SdkError, SdkResult};

/// Configuration for HTTP retry behaviour, derived from [`LlmConfig`].
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    /// Backoff multiplier for 429 rate-limit retries.
    pub rate_limit_multiplier: u64,
    /// Backoff multiplier for 529 overload retries.
    pub overload_multiplier: u64,
    /// Backoff multiplier for 5xx server-error retries.
    pub server_error_multiplier: u64,
}

impl RetryConfig {
    pub fn from_llm_config(cfg: &sdk_core::config::LlmConfig) -> Self {
        Self {
            max_retries: cfg.max_retries,
            base_delay_ms: cfg.retry_base_delay_ms,
            rate_limit_multiplier: cfg.retry_rate_limit_multiplier,
            overload_multiplier: cfg.retry_overload_multiplier,
            server_error_multiplier: cfg.retry_server_error_multiplier,
        }
    }
}

/// Shared retry handler for transient HTTP errors from LLM APIs.
///
/// Returns `Ok(())` if the caller should retry, or `Err` if retries are
/// exhausted or the error is non-retryable.
pub async fn handle_retryable_status(
    status: u16,
    retries: &mut u32,
    config: &RetryConfig,
) -> SdkResult<()> {
    match status {
        429 => {
            if *retries >= config.max_retries {
                return Err(SdkError::RateLimited {
                    retry_after_ms: 60_000,
                });
            }
            let wait = Duration::from_millis(
                config.base_delay_ms * 2u64.pow(*retries) * config.rate_limit_multiplier,
            );
            warn!(retry = *retries, wait_ms = ?wait, "Rate limited, backing off");
            tokio::time::sleep(wait).await;
            *retries += 1;
            Ok(())
        }
        // Anthropic overload
        529 => {
            if *retries >= config.max_retries {
                return Err(SdkError::LlmApi {
                    status,
                    message: "API overloaded".to_string(),
                });
            }
            let wait = Duration::from_millis(
                config.base_delay_ms * 2u64.pow(*retries) * config.overload_multiplier,
            );
            warn!(retry = *retries, "API overloaded, backing off");
            tokio::time::sleep(wait).await;
            *retries += 1;
            Ok(())
        }
        500 | 502 | 503 => {
            if *retries >= config.max_retries {
                return Err(SdkError::LlmApi {
                    status,
                    message: "API server error".to_string(),
                });
            }
            let wait = Duration::from_millis(
                config.base_delay_ms * (*retries as u64 + 1) * config.server_error_multiplier,
            );
            warn!(retry = *retries, status, "Server error, backing off");
            tokio::time::sleep(wait).await;
            *retries += 1;
            Ok(())
        }
        _ => Err(SdkError::LlmApi {
            status,
            message: format!("Non-retryable status {}", status),
        }),
    }
}
