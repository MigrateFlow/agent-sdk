use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use sdk_core::background::{BackgroundResult, BackgroundResultKind};
use sdk_core::events::AgentEvent;
use sdk_core::hooks::HookRegistry;
use sdk_core::memory::MemoryStore;
use crate::team_lead::{TeamLead, TeammateSpec};
use sdk_core::config::AgentConfig;
use sdk_core::error::{SdkError, SdkResult};
use sdk_task::mailbox::broker::MessageBroker;
use sdk_core::storage::AgentPaths;
use sdk_task::task::store::TaskStore;
use sdk_core::traits::llm_client::LlmClient;
use sdk_core::traits::prompt_builder::DefaultPromptBuilder;
use sdk_core::traits::tool::{Tool, ToolDefinition};
use sdk_core::types::task::Task;

/// Tool that lets the agent spawn an agent team at runtime.
/// The LLM decides when a task is complex enough to warrant a team.
pub struct SpawnAgentTeamTool {
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
    pub llm_client: Arc<dyn LlmClient>,
    pub llm_config: sdk_core::config::LlmConfig,
    pub event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    /// When set, background team results are sent back through this channel
    /// so the parent agent loop can inject them into its conversation.
    pub background_tx: Option<tokio::sync::mpsc::UnboundedSender<BackgroundResult>>,
}

