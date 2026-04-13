//! Live status table for parallel agents, rendered in-place on stderr.

use std::collections::HashMap;
use std::io::{self, Write};
use std::time::Instant;

use console::{style, Color};

use crate::display::{floor_char_boundary, format_token_count, AGENT_COLORS};

/// Current state of an agent tracked by the dashboard.
pub enum AgentState {
    Running,
    Completed { tool_calls: usize },
    Failed(String),
}

/// Status of an individual agent in the dashboard.
pub struct AgentStatus {
    pub name: String,
    pub color: Color,
    pub state: AgentState,
    pub iteration: usize,
    pub max_turns: usize,
    pub current_tool: Option<String>,
    pub tokens: u64,
    pub started_at: Instant,
}

/// A live status table renderer for parallel agents.
///
/// When two or more agents are being tracked, the dashboard renders an
/// in-place table to stderr using ANSI escape codes so repeated calls to
/// [`render`](Self::render) overwrite the previous output.
pub struct AgentDashboard {
    agents: HashMap<String, AgentStatus>,
    insertion_order: Vec<String>,
    next_color: usize,
    last_render_lines: usize,
    header_rule: String,
    footer_rule: String,
}

impl AgentDashboard {
    pub fn new() -> Self {
        const WIDTH: usize = 48;
        Self {
            agents: HashMap::new(),
            insertion_order: Vec::new(),
            next_color: 0,
            last_render_lines: 0,
            header_rule: "\u{2500}".repeat(WIDTH - 2),
            footer_rule: "\u{2500}".repeat(WIDTH),
        }
    }

    fn pick_color(&mut self) -> Color {
        let c = AGENT_COLORS[self.next_color % AGENT_COLORS.len()];
        self.next_color += 1;
        c
    }

    /// Register a new agent (on SubAgentSpawned event).
    pub fn add_agent(&mut self, name: &str, _description: &str) {
        if self.agents.contains_key(name) {
            return;
        }
        let color = self.pick_color();
        self.insertion_order.push(name.to_string());
        self.agents.insert(
            name.to_string(),
            AgentStatus {
                name: name.to_string(),
                color,
                state: AgentState::Running,
                iteration: 0,
                max_turns: 0,
                current_tool: None,
                tokens: 0,
                started_at: Instant::now(),
            },
        );
    }

    /// Update progress (on SubAgentProgress event).
    pub fn update_progress(
        &mut self,
        name: &str,
        iteration: usize,
        max_turns: usize,
        current_tool: Option<&str>,
        tokens: u64,
    ) {
        if let Some(status) = self.agents.get_mut(name) {
            status.iteration = iteration;
            status.max_turns = max_turns;
            status.current_tool = current_tool.map(|s| s.to_string());
            status.tokens = tokens;
        }
    }

    /// Mark completed (on SubAgentCompleted event).
    pub fn mark_completed(&mut self, name: &str, tokens: u64, tool_calls: usize) {
        if let Some(status) = self.agents.get_mut(name) {
            status.tokens = tokens;
            status.state = AgentState::Completed { tool_calls };
        }
    }

    /// Mark failed (on SubAgentFailed event).
    pub fn mark_failed(&mut self, name: &str, error: &str) {
        if let Some(status) = self.agents.get_mut(name) {
            status.state = AgentState::Failed(error.to_string());
        }
    }

