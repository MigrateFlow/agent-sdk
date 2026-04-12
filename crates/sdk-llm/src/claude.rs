use std::time::Duration;
use std::process::Stdio;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::debug;

use sdk_core::config::LlmConfig;
use sdk_core::error::{SdkError, SdkResult};
use sdk_core::types::chat::{ChatMessage, FunctionCall, ToolCall};
use sdk_core::types::usage::TokenUsage;
use sdk_core::traits::llm_client::LlmClient;
use sdk_core::traits::tool::ToolDefinition;

use super::rate_limiter::RateLimiter;
use super::retry::{RetryConfig, handle_retryable_status};

const ANTHROPIC_API_VERSION: &str = "2023-06-01";

pub struct ClaudeClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: usize,
    base_url: String,
    rate_limiter: RateLimiter,
    retry_config: RetryConfig,
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
    model: String,
    #[allow(dead_code)]
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    /// Prompt-cache write tokens (Anthropic-only). Optional: older responses
    /// and other providers omit this field.
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    /// Prompt-cache read (hit) tokens (Anthropic-only). Optional.
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
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
            .http1_only()
            .timeout(Duration::from_secs(config.http_timeout_secs))
            .build()
            .map_err(|e| SdkError::Config(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            http,
            api_key,
            model: config.resolve_model(),
            max_tokens: config.max_tokens,
            base_url,
            rate_limiter: RateLimiter::new(config.requests_per_minute),
            retry_config: RetryConfig::from_llm_config(config),
        })
    }

    async fn send_request(&self, request: &ApiRequest) -> SdkResult<ApiResponse> {
        if self.uses_dashscope_coding_plan() {
            return self.send_request_via_curl(request).await;
        }

        self.rate_limiter.acquire().await;

        let url = format!("{}/v1/messages", self.base_url);
        let mut retries = 0u32;

        loop {
            let response = self
                .http
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_API_VERSION)
                .header("content-type", "application/json")
                .json(request)
                .send()
                .await
                .map_err(|e| SdkError::LlmApi {
                    status: 0,
                    message: format!("Request failed: {}", e),
                })?;

            let status = response.status().as_u16();

            if status == 200 {
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

            // For non-200, try the shared retry handler.  If the status is
            // not retryable it returns an error with the body we already
            // consumed, so read the body first for unknown statuses.
            if matches!(status, 429 | 529 | 500 | 502 | 503) {
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

    fn uses_dashscope_coding_plan(&self) -> bool {
        self.base_url.contains("coding-intl.dashscope.aliyuncs.com/apps/anthropic")
    }

    async fn send_request_via_curl(&self, request: &ApiRequest) -> SdkResult<ApiResponse> {
        self.rate_limiter.acquire().await;

        let url = format!("{}/v1/messages", self.base_url);
        let body = serde_json::to_vec(request).map_err(SdkError::Serde)?;

        let mut child = tokio::process::Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .arg("--http1.1")
            .arg("--location")
            .arg("--request")
            .arg("POST")
            .arg(&url)
            .arg("--header")
            .arg(format!("x-api-key: {}", self.api_key))
            .arg("--header")
            .arg(format!("anthropic-version: {}", ANTHROPIC_API_VERSION))
            .arg("--header")
            .arg("content-type: application/json")
            .arg("--data-binary")
            .arg("@-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SdkError::Config(format!("Failed to spawn curl: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&body)
                .await
                .map_err(|e| SdkError::Config(format!("Failed to write curl request: {}", e)))?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| SdkError::Config(format!("curl execution failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(SdkError::LlmApi {
                status: output.status.code().unwrap_or(0) as u16,
                message: if stderr.is_empty() {
                    "curl request failed".to_string()
                } else {
                    stderr
                },
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let api_response: ApiResponse = serde_json::from_str(&stdout).map_err(|e| {
            SdkError::LlmResponseParse(format!(
                "Failed to parse Coding Plan response: {}",
                e
            ))
        })?;

        debug!(
            model = %api_response.model,
            input_tokens = api_response.usage.input_tokens,
            output_tokens = api_response.usage.output_tokens,
            "Claude response received via curl transport"
        );

        Ok(api_response)
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
        let (msg, usage) = self.chat_with_usage(messages, tools).await?;
        Ok((msg, usage.input_tokens + usage.output_tokens))
    }

    async fn chat_with_usage(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, TokenUsage)> {
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
        let usage = TokenUsage {
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cache_creation_input_tokens: response
                .usage
                .cache_creation_input_tokens
                .unwrap_or(0),
            cache_read_input_tokens: response.usage.cache_read_input_tokens.unwrap_or(0),
            model: response.model.clone(),
        };
        let chat_msg = anthropic_response_to_chat(&response);
        Ok((chat_msg, usage))
    }
}
