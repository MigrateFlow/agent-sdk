//! Integration tests for the Pre/Post LLM and tool hooks, plus the
//! `CostTracker` JSONL writer.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use agent_sdk::agent::agent_loop::AgentLoop;
use agent_sdk::agent::cost::{CostRecord, CostTracker};
use agent_sdk::agent::hooks::{Hook, HookEvent, HookRegistry, HookResult};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::traits::llm_client::LlmClient;
use agent_sdk::traits::tool::{Tool, ToolDefinition};
use agent_sdk::types::chat::{ChatMessage, FunctionCall, ToolCall};
use agent_sdk::types::usage::TokenUsage;
use agent_sdk::AgentEvent;
use agent_sdk::SdkResult;

// ─── Mock LLM ────────────────────────────────────────────────────────────────

/// A scripted LLM that returns a single assistant message with one tool call
/// on the first invocation, then a plain assistant answer on subsequent calls.
struct ScriptedLlm {
    calls: Mutex<usize>,
}

impl ScriptedLlm {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    async fn ask(&self, _system: &str, _user: &str) -> SdkResult<(String, u64)> {
        unreachable!("ask() not used by AgentLoop")
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
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, TokenUsage)> {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        let call_n = *n;
        drop(n);

        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 10,
            cache_read_input_tokens: 5,
            model: "claude-opus-4-6".into(),
        };

        if call_n == 1 {
            let tool_call = ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "noop".into(),
                    arguments: json!({"x": 1}).to_string(),
                },
            };
            Ok((
                ChatMessage::Assistant {
                    content: Some("Using the noop tool.".into()),
                    tool_calls: vec![tool_call],
                },
                usage,
            ))
        } else {
            Ok((
                ChatMessage::Assistant {
                    content: Some("Done.".into()),
                    tool_calls: vec![],
                },
                usage,
            ))
        }
    }
}

// ─── Mock tool ────────────────────────────────────────────────────────────────

struct NoopTool;

#[async_trait]
impl Tool for NoopTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "noop".into(),
            description: "No-op tool used by the hook/cost integration tests.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "x": { "type": "number" } }
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> SdkResult<serde_json::Value> {
        Ok(json!({"ok": true, "echo": args}))
    }
}

// ─── Hook observers ──────────────────────────────────────────────────────────

#[derive(Default)]
struct ObserverState {
    events: Vec<String>,
}

struct Observer(Arc<Mutex<ObserverState>>);

impl Hook for Observer {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        let label = match event {
            HookEvent::PreLlmRequest { .. } => "pre_llm_request",
            HookEvent::PostLlmRequest { .. } => "post_llm_request",
            HookEvent::PreToolCall { .. } => "pre_tool_call",
            HookEvent::PostToolCall { .. } => "post_tool_call",
            HookEvent::TeammateIdle { .. } => "teammate_idle",
            HookEvent::TaskCreated { .. } => "task_created",
            HookEvent::TaskCompleted { .. } => "task_completed",
        };
        self.0.lock().unwrap().events.push(label.to_string());
        HookResult::Continue
    }
}

/// Always rejects PreToolCall with the given feedback.
struct RejectingHook(String);

