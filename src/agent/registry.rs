use std::collections::HashMap;

use tracing::{info, warn};

use crate::error::AgentId;
use super::handle::{AgentHandle, AgentResult};

pub struct AgentRegistry {
    agents: HashMap<AgentId, AgentHandle>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    pub fn register(&mut self, handle: AgentHandle) {
        info!(agent_id = %handle.agent_id, "Agent registered");
        self.agents.insert(handle.agent_id, handle);
    }

    pub async fn collect_finished(&mut self) -> Vec<Result<AgentResult, (AgentId, String)>> {
        let finished_ids: Vec<AgentId> = self
            .agents
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| *id)
            .collect();

        let mut results = Vec::new();

        for id in finished_ids {
            if let Some(handle) = self.agents.remove(&id) {
                match handle.handle.await {
                    Ok(result) => {
                        info!(
                            agent_id = %id,
                            tasks_completed = result.tasks_completed,
                            tasks_failed = result.tasks_failed,
                            "Agent finished"
                        );
                        results.push(Ok(result));
                    }
                    Err(e) => {
                        warn!(agent_id = %id, error = %e, "Agent panicked");
                        results.push(Err((id, format!("Agent panicked: {}", e))));
                    }
                }
            }
        }

        results
    }

    pub fn active_count(&self) -> usize {
        self.agents.len()
    }

    pub fn active_agent_ids(&self) -> Vec<AgentId> {
        self.agents.keys().copied().collect()
    }

    pub async fn wait_all(&mut self) -> Vec<Result<AgentResult, (AgentId, String)>> {
        let mut results = Vec::new();
        let agents: HashMap<AgentId, AgentHandle> = std::mem::take(&mut self.agents);

        for (id, handle) in agents {
            match handle.handle.await {
                Ok(result) => results.push(Ok(result)),
                Err(e) => results.push(Err((id, format!("Agent panicked: {}", e)))),
            }
        }

        results
    }
}
