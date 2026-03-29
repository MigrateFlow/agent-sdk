use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::error::SdkResult;
use crate::types::file_change::{ChangeType, FileChange};
use crate::types::message::{
    Envelope, MessageKind, MessageTarget, TaskCompletePayload, TaskFailedPayload,
};
use crate::types::task::{Task, TaskResult};
use crate::tools::command_tools::RunCommandTool;
use crate::tools::context_tools::{GetTaskContextTool, ListCompletedTasksTool};
use crate::tools::fs_tools::{ListDirectoryTool, ReadFileTool, WriteFileTool};
use crate::tools::memory_tools::{ListMemoryTool, ReadMemoryTool, WriteMemoryTool};
use crate::tools::registry::ToolRegistry;
use crate::tools::search_tools::SearchFilesTool;

use super::agent_loop::AgentLoop;
use super::context::AgentContext;
use super::events::AgentEvent;
use super::handle::AgentResult;

pub struct Teammate {
    ctx: AgentContext,
}

impl Teammate {
    pub fn new(ctx: AgentContext) -> Self {
        Self { ctx }
    }

    /// Build the default tool registry. Override via `PromptBuilder::customize_tools`.
    fn build_tool_registry(&self) -> ToolRegistry {
        let mut registry = ToolRegistry::new();

        registry.register(Arc::new(ReadFileTool {
            source_root: self.ctx.source_root.clone(),
            work_dir: self.ctx.work_dir.clone(),
        }));
        registry.register(Arc::new(WriteFileTool {
            work_dir: self.ctx.work_dir.clone(),
        }));
        registry.register(Arc::new(ListDirectoryTool {
            source_root: self.ctx.source_root.clone(),
            work_dir: self.ctx.work_dir.clone(),
        }));
        registry.register(Arc::new(SearchFilesTool {
            source_root: self.ctx.source_root.clone(),
        }));
        registry.register(Arc::new(RunCommandTool::with_defaults(
            self.ctx.work_dir.clone(),
        )));
        registry.register(Arc::new(ReadMemoryTool {
            memory_store: self.ctx.memory_store.clone(),
        }));
        registry.register(Arc::new(WriteMemoryTool {
            memory_store: self.ctx.memory_store.clone(),
            agent_id: self.ctx.agent_id,
        }));
        registry.register(Arc::new(ListMemoryTool {
            memory_store: self.ctx.memory_store.clone(),
        }));
        registry.register(Arc::new(GetTaskContextTool {
            task_store: self.ctx.task_store.clone(),
        }));
        registry.register(Arc::new(ListCompletedTasksTool {
            task_store: self.ctx.task_store.clone(),
        }));

        registry
    }

    pub async fn run(self) -> AgentResult {
        let agent_id = self.ctx.agent_id;
        info!(agent_id = %agent_id, "Teammate started");

        let mut tasks_completed = 0;
        let mut tasks_failed = 0;
        let mut total_tokens = 0u64;
        let mut idle_cycles = 0u32;
        let max_idle_cycles = 50;

        let mut mailbox = match self.ctx.broker.agent_mailbox(agent_id) {
            Ok(m) => m,
            Err(e) => {
                error!(agent_id = %agent_id, error = %e, "Failed to open mailbox");
                return AgentResult {
                    agent_id,
                    tasks_completed,
                    tasks_failed,
                    total_tokens_used: total_tokens,
                };
            }
        };

        loop {
            if let Ok(messages) = mailbox.recv() {
                for msg in &messages {
                    if msg.kind == MessageKind::Shutdown {
                        info!(agent_id = %agent_id, "Received shutdown signal");
                        self.emit(AgentEvent::AgentShutdown { agent_id });
                        return AgentResult {
                            agent_id,
                            tasks_completed,
                            tasks_failed,
                            total_tokens_used: total_tokens,
                        };
                    }
                }
            }

            let completed_ids = match self.ctx.task_store.completed_task_ids() {
                Ok(ids) => ids,
                Err(e) => {
                    warn!(error = %e, "Failed to get completed task IDs");
                    Vec::new()
                }
            };

            match self.ctx.task_store.try_claim_next(agent_id, &completed_ids) {
                Ok(Some(task)) => {
                    idle_cycles = 0;
                    let task_id = task.id;
                    info!(agent_id = %agent_id, task_id = %task_id, "Processing task: {}", task.title);

                    self.emit(AgentEvent::TaskStarted {
                        agent_id,
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

        self.emit(AgentEvent::AgentShutdown { agent_id });
        AgentResult {
            agent_id,
            tasks_completed,
            tasks_failed,
            total_tokens_used: total_tokens,
        }
    }

    async fn process_task(&self, task: &Task) -> SdkResult<(TaskResult, u64)> {
        let base_tools = self.build_tool_registry();
        let tools = self.ctx.prompt_builder.customize_tools(task, base_tools);
        let system_prompt = self.ctx.prompt_builder.build_system_prompt(
            task,
            &self.ctx.source_root,
            &self.ctx.work_dir,
        );
        let user_message = self.ctx.prompt_builder.build_user_message(task);

        let mut agent_loop = AgentLoop::new(
            self.ctx.agent_id,
            self.ctx.llm_client.clone(),
            tools,
            system_prompt,
            self.ctx.max_loop_iterations,
        );

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

    fn collect_written_files(
        &self,
        messages: &[crate::types::chat::ChatMessage],
    ) -> Vec<FileChange> {
        let mut changes = Vec::new();

        for msg in messages {
            if let crate::types::chat::ChatMessage::Assistant { tool_calls, .. } = msg {
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
