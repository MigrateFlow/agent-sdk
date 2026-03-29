use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AgentId, TaskId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: Uuid,
    pub from: AgentId,
    pub to: MessageTarget,
    pub timestamp: DateTime<Utc>,
    pub kind: MessageKind,
    pub payload: serde_json::Value,
    pub in_reply_to: Option<Uuid>,
}

impl Envelope {
    pub fn new(from: AgentId, to: MessageTarget, kind: MessageKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            from,
            to,
            timestamp: Utc::now(),
            kind,
            payload: serde_json::Value::Null,
            in_reply_to: None,
        }
    }

    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }

    pub fn in_reply_to(mut self, msg_id: Uuid) -> Self {
        self.in_reply_to = Some(msg_id);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageTarget {
    Agent(AgentId),
    TeamLead,
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    TaskAssignment,
    TaskComplete,
    TaskFailed,
    QuestionForLead,
    AnswerFromLead,
    DependencyResolved,
    ContextShare,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompletePayload {
    pub task_id: TaskId,
    pub tokens_used: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFailedPayload {
    pub task_id: TaskId,
    pub error: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSharePayload {
    pub topic: String,
    pub content: String,
}
