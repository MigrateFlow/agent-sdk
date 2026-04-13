use std::path::Path;

use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use crate::display::{display_path, floor_char_boundary, format_token_count, truncate};

/// Detect current git branch (returns None if not a git repo).
pub fn git_branch(work_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(work_dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Detect primary project language from manifest files.
fn detect_project_type(work_dir: &Path) -> Option<&'static str> {
    let checks: &[(&str, &str)] = &[
        ("Cargo.toml", "Rust"),
        ("package.json", "JS/TS"),
        ("pyproject.toml", "Python"),
        ("go.mod", "Go"),
        ("Gemfile", "Ruby"),
        ("pom.xml", "Java"),
        ("build.gradle", "Java"),
    ];
    for (file, lang) in checks {
        if work_dir.join(file).exists() {
            return Some(lang);
        }
    }
    None
}

/// Count source files under `work_dir`, skipping common build/vendor dirs.
/// Only counts files matching the detected project language (or all source
/// files if no project type is detected). Returns None if no files found.
fn count_source_files(work_dir: &Path) -> Option<(usize, &'static str)> {
    use std::time::Instant;

    let (match_exts, label): (&[&str], &str) = match detect_project_type(work_dir) {
        Some("Rust") => (&["rs"], ".rs"),
        Some("JS/TS") => (&["ts", "tsx", "js", "jsx"], ".ts/.js"),
        Some("Python") => (&["py"], ".py"),
        Some("Go") => (&["go"], ".go"),
        Some("Java") => (&["java"], ".java"),
        Some("Ruby") => (&["rb"], ".rb"),
        _ => (&["rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb"], "source"),
    };

    let start = Instant::now();
    let skip_dirs: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv", "build", "dist", ".next"];
    let mut count = 0usize;
    let mut stack: Vec<(std::path::PathBuf, usize)> = vec![(work_dir.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > 5 || start.elapsed().as_millis() > 100 {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if ft.is_dir() {
                if !skip_dirs.contains(&name_str.as_ref()) {
                    stack.push((entry.path(), depth + 1));
                }
            } else if ft.is_file() {
                let path = entry.path();
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if match_exts.contains(&ext) {
                        count += 1;
                    }
                }
            }
        }
    }

    if count > 0 { Some((count, label)) } else { None }
}

pub fn print_welcome(
    model: &str,
    provider: &str,
    work_dir: &Path,
    tool_count: usize,
    mcp_count: usize,
    session_id: Option<&str>,
    session_path: Option<&Path>,
) {
    let version = env!("CARGO_PKG_VERSION");
    let branch = git_branch(work_dir);
    let project_name = work_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    // Line 1: title
    let title_line = format!(
        "{} {}",
        style("✻").cyan().bold(),
        style(format!("Agent SDK v{}", version)).bold(),
    );

    // Line 2: model
    let model_line = format!(
        "{} {}",
        style("Model:").dim(),
        model,
    );

    // Line 3: provider, tools, mcp
    let mut provider_parts = vec![
        format!("{}", style(provider).white()),
        format!("{} {}", style("Tools:").dim(), tool_count),
    ];
    if mcp_count > 0 {
        provider_parts.push(format!("{} {}", style("MCP:").dim(), mcp_count));
    }
    let provider_line = format!(
        "{} {}",
        style("Provider:").dim(),
        provider_parts.join(&format!(" {} ", style("·").dim())),
    );

    // Line 4: project, branch, file count
    let mut project_parts = vec![style(project_name).white().to_string()];
    if let Some(ref b) = branch {
        project_parts.push(format!("({})", style(b).green()));
    }
    if let Some((count, ext)) = count_source_files(work_dir) {
        project_parts.push(format!(
            "{} {} {} files",
            style("·").dim(),
            count,
            ext,
        ));
    }
    let project_line = format!(
        "{} {}",
        style("Project:").dim(),
        project_parts.join(" "),
    );

    // Line 5: session
    let session_line = if let Some(id) = session_id {
        let path_str = session_path
            .map(|p| display_path(p))
            .unwrap_or_default();
        if path_str.is_empty() {
            format!("{} {}", style("Session:").dim(), style(id).dim())
        } else {
            format!(
                "{} {} {} {}",
                style("Session:").dim(),
                style(id).dim(),
                style("·").dim(),
                style(path_str).dim(),
            )
        }
    } else {
        String::new()
    };

    let mut header = vec![title_line, model_line, provider_line, project_line];
    if !session_line.is_empty() {
        header.push(session_line);
    }

    let footer = vec![
        style("/help for commands · Ctrl+C to interrupt · Ctrl+C twice to quit").dim().to_string(),
    ];

    eprintln!();
    crate::ui::Panel::new()
        .color(console::Color::Cyan)
        .dim(true)
        .indent(1)
        .render_with_divider(&header, &footer);
    eprintln!();
}

pub fn create_spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
            .template("  {spinner:.dim} {msg:.dim}")
            .unwrap(),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

const SPINNER_VERBS: &[&str] = &[
    "Thinking…",
    "Analyzing…",
    "Reasoning…",
    "Working…",
    "Processing…",
];

/// A spinner that cycles through verb messages and can show a live token count.
pub struct CyclingSpinner {
    pb: ProgressBar,
    cycle_handle: Option<tokio::task::JoinHandle<()>>,
}

impl CyclingSpinner {
    pub fn new() -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
                .template("  {spinner:.dim} {msg:.dim}  {prefix:.dim}")
                .unwrap(),
        );
        pb.set_message(SPINNER_VERBS[0].to_string());
        pb.enable_steady_tick(std::time::Duration::from_millis(80));

        let pb_clone = pb.clone();
        let handle = tokio::spawn(async move {
            let mut idx = 0usize;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                idx += 1;
                pb_clone.set_message(SPINNER_VERBS[idx % SPINNER_VERBS.len()].to_string());
            }
        });

        Self {
            pb,
            cycle_handle: Some(handle),
        }
    }

    /// Update the token count shown next to the spinner.
    pub fn set_tokens(&self, tokens: u64) {
        if tokens > 0 {
            self.pb.set_prefix(format!("↓{}", format_token_count(tokens)));
        }
    }

    /// Clear the spinner and stop the cycling task.
    pub fn finish_and_clear(&self) {
        self.pb.finish_and_clear();
        if let Some(ref h) = self.cycle_handle {
            h.abort();
        }
    }
}

