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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::chat::ToolCall;
    use std::sync::Mutex;

    /// Minimal LLM client that returns a preset assistant message.
    struct FakeClient {
        reply: Mutex<Option<ChatMessage>>,
        tokens: u64,
    }

    impl FakeClient {
        fn new(reply: ChatMessage, tokens: u64) -> Self {
            Self {
                reply: Mutex::new(Some(reply)),
                tokens,
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmClient for FakeClient {
        async fn ask(&self, _system: &str, _user_message: &str) -> SdkResult<(String, u64)> {
            Ok(("hi".into(), self.tokens))
        }

        async fn chat(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> SdkResult<(ChatMessage, u64)> {
            let reply = self.reply.lock().unwrap().clone().expect("reply set");
            Ok((reply, self.tokens))
        }
    }

    #[tokio::test]
    async fn default_chat_with_usage_reports_output_tokens_only() {
        let client = FakeClient::new(
            ChatMessage::assistant("ok"),
            42,
        );
        let (_msg, usage) = client.chat_with_usage(&[], &[]).await.unwrap();
        assert_eq!(usage.output_tokens, 42);
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
        assert!(usage.model.is_empty());
    }

    #[tokio::test]
    async fn default_chat_stream_sends_text_for_final_answer() {
        let client = FakeClient::new(ChatMessage::assistant("hello world"), 3);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (_msg, tokens) = client.chat_stream(&[], &[], tx).await.unwrap();
        assert_eq!(tokens, 3);
        let delta = rx.recv().await.expect("one delta");
        match delta {
            StreamDelta::Text(s) => assert_eq!(s, "hello world"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn default_chat_stream_sends_thinking_when_tool_calls_present() {
        let reply = ChatMessage::assistant_with_tools(
            Some("thinking text".into()),
            vec![ToolCall::new("id1", "tool", "{}")],
        );
        let client = FakeClient::new(reply, 1);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = client.chat_stream(&[], &[], tx).await.unwrap();
        let delta = rx.recv().await.expect("one delta");
        assert!(matches!(delta, StreamDelta::Thinking(ref s) if s == "thinking text"));
    }

    #[tokio::test]
    async fn default_chat_stream_emits_nothing_when_no_content() {
        let reply = ChatMessage::assistant_with_tools(None, vec![ToolCall::new("id", "t", "{}")]);
        let client = FakeClient::new(reply, 0);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = client.chat_stream(&[], &[], tx).await.unwrap();
        assert!(rx.recv().await.is_none());
    }
}
