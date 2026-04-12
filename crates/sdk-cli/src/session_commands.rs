//! Slash commands for session management: `/sessions`, `/resume`, `/describe`.

use async_trait::async_trait;
use console::style;

use crate::commands::{CommandContext, CommandOutcome, SlashCommand};
use crate::display::truncate;
use crate::session_manager::{SessionManager, SessionStatus};
use sdk_core::error::SdkResult;

/// `/sessions` -- list all sessions for the current project.
pub struct SessionsCommand;

#[async_trait]
impl SlashCommand for SessionsCommand {
    fn name(&self) -> &str {
        "sessions"
    }

    fn help(&self) -> &str {
        "list all sessions for this project"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        let sessions_dir = ctx
            .session_path
            .parent()
            .unwrap_or(ctx.session_path.as_path());

        // Clean up stale PID files first so list_sessions reflects accurate status
        SessionManager::detect_interrupted(sessions_dir);

        let sessions = SessionManager::list_sessions(sessions_dir)?;
        let current_id = SessionManager::session_id_from_path(&ctx.session_path);

        eprintln!();
        eprintln!("  {}", style("Sessions").bold());
        eprintln!();

        for session in &sessions {
            let is_current = session.id == current_id
                || session.id.starts_with(&current_id)
                || current_id.starts_with(&session.id);

            let marker = if is_current {
                style("->").cyan()
            } else {
                style("  ").dim()
            };

            let short_id = if session.id.len() > 8 {
                &session.id[..8]
            } else {
                &session.id
            };

            let status_display = match session.status {
                SessionStatus::Active => style("active").green(),
                SessionStatus::Idle => style("idle").dim(),
                SessionStatus::Interrupted => style("interrupted").yellow(),
                SessionStatus::Completed => style("completed").dim(),
            };

            let desc = if session.description.is_empty() {
                "-".to_string()
            } else {
                truncate(&session.description, 40)
            };

            eprintln!(
                "  {} {} {} {} {} {}",
                marker,
                style(short_id).cyan(),
                status_display,
                style(format!("{} turns", session.turn_count)).dim(),
                style(format!("{}k tok", session.token_count / 1000)).dim(),
                style(desc).dim(),
            );
        }

        if sessions.is_empty() {
            eprintln!("    {}", style("No sessions found").dim());
        }

        eprintln!();
        Ok(CommandOutcome::Continue)
    }
}

/// `/resume [id]` -- switch to a different session.
pub struct ResumeCommand;

#[async_trait]
impl SlashCommand for ResumeCommand {
    fn name(&self) -> &str {
        "resume"
    }

    fn help(&self) -> &str {
        "resume a different session (/resume <id>)"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> SdkResult<CommandOutcome> {
        let args = args.trim();
        if args.is_empty() {
            return Ok(CommandOutcome::Output(format!(
                "  {} Usage: /resume <session-id>",
                style("?").yellow()
            )));
        }

        let sessions_dir = ctx
            .session_path
            .parent()
            .unwrap_or(ctx.session_path.as_path());

        let sessions = SessionManager::list_sessions(sessions_dir)?;

        let target = sessions.iter().find(|s| s.id.starts_with(args));

        match target {
            Some(session) => {
                // Save current session before switching
                ctx.save()?;

                let target_path = sessions_dir.join(format!("{}.json", session.id));
                Ok(CommandOutcome::SessionSwitch { path: target_path })
            }
            None => Ok(CommandOutcome::Output(format!(
                "  {} No session matching '{}'",
                style("?").yellow(),
                args
            ))),
        }
    }
}

/// `/describe [text]` -- set a description for the current session.
pub struct SessionDescribeCommand;

#[async_trait]
impl SlashCommand for SessionDescribeCommand {
    fn name(&self) -> &str {
        "describe"
    }

    fn help(&self) -> &str {
        "set description for current session"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> SdkResult<CommandOutcome> {
        let args = args.trim();
        if args.is_empty() {
            return Ok(CommandOutcome::Output(format!(
                "  {} Usage: /describe <text>",
                style("?").yellow()
            )));
        }

        // Update session metadata on disk
        let content = std::fs::read_to_string(&ctx.session_path).unwrap_or_default();
        if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(meta) = val.get_mut("metadata") {
                meta["description"] = serde_json::Value::String(args.to_string());
                meta["updated_at"] =
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339());
            } else {
                let mut meta = crate::session_manager::SessionMetadata::default();
                meta.id = SessionManager::session_id_from_path(&ctx.session_path);
                meta.description = args.to_string();
                val["metadata"] = serde_json::to_value(&meta).unwrap_or_default();
            }

            if let Ok(json) = serde_json::to_string(&val) {
                let _ = std::fs::write(&ctx.session_path, json);
            }
        }

        eprintln!(
            "  {} Session described as: {}",
            style("ok").green(),
            style(args).white(),
        );
        Ok(CommandOutcome::Continue)
    }
}
