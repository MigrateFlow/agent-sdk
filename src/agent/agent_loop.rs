use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::error::{AgentId, SdkError, SdkResult};
use crate::types::chat::ChatMessage;
use crate::traits::llm_client::LlmClient;
use crate::tools::registry::ToolRegistry;

use super::events::AgentEvent;

const BYTES_PER_TOKEN: usize = 4;
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 200_000;
const MAX_TOOL_RESULT_CHARS: usize = 12_000;
const COMPACT_KEEP_RECENT: usize = 10;

#[derive(Debug)]
pub struct AgentLoopResult {
    pub final_content: String,
    pub messages: Vec<ChatMessage>,
    pub total_tokens: u64,
    pub iterations: usize,
    pub tool_calls_count: usize,
}

pub struct AgentLoop {
    agent_id: AgentId,
    agent_name: String,
    llm_client: Arc<dyn LlmClient>,
    tools: ToolRegistry,
    messages: Vec<ChatMessage>,
    max_iterations: usize,
    max_context_chars: usize,
    total_tokens: u64,
    tool_calls_count: usize,
    event_tx: Option<UnboundedSender<AgentEvent>>,
}

impl AgentLoop {
    pub fn new(
        agent_id: AgentId,
        llm_client: Arc<dyn LlmClient>,
        tools: ToolRegistry,
        system_prompt: String,
        max_iterations: usize,
    ) -> Self {
        let messages = vec![ChatMessage::system(system_prompt)];
        Self {
            agent_id,
            agent_name: String::new(),
            llm_client,
            tools,
            messages,
            max_iterations,
            max_context_chars: DEFAULT_MAX_CONTEXT_TOKENS * BYTES_PER_TOKEN,
            total_tokens: 0,
            tool_calls_count: 0,
            event_tx: None,
        }
    }

    /// Set the maximum context window size in tokens.
    pub fn with_max_context_tokens(mut self, tokens: usize) -> Self {
        self.max_context_chars = tokens * BYTES_PER_TOKEN;
        self
    }

    /// Set a human-readable name for this agent (used in events).
    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    /// Create an AgentLoop with existing conversation history (for multi-turn).
    pub fn with_messages(
        agent_id: AgentId,
        llm_client: Arc<dyn LlmClient>,
        tools: ToolRegistry,
        messages: Vec<ChatMessage>,
        max_iterations: usize,
    ) -> Self {
        Self {
            agent_id,
            agent_name: String::new(),
            llm_client,
            tools,
            messages,
            max_iterations,
            max_context_chars: DEFAULT_MAX_CONTEXT_TOKENS * BYTES_PER_TOKEN,
            total_tokens: 0,
            tool_calls_count: 0,
            event_tx: None,
        }
    }

    pub fn set_event_sink(&mut self, tx: UnboundedSender<AgentEvent>) {
        self.event_tx = Some(tx);
    }

    /// Get a clone of the current conversation messages.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub async fn run(&mut self, initial_user_message: String) -> SdkResult<AgentLoopResult> {
        self.messages
            .push(ChatMessage::user(initial_user_message));

        let tool_defs = self.tools.definitions();

