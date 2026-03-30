use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::info;
use uuid::Uuid;

use crate::config::{AgentConfig, LlmConfig};
use crate::error::SdkResult;
use crate::traits::llm_client::LlmClient;
use crate::traits::prompt_builder::{DefaultPromptBuilder, PromptBuilder};
use crate::mailbox::broker::MessageBroker;
use crate::storage::AgentPaths;
use crate::task::store::TaskStore;
use crate::types::task::Task;

use super::agent_loop::{AgentLoop, AgentLoopResult};
use super::events::AgentEvent;
use super::hooks::HookRegistry;
use super::memory::MemoryStore;
use super::team_lead::{ExecutionSummary, TeamLead, TeammateSpec};

/// Result of an `AgentTeam::run()` call.
#[derive(Debug)]
pub enum TeamResult {
    /// Task was handled by a single agent (no team needed).
    Single(AgentLoopResult),
    /// Task was handled by a team of agents.
    Team(ExecutionSummary),
}

impl TeamResult {
    pub fn total_tokens(&self) -> u64 {
        match self {
            Self::Single(r) => r.total_tokens,
            Self::Team(s) => s.total_tokens_used,
        }
    }
}

/// High-level entry point for the agent SDK.
///
/// `AgentTeam` coordinates multiple agent instances working together.
/// One session acts as the team lead, coordinating work, assigning tasks,
/// and synthesizing results. Teammates work independently, each in its own
/// context window, and can communicate directly with each other.
///
/// There is no separate planning step — the lead IS the intelligence that
/// decides how to organize work, just like Claude Code's agent teams.
///
/// # Usage patterns
///
/// **You describe the team** — add teammates with roles, add tasks, and run:
/// ```rust,no_run
/// # use agent_sdk::agent::team::AgentTeam;
/// # use agent_sdk::config::{LlmConfig, AgentConfig};
/// # async fn ex() -> anyhow::Result<()> {
/// let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
///     .add_teammate("security", "Review for security vulnerabilities")
///     .add_teammate("performance", "Review for performance issues")
///     .run("Review the auth module")
///     .await?;
/// # Ok(()) }
/// ```
///
/// **Single agent** — for simple tasks, skip the team entirely:
/// ```rust,no_run
/// # use agent_sdk::agent::team::AgentTeam;
/// # use agent_sdk::config::{LlmConfig, AgentConfig};
/// # async fn ex() -> anyhow::Result<()> {
/// let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
///     .run_single("Explain this codebase")
///     .await?;
/// # Ok(()) }
/// ```
pub struct AgentTeam {
    llm_config: LlmConfig,
    agent_config: AgentConfig,
    llm_client: Option<Arc<dyn LlmClient>>,
    prompt_builder: Arc<dyn PromptBuilder>,
    hooks: HookRegistry,
    source_root: std::path::PathBuf,
    work_dir: std::path::PathBuf,
    event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    /// Explicit teammates to spawn.
    teammate_specs: Vec<TeammateSpec>,
    /// Pre-created tasks for the team.
    tasks: Vec<Task>,
}

impl AgentTeam {
    /// Create a new AgentTeam with the given LLM and agent configuration.
    pub fn new(llm_config: LlmConfig, agent_config: AgentConfig) -> Self {
        Self {
            llm_config,
            agent_config,
            llm_client: None,
            prompt_builder: Arc::new(DefaultPromptBuilder),
            hooks: HookRegistry::new(),
            source_root: std::path::PathBuf::from("."),
            work_dir: std::path::PathBuf::from("./output"),
            event_tx: None,
            teammate_specs: Vec::new(),
            tasks: Vec::new(),
        }
    }

    /// Set the source root directory (read-only source code).
    pub fn source_root(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.source_root = path.into();
        self
    }

