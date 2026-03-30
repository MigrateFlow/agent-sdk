# agent-sdk

Orchestrate teams of LLM-powered agents in Rust. Coordinate multiple agent instances working together — one session acts as the team lead, coordinating work and assigning tasks. Teammates work independently, each with its own context, and communicate directly with each other.

Detailed usage docs live in [docs/README.md](/Users/ThangLT4/Desktop/code/rust-agent-sdk/docs/README.md).

## When to use agent teams

Agent teams are most effective when parallel exploration adds real value:

- **Research and review**: multiple teammates investigate different aspects simultaneously, then share and challenge findings
- **New modules or features**: teammates each own a separate piece without stepping on each other
- **Debugging with competing hypotheses**: teammates test different theories in parallel
- **Cross-layer coordination**: changes spanning frontend, backend, and tests — each owned by a different teammate

Agent teams use more tokens than a single session. For sequential tasks, same-file edits, or highly dependent work, a single `AgentLoop` is more effective.

### Compare: single agent vs agent team

| | Single Agent (`AgentLoop`) | Agent Team (`AgentTeam`) |
|:--|:--|:--|
| **Context** | One context window | Each teammate has its own context |
| **Communication** | N/A | Teammates message each other directly |
| **Coordination** | Sequential tool calls | Shared task list with self-coordination |
| **Best for** | Focused tasks, quick operations | Complex work requiring parallel exploration |
| **Token cost** | Lower | Higher: each teammate is a separate agent |

## Quick start

```toml
# Cargo.toml
[dependencies]
agent-sdk = { path = "." }
tokio = { version = "1", features = ["full"] }
```

```bash
export ANTHROPIC_API_KEY="sk-ant-..."  # or OPENAI_API_KEY
```

### Start your first agent team

Create a team, describe the teammates you want, add tasks, and run. The lead spawns teammates, they claim work from the shared task list, and coordinate on their own:

```rust
use agent_sdk::agent::team::AgentTeam;
use agent_sdk::config::{LlmConfig, AgentConfig};
use agent_sdk::types::task::Task;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let task1 = Task::new("gen", "Create config module", "...", "src/config.rs");
    let task2 = Task::new("gen", "Create server module", "...", "src/server.rs")
        .with_dependencies(vec![task1.id]);

    let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
        .source_root(".")
        .work_dir("./output")
        .add_teammate("backend-dev", "You build Rust backend modules")
        .add_teammate("reviewer", "You review code for correctness")
        .add_task(task1)
        .add_task(task2)
        .run("Build a server project")
        .await?;

    println!("Tokens used: {}", result.total_tokens());
    Ok(())
}
```

For simple tasks that don't need a team, use `run_single`:

```rust
let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .run_single("Explain this codebase")
    .await?;
println!("{}", result.final_content);
```

### Interactive CLI

```bash
cargo run --bin agent                          # REPL mode
cargo run --bin agent -- "explain this code"   # One-shot
cargo run --bin agent -- -p openai -m gpt-4o   # OpenAI
cargo run --bin agent -- --allow-all-commands   # Unrestricted shell
```

## Architecture

```
          ┌─────────────────────────────────────────────┐
          │                 Team Lead                    │
          │  Spawns teammates, coordinates work,        │
          │  approves plans, routes messages             │
          └──────────┬──────────────┬───────────────────┘
                     │              │
               ┌─────▼────┐  ┌─────▼────┐
               │Teammate 1│  │Teammate 2│  ... (N parallel)
               │AgentLoop │  │AgentLoop │
               └─────┬────┘  └─────┬────┘
                     │              │
                     │    ┌─────────┘    Teammates can
                     │    │              message each other
               ┌─────▼────▼──────────────────────────┐
               │          Shared Services             │
               │  TaskStore · MemoryStore · Mailbox   │
               └─────────────────────────────────────┘
```

An agent team consists of:

| Component | Role |
|:--|:--|
| **Team lead** | The main session that creates the team, spawns teammates, and coordinates work |
| **Teammates** | Separate agent instances that each work on assigned tasks independently |
| **Task list** | Shared list of work items that teammates claim and complete |
| **Mailbox** | Messaging system for communication between all agents |
| **Memory** | Shared key-value store for inter-agent coordination |

The lead is the intelligence — there is no separate planning step. You tell it what you want and it coordinates the team, just like Claude Code's agent teams.

## Control your agent team

### Specify teammates and roles

Each teammate gets its own context window and works independently:

```rust
AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .add_teammate("security-reviewer", "Review for security vulnerabilities")
    .add_teammate("perf-reviewer", "Review for performance issues")
    .add_teammate("test-checker", "Validate test coverage")
```

### Add tasks with dependencies

Tasks are claimed by teammates from a shared task list. Dependencies are resolved automatically — blocked tasks unblock when their dependencies complete:

