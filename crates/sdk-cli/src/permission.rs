use std::collections::HashSet;
use std::io::{self, BufRead, Write};

use console::style;

/// Result of a permission check for a tool call.
pub enum PermissionDecision {
    Allow,
    Deny,
    AlwaysAllow,
}

/// Tracks which tools the user has permanently approved for the current session.
pub struct PermissionState {
    /// Tools that the user said "always allow" for this session.
    always_allowed: HashSet<String>,
    /// If true, skip all prompts (--allow-all flag).
    allow_all: bool,
}

impl PermissionState {
    pub fn new(allow_all: bool) -> Self {
        Self {
            always_allowed: HashSet::new(),
            allow_all,
        }
    }

    /// Check whether a tool call should be allowed.
    ///
    /// Read-only tools and tools covered by `allow_all` or `always_allowed` are
    /// auto-approved. For everything else the user is prompted on stderr.
    ///
    /// Returns `true` if the tool execution should proceed.
    pub fn check_permission(
        &mut self,
        tool_name: &str,
        tool_args_preview: &str,
        is_read_only: bool,
        is_destructive: bool,
    ) -> bool {
        // Auto-allow read-only tools.
        if is_read_only {
            return true;
        }

        // Auto-allow when the global flag is set.
        if self.allow_all {
            return true;
        }

        // Auto-allow tools the user already said "always" for.
        if self.always_allowed.contains(tool_name) {
            return true;
        }

        // Prompt the user.
        let decision = if is_destructive {
            prompt_destructive(tool_name, tool_args_preview)
        } else {
            prompt_write(tool_name, tool_args_preview)
        };

        match decision {
            PermissionDecision::Allow => true,
            PermissionDecision::AlwaysAllow => {
                self.always_allowed.insert(tool_name.to_string());
                true
            }
            PermissionDecision::Deny => false,
        }
    }
}

/// Prompt for a destructive tool -- no "always" option.
fn prompt_destructive(tool_name: &str, preview: &str) -> PermissionDecision {
    let prompt_text = format!(
        "  {} Allow {} {}? [y/n] ",
        style("\u{26a0}").yellow(),
        style(tool_name).bold(),
        preview,
    );
    eprint!("{}", prompt_text);
    let _ = io::stderr().flush();

    let line = read_stdin_line();
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => PermissionDecision::Allow,
        _ => PermissionDecision::Deny,
    }
}

/// Prompt for a write tool -- offers "a" (always allow this tool).
fn prompt_write(tool_name: &str, preview: &str) -> PermissionDecision {
    let prompt_text = format!(
        "  Allow {} {}? [y/n/a] ",
        style(tool_name).bold(),
        preview,
    );
    eprint!("{}", prompt_text);
    let _ = io::stderr().flush();

    let line = read_stdin_line();
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => PermissionDecision::Allow,
        "a" | "always" => PermissionDecision::AlwaysAllow,
        _ => PermissionDecision::Deny,
    }
}

/// Read a single line from stdin (blocking).
fn read_stdin_line() -> String {
    let stdin = io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    line
}
