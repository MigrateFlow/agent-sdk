use async_trait::async_trait;

use crate::error::SdkResult;
use crate::types::chat::ChatMessage;
use crate::traits::tool::ToolDefinition;

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn ask(&self, system: &str, user_message: &str) -> SdkResult<(String, u64)>;

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, u64)>;
}
