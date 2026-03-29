pub mod error;
pub mod config;
pub mod types;
pub mod traits;
pub mod llm;
pub mod task;
pub mod mailbox;
pub mod agent;
pub mod tools;

// Convenience re-exports
pub use error::{AgentId, TaskId, SdkError, SdkResult};
pub use config::{LlmConfig, LlmProvider, AgentConfig};
pub use agent::agent_loop::AgentLoop;
pub use agent::team::AgentTeam;
pub use agent::team_lead::{TeamLead, ExecutionSummary, TeammateSpec};
pub use agent::teammate::Teammate;
pub use agent::events::AgentEvent;
pub use agent::memory::MemoryStore;
pub use agent::hooks::{Hook, HookEvent, HookResult, HookRegistry};
pub use task::store::TaskStore;
pub use mailbox::broker::MessageBroker;
pub use traits::llm_client::LlmClient;
pub use traits::tool::{Tool, ToolDefinition};
pub use types::chat::ChatMessage;
pub use types::task::{Task, TaskResult, TaskStatus};
pub use llm::create_client;
