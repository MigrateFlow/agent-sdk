use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use sdk_agent::builder::{
    CommandToolPolicy, DefaultToolsetBuilder, SubAgentToolConfig, TeamToolConfig, ToolFilter,
};
use sdk_agent::subagent::SubAgentRegistry;
use sdk_core::background::BackgroundResult;
use sdk_core::error::AgentId;
use sdk_core::events::AgentEvent;
use sdk_core::memory::MemoryStore;
use sdk_core::registry::ToolRegistry;
use sdk_core::storage::AgentPaths;
use sdk_core::traits::llm_client::LlmClient;
use sdk_core::traits::tool::{Tool, ToolDefinition};

use crate::mode_tools::{
    AdvanceUltraPlanPhaseTool, EnterPlanModeTool, EnterUltraPlanTool, ExitPlanModeTool,
    ExitUltraPlanTool, ModeState,
};
use crate::session::CliTask;

pub struct UpdateTaskListTool {
    pub tasks: Arc<Mutex<Vec<CliTask>>>,
}

#[async_trait]
impl Tool for UpdateTaskListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "update_task_list".to_string(),
            description: "Update the visible task list for the current single-agent session. Use this for multi-step work to show the current tasks and their statuses. Status must be pending, in_progress, completed, or blocked.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "blocked"] }
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> sdk_core::error::SdkResult<serde_json::Value> {
        let items = arguments["items"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            return Ok(json!({ "error": "Missing or empty 'items' array" }));
        }

        let tasks = items
            .into_iter()
            .filter_map(|item| {
                let title = item["title"].as_str()?.trim();
                let status = item["status"].as_str()?.trim();
                if title.is_empty() || status.is_empty() {
                    return None;
                }
                Some(CliTask {
                    title: title.to_string(),
                    status: status.to_string(),
                })
            })
            .collect::<Vec<_>>();

        if tasks.is_empty() {
            return Ok(json!({ "error": "No valid task items provided" }));
        }

        let mut guard = self.tasks.lock().expect("task list mutex poisoned");
        *guard = tasks;

        Ok(json!({ "updated": true, "count": guard.len() }))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_tools(
    work_dir: &Path,
    _allow_all: bool,
    llm_client: Arc<dyn LlmClient>,
    llm_config: sdk_core::config::LlmConfig,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    subagent_registry: Arc<SubAgentRegistry>,
    background_tx: Option<tokio::sync::mpsc::UnboundedSender<BackgroundResult>>,
    tool_filter: Option<&[String]>,
    mcp_tools: &[Arc<dyn Tool>],
    paths: &AgentPaths,
    memory_store: Option<Arc<MemoryStore>>,
    cli_agent_id: AgentId,
    mode_state: Option<ModeState>,
) -> ToolRegistry {
    let filter = tool_filter
        .map(|names| ToolFilter::allow_only(names.iter().cloned()))
        .unwrap_or_default();
    let command_policy = CommandToolPolicy::Unrestricted;

    let mut builder = DefaultToolsetBuilder::with_filter(filter)
        .add_core_tools(
            work_dir.to_path_buf(),
            work_dir.to_path_buf(),
            command_policy,
        )
        .add_lsp_tools(paths.project_lsp_config_path(), work_dir.to_path_buf())
        .add_team_tool(TeamToolConfig {
            work_dir: work_dir.to_path_buf(),
            source_root: work_dir.to_path_buf(),
            llm_client: llm_client.clone(),
            llm_config: llm_config.clone(),
            event_tx: event_tx.clone(),
            background_tx: background_tx.clone(),
        })
        .add_subagent_tool(SubAgentToolConfig {
            work_dir: work_dir.to_path_buf(),
            source_root: work_dir.to_path_buf(),
            llm_client,
            event_tx,
            registry: subagent_registry,
            background_tx,
        });

    if let Some(store) = memory_store {
        builder = builder.add_memory_tools(store, cli_agent_id);
    }

    builder = builder.add_custom_tool(Arc::new(UpdateTaskListTool { tasks }));

    // Mode tools: let the agent enter/exit plan mode and ultraplan programmatically
    if let Some(ms) = mode_state {
        builder = builder
            .add_custom_tool(Arc::new(EnterPlanModeTool { state: ms.clone() }))
            .add_custom_tool(Arc::new(ExitPlanModeTool { state: ms.clone() }))
            .add_custom_tool(Arc::new(EnterUltraPlanTool { state: ms.clone() }))
            .add_custom_tool(Arc::new(AdvanceUltraPlanPhaseTool { state: ms.clone() }))
            .add_custom_tool(Arc::new(ExitUltraPlanTool { state: ms }));
    }

    for tool in mcp_tools {
        builder = builder.add_custom_tool(tool.clone());
    }

    builder.build()
}
