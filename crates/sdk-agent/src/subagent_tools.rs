use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use sdk_core::background::{BackgroundResult, BackgroundResultKind};
use sdk_core::events::AgentEvent;
use crate::subagent::{SubAgentDef, SubAgentRegistry, SubAgentRunner};
use crate::worktree::IsolationMode;
use sdk_core::error::{SdkError, SdkResult};
use sdk_core::traits::llm_client::LlmClient;
use sdk_core::traits::tool::{Tool, ToolDefinition};

/// Tool that lets the main agent spawn a subagent for a focused task.
///
/// The subagent runs in its own context window, does its work, and returns
/// results to the caller. Subagents cannot spawn other subagents.
///
/// The agent can either reference a registered subagent by name, or provide
/// an inline definition with a custom prompt and tool restrictions.
pub struct SpawnSubAgentTool {
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
    pub llm_client: Arc<dyn LlmClient>,
    pub event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    pub registry: Arc<SubAgentRegistry>,
    /// When set, background subagent results are sent back through this channel
    /// so the parent agent loop can inject them into its conversation.
    pub background_tx: Option<tokio::sync::mpsc::UnboundedSender<BackgroundResult>>,
}

#[derive(Debug, Deserialize)]
struct SubAgentRequest {
    /// Name of a registered subagent to use, OR a custom name for inline definition.
    name: String,
    /// The task/prompt to send to the subagent.
    prompt: String,
    /// Custom system prompt (for inline definition). If omitted and name matches
    /// a registered subagent, uses the registered definition.
    #[serde(default)]
    system_prompt: Option<String>,
    /// Optional description for inline definitions.
    #[serde(default)]
    description: Option<String>,
    /// Tool allowlist for inline definitions.
    #[serde(default)]
    allowed_tools: Vec<String>,
    /// Tool denylist for inline definitions.
    #[serde(default)]
    disallowed_tools: Vec<String>,
    /// Max agentic turns override.
    #[serde(default)]
    max_turns: Option<usize>,
    /// Run in background (concurrent). Default: false (foreground/blocking).
    #[serde(default)]
    background: bool,
    /// Isolation mode. Accepts "worktree" to run in a git worktree.
    #[serde(default)]
    isolation: Option<String>,
}

