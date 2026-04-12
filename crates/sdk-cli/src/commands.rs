//! Pluggable slash-command framework for the `agent` CLI.
//!
//! The REPL dispatches every line that starts with `/` to a
//! [`SlashCommandRegistry`]. Each command implements the [`SlashCommand`]
//! trait and receives a [`CommandContext`] exposing the mutable pieces of
//! REPL state it may need to read or mutate.
//!
//! The built-ins (`/help`, `/clear`, `/compact`, `/tasks`, `/cost`,
//! `/status`, `/quit`) are registered by [`SlashCommandRegistry::builtin`], and library
//! consumers may register additional commands via
//! [`SlashCommandRegistry::register`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use console::style;

use crate::compaction::compact_conversation;
use crate::display::{display_path, format_bytes, format_token_count, print_task_list, truncate};
use crate::session::CliTask;
use sdk_core::error::{SdkError, SdkResult};
use sdk_core::storage::AgentPaths;
use sdk_core::types::chat::ChatMessage;

/// Outcome returned by a slash command back to the REPL dispatcher.
#[derive(Debug)]
pub enum CommandOutcome {
    /// Continue the REPL loop without any further action.
    Continue,
    /// Signal that the conversation state has been reset.
    Clear,
    /// Signal that the conversation was compacted.
    Compact,
    /// Signal the REPL should exit.
    Quit,
    /// Emit the given text to the user (printed to stderr by the REPL).
    Output(String),
    /// Signal the REPL to switch to a different session file.
    SessionSwitch { path: PathBuf },
}

/// Mutable pieces of REPL state a slash command may read or change.
///
/// Fields are exposed individually rather than behind accessors so custom
/// commands can mutate them directly when needed.
pub struct CommandContext<'a> {
    pub messages: &'a mut Vec<ChatMessage>,
    pub tasks: Arc<Mutex<Vec<CliTask>>>,
    pub paths: &'a AgentPaths,
    pub session_path: PathBuf,
    pub system_prompt: &'a str,
    pub total_tokens: &'a mut u64,
    pub tool_calls: &'a mut usize,
    pub turns: &'a mut usize,
    pub agent_mode: &'a mut sdk_core::AgentMode,
    pub cache_state: Option<Arc<crate::cache_commands::CacheState>>,
    pub ultra_plan: &'a mut Option<sdk_core::types::ultra_plan::UltraPlanState>,
}

impl<'a> CommandContext<'a> {
    /// Persist the current conversation and task list to [`Self::session_path`].
    pub fn save(&self) -> SdkResult<()> {
        let tasks = self
            .tasks
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        crate::session::save_session_with_mode(
            &self.session_path,
            self.messages,
            &tasks,
            self.agent_mode,
        )?;
        Ok(())
    }
}

/// Category for grouping commands in the `/help` display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Core,
    Planning,
    Session,
    Cache,
}

impl CommandCategory {
    fn label(&self) -> &'static str {
        match self {
            Self::Core => "Core",
            Self::Planning => "Planning",
            Self::Session => "Sessions",
            Self::Cache => "Cache",
        }
    }

    fn all() -> &'static [CommandCategory] {
        &[
            CommandCategory::Core,
            CommandCategory::Planning,
            CommandCategory::Session,
            CommandCategory::Cache,
        ]
    }
}

/// Pluggable command trait.
///
/// Implementations must be `Send + Sync` because the registry stores them in
/// an `Arc<dyn SlashCommand>`.
#[async_trait]
pub trait SlashCommand: Send + Sync {
    /// Command name **without** the leading `/`, e.g. `"help"`.
    fn name(&self) -> &str;

    /// One-line help string shown by `/help`.
    fn help(&self) -> &str;

    /// Optional aliases (also without leading `/`). Default: empty.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Category for grouping in `/help`. Default: Core.
    fn category(&self) -> CommandCategory {
        CommandCategory::Core
    }

    /// Execute the command. `args` is the substring after the command name,
    /// already trimmed of surrounding whitespace.
    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> SdkResult<CommandOutcome>;
}

/// Registry of slash commands.
///
/// Dispatch is a linear scan over the registered commands; commands are
/// matched by their `name()` and any `aliases()`.
pub struct SlashCommandRegistry {
    commands: Vec<Arc<dyn SlashCommand>>,
}