        for iteration in 0..self.max_iterations {
            self.compact_if_needed();

            debug!(
                agent_id = %self.agent_id,
                iteration,
                messages = self.messages.len(),
                context_chars = self.estimate_context_size(),
                "Agent loop iteration"
            );

            let (response, tokens) = self
                .llm_client
                .chat(&self.messages, &tool_defs)
                .await?;
            self.total_tokens += tokens;

            match &response {
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } if !tool_calls.is_empty() => {
                    if let Some(text) = content {
                        if !text.is_empty() {
                            self.emit(AgentEvent::Thinking {
                                agent_id: self.agent_id,
                                name: self.agent_name.clone(),
                                content: truncate(text, 200),
                                iteration,
                            });
                        }
                    }

                    self.messages.push(response.clone());

                    for tool_call in tool_calls {
                        self.emit(AgentEvent::ToolCall {
                            agent_id: self.agent_id,
                            name: self.agent_name.clone(),
                            tool_name: tool_call.function.name.clone(),
                            // Keep full arguments for event consumers so they can
                            // reliably extract fields like path/command.
                            arguments: tool_call.function.arguments.clone(),
                            iteration,
                        });

                        let result = self
                            .tools
                            .execute(
                                &tool_call.function.name,
                                serde_json::from_str(&tool_call.function.arguments)
                                    .unwrap_or_default(),
                            )
                            .await;

                        let result_content = match &result {
                            Ok(val) => {
                                let full = serde_json::to_string(val).unwrap_or_default();
                                truncate_tool_result(&full)
                            }
                            Err(e) => {
                                serde_json::json!({"error": e.to_string()}).to_string()
                            }
                        };

                        self.emit(AgentEvent::ToolResult {
                            agent_id: self.agent_id,
                            name: self.agent_name.clone(),
                            tool_name: tool_call.function.name.clone(),
                            result_preview: truncate(&result_content, 300),
                            iteration,
                        });

                        self.messages.push(ChatMessage::tool_result(
                            &tool_call.id,
                            &result_content,
                        ));

                        self.tool_calls_count += 1;
                    }
                }
                ChatMessage::Assistant { content, .. } => {
                    let final_content = content.clone().unwrap_or_default();
                    self.messages.push(response);

                    info!(
                        agent_id = %self.agent_id,
                        iterations = iteration + 1,
                        tool_calls = self.tool_calls_count,
                        tokens = self.total_tokens,
                        "Agent loop completed"
                    );

                    return Ok(AgentLoopResult {
                        final_content,
                        messages: self.messages.clone(),
                        total_tokens: self.total_tokens,
                        iterations: iteration + 1,
                        tool_calls_count: self.tool_calls_count,
                    });
                }
                other => {
                    warn!(
                        agent_id = %self.agent_id,
                        "Unexpected message type from LLM, treating as final"
                    );
                    let final_content = other.text_content().unwrap_or("").to_string();
                    self.messages.push(response);
                    return Ok(AgentLoopResult {
                        final_content,
                        messages: self.messages.clone(),
                        total_tokens: self.total_tokens,
                        iterations: iteration + 1,
                        tool_calls_count: self.tool_calls_count,
                    });
                }
            }
        }

        Err(SdkError::MaxIterationsExceeded {
            max_iterations: self.max_iterations,
        })
    }

    fn estimate_context_size(&self) -> usize {
        self.messages.iter().map(|m| m.char_len()).sum()
    }

    fn compact_if_needed(&mut self) {
        let size = self.estimate_context_size();
        if size <= self.max_context_chars {
            return;
        }

        warn!(
            agent_id = %self.agent_id,
            size_chars = size,
            max_chars = self.max_context_chars,
            messages = self.messages.len(),
            "Context too large, compacting"
        );

        let total = self.messages.len();
        if total <= COMPACT_KEEP_RECENT + 2 {
            self.truncate_all_tool_results(2000);
            return;
        }

        let keep_after = total - COMPACT_KEEP_RECENT;

        for i in 1..keep_after {
            match &self.messages[i] {
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } => {
                    if content.len() > 200 {
                        let summary = format!(
                            "[compacted: {} chars] {}",
                            content.len(),
                            safe_prefix(content, 150)
                        );
                        self.messages[i] = ChatMessage::Tool {
                            tool_call_id: tool_call_id.clone(),
                            content: summary,
                        };
                    }
                }
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } if content.as_ref().is_some_and(|c| c.len() > 500) => {
                    let short = content.as_ref().map(|c| truncate(c, 200));
                    self.messages[i] = ChatMessage::Assistant {
                        content: short,
                        tool_calls: tool_calls.clone(),
                    };
                }
                _ => {}
            }
        }

        let new_size = self.estimate_context_size();
        debug!(
            agent_id = %self.agent_id,
            before = size,
            after = new_size,
            "Context compacted"
        );
    }

    fn truncate_all_tool_results(&mut self, max_chars: usize) {
        for msg in &mut self.messages {
            if let ChatMessage::Tool {
                tool_call_id,
                content,
            } = msg
            {
                if content.len() > max_chars {
                    let summary = format!(
                        "[truncated: {} chars] {}",
                        content.len(),
                        safe_prefix(content, max_chars)
                    );
                    *msg = ChatMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        content: summary,
                    };
                }
            }
        }
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}

fn truncate_tool_result(s: &str) -> String {
    if s.len() <= MAX_TOOL_RESULT_CHARS {
        return s.to_string();
    }

    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(content) = val.get_mut("content") {
            if let Some(text) = content.as_str() {
                if text.len() > MAX_TOOL_RESULT_CHARS - 200 {
                    let limit = MAX_TOOL_RESULT_CHARS - 200;
                    let truncated = format!(
                        "{}...\n\n[truncated: showing {}/{} chars. Use offset parameter to read more.]",
                        safe_prefix(text, limit),
                        limit,
                        text.len()
                    );
                    *content = serde_json::Value::String(truncated);
                    return serde_json::to_string(&val)
                        .unwrap_or_else(|_| safe_prefix(s, MAX_TOOL_RESULT_CHARS).to_string());
                }
            }
        }
    }

    format!(
        "{}...[truncated: {}/{} chars]",
        safe_prefix(s, MAX_TOOL_RESULT_CHARS),
        MAX_TOOL_RESULT_CHARS,
        s.len()
    )
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", safe_prefix(s, max_len))
    }
}

fn safe_prefix(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }

    match s.char_indices().map(|(idx, _)| idx).take_while(|&idx| idx <= max_len).last() {
        Some(0) | None => "",
        Some(idx) => &s[..idx],
    }
}
