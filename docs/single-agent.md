# Single-Agent Usage

This document covers the two current single-agent paths:

- `AgentTeam::run_single(...)` for the simplest programmatic usage
- `AgentLoop` when you need direct control over prompts, messages, and tools

## `AgentTeam::run_single(...)`

`AgentTeam::run_single(...)` is the easiest way to execute one tool-using agent against a repository.

```rust
use agent_sdk::{AgentConfig, AgentTeam, LlmConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
        .source_root(".")
        .work_dir(".")
        .run_single("Find all task-related modules and explain them")
        .await?;

    println!("{}", result.final_content);
    Ok(())
}
```

Current built-in toolset for `run_single(...)`:

- `read_file`
- `write_file`
- `list_directory`
- `search_files`
- `run_command`

Result type:

```rust
pub struct AgentLoopResult {
    pub final_content: String,
    pub messages: Vec<ChatMessage>,
    pub total_tokens: u64,
    pub iterations: usize,
    pub tool_calls_count: usize,
}
```

## `AgentLoop`

Use `AgentLoop` directly when you need to control:

- the system prompt
- the tool registry
- the initial message history
- event streaming per loop
- context window compaction settings

### Create A Tool Registry

```rust
use std::sync::Arc;

use agent_sdk::tools::command_tools::RunCommandTool;
use agent_sdk::tools::fs_tools::{ListDirectoryTool, ReadFileTool, WriteFileTool};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::tools::search_tools::SearchFilesTool;

let source_root = std::path::PathBuf::from(".");
let work_dir = std::path::PathBuf::from(".");

let mut tools = ToolRegistry::new();
tools.register(Arc::new(ReadFileTool {
    source_root: source_root.clone(),
    work_dir: work_dir.clone(),
}));
tools.register(Arc::new(WriteFileTool {
    work_dir: work_dir.clone(),
}));
tools.register(Arc::new(ListDirectoryTool {
    source_root: source_root.clone(),
    work_dir: work_dir.clone(),
}));
tools.register(Arc::new(SearchFilesTool {
    source_root: source_root.clone(),
}));
tools.register(Arc::new(RunCommandTool::with_defaults(work_dir.clone())));
```

### Run An Agent Loop

```rust
use std::sync::Arc;

use agent_sdk::{create_client, AgentLoop, ChatMessage, LlmConfig};
use uuid::Uuid;

let llm = Arc::new(create_client(&LlmConfig::default())?);

let mut loop_ = AgentLoop::new(
    Uuid::new_v4(),
    llm,
    tools,
    "You are a Rust code assistant.".to_string(),
    20,
);

let result = loop_.run("Inspect the project and summarize risks".to_string()).await?;
println!("{}", result.final_content);
```

### Resume A Multi-Turn Conversation

If you want to keep prior messages:

```rust
use agent_sdk::{AgentLoop, ChatMessage};
use uuid::Uuid;

let messages = vec![
    ChatMessage::system("You are a careful code reviewer."),
    ChatMessage::user("Inspect src/lib.rs"),
    ChatMessage::assistant("I will inspect the crate root and public exports."),
];

let mut loop_ = AgentLoop::with_messages(
    Uuid::new_v4(),
    llm,
    tools,
    messages,
    20,
);
```

### Tune Context Compaction

`AgentLoop` estimates context size using a `4 chars ~= 1 token` heuristic.

```rust
let loop_ = AgentLoop::new(
    Uuid::new_v4(),
    llm,
    tools,
    "You are concise.".to_string(),
    20,
)
.with_max_context_tokens(50_000);
```

When the conversation grows too large, the current implementation compacts older assistant and tool-result messages rather than failing immediately.

## Background Agent Results

When subagents or agent teams run in background mode, their results are delivered back to the parent agent's conversation automatically. Set up the background result channel:

```rust
use agent_sdk::agent::agent_loop::BackgroundResult;

let (bg_tx, bg_rx) = tokio::sync::mpsc::unbounded_channel::<BackgroundResult>();
loop_.set_background_rx(bg_rx);

// Pass bg_tx to SpawnSubAgentTool and SpawnAgentTeamTool via their
// `background_tx` field. When a background agent completes, its result
// is injected as a user message before the next LLM call.
```

This mirrors Claude Code's behavior: the parent agent continues working while background agents run concurrently, and is automatically notified with results when they finish.

## Event Streaming

Attach an event sink to observe loop activity:

```rust
use agent_sdk::AgentEvent;

let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
loop_.set_event_sink(tx);

tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        println!("{:?}", event);
    }
});
```

Common single-agent events:

- `Thinking`
- `ToolCall`
- `ToolResult`

## Tool Contracts

### `read_file`

Arguments:

```json
{ "path": "src/lib.rs", "offset": 0, "max_lines": 200 }
```

Notes:

- Reads from `source_root` first, then `work_dir`
- Rejects paths that escape both allowed roots
- Large files can be paged with `offset` and `max_lines`

### `write_file`

Arguments:

```json
{ "path": "notes/output.md", "content": "# title\nbody" }
```

Notes:

- Always writes into `work_dir`
- Creates parent directories as needed
- Replaces the full file content

### `list_directory`

Arguments:

```json
{ "path": "." }
```

Notes:

- Lists from `source_root`
- Returns `name` and `type` for each entry

### `search_files`

Arguments:

```json
{ "file_pattern": "src/**/*.rs", "content_pattern": "AgentTeam", "max_results": 20 }
```

Notes:

- You may provide `file_pattern`, `content_pattern`, or both
- Content search is plain substring matching
- Search scope is rooted at `source_root`

### `run_command`

Arguments:

```json
{ "command": "cargo check", "timeout_secs": 30 }
```

Notes:

- Executes in `work_dir`
- `RunCommandTool::with_defaults(...)` is unrestricted by default
- command execution is limited only by the current environment and process permissions