impl SlashCommandRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Register a custom command. Later registrations override earlier ones
    /// if they share a name/alias — the first match during dispatch wins, so
    /// insert in priority order.
    pub fn register(&mut self, cmd: Arc<dyn SlashCommand>) {
        self.commands.push(cmd);
    }

    /// Build a registry containing the built-in REPL commands.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(HelpCommand));
        r.register(Arc::new(ClearCommand));
        r.register(Arc::new(CompactCommand));
        r.register(Arc::new(TasksCommand));
        r.register(Arc::new(CostCommand));
        r.register(Arc::new(StatusCommand));
        r.register(Arc::new(crate::session_commands::SessionsCommand));
        r.register(Arc::new(crate::session_commands::ResumeCommand));
        r.register(Arc::new(crate::session_commands::SessionDescribeCommand));
        r.register(Arc::new(crate::plan_commands::PlanCommand));
        r.register(Arc::new(crate::plan_commands::ExitPlanCommand));
        r.register(Arc::new(crate::cache_commands::CacheCommand));
        r.register(Arc::new(crate::cache_commands::CacheClearCommand));
        r.register(Arc::new(crate::ultra_plan_commands::UltraPlanCommand));
        r.register(Arc::new(crate::ultra_plan_commands::NextPhaseCommand));
        r.register(Arc::new(crate::ultra_plan_commands::PhaseCommand));
        r.register(Arc::new(crate::ultra_plan_commands::ExitUltraPlanCommand));
        r.register(Arc::new(QuitCommand));
        r
    }

    /// Iterate over registered commands in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn SlashCommand>> {
        self.commands.iter()
    }

    /// Dispatch a raw input line. Returns:
    ///
    /// - `Ok(Some(outcome))` if the line starts with `/` and matched a known
    ///   command.
    /// - `Ok(None)` if the line does not start with `/` (caller should treat
    ///   it as a regular prompt).
    /// - `Err(SdkError::Config)` if the line starts with `/` but no command
    ///   matched; the caller should surface this to the user.
    pub async fn dispatch(
        &self,
        input: &str,
        ctx: &mut CommandContext<'_>,
    ) -> SdkResult<Option<CommandOutcome>> {
        let trimmed = input.trim();
        let Some(rest) = trimmed.strip_prefix('/') else {
            return Ok(None);
        };

        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (rest, ""),
        };

        for cmd in &self.commands {
            if cmd.name() == name || cmd.aliases().iter().any(|a| *a == name) {
                // `/help` needs a handle back to the registry to enumerate
                // commands, which the trait does not expose. Short-circuit
                // here so HelpCommand::execute stays trivial.
                if cmd.name() == "help" {
                    return Ok(Some(CommandOutcome::Output(self.help_text())));
                }
                return Ok(Some(cmd.execute(ctx, args).await?));
            }
        }

        Err(SdkError::Config(format!("Unknown slash command: /{}", name)))
    }

    /// Render the help text shown by `/help`, grouped by category.
    pub fn help_text(&self) -> String {
        let longest = self
            .commands
            .iter()
            .map(|c| c.name().len() + 1) // + '/'
            .max()
            .unwrap_or(0);

        let mut all_lines = Vec::new();

        for (cat_idx, cat) in CommandCategory::all().iter().enumerate() {
            let cmds_in_cat: Vec<_> = self
                .commands
                .iter()
                .filter(|c| c.category() == *cat)
                .collect();
            if cmds_in_cat.is_empty() {
                continue;
            }

            if cat_idx > 0 {
                all_lines.push(String::new());
            }
            all_lines.push(format!("{}", style(cat.label()).bold()));
            for cmd in &cmds_in_cat {
                let label = format!("/{}", cmd.name());
                let padded = format!("{:width$}", label, width = longest);
                all_lines.push(format!(
                    "  {}  {}",
                    style(padded).cyan(),
                    cmd.help(),
                ));
            }
        }

        all_lines.push(String::new());
        all_lines.push(format!("{}", style("Tips").bold()));
        all_lines.push(format!(
            "  End a line with {} for multi-line input",
            style("\\").cyan()
        ));
        all_lines.push(format!(
            "  {} to interrupt, press twice to force-quit",
            style("Ctrl+C").cyan()
        ));
        all_lines.push(format!(
            "  {} for one-shot mode",
            style("agent \"your prompt\"").cyan()
        ));

        // Render via panel to stderr directly, return empty string
        eprintln!();
        crate::ui::Panel::new()
            .title(style("Commands").bold().to_string())
            .color(console::Color::Cyan)
            .indent(2)
            .render(&all_lines);
        String::new()
    }
}

