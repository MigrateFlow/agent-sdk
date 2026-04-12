//! Subagent — a lightweight, isolated agent that runs a focused task and returns
//! results to the caller.
//!
//! Unlike agent teams where teammates communicate with each other via mailboxes,
//! subagents only report results back to the parent agent. They run in their own
//! context window with a custom system prompt and optional tool restrictions.
//!
//! Subagents **cannot** spawn other subagents (no nesting).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;
use uuid::Uuid;

use sdk_core::error::{AgentId, SdkResult};
use crate::builder::{CommandToolPolicy, DefaultToolsetBuilder, ToolFilter};
use sdk_core::registry::ToolRegistry;
use sdk_core::traits::llm_client::LlmClient;

use crate::agent_loop::{AgentLoop, AgentLoopResult};
use sdk_core::events::AgentEvent;

/// Definition of a subagent — its identity, capabilities, and constraints.
///
/// Analogous to a markdown frontmatter file in Claude Code's subagent system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentDef {
    /// Unique identifier (lowercase, hyphens). E.g. "code-reviewer".
    pub name: String,
    /// When the parent agent should delegate to this subagent.
    pub description: String,
    /// The system prompt (markdown body). Replaces the default system prompt entirely.
    pub prompt: String,
    /// Allowed tool names. If empty, inherits all default tools.
    /// Use this as an allowlist: only these tools will be available.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tool names to explicitly deny. Applied after `allowed_tools`.
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    /// Model override. If `None`, inherits from the parent.
    #[serde(default)]
    pub model: Option<String>,
    /// Maximum agentic turns before the subagent stops.
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// Maximum context window tokens.
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: usize,
    /// Whether to always run this subagent in the background.
    #[serde(default)]
    pub background: bool,
}

fn default_max_turns() -> usize {
    30
}

fn default_max_context_tokens() -> usize {
    200_000
}

impl SubAgentDef {
    /// Create a new subagent definition with required fields.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            prompt: prompt.into(),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            model: None,
            max_turns: default_max_turns(),
            max_context_tokens: default_max_context_tokens(),
            background: false,
        }
    }

    /// Restrict the subagent to only these tools.
    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Deny specific tools (removed from inherited or allowed set).
    pub fn with_disallowed_tools(mut self, tools: Vec<String>) -> Self {
        self.disallowed_tools = tools;
        self
    }

    /// Override the model for this subagent.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set maximum agentic turns.
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// Set maximum context window tokens.
    pub fn with_max_context_tokens(mut self, tokens: usize) -> Self {
        self.max_context_tokens = tokens;
        self
    }

    /// Set whether to always run in background.
    pub fn with_background(mut self, background: bool) -> Self {
        self.background = background;
        self
    }
}

/// Result returned when a subagent completes.
#[derive(Debug, Clone, Serialize)]
pub struct SubAgentResult {
    pub agent_id: AgentId,
    pub name: String,
    pub final_content: String,
    pub total_tokens: u64,
    pub iterations: usize,
    pub tool_calls_count: usize,
}

impl From<(AgentId, &str, AgentLoopResult)> for SubAgentResult {
    fn from((agent_id, name, result): (AgentId, &str, AgentLoopResult)) -> Self {
        Self {
            agent_id,
            name: name.to_string(),
            final_content: result.final_content,
            total_tokens: result.total_tokens,
            iterations: result.iterations,
            tool_calls_count: result.tool_calls_count,
        }
    }
}

/// Runs a subagent to completion.
///
/// The subagent gets its own `AgentLoop` with its own context window. It runs
/// the given task prompt and returns the result. Subagents cannot spawn other
/// subagents — the `spawn_subagent` tool is intentionally excluded.
pub struct SubAgentRunner {
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
    pub llm_client: Arc<dyn LlmClient>,
    pub event_tx: Option<UnboundedSender<AgentEvent>>,
    /// Optional override LLM client (for model override).
    pub override_llm_client: Option<Arc<dyn LlmClient>>,
}

impl SubAgentRunner {
    pub fn new(
        work_dir: PathBuf,
        source_root: PathBuf,
        llm_client: Arc<dyn LlmClient>,
    ) -> Self {
        Self {
            work_dir,
            source_root,
            llm_client,
            event_tx: None,
            override_llm_client: None,
        }
    }

    pub fn with_event_sink(mut self, tx: UnboundedSender<AgentEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn with_override_llm_client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.override_llm_client = Some(client);
        self
    }

    /// Run a subagent with the given definition and task prompt.
    ///
    /// Returns the subagent's result including final content and token usage.
    pub async fn run(
        &self,
        def: &SubAgentDef,
        task_prompt: &str,
    ) -> SdkResult<SubAgentResult> {
        let agent_id = Uuid::new_v4();
        let client = self
            .override_llm_client
            .clone()
            .unwrap_or_else(|| self.llm_client.clone());

        info!(
            agent_id = %agent_id,
            subagent = %def.name,
            "Spawning subagent"
        );

        self.emit(AgentEvent::SubAgentSpawned {
            agent_id,
            name: def.name.clone(),
            description: def.description.clone(),
        });

        // Build tool registry with restrictions
        let tools = self.build_tools(def);

        // Build system prompt
        let system_prompt = sdk_core::prompts::subagent_system_prompt(
            &def.prompt,
            &self.source_root,
            &self.work_dir,
        );

        let mut agent_loop = AgentLoop::new(
            agent_id,
            client,
            tools,
            system_prompt,
            def.max_turns,
        )
        .with_max_context_tokens(def.max_context_tokens)
        .with_agent_name(&def.name);

        if let Some(ref tx) = self.event_tx {
            agent_loop.set_event_sink(tx.clone());
        }

        match agent_loop.run(task_prompt.to_string()).await {
            Ok(loop_result) => {
                let result = SubAgentResult::from((agent_id, def.name.as_str(), loop_result));

                self.emit(AgentEvent::SubAgentCompleted {
                    agent_id,
                    name: def.name.clone(),
                    tokens_used: result.total_tokens,
                    iterations: result.iterations,
                    tool_calls: result.tool_calls_count,
                    final_content: result.final_content.clone(),
                });

                Ok(result)
            }
            Err(e) => {
                self.emit(AgentEvent::SubAgentFailed {
                    agent_id,
                    name: def.name.clone(),
                    error: e.to_string(),
                });
                Err(e)
            }
        }
    }

