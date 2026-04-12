use async_trait::async_trait;
use console::style;
use sdk_core::error::SdkResult;
use sdk_core::types::ultra_plan::{UltraPlanPhase, UltraPlanState, next_phase};

use crate::commands::{CommandCategory, CommandContext, CommandOutcome, SlashCommand};

/// `/ultraplan` -- enter UltraPlan mode at Research phase
pub struct UltraPlanCommand;

#[async_trait]
impl SlashCommand for UltraPlanCommand {
    fn name(&self) -> &str { "ultraplan" }
    fn help(&self) -> &str { "enter structured planning mode (Research -> Design -> Review -> Implement)" }
    fn category(&self) -> CommandCategory { CommandCategory::Planning }

    async fn execute(&self, ctx: &mut CommandContext<'_>, _args: &str) -> SdkResult<CommandOutcome> {
        if ctx.ultra_plan.is_some() {
            return Ok(CommandOutcome::Output(
                format!("  {} Already in UltraPlan mode. Use {} to see current phase.",
                    style("i").blue(), style("/phase").cyan())
            ));
        }

        *ctx.ultra_plan = Some(UltraPlanState::default());
        ctx.save()?;

        let lines = vec![
            format!("{} {}", style("ok").green(), style("UltraPlan activated").bold()),
            String::new(),
            format!("{}", style("Phase 1: Research").cyan().bold()),
            format!("  {} Read and explore the codebase", style("→").dim()),
            format!("  {} Identify all relevant files and patterns", style("→").dim()),
            format!("  {} Type {} to advance to Design", style("→").dim(), style("/nextphase").cyan()),
        ];
        eprintln!();
        crate::ui::Panel::new()
            .title(style("UltraPlan").bold().to_string())
            .color(console::Color::Yellow)
            .indent(2)
            .render(&lines);
        Ok(CommandOutcome::Output(String::new()))
    }
}

/// `/nextphase` -- advance to the next UltraPlan phase
pub struct NextPhaseCommand;

#[async_trait]
impl SlashCommand for NextPhaseCommand {
    fn name(&self) -> &str { "nextphase" }
    fn help(&self) -> &str { "advance to next UltraPlan phase" }
    fn category(&self) -> CommandCategory { CommandCategory::Planning }

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

                // Visual phase transition
                let transition = format!(
                    "  {} {} {} {} {}",
                    style("═══").dim(),
                    style(&old_phase).dim(),
                    style("──▸").cyan(),
                    style(format!("Phase {}: {}", phase_num, phase_name)).cyan().bold(),
                    style("═══").dim(),
                );
                Ok(CommandOutcome::Output(format!(
                    "\n{}\n  {}\n",
                    transition,
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
    fn category(&self) -> CommandCategory { CommandCategory::Planning }

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

        let current_idx = phases.iter().position(|(_, p)| state.phase == *p).unwrap_or(0);

        let mut lines = Vec::new();
        for (idx, (name, phase)) in phases.iter().enumerate() {
            let is_current = state.phase == *phase;
            let is_done = idx < current_idx;
            let (marker, label) = if is_done {
                (style("✓").green().to_string(), style(*name).green().to_string())
            } else if is_current {
                let note = if *phase == UltraPlanPhase::Implement { "full tools" } else { "read-only" };
                (style("●").cyan().to_string(), format!("{} {}", style(*name).cyan().bold(), style(format!("({})", note)).dim()))
            } else {
                (style("○").dim().to_string(), style(*name).dim().to_string())
            };
            lines.push(format!("{} {}", marker, label));
        }
        lines.push(String::new());
        lines.push(crate::ui::progress_bar(current_idx + 1, 4, 8));

        eprintln!();
        crate::ui::Panel::new()
            .title(style("UltraPlan Progress").bold().to_string())
            .color(console::Color::Yellow)
            .indent(2)
            .render(&lines);
        Ok(CommandOutcome::Output(String::new()))
    }
}

/// `/exitultraplan` -- exit UltraPlan mode
pub struct ExitUltraPlanCommand;

#[async_trait]
impl SlashCommand for ExitUltraPlanCommand {
    fn name(&self) -> &str { "exitultraplan" }
    fn help(&self) -> &str { "exit UltraPlan mode" }
    fn category(&self) -> CommandCategory { CommandCategory::Planning }

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
