# Extending The SDK

This SDK is designed around a few extension seams:

- `Tool`
- `PromptBuilder`
- `Hook`
- `LlmClient`

## Custom Tools

Implement `Tool` to expose new actions to an agent:

```rust
use async_trait::async_trait;
use serde_json::json;

use agent_sdk::{SdkResult, Tool, ToolDefinition};

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "echo".to_string(),
            description: "Echo a string.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        Ok(json!({
            "echo": arguments["text"].as_str().unwrap_or("")
        }))
    }
}
```

Register it in a `ToolRegistry`:

```rust
use std::sync::Arc;

use agent_sdk::tools::registry::ToolRegistry;

let mut registry = ToolRegistry::new();
registry.register(Arc::new(EchoTool));
```

## Custom Prompt Builder

Use a custom `PromptBuilder` when generic task prompts are not enough:

```rust
use agent_sdk::traits::prompt_builder::PromptBuilder;
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::Task;

struct ApiReviewPromptBuilder;

impl PromptBuilder for ApiReviewPromptBuilder {
    fn build_system_prompt(
        &self,
        task: &Task,
        source_root: &std::path::Path,
        work_dir: &std::path::Path,
    ) -> String {
        format!(
            "You review public Rust APIs.\nSource: {}\nWork: {}\nTask: {}",
            source_root.display(),
            work_dir.display(),
            task.title
        )
    }

    fn build_user_message(&self, task: &Task) -> String {
        format!("Complete this API review task:\n{}", task.description)
    }

    fn customize_tools(&self, _task: &Task, registry: ToolRegistry) -> ToolRegistry {
        registry
    }
}
```

Attach it with:

```rust
use std::sync::Arc;

let team = agent_sdk::AgentTeam::new(
    agent_sdk::LlmConfig::default(),
    agent_sdk::AgentConfig::default(),
)
.prompt_builder(Arc::new(ApiReviewPromptBuilder));
```

## Hooks

Hooks let you veto important lifecycle actions:

```rust
use agent_sdk::{Hook, HookEvent, HookResult};

struct NoEmptyTasks;

impl Hook for NoEmptyTasks {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        match event {
            HookEvent::TaskCreated { task } if task.description.trim().is_empty() => {
                HookResult::Reject {
                    feedback: "Task descriptions must not be empty".to_string(),
                }
            }
            _ => HookResult::Continue,
        }
    }
}
```

Attach with:

```rust
let team = agent_sdk::AgentTeam::new(
    agent_sdk::LlmConfig::default(),
    agent_sdk::AgentConfig::default(),
)
.add_hook(NoEmptyTasks);
```

## Custom LLM Client

The crate-level factory is:

```rust
let client = agent_sdk::create_client(&llm_config)?;
```

If you need a different backend, implement `LlmClient`:

```rust
use async_trait::async_trait;

use agent_sdk::{ChatMessage, LlmClient, SdkResult, ToolDefinition};

struct MockClient;

#[async_trait]
impl LlmClient for MockClient {
    async fn ask(&self, _system: &str, _user_message: &str) -> SdkResult<(String, u64)> {
        Ok(("ok".to_string(), 0))
    }

    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, u64)> {
        Ok((ChatMessage::assistant("done"), 0))
    }
}
```

Inject it into `AgentTeam`:

```rust
use std::sync::Arc;

let team = agent_sdk::AgentTeam::new(
    agent_sdk::LlmConfig::default(),
    agent_sdk::AgentConfig::default(),
)
.llm_client(Arc::new(MockClient));
```

## MCP Servers

The SDK can load external tools from [Model Context Protocol](https://modelcontextprotocol.io) servers. At startup the CLI reads `.agent/mcp.json` from the working directory, spawns each declared server as a child process, performs the JSON-RPC `initialize` handshake over its stdio, calls `tools/list`, and registers every advertised tool with the agent.

Manifest format:

```json
{
  "servers": [
    {
      "name": "weather",
      "command": "npx",
      "args": ["-y", "@example/weather-mcp"],
      "env": { "API_KEY": "..." }
    }
  ]
}
```

Each registered tool is namespaced as `mcp__<server_name>__<tool_name>` to avoid collisions with built-in tools and with tools from other servers. Its JSON Schema is taken verbatim from the server's `inputSchema`. The tool's return value is shaped as:

```json
{ "content": "<joined text blocks>", "is_error": false }
```

If a server fails to spawn, handshake, or list tools, it is logged and skipped — other servers continue to load. Servers are kept alive for the duration of the CLI process and killed on exit.

To use MCP programmatically, `agent_sdk::mcp` re-exports `McpClient`, `McpConfig`, `McpServerSpec`, and `StdioTransport`. The transport is generic over `AsyncRead + AsyncWrite`, so tests can drive the client through `tokio::io::duplex` without spawning a real process.

## Event Consumers

The SDK already emits structured `AgentEvent` values. Build adapters around that stream for:

- logging
- metrics
- TUI or GUI updates
- audit trails

Example:

```rust
use agent_sdk::AgentEvent;

let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::TaskCompleted { task_id, .. } => {
                println!("task done: {}", task_id);
            }
            _ => {}
        }
    }
});
```

## Testing Strategy

For library integrations, the safest pattern is:

1. use a mock `LlmClient`
2. use a temporary `work_dir`
3. register only the tools your test needs
4. assert on generated files, task JSON, and emitted events
