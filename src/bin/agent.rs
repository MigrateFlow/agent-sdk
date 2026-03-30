use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_sdk::config::{LlmConfig, LlmProvider};
use agent_sdk::tools::command_tools::RunCommandTool;
use agent_sdk::tools::fs_tools::{ListDirectoryTool, ReadFileTool, WriteFileTool};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::tools::search_tools::SearchFilesTool;
use agent_sdk::tools::team_tools::SpawnAgentTeamTool;
use agent_sdk::types::chat::ChatMessage;
use agent_sdk::AgentEvent;
use clap::Parser;
use console::style;

#[derive(Parser)]
#[command(name = "agent", about = "Interactive AI agent with tool access")]
struct Cli {
    /// LLM provider: claude or openai (auto-detected from LLM_PROVIDER env)
    #[arg(short, long)]
    provider: Option<String>,

    /// Model name (auto-detected from OPENAI_MODEL / ANTHROPIC_MODEL / LLM_MODEL env)
    #[arg(short, long)]
    model: Option<String>,

    /// Working directory
    #[arg(short = 'd', long, default_value = ".")]
    dir: PathBuf,

    /// Max tokens per LLM response
    #[arg(long, default_value = "16384")]
    max_tokens: usize,

    /// Max ReAct iterations per turn
    #[arg(long, default_value = "50")]
    max_iterations: usize,

    /// System prompt override
    #[arg(long)]
    system: Option<String>,

    /// Allow all shell commands (no whitelist)
    #[arg(long)]
    allow_all_commands: bool,

    /// Session file for interactive mode (defaults to <workdir>/.agent/cli-session.json)
    #[arg(long)]
    session: Option<PathBuf>,

    /// One-shot mode: run this prompt and exit
    prompt: Vec<String>,
}

fn build_system_prompt(work_dir: &std::path::Path) -> String {
    agent_sdk::prompts::cli_system_prompt(work_dir)
}

fn build_tools(
    work_dir: &std::path::Path,
    allow_all: bool,
    llm_client: Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
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

    if allow_all {
        registry.register(Arc::new(RunCommandTool {
            work_dir: work_dir.to_path_buf(),
            allowed_commands: vec![],
        }));
    } else {
        registry.register(Arc::new(RunCommandTool::with_defaults(work_dir.to_path_buf())));
    }

    // Agent team tool — lets the LLM decide when to spawn a team
    registry.register(Arc::new(SpawnAgentTeamTool {
        work_dir: work_dir.to_path_buf(),
        source_root: work_dir.to_path_buf(),
        llm_client,
        event_tx,
    }));

    registry
}

fn default_session_path(work_dir: &Path) -> PathBuf {
    work_dir.join(agent_sdk::config::AGENT_DIR).join("cli-session.json")
}

fn load_session(
    session_path: &Path,
    system_prompt: &str,
) -> anyhow::Result<Option<Vec<ChatMessage>>> {
    if !session_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(session_path)?;
    let messages: Vec<ChatMessage> = serde_json::from_str(&content)?;

    let first_ok = matches!(
        messages.first(),
        Some(ChatMessage::System { content }) if content == system_prompt
    );

    if !first_ok {
        return Ok(None);
    }

    Ok(Some(messages))
}

fn save_session(session_path: &Path, messages: &[ChatMessage]) -> anyhow::Result<()> {
    if let Some(parent) = session_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(messages)?;
    std::fs::write(session_path, content)?;
    Ok(())
}

