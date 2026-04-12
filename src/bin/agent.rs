use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use agent_sdk::config::{LlmConfig, LlmProvider};
use agent_sdk::tools::builder::{
    CommandToolPolicy, DefaultToolsetBuilder, SubAgentToolConfig, TeamToolConfig, ToolFilter,
};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::tools::mcp_tools::McpTool;
use agent_sdk::mcp::{McpClient, McpConfig, McpServerSpec, StdioTransport};
use agent_sdk::traits::tool::{Tool, ToolDefinition};
use agent_sdk::types::chat::ChatMessage;
use agent_sdk::cli::{
    display::{display_path, floor_char_boundary, format_token_count, print_task_list, truncate},
    session::{default_session_path, load_session, CliTask},
    CommandContext, CommandOutcome, SlashCommandRegistry,
};
use agent_sdk::{AgentEvent, AgentId, AgentMode, AgentPaths, MemoryStore, PLAN_MODE_READONLY_TOOLS, StreamDelta, UltraPlanState};
use agent_sdk::types::ultra_plan::{allowed_tools_for_phase, phase_system_suffix};
use clap::Parser;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "agent",
    about = "General-purpose AI agent CLI",
    version
)]
struct Cli {
    /// LLM provider: claude or openai (auto-detected from env)
    #[arg(short, long)]
    provider: Option<String>,

    /// Model name (auto-detected from env)
    #[arg(short, long)]
    model: Option<String>,

    /// Working directory
    #[arg(short = 'd', long, default_value = ".")]
    dir: PathBuf,

    /// Max tokens per LLM response
    #[arg(long, default_value = "16384")]
    max_tokens: usize,

    /// Max ReAct loop iterations per turn
    #[arg(long, default_value = "50")]
    max_iterations: usize,

    /// System prompt override
    #[arg(long)]
    system: Option<String>,

    /// Allow all shell commands (skip whitelist)
    #[arg(long)]
    allow_all_commands: bool,

    /// Output NDJSON events to stdout (for programmatic consumption)
    #[arg(long)]
    json: bool,

    /// Comma-separated list of tools to enable (default: all)
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Session file path
    #[arg(long)]
    session: Option<PathBuf>,

    /// Read one-shot prompt from a file instead of positional args
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// One-shot mode: run this prompt and exit
    prompt: Vec<String>,
}

// ─── NDJSON wire protocol ────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NdjsonEvent {
    Started { tools: Vec<String> },
    Thinking { content: String, iteration: usize },
    ToolCall { tool_call_id: String, tool_name: String, arguments: String, iteration: usize },
    ToolResult { tool_call_id: String, tool_name: String, content: String, iteration: usize },
    TextDelta { content: String },
    Completed { final_content: String, tokens_used: u64, iterations: usize, tool_calls: usize },
    Failed { error: String },
    // Team/subagent events for programmatic consumers
    TeamSpawned { teammate_count: usize },
    SubagentSpawned { name: String, description: String },
    SubagentProgress { name: String, iteration: usize, max_turns: usize, current_tool: Option<String>, tokens_so_far: u64 },
    SubagentCompleted { name: String, tokens_used: u64, iterations: usize, tool_calls: usize },
    SubagentFailed { name: String, error: String },
    TaskStarted { name: String, title: String },
    TaskCompleted { name: String, title: String, tokens_used: u64 },
    TaskFailed { name: String, title: String, error: String },
    #[allow(dead_code)]
    PlanModeChanged { mode: String },
}

fn emit_ndjson(event: &NdjsonEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{}", json);
    }
}

// ─── Display helpers ─────────────────────────────────────────────────────────

