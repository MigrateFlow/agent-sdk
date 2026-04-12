use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::agent::agent_loop::BackgroundResult;
use crate::agent::events::AgentEvent;
use crate::agent::memory::MemoryStore;
use crate::agent::subagent::SubAgentRegistry;
use crate::error::AgentId;
use crate::task::store::TaskStore;
use crate::traits::llm_client::LlmClient;
use crate::traits::tool::Tool;

use super::command_tools::RunCommandTool;
use super::context_tools::{GetTaskContextTool, ListCompletedTasksTool};
use super::edit_tools::EditFileTool;
use super::fs_tools::{ListDirectoryTool, ReadFileTool, WriteFileTool};
use super::glob_tools::GlobTool;
use super::grep_tools::GrepTool;
use super::lsp_tools::{
    LspDocumentSymbolsTool, LspFindReferencesTool, LspGotoDefinitionTool, SharedLspManager,
};
use super::memory_tools::{
    DeleteMemoryTool, ListMemoryTool, ReadMemoryTool, SearchMemoryTool, WriteMemoryTool,
};
use super::registry::ToolRegistry;
use super::search_tools::SearchFilesTool;
use super::subagent_tools::SpawnSubAgentTool;
use super::team_tools::SpawnAgentTeamTool;
use super::todo_tools::TodoWriteTool;
use super::web_tools::WebSearchTool;
use crate::lsp::{work_dir_to_root_uri, LspConfig, LspManager};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub enum CommandToolPolicy {
    #[default]
    Unrestricted,
    AllowList(Vec<String>),
}

#[derive(Debug, Clone, Default)]
pub struct ToolFilter {
    allowed: Option<HashSet<String>>,
    denied: HashSet<String>,
}

impl ToolFilter {
    pub fn allow_only<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allowed: Some(names.into_iter().map(Into::into).collect()),
            denied: HashSet::new(),
        }
    }

    pub fn deny<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.denied.extend(names.into_iter().map(Into::into));
        self
    }

    fn allows(&self, name: &str) -> bool {
        let pass_allow = self
            .allowed
            .as_ref()
            .map(|allowed| allowed.contains(name))
            .unwrap_or(true);
        pass_allow && !self.denied.contains(name)
    }
}

#[derive(Clone)]
pub struct TeamToolConfig {
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
    pub llm_client: Arc<dyn LlmClient>,
    pub event_tx: Option<UnboundedSender<AgentEvent>>,
    pub background_tx: Option<UnboundedSender<BackgroundResult>>,
}

#[derive(Clone)]
pub struct SubAgentToolConfig {
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
    pub llm_client: Arc<dyn LlmClient>,
    pub event_tx: Option<UnboundedSender<AgentEvent>>,
    pub registry: Arc<SubAgentRegistry>,
    pub background_tx: Option<UnboundedSender<BackgroundResult>>,
}

pub struct DefaultToolsetBuilder {
    registry: ToolRegistry,
    filter: ToolFilter,
}

impl Default for DefaultToolsetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultToolsetBuilder {
    pub fn new() -> Self {
        Self {
            registry: ToolRegistry::new(),
            filter: ToolFilter::default(),
        }
    }

    pub fn with_filter(filter: ToolFilter) -> Self {
        Self {
            registry: ToolRegistry::new(),
            filter,
        }
    }

    pub fn add_core_tools(
        mut self,
        source_root: PathBuf,
        work_dir: PathBuf,
        command_policy: CommandToolPolicy,
    ) -> Self {
        self.register(ReadFileTool {
            source_root: source_root.clone(),
            work_dir: work_dir.clone(),
        });
        self.register(WriteFileTool {
            work_dir: work_dir.clone(),
        });
        self.register(ListDirectoryTool {
            source_root: source_root.clone(),
            work_dir: work_dir.clone(),
        });
        self.register(EditFileTool {
            source_root: source_root.clone(),
            work_dir: work_dir.clone(),
        });
        self.register(GlobTool {
            source_root: source_root.clone(),
        });
        self.register(GrepTool {
            source_root: source_root.clone(),
        });
        self.register(SearchFilesTool { source_root });
        self.register(WebSearchTool);

        match command_policy {
            CommandToolPolicy::Unrestricted => {
                self.register(RunCommandTool::with_defaults(work_dir));
            }
            CommandToolPolicy::AllowList(allowed_commands) => {
                self.register(RunCommandTool::with_commands(work_dir, allowed_commands));
            }
        }

        self
    }

