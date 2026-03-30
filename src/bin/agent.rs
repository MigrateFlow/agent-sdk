use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use agent_sdk::config::{LlmConfig, LlmProvider};
use agent_sdk::tools::command_tools::RunCommandTool;
use agent_sdk::tools::fs_tools::{ListDirectoryTool, ReadFileTool, WriteFileTool};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::tools::search_tools::SearchFilesTool;
use agent_sdk::tools::subagent_tools::SpawnSubAgentTool;
use agent_sdk::tools::team_tools::SpawnAgentTeamTool;
use agent_sdk::tools::web_tools::WebSearchTool;
use agent_sdk::traits::tool::{Tool, ToolDefinition};
use agent_sdk::types::chat::ChatMessage;
use agent_sdk::AgentEvent;
use clap::Parser;
use console::{style, Term};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use serde_json::json;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "agent",
    about = "AI coding assistant — minimal Claude Code",
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

    /// Session file path
    #[arg(long)]
    session: Option<PathBuf>,

    /// One-shot mode: run this prompt and exit
    prompt: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CliTask {
    title: String,
    status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CliSessionData {
    messages: Vec<ChatMessage>,
    #[serde(default)]
    tasks: Vec<CliTask>,
}

// ─── Display helpers ─────────────────────────────────────────────────────────

/// Shorten home dir to ~ for display.
fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

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

fn print_welcome(model: &str, work_dir: &Path) {
    let version = env!("CARGO_PKG_VERSION");
    let branch = git_branch(work_dir);
    let dir = display_path(work_dir);

    let term = Term::stderr();
    let width = term.size().1 as usize;
    let box_width = width.min(60).max(40);
    let inner = box_width - 4; // "│ " + content + " │"

    let bar = "─".repeat(box_width - 2);

    eprintln!();
    eprintln!("  {}{}{}",
        style("╭").dim(), style(&bar).dim(), style("╮").dim());

    let title = format!("✻ agent v{}", version);
    let pad = inner.saturating_sub(console::measure_text_width(&title));
    eprintln!("  {} {}{} {}",
        style("│").dim(),
        style(&title).cyan().bold(),
        " ".repeat(pad),
        style("│").dim());

    let model_line = format!("model: {}", model);
    let pad = inner.saturating_sub(model_line.len());
    eprintln!("  {} {}{} {}",
        style("│").dim(),
        style(&model_line).white(),
        " ".repeat(pad),
        style("│").dim());

    let cwd_line = if let Some(ref b) = branch {
        format!("cwd:   {} ({})", dir, b)
    } else {
        format!("cwd:   {}", dir)
    };
    let pad = inner.saturating_sub(console::measure_text_width(&cwd_line));
    eprintln!("  {} {}{} {}",
        style("│").dim(),
        &cwd_line,
        " ".repeat(pad),
        style("│").dim());

    eprintln!("  {}{}{}",
        style("╰").dim(), style(&bar).dim(), style("╯").dim());

    eprintln!();
    eprintln!("  {}", style("/help for commands · Ctrl+C to cancel").dim());
    eprintln!();
}

fn print_help() {
    eprintln!();
    eprintln!("  {}", style("Slash commands").bold().underlined());
    eprintln!("    {}     Clear conversation & start fresh", style("/clear").cyan());
    eprintln!("    {}    Compact conversation with dynamic strategy", style("/compact").cyan());
    eprintln!("    {}     Show current task list", style("/tasks").cyan());
    eprintln!("    {}      Show session info", style("/cost").cyan());
    eprintln!("    {}      Show this help", style("/help").cyan());
    eprintln!("    {}      Exit", style("/quit").cyan());
    eprintln!();
    eprintln!("  {}", style("Tips").bold().underlined());
    eprintln!("    End a line with {} for multi-line input", style("\\").cyan());
    eprintln!("    {} interrupts the current generation", style("Ctrl+C").cyan());
    eprintln!("    Use {} for one-shot mode", style("agent \"your prompt\"").cyan());
    eprintln!();
}

fn create_spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("  {spinner:.cyan} {msg}")
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
            format!("Read {}", style(path).white())
        }
        "write_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("Write {}", style(path).white())
        }
        "list_directory" => {
            let path = arg_str(&args, "path").unwrap_or(".");
            format!("List {}", style(path).white())
        }
        "search_files" => {
            let pattern = arg_str(&args, "file_pattern")
                .or_else(|| arg_str(&args, "content_pattern"))
                .unwrap_or("files");
            format!("Search {}", style(pattern).white())
        }
        "web_search" => {
            let query = arg_str(&args, "query").unwrap_or("web");
            format!("Web search {}", style(query).white())
        }
        "run_command" => {
            let cmd = arg_str(&args, "command").unwrap_or("?");
            let short = if cmd.len() > 60 { &cmd[..60] } else { cmd };
            format!("$ {}", style(short).white())
        }
        "spawn_agent_team" => "Spawning agent team…".to_string(),
        "spawn_subagent" => {
            let name = arg_str(&args, "name").unwrap_or("subagent");
            let bg = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
            if bg {
                format!("Spawning subagent {} (background)…", style(name).white().bold())
            } else {
                format!("Spawning subagent {}…", style(name).white().bold())
            }
        }
        _ => {
            let name = humanize(tool_name);
            format!("{}", name)
        }
    }
}

