use crate::types::task::Task;
use crate::tools::registry::ToolRegistry;

/// Trait for building task-specific system prompts and user messages.
///
/// Implement this to customize how agents interpret tasks in your domain.
/// The SDK provides a `DefaultPromptBuilder` that works for generic tasks.
pub trait PromptBuilder: Send + Sync {
    /// Build the system prompt for a given task.
    fn build_system_prompt(&self, task: &Task, source_root: &std::path::Path, work_dir: &std::path::Path) -> String;

    /// Build the initial user message for the agent loop.
    fn build_user_message(&self, task: &Task) -> String;

    /// Optionally customize the tool registry for a given task.
    /// Default implementation returns the registry unchanged.
    fn customize_tools(&self, _task: &Task, registry: ToolRegistry) -> ToolRegistry {
        registry
    }
}

/// Default prompt builder that creates generic task prompts.
pub struct DefaultPromptBuilder;

impl PromptBuilder for DefaultPromptBuilder {
    fn build_system_prompt(&self, task: &Task, source_root: &std::path::Path, work_dir: &std::path::Path) -> String {
        crate::prompts::teammate_system_prompt(task, source_root, work_dir)
    }

    fn build_user_message(&self, task: &Task) -> String {
        crate::prompts::teammate_user_message(task)
    }
}
