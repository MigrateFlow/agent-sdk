use std::path::Path;

use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use crate::display::{display_path, floor_char_boundary, format_token_count, truncate};

/// Detect current git branch (returns None if not a git repo).
pub fn git_branch(work_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(work_dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

pub fn print_welcome(
    model: &str,
    provider: &str,
    work_dir: &Path,
    tool_count: usize,
    mcp_count: usize,
    session_id: Option<&str>,
) {
    let version = env!("CARGO_PKG_VERSION");
    let branch = git_branch(work_dir);
    let dir = display_path(work_dir);

    eprintln!();
    eprintln!(
        " {} {}",
        style("✻").cyan().bold(),
        style(format!("Agent v{}", version)).bold(),
    );

    let cwd_line = if let Some(ref b) = branch {
        format!("{} ({})", dir, style(b).cyan())
    } else {
        dir
    };
    eprintln!("   {} {}", style("cwd:").dim(), cwd_line);
    eprintln!(
        "   {} {} ({}) · {} tools",
        style("model:").dim(),
        model,
        style(provider).dim(),
        style(tool_count).dim(),
    );
    if mcp_count > 0 {
        eprintln!(
            "   {} {} server{}",
            style("mcp:").dim(),
            style(mcp_count).dim(),
            if mcp_count == 1 { "" } else { "s" },
        );
    }
    if let Some(id) = session_id {
        eprintln!(
            "   {} {}",
            style("session:").dim(),
            style(id).dim(),
        );
    }
    eprintln!();
    eprintln!(
        "   {}",
        style("/help for commands · Ctrl+C to interrupt · Ctrl+C twice to quit").dim()
    );
    eprintln!(
        "   {}",
        style("────────────────────────────────────────────────────").dim()
    );
    eprintln!();
}

pub fn create_spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("  {spinner:.dim} {msg:.dim}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

/// Format a tool call for display (Claude Code style).
pub fn format_tool_label(tool_name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();

    match tool_name {
        "read_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("{} {}", style("Read").bold(), style(path).cyan())
        }
        "write_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("{} {}", style("Write").bold(), style(path).cyan())
        }
        "list_directory" => {
            let path = arg_str(&args, "path").unwrap_or(".");
            format!("{} {}", style("List").bold(), style(path).cyan())
        }
        "search_files" => {
            let file_pat = arg_str(&args, "file_pattern");
            let content_pat = arg_str(&args, "content_pattern");
            match (file_pat, content_pat) {
                (Some(fp), Some(cp)) => {
                    format!("{} {} for {}", style("Search").bold(), style(fp).cyan(), style(format!("\"{}\"", cp)).white())
                }
                (Some(fp), None) => {
                    format!("{} {}", style("Search").bold(), style(fp).cyan())
                }
                (None, Some(cp)) => {
                    format!("{} {}", style("Search").bold(), style(format!("\"{}\"", cp)).white())
                }
                _ => format!("{}", style("Search").bold()),
            }
        }
        "web_search" => {
            let query = arg_str(&args, "query").unwrap_or("web");
            format!("{} \"{}\"", style("Web Search").bold(), style(query).white())
        }
        "run_command" => {
            let cmd = arg_str(&args, "command").unwrap_or("?");
            let short = if cmd.len() > 80 { &cmd[..floor_char_boundary(cmd, 80)] } else { cmd };
            format!("{}", style(format!("$ {}", short)).white())
        }
        "spawn_agent_team" => {
            format!("{}", style("Spawn Agent Team").bold().magenta())
        }
        "spawn_subagent" => {
            let name = arg_str(&args, "name").unwrap_or("subagent");
            let bg = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
            if bg {
                format!("{} {} {}", style("Spawn Subagent").bold(), style(name).cyan().bold(), style("(background)").dim())
            } else {
                format!("{} {}", style("Spawn Subagent").bold(), style(name).cyan().bold())
            }
        }
        "edit_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("{} {}", style("Edit").bold(), style(path).cyan())
        }
        "glob" => {
            let pattern = arg_str(&args, "pattern").unwrap_or("?");
            format!("{} {}", style("Glob").bold(), style(pattern).cyan())
        }
        "grep" => {
            let pattern = arg_str(&args, "pattern").unwrap_or("?");
            let mode = arg_str(&args, "output_mode").unwrap_or("files_with_matches");
            format!("{} {} ({})", style("Grep").bold(), style(format!("\"{}\"", pattern)).white(), mode)
        }
        "todo_write" => format!("{}", style("Todo").bold()),
        "update_task_list" => format!("{}", style("Update Task List").bold()),
        "read_memory" => {
            let key = arg_str(&args, "key").unwrap_or("?");
            format!("{} {}", style("Read Memory").bold(), style(key).cyan())
        }
        "write_memory" => {
            let key = arg_str(&args, "key").unwrap_or("?");
            format!("{} {}", style("Write Memory").bold(), style(key).cyan())
        }
        "list_memory" => {
            let prefix = arg_str(&args, "prefix").unwrap_or("");
            if prefix.is_empty() {
                format!("{}", style("List Memory").bold())
            } else {
                format!("{} {}", style("List Memory").bold(), style(prefix).cyan())
            }
        }
        "search_memory" => {
            let query = arg_str(&args, "query").unwrap_or("?");
            format!("{} \"{}\"", style("Search Memory").bold(), style(query).white())
        }
        "delete_memory" => {
            let key = arg_str(&args, "key").unwrap_or("?");
            format!("{} {}", style("Delete Memory").bold(), style(key).cyan())
        }
        _ => {
            let name = humanize(tool_name);
            format!("{}", style(name).bold())
        }
    }
}

