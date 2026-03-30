use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::config::LlmConfig;
use crate::error::{SdkError, SdkResult};
use crate::types::chat::{ChatMessage, FunctionCall, ToolCall};
use crate::traits::llm_client::LlmClient;
use crate::traits::tool::ToolDefinition;

use super::rate_limiter::RateLimiter;
use super::retry::{RetryConfig, handle_retryable_status};

pub struct OpenAiClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: usize,
    base_url: String,
    rate_limiter: RateLimiter,
    retry_config: RetryConfig,
}

#[derive(Debug, Clone, Serialize)]
struct ChatCompletionRequest {
    model: String,
    max_tokens: usize,
    messages: Vec<OaiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OaiToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OaiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OaiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OaiFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OaiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct OaiToolDef {
    #[serde(rename = "type")]
    tool_type: String,
    function: OaiFunctionDef,
}

#[derive(Debug, Clone, Serialize)]
struct OaiFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct ChatCompletionResponse {
    #[allow(dead_code)]
    id: Option<String>,
    choices: Vec<Choice>,
    #[allow(dead_code)]
    model: Option<String>,
    usage: Option<OaiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: OaiMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OaiUsage {
    #[allow(dead_code)]
    prompt_tokens: u64,
    #[allow(dead_code)]
    completion_tokens: u64,
    total_tokens: u64,
}

fn chat_messages_to_oai(messages: &[ChatMessage]) -> Vec<OaiMessage> {
    messages
        .iter()
        .map(|m| match m {
            ChatMessage::System { content } => OaiMessage {
                role: "system".to_string(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage::User { content } => OaiMessage {
                role: "user".to_string(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let oai_tool_calls = if tool_calls.is_empty() {
                    None
                } else {
                    Some(
                        tool_calls
                            .iter()
                            .map(|tc| OaiToolCall {
                                id: tc.id.clone(),
                                call_type: "function".to_string(),
                                function: OaiFunctionCall {
                                    name: tc.function.name.clone(),
                                    arguments: tc.function.arguments.clone(),
                                },
                            })
                            .collect(),
                    )
                };
                OaiMessage {
                    role: "assistant".to_string(),
                    content: content.clone(),
                    tool_calls: oai_tool_calls,
                    tool_call_id: None,
                }
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => OaiMessage {
                role: "tool".to_string(),
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: Some(tool_call_id.clone()),
            },
        })
        .collect()
}

fn oai_message_to_chat(msg: OaiMessage) -> ChatMessage {
    if msg.role == "assistant" {
        let tool_calls = msg
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                call_type: tc.call_type,
                function: FunctionCall {
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                },
            })
            .collect::<Vec<_>>();

        ChatMessage::Assistant {
            content: msg.content,
            tool_calls,
        }
    } else {
        ChatMessage::assistant(msg.content.unwrap_or_default())
    }
}

fn tool_defs_to_oai(tools: &[ToolDefinition]) -> Vec<OaiToolDef> {
    tools
        .iter()
        .map(|t| OaiToolDef {
            tool_type: "function".to_string(),
            function: OaiFunctionDef {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

impl OpenAiClient {
    pub fn new(config: &LlmConfig) -> SdkResult<Self> {
        let api_key = config.resolve_api_key().ok_or_else(|| {
            SdkError::Config(
                "OpenAI API key not set. Set OPENAI_API_KEY in .env or config.".to_string(),
            )
        })?;

        let base_url = config.resolve_base_url();

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.http_timeout_secs))
            .build()
            .map_err(|e| SdkError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            http,
            api_key,
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            base_url,
            rate_limiter: RateLimiter::new(config.requests_per_minute),
            retry_config: RetryConfig::from_llm_config(config),
        })
    }

    async fn send_chat(
        &self,
        messages: Vec<OaiMessage>,
        tools: Vec<OaiToolDef>,
    ) -> SdkResult<ChatCompletionResponse> {
        self.rate_limiter.acquire().await;

        let tool_choice = if tools.is_empty() {
            None
        } else {
            Some("auto".to_string())
        };

        let request = ChatCompletionRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages,
            tools,
            tool_choice,
        };

        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut retries = 0u32;

        loop {
            let response = self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request)
                .send()
                .await
                .map_err(|e| SdkError::LlmApi {
                    status: 0,
                    message: format!("Request failed: {}", e),
                })?;

            let status = response.status().as_u16();

            if status == 200 {
                let api_response: ChatCompletionResponse =
                    response.json().await.map_err(|e| {
                        SdkError::LlmResponseParse(format!(
                            "Failed to parse OpenAI response: {}",
                            e
                        ))
                    })?;
                debug!(
                    model = ?api_response.model,
                    total_tokens = ?api_response.usage.as_ref().map(|u| u.total_tokens),
                    "OpenAI response received"
                );
                return Ok(api_response);
            }

            if matches!(status, 429 | 500 | 502 | 503) {
                handle_retryable_status(status, &mut retries, &self.retry_config).await?;
            } else {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());
                return Err(SdkError::LlmApi {
                    status,
                    message: body,
                });
            }
        }
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn ask(&self, system: &str, user_message: &str) -> SdkResult<(String, u64)> {
        let messages = vec![
            OaiMessage {
                role: "system".to_string(),
                content: Some(system.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            OaiMessage {
                role: "user".to_string(),
                content: Some(user_message.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let response = self.send_chat(messages, Vec::new()).await?;

        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        let tokens = response.usage.map(|u| u.total_tokens).unwrap_or(0);
        Ok((text, tokens))
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, u64)> {
        let oai_messages = chat_messages_to_oai(messages);
        let oai_tools = tool_defs_to_oai(tools);

        let response = self.send_chat(oai_messages, oai_tools).await?;

        let msg = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| SdkError::LlmResponseParse("No choices in response".to_string()))?
            .message;

        let tokens = response.usage.map(|u| u.total_tokens).unwrap_or(0);
        let chat_msg = oai_message_to_chat(msg);
        Ok((chat_msg, tokens))
    }
}
