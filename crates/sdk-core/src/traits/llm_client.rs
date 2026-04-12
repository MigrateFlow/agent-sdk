use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::SdkResult;
use crate::types::chat::ChatMessage;
use crate::types::usage::TokenUsage;
use crate::traits::tool::ToolDefinition;

/// Incremental delta emitted during streaming.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// A chunk of assistant text content.
    Text(String),
    /// Thinking / reasoning text (content before tool calls).
    Thinking(String),
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn ask(&self, system: &str, user_message: &str) -> SdkResult<(String, u64)>;

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, u64)>;

    /// Detailed variant of `chat` that also returns a `TokenUsage` breakdown
    /// (input/output + optional cache metrics + model name).
    ///
    /// The default implementation forwards to `chat()` and synthesizes a zero
    /// cache-hit `TokenUsage`, so existing implementations keep working. Providers
    /// that can surface more detail should override this.
    async fn chat_with_usage(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, TokenUsage)> {
        let (msg, total) = self.chat(messages, tools).await?;
        Ok((
            msg,
            TokenUsage {
                input_tokens: 0,
                output_tokens: total,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                model: String::new(),
            },
        ))
    }

    /// Streaming variant of `chat`. Sends incremental deltas via `tx` as they
    /// arrive, then returns the complete `(ChatMessage, tokens)` at the end.
    ///
    /// Default implementation falls back to non-streaming `chat()` and sends
    /// the full text as a single delta.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        tx: mpsc::UnboundedSender<StreamDelta>,
    ) -> SdkResult<(ChatMessage, u64)> {
        let (msg, tokens) = self.chat(messages, tools).await?;
        if let ChatMessage::Assistant { ref content, ref tool_calls } = msg {
            if tool_calls.is_empty() {
                if let Some(text) = content {
                    let _ = tx.send(StreamDelta::Text(text.clone()));
                }
            } else if let Some(text) = content {
                let _ = tx.send(StreamDelta::Thinking(text.clone()));
            }
        }
        Ok((msg, tokens))
    }
}