/// Format a tool result preview line.
fn format_result_preview(tool_name: &str, result: &str) -> String {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();

    match tool_name {
        "read_file" => {
            let lines = val["lines"].as_u64().unwrap_or(0);
            format!("{} lines", lines)
        }
        "write_file" => {
            let written = val["lines_written"].as_u64().unwrap_or(0);
            format!("{} lines written", written)
        }
        "list_directory" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} entries", count)
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
            format!("{} web results", count)
        }
        "run_command" => {
            let code = val["exit_code"].as_i64().unwrap_or(-1);
            if code == 0 {
                let stdout = val["stdout"].as_str().unwrap_or("");
                let lines = stdout.lines().count();
                format!("exit 0 ({} lines)", lines)
            } else {
                let stderr = val["stderr"].as_str().unwrap_or("");
                let first_line = stderr.lines().next().unwrap_or("failed");
                format!("exit {} — {}", code, truncate(first_line, 60))
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
                format!("{} started in background", name)
            } else {
                format!("{} {} ({} tokens, {} tools)", name, status, format_token_count(tokens), tool_calls)
            }
        }
        "update_task_list" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} tasks updated", count)
        }
        _ => {
            if let Some(err) = val["error"].as_str() {
                format!("error: {}", truncate(err, 60))
            } else {
                truncate(result, 60)
            }
        }
    }
}

