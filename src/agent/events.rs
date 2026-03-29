use serde::Serialize;

use crate::error::{AgentId, TaskId};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TaskStarted {
        agent_id: AgentId,
        task_id: TaskId,
        title: String,
    },
    Thinking {
        agent_id: AgentId,
        content: String,
        iteration: usize,
    },
    ToolCall {
        agent_id: AgentId,
        tool_name: String,
        arguments: String,
        iteration: usize,
    },
    ToolResult {
        agent_id: AgentId,
        tool_name: String,
        result_preview: String,
        iteration: usize,
    },
    TaskCompleted {
        agent_id: AgentId,
        task_id: TaskId,
        tokens_used: u64,
        iterations: usize,
        tool_calls: usize,
    },
    TaskFailed {
        agent_id: AgentId,
        task_id: TaskId,
        error: String,
    },
    AgentShutdown {
        agent_id: AgentId,
    },
    /// Domain-specific custom event
    Custom {
        name: String,
        data: serde_json::Value,
    },
}

impl AgentEvent {
    pub fn agent_id(&self) -> Option<AgentId> {
        match self {
            Self::TaskStarted { agent_id, .. }
            | Self::Thinking { agent_id, .. }
            | Self::ToolCall { agent_id, .. }
            | Self::ToolResult { agent_id, .. }
            | Self::TaskCompleted { agent_id, .. }
            | Self::TaskFailed { agent_id, .. }
            | Self::AgentShutdown { agent_id } => Some(*agent_id),
            Self::Custom { .. } => None,
        }
    }
}
