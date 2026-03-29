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
        format!(
            r#"You are an expert worker agent.

## Environment
- Source directory (read-only): {source}
- Output directory (write target): {output}

## Available Tools
- `read_file` — Read files from source or output dir
- `write_file` — Write files to output directory
- `list_directory` — List files/directories
- `search_files` — Search for file patterns and content
- `run_command` — Run shell commands in output directory
- `read_memory` / `write_memory` / `list_memory` — Shared team context
- `get_task_context` / `list_completed_tasks` — See what other agents did

## Your Task
{title}
{description}

Target file: {target_file}

## Approach
1. Read relevant source files
2. Check what other agents already produced
3. Use memory for shared patterns and conventions
4. Write output using `write_file`
5. Verify your output compiles/parses
6. Respond with a brief summary"#,
            source = source_root.display(),
            output = work_dir.display(),
            title = task.title,
            description = task.description,
            target_file = task.target_file.display(),
        )
    }

    fn build_user_message(&self, task: &Task) -> String {
        format!(
            "Process this task: {}\n\nTarget: {}\n\nContext:\n{}",
            task.title,
            task.target_file.display(),
            serde_json::to_string_pretty(&task.context).unwrap_or_default()
        )
    }
}
