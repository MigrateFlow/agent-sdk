// Facade crate: re-exports from workspace sub-crates to preserve the public API.

// Re-export sub-crate modules under their original names.
pub use sdk_core::error;
pub use sdk_core::config;
pub use sdk_core::types;
pub use sdk_core::traits;
pub use sdk_core::prompts;
pub use sdk_core::storage;

// Modules that moved to sdk-core but were originally in other places
pub mod tools {
    pub use sdk_core::registry;
    pub use sdk_tools::fs_tools;
    pub use sdk_tools::edit_tools;
    pub use sdk_tools::glob_tools;
    pub use sdk_tools::grep_tools;
    pub use sdk_tools::command_tools;
    pub use sdk_tools::search_tools;
    pub use sdk_tools::web_tools;
    pub use sdk_tools::todo_tools;
    pub use sdk_tools::context_tools;
    pub use sdk_tools::memory_tools;
    pub use sdk_tools::mcp_tools;
    pub use sdk_tools::lsp_tools;
    pub use sdk_agent::builder;
    pub use sdk_agent::team_tools;
    pub use sdk_agent::subagent_tools;
}

pub mod agent {
    pub use sdk_agent::agent_loop;
    pub use sdk_agent::compaction;
    pub use sdk_agent::context;
    pub use sdk_agent::handle;
    pub use sdk_agent::registry;
    pub use sdk_agent::subagent;
    pub use sdk_agent::team;
    pub use sdk_agent::team_lead;
    pub use sdk_agent::teammate;
    // Types that moved to sdk-core
    pub use sdk_core::events;
    pub use sdk_core::hooks;
    pub use sdk_core::memory;
    pub use sdk_core::cost;
}

pub mod task {
    pub use sdk_task::task::store;
    pub use sdk_task::task::graph;
    pub use sdk_task::task::watcher;
    pub use sdk_task::task::file_lock;
}

pub mod mailbox {
    pub use sdk_task::mailbox::broker;
    pub use sdk_task::mailbox::mailbox;
}

pub mod llm {
    pub use sdk_llm::*;
}

pub mod mcp {
    pub use sdk_protocols::mcp::*;
}

pub mod lsp {
    pub use sdk_protocols::lsp::*;
}

pub mod cli {
    pub use sdk_cli::*;
}

// Convenience re-exports (preserve the existing public API)
pub use error::{AgentId, TaskId, SdkError, SdkResult};
pub use config::{LlmConfig, LlmProvider, AgentConfig, AGENT_DIR};
pub use sdk_agent::{AgentLoop, AgentTeam, TeamLead};
pub use sdk_agent::agent_loop::{AgentLoopResult, CompactionStrategy};
pub use sdk_core::background::{BackgroundResult, BackgroundResultKind};
pub use sdk_agent::subagent::{SubAgentDef, SubAgentRegistry, SubAgentResult, SubAgentRunner};
pub use sdk_agent::builder::{CommandToolPolicy, DefaultToolsetBuilder, ToolFilter};
pub use sdk_agent::team_lead::{ExecutionSummary, TeammateSpec};
pub use sdk_agent::teammate::Teammate;
pub use sdk_core::events::AgentEvent;
pub use sdk_core::memory::MemoryStore;
pub use sdk_core::types::memory::{MemoryEntry, MemoryType};
pub use sdk_core::hooks::{Hook, HookEvent, HookResult, HookRegistry};
pub use sdk_core::cost::{CostRecord, CostTracker};
pub use sdk_task::TaskStore;
pub use sdk_task::mailbox::broker::MessageBroker;
pub use sdk_core::traits::llm_client::{LlmClient, StreamDelta};
pub use sdk_core::traits::tool::{Tool, ToolDefinition};
pub use sdk_core::traits::prompt_builder::{PromptBuilder, DefaultPromptBuilder};
pub use sdk_core::types::chat::ChatMessage;
pub use sdk_core::types::task::{Task, TaskResult, TaskStatus};
pub use sdk_core::types::usage::TokenUsage;
pub use sdk_core::registry::ToolRegistry;
pub use sdk_llm::create_client;
pub use sdk_core::storage::AgentPaths;
pub use sdk_core::types::agent_mode::{AgentMode, PLAN_MODE_READONLY_TOOLS, is_plan_mode_tool, plan_mode_system_suffix};
pub use sdk_core::types::ultra_plan::{UltraPlanPhase, UltraPlanState};
pub use sdk_core::cache::{FileStateCache, ToolResultStore, StatsCache, CacheBreakDetector, CacheStats};
pub use sdk_protocols::mcp::{McpClient, McpConfig, McpServerSpec};
pub use sdk_protocols::lsp::{ChildLspClient, LspClient, LspConfig, LspManager, ServerSpec};
pub use sdk_cli::{CommandContext, CommandOutcome, SlashCommand, SlashCommandRegistry};
pub use sdk_cli::{SessionManager, SessionMetadata, SessionStatus};