fn fresh_session(system_prompt: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::system(system_prompt)]
}

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

    // Auto-detect provider
    let provider = if let Some(ref p) = cli.provider {
        match p.to_lowercase().as_str() {
            "openai" | "open_ai" => LlmProvider::OpenAi,
            _ => LlmProvider::Claude,
        }
    } else if std::env::var("LLM_PROVIDER")
        .map(|v| v.to_lowercase())
        .as_deref()
        == Ok("openai")
        || (std::env::var("OPENAI_API_KEY").is_ok()
            && std::env::var("ANTHROPIC_API_KEY").is_err())
    {
        LlmProvider::OpenAi
    } else {
        LlmProvider::Claude
    };

    // Auto-detect model
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

    // Event channel for team monitoring
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

    // Spawn event printer in background
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::TeamSpawned { teammate_count } => {
                    eprintln!(
                        "  {} spawned {} teammates",
                        style("team").magenta().bold(),
                        style(teammate_count).white(),
                    );
                }
                AgentEvent::TeammateSpawned { name, .. } => {
                    eprintln!(
                        "  {} + {}",
                        style("team").magenta().bold(),
                        style(&name).magenta(),
                    );
                }
                AgentEvent::TaskStarted { title, .. } => {
                    eprintln!(
                        "  {} {}",
                        style("task").blue().bold(),
                        style(&title).blue(),
                    );
                }
                AgentEvent::ToolCall {
                    agent_id,
                    tool_name,
                    arguments,
                    ..
                } => {
                    let short_id = short_agent_id(&agent_id);
                    let label = format_teammate_tool_call(&tool_name, &arguments);
                    eprintln!("  {} {}", style(short_id).magenta(), label);
                }
                AgentEvent::ToolResult {
                    agent_id,
                    tool_name,
                    result_preview,
                    ..
                } => {
                    let short_id = short_agent_id(&agent_id);
                    let tool = humanize_tool_name(&tool_name);
                    eprintln!(
                        "  {} -> {} {}",
                        style(short_id).magenta(),
                        style(tool).dim(),
                        result_preview
                    );
                }
                AgentEvent::TaskCompleted {
                    tokens_used,
                    tool_calls,
                    ..
                } => {
                    eprintln!(
                        "  {} {} tokens, {} tool calls",
                        style("  done").green().dim(),
                        style(tokens_used).dim(),
                        style(tool_calls).dim(),
                    );
                }
                AgentEvent::TaskFailed { error, .. } => {
                    eprintln!(
                        "  {} {}",
                        style("  fail").red().bold(),
                        style(&error).red().dim(),
                    );
                }
                AgentEvent::PlanSubmitted { plan_preview, .. } => {
                    eprintln!(
                        "  {} {}",
                        style("plan").yellow().bold(),
                        style(&plan_preview).dim(),
                    );
                }
                AgentEvent::PlanApproved { .. } => {
                    eprintln!("  {} approved", style("plan").yellow().bold());
                }
                AgentEvent::PlanRejected { feedback, .. } => {
                    eprintln!(
                        "  {} rejected: {}",
                        style("plan").yellow().bold(),
                        style(&feedback).dim(),
                    );
                }
                AgentEvent::TeammateIdle { .. } => {
                    eprintln!("  {} teammate idle", style("team").magenta().dim());
                }
                AgentEvent::ShutdownRequested { .. } => {
                    eprintln!("  {} shutting down", style("team").magenta().dim());
                }
                _ => {}
            }
        }
    });

    let system_prompt = cli
        .system
        .unwrap_or_else(|| build_system_prompt(&work_dir));

    eprintln!(
        "{} {} ({})",
        style("agent").cyan().bold(),
        style(&model).white(),
        style(work_dir.display()).dim(),
    );
    let session_path = cli
        .session
        .clone()
        .unwrap_or_else(|| default_session_path(&work_dir));

    eprintln!("{}", style("  Type your message, press Enter to send. Ctrl+C to exit.").dim());
    eprintln!();

    let mut messages = if cli.prompt.is_empty() {
        match load_session(&session_path, &system_prompt)? {
            Some(messages) => {
                eprintln!(
                    "{} {} ({})",
                    style("session").green().bold(),
                    style("restored conversation").green(),
                    style(format!("{} messages", messages.len())).dim(),
                );
                messages
            }
            None => {
                let fresh = fresh_session(&system_prompt);
                save_session(&session_path, &fresh)?;
                fresh
            }
        }
    } else {
        fresh_session(&system_prompt)
    };

    // One-shot mode
    if !cli.prompt.is_empty() {
        let prompt = cli.prompt.join(" ");
        run_turn(
            &mut messages,
            &prompt,
            &llm_client,
            &work_dir,
            cli.max_iterations,
            cli.allow_all_commands,
            Some(event_tx),
        )
        .await?;
        return Ok(());
    }

    // REPL mode
    loop {
        eprint!("{} ", style(">").cyan().bold());
        io::stderr().flush()?;

        let input = read_input()?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        match input.as_str() {
            "/quit" | "/exit" | "/q" => break,
            "/clear" | "/new" => {
                messages = fresh_session(&system_prompt);
                save_session(&session_path, &messages)?;
                eprintln!("{}", style("  Conversation cleared.").dim());
                continue;
            }
            "/help" => {
                print_help();
                continue;
            }
            "/status" => {
                eprintln!(
                    "  {} {} | {} {}",
                    style("session").green().bold(),
                    style(session_path.display()).dim(),
                    style("messages").green().bold(),
                    style(messages.len()).dim(),
                );
                continue;
            }
            _ => {}
        }

        run_turn(
            &mut messages,
            &input,
            &llm_client,
            &work_dir,
            cli.max_iterations,
            cli.allow_all_commands,
            Some(event_tx.clone()),
        )
        .await?;

        save_session(&session_path, &messages)?;
    }

    Ok(())
}

