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
    Envelope, MessageKind, MessageTarget, PlanSubmissionPayload, TaskCompletePayload,
    TaskFailedPayload, TeammateIdlePayload,
};
use crate::mailbox::broker::MessageBroker;
use crate::task::store::TaskStore;

use super::context::AgentContext;
use super::events::AgentEvent;
use super::handle::AgentHandle;
use super::hooks::HookRegistry;
use super::memory::MemoryStore;
use super::registry::AgentRegistry;
use super::teammate::Teammate;

/// Specification for a teammate to be spawned.
#[derive(Debug, Clone)]
pub struct TeammateSpec {
    pub name: String,
    pub prompt: String,
    pub require_plan_approval: bool,
}

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
    pub hooks: Arc<HookRegistry>,
    /// Named teammates to spawn. If empty, generic teammates are spawned
    /// up to `config.max_parallel_agents`.
    pub teammate_specs: Vec<TeammateSpec>,
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
        // Map agent IDs to human-readable names for event display.
        let mut name_map = std::collections::HashMap::<AgentId, String>::new();

        let mut lead_mailbox = self.broker.team_lead_mailbox()?;

        // Spawn teammates: use explicit specs if provided, otherwise generic
        if !self.teammate_specs.is_empty() {
            for spec in &self.teammate_specs {
                match self.spawn_named_teammate(spec).await {
                    Ok(handle) => {
                        name_map.insert(handle.agent_id, handle.name.clone());
                        self.emit(AgentEvent::TeammateSpawned {
                            agent_id: handle.agent_id,
                            name: handle.name.clone(),
                        });
                        registry.register(handle);
                        agents_spawned += 1;
                    }
                    Err(e) => {
                        error!(name = %spec.name, error = %e, "Failed to spawn teammate");
                    }
                }
            }
        } else {
            let initial_count = self.config.max_parallel_agents;
            for i in 0..initial_count {
                let name = format!("teammate-{}", i + 1);
                match self.spawn_teammate(&name, String::new(), false).await {
                    Ok(handle) => {
                        name_map.insert(handle.agent_id, handle.name.clone());
                        self.emit(AgentEvent::TeammateSpawned {
                            agent_id: handle.agent_id,
                            name: handle.name.clone(),
                        });
                        registry.register(handle);
                        agents_spawned += 1;
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to spawn teammate");
                    }
                }
            }
        }

        self.emit(AgentEvent::TeamSpawned {
            teammate_count: agents_spawned,
        });
        info!(count = agents_spawned, "Teammates spawned");

        // --- Main orchestration loop ---
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
                info!("All tasks processed, shutting down team");
                break;
            }

            // Process messages from teammates
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
                        MessageKind::PlanSubmission => {
                            self.handle_plan_submission(&msg, &name_map).await;
                        }
                        MessageKind::TeammateIdle => {
                            if let Ok(payload) =
                                serde_json::from_value::<TeammateIdlePayload>(msg.payload.clone())
                            {
                                debug!(
                                    from = %msg.from,
                                    tasks_completed = payload.tasks_completed,
                                    "Teammate idle"
                                );
                            }
                        }
                        MessageKind::ShutdownRejected => {
                            debug!(from = %msg.from, "Teammate rejected shutdown");
                        }
                        MessageKind::ShutdownAccepted => {
                            debug!(from = %msg.from, "Teammate accepted shutdown");
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

            // Collect results from finished agents
            let results = registry.collect_finished().await;
            for result in results {
                match result {
                    Ok(agent_result) => {
                        total_tokens += agent_result.total_tokens_used;
                        info!(
                            agent = %agent_result.name,
                            tasks = agent_result.tasks_completed,
                            "Teammate finished"
                        );
                    }
                    Err((crashed_id, reason)) => {
                        warn!(agent_id = %crashed_id, reason = %reason, "Teammate crashed");
                    }
                }
            }

            // Spawn replacement teammates if needed (only for generic mode)
            if self.teammate_specs.is_empty()
                && registry.active_count() < self.config.max_parallel_agents
                && (summary.pending > 0)
            {
                let name = format!("teammate-{}", agents_spawned + 1);
                match self.spawn_teammate(&name, String::new(), false).await {
                    Ok(handle) => {
                        name_map.insert(handle.agent_id, handle.name.clone());
                        self.emit(AgentEvent::TeammateSpawned {
                            agent_id: handle.agent_id,
                            name: handle.name.clone(),
                        });
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

        // --- Graceful shutdown ---
        let agent_ids = self.broker.registered_agents()?;
        for agent_id in &agent_ids {
            let shutdown = Envelope::new(
                self.id,
                MessageTarget::Agent(*agent_id),
                MessageKind::ShutdownRequest,
            );
            let _ = self.broker.route(&shutdown);
            self.emit(AgentEvent::ShutdownRequested {
                agent_id: *agent_id,
                name: name_map.get(agent_id).cloned().unwrap_or_default(),
            });
        }

        let final_results = registry.wait_all().await;
        for result in final_results.into_iter().flatten() {
            total_tokens += result.total_tokens_used;
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

    async fn handle_plan_submission(&self, msg: &Envelope, name_map: &std::collections::HashMap<AgentId, String>) {
        let payload: PlanSubmissionPayload = match serde_json::from_value(msg.payload.clone()) {
            Ok(p) => p,
            Err(_) => return,
        };

        info!(
            from = %msg.from,
            task_id = %payload.task_id,
            "Reviewing teammate plan"
        );

        let review_prompt = crate::prompts::plan_review_user_prompt(&payload.task_id, &payload.plan);

        let decision = match self.llm_client.ask(crate::prompts::plan_review_system_prompt(), &review_prompt).await {
            Ok((response, _)) => response,
            Err(e) => {
                warn!(error = %e, "Failed to review plan, auto-approving");
                "APPROVED".to_string()
            }
        };

        if decision.trim().starts_with("APPROVED") {
            let reply = Envelope::new(
                self.id,
                MessageTarget::Agent(msg.from),
                MessageKind::PlanApproved,
            )
            .in_reply_to(msg.id);
            let _ = self.broker.route(&reply);
            self.emit(AgentEvent::PlanApproved {
                agent_id: msg.from,
                name: name_map.get(&msg.from).cloned().unwrap_or_default(),
                task_id: payload.task_id,
            });
        } else {
            let feedback = decision
                .trim()
                .strip_prefix("REJECTED:")
                .unwrap_or(&decision)
                .trim()
                .to_string();
            let reply = Envelope::new(
                self.id,
                MessageTarget::Agent(msg.from),
                MessageKind::PlanRejected,
            )
            .with_payload(serde_json::json!({ "feedback": feedback }))
            .in_reply_to(msg.id);
            let _ = self.broker.route(&reply);
            self.emit(AgentEvent::PlanRejected {
                agent_id: msg.from,
                name: name_map.get(&msg.from).cloned().unwrap_or_default(),
                task_id: payload.task_id,
                feedback,
            });
        }
    }

    async fn spawn_teammate(
        &self,
        name: &str,
        role_prompt: String,
        require_plan_approval: bool,
    ) -> SdkResult<AgentHandle> {
        let agent_id = Uuid::new_v4();
        self.broker.register_agent(agent_id)?;

        let ctx = AgentContext {
            agent_id,
            name: name.to_string(),
            role_prompt,
            task_store: self.task_store.clone(),
            broker: self.broker.clone(),
            llm_client: self.llm_client.clone(),
            prompt_builder: self.prompt_builder.clone(),
            work_dir: self.work_dir.clone(),
            source_root: self.source_root.clone(),
            poll_interval_ms: self.config.poll_interval_ms,
            memory_store: self.memory_store.clone(),
            max_loop_iterations: self.config.max_loop_iterations,
            max_context_tokens: self.config.max_context_tokens,
            max_idle_cycles: self.config.max_idle_cycles,
            plan_approval_timeout_secs: self.config.plan_approval_timeout_secs,
            event_tx: self.event_tx.clone(),
            require_plan_approval,
            hooks: self.hooks.clone(),
        };

        tokio::fs::create_dir_all(&ctx.work_dir).await?;

        let teammate_name = name.to_string();
        let handle = tokio::spawn(async move {
            let teammate = Teammate::new(ctx);
            teammate.run().await
        });

        info!(agent_id = %agent_id, name = %name, "Teammate spawned");
        Ok(AgentHandle::new(agent_id, teammate_name, handle))
    }

    async fn spawn_named_teammate(&self, spec: &TeammateSpec) -> SdkResult<AgentHandle> {
        self.spawn_teammate(
            &spec.name,
            spec.prompt.clone(),
            spec.require_plan_approval,
        )
            .await
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}
