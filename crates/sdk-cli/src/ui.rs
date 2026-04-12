//! Reusable TUI rendering primitives: bordered panels, progress bars, tree connectors.

use std::io::{self, Write};

use console::{style, Color};

/// Get terminal width from stderr, fallback to 80.
pub fn term_width() -> usize {
    console::Term::stderr()
        .size_checked()
        .map(|(_, w)| w as usize)
        .unwrap_or(80)
}

/// Border style for panels.
#[derive(Clone, Copy)]
pub enum BorderStyle {
    /// Rounded corners: ╭╮╰╯
    Rounded,
    /// Sharp corners: ┌┐└┘
    Single,
}

impl BorderStyle {
    fn top_left(self) -> char {
        match self {
            Self::Rounded => '╭',
            Self::Single => '┌',
        }
    }
    fn top_right(self) -> char {
        match self {
            Self::Rounded => '╮',
            Self::Single => '┐',
        }
    }
    fn bottom_left(self) -> char {
        match self {
            Self::Rounded => '╰',
            Self::Single => '└',
        }
    }
    fn bottom_right(self) -> char {
        match self {
            Self::Rounded => '╯',
            Self::Single => '┘',
        }
    }
}

/// A bordered panel rendered to stderr.
pub struct Panel {
    title: Option<String>,
    border_style: BorderStyle,
    border_color: Color,
    dim: bool,
    indent: usize,
    max_width: usize,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            title: None,
            border_style: BorderStyle::Rounded,
            border_color: Color::White,
            dim: true,
            indent: 2,
            max_width: 80,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn border_style(mut self, s: BorderStyle) -> Self {
        self.border_style = s;
        self
    }

    pub fn color(mut self, c: Color) -> Self {
        self.border_color = c;
        self
    }

    pub fn dim(mut self, d: bool) -> Self {
        self.dim = d;
        self
    }

    pub fn indent(mut self, n: usize) -> Self {
        self.indent = n;
        self
    }

    fn effective_width(&self) -> usize {
        let tw = term_width();
        let available = tw.saturating_sub(self.indent);
        available.min(self.max_width)
    }

    fn border_char(&self, ch: char) -> String {
        if self.dim {
            style(ch).fg(self.border_color).dim().to_string()
        } else {
            style(ch).fg(self.border_color).to_string()
        }
    }

    fn horizontal(&self, width: usize) -> String {
        let line = "─".repeat(width);
        if self.dim {
            style(line).fg(self.border_color).dim().to_string()
        } else {
            style(line).fg(self.border_color).to_string()
        }
    }

    /// Render a full bordered panel with content lines.
    pub fn render(&self, lines: &[String]) {
        let width = self.effective_width();
        let indent = " ".repeat(self.indent);
        let inner = width.saturating_sub(4); // 2 for "│ " + 2 for " │"
        let bs = self.border_style;

        // Top border
        let top = if let Some(ref title) = self.title {
            let title_display_len = console::measure_text_width(title);
            let fill = inner.saturating_sub(title_display_len + 1); // +1 for space after title
            format!(
                "{}{}{} {} {}{}",
                indent,
                self.border_char(bs.top_left()),
                self.horizontal(1),
                title,
                self.horizontal(fill),
                self.border_char(bs.top_right()),
            )
        } else {
            format!(
                "{}{}{}{}",
                indent,
                self.border_char(bs.top_left()),
                self.horizontal(width.saturating_sub(2)),
                self.border_char(bs.top_right()),
            )
        };
        eprintln!("{}", top);

        // Content lines
        let vbar = self.border_char('│');
        for line in lines {
            let line_len = console::measure_text_width(line);
            let padding = inner.saturating_sub(line_len);
            eprintln!(
                "{}{}  {}{}{}",
                indent,
                vbar,
                line,
                " ".repeat(padding),
                vbar,
            );
        }

        // Bottom border
        eprintln!(
            "{}{}{}{}",
            indent,
            self.border_char(bs.bottom_left()),
            self.horizontal(width.saturating_sub(2)),
            self.border_char(bs.bottom_right()),
        );
    }

    /// Render a compact single-line panel: ╭─ label ── result ─╯
    pub fn render_inline(&self, label: &str, result: &str) {
        let indent = " ".repeat(self.indent);
        let bs = self.border_style;
        let label_len = console::measure_text_width(label);
        let result_len = console::measure_text_width(result);
        let width = self.effective_width();
        let fill = width.saturating_sub(label_len + result_len + 8); // corners + spaces + dashes

        eprintln!(
            "{}{}{} {} {} {} {}",
            indent,
            self.border_char(bs.top_left()),
            self.horizontal(1),
            label,
            self.horizontal(fill.max(1)),
            result,
            self.border_char(bs.bottom_right()),
        );
    }

    /// Render a panel with a horizontal divider between header and footer sections.
    pub fn render_with_divider(&self, header: &[String], footer: &[String]) {
        let width = self.effective_width();
        let indent = " ".repeat(self.indent);
        let inner = width.saturating_sub(4);
        let bs = self.border_style;

        // Top border
        let top = if let Some(ref title) = self.title {
            let title_display_len = console::measure_text_width(title);
            let fill = inner.saturating_sub(title_display_len + 1);
            format!(
                "{}{}{} {} {}{}",
                indent,
                self.border_char(bs.top_left()),
                self.horizontal(1),
                title,
                self.horizontal(fill),
                self.border_char(bs.top_right()),
            )
        } else {
            format!(
                "{}{}{}{}",
                indent,
                self.border_char(bs.top_left()),
                self.horizontal(width.saturating_sub(2)),
                self.border_char(bs.top_right()),
            )
        };
        eprintln!("{}", top);

        let vbar = self.border_char('│');

        // Header
        for line in header {
            let line_len = console::measure_text_width(line);
            let padding = inner.saturating_sub(line_len);
            eprintln!("{}{}  {}{}{}", indent, vbar, line, " ".repeat(padding), vbar);
        }

        // Divider
        eprintln!(
            "{}{}{}{}",
            indent,
            self.border_char('├'),
            self.horizontal(width.saturating_sub(2)),
            self.border_char('┤'),
        );

        // Footer
        for line in footer {
            let line_len = console::measure_text_width(line);
            let padding = inner.saturating_sub(line_len);
            eprintln!("{}{}  {}{}{}", indent, vbar, line, " ".repeat(padding), vbar);
        }

        // Bottom border
        eprintln!(
            "{}{}{}{}",
            indent,
            self.border_char(bs.bottom_left()),
            self.horizontal(width.saturating_sub(2)),
            self.border_char(bs.bottom_right()),
        );
    }
}

/// Render a horizontal progress bar: `▰▰▰▱▱▱▱ 3/7`
pub fn progress_bar(current: usize, total: usize, width: usize) -> String {
    if total == 0 {
        return String::new();
    }
    let filled = (current * width) / total;
    let empty = width.saturating_sub(filled);
    format!(
        "{}{} {}/{}",
        style("▰".repeat(filled)).cyan(),
        style("▱".repeat(empty)).dim(),
        current,
        total,
    )
}

/// Return a tree connector string for tree-style lists.
pub fn tree_connector(is_last: bool) -> &'static str {
    if is_last { "└── " } else { "├── " }
}

/// Overwrite the current stderr line (for progress updates).
pub fn overwrite_line(content: &str) {
    eprint!("\r\x1b[K{}", content);
    let _ = io::stderr().flush();
}
