pub mod claude;
pub mod openai;
pub mod rate_limiter;
pub mod retry;
pub mod util;

use std::sync::Arc;

use sdk_core::config::{LlmConfig, LlmProvider};
use sdk_core::error::SdkResult;
use sdk_core::traits::llm_client::LlmClient;

pub use claude::ClaudeClient;
pub use openai::OpenAiClient;

/// Factory: create the appropriate LLM client based on config provider.
pub fn create_client(config: &LlmConfig) -> SdkResult<Arc<dyn LlmClient>> {
    match config.provider {
        LlmProvider::Claude => Ok(Arc::new(ClaudeClient::new(config)?)),
        LlmProvider::OpenAi => Ok(Arc::new(OpenAiClient::new(config)?)),
    }
}
