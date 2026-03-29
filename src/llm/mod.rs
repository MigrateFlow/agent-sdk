pub mod claude;
pub mod openai;
pub mod rate_limiter;
pub mod util;

use std::sync::Arc;

use crate::config::{LlmConfig, LlmProvider};
use crate::error::SdkResult;
use crate::traits::llm_client::LlmClient;

pub use claude::ClaudeClient;
pub use openai::OpenAiClient;

/// Factory: create the appropriate LLM client based on config provider.
pub fn create_client(config: &LlmConfig) -> SdkResult<Arc<dyn LlmClient>> {
    match config.provider {
        LlmProvider::Claude => Ok(Arc::new(ClaudeClient::new(config)?)),
        LlmProvider::OpenAi => Ok(Arc::new(OpenAiClient::new(config)?)),
    }
}