/// Format a tool result preview line.
pub fn format_result_preview(tool_name: &str, result: &str) -> String {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();

    if let Some(err) = val["error"].as_str() {
        return format!("{} {}", style("✗").red(), style(truncate(err, 80)).red());
    }

    match tool_name {
        "read_file" => {
            let lines = val["lines"].as_u64().unwrap_or(0);
            let lines_returned = val["lines_returned"].as_u64().unwrap_or(lines);
            if lines_returned < lines {
                format!("{} lines (showing {})", lines, lines_returned)
            } else {
                format!("{} lines", lines)
            }
        }
        "write_file" => {
            let written = val["lines_written"].as_u64().unwrap_or(0);
            let bytes = val["bytes_written"].as_u64().unwrap_or(0);
            format!("{} lines · {} bytes written", written, bytes)
        }
        "list_directory" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} items", count)
        }
        "search_files" => {
            if let Some(n) = val["files_with_matches"].as_u64() {
                format!("{} files matched", n)
            } else if let Some(n) = val["total_matches"].as_u64() {
                format!("{} matches", n)
            } else {
                "done".to_string()
            }
        }
        "web_search" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} results", count)
        }
        "run_command" => {
            let code = val["exit_code"].as_i64().unwrap_or(-1);
            if code == 0 {
                let stdout = val["stdout"].as_str().unwrap_or("");
                let lines = stdout.lines().count();
                format!("{} ({} lines)", style("✓").green(), lines)
            } else {
                let stderr = val["stderr"].as_str().unwrap_or("");
                let first_line = stderr.lines().next().unwrap_or("failed");
                format!("{} exit {} — {}", style("✗").red(), code, truncate(first_line, 60))
            }
        }
        "spawn_agent_team" => {
            let status = val["status"].as_str().unwrap_or("?");
            let completed = val["tasks_completed"].as_u64().unwrap_or(0);
            let total = val["total_tasks"].as_u64().unwrap_or(0);
            format!("{} ({}/{} tasks)", status, completed, total)
        }
        "spawn_subagent" => {
            let status = val["status"].as_str().unwrap_or("?");
            let name = val["name"].as_str().unwrap_or("subagent");
            let tokens = val["total_tokens"].as_u64().unwrap_or(0);
            let tool_calls = val["tool_calls"].as_u64().unwrap_or(0);
            if status == "background" {
                format!("{} launched in background", name)
            } else {
                format!("{} {} · {} tokens · {} tools", name, status, format_token_count(tokens), tool_calls)
            }
        }
        "edit_file" => {
            let replacements = val["replacements_made"].as_u64().unwrap_or(0);
            format!("{} replacement(s)", replacements)
        }
        "glob" => {
            let shown = val["shown"].as_u64().unwrap_or(0);
            let total = val["total_matches"].as_u64().unwrap_or(0);
            if shown < total { format!("{} files (showing {})", total, shown) } else { format!("{} files", total) }
        }
        "grep" => {
            if let Some(n) = val["files_with_matches"].as_u64().or(val["total_matches"].as_u64()) {
                format!("{} files", n)
            } else if let Some(n) = val["total_shown"].as_u64() {
                format!("{} matches", n)
            } else {
                "done".to_string()
            }
        }
        "todo_write" => {
            let count = val["count"].as_u64().unwrap_or(0);
            let completed = val["completed"].as_u64().unwrap_or(0);
            format!("{}/{} completed", completed, count)
        }
        "update_task_list" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} tasks", count)
        }
        "read_memory" => {
            let bytes = val["content"].as_str().map(|s| s.len()).unwrap_or(0);
            if bytes > 0 { format!("{} bytes", bytes) } else { "not found".to_string() }
        }
        "write_memory" => { format!("{} saved", val["key"].as_str().unwrap_or("?")) }
        "list_memory" => { format!("{} keys", val["keys"].as_array().map(|a| a.len()).unwrap_or(0)) }
        "search_memory" => { format!("{} results", val["results"].as_array().map(|a| a.len()).unwrap_or(0)) }
        "delete_memory" => {
            if val["deleted"].as_bool().unwrap_or(false) { "deleted".to_string() } else { "not found".to_string() }
        }
        _ => truncate(result, 80),
    }
}