impl Default for SlashCommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Built-in commands ──────────────────────────────────────────────────────

/// `/help` — list registered commands.
pub struct HelpCommand;

#[async_trait]
impl SlashCommand for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }

    fn help(&self) -> &str {
        "show this help"
    }

    async fn execute(
        &self,
        _ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        // The dispatcher short-circuits `/help` and emits
        // `SlashCommandRegistry::help_text()` directly; this body is only
        // reached if someone calls `execute` without going through dispatch.
        Ok(CommandOutcome::Continue)
    }
}

/// `/clear` — reset the conversation back to the system prompt only.
pub struct ClearCommand;

#[async_trait]
impl SlashCommand for ClearCommand {
    fn name(&self) -> &str {
        "clear"
    }

    fn help(&self) -> &str {
        "clear conversation and start fresh"
    }

    fn aliases(&self) -> &[&str] {
        &["new"]
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        *ctx.messages = vec![ChatMessage::system(ctx.system_prompt)];
        if let Ok(mut guard) = ctx.tasks.lock() {
            guard.clear();
        }
        *ctx.total_tokens = 0;
        *ctx.tool_calls = 0;
        *ctx.turns = 0;
        ctx.save()?;
        Ok(CommandOutcome::Clear)
    }
}

/// `/compact` — shrink large messages using the dynamic profile.
pub struct CompactCommand;

#[async_trait]
impl SlashCommand for CompactCommand {
    fn name(&self) -> &str {
        "compact"
    }

    fn help(&self) -> &str {
        "compact context to free up space"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        let (freed, strategy) = compact_conversation(ctx.messages);
        ctx.save()?;
        let msg = format!(
            "  {} Compacted {} messages ({} strategy, {} remaining)",
            style("✓").green(),
            freed,
            style(strategy).dim(),
            ctx.messages.len(),
        );
        eprintln!("{}", msg);
        eprintln!();
        Ok(CommandOutcome::Compact)
    }
}

/// `/tasks` — print the current task list.
pub struct TasksCommand;

#[async_trait]
impl SlashCommand for TasksCommand {
    fn name(&self) -> &str {
        "tasks"
    }

    fn help(&self) -> &str {
        "show current task list"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        let current = ctx
            .tasks
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        print_task_list(&current);
        Ok(CommandOutcome::Continue)
    }
}

/// `/cost` — show recorded cost usage from `cost.jsonl`.
pub struct CostCommand;

#[async_trait]
impl SlashCommand for CostCommand {
    fn name(&self) -> &str {
        "cost"
    }

    fn help(&self) -> &str {
        "show recorded token cost usage"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        print_cost_summary(
            ctx.session_path.as_path(),
            *ctx.total_tokens,
            *ctx.tool_calls,
            *ctx.turns,
        );
        Ok(CommandOutcome::Continue)
    }
}

/// `/status` — show the active session summary.
pub struct StatusCommand;

#[async_trait]
impl SlashCommand for StatusCommand {
    fn name(&self) -> &str {
        "status"
    }