    /// Set the working/output directory.
    pub fn work_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.work_dir = path.into();
        self
    }

    /// Set a custom prompt builder.
    pub fn prompt_builder(mut self, builder: Arc<dyn PromptBuilder>) -> Self {
        self.prompt_builder = builder;
        self
    }

    /// Set an event channel for monitoring agent activity.
    pub fn event_channel(mut self, tx: mpsc::UnboundedSender<AgentEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Provide a pre-created LLM client (skips creating one from config).
    pub fn llm_client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.llm_client = Some(client);
        self
    }

    /// Add a hook for quality gates (TeammateIdle, TaskCreated, TaskCompleted).
    pub fn add_hook(mut self, hook: impl super::hooks::Hook + 'static) -> Self {
        self.hooks.add(hook);
        self
    }

    /// Add a named teammate with a specific role.
    ///
    /// ```rust,no_run
    /// # use agent_sdk::agent::team::AgentTeam;
    /// # use agent_sdk::config::{LlmConfig, AgentConfig};
    /// AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    ///     .add_teammate("security-reviewer", "Review for security vulnerabilities")
    ///     .add_teammate("perf-reviewer", "Review for performance issues");
    /// ```
    pub fn add_teammate(
        mut self,
        name: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        self.teammate_specs.push(TeammateSpec {
            name: name.into(),
            prompt: prompt.into(),
            require_plan_approval: false,
        });
        self
    }

    /// Add a teammate that must get plan approval from the lead before implementing.
    /// The teammate generates a plan, sends it to the lead for review, and only
    /// proceeds after the lead approves.
    pub fn add_teammate_with_plan_approval(
        mut self,
        name: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        self.teammate_specs.push(TeammateSpec {
            name: name.into(),
            prompt: prompt.into(),
            require_plan_approval: true,
        });
        self
    }

    /// Add a task for the team to work on.
    ///
    /// ```rust,no_run
    /// # use agent_sdk::agent::team::AgentTeam;
    /// # use agent_sdk::config::{LlmConfig, AgentConfig};
    /// # use agent_sdk::types::task::Task;
    /// let task1 = Task::new("gen", "Create config", "...", "config.rs");
    /// let task2 = Task::new("gen", "Create server", "...", "server.rs")
    ///     .with_dependencies(vec![task1.id]);
    ///
    /// AgentTeam::new(LlmConfig::default(), AgentConfig::default())
    ///     .add_task(task1)
    ///     .add_task(task2);
    /// ```
    pub fn add_task(mut self, task: Task) -> Self {
        self.tasks.push(task);
        self
    }

    /// Run the team. The lead spawns teammates, they claim tasks from the
    /// shared task list, and work until all tasks are done.
    pub async fn run(mut self, _goal: &str) -> SdkResult<TeamResult> {
        let client = match self.llm_client.take() {
            Some(c) => c,
            None => crate::llm::create_client(&self.llm_config)?,
        };
        let paths = AgentPaths::for_work_dir(&self.work_dir)?;
        let team_name = paths.new_team_name();
        let team_config_path = paths.team_config_path(&team_name);

        let hooks = Arc::new(std::mem::take(&mut self.hooks));
        let task_store = Arc::new(TaskStore::new(paths.team_tasks_dir(&team_name)));
        task_store.init()?;

        // Add tasks to the store
        for task in &self.tasks {
            let hook_result = hooks.evaluate(
                &super::hooks::HookEvent::TaskCreated { task: task.clone() },
            );
            if let super::hooks::HookResult::Reject { feedback } = hook_result {
                self.emit_event(AgentEvent::HookRejected {
                    event_name: "TaskCreated".to_string(),
                    feedback,
                });
                continue;
            }
            task_store.create_task(task)?;
        }

        std::fs::create_dir_all(paths.team_dir(&team_name)).map_err(crate::error::SdkError::Io)?;
        let broker = Arc::new(MessageBroker::new(paths.team_mailbox_dir(&team_name))?);
        let memory = Arc::new(MemoryStore::new(paths.team_memory_dir(&team_name))?);

        let lead = TeamLead {
            id: Uuid::new_v4(),
            team_name,
            team_config_path,
            task_store,
            broker,
            llm_client: client,
            prompt_builder: self.prompt_builder.clone(),
            config: self.agent_config.clone(),
            source_root: self.source_root.clone(),
            work_dir: self.work_dir.clone(),
            memory_store: memory,
            event_tx: self.event_tx.clone(),
            hooks,
            teammate_specs: self.teammate_specs.clone(),
        };

        self.emit_event(AgentEvent::TeamSpawned {
            teammate_count: self.teammate_specs.len().max(self.agent_config.max_parallel_agents),
        });

        lead.run().await.map(TeamResult::Team)
    }

    /// Run as a single agent (no team). For simple, focused tasks.
    pub async fn run_single(mut self, user_message: &str) -> SdkResult<AgentLoopResult> {
        let client = match self.llm_client.take() {
            Some(c) => c,
            None => crate::llm::create_client(&self.llm_config)?,
        };

        use crate::tools::command_tools::RunCommandTool;
        use crate::tools::fs_tools::{ListDirectoryTool, ReadFileTool, WriteFileTool};
        use crate::tools::registry::ToolRegistry;
        use crate::tools::search_tools::SearchFilesTool;
        use crate::tools::web_tools::WebSearchTool;

        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(ReadFileTool {
            source_root: self.source_root.clone(),
            work_dir: self.work_dir.clone(),
        }));
        tools.register(Arc::new(WriteFileTool {
            work_dir: self.work_dir.clone(),
        }));
        tools.register(Arc::new(ListDirectoryTool {
            source_root: self.source_root.clone(),
            work_dir: self.work_dir.clone(),
        }));
        tools.register(Arc::new(SearchFilesTool {
            source_root: self.source_root.clone(),
        }));
        tools.register(Arc::new(WebSearchTool));
        tools.register(Arc::new(RunCommandTool::with_defaults(
            self.work_dir.clone(),
        )));

        let system = crate::prompts::single_agent_system_prompt(
            &self.source_root,
            &self.work_dir,
        );

        let mut agent = AgentLoop::new(
            Uuid::new_v4(),
            client,
            tools,
            system,
            self.agent_config.max_loop_iterations,
        );

        if let Some(ref tx) = self.event_tx {
            agent.set_event_sink(tx.clone());
        }

        info!("Running as single agent");
        agent.run(user_message.to_string()).await
    }

    fn emit_event(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}
