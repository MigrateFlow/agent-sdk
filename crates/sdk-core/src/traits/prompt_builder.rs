use crate::types::task::Task;
use crate::registry::ToolRegistry;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn task() -> Task {
        Task::new("transform", "t", "d", PathBuf::from("f"))
    }

    #[test]
    fn default_builder_system_prompt_contains_task_title() {
        let b = DefaultPromptBuilder;
        let p = b.build_system_prompt(
            &task(),
            &PathBuf::from("/src"),
            &PathBuf::from("/work"),
        );
        assert!(p.contains("/src"));
        assert!(p.contains("/work"));
        assert!(p.contains("t"));
    }

    #[test]
    fn default_builder_user_message_contains_task() {
        let b = DefaultPromptBuilder;
        let msg = b.build_user_message(&task());
        assert!(msg.contains("Process this task: t"));
    }

    #[test]
    fn default_customize_tools_returns_registry_unchanged() {
        let b = DefaultPromptBuilder;
        let reg = ToolRegistry::new();
        let out = b.customize_tools(&task(), reg);
        assert!(out.is_empty());
    }
}
