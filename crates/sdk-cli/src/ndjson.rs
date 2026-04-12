use serde::Serialize;

/// NDJSON wire protocol events for programmatic consumption (`--json` mode).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NdjsonEvent {
    Started { tools: Vec<String> },
    Thinking { content: String, iteration: usize },
    ToolCall { tool_call_id: String, tool_name: String, arguments: String, iteration: usize },
    ToolResult { tool_call_id: String, tool_name: String, content: String, iteration: usize },
    TextDelta { content: String },
    Completed { final_content: String, tokens_used: u64, iterations: usize, tool_calls: usize },
    Failed { error: String },
    // Team/subagent events for programmatic consumers
    TeamSpawned { teammate_count: usize },
    SubagentSpawned { name: String, description: String },
    SubagentProgress { name: String, iteration: usize, max_turns: usize, current_tool: Option<String>, tokens_so_far: u64 },
    SubagentCompleted { name: String, tokens_used: u64, iterations: usize, tool_calls: usize },
    SubagentFailed { name: String, error: String },
    TaskStarted { name: String, title: String },
    TaskCompleted { name: String, title: String, tokens_used: u64 },
    TaskFailed { name: String, title: String, error: String },
    #[allow(dead_code)]
    PlanModeChanged { mode: String },
}

pub fn emit_ndjson(event: &NdjsonEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{}", json);
    }
}
