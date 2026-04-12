//! Slash commands for inspecting and managing the file-state cache.

use std::sync::Arc;

use async_trait::async_trait;
use console::style;

use sdk_core::cache::{FileStateCache, StatsCache};
use crate::commands::{CommandCategory, CommandContext, CommandOutcome, SlashCommand};
use crate::display::{format_bytes, format_token_count};
use sdk_core::error::SdkResult;

/// Shared cache state that can be attached to [`CommandContext`].
pub struct CacheState {
    pub file_cache: Arc<FileStateCache>,
    /// Path to the `stats.jsonl` file for loading persisted usage statistics.
    pub stats_path: std::path::PathBuf,
}

/// `/cache` — display cache statistics and efficiency metrics.
pub struct CacheCommand;

#[async_trait]
impl SlashCommand for CacheCommand {
    fn name(&self) -> &str {
        "cache"
    }

    fn help(&self) -> &str {
        "show cache statistics and efficiency"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Cache
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        let mut out = String::new();
        out.push('\n');
        out.push_str(&format!("  {}\n", style("Cache").bold()));

        match ctx.cache_state {
            Some(ref state) => {
                // ── File state cache ──
                let stats = state.file_cache.stats();
                out.push('\n');
                out.push_str(&format!("  {}\n", style("File state cache").underlined()));
                out.push_str(&format!(
                    "    entries: {} / {}\n",
                    style(stats.entries).white(),
                    style(stats.max_entries).dim(),
                ));
                out.push_str(&format!(
                    "    memory:  {} / {}\n",
                    style(format_bytes(stats.total_bytes as u64)).white(),
                    style(format_bytes(stats.max_bytes as u64)).dim(),
                ));
                if stats.max_bytes > 0 {
                    let pct = (stats.total_bytes as f64 / stats.max_bytes as f64) * 100.0;
                    out.push_str(&format!(
                        "    usage:   {}\n",
                        style(format!("{:.1}%", pct)).white(),
                    ));
                }

                // ── Persisted stats ──
                let entries = StatsCache::load(&state.stats_path);
                if !entries.is_empty() {
                    let summary = StatsCache::summary(&entries);
                    out.push('\n');
                    out.push_str(&format!("  {}\n", style("Usage statistics").underlined()));
                    out.push_str(&format!(
                        "    requests:    {}\n",
                        style(summary.total_requests).white(),
                    ));
                    out.push_str(&format!(
                        "    tokens in:   {}\n",
                        style(format_token_count(summary.total_tokens_in)).white(),
                    ));
                    out.push_str(&format!(
                        "    tokens out:  {}\n",
                        style(format_token_count(summary.total_tokens_out)).white(),
                    ));
                    out.push_str(&format!(
                        "    cache reads: {}\n",
                        style(format_token_count(summary.total_cache_reads)).white(),
                    ));
                    out.push_str(&format!(
                        "    est. cost:   {}\n",
                        style(format!("${:.4}", summary.total_usd)).white(),
                    ));
                    out.push_str(&format!(
                        "    days:        {}\n",
                        style(summary.days_tracked).white(),
                    ));
                } else {
                    out.push('\n');
                    out.push_str(&format!(
                        "    {}\n",
                        style("(no persisted usage stats yet)").dim(),
                    ));
                }
            }
            None => {
                out.push_str(&format!(
                    "    {}\n",
                    style("cache state not available").dim(),
                ));
            }
        }

        Ok(CommandOutcome::Output(out))
    }
}

/// `/cache-clear` — clear the in-memory file state cache.
pub struct CacheClearCommand;

#[async_trait]
impl SlashCommand for CacheClearCommand {
    fn name(&self) -> &str {
        "cache-clear"
    }

    fn help(&self) -> &str {
        "clear file state cache"
    }

    fn category(&self) -> CommandCategory {
        CommandCategory::Cache
    }

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        let msg = match ctx.cache_state {
            Some(ref state) => {
                let before = state.file_cache.stats().entries;
                state.file_cache.clear();
                format!(
                    "  {} cleared {} cached file {}",
                    style("✓").green(),
                    before,
                    if before == 1 { "entry" } else { "entries" },
                )
            }
            None => {
                format!(
                    "  {} {}",
                    style("!").yellow(),
                    style("cache state not available").dim(),
                )
            }
        };
        Ok(CommandOutcome::Output(msg))
    }
}
