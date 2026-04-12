//! CLI-facing primitives shared with the `agent` binary.
//!
//! This module exists so library consumers can register custom slash commands
//! (see [`commands`]) and reuse the CLI session/display helpers that the
//! binary itself relies on.

pub mod commands;
pub mod compaction;
pub mod display;
pub mod session;
pub mod ultra_plan_commands;

pub use commands::{
    ClearCommand, CommandContext, CommandOutcome, CompactCommand, CostCommand, HelpCommand,
    QuitCommand, SlashCommand, SlashCommandRegistry, StatusCommand, TasksCommand,
};
pub use session::{CliSessionData, CliTask};