pub fn print_team_plan(arguments: &str) {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let teammates = args["teammates"].as_array().cloned().unwrap_or_default();
    let tasks = args["tasks"].as_array().cloned().unwrap_or_default();
    let auto_assign = args["auto_assign"].as_bool().unwrap_or(true);

    if !teammates.is_empty() {
        eprintln!("    {} {}", style("Teammates").dim(), style(format!("({})", teammates.len())).dim());
        for (i, teammate) in teammates.iter().enumerate() {
            let name = teammate["name"].as_str().unwrap_or("unnamed");
            let role = teammate["role"].as_str().unwrap_or("");
            let needs_plan = teammate["require_plan_approval"].as_bool().unwrap_or(false);
            let connector = if i == teammates.len() - 1 && tasks.is_empty() { "⎿" } else { "│" };
            let suffix = if needs_plan { format!(" {}", style("[plan approval]").yellow()) } else { String::new() };
            eprintln!("    {} {} {}{}", style(connector).dim(), style(name).magenta().bold(), style(truncate(role, 60)).dim(), suffix);
        }
    }

    if !tasks.is_empty() {
        let assign_label = if auto_assign { "auto-assign" } else { "claim freely" };
        eprintln!("    {} {} ({})", style("│").dim(), style("Tasks").dim(), style(assign_label).dim());
        for (idx, task) in tasks.iter().enumerate() {
            let title = task["title"].as_str().unwrap_or("untitled");
            let depends_on = task["depends_on"].as_array().cloned().unwrap_or_default();
            let connector = if idx == tasks.len() - 1 { "⎿" } else { "│" };
            let dep_str = if depends_on.is_empty() {
                String::new()
            } else {
                let deps = depends_on.iter().filter_map(|v| v.as_u64()).map(|v| (v + 1).to_string()).collect::<Vec<_>>().join(", ");
                format!(" {}", style(format!("[deps: {}]", deps)).dim())
            };
            eprintln!("    {} {} {}{}", style(connector).dim(), style(format!("{}.", idx + 1)).magenta(), style(title).white(), dep_str);
        }
    }
}

