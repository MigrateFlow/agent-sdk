use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use console::style;
use uuid::Uuid;

use sdk_agent::subagent::{builtin_subagents, SubAgentRegistry};
use sdk_core::config::{LlmConfig, LlmProvider};
use sdk_core::events::AgentEvent;
use sdk_core::memory::MemoryStore;
use sdk_core::storage::AgentPaths;
use sdk_core::traits::tool::Tool;
use sdk_core::types::agent_mode::{AgentMode, PLAN_MODE_READONLY_TOOLS, plan_mode_system_suffix};
use sdk_core::types::chat::ChatMessage;
use sdk_core::types::ultra_plan::{UltraPlanState, allowed_tools_for_phase, phase_system_suffix};

use sdk_cli::cache_commands::CacheState;
use sdk_cli::permission::PermissionState;
use sdk_cli::commands::{CommandContext, CommandOutcome, SlashCommandRegistry};
use sdk_cli::compaction::compact_conversation;
use sdk_cli::mode_tools::ModeState;
use sdk_cli::display::{format_token_count, print_task_list};
use sdk_cli::event_handler::run_event_handler;
use sdk_cli::format::{print_usage, print_welcome};
use sdk_cli::mcp::load_mcp_tools;
use sdk_cli::ndjson::{emit_ndjson, NdjsonEvent};
use sdk_cli::session::{default_session_path, load_session, save_session_full, CliTask};
use sdk_cli::session_manager::SessionManager;
use sdk_cli::turn::run_turn;

// ─── CLI args ────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "agent", about = "General-purpose AI agent CLI", version)]
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

// ─── Input ───────────────────────────────────────────────────────────────────

