# Agent Teams

`AgentTeam` is the high-level orchestration API for running multiple agents in parallel with shared task state, mailbox messaging, and shared memory.

## Mental Model

An agent team has:

- one team lead
- zero or more named teammates, or a generic pool if no teammates are provided
- a file-backed task store
- a file-backed mailbox broker
- a file-backed shared memory store

The lead spawns teammates, teammates claim tasks, dependencies gate execution, and completion is tracked on disk.

## Build A Team

```rust
use agent_sdk::{AgentConfig, AgentTeam, LlmConfig, Task};

let analyze = Task::new(
    "analysis",
    "Inspect crate exports",
    "Read src/lib.rs and list the public SDK entrypoints.",
    "docs/exports.md",
)
.with_priority(0);

let write = Task::new(
    "docs",
    "Write team usage guide",
    "Document how AgentTeam coordinates teammates and tasks.",
    "docs/team-usage.md",
)
.with_dependencies(vec![analyze.id])
.with_priority(1);

let team = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .source_root(".")
    .work_dir(".")
    .add_teammate("api-reader", "Read the Rust API surface and summarize it")
    .add_teammate("writer", "Write markdown documentation from implementation details")
    .add_task(analyze)
    .add_task(write);
```

Run it:

```rust
let result = team.run("Document the SDK").await?;
println!("tokens: {}", result.total_tokens());
```

## Result Types

`AgentTeam::run(...)` returns `TeamResult`:

```rust
pub enum TeamResult {
    Single(AgentLoopResult),
    Team(ExecutionSummary),
}
```

Current `run(...)` always follows the team path and returns `Team(...)`. The `Single(...)` variant is useful conceptually alongside `run_single(...)`, but team execution itself is task-driven.

`ExecutionSummary` contains:

- `total_tasks`
- `tasks_completed`
- `tasks_failed`
- `total_tokens_used`
- `agents_spawned`

## Task Model

Tasks are explicit work items:

```rust
use agent_sdk::Task;
use serde_json::json;

let task = Task::new(
    "docs",
    "Write getting started guide",
    "Explain installation, config, and first program.",
    "docs/getting-started.md",
)
.with_priority(0)
.with_context(json!({
    "assigned_teammate": "writer",
    "audience": "sdk users"
}));
```

Key fields:

- `kind`: free-form category string
- `title`
- `description`
- `target_file`
- `dependencies`
- `priority`
- `max_retries`
- `context`

Task state machine:

- `Pending`
- `Claimed`
- `InProgress`
- `Completed`
- `Failed`
- `Blocked`

Dependency resolution is based on completed task ids. Lower `priority` values are claimed first.

## Named Teammates

Named teammates let you shape specialization:

```rust
let team = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .add_teammate("security-reviewer", "Review code for security issues")
    .add_teammate("perf-reviewer", "Review code for performance bottlenecks");
```

If you do not add teammates explicitly, the lead spawns a generic pool up to `max_parallel_agents`.

## Plan Approval Mode

You can require a teammate to submit a plan before implementation:

```rust
let team = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .add_teammate_with_plan_approval(
        "architect",
        "Refactor the configuration layer carefully",
    );
```

Current behavior:

- the teammate switches into plan mode
- it submits a plan to the lead by mailbox message
- the lead asks the LLM to approve or reject the plan
- approved plans proceed to execution
- rejected plans are returned with feedback

Relevant events:

- `PlanSubmitted`
- `PlanApproved`
- `PlanRejected`

## Shared Infrastructure On Disk

The runtime layout now follows the Claude Code agent-team model:

- project-local `.agent/` is for shared config checked into the repo
- team config lives under `~/.agent/teams/<team-name>/config.json`
- shared task state lives under `~/.agent/tasks/<team-name>/`
- other mutable team resources stay under the same `~/.agent/teams/<team-name>/` team directory

Current runtime layout:

```text
~/.agent/
  settings.json
  teams/
    <team-name>/
      config.json
      mailbox/
        team-lead/
          inbox.jsonl
          inbox.lock
        agents/
          <agent-id>/
            inbox.jsonl
            inbox.lock
      memory/
        <key>.json
  tasks/
    <team-name>/
      pending/
      in_progress/
      completed/
      failed/
```

Project-local config layout:

```text
.agent/
  settings.json
  settings.local.json
```

Each task is persisted as JSON, with a `.lock` file used during claiming.

## Shared Memory

Teammates can use a key-value store for coordination.

Built-in team tools:

- `read_memory`
- `write_memory`
- `list_memory`

Example payloads:

```json
{ "key": "style-guide", "value": { "tone": "concise", "format": "markdown" } }
```

Keys are stored as JSON files under `~/.agent/teams/<team-name>/memory/`.

## Task Context Tools

Teammates can inspect completed work using:

- `get_task_context`
- `list_completed_tasks`

This is useful when one task depends on artifacts or notes produced by another teammate.

## Hooks

Hooks let you enforce quality gates:

```rust
use agent_sdk::{Hook, HookEvent, HookResult};

struct RequireTests;

impl Hook for RequireTests {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        match event {
            HookEvent::TaskCompleted { task, .. } => {
                let has_test_note = task
                    .result
                    .as_ref()
                    .map(|r| r.notes.to_lowercase().contains("test"))
                    .unwrap_or(false);

                if has_test_note {
                    HookResult::Continue
                } else {
                    HookResult::Reject {
                        feedback: "Task completion must mention test coverage".to_string(),
                    }
                }
            }
            _ => HookResult::Continue,
        }
    }
}
```

Hook events currently available:

- `TeammateIdle`
- `TaskCreated`
- `TaskCompleted`

## Event Monitoring

You can subscribe to `AgentEvent` through an unbounded channel:

```rust
use agent_sdk::AgentEvent;

let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

tokio::spawn(async move {
    while let Some(event) = rx.recv().await {
        println!("{:?}", event);
    }
});

let team = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .event_channel(tx);
```

Important team events:

- `TeamSpawned`
- `TeammateSpawned`
- `TaskStarted`
- `TaskCompleted`
- `TaskFailed`
- `PlanSubmitted`
- `PlanApproved`
- `PlanRejected`
- `ShutdownRequested`
- `HookRejected`

## Custom Prompt Builder

For domain-specific systems, override prompt generation:

```rust
use std::sync::Arc;

use agent_sdk::traits::prompt_builder::PromptBuilder;
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::Task;

struct DocsPromptBuilder;

impl PromptBuilder for DocsPromptBuilder {
    fn build_system_prompt(
        &self,
        task: &Task,
        _source_root: &std::path::Path,
        _work_dir: &std::path::Path,
    ) -> String {
        format!("You write SDK documentation. Task: {}", task.title)
    }

    fn build_user_message(&self, task: &Task) -> String {
        format!("Complete this docs task:\n{}", task.description)
    }

    fn customize_tools(&self, _task: &Task, registry: ToolRegistry) -> ToolRegistry {
        registry
    }
}

let team = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    .prompt_builder(Arc::new(DocsPromptBuilder));
```

## Practical Caveats

- `source_root` is the read side used by `read_file`, `list_directory`, and `search_files`
- `work_dir` is the write side used by `write_file` and command execution
- if you set both to the repository root, agents read and write in the same tree
- if you separate them, teammates read from source and write generated output elsewhere
