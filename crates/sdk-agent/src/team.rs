use std::sync::Arc;

use tracing::info;
use uuid::Uuid;

use sdk_core::config::{AgentConfig, LlmConfig};
use sdk_core::error::SdkResult;
use sdk_core::registry::ToolRegistry;
use sdk_core::traits::llm_client::LlmClient;
use sdk_core::traits::prompt_builder::{DefaultPromptBuilder, PromptBuilder};
use crate::builder::{AgentToolConfig, CommandToolPolicy, DefaultToolsetBuilder};
use crate::subagent::SubAgentRegistry;

use crate::agent_loop::{AgentLoop, AgentLoopResult};
use sdk_core::events::AgentEvent;
use sdk_core::hooks::HookRegistry;

/// High-level entry point for the agent SDK.
///
/// `AgentTeam` is now a thin wrapper that creates an `AgentLoop` with the
/// unified `agent` tool registered. For simple tasks, `run_single()` creates
/// an AgentLoop without the agent tool.
pub struct AgentTeam {
    llm_config: LlmConfig,
    agent_config: AgentConfig,
    llm_client: Option<Arc<dyn LlmClient>>,
    prompt_builder: Arc<dyn PromptBuilder>,
    hooks: HookRegistry,
    source_root: std::path::PathBuf,
    work_dir: std::path::PathBuf,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    subagent_registry: Arc<SubAgentRegistry>,
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
            subagent_registry: Arc::new(SubAgentRegistry::new()),
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
    pub fn event_channel(mut self, tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Provide a pre-created LLM client (skips creating one from config).
    pub fn llm_client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.llm_client = Some(client);
        self
    }

    /// Add a hook for quality gates.
    pub fn add_hook(mut self, hook: impl sdk_core::hooks::Hook + 'static) -> Self {
        self.hooks.add(hook);
        self
    }

    /// Set a custom subagent registry (presets).
    pub fn subagent_registry(mut self, registry: Arc<SubAgentRegistry>) -> Self {
        self.subagent_registry = registry;
        self
    }

    /// Run the agent with the unified `agent` tool registered.
    /// The agent can spawn subagents as needed via the tool.
    pub async fn run(mut self, goal: &str) -> SdkResult<AgentLoopResult> {
        let client = self.resolve_client()?;

        let tools = DefaultToolsetBuilder::new()
            .add_core_tools(
                self.source_root.clone(),
                self.work_dir.clone(),
                CommandToolPolicy::Unrestricted,
            )
            .add_agent_tool(AgentToolConfig {
                work_dir: self.work_dir.clone(),
                source_root: self.source_root.clone(),
                llm_client: client.clone(),
                event_tx: self.event_tx.clone(),
                registry: self.subagent_registry.clone(),
                background_tx: None,
            })
            .build();

        info!("Running agent with agent tool");
        self.run_loop(client, tools, goal).await
    }

    /// Run as a single agent (no agent tool). For simple, focused tasks.
    pub async fn run_single(mut self, user_message: &str) -> SdkResult<AgentLoopResult> {
        let client = self.resolve_client()?;

        let tools = DefaultToolsetBuilder::new()
            .add_core_tools(
                self.source_root.clone(),
                self.work_dir.clone(),
                CommandToolPolicy::Unrestricted,
            )
            .build();

        info!("Running as single agent");
        self.run_loop(client, tools, user_message).await
    }

    fn resolve_client(&mut self) -> SdkResult<Arc<dyn LlmClient>> {
        match self.llm_client.take() {
            Some(c) => Ok(c),
            None => sdk_llm::create_client(&self.llm_config),
        }
    }

    async fn run_loop(
        &self,
        client: Arc<dyn LlmClient>,
        tools: ToolRegistry,
        user_message: &str,
    ) -> SdkResult<AgentLoopResult> {
        let system = sdk_core::prompts::single_agent_system_prompt(
            &self.source_root,
            &self.work_dir,
        );

        let mut agent = AgentLoop::new(
            Uuid::new_v4(),
            client,
            tools,
            system,
            self.agent_config.max_loop_iterations,
        )
        .with_compaction_config(self.agent_config.compaction.clone());

        if let Some(ref tx) = self.event_tx {
            agent.set_event_sink(tx.clone());
        }

        agent.run(user_message.to_string()).await
    }
}