fn read_input() -> io::Result<String> {
    let stdin = io::stdin();
    let mut full = String::new();
    loop {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        if line.is_empty() { return Ok(full); }
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

fn ctrlc_handler(interrupt: Arc<AtomicBool>) {
    tokio::spawn(async move {
        loop {
            tokio::signal::ctrl_c().await.ok();
            if interrupt.load(Ordering::Relaxed) {
                eprintln!("\n  {}", style("Force exit.").red());
                std::process::exit(130);
            }
            interrupt.store(true, Ordering::Relaxed);
        }
    });
}

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("sdk_cli=warn".parse().unwrap())
                .add_directive("sdk_core=warn".parse().unwrap())
                .add_directive("sdk_agent=warn".parse().unwrap()),
        )
        .with_target(false)
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let work_dir = match std::fs::canonicalize(&cli.dir) {
        Ok(p) => p,
        Err(e) => {
            if cli.json {
                emit_ndjson(&NdjsonEvent::Failed { error: format!("Working directory '{}' not found: {}", cli.dir.display(), e) });
                return Ok(());
            } else {
                return Err(e.into());
            }
        }
    };

    // ── Provider detection ──
    let provider = cli.provider.as_deref().and_then(LlmProvider::parse).unwrap_or_else(LlmProvider::detect);
    let model = cli.model.unwrap_or_else(|| {
        LlmConfig { provider: provider.clone(), model: String::new(), ..LlmConfig::default() }.resolve_model()
    });
    let llm_config = LlmConfig { provider, model: model.clone(), max_tokens: cli.max_tokens, ..LlmConfig::default() };
    let llm_client = sdk_llm::create_client(&llm_config)?;

    // ── Event channel for team monitoring ──
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let event_json_mode = cli.json;
    tokio::spawn(run_event_handler(event_rx, event_json_mode));

    // ── System prompt ──
    let mut system_prompt = cli.system.unwrap_or_else(|| sdk_core::prompts::cli_system_prompt(&work_dir));

    // ── Paths and memory store ──
    let paths = AgentPaths::for_work_dir(&work_dir)?;
    let memory_store: Option<Arc<MemoryStore>> = {
        let memory_dir = paths.project_memory_dir();
        match MemoryStore::new(memory_dir) {
            Ok(store) => {
                if let Ok(Some(index)) = store.load_index() {
                    system_prompt.push_str(&sdk_core::prompts::memory_context_section(&index));
                }
                Some(Arc::new(store))
            }
            Err(_) => None,
        }
    };

    let mut session_path = cli.session.unwrap_or_else(|| default_session_path(&work_dir));
    let cli_agent_id = Uuid::new_v4();

    // ── Subagent registry ──
    let subagent_registry = {
        let mut reg = SubAgentRegistry::new();
        for def in builtin_subagents() { reg.register(def); }
        Arc::new(reg)
    };

    // ── Ctrl+C handling ──
    let interrupt = Arc::new(AtomicBool::new(false));
    { let i = interrupt.clone(); ctrlc_handler(i); }

    // ── MCP servers ──
    let mcp_tools: Vec<Arc<dyn Tool>> = load_mcp_tools(&work_dir, cli.json).await;

    // ── One-shot mode ──
    let one_shot_prompt = if let Some(ref path) = cli.prompt_file {
        Some(std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read prompt file {}: {}", path.display(), e))?)
    } else if !cli.prompt.is_empty() {
        Some(cli.prompt.join(" "))
    } else {
        None
    };

    let mut permission_state = PermissionState::new(cli.allow_all_commands);

    if let Some(prompt) = one_shot_prompt {
        let mut messages = vec![ChatMessage::system(&system_prompt)];
        let tasks = Arc::new(Mutex::new(Vec::<CliTask>::new()));
        let tool_filter = cli.tools.as_deref();
        let one_shot_mode = ModeState::new(AgentMode::Normal, None);

        let result = run_turn(
            &mut messages, &prompt, &llm_client, &llm_config, &work_dir,
            cli.max_iterations, cli.allow_all_commands, Some(event_tx), tasks,
            interrupt, subagent_registry, cli.json, tool_filter, &mcp_tools,
            &paths, memory_store, cli_agent_id, Some(one_shot_mode),
            &mut permission_state,
        ).await;

        match result {
            Ok(stats) => { if !cli.json { print_usage(stats.tokens, stats.tool_calls, stats.iterations, stats.duration); } }
            Err(e) => {
                if cli.json { emit_ndjson(&NdjsonEvent::Failed { error: e.to_string() }); }
                else { return Err(e); }
            }
        }
        return Ok(());
    }

    // ── Interactive REPL ──
    let tool_count = 18 + mcp_tools.len() + if memory_store.is_some() { 5 } else { 0 };
    let provider_name = match llm_config.provider { LlmProvider::Claude => "claude", LlmProvider::OpenAi => "openai" };
    print_welcome(&model, provider_name, &work_dir, tool_count, mcp_tools.len(), None, Some(session_path.as_path()));

    let slash_registry = SlashCommandRegistry::builtin();
    let tasks = Arc::new(Mutex::new(Vec::<CliTask>::new()));
    let mut agent_mode = AgentMode::Normal;
    let mut ultra_plan_state: Option<UltraPlanState> = None;

    // Shared mode state — lets LLM-callable tools mutate agent_mode / ultra_plan
    let mode_state = ModeState::new(agent_mode.clone(), ultra_plan_state.clone());

    // ── Cache state ──
    let file_cache = Arc::new(sdk_core::cache::FileStateCache::new());
    let cache_state = {
        let stats_path = paths.project_state_dir().join("stats.jsonl");
        Arc::new(CacheState { file_cache: file_cache.clone(), stats_path })
    };

    let mut messages = match load_session(&session_path, &system_prompt) {
        Some(session) => {
            let n = session.messages.len();
            agent_mode = session.mode;
            ultra_plan_state = session.ultra_plan;
            { let mut current = tasks.lock().expect("task list mutex poisoned"); *current = session.tasks; }
            eprintln!("   {} Session restored ({} messages)", style("↻").green(), style(n).dim());
            if agent_mode == AgentMode::Plan {
                eprintln!("   {} {}", style("mode:").dim(), style("plan (read-only)").yellow());
            }
            if let Some(ref state) = ultra_plan_state {
                eprintln!("   {} {}", style("mode:").dim(), style(format!("ultraplan ({})", state.phase)).yellow());
            }
            let current = tasks.lock().expect("task list mutex poisoned").clone();
            if !current.is_empty() { eprintln!(); print_task_list(&current); }
            eprintln!();
            session.messages
        }
        None => vec![ChatMessage::system(&system_prompt)],
    };

    // Sync restored session mode into shared state
    {
        *mode_state.agent_mode.lock().expect("mode lock") = agent_mode.clone();
        *mode_state.ultra_plan.lock().expect("ultra lock") = ultra_plan_state.clone();
    }

    let mut session_tokens = 0u64;
    let mut session_tool_calls = 0usize;
    let mut session_turns = 0usize;

    // ── Session PID tracking ──
    let session_id = SessionManager::session_id_from_path(&session_path);
    if let Some(sessions_dir) = session_path.parent() {
        let _ = SessionManager::register_pid(sessions_dir, &session_id);
        let interrupted = SessionManager::detect_interrupted(sessions_dir);
        if !interrupted.is_empty() {
            eprintln!("   {} {} interrupted session(s) detected (use /sessions to view)", style("!").yellow(), interrupted.len());
        }
    }

    let project_name = work_dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "agent".to_string());

    loop {
        let mode_indicator = if ultra_plan_state.is_some() {
            let phase = &ultra_plan_state.as_ref().unwrap().phase;
            format!(" {}", style(format!("[{}]", phase)).yellow())
        } else {
            match agent_mode { AgentMode::Plan => format!(" {}", style("[plan]").yellow()), AgentMode::Normal => String::new() }
        };
        eprint!("{}{} {} ", style(&project_name).dim(), mode_indicator, style(">").cyan().bold());
        io::stderr().flush()?;

        let input = read_input()?;
        let input = input.trim().to_string();
        if input.is_empty() { continue; }

        // ── Slash commands ──
        if input.starts_with('/') {
            let mut ctx = CommandContext {
                messages: &mut messages, tasks: tasks.clone(), paths: &paths,
                session_path: session_path.clone(), system_prompt: &system_prompt,
                total_tokens: &mut session_tokens, tool_calls: &mut session_tool_calls,
                turns: &mut session_turns, agent_mode: &mut agent_mode,
                cache_state: Some(cache_state.clone()), ultra_plan: &mut ultra_plan_state,
            };

            match slash_registry.dispatch(&input, &mut ctx).await {
                Ok(Some(CommandOutcome::Quit)) => break,
                Ok(Some(CommandOutcome::Clear)) => {
                    eprintln!("  {} {}", style("✓").green(), style("Conversation cleared").dim());
                    eprintln!(); continue;
                }
                Ok(Some(CommandOutcome::Compact)) => continue,
                Ok(Some(CommandOutcome::Output(text))) => { eprintln!("{}", text); continue; }
                Ok(Some(CommandOutcome::Continue)) => continue,
                Ok(Some(CommandOutcome::SessionSwitch { path })) => {
                    let old_id = SessionManager::session_id_from_path(&session_path);
                    if let Some(dir) = session_path.parent() { SessionManager::cleanup_pid(dir, &old_id); }
                    messages = match load_session(&path, &system_prompt) {
                        Some(session) => { let mut current = tasks.lock().expect("task list mutex poisoned"); *current = session.tasks; session.messages }
                        None => vec![ChatMessage::system(&system_prompt)],
                    };
                    session_path = path;
                    session_tokens = 0; session_tool_calls = 0; session_turns = 0;
                    let new_id = SessionManager::session_id_from_path(&session_path);
                    if let Some(dir) = session_path.parent() { let _ = SessionManager::register_pid(dir, &new_id); }
                    eprintln!("  {} Session switched ({} messages)", style("ok").green(), style(messages.len()).dim());
                    eprintln!(); continue;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("  {} {}  (type {} for help)", style("?").yellow(), style(e.to_string()).white(), style("/help").cyan());
                    eprintln!(); continue;
                }
            }
        }

        // Plan mode: inject system prompt suffix
        if agent_mode == AgentMode::Plan {
            if let Some(ChatMessage::System { content }) = messages.first_mut() {
                if !content.contains("PLAN MODE ACTIVE") { content.push_str(plan_mode_system_suffix()); }
            }
        } else if let Some(ChatMessage::System { content }) = messages.first_mut() {
            if let Some(idx) = content.find("\n\n# PLAN MODE ACTIVE") { content.truncate(idx); }
        }

        // UltraPlan: apply phase suffix
        if let Some(ChatMessage::System { content }) = messages.first_mut() {
            if let Some(idx) = content.find("\n# ULTRAPLAN:") { content.truncate(idx); }
            if let Some(ref state) = ultra_plan_state { content.push_str(phase_system_suffix(&state.phase)); }
        }

        // Compute effective tool filter — always include mode-transition tools
        // so the agent can exit the mode it's in.
        let mode_tools: &[&str] = &[
            "enter_plan_mode", "exit_plan_mode",
            "enter_ultraplan", "advance_ultraplan_phase", "exit_ultraplan",
        ];
        let plan_filter: Option<Vec<String>> = if ultra_plan_state.is_some() {
            let tools = allowed_tools_for_phase(&ultra_plan_state.as_ref().unwrap().phase);
            if tools.is_empty() {
                None
            } else {
                let mut all: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
                for mt in mode_tools { all.push(mt.to_string()); }
                Some(all)
            }
        } else if agent_mode == AgentMode::Plan {
            let mut all: Vec<String> = PLAN_MODE_READONLY_TOOLS.iter().map(|s| s.to_string()).collect();
            for mt in mode_tools { all.push(mt.to_string()); }
            Some(all)
        } else { None };

        // ── Auto-compaction before turn: compact when context approaches limit ──
        {
            let compaction_cfg = sdk_core::config::CompactionConfig::default();
            let max_ctx = llm_config.max_tokens.max(sdk_core::config::AgentConfig::default().max_context_tokens);
            let estimated_tokens: usize = messages
                .iter()
                .map(|m| m.char_len() / compaction_cfg.chars_per_token.max(1))
                .sum();
            let token_threshold = (max_ctx as f64 * compaction_cfg.proactive_compaction_ratio) as usize;
            let needs_compaction = estimated_tokens > token_threshold
                || messages.len() > compaction_cfg.proactive_message_threshold;
            if needs_compaction {
                let before = messages.len();
                let (freed, strategy) = compact_conversation(&mut messages);
                if freed > 0 {
                    let after = messages.len();
                    eprintln!(
                        "  {} {}→{} messages ({} freed, {})",
                        style("↻").dim(),
                        before, after,
                        freed,
                        style(strategy).dim(),
                    );
                }
            }
        }

        // Sync local mode → shared state (so tools see slash-command changes)
        {
            *mode_state.agent_mode.lock().expect("mode lock") = agent_mode.clone();
            *mode_state.ultra_plan.lock().expect("ultra lock") = ultra_plan_state.clone();
        }

        let stats = run_turn(
            &mut messages, &input, &llm_client, &llm_config, &work_dir,
            cli.max_iterations, cli.allow_all_commands, Some(event_tx.clone()), tasks.clone(),
            interrupt.clone(), subagent_registry.clone(), false, plan_filter.as_deref(),
            &mcp_tools, &paths, memory_store.clone(), cli_agent_id,
            Some(mode_state.clone()),
            &mut permission_state,
        ).await?;

        // Sync shared state → local mode (so tool-initiated mode changes take effect)
        {
            agent_mode = mode_state.agent_mode.lock().expect("mode lock").clone();
            ultra_plan_state = mode_state.ultra_plan.lock().expect("ultra lock").clone();
        }

        session_tokens += stats.tokens;
        session_tool_calls += stats.tool_calls;
        session_turns += 1;

        print_usage(stats.tokens, stats.tool_calls, stats.iterations, stats.duration);

        if let Err(e) = save_session_full(&session_path, &messages, &tasks.lock().expect("task list mutex poisoned"), ultra_plan_state.as_ref()) {
            eprintln!("  {} session save: {}", style("⚠").yellow(), e);
        }
    }

    // ── Cleanup PID tracking ──
    let final_session_id = SessionManager::session_id_from_path(&session_path);
    if let Some(sessions_dir) = session_path.parent() { SessionManager::cleanup_pid(sessions_dir, &final_session_id); }

    eprintln!();
    eprintln!("  {} {} · {} · {} tool {}",
        style("Session:").dim(),
        style(format!("{} turns", session_turns)).dim(),
        style(format!("{} tokens", format_token_count(session_tokens))).dim(),
        style(session_tool_calls).dim(),
        if session_tool_calls == 1 { "use" } else { "uses" },
    );
    eprintln!();
    Ok(())
}
