use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::error::{AgentId, SdkError, SdkResult};
use crate::types::chat::ChatMessage;
use crate::traits::llm_client::LlmClient;
use crate::tools::registry::ToolRegistry;

use super::events::AgentEvent;
use super::hooks::{HookEvent, HookRegistry, HookResult};

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
    /// A compaction summary produced by an off-loop summarization subagent.
    /// `target_window_start` / `target_window_end` mark the range of messages
    /// that should be replaced by the summary (half-open interval
    /// `[start, end)`). `window_digest` is a hash of the original window used
    /// to detect if intervening writes have shifted the target range — in
    /// that case the summary is dropped.
    CompactionSummary {
        target_window_start: usize,
        target_window_end: usize,
        window_digest: u64,
        strategy: String,
    },
}

const CHARS_PER_ESTIMATED_TOKEN: usize = 4;
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 200_000;
const MAX_TOOL_RESULT_CHARS: usize = 12_000;
const COMPACT_KEEP_RECENT: usize = 10;

/// System prompt used when spawning a background summarization subagent.
const SUMMARIZER_SYSTEM: &str =
    "You are a conversation summarizer. Produce a concise, faithful summary of \
     the provided conversation window. Preserve decisions, intents, entities, \
     file paths, tool results, and open questions. Prefer bullet points. \
     Do not invent information. Do not include preambles like \"Here is a summary\".";