#[async_trait]
impl Tool for SpawnSubAgentTool {
    fn definition(&self) -> ToolDefinition {
        // Build the enum of available subagent names for the LLM
        let available: Vec<String> = self
            .registry
            .list()
            .iter()
            .map(|d| format!("{}: {}", d.name, d.description))
            .collect();

        let available_desc = if available.is_empty() {
            "No pre-registered subagents. Provide a system_prompt for inline definition.".to_string()
        } else {
            format!("Available subagents:\n{}", available.join("\n"))
        };

        ToolDefinition {
            name: "spawn_subagent".to_string(),
            description: format!(
                "Spawn a subagent to handle a focused task in its own context window. \
                The subagent works independently and returns results back to you. \
                Use this to preserve your main context by delegating exploration, \
                research, or self-contained tasks to a subagent.\n\n\
                You can reference a registered subagent by name, or create an inline \
                subagent by providing a system_prompt.\n\n\
                Subagents CANNOT spawn other subagents.\n\n\
                {available_desc}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the subagent. Use a registered name (e.g. 'explore', 'plan', 'general-purpose') or a custom name with system_prompt for inline definition."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task prompt to send to the subagent. Be specific about what you need."
                    },
                    "system_prompt": {
                        "type": "string",
                        "description": "Custom system prompt for an inline subagent definition. If omitted, uses the registered definition for the given name."
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional description for inline definitions."
                    },
                    "allowed_tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tool allowlist for inline definitions. Available: read_file, write_file, list_directory, search_files, web_search, run_command"
                    },
                    "disallowed_tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tool denylist. Tools listed here are removed from the available set."
                    },
                    "max_turns": {
                        "type": "integer",
                        "description": "Maximum agentic turns before the subagent stops (default: 30)."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "If true, run the subagent in the background (concurrent). Default: false (blocking)."
                    },
                    "isolation": {
                        "type": "string",
                        "enum": ["none", "worktree"],
                        "description": "Isolation mode. Use 'worktree' to run the subagent in an isolated git worktree so its file changes don't affect the main working tree. Default: 'none'."
                    }
                },
                "required": ["name", "prompt"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let request: SubAgentRequest =
            serde_json::from_value(arguments).map_err(|e| SdkError::ToolExecution {
                tool_name: "spawn_subagent".to_string(),
                message: format!("Invalid arguments: {}", e),
            })?;

        if request.prompt.trim().is_empty() {
            return Ok(json!({ "error": "prompt cannot be empty" }));
        }

        // Parse isolation mode from the request string.
        let isolation = match request.isolation.as_deref() {
            Some("worktree") => IsolationMode::Worktree,
            _ => IsolationMode::None,
        };

        // Resolve the subagent definition
        let def = if let Some(ref system_prompt) = request.system_prompt {
            // Inline definition
            let mut def = SubAgentDef::new(
                &request.name,
                request.description.as_deref().unwrap_or("Inline subagent"),
                system_prompt,
            );
            if !request.allowed_tools.is_empty() {
                def = def.with_allowed_tools(request.allowed_tools.clone());
            }
            if !request.disallowed_tools.is_empty() {
                def = def.with_disallowed_tools(request.disallowed_tools.clone());
            }
            if let Some(max_turns) = request.max_turns {
                def = def.with_max_turns(max_turns);
            }
            def
        } else if let Some(registered) = self.registry.get(&request.name) {
            // Use registered definition, with optional overrides
            let mut def = registered.clone();
            if let Some(max_turns) = request.max_turns {
                def.max_turns = max_turns;
            }
            if !request.disallowed_tools.is_empty() {
                def.disallowed_tools
                    .extend(request.disallowed_tools.iter().cloned());
            }
            def
        } else {
            return Ok(json!({
                "error": format!(
                    "No subagent '{}' registered and no system_prompt provided for inline definition. \
                    Available: {}",
                    request.name,
                    self.registry.list().iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            }));
        };

        // Apply isolation mode from the request.
        let def = def.with_isolation(isolation);

        let runner = SubAgentRunner::new(
            self.work_dir.clone(),
            self.source_root.clone(),
            self.llm_client.clone(),
        );
        let runner = if let Some(ref tx) = self.event_tx {
            runner.with_event_sink(tx.clone())
        } else {
            runner
        };

        if request.background || def.background {
            // Background execution — return immediately, deliver results later.
            // When background_tx is set the result is injected back into the
            // parent agent's conversation (like Claude Code).  The event channel
            // is always notified for CLI display.
            let agent_id = Uuid::new_v4();
            let handle = runner.run_background(def.clone(), request.prompt);

            let event_tx = self.event_tx.clone();
            let background_tx = self.background_tx.clone();
            let name = def.name.clone();
            tokio::spawn(async move {
                match handle.await {
                    Ok(Ok(result)) => {
                        // Deliver result back to parent agent's conversation
                        if let Some(bg_tx) = background_tx {
                            let _ = bg_tx.send(BackgroundResult {
                                name: result.name.clone(),
                                kind: BackgroundResultKind::SubAgent,
                                content: result.final_content.clone(),
                                tokens_used: result.total_tokens,
                            });
                        }
                        // Notify event listeners (CLI display)
                        if let Some(tx) = event_tx {
                            let _ = tx.send(AgentEvent::SubAgentCompleted {
                                agent_id: result.agent_id,
                                name: result.name,
                                tokens_used: result.total_tokens,
                                iterations: result.iterations,
                                tool_calls: result.tool_calls_count,
                                final_content: result.final_content,
                                worktree_path: result.worktree_path,
                                branch: result.worktree_branch,
                            });
                        }
                    }
                    Ok(Err(e)) => {
                        if let Some(tx) = event_tx {
                            let _ = tx.send(AgentEvent::SubAgentFailed {
                                agent_id,
                                name,
                                error: e.to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        if let Some(tx) = event_tx {
                            let _ = tx.send(AgentEvent::SubAgentFailed {
                                agent_id,
                                name,
                                error: format!("Task join error: {}", e),
                            });
                        }
                    }
                }
            });

            Ok(json!({
                "status": "background",
                "agent_id": agent_id.to_string(),
                "name": def.name,
                "message": "Subagent started in background. You will be notified when it completes — continue with other work."
            }))
        } else {
            // Foreground (blocking) execution
            match runner.run(&def, &request.prompt).await {
                Ok(result) => {
                    let mut resp = json!({
                        "status": "completed",
                        "name": result.name,
                        "agent_id": result.agent_id.to_string(),
                        "result": result.final_content,
                        "total_tokens": result.total_tokens,
                        "iterations": result.iterations,
                        "tool_calls": result.tool_calls_count,
                    });
                    if let Some(ref branch) = result.worktree_branch {
                        resp["worktree_branch"] = json!(branch);
                    }
                    if let Some(ref wt_path) = result.worktree_path {
                        resp["worktree_path"] = json!(wt_path);
                    }
                    Ok(resp)
                }
                Err(e) => Ok(json!({
                    "status": "failed",
                    "name": def.name,
                    "error": e.to_string(),
                })),
            }
        }
    }
}
