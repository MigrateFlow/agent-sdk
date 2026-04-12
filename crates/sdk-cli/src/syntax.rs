//! Syntax highlighting for code displayed in tool result panels.
//!
//! Uses `syntect` with built-in grammars and themes. Falls back to plain text
//! if highlighting fails for any reason.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::as_24_bit_terminal_escaped;

/// Lazy-loaded syntax set (built-in grammars).
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
/// Lazy-loaded theme set (built-in themes).
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Resolve a syntax definition for the given file path, falling back to plain text.
fn resolve_syntax(file_path: &str) -> &'static SyntaxReference {
    let ss = syntax_set();
    ss.find_syntax_for_file(file_path)
        .ok()
        .flatten()
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Create a highlighter configured for the given file path.
fn new_highlighter(file_path: &str) -> HighlightLines<'static> {
    let ts = theme_set();
    let theme = &ts.themes["base16-ocean.dark"];
    HighlightLines::new(resolve_syntax(file_path), theme)
}

/// Highlight a code string for terminal output.
///
/// Returns the highlighted string with ANSI escape codes.
/// Falls back to plain text if highlighting fails.
pub fn highlight_code(code: &str, file_path: &str) -> String {
    let ss = syntax_set();
    let mut highlighter = new_highlighter(file_path);

    let mut result = String::new();
    for line in code.lines() {
        match highlighter.highlight_line(line, ss) {
            Ok(ranges) => {
                result.push_str(&as_24_bit_terminal_escaped(&ranges[..], false));
                result.push('\n');
            }
            Err(_) => {
                result.push_str(line);
                result.push('\n');
            }
        }
    }
    // Reset terminal colors at end
    result.push_str("\x1b[0m");
    result
}

/// Highlight a single line for inline display.
///
/// Returns the line with ANSI escape codes, or the original line on failure.
pub fn highlight_line(line: &str, file_path: &str) -> String {
    let ss = syntax_set();
    let mut highlighter = new_highlighter(file_path);

    match highlighter.highlight_line(line, ss) {
        Ok(ranges) => {
            let mut out = as_24_bit_terminal_escaped(&ranges[..], false);
            out.push_str("\x1b[0m");
            out
        }
        Err(_) => line.to_string(),
    }
}

/// Highlight multiple lines, preserving cross-line syntax state.
///
/// Each returned string has ANSI codes and a trailing reset.
pub fn highlight_lines(lines: &[&str], file_path: &str) -> Vec<String> {
    let ss = syntax_set();
    let mut highlighter = new_highlighter(file_path);

    lines
        .iter()
        .map(|line| match highlighter.highlight_line(line, ss) {
            Ok(ranges) => {
                let mut out = as_24_bit_terminal_escaped(&ranges[..], false);
                out.push_str("\x1b[0m");
                out
            }
            Err(_) => line.to_string(),
        })
        .collect()
}

/// Format a diff with red/green coloring for removed/added lines.
///
/// Old lines are shown with `- ` prefix in red, new lines with `+ ` prefix in green.
pub fn format_colored_diff(old_text: &str, new_text: &str, _file_path: &str) -> Vec<String> {
    let mut lines = Vec::new();

    for line in old_text.lines() {
        lines.push(format!("\x1b[31m- {}\x1b[0m", line));
    }
    for line in new_text.lines() {
        lines.push(format!("\x1b[32m+ {}\x1b[0m", line));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_code_returns_nonempty_for_rust() {
        let code = "fn main() {\n    println!(\"hello\");\n}";
        let result = highlight_code(code, "test.rs");
        assert!(!result.is_empty());
        assert!(result.contains('\x1b'));
    }

    #[test]
    fn highlight_code_falls_back_for_unknown_extension() {
        let code = "some random text";
        let result = highlight_code(code, "file.unknownext123");
        assert!(result.contains("some random text"));
    }

    #[test]
    fn highlight_line_returns_nonempty() {
        let result = highlight_line("let x = 42;", "test.rs");
        assert!(!result.is_empty());
    }

    #[test]
    fn highlight_lines_preserves_cross_line_state() {
        let lines = vec!["fn main() {", "    let x = 42;", "}"];
        let result = highlight_lines(&lines, "test.rs");
        assert_eq!(result.len(), 3);
        for line in &result {
            assert!(line.contains('\x1b'));
        }
    }

    #[test]
    fn format_colored_diff_basic() {
        let lines = format_colored_diff("old line", "new line", "test.rs");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("- old line"));
        assert!(lines[1].contains("+ new line"));
        assert!(lines[0].contains("\x1b[31m"));
        assert!(lines[1].contains("\x1b[32m"));
    }

    #[test]
    fn highlight_code_handles_empty_input() {
        let result = highlight_code("", "test.py");
        assert!(result.contains("\x1b[0m"));
    }
}
