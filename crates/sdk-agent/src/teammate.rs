use std::time::Duration;

use tracing::{debug, error, info, warn};

use sdk_core::error::SdkResult;
use sdk_core::types::file_change::{ChangeType, FileChange};
use sdk_core::types::message::{
    Envelope, MessageKind, MessageTarget, PlanSubmissionPayload, ShutdownRejectedPayload,
    TaskCompletePayload, TaskFailedPayload, TeammateIdlePayload,
};
use sdk_core::types::task::{Task, TaskResult};
use crate::builder::{CommandToolPolicy, DefaultToolsetBuilder};
use sdk_core::registry::ToolRegistry;

use crate::agent_loop::AgentLoop;
use crate::context::AgentContext;
use sdk_core::events::AgentEvent;
use crate::handle::AgentResult;
use sdk_core::hooks::{HookEvent, HookResult};

pub struct Teammate {
    ctx: AgentContext,
}

impl Teammate {
    pub fn new(ctx: AgentContext) -> Self {
        Self { ctx }
    }

    /// Build the default tool registry. Override via `PromptBuilder::customize_tools`.
    fn build_tool_registry(&self) -> ToolRegistry {
        DefaultToolsetBuilder::new()
            .add_core_tools(
                self.ctx.source_root.clone(),
                self.ctx.work_dir.clone(),
                CommandToolPolicy::Unrestricted,
            )
            .add_memory_tools(self.ctx.memory_store.clone(), self.ctx.agent_id)
            .add_task_context_tools(self.ctx.task_store.clone())
            .build()
    }