    /// Run a subagent in the background, returning a handle to await the result.
    pub fn run_background(
        &self,
        def: SubAgentDef,
        task_prompt: String,
    ) -> tokio::task::JoinHandle<SdkResult<SubAgentResult>> {
        let runner = SubAgentRunner {
            work_dir: self.work_dir.clone(),
            source_root: self.source_root.clone(),
            llm_client: self.llm_client.clone(),
            event_tx: self.event_tx.clone(),
            override_llm_client: self.override_llm_client.clone(),
        };

        tokio::spawn(async move { runner.run(&def, &task_prompt).await })
    }

    /// Build the tool registry for a subagent, respecting allowed/disallowed lists.
    fn build_tools(&self, def: &SubAgentDef) -> ToolRegistry {
        let filter = if def.allowed_tools.is_empty() {
            ToolFilter::default()
        } else {
            ToolFilter::allow_only(def.allowed_tools.clone())
        }
        .deny(def.disallowed_tools.clone());

        DefaultToolsetBuilder::with_filter(filter)
            .add_core_tools(
                self.source_root.clone(),
                self.work_dir.clone(),
                CommandToolPolicy::Unrestricted,
            )
            .build()
    }

    fn emit(&self, event: AgentEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}

/// A registry of subagent definitions available for the agent to invoke.
#[derive(Debug, Clone, Default)]
pub struct SubAgentRegistry {
    defs: Vec<SubAgentDef>,
}

impl SubAgentRegistry {
    pub fn new() -> Self {
        Self { defs: Vec::new() }
    }

    /// Register a subagent definition.
    pub fn register(&mut self, def: SubAgentDef) {
        // Replace existing definition with same name
        self.defs.retain(|d| d.name != def.name);
        self.defs.push(def);
    }

    /// Get a subagent definition by name.
    pub fn get(&self, name: &str) -> Option<&SubAgentDef> {
        self.defs.iter().find(|d| d.name == name)
    }

    /// List all registered subagent definitions.
    pub fn list(&self) -> &[SubAgentDef] {
        &self.defs
    }

    /// Check if any subagents are registered.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Built-in subagent definitions that mirror Claude Code's defaults.
pub fn builtin_subagents() -> Vec<SubAgentDef> {
    vec![
        SubAgentDef {
            name: "explore".to_string(),
            description: "Fast, read-only agent for searching and analyzing codebases. \
                Use when you need to quickly find files, search code, or understand \
                the codebase without making changes. Keeps exploration out of your \
                main context."
                .to_string(),
            prompt: "You are a codebase exploration specialist. Your job is to search, \
                read, and analyze code efficiently. Report your findings concisely.\n\n\
                You have read-only access. Do NOT attempt to modify any files."
                .to_string(),
            allowed_tools: vec![
                "read_file".to_string(),
                "list_directory".to_string(),
                "search_files".to_string(),
                "run_command".to_string(),
            ],
            disallowed_tools: vec![
                "write_file".to_string(),
            ],
            model: None,
            max_turns: 20,
            max_context_tokens: 200_000,
            background: false,
        },
        SubAgentDef {
            name: "plan".to_string(),
            description: "Research agent for gathering context before presenting a plan. \
                Use when you need to understand the codebase to plan an implementation \
                strategy."
                .to_string(),
            prompt: "You are a software architect. Analyze the codebase and produce a \
                detailed implementation plan. Include:\n\
                1. What files need to be read/created/modified\n\
                2. The approach and key decisions\n\
                3. Potential risks or edge cases\n\
                4. Verification steps\n\n\
                You have read-only access. Do NOT attempt to modify any files."
                .to_string(),
            allowed_tools: vec![
                "read_file".to_string(),
                "list_directory".to_string(),
                "search_files".to_string(),
                "run_command".to_string(),
            ],
            disallowed_tools: vec![
                "write_file".to_string(),
            ],
            model: None,
            max_turns: 25,
            max_context_tokens: 200_000,
            background: false,
        },
        SubAgentDef {
            name: "general-purpose".to_string(),
            description: "Capable agent for complex, multi-step tasks requiring both \
                exploration and action. Use for research, multi-step operations, or \
                code modifications that benefit from isolated context."
                .to_string(),
            prompt: "You are an expert coding assistant handling a delegated task. \
                Work independently and return a clear, concise result summary. \
                Read files before modifying them. Verify your work."
                .to_string(),
            allowed_tools: Vec::new(), // all tools
            disallowed_tools: Vec::new(),
            model: None,
            max_turns: 30,
            max_context_tokens: 200_000,
            background: false,
        },
    ]
}