fn print_team_plan(arguments: &str) {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let teammates = args["teammates"].as_array().cloned().unwrap_or_default();
    let tasks = args["tasks"].as_array().cloned().unwrap_or_default();
    let auto_assign = args["auto_assign"].as_bool().unwrap_or(true);

    eprintln!("  {}", style("Team Plan").magenta().bold());

    if !teammates.is_empty() {
        eprintln!("    {}", style("teammates").dim());
        for teammate in teammates {
            let name = teammate["name"].as_str().unwrap_or("unnamed");
            let role = teammate["role"].as_str().unwrap_or("");
            let needs_plan = teammate["require_plan_approval"].as_bool().unwrap_or(false);
            if needs_plan {
                eprintln!(
                    "      {} {} {}",
                    style("•").magenta(),
                    style(name).white().bold(),
                    style(format!("— {} [plan approval]", truncate(role, 80))).dim(),
                );
            } else {
                eprintln!(
                    "      {} {} {}",
                    style("•").magenta(),
                    style(name).white().bold(),
                    style(format!("— {}", truncate(role, 80))).dim(),
                );
            }
        }
    }

    if !tasks.is_empty() {
        eprintln!(
            "    {} {}",
            style("tasks").dim(),
            style(if auto_assign { "(auto-assign)" } else { "(claim freely)" }).dim(),
        );
        for (idx, task) in tasks.iter().enumerate() {
            let title = task["title"].as_str().unwrap_or("untitled");
            let depends_on = task["depends_on"].as_array().cloned().unwrap_or_default();
            let line = if depends_on.is_empty() {
                title.to_string()
            } else {
                let deps = depends_on
                    .iter()
                    .filter_map(|v| v.as_u64())
                    .map(|v| (v + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} [deps: {}]", title, deps)
            };
            eprintln!(
                "      {} {}",
                style(format!("{}. ", idx + 1)).magenta(),
                style(line).white(),
            );
        }
    }

    eprintln!();
}

fn print_team_result_summary(result: &str) {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();
    let assignments = val["task_assignments"].as_array().cloned().unwrap_or_default();
    if assignments.is_empty() {
        return;
    }

    eprintln!("  {}", style("Task Assignments").magenta().bold());
    for (idx, assignment) in assignments.iter().enumerate() {
        let title = assignment["title"].as_str().unwrap_or("untitled");
        let target = assignment["target_file"].as_str().unwrap_or("?");
        let assignee = assignment["assigned_teammate"].as_str().unwrap_or("unassigned");
        eprintln!(
            "    {} {} {} {}",
            style(format!("{}. ", idx)).magenta(),
            style(title).white(),
            style(format!("→ {}", target)).dim(),
            style(format!("[{}]", assignee)).cyan(),
        );
    }
    eprintln!();
}

fn task_status_symbol(status: &str) -> &'static str {
    match status {
        "completed" => "✓",
        "in_progress" => "→",
        "blocked" => "!",
        _ => "•",
    }
}

fn print_task_list(tasks: &[CliTask]) {
    if tasks.is_empty() {
        return;
    }

    eprintln!("  {}", style("Task List").cyan().bold());
    for (idx, task) in tasks.iter().enumerate() {
        eprintln!(
            "    {} {} {} {}",
            style(format!("{}. ", idx + 1)).cyan(),
            style(task_status_symbol(&task.status)).cyan(),
            style(&task.title).white(),
            style(format!("[{}]", task.status)).dim(),
        );
    }
    eprintln!();
}

fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
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

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len])
    }
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
                    let limit = MAX_TOOL_RESULT_CHARS - 200;
                    let truncated = format!(
                        "{}…\n\n[truncated: {}/{} chars — use offset to read more]",
                        &text[..limit],
                        limit,
                        text.len()
                    );
                    *content = serde_json::Value::String(truncated);
                    return serde_json::to_string(&val)
                        .unwrap_or_else(|_| s[..MAX_TOOL_RESULT_CHARS].to_string());
                }
            }
        }
    }

    format!(
        "{}…[truncated: {}/{} chars]",
        &s[..MAX_TOOL_RESULT_CHARS],
        MAX_TOOL_RESULT_CHARS,
        s.len()
    )
}

fn format_token_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
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

// ─── Tools & session ─────────────────────────────────────────────────────────

fn build_tools(
    work_dir: &Path,
    allow_all: bool,
    llm_client: Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    subagent_registry: Arc<agent_sdk::SubAgentRegistry>,
    background_tx: Option<tokio::sync::mpsc::UnboundedSender<agent_sdk::agent::agent_loop::BackgroundResult>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(Arc::new(ReadFileTool {
        source_root: work_dir.to_path_buf(),
        work_dir: work_dir.to_path_buf(),
    }));
    registry.register(Arc::new(WriteFileTool {
        work_dir: work_dir.to_path_buf(),
    }));
    registry.register(Arc::new(ListDirectoryTool {
        source_root: work_dir.to_path_buf(),
        work_dir: work_dir.to_path_buf(),
    }));
    registry.register(Arc::new(SearchFilesTool {
        source_root: work_dir.to_path_buf(),
    }));
    registry.register(Arc::new(WebSearchTool));

    if allow_all {
        registry.register(Arc::new(RunCommandTool {
            work_dir: work_dir.to_path_buf(),
            allowed_commands: vec![],
        }));
    } else {
        registry.register(Arc::new(RunCommandTool::with_defaults(work_dir.to_path_buf())));
    }

    registry.register(Arc::new(SpawnAgentTeamTool {
        work_dir: work_dir.to_path_buf(),
        source_root: work_dir.to_path_buf(),
        llm_client: llm_client.clone(),
        event_tx: event_tx.clone(),
        background_tx: background_tx.clone(),
    }));

    registry.register(Arc::new(SpawnSubAgentTool {
        work_dir: work_dir.to_path_buf(),
        source_root: work_dir.to_path_buf(),
        llm_client,
        event_tx,
        registry: subagent_registry,
        background_tx,
    }));

    registry.register(Arc::new(UpdateTaskListTool { tasks }));

    registry
}

