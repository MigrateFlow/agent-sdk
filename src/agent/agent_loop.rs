use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::error::{AgentId, SdkError, SdkResult};
use crate::types::chat::ChatMessage;
use crate::traits::llm_client::LlmClient;
use crate::tools::registry::ToolRegistry;

use super::events::AgentEvent;

/// A result delivered from a background agent (subagent or team) back to the
/// parent agent's conversation.  The agent loop drains these before each LLM
/// call and injects them as user-role messages so the model can reference them.
#[derive(Debug, Clone)]
pub struct BackgroundResult {
    /// Human-readable name (e.g. "explore", "backend-team").
    pub name: String,
    /// Whether this was a subagent or a team.
    pub kind: BackgroundResultKind,
    /// The final content / summary produced by the background agent.
    pub content: String,
    /// Token usage.
    pub tokens_used: u64,
}

#[derive(Debug, Clone)]
pub enum BackgroundResultKind {
    SubAgent,
    AgentTeam,
}

const CHARS_PER_ESTIMATED_TOKEN: usize = 4;
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

#[derive(Debug, Clone)]
pub enum CompactionStrategy {
    /// Auto: dynamically select a strategy based on overflow severity and message mix
    Auto,
    /// Default: Keep recent messages, compress older ones
    Default,
    /// Conservative: Preserve more context at cost of higher memory
    Conservative,
    /// Aggressive: More aggressive compression for resource-constrained environments
    Aggressive,
    /// Custom: User-defined compaction rules with specific parameters
    Custom {
        keep_recent: usize,
        tool_result_chars_limit: usize,
        assistant_content_limit: usize,
        fallback_truncate_chars: usize,
    },
}

impl Default for CompactionStrategy {
    fn default() -> Self {
        CompactionStrategy::Auto
    }
}

#[derive(Debug, Clone, Copy)]
struct CompactionProfile {
    keep_recent: usize,
    tool_result_chars_limit: usize,
    assistant_content_limit: usize,
    fallback_truncate_chars: usize,
    compress_user_messages: bool,
}

impl CompactionProfile {
    const DEFAULT: Self = Self {
        keep_recent: COMPACT_KEEP_RECENT,
        tool_result_chars_limit: 200,
        assistant_content_limit: 500,
        fallback_truncate_chars: 2000,
        compress_user_messages: false,
    };

    const CONSERVATIVE: Self = Self {
        keep_recent: 15,
        tool_result_chars_limit: 500,
        assistant_content_limit: 1000,
        fallback_truncate_chars: 5000,
        compress_user_messages: false,
    };

    const AGGRESSIVE: Self = Self {
        keep_recent: 5,
        tool_result_chars_limit: 100,
        assistant_content_limit: 100,
        fallback_truncate_chars: 500,
        compress_user_messages: true,
    };
}

