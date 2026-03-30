use serde::Serialize;

use crate::error::{AgentId, TaskId};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    // --- Team lifecycle ---
    TeamSpawned {
        teammate_count: usize,
    },
    TeammateSpawned {
        agent_id: AgentId,
        name: String,
    },

    // --- Task lifecycle ---
    TaskStarted {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
        title: String,
    },
    Thinking {
        agent_id: AgentId,
        name: String,
        content: String,
        iteration: usize,
    },
    ToolCall {
        agent_id: AgentId,
        name: String,
        tool_name: String,
        arguments: String,
        iteration: usize,
    },
    ToolResult {
        agent_id: AgentId,
        name: String,
        tool_name: String,
        result_preview: String,
        iteration: usize,
    },
    TaskCompleted {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
        tokens_used: u64,
        iterations: usize,
        tool_calls: usize,
    },
    TaskFailed {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
        error: String,
    },

    // --- Plan mode ---
    PlanSubmitted {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
        plan_preview: String,
    },
    PlanApproved {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
    },
    PlanRejected {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
        feedback: String,
    },

    // --- Communication ---
    TeammateMessage {
        from: AgentId,
        from_name: String,
        to: AgentId,
        content_preview: String,
    },
    TeammateIdle {
        agent_id: AgentId,
        name: String,
        tasks_completed: usize,
    },

    // --- Shutdown ---
    ShutdownRequested {
        agent_id: AgentId,
        name: String,
    },
    ShutdownAccepted {
        agent_id: AgentId,
        name: String,
    },
    ShutdownRejected {
        agent_id: AgentId,
        name: String,
        reason: String,
    },
    AgentShutdown {
        agent_id: AgentId,
        name: String,
    },

    // --- Hooks ---
    HookRejected {
        event_name: String,
        feedback: String,
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
            | Self::PlanSubmitted { agent_id, .. }
            | Self::PlanApproved { agent_id, .. }
            | Self::PlanRejected { agent_id, .. }
            | Self::TeammateIdle { agent_id, .. }
            | Self::ShutdownRequested { agent_id, .. }
            | Self::ShutdownAccepted { agent_id, .. }
            | Self::ShutdownRejected { agent_id, .. }
            | Self::TeammateSpawned { agent_id, .. }
            | Self::AgentShutdown { agent_id, .. } => Some(*agent_id),
            Self::TeammateMessage { from, .. } => Some(*from),
            Self::TeamSpawned { .. }
            | Self::HookRejected { .. }
            | Self::Custom { .. } => None,
        }
    }

    /// Get the human-readable teammate name, if present.
    pub fn agent_name(&self) -> Option<&str> {
        match self {
            Self::TeammateSpawned { name, .. }
            | Self::TaskStarted { name, .. }
            | Self::Thinking { name, .. }
            | Self::ToolCall { name, .. }
            | Self::ToolResult { name, .. }
            | Self::TaskCompleted { name, .. }
            | Self::TaskFailed { name, .. }
            | Self::PlanSubmitted { name, .. }
            | Self::PlanApproved { name, .. }
            | Self::PlanRejected { name, .. }
            | Self::TeammateIdle { name, .. }
            | Self::ShutdownRequested { name, .. }
            | Self::ShutdownAccepted { name, .. }
            | Self::ShutdownRejected { name, .. }
            | Self::AgentShutdown { name, .. } => Some(name),
            Self::TeammateMessage { from_name, .. } => Some(from_name),
            Self::TeamSpawned { .. }
            | Self::HookRejected { .. }
            | Self::Custom { .. } => None,
        }
    }
}
