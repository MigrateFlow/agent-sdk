use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use tracing::debug;

use sdk_core::error::{AgentId, SdkError, SdkResult};
use sdk_core::types::message::{Envelope, MessageTarget};

use super::mailbox::Mailbox;

pub struct MessageBroker {
    base_dir: PathBuf,
    agent_dirs: RwLock<HashMap<AgentId, PathBuf>>,
    team_lead_dir: PathBuf,
}

impl MessageBroker {
    pub fn new(base_dir: PathBuf) -> SdkResult<Self> {
        let team_lead_dir = base_dir.join("team-lead");
        std::fs::create_dir_all(&team_lead_dir)?;
        std::fs::create_dir_all(base_dir.join("agents"))?;

        Ok(Self {
            base_dir,
            agent_dirs: RwLock::new(HashMap::new()),
            team_lead_dir,
        })
    }

    pub fn register_agent(&self, agent_id: AgentId) -> SdkResult<()> {
        let agent_dir = self.base_dir.join("agents").join(agent_id.to_string());
        std::fs::create_dir_all(&agent_dir)?;

        let mut dirs = self.agent_dirs.write().map_err(|_| {
            SdkError::Config("Failed to acquire agent dirs write lock".to_string())
        })?;
        dirs.insert(agent_id, agent_dir);

        debug!(agent_id = %agent_id, "Registered agent mailbox");
        Ok(())
    }

    pub fn route(&self, envelope: &Envelope) -> SdkResult<()> {
        match &envelope.to {
            MessageTarget::TeamLead => {
                let mailbox = Mailbox::new(&self.team_lead_dir)?;
                mailbox.send(envelope)?;
            }
            MessageTarget::Agent(agent_id) => {
                let dirs = self.agent_dirs.read().map_err(|_| {
                    SdkError::Config("Failed to acquire agent dirs read lock".to_string())
                })?;
                if let Some(dir) = dirs.get(agent_id) {
                    let mailbox = Mailbox::new(dir)?;
                    mailbox.send(envelope)?;
                } else {
                    return Err(SdkError::AgentCrashed {
                        agent_id: *agent_id,
                        reason: "Agent mailbox not found".to_string(),
                    });
                }
            }
            MessageTarget::Broadcast => {
                let mailbox = Mailbox::new(&self.team_lead_dir)?;
                mailbox.send(envelope)?;

                let dirs = self.agent_dirs.read().map_err(|_| {
                    SdkError::Config("Failed to acquire agent dirs read lock".to_string())
                })?;
                for (agent_id, dir) in dirs.iter() {
                    if *agent_id != envelope.from {
                        let mailbox = Mailbox::new(dir)?;
                        mailbox.send(envelope)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn team_lead_mailbox(&self) -> SdkResult<Mailbox> {
        Mailbox::new(&self.team_lead_dir)
    }

    pub fn agent_mailbox(&self, agent_id: AgentId) -> SdkResult<Mailbox> {
        let dirs = self.agent_dirs.read().map_err(|_| {
            SdkError::Config("Failed to acquire agent dirs read lock".to_string())
        })?;
        if let Some(dir) = dirs.get(&agent_id) {
            Mailbox::new(dir)
        } else {
            Err(SdkError::AgentCrashed {
                agent_id,
                reason: "Agent mailbox not found".to_string(),
            })
        }
    }

    pub fn registered_agents(&self) -> SdkResult<Vec<AgentId>> {
        let dirs = self.agent_dirs.read().map_err(|_| {
            SdkError::Config("Failed to acquire agent dirs read lock".to_string())
        })?;
        Ok(dirs.keys().copied().collect())
    }

    pub fn clear_all(&self) -> SdkResult<()> {
        let mut mailbox = Mailbox::new(&self.team_lead_dir)?;
        mailbox.clear()?;

        let dirs = self.agent_dirs.read().map_err(|_| {
            SdkError::Config("Failed to acquire agent dirs read lock".to_string())
        })?;
        for dir in dirs.values() {
            let mut mailbox = Mailbox::new(dir)?;
            mailbox.clear()?;
        }
        Ok(())
    }
}
