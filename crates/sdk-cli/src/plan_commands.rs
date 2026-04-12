use async_trait::async_trait;
use console::style;
use sdk_core::error::SdkResult;
use sdk_core::types::agent_mode::AgentMode;

use crate::commands::{CommandContext, CommandOutcome, SlashCommand};

/// `/plan` — enter plan mode (read-only exploration).
pub struct PlanCommand;

#[async_trait]
impl SlashCommand for PlanCommand {
    fn name(&self) -> &str {
        "plan"
    }

    fn help(&self) -> &str {
        "enter plan mode (read-only exploration)"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        if *ctx.agent_mode == AgentMode::Plan {
            return Ok(CommandOutcome::Output(format!(
                "  {} Already in plan mode. Use {} to exit.",
                style("i").blue(),
                style("/exitplan").cyan()
            )));
        }

        *ctx.agent_mode = AgentMode::Plan;
        ctx.save()?;

        Ok(CommandOutcome::Output(format!(
            "\n  {} {}\n\n  {}\n  {}\n",
            style("ok").green(),
            style("Plan mode activated").bold(),
            style("Read-only tools only -- explore, analyze, and design.").dim(),
            style("Type /exitplan to return to normal mode.").dim(),
        )))
    }
}

/// `/exitplan` — return to normal mode.
pub struct ExitPlanCommand;

#[async_trait]
impl SlashCommand for ExitPlanCommand {
    fn name(&self) -> &str {
        "exitplan"
    }

    fn help(&self) -> &str {
        "exit plan mode, return to normal"
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        if *ctx.agent_mode != AgentMode::Plan {
            return Ok(CommandOutcome::Output(format!(
                "  {} Not in plan mode.",
                style("i").blue()
            )));
        }

        *ctx.agent_mode = AgentMode::Normal;
        ctx.save()?;

        Ok(CommandOutcome::Output(format!(
            "\n  {} {}\n  {}\n",
            style("ok").green(),
            style("Plan mode deactivated").bold(),
            style("Full tool access restored.").dim(),
        )))
    }
}