/// Detect current git branch (returns None if not a git repo).
fn git_branch(work_dir: &Path) -> Option<String> {
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

fn print_welcome(
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

fn create_spinner(msg: &str) -> ProgressBar {
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
fn format_tool_label(tool_name: &str, arguments: &str) -> String {
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
        "todo_write" => {
            format!("{}", style("Todo").bold())
        }
        "update_task_list" => {
            format!("{}", style("Update Task List").bold())
        }
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
fn format_result_preview(tool_name: &str, result: &str) -> String {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();

    // Check for error first
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
                format!(
                    "{} exit {} — {}",
                    style("✗").red(),
                    code,
                    truncate(first_line, 60)
                )
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
                format!(
                    "{} {} · {} tokens · {} tools",
                    name,
                    status,
                    format_token_count(tokens),
                    tool_calls
                )
            }
        }
        "edit_file" => {
            let replacements = val["replacements_made"].as_u64().unwrap_or(0);
            format!("{} replacement(s)", replacements)
        }
        "glob" => {
            let shown = val["shown"].as_u64().unwrap_or(0);
            let total = val["total_matches"].as_u64().unwrap_or(0);
            if shown < total {
                format!("{} files (showing {})", total, shown)
            } else {
                format!("{} files", total)
            }
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
            if bytes > 0 {
                format!("{} bytes", bytes)
            } else {
                "not found".to_string()
            }
        }
        "write_memory" => {
            let key = val["key"].as_str().unwrap_or("?");
            format!("{} saved", key)
        }
        "list_memory" => {
            let keys = val["keys"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("{} keys", keys)
        }
        "search_memory" => {
            let results = val["results"].as_array().map(|a| a.len()).unwrap_or(0);
            format!("{} results", results)
        }
        "delete_memory" => {
            if val["deleted"].as_bool().unwrap_or(false) {
                "deleted".to_string()
            } else {
                "not found".to_string()
            }
        }
        _ => truncate(result, 80),
    }
}

fn print_team_plan(arguments: &str) {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let teammates = args["teammates"].as_array().cloned().unwrap_or_default();
    let tasks = args["tasks"].as_array().cloned().unwrap_or_default();
    let auto_assign = args["auto_assign"].as_bool().unwrap_or(true);

    if !teammates.is_empty() {
        eprintln!(
            "    {} {}",
            style("Teammates").dim(),
            style(format!("({})", teammates.len())).dim(),
        );
        for (i, teammate) in teammates.iter().enumerate() {
            let name = teammate["name"].as_str().unwrap_or("unnamed");
            let role = teammate["role"].as_str().unwrap_or("");
            let needs_plan = teammate["require_plan_approval"].as_bool().unwrap_or(false);
            let connector = if i == teammates.len() - 1 && tasks.is_empty() { "⎿" } else { "│" };
            let suffix = if needs_plan {
                format!(" {}", style("[plan approval]").yellow())
            } else {
                String::new()
            };
            eprintln!(
                "    {} {} {}{}",
                style(connector).dim(),
                style(name).magenta().bold(),
                style(truncate(role, 60)).dim(),
                suffix,
            );
        }
    }

    if !tasks.is_empty() {
        let assign_label = if auto_assign { "auto-assign" } else { "claim freely" };
        eprintln!(
            "    {} {} ({})",
            style("│").dim(),
            style("Tasks").dim(),
            style(assign_label).dim(),
        );
        for (idx, task) in tasks.iter().enumerate() {
            let title = task["title"].as_str().unwrap_or("untitled");
            let depends_on = task["depends_on"].as_array().cloned().unwrap_or_default();
            let connector = if idx == tasks.len() - 1 { "⎿" } else { "│" };
            let dep_str = if depends_on.is_empty() {
                String::new()
            } else {
                let deps = depends_on
                    .iter()
                    .filter_map(|v| v.as_u64())
                    .map(|v| (v + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" {}", style(format!("[deps: {}]", deps)).dim())
            };
            eprintln!(
                "    {} {} {}{}",
                style(connector).dim(),
                style(format!("{}.", idx + 1)).magenta(),
                style(title).white(),
                dep_str,
            );
        }
    }
}

fn print_team_result_summary(result: &str) {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();
    let assignments = val["task_assignments"].as_array().cloned().unwrap_or_default();
    if assignments.is_empty() {
        return;
    }

    eprintln!("    {}", style("Assignments").dim());
    for (idx, assignment) in assignments.iter().enumerate() {
        let title = assignment["title"].as_str().unwrap_or("untitled");
        let target = assignment["target_file"].as_str().unwrap_or("?");
        let assignee = assignment["assigned_teammate"].as_str().unwrap_or("unassigned");
        let connector = if idx == assignments.len() - 1 { "⎿" } else { "│" };
        eprintln!(
            "    {} {} {} {}",
            style(connector).dim(),
            style(title).white(),
            style(format!("→ {}", target)).dim(),
            style(format!("[{}]", assignee)).cyan(),
        );
    }
}

fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

/// Infer a language hint from file extension for display.
fn lang_hint(path: &str) -> &'static str {
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

fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, part) in name.split('_').filter(|s| !s.is_empty()).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() { name.to_string() } else { out }
}

const MAX_TOOL_RESULT_CHARS: usize = 12_000;

fn truncate_tool_result(s: &str) -> String {
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
                        &text[..limit],
                        limit,
                        text.len()
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
    format!(
        "{}…[truncated: {}/{} chars]",
        &s[..end],
        end,
        s.len()
    )
}

struct UpdateTaskListTool {
    tasks: Arc<Mutex<Vec<CliTask>>>,
}

#[async_trait]
impl Tool for UpdateTaskListTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "update_task_list".to_string(),
            description: "Update the visible task list for the current single-agent session. Use this for multi-step work to show the current tasks and their statuses. Status must be pending, in_progress, completed, or blocked.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "blocked"] }
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> agent_sdk::SdkResult<serde_json::Value> {
        let items = arguments["items"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            return Ok(json!({ "error": "Missing or empty 'items' array" }));
        }

        let tasks = items
            .into_iter()
            .filter_map(|item| {
                let title = item["title"].as_str()?.trim();
                let status = item["status"].as_str()?.trim();
                if title.is_empty() || status.is_empty() {
                    return None;
                }
                Some(CliTask {
                    title: title.to_string(),
                    status: status.to_string(),
                })
            })
            .collect::<Vec<_>>();

        if tasks.is_empty() {
            return Ok(json!({ "error": "No valid task items provided" }));
        }

        let mut guard = self.tasks.lock().expect("task list mutex poisoned");
        *guard = tasks;

        Ok(json!({ "updated": true, "count": guard.len() }))
    }
}

// ─── MCP ─────────────────────────────────────────────────────────────────────

/// Load and initialize all MCP servers declared in `.agent/mcp.json`.
/// Returns the list of tools to register. Failures for individual servers
/// are logged and skipped.
async fn load_mcp_tools(work_dir: &Path, json_mode: bool) -> Vec<Arc<dyn Tool>> {
    let paths = match agent_sdk::storage::AgentPaths::for_work_dir(work_dir) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let config_path = paths.project_mcp_config_path();

    let config = match McpConfig::load(&config_path) {
        Ok(c) => c,
        // Missing manifest is the common case — no MCP configured.
        Err(agent_sdk::SdkError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Vec::new();
        }
        Err(e) => {
            if !json_mode {
                eprintln!(
                    "  {} failed to read {}: {}",
                    style("⚠").yellow(),
                    display_path(&config_path),
                    e,
                );
            }
            return Vec::new();
        }
    };

    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();
    for server in &config.servers {
        match spawn_and_register_mcp_server(server).await {
            Ok(tools) => {
                if !json_mode && !tools.is_empty() {
                    eprintln!(
                        "  {} mcp server {} ({} tool{})",
                        style("✓").green(),
                        style(&server.name).cyan(),
                        tools.len(),
                        if tools.len() == 1 { "" } else { "s" },
                    );
                }
                all_tools.extend(tools);
            }
            Err(e) => {
                if !json_mode {
                    eprintln!(
                        "  {} mcp server {} failed: {}",
                        style("⚠").yellow(),
                        style(&server.name).cyan(),
                        e,
                    );
                }
            }
        }
    }
    all_tools
}

