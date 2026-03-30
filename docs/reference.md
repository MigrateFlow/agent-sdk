# Reference

This is a compact reference for the current public SDK surface.

## Top-Level Re-Exports

From `src/lib.rs`:

- `AgentLoop`
- `AgentTeam`
- `TeamLead`
- `ExecutionSummary`
- `TeammateSpec`
- `Teammate`
- `AgentEvent`
- `MemoryStore`
- `Hook`
- `HookEvent`
- `HookResult`
- `HookRegistry`
- `TaskStore`
- `MessageBroker`
- `LlmClient`
- `Tool`
- `ToolDefinition`
- `ChatMessage`
- `Task`
- `TaskResult`
- `TaskStatus`
- `create_client`
- `LlmConfig`
- `LlmProvider`
- `AgentConfig`
- `AGENT_DIR`
- `AgentPaths`
- `AgentId`
- `TaskId`
- `SdkError`
- `SdkResult`

## `LlmProvider`

```rust
pub enum LlmProvider {
    Claude,
    OpenAi,
}
```

## `LlmConfig`

```rust
pub struct LlmConfig {
    pub provider: LlmProvider,
    pub model: String,
    pub max_tokens: usize,
    pub requests_per_minute: u32,
    pub tokens_per_minute: u32,
    pub api_key: Option<String>,
    pub api_base_url: Option<String>,
    pub http_timeout_secs: u64,
    pub max_retries: u32,
    pub retry_base_delay_ms: u64,
}
```

Helpers:

- `resolve_api_key()`
- `resolve_base_url()`

## `AgentConfig`

```rust
pub struct AgentConfig {
    pub max_parallel_agents: usize,
    pub poll_interval_ms: u64,
    pub max_task_retries: u32,
    pub max_loop_iterations: usize,
    pub max_context_tokens: usize,
    pub max_idle_cycles: u32,
    pub plan_approval_timeout_secs: u64,
    pub command_timeout_secs: u64,
}
```

## `Task`

```rust
pub struct Task {
    pub id: TaskId,
    pub kind: String,
    pub status: TaskStatus,
    pub title: String,
    pub description: String,
    pub target_file: PathBuf,
    pub dependencies: Vec<TaskId>,
    pub priority: u32,
    pub retry_count: u32,
    pub max_retries: u32,
    pub context: serde_json::Value,
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Builders:

- `Task::new(kind, title, description, target_file)`
- `with_dependencies(...)`
- `with_priority(...)`
- `with_context(...)`

## `TaskStatus`

States:

- `Pending`
- `Claimed { agent_id, at }`
- `InProgress { agent_id, started_at }`
- `Completed { agent_id, completed_at }`
- `Failed { agent_id, error, failed_at }`
- `Blocked { reason }`

Helpers:

- `is_completed()`
- `is_pending()`
- `is_failed()`
- `assigned_agent()`

## `TaskResult`

```rust
pub struct TaskResult {
    pub file_changes: Vec<FileChange>,
    pub notes: String,
    pub llm_tokens_used: u64,
    pub conversation_log: Vec<ChatMessage>,
    pub tool_calls_count: usize,
    pub extra: Option<serde_json::Value>,
}
```

## `ChatMessage`

Variants:

- `System { content }`
- `User { content }`
- `Assistant { content, tool_calls }`
- `Tool { tool_call_id, content }`

Helpers:

- `system(...)`
- `user(...)`
- `assistant(...)`
- `assistant_with_tools(...)`
- `tool_result(...)`
- `is_final_answer()`
- `text_content()`
- `char_len()`

## `AgentLoop`

Constructors and methods:

- `AgentLoop::new(...)`
- `AgentLoop::with_messages(...)`
- `with_max_context_tokens(...)`
- `set_event_sink(...)`
- `messages()`
- `run(...)`

## `AgentTeam`

Methods:

- `AgentTeam::new(...)`
- `source_root(...)`
- `work_dir(...)`
- `prompt_builder(...)`
- `event_channel(...)`
- `llm_client(...)`
- `add_hook(...)`
- `add_teammate(...)`
- `add_teammate_with_plan_approval(...)`
- `add_task(...)`
- `run(...)`
- `run_single(...)`

## `PromptBuilder`

Methods:

- `build_system_prompt(...)`
- `build_user_message(...)`
- `customize_tools(...)`

Default implementation:

- `DefaultPromptBuilder`

## `Hook`

Method:

- `on_event(&HookEvent) -> HookResult`

## `HookEvent`

Variants:

- `TeammateIdle { agent_id, name, tasks_completed }`
- `TaskCreated { task }`
- `TaskCompleted { task, agent_id }`

## `HookResult`

Variants:

- `Continue`
- `Reject { feedback }`

## `AgentEvent`

Variants:

- `TeamSpawned`
- `TeammateSpawned`
- `TaskStarted`
- `Thinking`
- `ToolCall`
- `ToolResult`
- `TaskCompleted`
- `TaskFailed`
- `PlanSubmitted`
- `PlanApproved`
- `PlanRejected`
- `TeammateMessage`
- `TeammateIdle`
- `ShutdownRequested`
- `ShutdownAccepted`
- `ShutdownRejected`
- `AgentShutdown`
- `HookRejected`
- `Custom`

Helper:

- `agent_id()`

## `Tool`

Methods:

- `definition() -> ToolDefinition`
- `execute(arguments) -> SdkResult<serde_json::Value>`

## Built-In Tools

File and shell tools:

- `ReadFileTool`
- `WriteFileTool`
- `ListDirectoryTool`
- `SearchFilesTool`
- `RunCommandTool`

Team tools:

- `SpawnAgentTeamTool`
- `ReadMemoryTool`
- `WriteMemoryTool`
- `ListMemoryTool`
- `GetTaskContextTool`
- `ListCompletedTasksTool`

## Persistent Components

- `AgentPaths`: resolves project-local `.agent/` config paths plus Claude-style team runtime paths under `~/.agent/teams/<team-name>/` and `~/.agent/tasks/<team-name>/`
- `TaskStore`: file-backed tasks under the configured team task directory
- `MessageBroker`: routes `Envelope` values to mailboxes
- `Mailbox`: JSONL inbox with file locking
- `MemoryStore`: JSON-backed shared key-value storage under the configured team directory

## Errors

Common `SdkError` variants:

- `Io`
- `Serde`
- `LlmApi`
- `RateLimited`
- `LlmResponseParse`
- `TaskNotFound`
- `TaskFailed`
- `AgentCrashed`
- `DependencyCycle`
- `LockFailed`
- `Config`
- `ToolExecution`
- `MaxIterationsExceeded`
- `ContextOverflow`