pub fn print_team_result_summary(result: &str) {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();
    let assignments = val["task_assignments"].as_array().cloned().unwrap_or_default();
    if assignments.is_empty() { return; }

    eprintln!("    {}", style("Assignments").dim());
    for (idx, assignment) in assignments.iter().enumerate() {
        let title = assignment["title"].as_str().unwrap_or("untitled");
        let target = assignment["target_file"].as_str().unwrap_or("?");
        let assignee = assignment["assigned_teammate"].as_str().unwrap_or("unassigned");
        let connector = if idx == assignments.len() - 1 { "⎿" } else { "│" };
        eprintln!("    {} {} {} {}", style(connector).dim(), style(title).white(), style(format!("→ {}", target)).dim(), style(format!("[{}]", assignee)).cyan());
    }
}

pub fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

/// Infer a language hint from file extension for display.
pub fn lang_hint(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => ", Rust",
        "ts" | "tsx" => ", TypeScript",
        "js" | "jsx" => ", JavaScript",
        "py" => ", Python",
        "go" => ", Go",
        "java" => ", Java",
        "rb" => ", Ruby",
        "toml" => ", TOML",
        "json" => ", JSON",
        "yaml" | "yml" => ", YAML",
        "md" => ", Markdown",
        "html" => ", HTML",
        "css" => ", CSS",
        "sh" | "bash" => ", Shell",
        "sql" => ", SQL",
        _ => "",
    }
}

pub fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, part) in name.split('_').filter(|s| !s.is_empty()).enumerate() {
        if i > 0 { out.push(' '); }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() { name.to_string() } else { out }
}

pub const MAX_TOOL_RESULT_CHARS: usize = 12_000;

pub fn truncate_tool_result(s: &str) -> String {
    if s.len() <= MAX_TOOL_RESULT_CHARS {
        return s.to_string();
    }

    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(content) = val.get_mut("content") {
            if let Some(text) = content.as_str() {
                if text.len() > MAX_TOOL_RESULT_CHARS - 200 {
                    let limit = floor_char_boundary(text, MAX_TOOL_RESULT_CHARS - 200);
                    let truncated = format!(
                        "{}…\n\n[truncated: {}/{} chars — use offset to read more]",
                        &text[..limit], limit, text.len()
                    );
                    *content = serde_json::Value::String(truncated);
                    let fallback_end = floor_char_boundary(s, MAX_TOOL_RESULT_CHARS);
                    return serde_json::to_string(&val)
                        .unwrap_or_else(|_| s[..fallback_end].to_string());
                }
            }
        }
    }

    let end = floor_char_boundary(s, MAX_TOOL_RESULT_CHARS);
    format!("{}…[truncated: {}/{} chars]", &s[..end], end, s.len())
}

pub fn format_duration(d: std::time::Duration) -> String {
    if d.as_secs() >= 60 {
        format!("{}m{:.0}s", d.as_secs() / 60, d.as_secs() % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Print one-line turn usage stats.
pub fn print_usage(tokens: u64, tool_calls: usize, iterations: usize, duration: std::time::Duration) {
    let dur = format_duration(duration);
    let mut parts: Vec<String> = Vec::new();
    if iterations > 1 {
        parts.push(format!("{} iterations", iterations));
    }
    parts.push(format!("{} tokens", format_token_count(tokens)));
    parts.push(format!("{} tool {}", tool_calls, if tool_calls == 1 { "use" } else { "uses" }));
    parts.push(dur);
    eprintln!("  {}", style(parts.join(" · ")).dim());
    eprintln!();
}
