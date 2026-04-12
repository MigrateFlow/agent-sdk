use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use serde::Serialize;

use sdk_core::config::AgentConfig;
use sdk_core::error::{AgentId, SdkResult};
use sdk_core::traits::llm_client::LlmClient;
use sdk_core::traits::prompt_builder::PromptBuilder;
use sdk_core::types::message::{
    Envelope, MessageKind, MessageTarget, PlanSubmissionPayload, TaskCompletePayload,
    TaskFailedPayload, TeammateIdlePayload,
};
use sdk_task::mailbox::broker::MessageBroker;
use sdk_task::task::store::TaskStore;

use crate::context::AgentContext;
use sdk_core::events::AgentEvent;
use crate::handle::AgentHandle;
use sdk_core::hooks::HookRegistry;
use sdk_core::memory::MemoryStore;
use crate::registry::AgentRegistry;
use crate::teammate::Teammate;

/// Specification for a teammate to be spawned.
#[derive(Debug, Clone)]
pub struct TeammateSpec {
    pub name: String,
    pub prompt: String,
    pub require_plan_approval: bool,
    pub isolation: Option<crate::worktree::IsolationMode>,
    /// Optional model override. When set, a dedicated `LlmClient` is created
    /// for this teammate. When `None`, the teammate shares the lead's client.
    pub model: Option<String>,
}

pub struct TeamLead {
    pub id: AgentId,
    pub team_name: String,
    pub team_config_path: std::path::PathBuf,
    pub task_store: Arc<TaskStore>,
    pub broker: Arc<MessageBroker>,
    pub llm_client: Arc<dyn LlmClient>,
    pub llm_config: sdk_core::config::LlmConfig,
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
    /// High-level team goal threaded in from `AgentTeam::run(goal)`.
    /// When non-empty, it is prepended to each teammate's role prompt so
    /// every teammate sees the shared objective in its system prompt.
    pub team_goal: String,
}

#[derive(Debug, Serialize)]
struct TeamConfigFile {
    team_name: String,
    lead_id: AgentId,
    work_dir: std::path::PathBuf,
    source_root: std::path::PathBuf,
    members: Vec<TeamConfigMember>,
}

#[derive(Debug, Clone, Serialize)]
struct TeamConfigMember {
    name: String,
    agent_id: AgentId,
    agent_type: String,
    require_plan_approval: bool,
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
        self.write_team_config(&[])?;

        let mut registry = AgentRegistry::new();
        let mut total_tokens = 0u64;
        let mut agents_spawned = 0usize;
        // Map agent IDs to human-readable names for event display.
        let mut name_map = std::collections::HashMap::<AgentId, String>::new();
        let mut team_members = Vec::<TeamConfigMember>::new();
        // Track worktree handles for cleanup during shutdown.
        let mut worktree_handles = std::collections::HashMap::<AgentId, crate::worktree::WorktreeHandle>::new();

        let mut lead_mailbox = self.broker.team_lead_mailbox()?;