pub struct AgentLoop {
    agent_id: AgentId,
    agent_name: String,
    llm_client: Arc<dyn LlmClient>,
    tools: ToolRegistry,
    messages: Vec<ChatMessage>,
    max_iterations: usize,
    max_context_tokens: usize,
    total_tokens: u64,
    tool_calls_count: usize,
    event_tx: Option<UnboundedSender<AgentEvent>>,
    compaction_strategy: CompactionStrategy,
    /// Receives results from background subagents / teams.
    /// Drained before each LLM call and injected as user messages.
    background_rx: Option<UnboundedReceiver<BackgroundResult>>,
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
            max_context_tokens: DEFAULT_MAX_CONTEXT_TOKENS,
            total_tokens: 0,
            tool_calls_count: 0,
            event_tx: None,
            compaction_strategy: CompactionStrategy::default(),
            background_rx: None,
        }
    }

    /// Set the maximum context window size in tokens.
    pub fn with_max_context_tokens(mut self, tokens: usize) -> Self {
        self.max_context_tokens = tokens;
        self
    }

    /// Set the compaction strategy to use.
    pub fn with_compaction_strategy(mut self, strategy: CompactionStrategy) -> Self {
        self.compaction_strategy = strategy;
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
            max_context_tokens: DEFAULT_MAX_CONTEXT_TOKENS,
            total_tokens: 0,
            tool_calls_count: 0,
            event_tx: None,
            compaction_strategy: CompactionStrategy::default(),
            background_rx: None,
        }
    }

    pub fn set_event_sink(&mut self, tx: UnboundedSender<AgentEvent>) {
        self.event_tx = Some(tx);
    }

    /// Set the receiver for background agent results.
    /// Results arriving on this channel are injected as user messages before
    /// each LLM call, mirroring Claude Code's background agent notification.
    pub fn set_background_rx(&mut self, rx: UnboundedReceiver<BackgroundResult>) {
        self.background_rx = Some(rx);
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
            // Drain any completed background agent results and inject them
            // as user messages so the LLM can reference them.
            self.drain_background_results();

            self.compact_if_needed();

            debug!(
                agent_id = %self.agent_id,
                iteration,
                messages = self.messages.len(),
                context_tokens = self.estimate_context_tokens(),
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

                    // Emit ToolCall events for all calls upfront
                    for tool_call in tool_calls {
                        self.emit(AgentEvent::ToolCall {
                            agent_id: self.agent_id,
                            name: self.agent_name.clone(),
                            tool_name: tool_call.function.name.clone(),
                            arguments: tool_call.function.arguments.clone(),
                            iteration,
                        });
                    }

                    // Execute all tool calls in parallel
                    let tools_ref = &self.tools;
                    let futures: Vec<_> = tool_calls
                        .iter()
                        .map(|tool_call| {
                            let name = tool_call.function.name.clone();
                            let args: serde_json::Value =
                                serde_json::from_str(&tool_call.function.arguments)
                                    .unwrap_or_default();
                            let id = tool_call.id.clone();
                            async move {
                                let result = tools_ref.execute(&name, args).await;
                                (id, name, result)
                            }
                        })
                        .collect();

                    let results = futures_util::future::join_all(futures).await;

                    for (call_id, tool_name, result) in results {
                        let (result_content, result_preview) = match &result {
                            Ok(val) => {
                                let preview = build_result_preview(val);
                                let full = serde_json::to_string(val).unwrap_or_default();
                                (truncate_tool_result(&full), preview)
                            }
                            Err(e) => {
                                let err = serde_json::json!({"error": e.to_string()}).to_string();
                                (err.clone(), err)
                            }
                        };

                        self.emit(AgentEvent::ToolResult {
                            agent_id: self.agent_id,
                            name: self.agent_name.clone(),
                            tool_name,
                            result_preview,
                            iteration,
                        });

                        self.messages.push(ChatMessage::tool_result(
                            &call_id,
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

    fn estimate_context_tokens(&self) -> usize {
        self.messages
            .iter()
            .map(|m| m.char_len().div_ceil(CHARS_PER_ESTIMATED_TOKEN))
            .sum()
    }

    fn compact_if_needed(&mut self) {
        let size = self.estimate_context_tokens();
        if size <= self.max_context_tokens {
            return;
        }

        warn!(
            agent_id = %self.agent_id,
            estimated_tokens = size,
            max_tokens = self.max_context_tokens,
            messages = self.messages.len(),
            "Context too large, compacting"
        );

        let selected = self.resolve_compaction_strategy(size);
        debug!(
            agent_id = %self.agent_id,
            configured = ?self.compaction_strategy,
            selected = ?selected,
            "Selected compaction strategy"
        );

        match selected {
            CompactionStrategy::Auto | CompactionStrategy::Default => {
                self.compact_with_profile(CompactionProfile::DEFAULT)
            }
            CompactionStrategy::Conservative => {
                self.compact_with_profile(CompactionProfile::CONSERVATIVE)
            }
            CompactionStrategy::Aggressive => {
                self.compact_with_profile(CompactionProfile::AGGRESSIVE)
            }
            CompactionStrategy::Custom {
                keep_recent,
                tool_result_chars_limit,
                assistant_content_limit,
                fallback_truncate_chars,
            } => {
                self.compact_with_custom_strategy(
                    keep_recent,
                    tool_result_chars_limit,
                    assistant_content_limit,
                    fallback_truncate_chars,
                );
            }
        }

        let new_size = self.estimate_context_tokens();
        debug!(
            agent_id = %self.agent_id,
            before = size,
            after = new_size,
            "Context compacted"
        );
    }

    fn resolve_compaction_strategy(&self, size: usize) -> CompactionStrategy {
        match &self.compaction_strategy {
            CompactionStrategy::Auto => self.select_dynamic_strategy(size),
            other => other.clone(),
        }
    }

    fn select_dynamic_strategy(&self, size: usize) -> CompactionStrategy {
        let total = self.messages.len().max(1);
        let overflow_ratio = size as f64 / self.max_context_tokens.max(1) as f64;
        let tool_count = self.messages.iter().filter(|m| matches!(m, ChatMessage::Tool { .. })).count();
        let assistant_count = self
            .messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .count();
        let tool_ratio = tool_count as f64 / total as f64;
        let assistant_ratio = assistant_count as f64 / total as f64;

        if overflow_ratio >= 1.8 || total >= 80 {
            return CompactionStrategy::Aggressive;
        }

        if tool_ratio >= 0.35 {
            return if overflow_ratio >= 1.25 {
                CompactionStrategy::Aggressive
            } else {
                CompactionStrategy::Default
            };
        }

        if assistant_ratio >= 0.45 && overflow_ratio < 1.2 {
            return CompactionStrategy::Conservative;
        }

        if overflow_ratio >= 1.35 {
            CompactionStrategy::Default
        } else {
            CompactionStrategy::Conservative
        }
    }

    fn compact_with_profile(&mut self, profile: CompactionProfile) {
        let total = self.messages.len();
        if total <= profile.keep_recent + 2 {
            self.truncate_all_tool_results(profile.fallback_truncate_chars);
            return;
        }

        let keep_after = total - profile.keep_recent;

        for i in 1..keep_after {
            match &self.messages[i] {
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } => {
                    if content.len() > profile.tool_result_chars_limit {
                        let summary = format!(
                            "[compacted: {} chars] {}",
                            content.len(),
                            safe_prefix(content, profile.tool_result_chars_limit.saturating_sub(50))
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
                } if content
                    .as_ref()
                    .is_some_and(|c| c.len() > profile.assistant_content_limit) =>
                {
                    let short = content
                        .as_ref()
                        .map(|c| truncate(c, profile.assistant_content_limit.saturating_sub(100)));
                    self.messages[i] = ChatMessage::Assistant {
                        content: short,
                        tool_calls: tool_calls.clone(),
                    };
                }
                ChatMessage::User { content } if profile.compress_user_messages && content.len() > 200 => {
                    let short = truncate(content, 150);
                    self.messages[i] = ChatMessage::User { content: short };
                }
                _ => {}
            }
        }
    }

    fn compact_with_custom_strategy(
        &mut self,
        keep_recent: usize,
        tool_result_chars_limit: usize,
        assistant_content_limit: usize,
        fallback_truncate_chars: usize,
    ) {
        self.compact_with_profile(CompactionProfile {
            keep_recent,
            tool_result_chars_limit,
            assistant_content_limit,
            fallback_truncate_chars,
            compress_user_messages: false,
        });
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

    /// Drain all pending background results and inject them as user messages.
    /// This mirrors Claude Code's behavior: when a background agent finishes,
    /// the parent agent is automatically notified with the result content.
    fn drain_background_results(&mut self) {
        let rx = match self.background_rx.as_mut() {
            Some(rx) => rx,
            None => return,
        };

        while let Ok(result) = rx.try_recv() {
            let kind_label = match result.kind {
                BackgroundResultKind::SubAgent => "subagent",
                BackgroundResultKind::AgentTeam => "agent team",
            };
            let notification = format!(
                "[Background {} '{}' completed — {} tokens]\n\n{}",
                kind_label, result.name, result.tokens_used, result.content,
            );
            info!(
                agent_id = %self.agent_id,
                background_agent = %result.name,
                tokens = result.tokens_used,
                "Background agent result injected into conversation"
            );
            self.messages.push(ChatMessage::user(notification));
        }
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}

/// Build a compact JSON preview of a tool result for the `ToolResult` event.
///
/// Unlike raw truncation, this extracts metadata fields and omits large body
/// fields like `content` / `stdout`, so event consumers (CLI display) can
/// reliably parse the preview.
fn build_result_preview(val: &serde_json::Value) -> String {
    let obj = match val.as_object() {
        Some(o) => o,
        None => return truncate(&val.to_string(), 300),
    };

    let mut preview = serde_json::Map::new();
    for (key, value) in obj {
        match key.as_str() {
            // Skip large body fields — include everything else
            "content" | "stdout" | "stderr" => {
                if let Some(s) = value.as_str() {
                    let lines = s.lines().count();
                    preview.insert(
                        key.clone(),
                        serde_json::Value::String(format!("[{} lines]", lines)),
                    );
                }
            }
            _ => {
                preview.insert(key.clone(), value.clone());
            }
        }
    }

    serde_json::to_string(&serde_json::Value::Object(preview)).unwrap_or_default()
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