impl Drop for CyclingSpinner {
    fn drop(&mut self) {
        if let Some(h) = self.cycle_handle.take() {
            h.abort();
        }
    }
}

/// Format a tool call for display (Claude Code style).
pub fn format_tool_label(tool_name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();

    match tool_name {
        "read_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("{} {}", style("Read").bold(), style(path).cyan())
        }
        "write_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("{} {}", style("Write").bold(), style(path).cyan())
        }
        "list_directory" => {
            let path = arg_str(&args, "path").unwrap_or(".");
            format!("{} {}", style("List").bold(), style(path).cyan())
        }
        "search_files" => {
            let file_pat = arg_str(&args, "file_pattern");
            let content_pat = arg_str(&args, "content_pattern");
            match (file_pat, content_pat) {
                (Some(fp), Some(cp)) => {
                    format!("{} {} for {}", style("Search").bold(), style(fp).cyan(), style(format!("\"{}\"", cp)).white())
                }
                (Some(fp), None) => {
                    format!("{} {}", style("Search").bold(), style(fp).cyan())
                }
                (None, Some(cp)) => {
                    format!("{} {}", style("Search").bold(), style(format!("\"{}\"", cp)).white())
                }
                _ => format!("{}", style("Search").bold()),
            }
        }
        "web_search" => {
            let query = arg_str(&args, "query").unwrap_or("web");
            format!("{} \"{}\"", style("Web Search").bold(), style(query).white())
        }
        "run_command" => {
            let cmd = arg_str(&args, "command").unwrap_or("?");
            let short = if cmd.len() > 80 { &cmd[..floor_char_boundary(cmd, 80)] } else { cmd };
            format!("{}", style(format!("$ {}", short)).white())
        }
        "agent" => {
            let preset = arg_str(&args, "preset").unwrap_or("agent");
            let bg = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
            if bg {
                format!("{} {} {}", style("Agent").bold(), style(preset).cyan().bold(), style("(background)").dim())
            } else {
                format!("{} {}", style("Agent").bold(), style(preset).cyan().bold())
            }
        }
        "edit_file" => {
            let path = arg_str(&args, "path").unwrap_or("?");
            format!("{} {}", style("Edit").bold(), style(path).cyan())
        }
        "glob" => {
            let pattern = arg_str(&args, "pattern").unwrap_or("?");
            format!("{} {}", style("Glob").bold(), style(pattern).cyan())
        }
        "grep" => {
            let pattern = arg_str(&args, "pattern").unwrap_or("?");
            let mode = arg_str(&args, "output_mode").unwrap_or("files_with_matches");
            format!("{} {} ({})", style("Grep").bold(), style(format!("\"{}\"", pattern)).white(), mode)
        }
        "todo_write" => format!("{}", style("Todo").bold()),
        "update_task_list" => format!("{}", style("Update Task List").bold()),
        "read_memory" => {
            let key = arg_str(&args, "key").unwrap_or("?");
            format!("{} {}", style("Read Memory").bold(), style(key).cyan())
        }
        "write_memory" => {
            let key = arg_str(&args, "key").unwrap_or("?");
            format!("{} {}", style("Write Memory").bold(), style(key).cyan())
        }
        "list_memory" => {
            let prefix = arg_str(&args, "prefix").unwrap_or("");
            if prefix.is_empty() {
                format!("{}", style("List Memory").bold())
            } else {
                format!("{} {}", style("List Memory").bold(), style(prefix).cyan())
            }
        }
        "search_memory" => {
            let query = arg_str(&args, "query").unwrap_or("?");
            format!("{} \"{}\"", style("Search Memory").bold(), style(query).white())
        }
        "delete_memory" => {
            let key = arg_str(&args, "key").unwrap_or("?");
            format!("{} {}", style("Delete Memory").bold(), style(key).cyan())
        }
        _ => {
            let name = humanize(tool_name);
            format!("{}", style(name).bold())
        }
    }
}