```rust
let schema = Task::new("analyze", "Parse API schema", "...", "schema.json")
    .with_priority(0);

let client = Task::new("codegen", "Generate client", "...", "client.rs")
    .with_dependencies(vec![schema.id])  // waits for schema
    .with_priority(1);

let tests = Task::new("test", "Write tests", "...", "tests.rs")
    .with_dependencies(vec![client.id])
    .with_priority(2);
```

Task claiming uses file locking to prevent race conditions when multiple teammates try to claim the same task.

### Require plan approval

For complex or risky work, require teammates to plan before implementing:

```rust
AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .add_teammate_with_plan_approval(
        "architect",
        "Refactor the authentication module"
    )
```

The teammate generates a plan, sends it to the lead for review. The lead evaluates using the LLM and either approves (teammate implements) or rejects with feedback (teammate revises).

### Teammate-to-teammate messaging

Teammates communicate directly through the message broker — not just through the lead:

```rust
use agent_sdk::types::message::*;

// Direct message to another teammate
let msg = Envelope::new(my_id, MessageTarget::Agent(other_id), MessageKind::TeammateMessage)
    .with_payload(serde_json::json!({ "content": "Found an issue in auth.rs" }));
broker.route(&msg)?;

// Broadcast to all teammates
let broadcast = Envelope::new(my_id, MessageTarget::Broadcast, MessageKind::ContextShare)
    .with_payload(serde_json::json!({ "topic": "conventions", "content": "Use snake_case" }));
broker.route(&broadcast)?;
```

### Shutdown negotiation

When the lead requests shutdown, teammates can accept or reject:

- **Accept**: teammate is idle, shuts down gracefully
- **Reject**: teammate is still working, provides a reason and keeps going

### Enforce quality gates with hooks

Hooks run at key points in the agent lifecycle. Return `Reject` to block an action and send feedback:

```rust
use agent_sdk::{Hook, HookEvent, HookResult};

struct RequireTestCoverage;

impl Hook for RequireTestCoverage {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        match event {
            HookEvent::TaskCompleted { task, .. } => {
                if let Some(result) = &task.result {
                    if !result.notes.to_lowercase().contains("test") {
                        return HookResult::Reject {
                            feedback: "Must include test coverage".to_string(),
                        };
                    }
                }
                HookResult::Continue
            }
            HookEvent::TeammateIdle { tasks_completed, .. } if *tasks_completed == 0 => {
                HookResult::Reject { feedback: "Keep looking for work".to_string() }
            }
            _ => HookResult::Continue,
        }
    }
}
```

| Hook | When it fires | Reject effect |
|:--|:--|:--|
| `TeammateIdle` | Teammate has no more work | Keeps teammate active |
| `TaskCreated` | Task being added to store | Prevents task creation |
| `TaskCompleted` | Task being marked done | Prevents completion, task retries |

### Monitor agent events

Subscribe to events for logging, UI, or metrics:

```rust
let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::TeammateSpawned { name, .. } => println!("+ {name}"),
            AgentEvent::TaskStarted { title, .. } => println!("> {title}"),
            AgentEvent::TaskCompleted { tokens_used, .. } => println!("  done ({tokens_used} tokens)"),
            AgentEvent::PlanSubmitted { plan_preview, .. } => println!("  plan: {plan_preview}"),
            AgentEvent::PlanApproved { .. } => println!("  approved"),
            AgentEvent::ShutdownRequested { .. } => println!("  shutting down"),
            _ => {}
        }
    }
});

let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .event_channel(tx)
    // ...
    .run("...")
    .await?;
```

## Low-level API

### AgentLoop (single agent)

The building block underneath everything. Use it for full control:

```rust
use std::sync::Arc;
use agent_sdk::{AgentLoop, create_client, LlmConfig};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::tools::fs_tools::ReadFileTool;
use uuid::Uuid;

let client = create_client(&LlmConfig::default())?;
let mut tools = ToolRegistry::new();
tools.register(Arc::new(ReadFileTool { source_root: ".".into(), work_dir: ".".into() }));

let mut agent = AgentLoop::new(
    Uuid::new_v4(), client, tools,
    "You are a coding assistant.".to_string(), 50,
);
let result = agent.run("Summarize main.rs".to_string()).await?;
```

### Custom tools

Implement the `Tool` trait to give agents new capabilities:

```rust
use async_trait::async_trait;
use agent_sdk::{Tool, ToolDefinition, SdkResult};
use serde_json::json;

pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "my_tool".to_string(),
            description: "Does something useful".to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "input": { "type": "string" } },
                "required": ["input"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> SdkResult<serde_json::Value> {
        let input = args["input"].as_str().unwrap_or("");
        Ok(json!({ "output": format!("processed: {input}") }))
    }
}
```

Add tools to teammates via `PromptBuilder::customize_tools`.

### TeamLead (direct control)

For full control over team orchestration:

```rust
use std::sync::Arc;
use agent_sdk::*;
use agent_sdk::config::AgentConfig;
use agent_sdk::agent::hooks::HookRegistry;
use agent_sdk::traits::prompt_builder::DefaultPromptBuilder;
use uuid::Uuid;

let lead = TeamLead {
    id: Uuid::new_v4(),
    task_store: Arc::new(TaskStore::new("./output".into())),
    broker: Arc::new(MessageBroker::new("./output/mailbox".into())?),
    llm_client: create_client(&LlmConfig::default())?,
    prompt_builder: Arc::new(DefaultPromptBuilder),
    config: AgentConfig::default(),
    source_root: ".".into(),
    work_dir: "./output".into(),
    memory_store: Arc::new(MemoryStore::new("./output/memory".into())?),
    event_tx: None,
    hooks: Arc::new(HookRegistry::new()),
    teammate_specs: vec![
        TeammateSpec { name: "worker-1".into(), prompt: "...".into(), require_plan_approval: false },
    ],
};
let summary = lead.run().await?;
```

## Built-in tools

| Tool | Description |
|:--|:--|
| `read_file` | Read file contents with optional offset/limit |
| `write_file` | Write/create files in the work directory |
| `list_directory` | List files and directories |
| `search_files` | Search by glob pattern and/or content regex |
| `run_command` | Execute shell commands (configurable whitelist) |
| `read_memory` / `write_memory` / `list_memory` | Shared key-value memory |
| `get_task_context` / `list_completed_tasks` | Inspect other agents' work |

## Configuration

### LlmConfig

```rust
LlmConfig {
    provider: LlmProvider::Claude,        // Claude or OpenAi
    model: "claude-sonnet-4-20250514".into(),
    max_tokens: 4096,
    requests_per_minute: 50,
    tokens_per_minute: 80_000,
    api_key: None,       // falls back to ANTHROPIC_API_KEY / OPENAI_API_KEY
    api_base_url: None,  // falls back to env or default
}
```

### AgentConfig

```rust
AgentConfig {
    max_parallel_agents: 4,      // concurrent teammates
    poll_interval_ms: 200,       // task polling interval
    max_task_retries: 3,         // retries before permanent failure
    max_loop_iterations: 50,     // max ReAct iterations per task
    max_context_tokens: 200_000, // context window budget
}
```

### Environment variables

| Variable | Description |
|:--|:--|
| `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | API key (required) |
| `LLM_PROVIDER` | `claude` or `openai` (auto-detected from keys) |
| `LLM_MODEL` / `ANTHROPIC_MODEL` / `OPENAI_MODEL` | Model override |
| `ANTHROPIC_API_BASE_URL` / `OPENAI_API_BASE_URL` | Custom endpoint |

## Best practices

### Give teammates enough context

Teammates don't inherit the lead's conversation. Include details in task descriptions and context:

```rust
Task::new("review", "Security review", "Review src/auth/ for vulnerabilities...", "review.md")
    .with_context(json!({ "app_uses": "JWT in httpOnly cookies", "focus": ["token handling"] }))
```

### Choose appropriate team size

Start with 3-5 teammates. 5-6 tasks per teammate keeps everyone productive. Three focused teammates often outperform five scattered ones.

### Size tasks appropriately

- **Too small**: coordination overhead exceeds benefit
- **Too large**: teammates work too long without check-ins
- **Just right**: self-contained units that produce a clear deliverable

### Avoid file conflicts

Two teammates editing the same file leads to overwrites. Break work so each teammate owns different files.

## Examples

```bash
cargo run --example single_agent    # Direct AgentLoop with custom tools
cargo run --example multi_agent     # Team with tasks and dependencies
cargo run --example named_team      # Named teammates with roles + hooks + plan approval
```

## Project structure

```
src/
  lib.rs                # Public API re-exports
  config.rs             # LlmConfig, AgentConfig
  error.rs              # SdkError, TaskId, AgentId
  types/                # ChatMessage, Task, Envelope, MemoryEntry, FileChange
  traits/               # LlmClient, Tool, PromptBuilder traits
  llm/                  # Claude + OpenAI clients, rate limiter
  agent/
    team.rs             # AgentTeam — high-level entry point
    team_lead.rs        # TeamLead orchestrator
    teammate.rs         # Teammate worker (plan mode, shutdown negotiation)
    agent_loop.rs       # ReAct loop (Reason + Act)
    hooks.rs            # Hook system (TeammateIdle, TaskCreated, TaskCompleted)
    events.rs           # AgentEvent enum
    context.rs          # Per-agent context
    memory.rs           # MemoryStore
  task/                 # TaskStore, dependency graph, file locking
  mailbox/              # MessageBroker, file-based mailboxes
  tools/                # Built-in tools (fs, search, command, memory, context)
  bin/agent.rs          # Interactive CLI
examples/
  single_agent.rs       # Direct AgentLoop usage
  multi_agent.rs        # Team with tasks
  named_team.rs         # Named teammates + hooks + plan approval
```

## License

MIT
