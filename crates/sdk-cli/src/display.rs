//! Display helpers shared between the CLI binary and the built-in slash
//! commands.

use std::path::Path;

use console::style;

use crate::session::CliTask;

/// Shorten the home dir to `~` for display.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = path.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

/// Find the largest byte index `<= desired` that lies on a UTF-8 boundary.
pub fn floor_char_boundary(s: &str, desired: usize) -> usize {
    if desired >= s.len() {
        return s.len();
    }
    let mut idx = desired;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Truncate a string to at most `max_len` bytes along a UTF-8 boundary.
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = floor_char_boundary(s, max_len);
        format!("{}…", &s[..end])
    }
}

/// Format a token count with a short suffix (`k`, `M`).
pub fn format_token_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Map a task status to its display symbol and color.
pub fn task_status_display(status: &str) -> (console::StyledObject<&'static str>, console::Color) {
    match status {
        "completed" => (style("✓").green(), console::Color::Green),
        "in_progress" => (style("◐").cyan(), console::Color::Cyan),
        "blocked" => (style("!").red(), console::Color::Red),
        _ => (style("○").dim(), console::Color::White),
    }
}

/// Format a byte count as a human-readable string (e.g. "1.2 MB").
pub fn format_bytes(bytes: u64) -> String {
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

/// Print the visible task list to stderr.
pub fn print_task_list(tasks: &[CliTask]) {
    if tasks.is_empty() {
        return;
    }

    let completed = tasks.iter().filter(|t| t.status == "completed").count();
    let total = tasks.len();

    eprintln!("  {} ({}/{})", style("Tasks").bold(), completed, total);
    for task in tasks.iter() {
        let (symbol, color) = task_status_display(&task.status);
        eprintln!("    {} {}", symbol, style(&task.title).fg(color));
    }
}