/// Format a tool result preview line.
pub fn format_result_preview(tool_name: &str, result: &str) -> String {
    let val: serde_json::Value = serde_json::from_str(result).unwrap_or_default();

    if let Some(err) = val["error"].as_str() {
        return format!("{} {}", style("✗").red(), style(truncate(err, 80)).red());
    }

    match tool_name {
        "read_file" => {
            let lines = val["lines"].as_u64().unwrap_or(0);
            let lines_returned = val["lines_returned"].as_u64().unwrap_or(lines);
            if lines_returned < lines {
                format!("{} lines (showing {})", lines, lines_returned)
            } else {
                format!("{} lines", lines)
            }
        }
        "write_file" => {
            let written = val["lines_written"].as_u64().unwrap_or(0);
            let bytes = val["bytes_written"].as_u64().unwrap_or(0);
            format!("{} lines · {} bytes written", written, bytes)
        }
        "list_directory" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} items", count)
        }
        "search_files" => {
            if let Some(n) = val["files_with_matches"].as_u64() {
                format!("{} files matched", n)
            } else if let Some(n) = val["total_matches"].as_u64() {
                format!("{} matches", n)
            } else {
                "done".to_string()
            }
        }
        "web_search" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} results", count)
        }
        "run_command" => {
            let code = val["exit_code"].as_i64().unwrap_or(-1);
            if code == 0 {
                let stdout = val["stdout"].as_str().unwrap_or("");
                let lines = stdout.lines().count();
                format!("{} ({} lines)", style("✓").green(), lines)
            } else {
                let stderr = val["stderr"].as_str().unwrap_or("");
                let first_line = stderr.lines().next().unwrap_or("failed");
                format!("{} exit {} — {}", style("✗").red(), code, truncate(first_line, 60))
            }
        }
        "agent" => {
            let status = val["status"].as_str().unwrap_or("?");
            let name = val["name"].as_str().unwrap_or("agent");
            let tokens = val["total_tokens"].as_u64().unwrap_or(0);
            let tool_calls = val["tool_calls"].as_u64().unwrap_or(0);
            if status == "background" {
                format!("{} launched in background", name)
            } else {
                format!("{} {} · {} tokens · {} tools", name, status, format_token_count(tokens), tool_calls)
            }
        }
        "edit_file" => {
            let replacements = val["replacements_made"].as_u64().unwrap_or(0);
            format!("{} replacement(s)", replacements)
        }
        "glob" => {
            let shown = val["shown"].as_u64().unwrap_or(0);
            let total = val["total_matches"].as_u64().unwrap_or(0);
            if shown < total { format!("{} files (showing {})", total, shown) } else { format!("{} files", total) }
        }
        "grep" => {
            if let Some(n) = val["files_with_matches"].as_u64().or(val["total_matches"].as_u64()) {
                format!("{} files", n)
            } else if let Some(n) = val["total_shown"].as_u64() {
                format!("{} matches", n)
            } else {
                "done".to_string()
            }
        }
        "todo_write" => {
            let count = val["count"].as_u64().unwrap_or(0);
            let completed = val["completed"].as_u64().unwrap_or(0);
            format!("{}/{} completed", completed, count)
        }
        "update_task_list" => {
            let count = val["count"].as_u64().unwrap_or(0);
            format!("{} tasks", count)
        }
        "read_memory" => {
            let bytes = val["content"].as_str().map(|s| s.len()).unwrap_or(0);
            if bytes > 0 { format!("{} bytes", bytes) } else { "not found".to_string() }
        }
        "write_memory" => { format!("{} saved", val["key"].as_str().unwrap_or("?")) }
        "list_memory" => { format!("{} keys", val["keys"].as_array().map(|a| a.len()).unwrap_or(0)) }
        "search_memory" => { format!("{} results", val["results"].as_array().map(|a| a.len()).unwrap_or(0)) }
        "delete_memory" => {
            if val["deleted"].as_bool().unwrap_or(false) { "deleted".to_string() } else { "not found".to_string() }
        }
        _ => truncate(result, 80),
    }
}