        // Spawn teammates: use explicit specs if provided, otherwise generic
        if !self.teammate_specs.is_empty() {
            for spec in &self.teammate_specs {
                match self.spawn_named_teammate(spec, &mut worktree_handles).await {
                    Ok(handle) => {
                        let wt_path = worktree_handles.get(&handle.agent_id)
                            .map(|h| h.path.to_string_lossy().to_string());
                        name_map.insert(handle.agent_id, handle.name.clone());
                        team_members.push(TeamConfigMember {
                            name: handle.name.clone(),
                            agent_id: handle.agent_id,
                            agent_type: "teammate".to_string(),
                            require_plan_approval: spec.require_plan_approval,
                        });
                        self.write_team_config(&team_members)?;
                        self.emit(AgentEvent::TeammateSpawned {
                            agent_id: handle.agent_id,
                            name: handle.name.clone(),
                            worktree_path: wt_path,
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
                match self.spawn_teammate(&name, String::new(), false, None, None, &mut worktree_handles).await {
                    Ok(handle) => {
                        name_map.insert(handle.agent_id, handle.name.clone());
                        team_members.push(TeamConfigMember {
                            name: handle.name.clone(),
                            agent_id: handle.agent_id,
                            agent_type: "teammate".to_string(),
                            require_plan_approval: false,
                        });
                        self.write_team_config(&team_members)?;
                        self.emit(AgentEvent::TeammateSpawned {
                            agent_id: handle.agent_id,
                            name: handle.name.clone(),
                            worktree_path: None,
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

            // Spawn replacement teammates only in generic mode (no named specs).
            // When named specs are provided, the original teammates are sufficient --
            // don't keep spawning generic teammate-N agents.
            if self.teammate_specs.is_empty() {
                let active = registry.active_count();
                let max_agents = self.config.max_parallel_agents;
                if active < max_agents && summary.pending > 0 {
                    let name = format!("teammate-{}", agents_spawned + 1);
                    match self.spawn_teammate(&name, String::new(), false, None, None, &mut worktree_handles).await {
                        Ok(handle) => {
                            name_map.insert(handle.agent_id, handle.name.clone());
                            team_members.push(TeamConfigMember {
                                name: handle.name.clone(),
                                agent_id: handle.agent_id,
                                agent_type: "teammate".to_string(),
                                require_plan_approval: false,
                            });
                            self.write_team_config(&team_members)?;
                            self.emit(AgentEvent::TeammateSpawned {
                                agent_id: handle.agent_id,
                                name: handle.name.clone(),
                                worktree_path: None,
                            });
                            registry.register(handle);
                            agents_spawned += 1;
                            info!("Spawned replacement teammate for pending tasks");
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to spawn replacement teammate");
                        }
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

        // Clean up worktrees: preserve those with uncommitted changes.
        for (_agent_id, handle) in &worktree_handles {
            let has_changes = crate::worktree::has_uncommitted_changes(&handle.path).await;
            if let Err(e) = crate::worktree::cleanup_worktree(&self.source_root, handle, has_changes).await {
                warn!(path = %handle.path.display(), error = %e, "Failed to clean up worktree");
            } else if has_changes {
                info!(path = %handle.path.display(), branch = %handle.branch, "Worktree preserved (has uncommitted changes)");
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

        let review_prompt = sdk_core::prompts::plan_review_user_prompt(&payload.task_id, &payload.plan);

        let decision = match self.llm_client.ask(sdk_core::prompts::plan_review_system_prompt(), &review_prompt).await {
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
        isolation: Option<crate::worktree::IsolationMode>,
        model: Option<String>,
        worktree_handles: &mut std::collections::HashMap<AgentId, crate::worktree::WorktreeHandle>,
    ) -> SdkResult<AgentHandle> {
        let agent_id = Uuid::new_v4();
        self.broker.register_agent(agent_id)?;

        let effective_role_prompt = if self.team_goal.is_empty() {
            role_prompt
        } else if role_prompt.trim().is_empty() {
            format!("Team goal: {}", self.team_goal)
        } else {
            format!("Team goal: {}\n\n{}", self.team_goal, role_prompt)
        };

        let teammate_work_dir = if isolation == Some(crate::worktree::IsolationMode::Worktree) {
            let handle = crate::worktree::create_worktree(&self.source_root, agent_id, name).await?;
            let wt_path = handle.path.clone();
            worktree_handles.insert(agent_id, handle);
            wt_path
        } else {
            self.work_dir.clone()
        };

        let teammate_llm_client = if let Some(ref model_name) = model {
            let mut cfg = self.llm_config.clone();
            cfg.model = model_name.clone();
            sdk_llm::create_client(&cfg)?
        } else {
            self.llm_client.clone()
        };

        let ctx = AgentContext {
            agent_id,
            name: name.to_string(),
            role_prompt: effective_role_prompt,
            task_store: self.task_store.clone(),
            broker: self.broker.clone(),
            llm_client: teammate_llm_client,
            prompt_builder: self.prompt_builder.clone(),
            work_dir: teammate_work_dir,
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

    async fn spawn_named_teammate(
        &self,
        spec: &TeammateSpec,
        worktree_handles: &mut std::collections::HashMap<AgentId, crate::worktree::WorktreeHandle>,
    ) -> SdkResult<AgentHandle> {
        self.spawn_teammate(
            &spec.name,
            spec.prompt.clone(),
            spec.require_plan_approval,
            spec.isolation.clone(),
            spec.model.clone(),
            worktree_handles,
        )
            .await
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }

    fn write_team_config(&self, members: &[TeamConfigMember]) -> SdkResult<()> {
        if let Some(parent) = self.team_config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let config = TeamConfigFile {
            team_name: self.team_name.clone(),
            lead_id: self.id,
            work_dir: self.work_dir.clone(),
            source_root: self.source_root.clone(),
            members: members.to_vec(),
        };

        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&self.team_config_path, content)?;
        Ok(())
    }
}
