use crate::error::AgentId;
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct AgentResult {
    pub agent_id: AgentId,
    pub name: String,
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub total_tokens_used: u64,
}

pub struct AgentHandle {
    pub agent_id: AgentId,
    pub name: String,
    pub handle: JoinHandle<AgentResult>,
}

impl AgentHandle {
    pub fn new(agent_id: AgentId, name: String, handle: JoinHandle<AgentResult>) -> Self {
        Self {
            agent_id,
            name,
            handle,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}
