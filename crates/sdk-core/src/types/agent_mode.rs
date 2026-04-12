use serde::{Deserialize, Serialize};

/// Agent operating mode — controls tool access and system prompt behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentMode {
    /// Full tool access, normal operation.
    Normal,
    /// Read-only exploration mode — restricted tools, plan-focused prompt.
    Plan,
}

impl Default for AgentMode {
    fn default() -> Self {
        Self::Normal
    }
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "normal"),
            Self::Plan => write!(f, "plan"),
        }
    }
}

/// Tools allowed in Plan mode (read-only exploration tools).
pub const PLAN_MODE_READONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_directory",
    "glob",
    "grep",
    "search_files",
    "web_search",
    "todo_write",
    "update_task_list",
    "spawn_subagent",
];

/// Check if a tool is allowed in plan mode.
pub fn is_plan_mode_tool(name: &str) -> bool {
    PLAN_MODE_READONLY_TOOLS.contains(&name)
}

/// System prompt suffix appended when in Plan mode.
pub fn plan_mode_system_suffix() -> &'static str {
    r#"

# PLAN MODE ACTIVE

You are currently in **Plan Mode**. In this mode:

1. **Read and explore only.** Use read_file, list_directory, glob, grep, search_files, and web_search to understand the codebase. You may also spawn subagents for focused exploration.
2. **Do NOT make changes.** Do not use write_file, edit_file, or run_command. These tools are not available in plan mode.
3. **Analyze and design.** Think through the approach, identify files that need changes, consider edge cases, and estimate scope.
4. **Present your plan.** When you have a clear understanding, present a structured plan to the user with:
   - Summary of what needs to change
   - List of files to modify/create
   - Key design decisions and trade-offs
   - Verification strategy (tests, manual checks)
5. **Exit plan mode.** The user can type `/exitplan` to switch back to normal mode for implementation.

Focus on understanding before proposing changes. Read the actual code, don't guess.
"#
}