    pub async fn run(self) -> AgentResult {
        let agent_id = self.ctx.agent_id;
        let name = self.ctx.name.clone();
        info!(agent_id = %agent_id, name = %name, "Teammate started");

        let mut tasks_completed = 0;
        let mut tasks_failed = 0;
        let mut total_tokens = 0u64;
        let mut idle_cycles = 0u32;
        let max_idle_cycles = self.ctx.max_idle_cycles;

        let mut mailbox = match self.ctx.broker.agent_mailbox(agent_id) {
            Ok(m) => m,
            Err(e) => {
                error!(agent_id = %agent_id, error = %e, "Failed to open mailbox");
                return AgentResult {
                    agent_id,
                    name,
                    tasks_completed,
                    tasks_failed,
                    total_tokens_used: total_tokens,
                };
            }
        };

        loop {
            // --- Check mailbox for messages ---
            if let Ok(messages) = mailbox.recv() {
                for msg in &messages {
                    match msg.kind {
                        MessageKind::Shutdown => {
                            info!(agent_id = %agent_id, "Received shutdown (immediate)");
                            self.emit(AgentEvent::AgentShutdown { agent_id, name: name.clone() });
                            return AgentResult {
                                agent_id,
                                name,
                                tasks_completed,
                                tasks_failed,
                                total_tokens_used: total_tokens,
                            };
                        }
                        MessageKind::ShutdownRequest => {
                            // Shutdown negotiation: accept if idle, reject if working
                            let has_work = self.ctx.task_store
                                .completed_task_ids()
                                .map(|ids| {
                                    self.ctx
                                        .task_store
                                        .try_claim_next(agent_id, &self.ctx.name, &ids)
                                        .ok()
                                        .flatten()
                                        .is_some()
                                })
                                .unwrap_or(false);

                            if has_work {
                                info!(agent_id = %agent_id, "Rejecting shutdown: still have work");
                                let reply = Envelope::new(
                                    agent_id,
                                    MessageTarget::TeamLead,
                                    MessageKind::ShutdownRejected,
                                )
                                .with_payload(
                                    serde_json::to_value(ShutdownRejectedPayload {
                                        reason: "Still processing tasks".to_string(),
                                    })
                                    .unwrap_or_default(),
                                )
                                .in_reply_to(msg.id);
                                let _ = self.ctx.broker.route(&reply);
                                self.emit(AgentEvent::ShutdownRejected {
                                    agent_id,
                                    name: name.clone(),
                                    reason: "Still processing tasks".to_string(),
                                });
                            } else {
                                info!(agent_id = %agent_id, "Accepting shutdown");
                                let reply = Envelope::new(
                                    agent_id,
                                    MessageTarget::TeamLead,
                                    MessageKind::ShutdownAccepted,
                                )
                                .in_reply_to(msg.id);
                                let _ = self.ctx.broker.route(&reply);
                                self.emit(AgentEvent::ShutdownAccepted { agent_id, name: name.clone() });
                                self.emit(AgentEvent::AgentShutdown { agent_id, name: name.clone() });
                                return AgentResult {
                                    agent_id,
                                    name,
                                    tasks_completed,
                                    tasks_failed,
                                    total_tokens_used: total_tokens,
                                };
                            }
                        }
                        MessageKind::PlanApproved => {
                            debug!(agent_id = %agent_id, "Plan approved by lead");
                            // Plan approval is handled inside process_task
                        }
                        MessageKind::PlanRejected => {
                            debug!(agent_id = %agent_id, "Plan rejected by lead");
                        }
                        MessageKind::TeammateMessage => {
                            // Teammate-to-teammate messages can be read from context
                            debug!(agent_id = %agent_id, from = %msg.from, "Received teammate message");
                        }
                        _ => {}
                    }
                }
            }

            // --- Try to claim and process a task ---
            let completed_ids = match self.ctx.task_store.completed_task_ids() {
                Ok(ids) => ids,
                Err(e) => {
                    warn!(error = %e, "Failed to get completed task IDs");
                    Vec::new()
                }
            };

            match self
                .ctx
                .task_store
                .try_claim_next(agent_id, &self.ctx.name, &completed_ids)
            {
                Ok(Some(task)) => {
                    idle_cycles = 0;
                    let task_id = task.id;
                    info!(agent_id = %agent_id, task_id = %task_id, "Processing task: {}", task.title);

                    self.emit(AgentEvent::TaskStarted {
                        agent_id,
                        name: name.clone(),
                        task_id,
                        title: task.title.clone(),
                    });

                    if let Err(e) = self.ctx.task_store.mark_in_progress(task_id, agent_id) {
                        warn!(error = %e, "Failed to mark task in-progress");
                    }

                    match self.process_task(&task).await {
                        Ok((result, tokens)) => {
                            total_tokens += tokens;

                            let tool_calls = result.tool_calls_count;
                            let iterations = result.conversation_log.len() / 2;

                            // Run TaskCompleted hook
                            let hook_result = self.ctx.hooks.evaluate(&HookEvent::TaskCompleted {
                                task: task.clone(),
                                agent_id,
                            });

                            if let HookResult::Reject { feedback } = hook_result {
                                warn!(task_id = %task_id, "TaskCompleted hook rejected: {}", feedback);
                                self.emit(AgentEvent::HookRejected {
                                    event_name: "TaskCompleted".to_string(),
                                    feedback: feedback.clone(),
                                });
                                // Hook rejected completion — mark as failed so it retries
                                if let Err(e) = self.ctx.task_store.fail_task(
                                    task_id,
                                    agent_id,
                                    format!("Hook rejected: {}", feedback),
                                ) {
                                    error!(error = %e, "Failed to mark task as failed after hook rejection");
                                }
                                tasks_failed += 1;
                                continue;
                            }

                            if let Err(e) =
                                self.ctx
                                    .task_store
                                    .complete_task(task_id, agent_id, result)
                            {
                                error!(error = %e, "Failed to complete task");
                                tasks_failed += 1;
                            } else {
                                tasks_completed += 1;

                                self.emit(AgentEvent::TaskCompleted {
                                    agent_id,
                                    name: name.clone(),
                                    task_id,
                                    tokens_used: tokens,
                                    iterations,
                                    tool_calls,
                                });

                                let envelope = Envelope::new(
                                    agent_id,
                                    MessageTarget::TeamLead,
                                    MessageKind::TaskComplete,
                                )
                                .with_payload(
                                    serde_json::to_value(TaskCompletePayload {
                                        task_id,
                                        tokens_used: tokens,
                                    })
                                    .unwrap_or_default(),
                                );

                                if let Err(e) = self.ctx.broker.route(&envelope) {
                                    warn!(error = %e, "Failed to send completion message");
                                }
                            }
                        }
                        Err(e) => {
                            error!(task_id = %task_id, error = %e, "Task processing failed");

                            self.emit(AgentEvent::TaskFailed {
                                agent_id,
                                name: name.clone(),
                                task_id,
                                error: e.to_string(),
                            });

                            let error_msg = e.to_string();
                            if let Err(e2) = self.ctx.task_store.fail_task(
                                task_id,
                                agent_id,
                                error_msg.clone(),
                            ) {
                                error!(error = %e2, "Failed to mark task as failed");
                            }
                            tasks_failed += 1;

                            let envelope = Envelope::new(
                                agent_id,
                                MessageTarget::TeamLead,
                                MessageKind::TaskFailed,
                            )
                            .with_payload(
                                serde_json::to_value(TaskFailedPayload {
                                    task_id,
                                    error: error_msg,
                                    retryable: true,
                                })
                                .unwrap_or_default(),
                            );

                            if let Err(e) = self.ctx.broker.route(&envelope) {
                                warn!(error = %e, "Failed to send failure message");
                            }
                        }
                    }
                }
                Ok(None) => {
                    idle_cycles += 1;

                    // Notify lead when going idle (first time)
                    if idle_cycles == 1 {
                        // Run TeammateIdle hook
                        let hook_result = self.ctx.hooks.evaluate(&HookEvent::TeammateIdle {
                            agent_id,
                            name: self.ctx.name.clone(),
                            tasks_completed,
                        });

                        if let HookResult::Reject { feedback } = hook_result {
                            debug!(agent_id = %agent_id, "TeammateIdle hook: keep working - {}", feedback);
                            self.emit(AgentEvent::HookRejected {
                                event_name: "TeammateIdle".to_string(),
                                feedback,
                            });
                            idle_cycles = 0; // reset to keep trying
                            continue;
                        }

                        self.emit(AgentEvent::TeammateIdle {
                            agent_id,
                            name: name.clone(),
                            tasks_completed,
                        });

                        let envelope = Envelope::new(
                            agent_id,
                            MessageTarget::TeamLead,
                            MessageKind::TeammateIdle,
                        )
                        .with_payload(
                            serde_json::to_value(TeammateIdlePayload { tasks_completed })
                                .unwrap_or_default(),
                        );
                        let _ = self.ctx.broker.route(&envelope);
                    }

                    if idle_cycles >= max_idle_cycles {
                        debug!(agent_id = %agent_id, "No more tasks, exiting");
                        break;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Error claiming task");
                    idle_cycles += 1;
                }
            }

            tokio::time::sleep(Duration::from_millis(self.ctx.poll_interval_ms)).await;
        }

        self.emit(AgentEvent::AgentShutdown { agent_id, name: name.clone() });
        AgentResult {
            agent_id,
            name,
            tasks_completed,
            tasks_failed,
            total_tokens_used: total_tokens,
        }
    }

    async fn process_task(&self, task: &Task) -> SdkResult<(TaskResult, u64)> {
        let base_tools = self.build_tool_registry();
        let tools = self.ctx.prompt_builder.customize_tools(task, base_tools);
        let mut system_prompt = self.ctx.prompt_builder.build_system_prompt(
            task,
            &self.ctx.source_root,
            &self.ctx.work_dir,
        );
        if !self.ctx.role_prompt.trim().is_empty() {
            system_prompt.push_str(&sdk_core::prompts::teammate_role_suffix(&self.ctx.role_prompt));
        }
        let user_message = self.ctx.prompt_builder.build_user_message(task);

        // --- Plan mode: if required, first generate a plan, wait for approval ---
        if self.ctx.require_plan_approval {
            let plan = self.generate_plan(task, &system_prompt).await?;

            self.emit(AgentEvent::PlanSubmitted {
                agent_id: self.ctx.agent_id,
                name: self.ctx.name.clone(),
                task_id: task.id,
                plan_preview: truncate(&plan, 200),
            });

            // Send plan to lead for approval
            let envelope = Envelope::new(
                self.ctx.agent_id,
                MessageTarget::TeamLead,
                MessageKind::PlanSubmission,
            )
            .with_payload(
                serde_json::to_value(PlanSubmissionPayload {
                    task_id: task.id,
                    plan: plan.clone(),
                })
                .unwrap_or_default(),
            );
            self.ctx.broker.route(&envelope)?;

            // Wait for approval or rejection
            self.wait_for_plan_decision(task).await?;
        }

        // --- Execute the task ---
        let agent_loop = AgentLoop::new(
            self.ctx.agent_id,
            self.ctx.llm_client.clone(),
            tools,
            system_prompt,
            self.ctx.max_loop_iterations,
        )
        .with_max_context_tokens(self.ctx.max_context_tokens)
        .with_agent_name(&self.ctx.name);
        let mut agent_loop = agent_loop;

        if let Some(ref tx) = self.ctx.event_tx {
            agent_loop.set_event_sink(tx.clone());
        }

        let loop_result = agent_loop.run(user_message).await?;

        let output_files = self.collect_written_files(&loop_result.messages);

        let result = TaskResult {
            file_changes: output_files,
            notes: loop_result.final_content.clone(),
            llm_tokens_used: loop_result.total_tokens,
            conversation_log: loop_result.messages,
            tool_calls_count: loop_result.tool_calls_count,
            extra: None,
        };

        Ok((result, loop_result.total_tokens))
    }

    /// Generate a read-only plan for a task (plan mode).
    async fn generate_plan(&self, task: &Task, system_prompt: &str) -> SdkResult<String> {
        let plan_prompt = sdk_core::prompts::plan_mode_prompt(system_prompt, task);

        let (plan, _tokens) = self.ctx.llm_client.ask(&plan_prompt, &task.description).await?;
        Ok(plan)
    }

    /// Wait for the lead to approve or reject the plan.
    async fn wait_for_plan_decision(&self, task: &Task) -> SdkResult<()> {
        let agent_id = self.ctx.agent_id;
        let mut mailbox = self.ctx.broker.agent_mailbox(agent_id)?;
        let max_wait = self.ctx.plan_approval_timeout_secs;

        for _ in 0..max_wait {
            if let Ok(messages) = mailbox.recv() {
                for msg in messages {
                    match msg.kind {
                        MessageKind::PlanApproved => {
                            info!(agent_id = %agent_id, task_id = %task.id, "Plan approved");
                            self.emit(AgentEvent::PlanApproved {
                                agent_id,
                                name: self.ctx.name.clone(),
                                task_id: task.id,
                            });
                            return Ok(());
                        }
                        MessageKind::PlanRejected => {
                            let feedback = msg.payload["feedback"]
                                .as_str()
                                .unwrap_or("No feedback")
                                .to_string();
                            info!(agent_id = %agent_id, task_id = %task.id, "Plan rejected: {}", feedback);
                            self.emit(AgentEvent::PlanRejected {
                                agent_id,
                                name: self.ctx.name.clone(),
                                task_id: task.id,
                                feedback,
                            });
                            // On rejection, generate a new plan (recursive retry)
                            // For now, just proceed with implementation
                            return Ok(());
                        }
                        MessageKind::Shutdown | MessageKind::ShutdownRequest => {
                            return Err(sdk_core::error::SdkError::AgentCrashed {
                                agent_id,
                                reason: "Shutdown while waiting for plan approval".to_string(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        // Timeout: proceed anyway
        warn!(agent_id = %agent_id, "Plan approval timed out, proceeding");
        Ok(())
    }

    fn collect_written_files(
        &self,
        messages: &[sdk_core::types::chat::ChatMessage],
    ) -> Vec<FileChange> {
        let mut changes = Vec::new();

        for msg in messages {
            if let sdk_core::types::chat::ChatMessage::Assistant { tool_calls, .. } = msg {
                for tc in tool_calls {
                    if tc.function.name == "write_file" {
                        if let Ok(args) =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        {
                            if let (Some(path), Some(content)) =
                                (args["path"].as_str(), args["content"].as_str())
                            {
                                changes.push(FileChange {
                                    path: std::path::PathBuf::from(path),
                                    change_type: ChangeType::Created,
                                    original_content: None,
                                    new_content: content.to_string(),
                                    hunks: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
        }

        changes
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.ctx.event_tx {
            let _ = tx.send(event);
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
