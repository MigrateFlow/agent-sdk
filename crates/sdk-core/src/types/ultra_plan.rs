use serde::{Serialize, Deserialize};

/// Phases of the UltraPlan structured planning workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UltraPlanPhase {
    Research,
    Design,
    Review,
    Implement,
}

impl Default for UltraPlanPhase {
    fn default() -> Self { Self::Research }
}

impl std::fmt::Display for UltraPlanPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Research => write!(f, "Research"),
            Self::Design => write!(f, "Design"),
            Self::Review => write!(f, "Review"),
            Self::Implement => write!(f, "Implement"),
        }
    }
}

/// State tracked across turns for an active UltraPlan session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UltraPlanState {
    pub phase: UltraPlanPhase,
}

impl Default for UltraPlanState {
    fn default() -> Self {
        Self {
            phase: UltraPlanPhase::Research,
        }
    }
}

/// Get the tools allowed for a given UltraPlan phase.
/// Returns an empty slice for the Implement phase, meaning all tools are allowed.
pub fn allowed_tools_for_phase(phase: &UltraPlanPhase) -> &'static [&'static str] {
    match phase {
        UltraPlanPhase::Research => &[
            "read_file", "list_directory", "glob", "grep", "search_files",
            "web_search", "spawn_subagent", "todo_write", "update_task_list",
        ],
        UltraPlanPhase::Design => &[
            "read_file", "list_directory", "glob", "grep", "search_files",
            "web_search", "todo_write", "update_task_list",
        ],
        UltraPlanPhase::Review => &[
            "read_file", "list_directory", "glob", "grep", "search_files",
            "run_command", "todo_write", "update_task_list",
        ],
        UltraPlanPhase::Implement => &[], // Empty means ALL tools allowed
    }
}

/// Get the next phase, or None if at the end.
pub fn next_phase(phase: &UltraPlanPhase) -> Option<UltraPlanPhase> {
    match phase {
        UltraPlanPhase::Research => Some(UltraPlanPhase::Design),
        UltraPlanPhase::Design => Some(UltraPlanPhase::Review),
        UltraPlanPhase::Review => Some(UltraPlanPhase::Implement),
        UltraPlanPhase::Implement => None,
    }
}

/// System prompt suffix for the current UltraPlan phase.
pub fn phase_system_suffix(phase: &UltraPlanPhase) -> &'static str {
    match phase {
        UltraPlanPhase::Research => r#"
# ULTRAPLAN: RESEARCH PHASE

You are in the **Research** phase of structured planning. Your goal is to deeply understand the problem and codebase.

**What to do:**
- Read relevant source files to understand the current implementation
- Search for related patterns, similar features, and dependencies
- Spawn subagents for parallel exploration of different areas
- Identify all files that will need changes
- Note any constraints, edge cases, or risks

**What NOT to do:**
- Do not write, edit, or create files
- Do not run commands that modify state
- Do not start implementing yet

**When done:** Present a summary of your findings. The user can advance to the Design phase with `/nextphase`.
"#,

        UltraPlanPhase::Design => r#"
# ULTRAPLAN: DESIGN PHASE

You are in the **Design** phase. Use your research findings to architect the solution.

**What to do:**
- Design the implementation approach (types, functions, modules)
- Identify interfaces between components
- Plan the order of changes
- Create a task list with `/update_task_list`
- Consider alternative approaches and trade-offs
- Document key design decisions

**What NOT to do:**
- Do not write, edit, or create files yet
- Do not run modification commands

**When done:** Present the design document. The user can advance to Review with `/nextphase`.
"#,

        UltraPlanPhase::Review => r#"
# ULTRAPLAN: REVIEW PHASE

You are in the **Review** phase. Validate your design before implementation.

**What to do:**
- Review the design against the original requirements
- Check for missing edge cases or error handling
- Run existing tests to establish a baseline (`run_command`)
- Verify that the planned changes won't break existing functionality
- Read any test files related to the changes

**What NOT to do:**
- Do not write, edit, or create files yet
- Only run commands for reading/testing, not modifying

**When done:** Present your review findings. The user can advance to Implement with `/nextphase`.
"#,

        UltraPlanPhase::Implement => r#"
# ULTRAPLAN: IMPLEMENT PHASE

You are in the **Implement** phase. Full tool access is restored. Execute your design.

**What to do:**
- Follow your design document from the Design phase
- Make changes in the order you planned
- Write tests for new functionality
- Run tests after each significant change
- Update the task list as you progress

**Guidelines:**
- Commit to the design -- don't redesign during implementation
- If you discover a blocker, note it but continue with what you can
- Verify your work compiles and tests pass
"#,
    }
}
