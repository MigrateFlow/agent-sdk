//! Slash commands for inspecting and managing the file-state cache.

use std::sync::Arc;

use async_trait::async_trait;
use console::style;

use crate::cache::{FileStateCache, StatsCache};
use crate::cli::commands::{CommandContext, CommandOutcome, SlashCommand};
use crate::cli::display::format_token_count;
use crate::error::SdkResult;

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

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        eprintln!();
        eprintln!("  {}", style("Cache").bold());

        match ctx.cache_state {
            Some(ref state) => {
                // ── File state cache ──
                let stats = state.file_cache.stats();
                eprintln!();
                eprintln!("  {}", style("File state cache").underlined());
                eprintln!(
                    "    entries: {} / {}",
                    style(stats.entries).white(),
                    style(stats.max_entries).dim(),
                );
                eprintln!(
                    "    memory:  {} / {}",
                    style(format_bytes(stats.total_bytes as u64)).white(),
                    style(format_bytes(stats.max_bytes as u64)).dim(),
                );
                if stats.max_bytes > 0 {
                    let pct = (stats.total_bytes as f64 / stats.max_bytes as f64) * 100.0;
                    eprintln!(
                        "    usage:   {}",
                        style(format!("{:.1}%", pct)).white(),
                    );
                }

                // ── Persisted stats ──
                let entries = StatsCache::load(&state.stats_path);
                if !entries.is_empty() {
                    let summary = StatsCache::summary(&entries);
                    eprintln!();
                    eprintln!("  {}", style("Usage statistics").underlined());
                    eprintln!(
                        "    requests:    {}",
                        style(summary.total_requests).white(),
                    );
                    eprintln!(
                        "    tokens in:   {}",
                        style(format_token_count(summary.total_tokens_in)).white(),
                    );
                    eprintln!(
                        "    tokens out:  {}",
                        style(format_token_count(summary.total_tokens_out)).white(),
                    );
                    eprintln!(
                        "    cache reads: {}",
                        style(format_token_count(summary.total_cache_reads)).white(),
                    );
                    eprintln!(
                        "    est. cost:   {}",
                        style(format!("${:.4}", summary.total_usd)).white(),
                    );
                    eprintln!(
                        "    days:        {}",
                        style(summary.days_tracked).white(),
                    );
                } else {
                    eprintln!();
                    eprintln!(
                        "    {}",
                        style("(no persisted usage stats yet)").dim(),
                    );
                }
            }
            None => {
                eprintln!(
                    "    {}",
                    style("cache state not available").dim(),
                );
            }
        }

        eprintln!();
        Ok(CommandOutcome::Continue)
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

    async fn execute(
        &self,
        ctx: &mut CommandContext<'_>,
        _args: &str,
    ) -> SdkResult<CommandOutcome> {
        match ctx.cache_state {
            Some(ref state) => {
                let before = state.file_cache.stats().entries;
                state.file_cache.clear();
                eprintln!(
                    "  {} cleared {} cached file {}",
                    style("✓").green(),
                    before,
                    if before == 1 { "entry" } else { "entries" },
                );
            }
            None => {
                eprintln!(
                    "  {} {}",
                    style("!").yellow(),
                    style("cache state not available").dim(),
                );
            }
        }
        eprintln!();
        Ok(CommandOutcome::Continue)
    }
}

/// Format a byte count as a human-readable string (e.g. "1.2 MB").
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
