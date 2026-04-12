use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::error::{AgentId, TaskId};
use crate::types::chat::ChatMessage;
use crate::types::file_change::FileChange;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Claimed {
        agent_id: AgentId,
        at: DateTime<Utc>,
    },
    InProgress {
        agent_id: AgentId,
        started_at: DateTime<Utc>,
    },
    Completed {
        agent_id: AgentId,
        completed_at: DateTime<Utc>,
    },
    Failed {
        agent_id: AgentId,
        error: String,
        failed_at: DateTime<Utc>,
    },
    Blocked {
        reason: String,
    },
}

impl TaskStatus {
    pub fn is_completed(&self) -> bool {
        matches!(self, TaskStatus::Completed { .. })
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, TaskStatus::Pending)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, TaskStatus::Failed { .. })
    }

    pub fn assigned_agent(&self) -> Option<AgentId> {
        match self {
            TaskStatus::Claimed { agent_id, .. }
            | TaskStatus::InProgress { agent_id, .. }
            | TaskStatus::Completed { agent_id, .. }
            | TaskStatus::Failed { agent_id, .. } => Some(*agent_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// Extensible task kind (e.g. "transform_file", "verify_contract", "custom_check")
    pub kind: String,
    pub status: TaskStatus,
    pub title: String,
    pub description: String,
    pub target_file: PathBuf,
    pub dependencies: Vec<TaskId>,
    pub priority: u32,
    pub retry_count: u32,
    pub max_retries: u32,
    /// Arbitrary context for the agent (domain-specific data)
    pub context: serde_json::Value,
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    pub fn new(
        kind: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        target_file: impl Into<PathBuf>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            kind: kind.into(),
            status: TaskStatus::Pending,
            title: title.into(),
            description: description.into(),
            target_file: target_file.into(),
            dependencies: Vec::new(),
            priority: 0,
            retry_count: 0,
            max_retries: 3,
            context: serde_json::Value::Null,
            result: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_dependencies(mut self, deps: Vec<TaskId>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_context(mut self, context: serde_json::Value) -> Self {
        self.context = context;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub file_changes: Vec<FileChange>,
    pub notes: String,
    pub llm_tokens_used: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation_log: Vec<ChatMessage>,
    #[serde(default)]
    pub tool_calls_count: usize,
    /// Domain-specific structured output (e.g. contract verdicts, analysis results)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_defaults() {
        let t = Task::new("kind", "title", "desc", PathBuf::from("out.rs"));
        assert_eq!(t.kind, "kind");
        assert_eq!(t.title, "title");
        assert_eq!(t.description, "desc");
        assert_eq!(t.target_file, PathBuf::from("out.rs"));
        assert_eq!(t.priority, 0);
        assert_eq!(t.retry_count, 0);
        assert_eq!(t.max_retries, 3);
        assert!(t.dependencies.is_empty());
        assert!(t.result.is_none());
        assert!(matches!(t.status, TaskStatus::Pending));
    }

    #[test]
    fn builder_methods_mutate_fields() {
        let dep = Uuid::new_v4();
        let ctx = serde_json::json!({"foo": 1});
        let t = Task::new("k", "t", "d", PathBuf::from("f"))
            .with_dependencies(vec![dep])
            .with_priority(9)
            .with_context(ctx.clone());
        assert_eq!(t.dependencies, vec![dep]);
        assert_eq!(t.priority, 9);
        assert_eq!(t.context, ctx);
    }

    #[test]
    fn status_classification_helpers() {
        assert!(TaskStatus::Pending.is_pending());
        assert!(!TaskStatus::Pending.is_completed());
        assert!(!TaskStatus::Pending.is_failed());
        assert_eq!(TaskStatus::Pending.assigned_agent(), None);

        let id = Uuid::new_v4();
        let now = Utc::now();

        let claimed = TaskStatus::Claimed {
            agent_id: id,
            at: now,
        };
        assert_eq!(claimed.assigned_agent(), Some(id));

        let in_progress = TaskStatus::InProgress {
            agent_id: id,
            started_at: now,
        };
        assert_eq!(in_progress.assigned_agent(), Some(id));

        let completed = TaskStatus::Completed {
            agent_id: id,
            completed_at: now,
        };
        assert!(completed.is_completed());
        assert_eq!(completed.assigned_agent(), Some(id));

        let failed = TaskStatus::Failed {
            agent_id: id,
            error: "e".into(),
            failed_at: now,
        };
        assert!(failed.is_failed());
        assert_eq!(failed.assigned_agent(), Some(id));

        let blocked = TaskStatus::Blocked {
            reason: "upstream".into(),
        };
        assert!(!blocked.is_pending());
        assert!(!blocked.is_completed());
        assert_eq!(blocked.assigned_agent(), None);
    }

    #[test]
    fn task_serde_roundtrip() {
        let t = Task::new("k", "t", "d", PathBuf::from("f")).with_priority(2);
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, t.id);
        assert_eq!(back.priority, 2);
    }
}