    /// Render the dashboard to stderr. Uses ANSI escape codes to overwrite
    /// the previous render.
    pub fn render(&mut self) {
        let mut stderr = io::stderr().lock();

        // Move cursor up to overwrite previous render
        if self.last_render_lines > 0 {
            let _ = write!(stderr, "\x1b[{}A", self.last_render_lines);
        }

        // Header
        let _ = write!(stderr, "\r\x1b[K");
        let _ = writeln!(
            stderr,
            "  {} {}",
            style("Agents").bold(),
            style(&self.header_rule).dim(),
        );

        // Per-agent rows
        for key in &self.insertion_order {
            let status = match self.agents.get(key) {
                Some(s) => s,
                None => continue,
            };
            let _ = write!(stderr, "\r\x1b[K");
            let display_name = if status.name.len() > 16 {
                &status.name[..floor_char_boundary(&status.name, 16)]
            } else {
                &status.name
            };

            let token_str = format!("\u{2193}{}", format_token_count(status.tokens));

            match &status.state {
                AgentState::Running => {
                    let tool_info = status
                        .current_tool
                        .as_deref()
                        .map(|t| {
                            let t = if t.len() > 12 {
                                &t[..floor_char_boundary(t, 12)]
                            } else {
                                t
                            };
                            format!("\u{00b7} {}", t)
                        })
                        .unwrap_or_default();
                    let _ = writeln!(
                        stderr,
                        "  {} {:<16} {} {}/{}  {:<16} {}",
                        style("\u{2502}").fg(status.color),
                        style(display_name).fg(status.color).bold(),
                        style("\u{25d0}").fg(status.color),
                        status.iteration + 1,
                        status.max_turns,
                        style(&tool_info).dim(),
                        style(&token_str).dim(),
                    );
                }
                AgentState::Completed { tool_calls } => {
                    let info = format!(
                        "\u{00b7} {} tool {}",
                        tool_calls,
                        if *tool_calls == 1 { "use" } else { "uses" },
                    );
                    let _ = writeln!(
                        stderr,
                        "  {} {:<16} {} {:<20} {}",
                        style("\u{2502}").fg(status.color),
                        style(display_name).fg(status.color).bold(),
                        style("\u{2713}").green(),
                        style(&info).dim(),
                        style(&token_str).dim(),
                    );
                }
                AgentState::Failed(_) => {
                    let _ = writeln!(
                        stderr,
                        "  {} {:<16} {} {:<20} {}",
                        style("\u{2502}").fg(status.color),
                        style(display_name).fg(status.color).bold(),
                        style("\u{2717}").red(),
                        style("failed").red(),
                        style(&token_str).dim(),
                    );
                }
            }
        }

        // Footer rule
        let _ = write!(stderr, "\r\x1b[K");
        let _ = writeln!(stderr, "  {}", style(&self.footer_rule).dim());

        // 1 header + N agents + 1 footer
        self.last_render_lines = 2 + self.insertion_order.len();

        let _ = stderr.flush();
    }

    /// Returns true if all agents are done (completed or failed).
    pub fn all_done(&self) -> bool {
        !self.agents.is_empty()
            && self.agents.values().all(|s| {
                matches!(s.state, AgentState::Completed { .. } | AgentState::Failed(_))
            })
    }

    /// Returns true if any agents are tracked.
    pub fn has_agents(&self) -> bool {
        !self.agents.is_empty()
    }

    /// Returns the number of tracked agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_track_agents() {
        let mut dash = AgentDashboard::new();
        assert!(!dash.has_agents());

        dash.add_agent("explorer", "explores code");
        assert!(dash.has_agents());
        assert_eq!(dash.agent_count(), 1);
        assert!(!dash.all_done());

        dash.add_agent("reviewer", "reviews code");
        assert_eq!(dash.agent_count(), 2);
    }

    #[test]
    fn duplicate_add_is_ignored() {
        let mut dash = AgentDashboard::new();
        dash.add_agent("a", "");
        dash.add_agent("a", "");
        assert_eq!(dash.agent_count(), 1);
    }

    #[test]
    fn progress_updates() {
        let mut dash = AgentDashboard::new();
        dash.add_agent("a", "");
        dash.update_progress("a", 3, 20, Some("glob"), 1200);
        let s = dash.agents.get("a").unwrap();
        assert_eq!(s.iteration, 3);
        assert_eq!(s.max_turns, 20);
        assert_eq!(s.current_tool.as_deref(), Some("glob"));
        assert_eq!(s.tokens, 1200);
    }

    #[test]
    fn completion_and_failure() {
        let mut dash = AgentDashboard::new();
        dash.add_agent("a", "");
        dash.add_agent("b", "");
        assert!(!dash.all_done());

        dash.mark_completed("a", 5000, 12);
        assert!(!dash.all_done());

        dash.mark_failed("b", "timeout");
        assert!(dash.all_done());
    }

    #[test]
    fn all_done_empty_is_false() {
        let dash = AgentDashboard::new();
        assert!(!dash.all_done());
    }

    #[test]
    fn colors_rotate() {
        let mut dash = AgentDashboard::new();
        for i in 0..8 {
            dash.add_agent(&format!("agent-{}", i), "");
        }
        // After 6 unique colors, it should wrap around
        let c0 = dash.agents.get("agent-0").unwrap().color;
        let c6 = dash.agents.get("agent-6").unwrap().color;
        assert_eq!(format!("{:?}", c0), format!("{:?}", c6));
    }
}
