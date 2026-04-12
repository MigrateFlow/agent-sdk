use std::time::Duration;
use std::process::Stdio;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::debug;

use tokio::sync::mpsc;

use sdk_core::config::LlmConfig;
use sdk_core::error::{SdkError, SdkResult};
use sdk_core::types::chat::{ChatMessage, FunctionCall, ToolCall};
use sdk_core::types::usage::TokenUsage;
use sdk_core::traits::llm_client::{LlmClient, StreamDelta};
use sdk_core::traits::tool::ToolDefinition;

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
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

// ─── SSE streaming types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct StreamingChatCompletionRequest {
    model: String,
    max_tokens: usize,
    messages: Vec<OaiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OaiToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Clone, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    usage: Option<OaiUsage>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamChoice {
    delta: StreamDeltaMsg,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamDeltaMsg {
    content: Option<String>,
    tool_calls: Option<Vec<StreamToolCallDelta>>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<StreamFunctionDelta>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
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

    async fn send_chat(
        &self,
        messages: Vec<OaiMessage>,
        tools: Vec<OaiToolDef>,
    ) -> SdkResult<ChatCompletionResponse> {
        if self.uses_dashscope_coding_plan() {
            return self.send_chat_via_curl(messages, tools).await;
        }

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

        let url = format!("{}/v1/chat/completions", self.normalized_base_url());
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

    fn uses_dashscope_coding_plan(&self) -> bool {
        self.base_url.contains("coding-intl.dashscope.aliyuncs.com")
    }

    fn normalized_base_url(&self) -> &str {
        let trimmed = self.base_url.trim_end_matches('/');
        trimmed.strip_suffix("/v1").unwrap_or(trimmed)
    }

    async fn send_chat_stream(
        &self,
        messages: Vec<OaiMessage>,
        tools: Vec<OaiToolDef>,
        tx: &mpsc::UnboundedSender<StreamDelta>,
    ) -> SdkResult<(ChatMessage, u64)> {
        self.rate_limiter.acquire().await;

        let tool_choice = if tools.is_empty() {
            None
        } else {
            Some("auto".to_string())
        };

        let stream_options = if self.uses_dashscope_coding_plan() {
            None
        } else {
            Some(StreamOptions { include_usage: true })
        };

        let request = StreamingChatCompletionRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages,
            tools,
            tool_choice,
            stream: true,
            stream_options,
        };

        let url = format!("{}/v1/chat/completions", self.normalized_base_url());

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
                message: format!("Streaming request failed: {}", e),
            })?;

        let status = response.status().as_u16();
        if status != 200 {
            let body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(SdkError::LlmApi { status, message: body });
        }

        // Accumulate the response from SSE chunks
        let mut content = String::new();
        let mut tool_calls_map: Vec<(String, String, String)> = Vec::new(); // (id, name, args)
        let mut total_tokens = 0u64;
        let mut has_tool_calls = false;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut done = false;

        use futures_util::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk_bytes = chunk_result.map_err(|e| SdkError::LlmApi {
                status: 0,
                message: format!("Stream read error: {}", e),
            })?;

            buffer.push_str(&String::from_utf8_lossy(&chunk_bytes));

            // Process complete SSE lines from buffer
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                let data = if let Some(d) = line.strip_prefix("data: ") {
                    d.trim()
                } else {
                    continue;
                };

                if data == "[DONE]" {
                    done = true;
                    break;
                }

                let chunk: StreamChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Extract usage from the final chunk
                if let Some(usage) = chunk.usage {
                    total_tokens = usage.total_tokens;
                }

                for choice in &chunk.choices {
                    // Text content delta
                    if let Some(ref text) = choice.delta.content {
                        if !text.is_empty() {
                            content.push_str(text);
                            if !has_tool_calls {
                                let _ = tx.send(StreamDelta::Text(text.clone()));
                            } else {
                                let _ = tx.send(StreamDelta::Thinking(text.clone()));
                            }
                        }
                    }

                    // Tool call deltas
                    if let Some(ref tc_deltas) = choice.delta.tool_calls {
                        has_tool_calls = true;
                        for tc_delta in tc_deltas {
                            let idx = tc_delta.index;
                            // Grow the map if needed
                            while tool_calls_map.len() <= idx {
                                tool_calls_map.push((String::new(), String::new(), String::new()));
                            }
                            if let Some(ref id) = tc_delta.id {
                                if !id.is_empty() {
                                    tool_calls_map[idx].0 = id.clone();
                                }
                            }
                            if let Some(ref func) = tc_delta.function {
                                if let Some(ref name) = func.name {
                                    if !name.is_empty() {
                                        tool_calls_map[idx].1 = name.clone();
                                    }
                                }
                                if let Some(ref args) = func.arguments {
                                    tool_calls_map[idx].2.push_str(args);
                                }
                            }
                        }
                    }

                    // Check finish_reason for thinking text
                    if choice.finish_reason.as_deref() == Some("tool_calls") && !content.is_empty() {
                        // Content before tool calls was thinking text — already sent as Thinking
                    }
                }
            }
            if done { break; }
        }

        // Build the final ChatMessage
        let tool_calls: Vec<ToolCall> = tool_calls_map
            .into_iter()
            .filter(|(id, _, _)| !id.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                call_type: "function".to_string(),
                function: FunctionCall { name, arguments },
            })
            .collect();

        let msg = if tool_calls.is_empty() {
            ChatMessage::Assistant {
                content: if content.is_empty() { None } else { Some(content) },
                tool_calls: vec![],
            }
        } else {
            // When there are tool calls, content is "thinking" text
            ChatMessage::Assistant {
                content: if content.is_empty() { None } else { Some(content) },
                tool_calls,
            }
        };

        Ok((msg, total_tokens))
    }

    async fn send_chat_stream_via_curl(
        &self,
        messages: Vec<OaiMessage>,
        tools: Vec<OaiToolDef>,
        tx: &mpsc::UnboundedSender<StreamDelta>,
    ) -> SdkResult<(ChatMessage, u64)> {
        self.rate_limiter.acquire().await;

        let tool_choice = if tools.is_empty() {
            None
        } else {
            Some("auto".to_string())
        };

        let request = StreamingChatCompletionRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages,
            tools,
            tool_choice,
            stream: true,
            stream_options: None,
        };

        let url = format!("{}/v1/chat/completions", self.normalized_base_url());
        let body = serde_json::to_vec(&request).map_err(SdkError::Serde)?;

        let mut child = tokio::process::Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .arg("--http1.1")
            .arg("--no-buffer")
            .arg("--location")
            .arg("--request")
            .arg("POST")
            .arg(&url)
            .arg("--header")
            .arg(format!("Authorization: Bearer {}", self.api_key))
            .arg("--header")
            .arg("Content-Type: application/json")
            .arg("--data-binary")
            .arg("@-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SdkError::Config(format!("Failed to spawn curl: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&body).await
                .map_err(|e| SdkError::Config(format!("Failed to write curl request: {}", e)))?;
        }

        // Read stdout line by line for SSE parsing
        let stdout = child.stdout.take()
            .ok_or_else(|| SdkError::Config("No stdout from curl".to_string()))?;

        let mut content = String::new();
        let mut tool_calls_map: Vec<(String, String, String)> = Vec::new();
        let mut total_tokens = 0u64;
        let mut has_tool_calls = false;

        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await
                .map_err(|e| SdkError::Config(format!("Failed to read curl stdout: {}", e)))?;
            if bytes_read == 0 { break; }

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with(':') { continue; }

            let data = match trimmed.strip_prefix("data: ") {
                Some(d) => d.trim(),
                None => continue,
            };

            if data == "[DONE]" { break; }

            let chunk: StreamChunk = match serde_json::from_str(data) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if let Some(usage) = chunk.usage {
                total_tokens = usage.total_tokens;
            }

            for choice in &chunk.choices {
                if let Some(ref text) = choice.delta.content {
                    if !text.is_empty() {
                        content.push_str(text);
                        if !has_tool_calls {
                            let _ = tx.send(StreamDelta::Text(text.clone()));
                        } else {
                            let _ = tx.send(StreamDelta::Thinking(text.clone()));
                        }
                    }
                }

                if let Some(ref tc_deltas) = choice.delta.tool_calls {
                    has_tool_calls = true;
                    for tc_delta in tc_deltas {
                        let idx = tc_delta.index;
                        while tool_calls_map.len() <= idx {
                            tool_calls_map.push((String::new(), String::new(), String::new()));
                        }
                        if let Some(ref id) = tc_delta.id {
                            if !id.is_empty() {
                                tool_calls_map[idx].0 = id.clone();
                            }
                        }
                        if let Some(ref func) = tc_delta.function {
                            if let Some(ref name) = func.name {
                                if !name.is_empty() {
                                    tool_calls_map[idx].1 = name.clone();
                                }
                            }
                            if let Some(ref args) = func.arguments {
                                tool_calls_map[idx].2.push_str(args);
                            }
                        }
                    }
                }

                // Check finish_reason for thinking text
                if choice.finish_reason.as_deref() == Some("tool_calls") && !content.is_empty() {
                    // Content before tool calls was thinking text — already sent as Thinking
                }
            }
        }

        // Wait for curl to exit
        let _ = child.wait().await;

        let tool_calls: Vec<ToolCall> = tool_calls_map
            .into_iter()
            .filter(|(id, _, _)| !id.is_empty())
            .map(|(id, name, arguments)| ToolCall {
                id,
                call_type: "function".to_string(),
                function: FunctionCall { name, arguments },
            })
            .collect();

        let msg = ChatMessage::Assistant {
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls,
        };

        Ok((msg, total_tokens))
    }

    async fn send_chat_via_curl(
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

        let url = format!("{}/v1/chat/completions", self.normalized_base_url());
        let body = serde_json::to_vec(&request).map_err(SdkError::Serde)?;

        let mut child = tokio::process::Command::new("curl")
            .arg("--silent")
            .arg("--show-error")
            .arg("--http1.1")
            .arg("--location")
            .arg("--request")
            .arg("POST")
            .arg(&url)
            .arg("--header")
            .arg(format!("Authorization: Bearer {}", self.api_key))
            .arg("--header")
            .arg("Content-Type: application/json")
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
        let api_response: ChatCompletionResponse =
            serde_json::from_str(&stdout).map_err(|e| {
                SdkError::LlmResponseParse(format!(
                    "Failed to parse Coding Plan OpenAI response: {}",
                    e
                ))
            })?;

        debug!(
            model = ?api_response.model,
            total_tokens = ?api_response.usage.as_ref().map(|u| u.total_tokens),
            "OpenAI response received via curl transport"
        );

        Ok(api_response)
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
        let (msg, usage) = self.chat_with_usage(messages, tools).await?;
        Ok((msg, usage.input_tokens + usage.output_tokens))
    }

    async fn chat_with_usage(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, TokenUsage)> {
        let oai_messages = chat_messages_to_oai(messages);
        let oai_tools = tool_defs_to_oai(tools);

        let response = self.send_chat(oai_messages, oai_tools).await?;

        let model = response.model.clone().unwrap_or_else(|| self.model.clone());
        let usage = response
            .usage
            .as_ref()
            .map(|u| TokenUsage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                model: model.clone(),
            })
            .unwrap_or_else(|| TokenUsage {
                model: model.clone(),
                ..Default::default()
            });

        let msg = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| SdkError::LlmResponseParse("No choices in response".to_string()))?
            .message;

        let chat_msg = oai_message_to_chat(msg);
        Ok((chat_msg, usage))
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        tx: mpsc::UnboundedSender<StreamDelta>,
    ) -> SdkResult<(ChatMessage, u64)> {
        let oai_messages = chat_messages_to_oai(messages);
        let oai_tools = tool_defs_to_oai(tools);

        // DashScope Coding Plan needs curl transport for streaming too
        if self.uses_dashscope_coding_plan() {
            return self.send_chat_stream_via_curl(oai_messages, oai_tools, &tx).await;
        }

        self.send_chat_stream(oai_messages, oai_tools, &tx).await
    }
}