pub fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

/// Infer a language hint from file extension for display.
pub fn lang_hint(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => ", Rust",
        "ts" | "tsx" => ", TypeScript",
        "js" | "jsx" => ", JavaScript",
        "py" => ", Python",
        "go" => ", Go",
        "java" => ", Java",
        "rb" => ", Ruby",
        "toml" => ", TOML",
        "json" => ", JSON",
        "yaml" | "yml" => ", YAML",
        "md" => ", Markdown",
        "html" => ", HTML",
        "css" => ", CSS",
        "sh" | "bash" => ", Shell",
        "sql" => ", SQL",
        _ => "",
    }
}

pub fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, part) in name.split('_').filter(|s| !s.is_empty()).enumerate() {
        if i > 0 { out.push(' '); }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() { name.to_string() } else { out }
}

/// Maximum characters kept per tool result before truncation.
/// Reads from [`sdk_core::config::CompactionConfig`] default.
pub fn default_max_tool_result_chars() -> usize {
    sdk_core::config::CompactionConfig::default().max_tool_result_chars
}

pub fn truncate_tool_result(s: &str) -> String {
    let max_chars = default_max_tool_result_chars();
    if s.len() <= max_chars {
        return s.to_string();
    }

    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(content) = val.get_mut("content") {
            if let Some(text) = content.as_str() {
                if text.len() > max_chars - 200 {
                    let limit = floor_char_boundary(text, max_chars - 200);
                    let truncated = format!(
                        "{}…\n\n[truncated: {}/{} chars — use offset to read more]",
                        &text[..limit], limit, text.len()
                    );
                    *content = serde_json::Value::String(truncated);
                    let fallback_end = floor_char_boundary(s, max_chars);
                    return serde_json::to_string(&val)
                        .unwrap_or_else(|_| s[..fallback_end].to_string());
                }
            }
        }
    }

    let end = floor_char_boundary(s, max_chars);
    format!("{}…[truncated: {}/{} chars]", &s[..end], end, s.len())
}

pub fn format_duration(d: std::time::Duration) -> String {
    if d.as_secs() >= 60 {
        format!("{}m{:.0}s", d.as_secs() / 60, d.as_secs() % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Print one-line turn usage stats.
pub fn print_usage(tokens: u64, tool_calls: usize, iterations: usize, duration: std::time::Duration) {
    let dur = format_duration(duration);
    let mut parts: Vec<String> = Vec::new();
    if iterations > 1 {
        parts.push(format!("{} iterations", iterations));
    }
    parts.push(format!("{} tokens", format_token_count(tokens)));
    parts.push(format!("{} tool {}", tool_calls, if tool_calls == 1 { "use" } else { "uses" }));
    parts.push(dur);
    eprintln!("  {}", style(parts.join(" · ")).dim());
    eprintln!();
}