    pub fn add_memory_tools(
        mut self,
        memory_store: Arc<MemoryStore>,
        agent_id: AgentId,
    ) -> Self {
        self.register(ReadMemoryTool {
            memory_store: memory_store.clone(),
        });
        self.register(WriteMemoryTool {
            memory_store: memory_store.clone(),
            agent_id,
        });
        self.register(ListMemoryTool {
            memory_store: memory_store.clone(),
        });
        self.register(SearchMemoryTool {
            memory_store: memory_store.clone(),
        });
        self.register(DeleteMemoryTool { memory_store });
        self
    }

    pub fn add_task_context_tools(mut self, task_store: Arc<TaskStore>) -> Self {
        self.register(GetTaskContextTool {
            task_store: task_store.clone(),
        });
        self.register(ListCompletedTasksTool { task_store });
        self
    }

    pub fn add_team_tool(mut self, config: TeamToolConfig) -> Self {
        self.register(SpawnAgentTeamTool {
            work_dir: config.work_dir,
            source_root: config.source_root,
            llm_client: config.llm_client,
            event_tx: config.event_tx,
            background_tx: config.background_tx,
        });
        self
    }

    pub fn add_subagent_tool(mut self, config: SubAgentToolConfig) -> Self {
        self.register(SpawnSubAgentTool {
            work_dir: config.work_dir,
            source_root: config.source_root,
            llm_client: config.llm_client,
            event_tx: config.event_tx,
            registry: config.registry,
            background_tx: config.background_tx,
        });
        self
    }

    pub fn add_todo_tool(mut self, items: Arc<Mutex<Vec<super::todo_tools::TodoItem>>>) -> Self {
        self.register(TodoWriteTool { items });
        self
    }

    /// Register the three LSP code-intelligence tools. Reads the LSP
    /// manifest from `manifest_path`. If the manifest is missing, this is a
    /// no-op that logs at debug level; a malformed manifest is also logged
    /// and skipped so the CLI stays usable when the file is broken.
    pub fn add_lsp_tools(mut self, manifest_path: PathBuf, work_dir: PathBuf) -> Self {
        if !manifest_path.exists() {
            tracing::debug!(
                path = %manifest_path.display(),
                "LSP manifest missing; skipping LSP tool registration"
            );
            return self;
        }
        let config = match LspConfig::load(&manifest_path) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Failed to parse LSP manifest; skipping LSP tool registration"
                );
                return self;
            }
        };
        if config.is_empty() {
            tracing::debug!("LSP manifest is empty; skipping LSP tool registration");
            return self;
        }
        let root_uri = match work_dir_to_root_uri(&work_dir) {
            Ok(uri) => uri,
            Err(e) => {
                tracing::warn!(error = %e, "Could not build LSP root URI; skipping LSP tools");
                return self;
            }
        };
        let manager: SharedLspManager = Arc::new(Mutex::new(LspManager::new(config, root_uri)));
        // Use `work_dir` as source_root when no explicit root is available.
        let source_root = work_dir.clone();
        self.register(LspGotoDefinitionTool {
            manager: manager.clone(),
            work_dir: work_dir.clone(),
            source_root: source_root.clone(),
        });
        self.register(LspFindReferencesTool {
            manager: manager.clone(),
            work_dir: work_dir.clone(),
            source_root: source_root.clone(),
        });
        self.register(LspDocumentSymbolsTool {
            manager,
            work_dir,
            source_root,
        });
        self
    }

    pub fn add_custom_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        let name = tool.definition().name;
        if self.filter.allows(&name) {
            self.registry.register(tool);
        }
        self
    }

    pub fn build(self) -> ToolRegistry {
        self.registry
    }

    fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        let name = tool.definition().name;
        if self.filter.allows(&name) {
            self.registry.register(Arc::new(tool));
        }
    }
}
