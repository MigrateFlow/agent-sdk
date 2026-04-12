use async_trait::async_trait;
use console::style;
use crate::error::SdkResult;
use crate::types::ultra_plan::{UltraPlanPhase, UltraPlanState, next_phase};

use crate::cli::commands::{CommandContext, CommandOutcome, SlashCommand};

/// `/ultraplan` -- enter UltraPlan mode at Research phase
pub struct UltraPlanCommand;

#[async_trait]
impl SlashCommand for UltraPlanCommand {
    fn name(&self) -> &str { "ultraplan" }
    fn help(&self) -> &str { "enter structured planning mode (Research -> Design -> Review -> Implement)" }

    async fn execute(&self, ctx: &mut CommandContext<'_>, _args: &str) -> SdkResult<CommandOutcome> {
        if ctx.ultra_plan.is_some() {
            return Ok(CommandOutcome::Output(
                format!("  {} Already in UltraPlan mode. Use {} to see current phase.",
                    style("i").blue(), style("/phase").cyan())
            ));
        }

        *ctx.ultra_plan = Some(UltraPlanState::default());
        ctx.save()?;

        Ok(CommandOutcome::Output(format!(
            "\n  {} {}\n\n  {}\n  {}\n  {}\n  {}\n",
            style("ok").green(),
            style("UltraPlan activated").bold(),
            style("Phase 1: Research").cyan().bold(),
            style("  -> Read and explore the codebase").dim(),
            style("  -> Identify all relevant files and patterns").dim(),
            style("  -> Type /nextphase to advance to Design").dim(),
        )))
    }
}

/// `/nextphase` -- advance to the next UltraPlan phase
pub struct NextPhaseCommand;

#[async_trait]
impl SlashCommand for NextPhaseCommand {
    fn name(&self) -> &str { "nextphase" }
    fn help(&self) -> &str { "advance to next UltraPlan phase" }

    async fn execute(&self, ctx: &mut CommandContext<'_>, _args: &str) -> SdkResult<CommandOutcome> {
        let current_phase = match ctx.ultra_plan.as_ref() {
            Some(s) => s.phase.clone(),
            None => return Ok(CommandOutcome::Output(
                format!("  {} Not in UltraPlan mode. Use {} to start.",
                    style("?").yellow(), style("/ultraplan").cyan())
            )),
        };

        match next_phase(&current_phase) {
            Some(new_phase) => {
                let old_phase = current_phase.to_string();
                let phase_name = new_phase.to_string();
                let phase_num = match new_phase {
                    UltraPlanPhase::Research => 1,
                    UltraPlanPhase::Design => 2,
                    UltraPlanPhase::Review => 3,
                    UltraPlanPhase::Implement => 4,
                };
                let tools_note = if new_phase == UltraPlanPhase::Implement {
                    "Full tool access restored.".to_string()
                } else {
                    "Read-only tools active.".to_string()
                };

                // Update state
                if let Some(ref mut state) = ctx.ultra_plan {
                    state.phase = new_phase;
                }
                ctx.save()?;

                Ok(CommandOutcome::Output(format!(
                    "\n  {} {} (was: {})\n  {}\n",
                    style("ok").green(),
                    style(format!("Phase {}: {}", phase_num, phase_name)).cyan().bold(),
                    style(old_phase).dim(),
                    style(tools_note).dim(),
                )))
            }
            None => {
                Ok(CommandOutcome::Output(
                    format!("  {} Already at the final phase (Implement).", style("i").blue())
                ))
            }
        }
    }
}

/// `/phase` -- show current UltraPlan phase and progress
pub struct PhaseCommand;

#[async_trait]
impl SlashCommand for PhaseCommand {
    fn name(&self) -> &str { "phase" }
    fn help(&self) -> &str { "show current UltraPlan phase" }

    async fn execute(&self, ctx: &mut CommandContext<'_>, _args: &str) -> SdkResult<CommandOutcome> {
        let state = match ctx.ultra_plan.as_ref() {
            Some(s) => s,
            None => return Ok(CommandOutcome::Output(
                format!("  {} Not in UltraPlan mode.", style("i").blue())
            )),
        };

        let phases = [
            ("Research", UltraPlanPhase::Research),
            ("Design", UltraPlanPhase::Design),
            ("Review", UltraPlanPhase::Review),
            ("Implement", UltraPlanPhase::Implement),
        ];

        let mut output = String::new();
        output.push_str(&format!("\n  {}\n\n", style("UltraPlan Progress").bold()));

        for (name, phase) in &phases {
            let is_current = state.phase == *phase;
            let marker = if is_current { style(">").cyan() } else { style(".").dim() };
            let label = if is_current { style(*name).cyan().bold() } else { style(*name).dim() };
            output.push_str(&format!("    {} {}\n", marker, label));
        }
        output.push('\n');

        Ok(CommandOutcome::Output(output))
    }
}

/// `/exitultraplan` -- exit UltraPlan mode
pub struct ExitUltraPlanCommand;

#[async_trait]
impl SlashCommand for ExitUltraPlanCommand {
    fn name(&self) -> &str { "exitultraplan" }
    fn help(&self) -> &str { "exit UltraPlan mode" }

    async fn execute(&self, ctx: &mut CommandContext<'_>, _args: &str) -> SdkResult<CommandOutcome> {
        if ctx.ultra_plan.is_none() {
            return Ok(CommandOutcome::Output(
                format!("  {} Not in UltraPlan mode.", style("i").blue())
            ));
        }

        *ctx.ultra_plan = None;
        ctx.save()?;

        Ok(CommandOutcome::Output(format!(
            "\n  {} {}\n  {}\n",
            style("ok").green(),
            style("UltraPlan deactivated").bold(),
            style("Full tool access restored.").dim(),
        )))
    }
}
