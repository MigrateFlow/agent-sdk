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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error as IoError, ErrorKind};

    #[test]
    fn io_error_is_auto_converted() {
        let ioe = IoError::new(ErrorKind::NotFound, "missing");
        let err: SdkError = ioe.into();
        assert!(matches!(err, SdkError::Io(_)));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn serde_error_is_auto_converted() {
        let err: SdkError = serde_json::from_str::<u32>("not-json").unwrap_err().into();
        assert!(matches!(err, SdkError::Serde(_)));
    }

    #[test]
    fn llm_api_display_includes_status_and_message() {
        let e = SdkError::LlmApi {
            status: 429,
            message: "too many".into(),
        };
        let s = e.to_string();
        assert!(s.contains("429"));
        assert!(s.contains("too many"));
    }

    #[test]
    fn rate_limited_display() {
        let e = SdkError::RateLimited {
            retry_after_ms: 1500,
        };
        assert!(e.to_string().contains("1500ms"));
    }

    #[test]
    fn task_not_found_display_contains_task_id() {
        let id = uuid::Uuid::new_v4();
        let e = SdkError::TaskNotFound { task_id: id };
        assert!(e.to_string().contains(&id.to_string()));
    }

    #[test]
    fn task_failed_display_contains_reason() {
        let id = uuid::Uuid::new_v4();
        let e = SdkError::TaskFailed {
            task_id: id,
            reason: "bad".into(),
        };
        let s = e.to_string();
        assert!(s.contains(&id.to_string()));
        assert!(s.contains("bad"));
    }

    #[test]
    fn agent_crashed_display() {
        let id = uuid::Uuid::new_v4();
        let e = SdkError::AgentCrashed {
            agent_id: id,
            reason: "panic".into(),
        };
        let s = e.to_string();
        assert!(s.contains(&id.to_string()));
        assert!(s.contains("panic"));
    }

    #[test]
    fn dependency_cycle_display_mentions_task_ids() {
        let ids = vec![uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
        let e = SdkError::DependencyCycle {
            task_ids: ids.clone(),
        };
        let s = e.to_string();
        for id in &ids {
            assert!(s.contains(&id.to_string()));
        }
    }

    #[test]
    fn lock_failed_display() {
        let e = SdkError::LockFailed {
            path: std::path::PathBuf::from("/tmp/x.lock"),
        };
        assert!(e.to_string().contains("/tmp/x.lock"));
    }

    #[test]
    fn config_display_passes_through_message() {
        let e = SdkError::Config("missing API key".into());
        assert!(e.to_string().contains("missing API key"));
    }

    #[test]
    fn tool_execution_display() {
        let e = SdkError::ToolExecution {
            tool_name: "read_file".into(),
            message: "no permission".into(),
        };
        let s = e.to_string();
        assert!(s.contains("read_file"));
        assert!(s.contains("no permission"));
    }

    #[test]
    fn max_iterations_display() {
        let e = SdkError::MaxIterationsExceeded { max_iterations: 20 };
        assert!(e.to_string().contains("20"));
    }

    #[test]
    fn context_overflow_display() {
        let e = SdkError::ContextOverflow {
            current_tokens: 9001,
            max_tokens: 8000,
        };
        let s = e.to_string();
        assert!(s.contains("9001"));
        assert!(s.contains("8000"));
    }

    #[test]
    fn llm_response_parse_display() {
        let e = SdkError::LlmResponseParse("bad json".into());
        assert!(e.to_string().contains("bad json"));
    }
}