impl Hook for RejectingHook {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        match event {
            HookEvent::PreToolCall { .. } => HookResult::Reject {
                feedback: self.0.clone(),
            },
            _ => HookResult::Continue,
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cost_tracker_writes_jsonl_and_emits_hook_order() {
    let tmp = tempfile::TempDir::new().expect("temp dir");

    let observer_state = Arc::new(Mutex::new(ObserverState::default()));
    let mut registry = HookRegistry::new();
    registry.add(Observer(observer_state.clone()));
    // Register CostTracker as a hook — it listens on PostLlmRequest and
    // writes JSONL rows including cache metrics.
    registry.add(CostTracker::new(tmp.path()).expect("tracker"));
    let hooks = Arc::new(registry);

    let mut tool_reg = ToolRegistry::new();
    tool_reg.register(Arc::new(NoopTool));

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();

    let mut loop_ = AgentLoop::new(
        Uuid::new_v4(),
        Arc::new(ScriptedLlm::new()),
        tool_reg,
        "system".into(),
        5,
    )
    .with_hooks(hooks);
    loop_.set_event_sink(event_tx);

    let result = loop_.run("go".into()).await.expect("run ok");
    assert_eq!(result.iterations, 2, "one tool turn + final answer");
    assert!(result.tool_calls_count >= 1);

    // Drop the sender side so the receiver closes; drain anyway for safety.
    while event_rx.try_recv().is_ok() {}

    // (a) cost.jsonl exists and has exactly two rows (one per LLM request).
    let path = tmp.path().join("cost.jsonl");
    assert!(path.exists(), "cost.jsonl should be written");
    let records = CostTracker::read_all(&path).expect("read cost");
    assert_eq!(records.len(), 2, "one record per LLM call");
    let r = &records[0];
    assert_eq!(r.model, "claude-opus-4-6");
    assert_eq!(r.tokens_in, 100);
    assert_eq!(r.tokens_out, 50);
    assert_eq!(r.cache_in, 10);
    assert_eq!(r.cache_read, 5);
    // claude-opus-4-6: input=$15, output=$75, cache_write=$18.75, cache_read=$1.50
    // per 1K. With 100/50/10/5 -> 0.1*15 + 0.05*75 + 0.01*18.75 + 0.005*1.5
    // = 1.5 + 3.75 + 0.1875 + 0.0075 = 5.445
    assert!(
        (r.estimated_usd - 5.445).abs() < 1e-6,
        "estimated_usd = {}",
        r.estimated_usd
    );
    assert!(!r.timestamp.is_empty(), "timestamp should be populated");

    // (b) Hook event ordering for the first iteration.
    let events = observer_state.lock().unwrap().events.clone();
    let first_iter: Vec<&str> = events
        .iter()
        .map(|s| s.as_str())
        .take(4)
        .collect();
    assert_eq!(
        first_iter,
        vec![
            "pre_llm_request",
            "post_llm_request",
            "pre_tool_call",
            "post_tool_call",
        ],
        "ordering of hooks during the first tool-invoking iteration"
    );

    // Second LLM request fires after the tool result is added back.
    assert_eq!(
        events.iter().filter(|s| *s == "pre_llm_request").count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|s| *s == "post_llm_request")
            .count(),
        2
    );
}

#[tokio::test]
async fn pre_tool_call_rejection_short_circuits_with_feedback() {
    let mut registry = HookRegistry::new();
    registry.add(RejectingHook("tool not allowed here".into()));
    let hooks = Arc::new(registry);

    let mut tool_reg = ToolRegistry::new();
    tool_reg.register(Arc::new(NoopTool));

    let mut loop_ = AgentLoop::new(
        Uuid::new_v4(),
        Arc::new(ScriptedLlm::new()),
        tool_reg,
        "system".into(),
        5,
    )
    .with_hooks(hooks);

    let result = loop_.run("go".into()).await.expect("run ok");

    // The synthesized tool-result message should contain the hook feedback.
    let tool_msg = result
        .messages
        .iter()
        .find_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("a synthesized tool_result message");

    let payload: serde_json::Value =
        serde_json::from_str(&tool_msg).expect("synthesized result is valid JSON");
    assert_eq!(payload["rejected_by_hook"], json!(true));
    assert_eq!(payload["feedback"], json!("tool not allowed here"));

    // The final assistant message still arrives because the loop continues
    // after the synthesized tool_result.
    let final_content = &result.final_content;
    assert!(
        !final_content.is_empty(),
        "final assistant content should still be produced"
    );
}

#[test]
fn read_all_handles_missing_file() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let path = tmp.path().join("nonexistent-cost.jsonl");
    let records: Vec<CostRecord> = CostTracker::read_all(&path).expect("ok for missing");
    assert!(records.is_empty());
}