fn default_session_path(work_dir: &Path) -> PathBuf {
    agent_sdk::storage::AgentPaths::for_work_dir(work_dir)
        .map(|paths| paths.cli_session_path())
        .unwrap_or_else(|_| {
            work_dir
                .join(agent_sdk::config::AGENT_DIR)
                .join("session.json")
        })
}

fn load_session(path: &Path, system_prompt: &str) -> Option<CliSessionData> {
    let content = std::fs::read_to_string(path).ok()?;
    let session = serde_json::from_str::<CliSessionData>(&content)
        .ok()
        .or_else(|| {
            serde_json::from_str::<Vec<ChatMessage>>(&content)
                .ok()
                .map(|messages| CliSessionData {
                    messages,
                    tasks: Vec::new(),
                })
        })?;

    // Validate system prompt matches
    match session.messages.first() {
        Some(ChatMessage::System { content }) if content == system_prompt => Some(session),
        _ => None,
    }
}

fn save_session(path: &Path, messages: &[ChatMessage], tasks: &[CliTask]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let session = CliSessionData {
        messages: messages.to_vec(),
        tasks: tasks.to_vec(),
    };
    std::fs::write(path, serde_json::to_string(&session)?)?;
    Ok(())
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
    duration: std::time::Duration,
}

#[derive(Debug, Clone, Copy)]
struct CliCompactionProfile {
    keep_recent: usize,
    tool_limit: usize,
    assistant_limit: usize,
    compress_user_messages: bool,
}