/// Run a single conversational turn with the ReAct loop.
async fn run_turn(
    messages: &mut Vec<ChatMessage>,
    user_input: &str,
    llm_client: &Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    work_dir: &std::path::Path,
    max_iterations: usize,
    allow_all: bool,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
) -> anyhow::Result<()> {
    let tools = build_tools(work_dir, allow_all, llm_client.clone(), event_tx);
    let tool_defs = tools.definitions();

    messages.push(ChatMessage::user(user_input));

    let mut total_tokens = 0u64;
    let mut tool_calls_count = 0usize;

    for _iteration in 0..max_iterations {
        let (response, tokens) = llm_client.chat(messages, &tool_defs).await?;
        total_tokens += tokens;

        match response {
            ChatMessage::Assistant { ref content, ref tool_calls } if !tool_calls.is_empty() => {
                if let Some(text) = content {
                    if !text.is_empty() {
                        eprintln!(
                            "  {} {}",
                            style("thinking").yellow().bold(),
                            style(truncate(text, 200)).dim(),
                        );
                    }
                }

                messages.push(response.clone());

                for tc in tool_calls {
                    let args_preview = truncate(&tc.function.arguments, 120);
                    let is_team = tc.function.name == "spawn_agent_team";

                    if is_team {
                        eprintln!(
                            "  {} {}",
                            style("team").magenta().bold(),
                            style("Spawning agent team...").magenta(),
                        );
                    } else {
                        eprintln!(
                            "  {} {} {}",
                            style("tool").cyan().bold(),
                            style(&tc.function.name).cyan(),
                            style(&args_preview).dim(),
                        );
                    }

                    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();

                    let result = tools.execute(&tc.function.name, args).await;

                    let result_content = match &result {
                        Ok(val) => {
                            let full = serde_json::to_string(val).unwrap_or_default();
                            truncate_tool_result(&full)
                        }
                        Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                    };

                    let preview = truncate(&result_content, 120);
                    if is_team {
                        eprintln!(
                            "  {} {}",
                            style("team").magenta().bold(),
                            style(&preview).dim(),
                        );
                    } else {
                        eprintln!(
                            "  {} {}",
                            style("  ↳").green().dim(),
                            style(&preview).dim(),
                        );
                    }

                    messages.push(ChatMessage::tool_result(&tc.id, &result_content));
                    tool_calls_count += 1;
                }
            }
            ChatMessage::Assistant { ref content, .. } => {
                let answer = content.clone().unwrap_or_default();
                messages.push(response);

                println!("\n{}\n", answer);

                eprintln!(
                    "  {} tokens: {} | tool calls: {}",
                    style("usage").dim(),
                    style(total_tokens).dim(),
                    style(tool_calls_count).dim(),
                );
                eprintln!();
                return Ok(());
            }
            other => {
                let text = other.text_content().unwrap_or("").to_string();
                messages.push(other);
                println!("\n{}\n", text);
                return Ok(());
            }
        }
    }

    eprintln!(
        "  {} max iterations ({}) reached",
        style("limit").yellow().bold(),
        max_iterations,
    );
    Ok(())
}

