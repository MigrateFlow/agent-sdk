use serde::Serialize;

use crate::error::{AgentId, TaskId};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    // --- Task lifecycle ---
    TaskCreated {
        agent_id: AgentId,
        name: String,
        task_id: TaskId,
        title: String,
    },
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

    // --- Hooks ---
    HookRejected {
        event_name: String,
        feedback: String,
    },

    // --- Memory / compaction ---
    MemoryCompacted {
        strategy: String,
        messages_before: usize,
        messages_after: usize,
        tokens_saved: u64,
    },

    // --- Subagent lifecycle ---
    SubAgentSpawned {
        agent_id: AgentId,
        name: String,
        description: String,
    },
    SubAgentProgress {
        agent_id: AgentId,
        name: String,
        iteration: usize,
        max_turns: usize,
        current_tool: Option<String>,
        tokens_so_far: u64,
    },
    SubAgentCompleted {
        agent_id: AgentId,
        name: String,
        tokens_used: u64,
        iterations: usize,
        tool_calls: usize,
        /// The final content returned by the subagent (for display and result delivery).
        final_content: String,
        /// Worktree path when the subagent ran in an isolated worktree and left changes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_path: Option<String>,
        /// Branch name containing the subagent's changes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    SubAgentFailed {
        agent_id: AgentId,
        name: String,
        error: String,
    },
    /// Intermediate update from a running subagent — partial result before completion.
    SubAgentUpdate {
        agent_id: AgentId,
        name: String,
        iteration: usize,
        /// The assistant's text content from this iteration.
        content: String,
        /// Whether this is the final iteration (subagent is about to return).
        is_final: bool,
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
            Self::TaskCreated { agent_id, .. }
            | Self::TaskStarted { agent_id, .. }
            | Self::Thinking { agent_id, .. }
            | Self::ToolCall { agent_id, .. }
            | Self::ToolResult { agent_id, .. }
            | Self::TaskCompleted { agent_id, .. }
            | Self::TaskFailed { agent_id, .. }
            | Self::PlanSubmitted { agent_id, .. }
            | Self::PlanApproved { agent_id, .. }
            | Self::PlanRejected { agent_id, .. } => Some(*agent_id),
            Self::SubAgentSpawned { agent_id, .. }
            | Self::SubAgentProgress { agent_id, .. }
            | Self::SubAgentCompleted { agent_id, .. }
            | Self::SubAgentFailed { agent_id, .. }
            | Self::SubAgentUpdate { agent_id, .. } => Some(*agent_id),
            Self::HookRejected { .. }
            | Self::MemoryCompacted { .. }
            | Self::Custom { .. } => None,
        }
    }

    /// Get the human-readable agent name, if present.
    pub fn agent_name(&self) -> Option<&str> {
        match self {
            Self::TaskCreated { name, .. }
            | Self::TaskStarted { name, .. }
            | Self::Thinking { name, .. }
            | Self::ToolCall { name, .. }
            | Self::ToolResult { name, .. }
            | Self::TaskCompleted { name, .. }
            | Self::TaskFailed { name, .. }
            | Self::PlanSubmitted { name, .. }
            | Self::PlanApproved { name, .. }
            | Self::PlanRejected { name, .. } => Some(name),
            Self::SubAgentSpawned { name, .. }
            | Self::SubAgentProgress { name, .. }
            | Self::SubAgentCompleted { name, .. }
            | Self::SubAgentFailed { name, .. }
            | Self::SubAgentUpdate { name, .. } => Some(name),
            Self::HookRejected { .. }
            | Self::MemoryCompacted { .. }
            | Self::Custom { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn agent() -> AgentId {
        Uuid::new_v4()
    }

    #[test]
    fn agent_id_returns_some_for_agent_bearing_variants() {
        let id = agent();
        let tid = Uuid::new_v4();
        let cases: Vec<AgentEvent> = vec![
            AgentEvent::TaskCreated {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
                title: "t".into(),
            },
            AgentEvent::TaskStarted {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
                title: "t".into(),
            },
            AgentEvent::Thinking {
                agent_id: id,
                name: "n".into(),
                content: "c".into(),
                iteration: 1,
            },
            AgentEvent::ToolCall {
                agent_id: id,
                name: "n".into(),
                tool_name: "read".into(),
                arguments: "{}".into(),
                iteration: 1,
            },
            AgentEvent::ToolResult {
                agent_id: id,
                name: "n".into(),
                tool_name: "read".into(),
                result_preview: "ok".into(),
                iteration: 1,
            },
            AgentEvent::TaskCompleted {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
                tokens_used: 0,
                iterations: 1,
                tool_calls: 0,
            },
            AgentEvent::TaskFailed {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
                error: "oops".into(),
            },
            AgentEvent::PlanSubmitted {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
                plan_preview: "p".into(),
            },
            AgentEvent::PlanApproved {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
            },
            AgentEvent::PlanRejected {
                agent_id: id,
                name: "n".into(),
                task_id: tid,
                feedback: "f".into(),
            },
            AgentEvent::SubAgentSpawned {
                agent_id: id,
                name: "n".into(),
                description: "d".into(),
            },
            AgentEvent::SubAgentProgress {
                agent_id: id,
                name: "n".into(),
                iteration: 1,
                max_turns: 10,
                current_tool: None,
                tokens_so_far: 0,
            },
            AgentEvent::SubAgentCompleted {
                agent_id: id,
                name: "n".into(),
                tokens_used: 0,
                iterations: 1,
                tool_calls: 0,
                final_content: "done".into(),
                worktree_path: None,
                branch: None,
            },
            AgentEvent::SubAgentFailed {
                agent_id: id,
                name: "n".into(),
                error: "boom".into(),
            },
            AgentEvent::SubAgentUpdate {
                agent_id: id,
                name: "n".into(),
                iteration: 1,
                content: "c".into(),
                is_final: false,
            },
        ];
        for e in &cases {
            assert_eq!(e.agent_id(), Some(id), "agent_id for {e:?}");
            assert_eq!(e.agent_name(), Some("n"), "agent_name for {e:?}");
        }
    }

    #[test]
    fn variants_without_agent_return_none() {
        let events = [
            AgentEvent::HookRejected {
                event_name: "pre".into(),
                feedback: "no".into(),
            },
            AgentEvent::MemoryCompacted {
                strategy: "sum".into(),
                messages_before: 10,
                messages_after: 2,
                tokens_saved: 100,
            },
            AgentEvent::Custom {
                name: "x".into(),
                data: serde_json::json!({}),
            },
        ];
        for e in &events {
            assert!(e.agent_id().is_none());
            assert!(e.agent_name().is_none());
        }
    }

    #[test]
    fn serialization_uses_snake_case_tagged_type() {
        let e = AgentEvent::SubAgentSpawned {
            agent_id: agent(),
            name: "n".into(),
            description: "d".into(),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["type"], "sub_agent_spawned");
    }
}
