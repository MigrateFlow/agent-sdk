//! Subagent — a lightweight, isolated agent that runs a focused task and returns
//! results to the caller.
//!
//! Subagents run in their own context window with a custom system prompt and
//! optional tool restrictions, and report results back to the parent agent.
//!
//! Subagents **cannot** spawn other subagents (no nesting).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tracing::info;
use uuid::Uuid;

use sdk_core::error::{AgentId, SdkResult};
use crate::builder::{CommandToolPolicy, DefaultToolsetBuilder, ToolFilter};
use crate::worktree::{self, IsolationMode};
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
    /// Isolation mode: run in the main working tree or a dedicated git worktree.
    #[serde(default)]
    pub isolation: IsolationMode,
}

fn default_max_turns() -> usize {
    sdk_core::config::AgentConfig::default().subagent_max_turns
}

fn default_max_context_tokens() -> usize {
    sdk_core::config::AgentConfig::default().subagent_max_context_tokens
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
            isolation: IsolationMode::default(),
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

    /// Set the isolation mode (e.g. `IsolationMode::Worktree`).
    pub fn with_isolation(mut self, mode: IsolationMode) -> Self {
        self.isolation = mode;
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
    /// Branch name when the subagent ran in a worktree and left changes behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    /// Worktree path when the subagent ran in a worktree and left changes behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
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
            worktree_branch: None,
            worktree_path: None,
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
    /// When the definition uses `IsolationMode::Worktree`, the subagent runs
    /// inside a dedicated git worktree and changes are preserved on a branch
    /// for the user to merge.
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

        // Optionally create a worktree for isolation.
        let wt_handle = if def.isolation == IsolationMode::Worktree {
            Some(worktree::create_worktree(&self.source_root, agent_id, &def.name).await?)
        } else {
            None
        };

        // Determine effective work_dir (worktree path or the default).
        let effective_work_dir = wt_handle
            .as_ref()
            .map(|h| h.path.clone())
            .unwrap_or_else(|| self.work_dir.clone());

        // Build tool registry with restrictions, using effective work_dir.
        let tools = self.build_tools_with_work_dir(def, &effective_work_dir);

        // Build system prompt
        let system_prompt = sdk_core::prompts::subagent_system_prompt(
            &def.prompt,
            &self.source_root,
            &effective_work_dir,
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
                let mut result = SubAgentResult::from((agent_id, def.name.as_str(), loop_result));

                // Handle worktree cleanup — preserve branch when there are changes.
                if let Some(ref handle) = wt_handle {
                    let has_changes = worktree::has_uncommitted_changes(&handle.path).await;
                    if has_changes {
                        result.worktree_branch = Some(handle.branch.clone());
                        result.worktree_path = Some(
                            handle.path.to_string_lossy().to_string(),
                        );
                    }
                    worktree::cleanup_worktree(&self.source_root, handle, has_changes).await?;
                }

                self.emit(AgentEvent::SubAgentCompleted {
                    agent_id,
                    name: def.name.clone(),
                    tokens_used: result.total_tokens,
                    iterations: result.iterations,
                    tool_calls: result.tool_calls_count,
                    final_content: result.final_content.clone(),
                    worktree_path: result.worktree_path.clone(),
                    branch: result.worktree_branch.clone(),
                });

                Ok(result)
            }
            Err(e) => {
                // Best-effort worktree cleanup on failure.
                if let Some(ref handle) = wt_handle {
                    let _ = worktree::cleanup_worktree(&self.source_root, handle, false).await;
                }

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
    /// Accepts a `work_dir` so callers can point at a worktree path.
    fn build_tools_with_work_dir(&self, def: &SubAgentDef, work_dir: &Path) -> ToolRegistry {
        let filter = if def.allowed_tools.is_empty() {
            ToolFilter::default()
        } else {
            ToolFilter::allow_only(def.allowed_tools.clone())
        }
        .deny(def.disallowed_tools.clone());

        DefaultToolsetBuilder::with_filter(filter)
            .add_core_tools(
                self.source_root.clone(),
                work_dir.to_path_buf(),
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
            isolation: IsolationMode::None,
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
            isolation: IsolationMode::None,
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
            isolation: IsolationMode::None,
        },
        SubAgentDef {
            name: "code-reviewer".to_string(),
            description: "Reviews code for bugs, security issues, style problems, and performance. \
                Use when you need a second opinion on code changes or want a thorough review \
                before committing. Read-only — does not modify files.".to_string(),
            prompt: sdk_core::prompts::CODE_REVIEWER_PROMPT.to_string(),
            allowed_tools: vec![
                "read_file".to_string(),
                "list_directory".to_string(),
                "search_files".to_string(),
                "run_command".to_string(),
                "glob".to_string(),
                "grep".to_string(),
            ],
            disallowed_tools: vec![
                "write_file".to_string(),
                "edit_file".to_string(),
            ],
            model: None,
            max_turns: 20,
            max_context_tokens: 200_000,
            background: false,
            isolation: IsolationMode::None,
        },
        SubAgentDef {
            name: "test-runner".to_string(),
            description: "Runs tests, analyzes failures, and suggests fixes. Use when you need \
                to verify code changes pass tests, investigate test failures, or get a test \
                status report. Read-only files but can run test commands.".to_string(),
            prompt: sdk_core::prompts::TEST_RUNNER_PROMPT.to_string(),
            allowed_tools: vec![
                "read_file".to_string(),
                "list_directory".to_string(),
                "search_files".to_string(),
                "run_command".to_string(),
                "glob".to_string(),
                "grep".to_string(),
            ],
            disallowed_tools: vec![
                "write_file".to_string(),
                "edit_file".to_string(),
            ],
            model: None,
            max_turns: 15,
            max_context_tokens: 200_000,
            background: false,
            isolation: IsolationMode::None,
        },
        SubAgentDef {
            name: "refactor".to_string(),
            description: "Restructures code while preserving behavior. Use for code cleanup, \
                renaming, extracting functions, reorganizing modules, or other structural \
                improvements. Has full file access including edit_file for surgical changes.".to_string(),
            prompt: sdk_core::prompts::REFACTOR_PROMPT.to_string(),
            allowed_tools: Vec::new(), // all tools
            disallowed_tools: Vec::new(),
            model: None,
            max_turns: 25,
            max_context_tokens: 200_000,
            background: false,
            isolation: IsolationMode::None,
        },
    ]
}