async fn run_turn(
    messages: &mut Vec<ChatMessage>,
    user_input: &str,
    llm_client: &Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    work_dir: &Path,
    max_iterations: usize,
    allow_all: bool,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    interrupt: Arc<AtomicBool>,
    subagent_registry: Arc<agent_sdk::SubAgentRegistry>,
) -> anyhow::Result<TurnStats> {
    // Create background result channel — tools send completed background results
    // here, and we drain them before each LLM call to inject into conversation.
    let (background_tx, mut background_rx) =
        tokio::sync::mpsc::unbounded_channel::<agent_sdk::agent::agent_loop::BackgroundResult>();

    let tools = build_tools(
        work_dir,
        allow_all,
        llm_client.clone(),
        event_tx,
        tasks.clone(),
        subagent_registry,
        Some(background_tx),
    );
    let tool_defs = tools.definitions();
    let started = Instant::now();

    messages.push(ChatMessage::user(user_input));

    let mut total_tokens = 0u64;
    let mut tool_calls_count = 0usize;

    for _iteration in 0..max_iterations {
        // Drain any completed background agent results and inject them
        // into the conversation so the LLM can reference them.
        while let Ok(result) = background_rx.try_recv() {
            let kind_label = match result.kind {
                agent_sdk::agent::agent_loop::BackgroundResultKind::SubAgent => "subagent",
                agent_sdk::agent::agent_loop::BackgroundResultKind::AgentTeam => "agent team",
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
            eprintln!("\n  {}", style("⏎ Cancelled").yellow());
            return Ok(TurnStats {
                tokens: total_tokens,
                tool_calls: tool_calls_count,
                duration: started.elapsed(),
            });
        }

        let spinner = create_spinner("Thinking…");

        let result = llm_client.chat(messages, &tool_defs).await;

        spinner.finish_and_clear();

        // Check interrupt after call returns
        if interrupt.load(Ordering::Relaxed) {
            interrupt.store(false, Ordering::Relaxed);
            eprintln!("  {}", style("⏎ Cancelled").yellow());
            return Ok(TurnStats {
                tokens: total_tokens,
                tool_calls: tool_calls_count,
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
                        eprintln!("  {}", style(truncate(text, 200)).dim().italic());
                    }
                }

                messages.push(response.clone());

                for tc in tool_calls {
                    let label = format_tool_label(&tc.function.name, &tc.function.arguments);
                    eprintln!("  {} {}", style("⎿").cyan(), label);

                    if tc.function.name == "spawn_agent_team" {
                        print_team_plan(&tc.function.arguments);
                    }

                    let args: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or_default();

                    let result = tools.execute(&tc.function.name, args).await;

                    let result_content = match &result {
                        Ok(val) => {
                            let full = serde_json::to_string(val).unwrap_or_default();
                            truncate_tool_result(&full)
                        }
                        Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                    };

                    let preview = format_result_preview(&tc.function.name, &result_content);
                    eprintln!("    {}", style(&preview).dim());

                    if tc.function.name == "spawn_agent_team" {
                        print_team_result_summary(&result_content);
                    } else if tc.function.name == "update_task_list" {
                        let current = tasks.lock().expect("task list mutex poisoned").clone();
                        print_task_list(&current);
                    }

                    messages.push(ChatMessage::tool_result(&tc.id, &result_content));
                    tool_calls_count += 1;
                }
                eprintln!();
            }

            ChatMessage::Assistant { ref content, .. } => {
                let answer = content.clone().unwrap_or_default();
                messages.push(response);

                // Print final answer
                eprintln!();
                for line in answer.lines() {
                    println!("{}", line);
                }
                eprintln!();

                return Ok(TurnStats {
                    tokens: total_tokens,
                    tool_calls: tool_calls_count,
                    duration: started.elapsed(),
                });
            }

            other => {
                let text = other.text_content().unwrap_or("").to_string();
                messages.push(other);
                eprintln!();
                println!("{}", text);
                eprintln!();
                return Ok(TurnStats {
                    tokens: total_tokens,
                    tool_calls: tool_calls_count,
                    duration: started.elapsed(),
                });
            }
        }
    }

    eprintln!(
        "  {} max iterations ({}) reached",
        style("⚠").yellow(),
        max_iterations,
    );
    Ok(TurnStats {
        tokens: total_tokens,
        tool_calls: tool_calls_count,
        duration: started.elapsed(),
    })
}

// ─── Compact ─────────────────────────────────────────────────────────────────

fn select_cli_compaction_profile(messages: &[ChatMessage]) -> (&'static str, CliCompactionProfile) {
    let total = messages.len().max(1);
    let tool_count = messages.iter().filter(|m| matches!(m, ChatMessage::Tool { .. })).count();
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
        .count();
    let tool_ratio = tool_count as f64 / total as f64;
    let assistant_ratio = assistant_count as f64 / total as f64;

    if total >= 60 || tool_ratio >= 0.35 {
        return (
            "aggressive",
            CliCompactionProfile {
                keep_recent: 5,
                tool_limit: 120,
                assistant_limit: 120,
                compress_user_messages: true,
            },
        );
    }

    if assistant_ratio >= 0.45 {
        return (
            "conservative",
            CliCompactionProfile {
                keep_recent: 8,
                tool_limit: 350,
                assistant_limit: 250,
                compress_user_messages: false,
            },
        );
    }

    (
        "default",
        CliCompactionProfile {
            keep_recent: 6,
            tool_limit: 200,
            assistant_limit: 150,
            compress_user_messages: false,
        },
    )
}