/// Overflow ratio above which summarization is warranted even when the
/// configured strategy is not explicitly aggressive.
const SUMMARIZATION_OVERFLOW_RATIO: f64 = 1.8;

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
    /// Sender paired with `background_rx`; retained so the loop can dispatch
    /// its own background work (e.g. memory consolidation summarization).
    background_tx: Option<UnboundedSender<BackgroundResult>>,
    /// Guards against spawning a second in-flight compaction-summary task
    /// while one is still running. Set to `true` when a summarization task
    /// is spawned; cleared by that task when it terminates (successfully or
    /// not).
    compaction_in_flight: Arc<AtomicBool>,
    /// Optional hook registry. When set, the loop evaluates hooks around LLM
    /// requests and tool calls, and gives `PreToolCall` hooks the ability to
    /// veto a tool invocation.
    hooks: Option<Arc<HookRegistry>>,
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
            background_tx: None,
            compaction_in_flight: Arc::new(AtomicBool::new(false)),
            hooks: None,
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
            background_tx: None,
            compaction_in_flight: Arc::new(AtomicBool::new(false)),
            hooks: None,
        }
    }

    /// Attach a hook registry. Hooks fire for `PreLlmRequest`,
    /// `PostLlmRequest`, `PreToolCall`, and `PostToolCall`.
    pub fn with_hooks(mut self, hooks: Arc<HookRegistry>) -> Self {
        self.hooks = Some(hooks);
        self
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

    /// Install both ends of the background-result channel so the loop can
    /// dispatch its own off-loop work (e.g. memory consolidation summaries).
    /// The caller receives a clone of the sender for use in tools that also
    /// need to post background results (subagents, teams, etc.).
    pub fn install_background_channel(&mut self) -> UnboundedSender<BackgroundResult> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.background_rx = Some(rx);
        self.background_tx = Some(tx.clone());
        tx
    }

    /// Provide the loop with a sender clone for dispatching its own background
    /// work. Use this when the channel was created externally and passed in
    /// via [`set_background_rx`].
    pub fn set_background_tx(&mut self, tx: UnboundedSender<BackgroundResult>) {
        self.background_tx = Some(tx);
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

            // Pre-LLM hook
            self.evaluate_hook(HookEvent::PreLlmRequest {
                message_count: self.messages.len(),
            });

            let llm_started = Instant::now();
            let (response, usage) = self
                .llm_client
                .chat_with_usage(&self.messages, &tool_defs)
                .await?;
            let llm_elapsed_ms = llm_started.elapsed().as_millis() as u64;
            let tokens = usage.input_tokens + usage.output_tokens;
            self.total_tokens += tokens;

            // Post-LLM hook — carries the full usage breakdown so hooks like
            // CostTracker can record cache metrics.
            self.evaluate_hook(HookEvent::PostLlmRequest {
                tokens_in: usage.input_tokens,
                tokens_out: usage.output_tokens,
                cache_in: usage.cache_creation_input_tokens,
                cache_read: usage.cache_read_input_tokens,
                duration_ms: llm_elapsed_ms,
                model: usage.model.clone(),
            });

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

                    // Evaluate PreToolCall hooks. A rejection synthesizes a
                    // tool_result with the feedback instead of dispatching.
                    //
                    // Each element is either:
                    //   - Ok((id, name, args)) — allowed, will be dispatched
                    //   - Err((id, name, feedback)) — rejected, synthesize result
                    enum ToolPlan {
                        Run {
                            id: String,
                            name: String,
                            args: serde_json::Value,
                        },
                        Reject {
                            id: String,
                            name: String,
                            feedback: String,
                        },
                    }

                    let mut plan: Vec<ToolPlan> = Vec::with_capacity(tool_calls.len());
                    for tool_call in tool_calls {
                        let name = tool_call.function.name.clone();
                        let args: serde_json::Value =
                            serde_json::from_str(&tool_call.function.arguments)
                                .unwrap_or_default();
                        let id = tool_call.id.clone();

                        let decision = self.evaluate_hook(HookEvent::PreToolCall {
                            name: name.clone(),
                            args: args.clone(),
                        });

                        match decision {
                            HookResult::Reject { feedback } => {
                                plan.push(ToolPlan::Reject { id, name, feedback });
                            }
                            HookResult::Continue => {
                                plan.push(ToolPlan::Run { id, name, args });
                            }
                        }
                    }

                    // Dispatch allowed tool calls in parallel; rejected ones
                    // resolve immediately with synthesized feedback.
                    let tools_ref = &self.tools;
                    let futures: Vec<_> = plan
                        .into_iter()
                        .map(|entry| async move {
                            match entry {
                                ToolPlan::Run { id, name, args } => {
                                    let started = Instant::now();
                                    let args_for_hook = args.clone();
                                    let result = tools_ref.execute(&name, args).await;
                                    let duration_ms = started.elapsed().as_millis() as u64;
                                    (id, name, args_for_hook, Ok(result), duration_ms)
                                }
                                ToolPlan::Reject { id, name, feedback } => {
                                    (id, name, serde_json::Value::Null, Err(feedback), 0)
                                }
                            }
                        })
                        .collect();

                    let results = futures_util::future::join_all(futures).await;

                    for (call_id, tool_name, args, outcome, duration_ms) in results {
                        let (result_content, result_preview) = match outcome {
                            Ok(Ok(val)) => {
                                let preview = build_result_preview(&val);
                                let full = serde_json::to_string(&val).unwrap_or_default();
                                (truncate_tool_result(&full), preview)
                            }
                            Ok(Err(e)) => {
                                let err = serde_json::json!({"error": e.to_string()}).to_string();
                                (err.clone(), err)
                            }
                            Err(feedback) => {
                                // Hook rejection: synthesize a tool_result
                                // carrying the feedback so the model sees it.
                                let payload =
                                    serde_json::json!({"rejected_by_hook": true, "feedback": feedback})
                                        .to_string();
                                (payload.clone(), payload)
                            }
                        };

                        self.emit(AgentEvent::ToolResult {
                            agent_id: self.agent_id,
                            name: self.agent_name.clone(),
                            tool_name: tool_name.clone(),
                            result_preview: result_preview.clone(),
                            iteration,
                        });

                        // PostToolCall hook (including rejected calls so
                        // observers see every decision).
                        self.evaluate_hook(HookEvent::PostToolCall {
                            name: tool_name,
                            args,
                            result_preview,
                            duration_ms,
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

        // When the selected strategy warrants expensive LLM-backed
        // summarization, we first run an inexpensive truncation pass so the
        // immediate next LLM call stays within budget, THEN dispatch a
        // background task that will produce a higher-quality summary of the
        // resulting window. The summary is spliced in on a later
        // `drain_background_results()` tick.
        //
        // Order matters: we snapshot/hash AFTER the inline truncation so the
        // digest check at splice time sees a consistent window. Otherwise
        // the inline mutation would invalidate the pending summary.
        if self.should_summarize_in_background(&selected, size) {
            self.compact_with_profile(CompactionProfile::AGGRESSIVE);
            let after_inline = self.estimate_context_tokens();
            debug!(
                agent_id = %self.agent_id,
                before = size,
                after = after_inline,
                "Context compacted (inline truncation)"
            );
            if self.try_spawn_summarization(&selected) {
                debug!(
                    agent_id = %self.agent_id,
                    after_tokens = after_inline,
                    "Background compaction summarization dispatched"
                );
            }
            return;
        }

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

    /// Decide whether the strategy warrants an LLM-backed summarization.
    /// We summarize when the configured/selected strategy is `Aggressive`
    /// OR overflow is severe enough that a summary pays for itself.
    fn should_summarize_in_background(
        &self,
        selected: &CompactionStrategy,
        size: usize,
    ) -> bool {
        let overflow = size as f64 / self.max_context_tokens.max(1) as f64;
        matches!(selected, CompactionStrategy::Aggressive)
            || overflow >= SUMMARIZATION_OVERFLOW_RATIO
    }

    /// Attempt to spawn the background summarization task. Returns `false` if
    /// the loop can't dispatch (no sender, another task already running,
    /// window too small, or LLM client unavailable).
    fn try_spawn_summarization(&self, selected: &CompactionStrategy) -> bool {
        let tx = match self.background_tx.as_ref() {
            Some(tx) => tx.clone(),
            None => {
                debug!(
                    agent_id = %self.agent_id,
                    "No background sender installed; skipping async summarization"
                );
                return false;
            }
        };

        // Only one in-flight summarization at a time.
        if self
            .compaction_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            debug!(
                agent_id = %self.agent_id,
                "Compaction summarization already in flight; skipping spawn"
            );
            return false;
        }

        let window = match self.select_summarize_window() {
            Some(w) => w,
            None => {
                self.compaction_in_flight.store(false, Ordering::Release);
                return false;
            }
        };

        let serialized = match serde_json::to_string(&self.messages[window.start..window.end]) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    agent_id = %self.agent_id,
                    error = %e,
                    "Failed to serialize compaction window; skipping"
                );
                self.compaction_in_flight.store(false, Ordering::Release);
                return false;
            }
        };

        let digest = hash_messages(&self.messages[window.start..window.end]);
        let strategy_label = format!("{:?}", selected);
        let llm_client = self.llm_client.clone();
        let in_flight = self.compaction_in_flight.clone();
        let agent_id = self.agent_id;
        let start = window.start;
        let end = window.end;

        info!(
            agent_id = %agent_id,
            window_start = start,
            window_end = end,
            messages_in_window = end - start,
            "Spawning background compaction summarization"
        );

        tokio::spawn(async move {
            let user_msg = format!(
                "Summarize the following conversation window. Return a single \
                 block of text, no preamble.\n\n<window>\n{}\n</window>",
                serialized
            );
            let result = llm_client.ask(SUMMARIZER_SYSTEM, &user_msg).await;
            match result {
                Ok((summary, tokens_used)) => {
                    let payload = BackgroundResult {
                        name: "compaction-summary".to_string(),
                        kind: BackgroundResultKind::CompactionSummary {
                            target_window_start: start,
                            target_window_end: end,
                            window_digest: digest,
                            strategy: strategy_label,
                        },
                        content: summary,
                        tokens_used,
                    };
                    if let Err(e) = tx.send(payload) {
                        warn!(
                            agent_id = %agent_id,
                            error = %e,
                            "Failed to deliver compaction summary (receiver dropped)"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        agent_id = %agent_id,
                        error = %e,
                        "Background compaction summarization failed"
                    );
                }
            }
            in_flight.store(false, Ordering::Release);
        });

        true
    }

    /// Choose the window of messages to summarize.
    /// Returns `[start, end)` with `start >= 1` (never touches system message)
    /// and `end <= len - keep_recent`. Returns `None` if the window would be
    /// empty or degenerate.
    fn select_summarize_window(&self) -> Option<SummarizeWindow> {
        let total = self.messages.len();
        let keep = COMPACT_KEEP_RECENT;
        if total <= keep + 2 {
            return None;
        }
        let start = 1usize;
        let end = total - keep;
        if end <= start {
            return None;
        }
        Some(SummarizeWindow { start, end })
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
    /// Compaction summaries are spliced into the message history in place of
    /// the summarized window rather than appended.
    fn drain_background_results(&mut self) {
        // Collect first, then process. This avoids holding `&mut rx` while we
        // mutate `self.messages`.
        let mut pending: Vec<BackgroundResult> = Vec::new();
        if let Some(rx) = self.background_rx.as_mut() {
            while let Ok(result) = rx.try_recv() {
                pending.push(result);
            }
        }

        for result in pending {
            let BackgroundResult { name, kind, content, tokens_used } = result;
            match kind {
                BackgroundResultKind::CompactionSummary {
                    target_window_start,
                    target_window_end,
                    window_digest,
                    strategy,
                } => {
                    self.apply_compaction_summary(
                        content,
                        target_window_start,
                        target_window_end,
                        window_digest,
                        strategy,
                    );
                }
                BackgroundResultKind::SubAgent | BackgroundResultKind::AgentTeam => {
                    let kind_label = match kind {
                        BackgroundResultKind::SubAgent => "subagent",
                        BackgroundResultKind::AgentTeam => "agent team",
                        BackgroundResultKind::CompactionSummary { .. } => unreachable!(),
                    };
                    let notification = format!(
                        "[Background {} '{}' completed — {} tokens]\n\n{}",
                        kind_label, name, tokens_used, content,
                    );
                    info!(
                        agent_id = %self.agent_id,
                        background_agent = %name,
                        tokens = tokens_used,
                        "Background agent result injected into conversation"
                    );
                    self.messages.push(ChatMessage::user(notification));
                }
            }
        }
    }

    fn apply_compaction_summary(
        &mut self,
        summary: String,
        start: usize,
        end: usize,
        expected_digest: u64,
        strategy: String,
    ) {
        // Validate the splice is still sane against the current history.
        if start == 0 || end > self.messages.len() || end <= start {
            warn!(
                agent_id = %self.agent_id,
                start, end, len = self.messages.len(),
                "Compaction summary indices out of range; dropping"
            );
            return;
        }

        // Confirm the window still matches what we summarized. If not, drop
        // the summary — splicing stale content over fresher messages would
        // corrupt history.
        let actual_digest = hash_messages(&self.messages[start..end]);
        if actual_digest != expected_digest {
            warn!(
                agent_id = %self.agent_id,
                "Compaction summary window shifted under us; dropping summary"
            );
            return;
        }

        let messages_before = self.messages.len();
        let chars_before: usize = self.messages[start..end].iter().map(|m| m.char_len()).sum();

        let replacement = ChatMessage::system(format!(
            "[Memory consolidation summary of messages {}..{}]\n\n{}",
            start, end, summary
        ));
        self.messages.splice(start..end, std::iter::once(replacement));

        let messages_after = self.messages.len();
        let chars_after = self.messages[start].char_len();
        let tokens_saved = chars_before
            .saturating_sub(chars_after)
            .div_ceil(CHARS_PER_ESTIMATED_TOKEN) as u64;

        info!(
            agent_id = %self.agent_id,
            messages_before,
            messages_after,
            tokens_saved,
            strategy = %strategy,
            "Compaction summary spliced into history"
        );

        self.emit(AgentEvent::MemoryCompacted {
            strategy,
            messages_before,
            messages_after,
            tokens_saved,
        });
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }

    /// Evaluate all configured hooks for `event`. Returns the first `Reject`
    /// encountered (or `Continue` when no registry is attached). Mirrors the
    /// short-circuit semantics of `HookRegistry::evaluate`.
    fn evaluate_hook(&self, event: HookEvent) -> HookResult {
        match &self.hooks {
            Some(registry) => {
                let result = registry.evaluate(&event);
                if let HookResult::Reject { feedback } = &result {
                    let event_name = hook_event_name(&event);
                    self.emit(AgentEvent::HookRejected {
                        event_name: event_name.to_string(),
                        feedback: feedback.clone(),
                    });
                }
                result
            }
            None => HookResult::Continue,
        }
    }
}

fn hook_event_name(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::TeammateIdle { .. } => "teammate_idle",
        HookEvent::TaskCreated { .. } => "task_created",
        HookEvent::TaskCompleted { .. } => "task_completed",
        HookEvent::PreToolCall { .. } => "pre_tool_call",
        HookEvent::PostToolCall { .. } => "post_tool_call",
        HookEvent::PreLlmRequest { .. } => "pre_llm_request",
        HookEvent::PostLlmRequest { .. } => "post_llm_request",
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

#[derive(Debug, Clone, Copy)]
struct SummarizeWindow {
    start: usize,
    end: usize,
}

/// Produce a stable digest for a slice of messages, used to detect whether
/// a compaction summary's target window has shifted under intervening
/// writes. Uses `DefaultHasher` which is stable within a single process run.
fn hash_messages(messages: &[ChatMessage]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for msg in messages {
        match msg {
            ChatMessage::System { content } => {
                "S".hash(&mut hasher);
                content.hash(&mut hasher);
            }
            ChatMessage::User { content } => {
                "U".hash(&mut hasher);
                content.hash(&mut hasher);
            }
            ChatMessage::Assistant { content, tool_calls } => {
                "A".hash(&mut hasher);
                content.as_deref().unwrap_or("").hash(&mut hasher);
                for tc in tool_calls {
                    tc.id.hash(&mut hasher);
                    tc.function.name.hash(&mut hasher);
                    tc.function.arguments.hash(&mut hasher);
                }
            }
            ChatMessage::Tool { tool_call_id, content } => {
                "T".hash(&mut hasher);
                tool_call_id.hash(&mut hasher);
                content.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
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
