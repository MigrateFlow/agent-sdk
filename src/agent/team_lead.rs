use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::AgentConfig;
use crate::error::{AgentId, SdkResult};
use crate::traits::llm_client::LlmClient;
use crate::traits::prompt_builder::PromptBuilder;
use crate::types::message::{
    Envelope, MessageKind, MessageTarget, TaskCompletePayload, TaskFailedPayload,
};
use crate::mailbox::broker::MessageBroker;
use crate::task::store::TaskStore;

use super::context::AgentContext;
use super::events::AgentEvent;
use super::handle::AgentHandle;
use super::memory::MemoryStore;
use super::registry::AgentRegistry;
use super::teammate::Teammate;

pub struct TeamLead {
    pub id: AgentId,
    pub task_store: Arc<TaskStore>,
    pub broker: Arc<MessageBroker>,
    pub llm_client: Arc<dyn LlmClient>,
    pub prompt_builder: Arc<dyn PromptBuilder>,
    pub config: AgentConfig,
    pub source_root: std::path::PathBuf,
    pub work_dir: std::path::PathBuf,
    pub memory_store: Arc<MemoryStore>,
    pub event_tx: Option<UnboundedSender<AgentEvent>>,
}

#[derive(Debug)]
pub struct ExecutionSummary {
    pub total_tasks: usize,
    pub tasks_completed: usize,
    pub tasks_failed: usize,
    pub total_tokens_used: u64,
    pub agents_spawned: usize,
}

impl TeamLead {
    pub async fn run(&self) -> SdkResult<ExecutionSummary> {
        info!(lead_id = %self.id, "Team lead starting orchestration");

        let mut registry = AgentRegistry::new();
        let mut total_tokens = 0u64;
        let mut agents_spawned = 0usize;

        let mut lead_mailbox = self.broker.team_lead_mailbox()?;

        let initial_count = self.config.max_parallel_agents;
        for _ in 0..initial_count {
            match self.spawn_teammate().await {
                Ok(handle) => {
                    registry.register(handle);
                    agents_spawned += 1;
                }
                Err(e) => {
                    error!(error = %e, "Failed to spawn teammate");
                }
            }
        }

        info!(count = agents_spawned, "Teammates spawned");

        loop {
            let summary = self.task_store.summary()?;

            debug!(
                pending = summary.pending,
                in_progress = summary.in_progress,
                completed = summary.completed,
                failed = summary.failed,
                active_agents = registry.active_count(),
                "Status update"
            );

            if summary.is_done() {
                info!("All tasks processed, shutting down agents");
                break;
            }

            if let Ok(messages) = lead_mailbox.recv() {
                for msg in messages {
                    match msg.kind {
                        MessageKind::TaskComplete => {
                            if let Ok(payload) =
                                serde_json::from_value::<TaskCompletePayload>(msg.payload.clone())
                            {
                                total_tokens += payload.tokens_used;
                                debug!(task_id = %payload.task_id, "Task completed notification");
                            }
                        }
                        MessageKind::TaskFailed => {
                            if let Ok(payload) =
                                serde_json::from_value::<TaskFailedPayload>(msg.payload.clone())
                            {
                                warn!(
                                    task_id = %payload.task_id,
                                    error = %payload.error,
                                    "Task failed notification"
                                );
                            }
                        }
                        MessageKind::QuestionForLead => {
                            debug!(from = %msg.from, "Question from teammate (auto-responding)");
                            let reply = Envelope::new(
                                self.id,
                                MessageTarget::Agent(msg.from),
                                MessageKind::AnswerFromLead,
                            )
                            .in_reply_to(msg.id);
                            let _ = self.broker.route(&reply);
                        }
                        _ => {}
                    }
                }
            }

            let results = registry.collect_finished().await;
            for result in results {
                match result {
                    Ok(agent_result) => {
                        total_tokens += agent_result.total_tokens_used;
                    }
                    Err((crashed_id, reason)) => {
                        warn!(agent_id = %crashed_id, reason = %reason, "Agent crashed");
                    }
                }
            }

            if registry.active_count() < self.config.max_parallel_agents
                && (summary.pending > 0)
            {
                match self.spawn_teammate().await {
                    Ok(handle) => {
                        registry.register(handle);
                        agents_spawned += 1;
                        info!("Spawned replacement teammate");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to spawn replacement");
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(self.config.poll_interval_ms)).await;
        }

        let agent_ids = self.broker.registered_agents()?;
        for agent_id in &agent_ids {
            let shutdown =
                Envelope::new(self.id, MessageTarget::Agent(*agent_id), MessageKind::Shutdown);
            let _ = self.broker.route(&shutdown);
        }

        let final_results = registry.wait_all().await;
        for result in final_results {
            if let Ok(r) = result {
                total_tokens += r.total_tokens_used;
            }
        }

        let final_summary = self.task_store.summary()?;

        Ok(ExecutionSummary {
            total_tasks: final_summary.total(),
            tasks_completed: final_summary.completed,
            tasks_failed: final_summary.failed,
            total_tokens_used: total_tokens,
            agents_spawned,
        })
    }

    async fn spawn_teammate(&self) -> SdkResult<AgentHandle> {
        let agent_id = Uuid::new_v4();

        self.broker.register_agent(agent_id)?;

        let ctx = AgentContext {
            agent_id,
            task_store: self.task_store.clone(),
            broker: self.broker.clone(),
            llm_client: self.llm_client.clone(),
            prompt_builder: self.prompt_builder.clone(),
            work_dir: self.work_dir.clone(),
            source_root: self.source_root.clone(),
            poll_interval_ms: self.config.poll_interval_ms,
            memory_store: self.memory_store.clone(),
            max_loop_iterations: self.config.max_loop_iterations,
            event_tx: self.event_tx.clone(),
        };

        tokio::fs::create_dir_all(&ctx.work_dir).await?;

        let handle = tokio::spawn(async move {
            let teammate = Teammate::new(ctx);
            teammate.run().await
        });

        info!(agent_id = %agent_id, "Teammate spawned");
        Ok(AgentHandle::new(agent_id, handle))
    }
}
