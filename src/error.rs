use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

pub type TaskId = Uuid;
pub type AgentId = Uuid;

#[derive(Error, Debug)]
pub enum SdkError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("LLM API error: {status} - {message}")]
    LlmApi { status: u16, message: String },

    #[error("LLM rate limited, retry after {retry_after_ms}ms")]
    RateLimited { retry_after_ms: u64 },

    #[error("LLM response parse error: {0}")]
    LlmResponseParse(String),

    #[error("Task {task_id} not found")]
    TaskNotFound { task_id: TaskId },

    #[error("Task {task_id} failed: {reason}")]
    TaskFailed { task_id: TaskId, reason: String },

    #[error("Agent {agent_id} crashed: {reason}")]
    AgentCrashed { agent_id: AgentId, reason: String },

    #[error("Dependency cycle detected involving tasks: {task_ids:?}")]
    DependencyCycle { task_ids: Vec<TaskId> },

    #[error("Lock acquisition failed for {path}")]
    LockFailed { path: PathBuf },

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Tool execution error in {tool_name}: {message}")]
    ToolExecution { tool_name: String, message: String },

    #[error("Agent loop exceeded maximum iterations ({max_iterations})")]
    MaxIterationsExceeded { max_iterations: usize },

    #[error("Context window overflow: {current_tokens} exceeds {max_tokens} limit")]
    ContextOverflow { current_tokens: usize, max_tokens: usize },
}

pub type SdkResult<T> = Result<T, SdkError>;