fn compact_conversation(messages: &mut Vec<ChatMessage>) -> (usize, &'static str) {
    let before = messages.len();
    if before <= 4 {
        return (0, "none");
    }

    let (strategy, profile) = select_cli_compaction_profile(messages);
    let keep_tail = profile.keep_recent.min(before - 1);
    let compact_end = before - keep_tail;

    for i in 1..compact_end {
        match &messages[i] {
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                if content.len() > profile.tool_limit {
                    let summary = format!("[compacted: {} chars]", content.len());
                    messages[i] = ChatMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        content: summary,
                    };
                }
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } if content
                .as_ref()
                .is_some_and(|c| c.len() > profile.assistant_limit) =>
            {
                let short = content.as_ref().map(|c| truncate(c, profile.assistant_limit));
                messages[i] = ChatMessage::Assistant {
                    content: short,
                    tool_calls: tool_calls.clone(),
                };
            }
            ChatMessage::User { content } if profile.compress_user_messages && content.len() > 200 => {
                messages[i] = ChatMessage::User {
                    content: truncate(content, 150),
                };
            }
            _ => {}
        }
    }

    let _ = before - messages.len();
    // Messages aren't removed, just shortened — return count of compacted entries
    (compact_end.saturating_sub(1), strategy)
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
    let work_dir = std::fs::canonicalize(&cli.dir)?;

    // ── Provider detection ──
    let provider = if let Some(ref p) = cli.provider {
        match p.to_lowercase().as_str() {
            "openai" | "open_ai" => LlmProvider::OpenAi,
            _ => LlmProvider::Claude,
        }
    } else if std::env::var("LLM_PROVIDER")
        .map(|v| v.to_lowercase())
        .as_deref()
        == Ok("openai")
        || (std::env::var("OPENAI_API_KEY").is_ok() && std::env::var("ANTHROPIC_API_KEY").is_err())
    {
        LlmProvider::OpenAi
    } else {
        LlmProvider::Claude
    };

    // ── Model detection ──
    let model = cli.model.unwrap_or_else(|| {
        let provider_env = match provider {
            LlmProvider::Claude => "ANTHROPIC_MODEL",
            LlmProvider::OpenAi => "OPENAI_MODEL",
        };
        std::env::var(provider_env)
            .or_else(|_| std::env::var("LLM_MODEL"))
            .unwrap_or_else(|_| match provider {
                LlmProvider::Claude => "claude-sonnet-4-20250514".to_string(),
                LlmProvider::OpenAi => "gpt-4o".to_string(),
            })
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

    tokio::spawn(async move {
        // Color palette for teammates — cycle through these
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

        let teammate_color = |name: &str, map: &mut std::collections::HashMap<String, console::Color>, next: &mut usize| -> console::Color {
            *map.entry(name.to_string()).or_insert_with(|| {
                let c = COLORS[*next % COLORS.len()];
                *next += 1;
                c
            })
        };

        // Fixed-width name tag for alignment
        let name_tag = |name: &str, color: console::Color| -> String {
            let display = if name.len() > 18 { &name[..18] } else { name };
            format!("{}", style(format!("{:<18}", display)).fg(color).bold())
        };

        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::TeamSpawned { teammate_count } => {
                    eprintln!();
                    eprintln!(
                        "  {} {} teammates",
                        style("⎿ Team").cyan().bold(),
                        style(teammate_count).white(),
                    );
                }
                AgentEvent::TeammateSpawned { ref name, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {}",
                        style("+").fg(c),
                        style(name).fg(c).bold(),
                    );
                }
                AgentEvent::TaskStarted { ref name, ref title, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!();
                    eprintln!(
                        "    {} {} {}",
                        name_tag(name, c),
                        style("▸").fg(c),
                        title,
                    );
                }
                AgentEvent::Thinking { ref name, ref content, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {}",
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
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    let label = format_tool_label(tool_name, arguments);
                    eprintln!(
                        "    {} {} {}",
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
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    let preview = format_result_preview(tool_name, result_preview);
                    eprintln!(
                        "    {}   {}",
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
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} {} tokens · {} tools",
                        name_tag(name, c),
                        style("✓").green(),
                        format_token_count(tokens_used),
                        tool_calls,
                    );
                }
                AgentEvent::TaskFailed { ref name, ref error, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} {}",
                        name_tag(name, c),
                        style("✗").red(),
                        style(truncate(error, 60)).red().dim(),
                    );
                }
                AgentEvent::PlanSubmitted { ref name, ref plan_preview, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} {}",
                        name_tag(name, c),
                        style("plan").yellow(),
                        style(truncate(plan_preview, 60)).dim(),
                    );
                }
                AgentEvent::PlanApproved { ref name, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} approved",
                        name_tag(name, c),
                        style("plan").green(),
                    );
                }
                AgentEvent::PlanRejected { ref name, ref feedback, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} rejected: {}",
                        name_tag(name, c),
                        style("plan").yellow(),
                        style(truncate(feedback, 60)).dim(),
                    );
                }
                AgentEvent::TeammateIdle { ref name, tasks_completed, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} idle ({} tasks done)",
                        name_tag(name, c),
                        style("…").dim(),
                        tasks_completed,
                    );
                }
                AgentEvent::AgentShutdown { ref name, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {}",
                        name_tag(name, c),
                        style("done").dim(),
                    );
                }
                AgentEvent::SubAgentSpawned { ref name, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} {}",
                        style("⎿").cyan(),
                        style("subagent").dim(),
                        style(name).fg(c).bold(),
                    );
                }
                AgentEvent::SubAgentCompleted {
                    ref name,
                    tokens_used,
                    tool_calls,
                    ref final_content,
                    ..
                } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} {} tokens · {} tools",
                        name_tag(name, c),
                        style("✓").green(),
                        format_token_count(tokens_used),
                        tool_calls,
                    );
                    // Show a brief preview of the result
                    if !final_content.is_empty() {
                        let preview = truncate(final_content.lines().next().unwrap_or(""), 80);
                        eprintln!(
                            "    {}   {}",
                            name_tag(name, c),
                            style(preview).dim(),
                        );
                    }
                }
                AgentEvent::SubAgentFailed { ref name, ref error, .. } => {
                    let c = teammate_color(name, &mut color_map, &mut next_color);
                    eprintln!(
                        "    {} {} {}",
                        name_tag(name, c),
                        style("✗").red(),
                        style(truncate(error, 60)).red().dim(),
                    );
                }
                _ => {}
            }
        }
    });

    // ── System prompt ──
    let system_prompt = cli
        .system
        .unwrap_or_else(|| agent_sdk::prompts::cli_system_prompt(&work_dir));

    let session_path = cli
        .session
        .unwrap_or_else(|| default_session_path(&work_dir));

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

    // ── One-shot mode ──
    if !cli.prompt.is_empty() {
        let prompt = cli.prompt.join(" ");
        let mut messages = vec![ChatMessage::system(&system_prompt)];
        let tasks = Arc::new(Mutex::new(Vec::<CliTask>::new()));

        let stats = run_turn(
            &mut messages,
            &prompt,
            &llm_client,
            &work_dir,
            cli.max_iterations,
            cli.allow_all_commands,
            Some(event_tx),
            tasks,
            interrupt,
            subagent_registry,
        )
        .await?;

        print_usage(&stats);
        return Ok(());
    }

    // ── Interactive REPL ──
    print_welcome(&model, &work_dir);

    let tasks = Arc::new(Mutex::new(Vec::<CliTask>::new()));

    let mut messages = match load_session(&session_path, &system_prompt) {
        Some(session) => {
            let n = session.messages.len();
            {
                let mut current = tasks.lock().expect("task list mutex poisoned");
                *current = session.tasks;
            }
            eprintln!(
                "  {} restored ({} messages)",
                style("↻").green(),
                style(n).dim(),
            );
            eprintln!();
            let current = tasks.lock().expect("task list mutex poisoned").clone();
            print_task_list(&current);
            session.messages
        }
        None => {
            vec![ChatMessage::system(&system_prompt)]
        }
    };

    let mut session_tokens = 0u64;
    let mut session_tool_calls = 0usize;
    let mut session_turns = 0usize;

    loop {
        eprint!("{} ", style("❯").cyan().bold());
        io::stderr().flush()?;

        let input = read_input()?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        // ── Slash commands ──
        match input.as_str() {
            "/quit" | "/exit" | "/q" => break,

            "/clear" | "/new" => {
                messages = vec![ChatMessage::system(&system_prompt)];
                {
                    let mut current = tasks.lock().expect("task list mutex poisoned");
                    current.clear();
                }
                save_session(
                    &session_path,
                    &messages,
                    &tasks.lock().expect("task list mutex poisoned"),
                )?;
                session_tokens = 0;
                session_tool_calls = 0;
                session_turns = 0;
                eprintln!("  {}", style("Conversation cleared.").dim());
                eprintln!();
                continue;
            }

            "/compact" => {
                let (freed, strategy) = compact_conversation(&mut messages);
                save_session(
                    &session_path,
                    &messages,
                    &tasks.lock().expect("task list mutex poisoned"),
                )?;
                eprintln!(
                    "  {} compacted {} messages using {} strategy ({} remaining)",
                    style("↻").green(),
                    freed,
                    style(strategy).dim(),
                    messages.len(),
                );
                eprintln!();
                continue;
            }

            "/cost" | "/status" => {
                eprintln!(
                    "  {} {}",
                    style("session").white().bold(),
                    style(&display_path(&session_path)).dim(),
                );
                eprintln!(
                    "  {} turns · {} tokens · {} tool calls · {} messages",
                    style(session_turns).white(),
                    style(format_token_count(session_tokens)).white(),
                    style(session_tool_calls).white(),
                    style(messages.len()).dim(),
                );
                let current = tasks.lock().expect("task list mutex poisoned").clone();
                print_task_list(&current);
                eprintln!();
                continue;
            }

            "/tasks" => {
                let current = tasks.lock().expect("task list mutex poisoned").clone();
                print_task_list(&current);
                continue;
            }

            "/help" => {
                print_help();
                continue;
            }

            cmd if cmd.starts_with('/') => {
                eprintln!(
                    "  {} unknown command: {}",
                    style("?").yellow(),
                    style(cmd).dim(),
                );
                eprintln!("  {}", style("Type /help for available commands").dim());
                eprintln!();
                continue;
            }

            _ => {}
        }

        let stats = run_turn(
            &mut messages,
            &input,
            &llm_client,
            &work_dir,
            cli.max_iterations,
            cli.allow_all_commands,
            Some(event_tx.clone()),
            tasks.clone(),
            interrupt.clone(),
            subagent_registry.clone(),
        )
        .await?;

        session_tokens += stats.tokens;
        session_tool_calls += stats.tool_calls;
        session_turns += 1;

        print_usage(&stats);

        if let Err(e) = save_session(
            &session_path,
            &messages,
            &tasks.lock().expect("task list mutex poisoned"),
        ) {
            eprintln!("  {} session save: {}", style("⚠").yellow(), e);
        }
    }

    eprintln!();
    eprintln!(
        "  {} {} turns · {} tokens · {} tool calls",
        style("session").dim(),
        session_turns,
        format_token_count(session_tokens),
        session_tool_calls,
    );
    eprintln!();
    Ok(())
}

fn print_usage(stats: &TurnStats) {
    let duration = if stats.duration.as_secs() >= 60 {
        format!("{}m{:.0}s", stats.duration.as_secs() / 60, stats.duration.as_secs() % 60)
    } else {
        format!("{:.1}s", stats.duration.as_secs_f64())
    };

    eprintln!(
        "  {}",
        style(format!(
            "{} tokens · {} tool calls · {}",
            format_token_count(stats.tokens),
            stats.tool_calls,
            duration,
        ))
        .dim(),
    );
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