async fn spawn_and_register_mcp_server(
    server: &McpServerSpec,
) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
    let mut cmd = tokio::process::Command::new(&server.command);
    cmd.args(&server.args)
        .envs(&server.env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{}`: {}", server.command, e))?;
    let transport = StdioTransport::from_child(child)?;
    let mut client = McpClient::new(transport, server.name.clone());
    client.initialize().await?;
    let specs = client.list_tools().await?;

    let client = Arc::new(tokio::sync::Mutex::new(client));
    let mut tools: Vec<Arc<dyn Tool>> = Vec::with_capacity(specs.len());
    for spec in specs {
        tools.push(Arc::new(McpTool {
            client: client.clone(),
            spec,
            server_name: server.name.clone(),
        }));
    }
    Ok(tools)
}

// ─── Tools & session ─────────────────────────────────────────────────────────

fn build_tools(
    work_dir: &Path,
    allow_all: bool,
    llm_client: Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    llm_config: agent_sdk::config::LlmConfig,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    subagent_registry: Arc<agent_sdk::SubAgentRegistry>,
    background_tx: Option<tokio::sync::mpsc::UnboundedSender<agent_sdk::agent::agent_loop::BackgroundResult>>,
    tool_filter: Option<&[String]>,
    mcp_tools: &[Arc<dyn Tool>],
    paths: &AgentPaths,
    memory_store: Option<Arc<MemoryStore>>,
    cli_agent_id: AgentId,
) -> ToolRegistry {
    let filter = tool_filter
        .map(|names| ToolFilter::allow_only(names.iter().cloned()))
        .unwrap_or_default();
    let command_policy = if allow_all {
        CommandToolPolicy::Unrestricted
    } else {
        CommandToolPolicy::Unrestricted
    };

    let mut builder = DefaultToolsetBuilder::with_filter(filter)
        .add_core_tools(
            work_dir.to_path_buf(),
            work_dir.to_path_buf(),
            command_policy,
        )
        .add_lsp_tools(paths.project_lsp_config_path(), work_dir.to_path_buf())
        .add_team_tool(TeamToolConfig {
            work_dir: work_dir.to_path_buf(),
            source_root: work_dir.to_path_buf(),
            llm_client: llm_client.clone(),
            llm_config: llm_config.clone(),
            event_tx: event_tx.clone(),
            background_tx: background_tx.clone(),
        })
        .add_subagent_tool(SubAgentToolConfig {
            work_dir: work_dir.to_path_buf(),
            source_root: work_dir.to_path_buf(),
            llm_client,
            event_tx,
            registry: subagent_registry,
            background_tx,
        });

    // Wire memory tools into the CLI agent
    if let Some(store) = memory_store {
        builder = builder.add_memory_tools(store, cli_agent_id);
    }

    builder = builder.add_custom_tool(Arc::new(UpdateTaskListTool { tasks }));

    for tool in mcp_tools {
        builder = builder.add_custom_tool(tool.clone());
    }

    builder.build()
}

// ─── Input ───────────────────────────────────────────────────────────────────

/// Read input with multi-line support (trailing `\` continues).
fn read_input() -> io::Result<String> {
    read_input_buffered()
}

fn read_input_buffered() -> io::Result<String> {
    let stdin = io::stdin();
    let mut full = String::new();

    loop {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;

        if line.is_empty() {
            // EOF
            return Ok(full);
        }

        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');

        if trimmed.ends_with('\\') {
            full.push_str(&trimmed[..trimmed.len() - 1]);
            full.push('\n');
            eprint!("  {} ", style("…").dim());
            io::stderr().flush()?;
        } else {
            full.push_str(trimmed);
            break;
        }
    }

    Ok(full)
}

// ─── ReAct turn ──────────────────────────────────────────────────────────────

struct TurnStats {
    tokens: u64,
    tool_calls: usize,
    iterations: usize,
    duration: std::time::Duration,
}

async fn run_turn(
    messages: &mut Vec<ChatMessage>,
    user_input: &str,
    llm_client: &Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    llm_config: &agent_sdk::config::LlmConfig,
    work_dir: &Path,
    max_iterations: usize,
    allow_all: bool,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    interrupt: Arc<AtomicBool>,
    subagent_registry: Arc<agent_sdk::SubAgentRegistry>,
    json_mode: bool,
    tool_filter: Option<&[String]>,
    mcp_tools: &[Arc<dyn Tool>],
    paths: &AgentPaths,
    memory_store: Option<Arc<MemoryStore>>,
    cli_agent_id: AgentId,
) -> anyhow::Result<TurnStats> {
    // Create background result channel — tools send completed background results
    // here, and we drain them before each LLM call to inject into conversation.
    let (background_tx, mut background_rx) =
        tokio::sync::mpsc::unbounded_channel::<agent_sdk::agent::agent_loop::BackgroundResult>();

    let tools = build_tools(
        work_dir,
        allow_all,
        llm_client.clone(),
        llm_config.clone(),
        event_tx,
        tasks.clone(),
        subagent_registry,
        Some(background_tx),
        tool_filter,
        mcp_tools,
        paths,
        memory_store,
        cli_agent_id,
    );
    let tool_defs = tools.definitions();
    let started = Instant::now();

    if json_mode {
        emit_ndjson(&NdjsonEvent::Started {
            tools: tool_defs.iter().map(|t| t.name.clone()).collect(),
        });
    }

    messages.push(ChatMessage::user(user_input));

    let mut total_tokens = 0u64;
    let mut tool_calls_count = 0usize;

    for iteration in 0..max_iterations {
        // Drain any completed background agent results and inject them
        // into the conversation so the LLM can reference them.
        while let Ok(result) = background_rx.try_recv() {
            let kind_label = match &result.kind {
                agent_sdk::agent::agent_loop::BackgroundResultKind::SubAgent => "subagent",
                agent_sdk::agent::agent_loop::BackgroundResultKind::AgentTeam => "agent team",
                // Compaction summaries are only produced by `AgentLoop`'s
                // own background consolidation path. The CLI runs its own
                // loop and does not currently dispatch summaries, so if one
                // somehow lands here we simply ignore it.
                agent_sdk::agent::agent_loop::BackgroundResultKind::CompactionSummary { .. }
                | agent_sdk::agent::agent_loop::BackgroundResultKind::SubAgentPartial => {
                    continue;
                }
            };
            let notification = format!(
                "[Background {} '{}' completed — {} tokens]\n\n{}",
                kind_label, result.name, result.tokens_used, result.content,
            );
            messages.push(ChatMessage::user(notification));
        }
        // Check for interrupt before each LLM call
        if interrupt.load(Ordering::Relaxed) {
            interrupt.store(false, Ordering::Relaxed);
            if !json_mode {
                eprintln!("\n  {}", style("Interrupted").yellow());
            }
            return Ok(TurnStats {
                tokens: total_tokens,
                tool_calls: tool_calls_count,
                iterations: iteration + 1,
                duration: started.elapsed(),
            });
        }

        let mut spinner = if json_mode { None } else { Some(create_spinner("Thinking…")) };

        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<StreamDelta>();

        // Signal so the emit task can tell us to clear the spinner
        // before it starts writing streamed text to stderr.
        let (streaming_started_tx, streaming_started_rx) = tokio::sync::oneshot::channel::<()>();

        let is_json = json_mode;
        let emit_handle = tokio::spawn(async move {
            let mut streaming_started = false;
            let mut started_tx = Some(streaming_started_tx);
            while let Some(delta) = delta_rx.recv().await {
                match delta {
                    StreamDelta::Text(text) => {
                        if is_json {
                            emit_ndjson(&NdjsonEvent::TextDelta { content: text });
                        } else {
                            if !streaming_started {
                                streaming_started = true;
                                // Signal the main task to clear the spinner
                                if let Some(tx) = started_tx.take() {
                                    let _ = tx.send(());
                                }
                                // Small yield to let the main task clear the spinner
                                tokio::task::yield_now().await;
                            }
                            eprint!("{}", text);
                            let _ = io::stderr().flush();
                        }
                    }
                    StreamDelta::Thinking(_) => {
                        // Thinking text is handled after the response completes
                    }
                }
            }
            streaming_started
        });

        // Clone messages so chat_stream doesn't borrow `messages` across
        // the rest of the loop iteration where we need to push into it.
        let messages_snapshot = messages.clone();
        let llm_fut = llm_client.chat_stream(&messages_snapshot, &tool_defs, delta_tx);

        // Wait for LLM completion, but clear the spinner as soon as
        // the first text delta arrives so it doesn't overwrite streamed text.
        tokio::pin!(llm_fut);
        let result = tokio::select! {
            biased;
            _ = streaming_started_rx => {
                // First text delta arrived — clear spinner immediately
                if let Some(s) = spinner.take() {
                    s.finish_and_clear();
                }
                // Continue waiting for LLM to finish
                llm_fut.await
            }
            res = &mut llm_fut => res,
        };

        let streamed = emit_handle.await.unwrap_or(false);
        if let Some(s) = spinner {
            s.finish_and_clear();
        }

        // Check interrupt after call returns
        if interrupt.load(Ordering::Relaxed) {
            interrupt.store(false, Ordering::Relaxed);
            if !json_mode {
                eprintln!("  {}", style("Interrupted").yellow());
            }
            return Ok(TurnStats {
                tokens: total_tokens,
                tool_calls: tool_calls_count,
                iterations: iteration + 1,
                duration: started.elapsed(),
            });
        }

        let (response, tokens) = result?;
        total_tokens += tokens;

        match response {
            ChatMessage::Assistant {
                ref content,
                ref tool_calls,
            } if !tool_calls.is_empty() => {
                // Show thinking text
                if let Some(text) = content {
                    if !text.is_empty() {
                        if json_mode {
                            emit_ndjson(&NdjsonEvent::Thinking {
                                content: text.clone(),
                                iteration,
                            });
                        } else {
                            // Clear the spinner line if streaming was active
                            if streamed {
                                eprint!("\r\x1b[K");
                            }
                            // Show thinking in a subtle way like Claude Code
                            let thinking_lines: Vec<&str> = text.lines().collect();
                            let show_lines = thinking_lines.len().min(3);
                            for line in &thinking_lines[..show_lines] {
                                eprintln!(
                                    "  {} {}",
                                    style("│").dim(),
                                    style(truncate(line, 100)).dim().italic()
                                );
                            }
                            if thinking_lines.len() > show_lines {
                                eprintln!(
                                    "  {} {}",
                                    style("│").dim(),
                                    style(format!("… +{} more lines", thinking_lines.len() - show_lines)).dim()
                                );
                            }
                        }
                    }
                }

                messages.push(response.clone());

                // Show iteration indicator for multi-iteration turns
                if !json_mode && iteration > 0 {
                    eprintln!(
                        "  {}",
                        style(format!("[iter {}]", iteration + 1)).dim()
                    );
                }

                let tc_count = tool_calls.len();
                for (tc_idx, tc) in tool_calls.iter().enumerate() {
                    let is_last_tc = tc_idx == tc_count - 1;

                    if json_mode {
                        emit_ndjson(&NdjsonEvent::ToolCall {
                            tool_call_id: tc.id.clone(),
                            tool_name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                            iteration,
                        });
                    } else {
                        let label = format_tool_label(&tc.function.name, &tc.function.arguments);
                        let connector = if is_last_tc { "└" } else { "├" };
                        eprintln!("  {} {}", style(connector).cyan(), label);

                        if tc.function.name == "spawn_agent_team" {
                            print_team_plan(&tc.function.arguments);
                        }

                        // Show write_file content preview with header
                        if tc.function.name == "write_file" {
                            let args: serde_json::Value =
                                serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                            if let Some(content) = args["content"].as_str() {
                                let path = args["path"].as_str().unwrap_or("?");
                                let lang = lang_hint(path);
                                let lines: Vec<&str> = content.lines().collect();
                                eprintln!(
                                    "  {}  {}",
                                    style("│").dim(),
                                    style(format!("── {} ({} lines{}) ──", path, lines.len(), lang)).dim()
                                );
                                let show = lines.len().min(8);
                                for line in &lines[..show] {
                                    eprintln!(
                                        "  {}  {}",
                                        style("│").dim(),
                                        style(truncate(line, 100)).dim()
                                    );
                                }
                                if lines.len() > show {
                                    eprintln!(
                                        "  {}  {}",
                                        style("│").dim(),
                                        style(format!("… +{} more lines", lines.len() - show)).dim()
                                    );
                                }
                            }
                        }

                        // Show edit_file diff preview with header
                        if tc.function.name == "edit_file" {
                            let args: serde_json::Value =
                                serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                            let old = args["old_string"].as_str().unwrap_or("");
                            let new = args["new_string"].as_str().unwrap_or("");
                            if !old.is_empty() || !new.is_empty() {
                                let path = args["path"].as_str().unwrap_or("?");
                                eprintln!(
                                    "  {}  {}",
                                    style("│").dim(),
                                    style(format!("@@ {} @@", path)).cyan().dim()
                                );
                                let old_lines: Vec<&str> = old.lines().collect();
                                let new_lines: Vec<&str> = new.lines().collect();
                                let max_preview = 6;
                                let show_old = old_lines.len().min(max_preview);
                                let show_new = new_lines.len().min(max_preview);
                                for line in &old_lines[..show_old] {
                                    eprintln!(
                                        "  {}  {}",
                                        style("│").dim(),
                                        style(format!("- {}", truncate(line, 90))).red().dim()
                                    );
                                }
                                if old_lines.len() > show_old {
                                    eprintln!(
                                        "  {}  {}",
                                        style("│").dim(),
                                        style(format!("  … +{} more", old_lines.len() - show_old)).dim()
                                    );
                                }
                                for line in &new_lines[..show_new] {
                                    eprintln!(
                                        "  {}  {}",
                                        style("│").dim(),
                                        style(format!("+ {}", truncate(line, 90))).green().dim()
                                    );
                                }
                                if new_lines.len() > show_new {
                                    eprintln!(
                                        "  {}  {}",
                                        style("│").dim(),
                                        style(format!("  … +{} more", new_lines.len() - show_new)).dim()
                                    );
                                }
                            }
                        }
                    }

                    let args: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or_default();

                    let tool_start = Instant::now();
                    let result = tools.execute(&tc.function.name, args).await;
                    let tool_elapsed = tool_start.elapsed();

                    let result_content = match &result {
                        Ok(val) => {
                            let full = serde_json::to_string(val).unwrap_or_default();
                            truncate_tool_result(&full)
                        }
                        Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                    };

                    if json_mode {
                        emit_ndjson(&NdjsonEvent::ToolResult {
                            tool_call_id: tc.id.clone(),
                            tool_name: tc.function.name.clone(),
                            content: result_content.clone(),
                            iteration,
                        });
                    } else {
                        let preview = format_result_preview(&tc.function.name, &result_content);
                        let timing = if tool_elapsed.as_secs_f64() > 1.0 {
                            format!(" ({})", format_duration(tool_elapsed))
                        } else {
                            String::new()
                        };
                        eprintln!("    {}{}", style(&preview).dim(), style(&timing).dim());

                        if tc.function.name == "spawn_agent_team" {
                            print_team_result_summary(&result_content);
                        } else if tc.function.name == "update_task_list" {
                            let current = tasks.lock().expect("task list mutex poisoned").clone();
                            print_task_list(&current);
                        }
                    }

                    messages.push(ChatMessage::tool_result(&tc.id, &result_content));
                    tool_calls_count += 1;
                }
            }

            ChatMessage::Assistant { ref content, .. } => {
                let answer = content.clone().unwrap_or_default();
                messages.push(response);

                if json_mode {
                    emit_ndjson(&NdjsonEvent::Completed {
                        final_content: answer,
                        tokens_used: total_tokens,
                        iterations: iteration + 1,
                        tool_calls: tool_calls_count,
                    });
                } else if streamed {
                    // Text was already streamed to stderr — just add newlines
                    eprintln!();
                    eprintln!();
                } else {
                    // Fallback: print the full answer
                    eprintln!();
                    for line in answer.lines() {
                        eprintln!("{}", line);
                    }
                    eprintln!();
                }

                return Ok(TurnStats {
                    tokens: total_tokens,
                    tool_calls: tool_calls_count,
                    iterations: iteration + 1,
                    duration: started.elapsed(),
                });
            }

            other => {
                let text = other.text_content().unwrap_or("").to_string();
                messages.push(other);

                if json_mode {
                    emit_ndjson(&NdjsonEvent::Completed {
                        final_content: text,
                        tokens_used: total_tokens,
                        iterations: iteration + 1,
                        tool_calls: tool_calls_count,
                    });
                } else if !streamed {
                    eprintln!();
                    eprintln!("{}", text);
                    eprintln!();
                } else {
                    eprintln!();
                    eprintln!();
                }

                return Ok(TurnStats {
                    tokens: total_tokens,
                    tool_calls: tool_calls_count,
                    iterations: iteration + 1,
                    duration: started.elapsed(),
                });
            }
        }
    }

    if json_mode {
        emit_ndjson(&NdjsonEvent::Failed {
            error: format!("max iterations ({}) reached", max_iterations),
        });
    } else {
        eprintln!();
        eprintln!(
            "  {} Max iterations ({}) reached",
            style("⚠").yellow(),
            max_iterations,
        );
    }
    Ok(TurnStats {
        tokens: total_tokens,
        tool_calls: tool_calls_count,
        iterations: max_iterations,
        duration: started.elapsed(),
    })
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("agent_sdk=warn".parse().unwrap()),
        )
        .with_target(false)
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let work_dir = match std::fs::canonicalize(&cli.dir) {
        Ok(p) => p,
        Err(e) => {
            if cli.json {
                emit_ndjson(&NdjsonEvent::Failed {
                    error: format!("Working directory '{}' not found: {}", cli.dir.display(), e),
                });
                return Ok(());
            } else {
                return Err(e.into());
            }
        }
    };

    // ── Provider detection ──
    let provider = cli
        .provider
        .as_deref()
        .and_then(LlmProvider::parse)
        .unwrap_or_else(LlmProvider::detect);

    // ── Model detection ──
    let model = cli.model.unwrap_or_else(|| {
        LlmConfig {
            provider: provider.clone(),
            model: String::new(),
            ..LlmConfig::default()
        }
        .resolve_model()
    });

    let llm_config = LlmConfig {
        provider,
        model: model.clone(),
        max_tokens: cli.max_tokens,
        ..LlmConfig::default()
    };

    let llm_client = agent_sdk::llm::create_client(&llm_config)?;

    // ── Event channel for team monitoring ──
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let event_json_mode = cli.json;

    tokio::spawn(async move {
        // Color palette for agents — cycle through these
        const COLORS: &[console::Color] = &[
            console::Color::Magenta,
            console::Color::Blue,
            console::Color::Yellow,
            console::Color::Green,
            console::Color::Cyan,
            console::Color::Red,
        ];
        let mut color_map = std::collections::HashMap::<String, console::Color>::new();
        let mut next_color = 0usize;

        let agent_color = |name: &str, map: &mut std::collections::HashMap<String, console::Color>, next: &mut usize| -> console::Color {
            *map.entry(name.to_string()).or_insert_with(|| {
                let c = COLORS[*next % COLORS.len()];
                *next += 1;
                c
            })
        };

        // Render agent name as a fixed-width tag with a left border
        let name_tag = |name: &str, color: console::Color| -> String {
            let display = if name.len() > 16 { &name[..floor_char_boundary(name, 16)] } else { name };
            format!(
                "  {} {}",
                style("│").fg(color),
                style(format!("{:<16}", display)).fg(color).bold(),
            )
        };

        while let Some(event) = event_rx.recv().await {
            // In JSON mode, emit structured NDJSON for key events
            if event_json_mode {
                match &event {
                    AgentEvent::TeamSpawned { teammate_count } => {
                        emit_ndjson(&NdjsonEvent::TeamSpawned { teammate_count: *teammate_count });
                    }
                    AgentEvent::SubAgentSpawned { name, description, .. } => {
                        emit_ndjson(&NdjsonEvent::SubagentSpawned { name: name.clone(), description: description.clone() });
                    }
                    AgentEvent::SubAgentProgress { name, iteration, max_turns, current_tool, tokens_so_far, .. } => {
                        emit_ndjson(&NdjsonEvent::SubagentProgress { name: name.clone(), iteration: *iteration, max_turns: *max_turns, current_tool: current_tool.clone(), tokens_so_far: *tokens_so_far });
                    }
                    AgentEvent::SubAgentCompleted { name, tokens_used, iterations, tool_calls, .. } => {
                        emit_ndjson(&NdjsonEvent::SubagentCompleted { name: name.clone(), tokens_used: *tokens_used, iterations: *iterations, tool_calls: *tool_calls });
                    }
                    AgentEvent::SubAgentFailed { name, error, .. } => {
                        emit_ndjson(&NdjsonEvent::SubagentFailed { name: name.clone(), error: error.clone() });
                    }
                    AgentEvent::TaskStarted { name, title, .. } => {
                        emit_ndjson(&NdjsonEvent::TaskStarted { name: name.clone(), title: title.clone() });
                    }
                    AgentEvent::TaskCompleted { name, tokens_used, .. } => {
                        emit_ndjson(&NdjsonEvent::TaskCompleted { name: name.clone(), title: String::new(), tokens_used: *tokens_used });
                    }
                    AgentEvent::TaskFailed { name, error, .. } => {
                        emit_ndjson(&NdjsonEvent::TaskFailed { name: name.clone(), title: String::new(), error: error.clone() });
                    }
                    _ => {}
                }
                // In JSON mode, skip the stderr rendering
                continue;
            }
            match event {
                // ── Team lifecycle ──────────────────────────────────────
                AgentEvent::TeamSpawned { teammate_count } => {
                    eprintln!();
                    eprintln!(
                        "  {} {}",
                        style("⎿").cyan(),
                        style(format!("Agent Team ({} teammates)", teammate_count)).cyan().bold(),
                    );
                }
                AgentEvent::TeammateSpawned { ref name, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {}",
                        style("⎿").fg(c),
                        style(name).fg(c).bold(),
                    );
                }

                // ── Task lifecycle ──────────────────────────────────────
                AgentEvent::TaskStarted { ref name, ref title, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!();
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("▸").fg(c),
                        style(title).white(),
                    );
                }
                AgentEvent::Thinking { ref name, ref content, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{}   {}",
                        name_tag(name, c),
                        style(truncate(content, 80)).dim().italic(),
                    );
                }
                AgentEvent::ToolCall {
                    ref name,
                    ref tool_name,
                    ref arguments,
                    ..
                } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    let label = format_tool_label(tool_name, arguments);
                    eprintln!(
                        "{}   {} {}",
                        name_tag(name, c),
                        style("⎿").fg(c),
                        label,
                    );
                }
                AgentEvent::ToolResult {
                    ref name,
                    ref tool_name,
                    ref result_preview,
                    ..
                } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    let preview = format_result_preview(tool_name, result_preview);
                    eprintln!(
                        "{}     {}",
                        name_tag(name, c),
                        style(&preview).dim(),
                    );
                }
                AgentEvent::TaskCompleted {
                    ref name,
                    tokens_used,
                    tool_calls,
                    ..
                } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {} · {} tool {}",
                        name_tag(name, c),
                        style("✓").green(),
                        style(format!("{} tokens", format_token_count(tokens_used))).dim(),
                        tool_calls,
                        if tool_calls == 1 { "use" } else { "uses" },
                    );
                }
                AgentEvent::TaskFailed { ref name, ref error, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("✗").red(),
                        style(truncate(error, 80)).red(),
                    );
                }

                // ── Plan mode ───────────────────────────────────────────
                AgentEvent::PlanSubmitted { ref name, ref plan_preview, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("📋 plan submitted").yellow(),
                        style(truncate(plan_preview, 60)).dim(),
                    );
                }
                AgentEvent::PlanApproved { ref name, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {}",
                        name_tag(name, c),
                        style("✓ plan approved").green(),
                    );
                }
                AgentEvent::PlanRejected { ref name, ref feedback, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("✗ plan rejected").yellow(),
                        style(truncate(feedback, 60)).dim(),
                    );
                }

                // ── Idle / shutdown ──────────────────────────────────────
                AgentEvent::TeammateIdle { ref name, tasks_completed, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("…").dim(),
                        style(format!("idle ({} tasks done)", tasks_completed)).dim(),
                    );
                }
                AgentEvent::AgentShutdown { ref name, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {}",
                        name_tag(name, c),
                        style("done").dim(),
                    );
                }

                // ── Subagent lifecycle ───────────────────────────────────
                AgentEvent::SubAgentSpawned { ref name, ref description, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    let desc = if description.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", truncate(description, 60))
                    };
                    eprintln!();
                    eprintln!(
                        "  {} {} {}{}",
                        style("⎿").cyan(),
                        style("Subagent").bold(),
                        style(name).fg(c).bold(),
                        style(desc).dim(),
                    );
                }
                AgentEvent::SubAgentCompleted {
                    ref name,
                    tokens_used,
                    tool_calls,
                    ref final_content,
                    ..
                } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {} · {} tool {}",
                        name_tag(name, c),
                        style("✓").green(),
                        style(format!("{} tokens", format_token_count(tokens_used))).dim(),
                        tool_calls,
                        if tool_calls == 1 { "use" } else { "uses" },
                    );
                    // Show a brief preview of the result
                    if !final_content.is_empty() {
                        let lines: Vec<&str> = final_content.lines().take(3).collect();
                        for line in &lines {
                            eprintln!(
                                "{}   {}",
                                name_tag(name, c),
                                style(truncate(line, 80)).dim(),
                            );
                        }
                        let total_lines = final_content.lines().count();
                        if total_lines > 3 {
                            eprintln!(
                                "{}   {}",
                                name_tag(name, c),
                                style(format!("… +{} more lines", total_lines - 3)).dim(),
                            );
                        }
                    }
                }
                AgentEvent::SubAgentFailed { ref name, ref error, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("✗").red(),
                        style(truncate(error, 80)).red(),
                    );
                }
                AgentEvent::SubAgentUpdate { ref name, ref content, is_final, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    let marker = if is_final { "✔" } else { "…" };
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style(marker).fg(c),
                        style(truncate(content, 80)).dim(),
                    );
                }

                // ── Subagent progress ────────────────────────────────────
                AgentEvent::SubAgentProgress {
                    ref name,
                    iteration,
                    max_turns,
                    ref current_tool,
                    tokens_so_far,
                    ..
                } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    let tool_info = current_tool
                        .as_deref()
                        .map(|t| format!(" · {}", t))
                        .unwrap_or_default();
                    eprint!(
                        "\r{}   {} {}/{}{} · {}",
                        name_tag(name, c),
                        style("◐").fg(c),
                        iteration + 1,
                        max_turns,
                        style(&tool_info).dim(),
                        style(format!("{} tokens", format_token_count(tokens_so_far))).dim(),
                    );
                    let _ = io::stderr().flush();
                }

                // ── Dynamic task creation ───────────────────────────────
                AgentEvent::TaskCreated { ref name, ref title, .. } => {
                    let c = agent_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{} {} {}",
                        name_tag(name, c),
                        style("+").fg(c),
                        style(format!("new task: \"{}\"", truncate(title, 60))).dim(),
                    );
                }

                // ── Memory compaction ───────────────────────────────────
                AgentEvent::MemoryCompacted {
                    ref strategy,
                    messages_before,
                    messages_after,
                    tokens_saved,
                    ..
                } => {
                    eprintln!(
                        "  {} {} {}→{} messages ({} saved, {})",
                        style("↻").dim(),
                        style("compacted:").dim(),
                        messages_before,
                        messages_after,
                        format_token_count(tokens_saved),
                        style(strategy).dim(),
                    );
                }

                // ── Communication ───────────────────────────────────────
                AgentEvent::TeammateMessage { ref from_name, ref content_preview, .. } => {
                    let c = agent_color(from_name, &mut color_map, &mut next_color);
                    eprintln!(
                        "{}   {} {}",
                        name_tag(from_name, c),
                        style("→").fg(c),
                        style(truncate(content_preview, 60)).dim(),
                    );
                }

                _ => {}
            }
        }
    });

    // ── System prompt ──
    let mut system_prompt = cli
        .system
        .unwrap_or_else(|| agent_sdk::prompts::cli_system_prompt(&work_dir));

    // ── Paths and memory store (shared by one-shot and REPL) ──
    let paths = AgentPaths::for_work_dir(&work_dir)?;

    // Create memory store and inject index into system prompt
    let memory_store: Option<Arc<MemoryStore>> = {
        let memory_dir = paths.project_memory_dir();
        match MemoryStore::new(memory_dir) {
            Ok(store) => {
                if let Ok(Some(index)) = store.load_index() {
                    system_prompt.push_str(&agent_sdk::prompts::memory_context_section(&index));
                }
                Some(Arc::new(store))
            }
            Err(_) => None,
        }
    };

    let mut session_path = cli
        .session
        .unwrap_or_else(|| default_session_path(&work_dir));

    // Agent ID for the CLI session
    let cli_agent_id: AgentId = Uuid::new_v4();

    // ── Subagent registry with built-in definitions ──
    let subagent_registry = {
        let mut reg = agent_sdk::SubAgentRegistry::new();
        for def in agent_sdk::agent::subagent::builtin_subagents() {
            reg.register(def);
        }
        Arc::new(reg)
    };

    // ── Ctrl+C handling ──
    let interrupt = Arc::new(AtomicBool::new(false));
    {
        let interrupt = interrupt.clone();
        ctrlc_handler(interrupt);
    }

    // ── MCP servers ──
    let mcp_tools: Vec<Arc<dyn Tool>> = load_mcp_tools(&work_dir, cli.json).await;

    // ── One-shot mode ──
    let one_shot_prompt = if let Some(ref path) = cli.prompt_file {
        Some(std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("Failed to read prompt file {}: {}", path.display(), e)
        })?)
    } else if !cli.prompt.is_empty() {
        Some(cli.prompt.join(" "))
    } else {
        None
    };

    if let Some(prompt) = one_shot_prompt {
        let mut messages = vec![ChatMessage::system(&system_prompt)];
        let tasks = Arc::new(Mutex::new(Vec::<CliTask>::new()));
        let tool_filter = cli.tools.as_deref();

        let result = run_turn(
            &mut messages,
            &prompt,
            &llm_client,
            &llm_config,
            &work_dir,
            cli.max_iterations,
            cli.allow_all_commands,
            Some(event_tx),
            tasks,
            interrupt,
            subagent_registry,
            cli.json,
            tool_filter,
            &mcp_tools,
            &paths,
            memory_store,
            cli_agent_id,
        )
        .await;

        match result {
            Ok(stats) => {
                if !cli.json {
                    print_usage(&stats);
                }
            }
            Err(e) => {
                if cli.json {
                    emit_ndjson(&NdjsonEvent::Failed {
                        error: e.to_string(),
                    });
                } else {
                    return Err(e);
                }
            }
        }
        return Ok(());
    }

    // ── Interactive REPL ──
    // Count tools: 13 built-in + LSP (up to 3) + memory (up to 5) + MCP
    let tool_count = 13 + mcp_tools.len() + if memory_store.is_some() { 5 } else { 0 };
    let provider_name = match llm_config.provider {
        LlmProvider::Claude => "claude",
        LlmProvider::OpenAi => "openai",
    };
    print_welcome(&model, provider_name, &work_dir, tool_count, mcp_tools.len(), None);
    let slash_registry = SlashCommandRegistry::builtin();

    let tasks = Arc::new(Mutex::new(Vec::<CliTask>::new()));

    let mut agent_mode = AgentMode::Normal;
    let mut ultra_plan_state: Option<UltraPlanState> = None;

    // ── Cache state ──
    let file_cache = Arc::new(agent_sdk::FileStateCache::new());
    let cache_state = {
        let stats_path = paths.project_state_dir().join("stats.jsonl");
        Arc::new(agent_sdk::cli::cache_commands::CacheState {
            file_cache: file_cache.clone(),
            stats_path,
        })
    };

    let mut messages = match load_session(&session_path, &system_prompt) {
        Some(session) => {
            let n = session.messages.len();
            agent_mode = session.mode;
            ultra_plan_state = session.ultra_plan;
            {
                let mut current = tasks.lock().expect("task list mutex poisoned");
                *current = session.tasks;
            }
            eprintln!(
                "   {} Session restored ({} messages)",
                style("↻").green(),
                style(n).dim(),
            );
            if agent_mode == AgentMode::Plan {
                eprintln!(
                    "   {} {}",
                    style("mode:").dim(),
                    style("plan (read-only)").yellow(),
                );
            }
            if let Some(ref state) = ultra_plan_state {
                eprintln!(
                    "   {} {}",
                    style("mode:").dim(),
                    style(format!("ultraplan ({})", state.phase)).yellow(),
                );
            }
            let current = tasks.lock().expect("task list mutex poisoned").clone();
            if !current.is_empty() {
                eprintln!();
                print_task_list(&current);
            }
            eprintln!();
            session.messages
        }
        None => {
            vec![ChatMessage::system(&system_prompt)]
        }
    };

    let mut session_tokens = 0u64;
    let mut session_tool_calls = 0usize;
    let mut session_turns = 0usize;

    // ── Session PID tracking ──
    let session_id = agent_sdk::cli::SessionManager::session_id_from_path(&session_path);
    if let Some(sessions_dir) = session_path.parent() {
        let _ = agent_sdk::cli::SessionManager::register_pid(sessions_dir, &session_id);

        // Report any interrupted sessions from previous runs
        let interrupted = agent_sdk::cli::SessionManager::detect_interrupted(sessions_dir);
        if !interrupted.is_empty() {
            eprintln!(
                "   {} {} interrupted session(s) detected (use /sessions to view)",
                style("!").yellow(),
                interrupted.len(),
            );
        }
    }

    // Derive project name from the work directory for the prompt
    let project_name = work_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "agent".to_string());

    loop {
        let mode_indicator = if ultra_plan_state.is_some() {
            let phase = &ultra_plan_state.as_ref().unwrap().phase;
            format!(" {}", style(format!("[{}]", phase)).yellow())
        } else {
            match agent_mode {
                AgentMode::Plan => format!(" {}", style("[plan]").yellow()),
                AgentMode::Normal => String::new(),
            }
        };
        eprint!("{}{} {} ", style(&project_name).dim(), mode_indicator, style(">").cyan().bold());
        io::stderr().flush()?;

        let input = read_input()?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        // ── Slash commands ──
        if input.starts_with('/') {
            let mut ctx = CommandContext {
                messages: &mut messages,
                tasks: tasks.clone(),
                paths: &paths,
                session_path: session_path.clone(),
                system_prompt: &system_prompt,
                total_tokens: &mut session_tokens,
                tool_calls: &mut session_tool_calls,
                turns: &mut session_turns,
                agent_mode: &mut agent_mode,
                cache_state: Some(cache_state.clone()),
                ultra_plan: &mut ultra_plan_state,
            };

            match slash_registry.dispatch(&input, &mut ctx).await {
                Ok(Some(CommandOutcome::Quit)) => break,
                Ok(Some(CommandOutcome::Clear)) => {
                    eprintln!(
                        "  {} {}",
                        style("✓").green(),
                        style("Conversation cleared").dim(),
                    );
                    eprintln!();
                    continue;
                }
                Ok(Some(CommandOutcome::Compact)) => continue,
                Ok(Some(CommandOutcome::Output(text))) => {
                    eprintln!("{}", text);
                    continue;
                }
                Ok(Some(CommandOutcome::Continue)) => continue,
                Ok(Some(CommandOutcome::SessionSwitch { path })) => {
                    // Clean up PID for old session
                    let old_id = agent_sdk::cli::SessionManager::session_id_from_path(&session_path);
                    if let Some(dir) = session_path.parent() {
                        agent_sdk::cli::SessionManager::cleanup_pid(dir, &old_id);
                    }

                    // Switch to the target session
                    messages = match load_session(&path, &system_prompt) {
                        Some(session) => {
                            let mut current = tasks.lock().expect("task list mutex poisoned");
                            *current = session.tasks;
                            session.messages
                        }
                        None => vec![ChatMessage::system(&system_prompt)],
                    };
                    session_path = path;
                    session_tokens = 0;
                    session_tool_calls = 0;
                    session_turns = 0;

                    // Register PID for new session
                    let new_id = agent_sdk::cli::SessionManager::session_id_from_path(&session_path);
                    if let Some(dir) = session_path.parent() {
                        let _ = agent_sdk::cli::SessionManager::register_pid(dir, &new_id);
                    }

                    eprintln!(
                        "  {} Session switched ({} messages)",
                        style("ok").green(),
                        style(messages.len()).dim(),
                    );
                    eprintln!();
                    continue;
                }
                Ok(None) => {
                    // Not a slash command — fall through to regular prompt.
                }
                Err(e) => {
                    eprintln!(
                        "  {} {}  (type {} for help)",
                        style("?").yellow(),
                        style(e.to_string()).white(),
                        style("/help").cyan(),
                    );
                    eprintln!();
                    continue;
                }
            }
        }

        // Plan mode: inject system prompt suffix and filter tools
        if agent_mode == AgentMode::Plan {
            if let Some(ChatMessage::System { content }) = messages.first_mut() {
                if !content.contains("PLAN MODE ACTIVE") {
                    content.push_str(agent_sdk::plan_mode_system_suffix());
                }
            }
        } else {
            // Remove plan mode suffix if present (user exited plan mode)
            if let Some(ChatMessage::System { content }) = messages.first_mut() {
                if let Some(idx) = content.find("\n\n# PLAN MODE ACTIVE") {
                    content.truncate(idx);
                }
            }
        }

        // ── UltraPlan: apply phase-specific system prompt suffix ──
        if let Some(ChatMessage::System { content }) = messages.first_mut() {
            // Strip any existing ultraplan suffix first
            if let Some(idx) = content.find("\n# ULTRAPLAN:") {
                content.truncate(idx);
            }
            // Append the current phase suffix if active
            if let Some(ref state) = ultra_plan_state {
                content.push_str(phase_system_suffix(&state.phase));
            }
        }

        // ── Compute effective tool filter (plan mode or ultraplan) ──
        let plan_filter: Option<Vec<String>> = if ultra_plan_state.is_some() {
            // UltraPlan takes priority over plan mode
            let tools = allowed_tools_for_phase(&ultra_plan_state.as_ref().unwrap().phase);
            if tools.is_empty() {
                None // Implement phase = all tools
            } else {
                Some(tools.iter().map(|s| s.to_string()).collect())
            }
        } else if agent_mode == AgentMode::Plan {
            Some(PLAN_MODE_READONLY_TOOLS.iter().map(|s| s.to_string()).collect())
        } else {
            None
        };
        let plan_filter_refs = plan_filter.as_deref();

        let stats = run_turn(
            &mut messages,
            &input,
            &llm_client,
            &llm_config,
            &work_dir,
            cli.max_iterations,
            cli.allow_all_commands,
            Some(event_tx.clone()),
            tasks.clone(),
            interrupt.clone(),
            subagent_registry.clone(),
            false,
            plan_filter_refs,
            &mcp_tools,
            &paths,
            memory_store.clone(),
            cli_agent_id,
        )
        .await?;

        session_tokens += stats.tokens;
        session_tool_calls += stats.tool_calls;
        session_turns += 1;

        print_usage(&stats);

        if let Err(e) = agent_sdk::cli::session::save_session_full(
            &session_path,
            &messages,
            &tasks.lock().expect("task list mutex poisoned"),
            ultra_plan_state.as_ref(),
        ) {
            eprintln!("  {} session save: {}", style("⚠").yellow(), e);
        }
    }

    // ── Cleanup PID tracking ──
    let final_session_id = agent_sdk::cli::SessionManager::session_id_from_path(&session_path);
    if let Some(sessions_dir) = session_path.parent() {
        agent_sdk::cli::SessionManager::cleanup_pid(sessions_dir, &final_session_id);
    }

    eprintln!();
    eprintln!(
        "  {} {} · {} · {} tool {}",
        style("Session:").dim(),
        style(format!("{} turns", session_turns)).dim(),
        style(format!("{} tokens", format_token_count(session_tokens))).dim(),
        style(session_tool_calls).dim(),
        if session_tool_calls == 1 { "use" } else { "uses" },
    );
    eprintln!();
    Ok(())
}

fn format_duration(d: std::time::Duration) -> String {
    if d.as_secs() >= 60 {
        format!("{}m{:.0}s", d.as_secs() / 60, d.as_secs() % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn print_usage(stats: &TurnStats) {
    let duration = format_duration(stats.duration);

    // Compact one-line stats like Claude Code
    let mut parts: Vec<String> = Vec::new();
    if stats.iterations > 1 {
        parts.push(format!("{} iterations", stats.iterations));
    }
    parts.push(format!("{} tokens", format_token_count(stats.tokens)));
    parts.push(format!("{} tool {}", stats.tool_calls, if stats.tool_calls == 1 { "use" } else { "uses" }));
    parts.push(duration);

    eprintln!("  {}", style(parts.join(" · ")).dim());
    eprintln!();
}

fn ctrlc_handler(interrupt: Arc<AtomicBool>) {
    tokio::spawn(async move {
        loop {
            tokio::signal::ctrl_c().await.ok();
            if interrupt.load(Ordering::Relaxed) {
                // Double Ctrl+C = force exit
                eprintln!("\n  {}", style("Force exit.").red());
                std::process::exit(130);
            }
            interrupt.store(true, Ordering::Relaxed);
        }
    });
}
