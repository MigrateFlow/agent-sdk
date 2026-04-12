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
    // --- Task lifecycle ---
    TaskAssignment,
    TaskComplete,
    TaskFailed,
    DependencyResolved,

    // --- Plan mode ---
    /// Teammate submits a plan for lead approval.
    PlanSubmission,
    /// Lead approves a teammate's plan — teammate exits plan mode.
    PlanApproved,
    /// Lead rejects a plan with feedback — teammate revises.
    PlanRejected,

    // --- Lead <-> Teammate ---
    QuestionForLead,
    AnswerFromLead,

    // --- Teammate <-> Teammate ---
    /// Direct message between teammates.
    TeammateMessage,

    // --- Lifecycle ---
    /// Teammate notifies it has no more work.
    TeammateIdle,
    /// Lead requests teammate to shut down.
    ShutdownRequest,
    /// Teammate accepts shutdown.
    ShutdownAccepted,
    /// Teammate rejects shutdown with a reason.
    ShutdownRejected,
    /// Legacy: immediate shutdown (no negotiation).
    Shutdown,

    // --- Context sharing ---
    ContextShare,
}

// --- Payloads ---

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSubmissionPayload {
    pub task_id: TaskId,
    pub plan: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRejectionPayload {
    pub task_id: TaskId,
    pub feedback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateMessagePayload {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownRejectedPayload {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateIdlePayload {
    pub tasks_completed: usize,
}