fn read_input() -> io::Result<String> {
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line)
}

fn print_help() {
    eprintln!();
    eprintln!("  {}", style("Commands:").bold());
    eprintln!("    {}    — Clear conversation history", style("/clear").cyan());
    eprintln!("    {}      — Start a fresh session", style("/new").cyan());
    eprintln!("    {}   — Show current session info", style("/status").cyan());
    eprintln!("    {}   — Show this help", style("/help").cyan());
    eprintln!("    {}   — Exit", style("/quit").cyan());
    eprintln!();
    eprintln!("  {}", style("Usage:").bold());
    eprintln!("    agent [OPTIONS] [PROMPT]      One-shot mode");
    eprintln!("    agent [OPTIONS]               Interactive REPL");
    eprintln!("    agent --allow-all-commands     Allow any shell command");
    eprintln!("    agent -p openai -m gpt-4o     Use OpenAI");
    eprintln!("    agent --session /tmp/agent.json  Use a custom session file");
    eprintln!();
    eprintln!("  {}", style("Agent Teams:").bold());
    eprintln!("    The agent automatically decides when to spawn a team.");
    eprintln!("    Ask it to work on complex tasks with parallel components");
    eprintln!("    and it will create teammates on its own.");
    eprintln!();
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
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
                        "{}...\n\n[truncated: {}/{} chars]",
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
        "{}...[truncated: {}/{} chars]",
        &s[..MAX_TOOL_RESULT_CHARS],
        MAX_TOOL_RESULT_CHARS,
        s.len()
    )
}

fn short_agent_id(agent_id: &agent_sdk::error::AgentId) -> String {
    agent_id.to_string().chars().take(8).collect()
}

fn format_teammate_tool_call(tool_name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();

    match tool_name {
        "read_file" => format!("Read {}", arg_as_path(&args, "path").unwrap_or("?")),
        "list_directory" => format!("List {}", arg_as_path(&args, "path").unwrap_or("?")),
        "write_file" => format!("Write {}", arg_as_path(&args, "path").unwrap_or("?")),
        "search_files" => format!(
            "Search {}",
            arg_as_str(&args, "query")
                .or_else(|| arg_as_str(&args, "pattern"))
                .unwrap_or("files")
        ),
        "run_command" => format!(
            "Run {}",
            arg_as_str(&args, "command")
                .or_else(|| arg_as_str(&args, "cmd"))
                .unwrap_or("command")
        ),
        "list_completed_tasks" => "Tasks completed".to_string(),
        "get_task_context" => format!(
            "Task context {}",
            arg_as_str(&args, "task_id").unwrap_or("?")
        ),
        "read_memory" => format!("Recall {}", arg_as_str(&args, "key").unwrap_or("?")),
        "write_memory" => format!("Remember {}", arg_as_str(&args, "key").unwrap_or("?")),
        "list_memory" => "List memory".to_string(),
        _ => format!("{} {}", humanize_tool_name(tool_name), truncate(arguments, 80)),
    }
}

fn arg_as_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn arg_as_path<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arg_as_str(args, key).map(|p| if p.is_empty() { "/" } else { p })
}

fn humanize_tool_name(name: &str) -> String {
    let mut out = String::new();
    for (idx, part) in name.split('_').filter(|s| !s.is_empty()).enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        name.to_string()
    } else {
        out
    }
}
