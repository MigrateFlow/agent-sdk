use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::LlmConfig;
use crate::error::{SdkError, SdkResult};
use crate::types::chat::{ChatMessage, FunctionCall, ToolCall};
use crate::traits::llm_client::LlmClient;
use crate::traits::tool::ToolDefinition;

use super::rate_limiter::RateLimiter;

pub struct ClaudeClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: usize,
    base_url: String,
    rate_limiter: RateLimiter,
}

#[derive(Debug, Clone, Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicToolDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicToolDef {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiResponse {
    #[allow(dead_code)]
    id: String,
    content: Vec<ContentBlock>,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Debug, Clone, Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

impl Usage {
    fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

fn chat_messages_to_anthropic(messages: &[ChatMessage]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system_prompt = None;
    let mut anthropic_msgs = Vec::new();

    for msg in messages {
        match msg {
            ChatMessage::System { content } => {
                system_prompt = Some(content.clone());
            }
            ChatMessage::User { content } => {
                anthropic_msgs.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text(content.clone()),
                });
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut blocks = Vec::new();
                if let Some(text) = content {
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                }
                for tc in tool_calls {
                    let input: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                    blocks.push(ContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        input,
                    });
                }
                if blocks.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: String::new(),
                    });
                }
                anthropic_msgs.push(AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(blocks),
                });
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                anthropic_msgs.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tool_call_id.clone(),
                        content: content.clone(),
                    }]),
                });
            }
        }
    }

    (system_prompt, anthropic_msgs)
}

fn anthropic_response_to_chat(response: &ApiResponse) -> ChatMessage {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in &response.content {
        match block {
            ContentBlock::Text { text } => {
                text_parts.push(text.clone());
            }
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id: id.clone(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(input).unwrap_or_default(),
                    },
                });
            }
            _ => {}
        }
    }

    let content = if text_parts.is_empty() {
        None
    } else {
        let joined = text_parts.join("");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    };

    ChatMessage::Assistant {
        content,
        tool_calls,
    }
}

fn tool_defs_to_anthropic(tools: &[ToolDefinition]) -> Vec<AnthropicToolDef> {
    tools
        .iter()
        .map(|t| AnthropicToolDef {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect()
}

impl ClaudeClient {
    pub fn new(config: &LlmConfig) -> SdkResult<Self> {
        let api_key = config.resolve_api_key().ok_or_else(|| {
            SdkError::Config(
                "Anthropic API key not set. Set ANTHROPIC_API_KEY in .env or config.".to_string(),
            )
        })?;

        let base_url = config.resolve_base_url();

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| SdkError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            http,
            api_key,
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            base_url,
            rate_limiter: RateLimiter::new(config.requests_per_minute),
        })
    }

    async fn send_request(&self, request: &ApiRequest) -> SdkResult<ApiResponse> {
        self.rate_limiter.acquire().await;

        let url = format!("{}/v1/messages", self.base_url);
        let mut retries = 0u32;
        let max_retries = 3;

        loop {
            let response = self
                .http
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(request)
                .send()
                .await
                .map_err(|e| SdkError::LlmApi {
                    status: 0,
                    message: format!("Request failed: {}", e),
                })?;

            let status = response.status().as_u16();

            match status {
                200 => {
                    let api_response: ApiResponse =
                        response.json().await.map_err(|e| {
                            SdkError::LlmResponseParse(format!(
                                "Failed to parse response: {}",
                                e
                            ))
                        })?;
                    debug!(
                        model = %api_response.model,
                        input_tokens = api_response.usage.input_tokens,
                        output_tokens = api_response.usage.output_tokens,
                        "Claude response received"
                    );
                    return Ok(api_response);
                }
                429 => {
                    if retries >= max_retries {
                        return Err(SdkError::RateLimited {
                            retry_after_ms: 60000,
                        });
                    }
                    let wait = Duration::from_millis(1000 * 2u64.pow(retries));
                    warn!(retry = retries, wait_ms = ?wait, "Rate limited, backing off");
                    tokio::time::sleep(wait).await;
                    retries += 1;
                }
                529 => {
                    if retries >= max_retries {
                        return Err(SdkError::LlmApi {
                            status,
                            message: "API overloaded".to_string(),
                        });
                    }
                    let wait = Duration::from_secs(30);
                    warn!(retry = retries, "API overloaded, waiting 30s");
                    tokio::time::sleep(wait).await;
                    retries += 1;
                }
                _ => {
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
}

#[async_trait]
impl LlmClient for ClaudeClient {
    async fn ask(&self, system: &str, user_message: &str) -> SdkResult<(String, u64)> {
        let request = ApiRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: Some(system.to_string()),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text(user_message.to_string()),
            }],
            tools: Vec::new(),
        };

        let response = self.send_request(&request).await?;
        let text = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        let tokens = response.usage.total_tokens();
        Ok((text, tokens))
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, u64)> {
        let (system_prompt, anthropic_msgs) = chat_messages_to_anthropic(messages);
        let anthropic_tools = tool_defs_to_anthropic(tools);

        let request = ApiRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            system: system_prompt,
            messages: anthropic_msgs,
            tools: anthropic_tools,
        };

        let response = self.send_request(&request).await?;
        let tokens = response.usage.total_tokens();
        let chat_msg = anthropic_response_to_chat(&response);
        Ok((chat_msg, tokens))
    }
}