#[derive(Debug, Deserialize)]
struct TeamRequest {
    teammates: Vec<TeammateRequest>,
    tasks: Vec<TaskRequest>,
    /// When true (default), the SDK automatically assigns tasks to teammates
    /// based on name/role keyword matching.  Set to false to let teammates
    /// claim tasks on a first-come basis.
    #[serde(default = "default_true")]
    auto_assign: bool,
    /// Run the team in the background (concurrent). Default: false (blocking).
    /// When true, returns immediately and delivers results via the background
    /// channel when all tasks complete.
    #[serde(default)]
    background: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct TeammateRequest {
    name: String,
    role: String,
    #[serde(default)]
    require_plan_approval: bool,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskRequest {
    title: String,
    description: String,
    target_file: String,
    #[serde(default)]
    depends_on: Vec<usize>,
    #[serde(default)]
    priority: u32,
}

#[async_trait]
impl Tool for SpawnAgentTeamTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spawn_agent_team".to_string(),
            description: "Spawn a team of parallel agents to work on complex tasks. \
                Use this when the work can be split into independent pieces that benefit \
                from parallel execution. Each teammate works independently with its own context. \
                Do NOT use this for simple, sequential tasks — handle those yourself."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "teammates": {
                        "type": "array",
                        "description": "The teammates to spawn",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "description": "Short name for the teammate (e.g. 'backend-dev', 'reviewer')" },
                                "role": { "type": "string", "description": "Description of what this teammate should do" },
                                "require_plan_approval": { "type": "boolean", "description": "If true, teammate must submit a plan before implementing" },
                                "isolation": { "type": "string", "enum": ["none", "worktree"], "description": "Isolation mode: 'worktree' gives the teammate its own git worktree to prevent merge conflicts" },
                                "model": { "type": "string", "description": "Optional LLM model override for this teammate (e.g. 'claude-sonnet-4-5-20250514'). Defaults to the parent's model." }
                            },
                            "required": ["name", "role"]
                        }
                    },
                    "auto_assign": {
                        "type": "boolean",
                        "description": "Auto-assign tasks to teammates by keyword matching (default: true). Set false to let teammates claim freely."
                    },
                    "background": {
                        "type": "boolean",
                        "description": "If true, run the team in background (concurrent). Returns immediately; you will be notified when all tasks complete. Default: false."
                    },
                    "tasks": {
                        "type": "array",
                        "description": "Tasks for the team to work on",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "Short task title" },
                                "description": { "type": "string", "description": "Detailed instructions for the agent" },
                                "target_file": { "type": "string", "description": "Output file path" },
                                "depends_on": { "type": "array", "items": { "type": "integer" }, "description": "Indices (0-based) of tasks this depends on" },
                                "priority": { "type": "integer", "description": "Priority (lower = higher priority)" }
                            },
                            "required": ["title", "description", "target_file"]
                        }
                    }
                },
                "required": ["teammates", "tasks"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let request: TeamRequest =
            serde_json::from_value(arguments).map_err(|e| SdkError::ToolExecution {
                tool_name: "spawn_agent_team".to_string(),
                message: format!("Invalid arguments: {}", e),
            })?;

        if request.teammates.is_empty() {
            return Ok(json!({ "error": "Must specify at least one teammate" }));
        }
        if request.tasks.is_empty() {
            return Ok(json!({ "error": "Must specify at least one task" }));
        }
        if let Some(dupe) = duplicate_target_file(&request.tasks) {
            return Ok(json!({
                "error": format!(
                    "Conflicting task ownership: multiple tasks target '{}'. Split work so each teammate owns different files.",
                    dupe
                )
            }));
        }

        let paths = AgentPaths::for_work_dir(&self.work_dir)?;
        let team_name = paths.new_team_name();
        let team_config_path = paths.team_config_path(&team_name);
        tokio::fs::create_dir_all(paths.team_dir(&team_name))
            .await
            .map_err(SdkError::Io)?;

        let task_store = Arc::new(TaskStore::new(paths.team_tasks_dir(&team_name)));
        task_store.init()?;

        let broker = Arc::new(MessageBroker::new(paths.team_mailbox_dir(&team_name))?);
        let memory = Arc::new(MemoryStore::new(paths.team_memory_dir(&team_name))?);

        // Create tasks, resolving dependency indices to TaskIds
        let mut created_tasks: Vec<Task> = Vec::new();
        for (i, tr) in request.tasks.iter().enumerate() {
            let mut deps: Vec<_> = tr
                .depends_on
                .iter()
                .filter_map(|&idx| created_tasks.get(idx).map(|t| t.id))
                .collect();
            if deps.is_empty() && looks_like_integration_task(tr) {
                deps = created_tasks.iter().map(|t| t.id).collect();
            }

            let assigned_teammate = if request.auto_assign {
                choose_assignee(tr, &request.teammates, i)
            } else {
                None
            };

            let mut task = Task::new(&tr.title, &tr.title, &tr.description, &tr.target_file)
                .with_priority(tr.priority.max(i as u32))
                .with_dependencies(deps);
            if let Some(name) = assigned_teammate {
                task = task.with_context(json!({ "assigned_teammate": name }));
            }

            task_store.create_task(&task)?;
            created_tasks.push(task);
        }

        // Create teammate specs
        let teammate_specs: Vec<TeammateSpec> = request
            .teammates
            .iter()
            .map(|t| {
                let isolation = match t.isolation.as_deref() {
                    Some("worktree") => Some(crate::worktree::IsolationMode::Worktree),
                    _ => None,
                };
                TeammateSpec {
                    name: t.name.clone(),
                    prompt: t.role.clone(),
                    require_plan_approval: t.require_plan_approval,
                    isolation,
                    model: t.model.clone(),
                }
            })
            .collect();

        let teammate_names: Vec<_> = teammate_specs.iter().map(|t| t.name.clone()).collect();
        let task_titles: Vec<_> = created_tasks.iter().map(|t| t.title.clone()).collect();
        let task_assignments: Vec<_> = created_tasks
            .iter()
            .map(|t| {
                json!({
                    "title": t.title,
                    "target_file": t.target_file,
                    "assigned_teammate": t.context.get("assigned_teammate").and_then(|v| v.as_str()),
                    "depends_on": t.dependencies,
                })
            })
            .collect();
        let task_count = created_tasks.len();
        let teammate_count = teammate_specs.len();

        // Run the team lead
        let lead = TeamLead {
            id: Uuid::new_v4(),
            team_name: team_name.clone(),
            team_config_path,
            task_store,
            broker,
            llm_client: self.llm_client.clone(),
            llm_config: self.llm_config.clone(),
            prompt_builder: Arc::new(DefaultPromptBuilder),
            config: AgentConfig {
                max_parallel_agents: teammate_count,
                max_loop_iterations: 30,
                max_task_retries: 2,
                ..Default::default()
            },
            source_root: self.source_root.clone(),
            work_dir: self.work_dir.clone(),
            memory_store: memory,
            event_tx: self.event_tx.clone(),
            hooks: Arc::new(HookRegistry::new()),
            teammate_specs,
            team_goal: String::new(),
        };

        if request.background {
            // Background execution — return immediately, deliver results later.
            let background_tx = self.background_tx.clone();
            let team_name_bg = team_name.clone();
            let teammate_names_bg = teammate_names.clone();
            let task_titles_bg = task_titles.clone();

            tokio::spawn(async move {
                match lead.run().await {
                    Ok(summary) => {
                        let content = format!(
                            "Team '{}' completed: {}/{} tasks succeeded, {} failed. {} tokens used.\nTeammates: {}\nTasks: {}",
                            team_name_bg,
                            summary.tasks_completed,
                            summary.total_tasks,
                            summary.tasks_failed,
                            summary.total_tokens_used,
                            teammate_names_bg.join(", "),
                            task_titles_bg.join(", "),
                        );
                        if let Some(bg_tx) = background_tx {
                            let _ = bg_tx.send(BackgroundResult {
                                name: team_name_bg,
                                kind: BackgroundResultKind::AgentTeam,
                                content,
                                tokens_used: summary.total_tokens_used,
                            });
                        }
                    }
                    Err(e) => {
                        if let Some(bg_tx) = background_tx {
                            let _ = bg_tx.send(BackgroundResult {
                                name: team_name_bg,
                                kind: BackgroundResultKind::AgentTeam,
                                content: format!("Team failed: {}", e),
                                tokens_used: 0,
                            });
                        }
                    }
                }
            });

            Ok(json!({
                "status": "background",
                "team_name": team_name,
                "teammates": teammate_names,
                "tasks": task_titles,
                "task_assignments": task_assignments,
                "total_tasks": task_count,
                "message": "Agent team started in background. You will be notified when all tasks complete — continue with other work."
            }))
        } else {
            // Foreground (blocking) execution
            match lead.run().await {
                Ok(summary) => Ok(json!({
                    "status": "completed",
                    "team_name": team_name,
                    "teammates": teammate_names,
                    "tasks": task_titles,
                    "task_assignments": task_assignments,
                    "total_tasks": summary.total_tasks,
                    "tasks_completed": summary.tasks_completed,
                    "tasks_failed": summary.tasks_failed,
                    "agents_spawned": summary.agents_spawned,
                    "total_tokens_used": summary.total_tokens_used
                })),
                Err(e) => Ok(json!({
                    "status": "failed",
                    "error": e.to_string(),
                    "team_name": team_name,
                    "teammates": teammate_names,
                    "tasks_created": task_count
                })),
            }
        }
    }
}