    fn help(&self) -> &str {
        "show session stats & token usage"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        let session_file: &Path = ctx.session_path.as_path();

        // Show current mode
        let mode_str = if ctx.ultra_plan.is_some() {
            let phase = &ctx.ultra_plan.as_ref().unwrap().phase;
            format!("ultraplan ({})", phase)
        } else {
            match ctx.agent_mode {
                sdk_core::AgentMode::Plan => "plan (read-only)".to_string(),
                sdk_core::AgentMode::Normal => "normal".to_string(),
            }
        };

        let mut lines = vec![
            format!(
                "{} · {} · {} tool {} · {} messages",
                style(format!("{} turns", *ctx.turns)).white(),
                style(format!("{} tokens", format_token_count(*ctx.total_tokens))).white(),
                style(*ctx.tool_calls).white(),
                if *ctx.tool_calls == 1 { "use" } else { "uses" },
                style(ctx.messages.len()).dim(),
            ),
            format!(
                "{} {}",
                style("mode:").dim(),
                if mode_str == "normal" { style(mode_str).dim() } else { style(mode_str).yellow() },
            ),
        ];

        // Show cache stats if available
        if let Some(ref cache) = ctx.cache_state {
            let stats = cache.file_cache.stats();
            if stats.entries > 0 {
                lines.push(format!(
                    "{} {} entries, {}",
                    style("cache:").dim(),
                    stats.entries,
                    style(format_bytes(stats.total_bytes as u64)).dim(),
                ));
            }
        }

        eprintln!();
        let title = format!(
            "{} {}",
            style("Session").bold(),
            style(display_path(session_file)).dim(),
        );
        crate::ui::Panel::new().title(title).color(console::Color::Cyan).render(&lines);

        let current = ctx
            .tasks
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if !current.is_empty() {
            eprintln!();
            print_task_list(&current);
        }
        eprintln!();
        Ok(CommandOutcome::Continue)
    }
}

fn print_cost_summary(
    session_path: &Path,
    session_tokens: u64,
    session_tool_calls: usize,
    session_turns: usize,
) {
    let cost_path = session_path
        .parent()
        .map(|p| p.join("cost.jsonl"))
        .unwrap_or_else(|| PathBuf::from("cost.jsonl"));

    let records = match sdk_core::cost::CostTracker::read_all(&cost_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!();
            eprintln!("  {} could not read cost log: {}", style("!").yellow(), e);
            eprintln!();
            return;
        }
    };

    let title = format!(
        "{} {}",
        style("Cost").bold(),
        style(display_path(&cost_path)).dim(),
    );

    if records.is_empty() {
        let lines = vec![
            format!(
                "{} · {} · {} tool {}",
                style(format!("{} turns", session_turns)).white(),
                style(format!("{} tokens", format_token_count(session_tokens))).white(),
                style(session_tool_calls).white(),
                if session_tool_calls == 1 { "use" } else { "uses" },
            ),
            style("(no cost entries yet — start a turn to populate cost.jsonl)").dim().to_string(),
        ];
        eprintln!();
        crate::ui::Panel::new().title(title).color(console::Color::Cyan).render(&lines);
        eprintln!();
        return;
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<28} {:>10} {:>10} {:>10} {:>10} {:>10}",
        style("model").dim(),
        style("in").dim(),
        style("out").dim(),
        style("cache_w").dim(),
        style("cache_r").dim(),
        style("usd").dim(),
    ));

    let mut tot_in = 0u64;
    let mut tot_out = 0u64;
    let mut tot_cw = 0u64;
    let mut tot_cr = 0u64;
    let mut tot_usd = 0.0f64;
    for r in &records {
        tot_in += r.tokens_in;
        tot_out += r.tokens_out;
        tot_cw += r.cache_in;
        tot_cr += r.cache_read;
        tot_usd += r.estimated_usd;
        lines.push(format!(
            "{:<28} {:>10} {:>10} {:>10} {:>10} {:>10.4}",
            truncate(&r.model, 28),
            format_token_count(r.tokens_in),
            format_token_count(r.tokens_out),
            format_token_count(r.cache_in),
            format_token_count(r.cache_read),
            r.estimated_usd,
        ));
    }
    lines.push(format!(
        "{:<28} {:>10} {:>10} {:>10} {:>10} {:>10.4}",
        style("total").bold().to_string(),
        format_token_count(tot_in),
        format_token_count(tot_out),
        format_token_count(tot_cw),
        format_token_count(tot_cr),
        tot_usd,
    ));

    eprintln!();
    crate::ui::Panel::new().title(title).color(console::Color::Cyan).render(&lines);
    eprintln!();
}

/// `/quit` — exit the REPL.
pub struct QuitCommand;

#[async_trait]
impl SlashCommand for QuitCommand {
    fn name(&self) -> &str {
        "quit"
    }

    fn help(&self) -> &str {
        "exit the agent"
    }

    fn aliases(&self) -> &[&str] {
        &["exit", "q"]
    }

    async fn execute(
        &self,
        _ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        Ok(CommandOutcome::Quit)
    }
}
