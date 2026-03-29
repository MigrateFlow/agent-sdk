use std::io::{self, BufRead, Write};
use std::path::PathBuf;
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

    /// One-shot mode: run this prompt and exit
    prompt: Vec<String>,
}

fn build_system_prompt(work_dir: &std::path::Path) -> String {
    format!(
        r#"You are an expert AI coding assistant with direct access to the filesystem and shell.

## Environment
- Working directory: {work_dir}
- You can read, write, search files, and run commands

## Available Tools
- `read_file` — Read file contents (supports offset/max_lines for large files)
- `write_file` — Write/create files in the working directory
- `list_directory` — List directory contents
- `search_files` — Search by glob pattern and/or content
- `run_command` — Execute shell commands
- `spawn_agent_team` — Spawn a team of parallel agents for complex tasks

## Agent Teams
When a task is complex and has independent parts that benefit from parallel work,
use `spawn_agent_team` to create a team. Define teammates (with names and roles)
and tasks (with descriptions and dependencies). The team works in parallel and
reports back when done.

Good candidates for agent teams:
- Building multiple independent modules
- Reviewing code from different angles (security, performance, tests)
- Investigating a bug with competing hypotheses

Do NOT use agent teams for simple tasks — handle those yourself directly.

## Guidelines
- Read files before modifying them
- Write complete files, no placeholders
- After writing code, verify it compiles/works using run_command
- Be concise in your responses
- When asked to make changes, do them directly — don't just explain"#,
        work_dir = work_dir.display(),
    )
}

fn build_tools(
    work_dir: &PathBuf,
    allow_all: bool,
    llm_client: Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    registry.register(Arc::new(ReadFileTool {
        source_root: work_dir.clone(),
        work_dir: work_dir.clone(),
    }));
    registry.register(Arc::new(WriteFileTool {
        work_dir: work_dir.clone(),
    }));
    registry.register(Arc::new(ListDirectoryTool {
        source_root: work_dir.clone(),
        work_dir: work_dir.clone(),
    }));
    registry.register(Arc::new(SearchFilesTool {
        source_root: work_dir.clone(),
    }));

    if allow_all {
        registry.register(Arc::new(RunCommandTool {
            work_dir: work_dir.clone(),
            allowed_commands: vec![],
        }));
    } else {
        registry.register(Arc::new(RunCommandTool::with_defaults(work_dir.clone())));
    }

    // Agent team tool — lets the LLM decide when to spawn a team
    registry.register(Arc::new(SpawnAgentTeamTool {
        work_dir: work_dir.clone(),
        source_root: work_dir.clone(),
        llm_client,
        event_tx,
    }));

    registry
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
    {
        LlmProvider::OpenAi
    } else if std::env::var("OPENAI_API_KEY").is_ok()
        && std::env::var("ANTHROPIC_API_KEY").is_err()
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
        requests_per_minute: 60,
        tokens_per_minute: 200_000,
        api_key: None,
        api_base_url: None,
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
    eprintln!(
        "{}",
        style("  Type your message, press Enter to send. Ctrl+C to exit.").dim()
    );
    eprintln!();

    let mut messages: Vec<ChatMessage> = vec![ChatMessage::system(&system_prompt)];

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
            "/clear" => {
                messages = vec![ChatMessage::system(&system_prompt)];
                eprintln!("{}", style("  Conversation cleared.").dim());
                continue;
            }
            "/help" => {
                print_help();
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
    }

    Ok(())
}

/// Run a single conversational turn with the ReAct loop.
async fn run_turn(
    messages: &mut Vec<ChatMessage>,
    user_input: &str,
    llm_client: &Arc<dyn agent_sdk::traits::llm_client::LlmClient>,
    work_dir: &PathBuf,
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
            ChatMessage::Assistant {
                ref content,
                ref tool_calls,
            } if !tool_calls.is_empty() => {
                // Show thinking
                if let Some(text) = content {
                    if !text.is_empty() {
                        eprintln!(
                            "  {} {}",
                            style("thinking").dim(),
                            style(truncate(text, 200)).dim().italic(),
                        );
                    }
                }

                messages.push(response);

                // Execute each tool call
                let pending_calls: Vec<_> = if let Some(ChatMessage::Assistant { tool_calls, .. }) =
                    messages.last()
                {
                    tool_calls.clone()
                } else {
                    vec![]
                };

                for tc in &pending_calls {
                    let is_team = tc.function.name == "spawn_agent_team";
                    let args_preview = truncate(&tc.function.arguments, 150);

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

                    // Show abbreviated result
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
                // Final answer
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
    eprintln!("    {}  — Clear conversation history", style("/clear").cyan());
    eprintln!("    {}   — Show this help", style("/help").cyan());
    eprintln!("    {}   — Exit", style("/quit").cyan());
    eprintln!();
    eprintln!("  {}", style("Usage:").bold());
    eprintln!("    agent [OPTIONS] [PROMPT]      One-shot mode");
    eprintln!("    agent [OPTIONS]               Interactive REPL");
    eprintln!("    agent --allow-all-commands     Allow any shell command");
    eprintln!("    agent -p openai -m gpt-4o     Use OpenAI");
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