fn choose_assignee(
    task: &TaskRequest,
    teammates: &[TeammateRequest],
    task_index: usize,
) -> Option<String> {
    if teammates.is_empty() {
        return None;
    }

    let task_text = format!(
        "{} {} {}",
        task.title.to_lowercase(),
        task.description.to_lowercase(),
        task.target_file.to_lowercase()
    );

    let mut best_score = 0usize;
    let mut best_name: Option<String> = None;

    for teammate in teammates {
        let mut score = 0usize;
        for token in teammate
            .name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .chain(teammate.role.split(|c: char| !c.is_ascii_alphanumeric()))
        {
            let token = token.to_lowercase();
            if token.len() < 3 {
                continue;
            }
            if task_text.contains(&token) {
                score += 1;
            }
        }

        if score > best_score {
            best_score = score;
            best_name = Some(teammate.name.clone());
        }
    }

    if best_name.is_some() {
        best_name
    } else {
        // Fallback to deterministic round-robin to keep teammates utilized.
        Some(teammates[task_index % teammates.len()].name.clone())
    }
}

fn looks_like_integration_task(task: &TaskRequest) -> bool {
    let file = task.target_file.to_lowercase();
    let title = task.title.to_lowercase();
    let desc = task.description.to_lowercase();

    file.ends_with("main.rs")
        || title.contains("main")
        || title.contains("entrypoint")
        || desc.contains("wire")
        || desc.contains("integrate")
}

fn duplicate_target_file(tasks: &[TaskRequest]) -> Option<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    for task in tasks {
        let key = task.target_file.trim().to_lowercase();
        if !seen.insert(key.clone()) {
            return Some(task.target_file.clone());
        }
    }
    None
}
