use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::error::AgentId;
use crate::traits::llm_client::LlmClient;
use crate::traits::prompt_builder::PromptBuilder;
use crate::mailbox::broker::MessageBroker;
use crate::task::store::TaskStore;

use super::events::AgentEvent;
use super::memory::MemoryStore;

/// Per-agent working state. Each teammate gets its own context.
#[derive(Clone)]
pub struct AgentContext {
    pub agent_id: AgentId,
    pub task_store: Arc<TaskStore>,
    pub broker: Arc<MessageBroker>,
    pub llm_client: Arc<dyn LlmClient>,
    pub prompt_builder: Arc<dyn PromptBuilder>,
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
    pub poll_interval_ms: u64,
    pub memory_store: Arc<MemoryStore>,
    pub max_loop_iterations: usize,
    pub event_tx: Option<UnboundedSender<AgentEvent>>,
}
