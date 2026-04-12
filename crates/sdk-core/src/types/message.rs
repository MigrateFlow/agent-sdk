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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_new_sets_defaults() {
        let from = Uuid::new_v4();
        let to_agent = Uuid::new_v4();
        let env = Envelope::new(
            from,
            MessageTarget::Agent(to_agent),
            MessageKind::TeammateMessage,
        );
        assert_eq!(env.from, from);
        assert_eq!(env.to, MessageTarget::Agent(to_agent));
        assert_eq!(env.kind, MessageKind::TeammateMessage);
        assert_eq!(env.payload, serde_json::Value::Null);
        assert!(env.in_reply_to.is_none());
    }

    #[test]
    fn envelope_builder_methods() {
        let e = Envelope::new(
            Uuid::new_v4(),
            MessageTarget::TeamLead,
            MessageKind::QuestionForLead,
        );
        let reply_id = Uuid::new_v4();
        let built = e
            .with_payload(serde_json::json!({"q": "why"}))
            .in_reply_to(reply_id);
        assert_eq!(built.payload["q"], "why");
        assert_eq!(built.in_reply_to, Some(reply_id));
    }

    #[test]
    fn target_and_kind_serialize_snake_case() {
        let j = serde_json::to_value(&MessageTarget::Broadcast).unwrap();
        assert_eq!(j, serde_json::json!("broadcast"));
        let j = serde_json::to_value(&MessageKind::TaskAssignment).unwrap();
        assert_eq!(j, serde_json::json!("task_assignment"));
    }

    #[test]
    fn payload_types_serde_roundtrip() {
        let p = TaskCompletePayload {
            task_id: Uuid::new_v4(),
            tokens_used: 123,
        };
        let back: TaskCompletePayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.tokens_used, 123);

        let p = TaskFailedPayload {
            task_id: Uuid::new_v4(),
            error: "oops".into(),
            retryable: true,
        };
        let back: TaskFailedPayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert!(back.retryable);
        assert_eq!(back.error, "oops");

        let p = ContextSharePayload {
            topic: "t".into(),
            content: "c".into(),
        };
        let back: ContextSharePayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.topic, "t");

        let p = PlanSubmissionPayload {
            task_id: Uuid::new_v4(),
            plan: "the plan".into(),
        };
        let back: PlanSubmissionPayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.plan, "the plan");

        let p = PlanRejectionPayload {
            task_id: Uuid::new_v4(),
            feedback: "needs work".into(),
        };
        let back: PlanRejectionPayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.feedback, "needs work");

        let p = TeammateMessagePayload {
            content: "hi".into(),
        };
        let back: TeammateMessagePayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.content, "hi");

        let p = ShutdownRejectedPayload {
            reason: "busy".into(),
        };
        let back: ShutdownRejectedPayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.reason, "busy");

        let p = TeammateIdlePayload { tasks_completed: 4 };
        let back: TeammateIdlePayload =
            serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
        assert_eq!(back.tasks_completed, 4);
    }
}
