use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::time::Instant;

use console::style;
use tokio::sync::mpsc;

use sdk_core::events::AgentEvent;

use crate::display::{floor_char_boundary, format_token_count, truncate};
use crate::ndjson::{emit_ndjson, NdjsonEvent};

const COLORS: &[console::Color] = &[
    console::Color::Magenta,
    console::Color::Blue,
    console::Color::Yellow,
    console::Color::Green,
    console::Color::Cyan,
    console::Color::Red,
];

/// Stateful event renderer that tracks agent colors, start times, and active agents.
struct EventRenderer {
    color_map: HashMap<String, console::Color>,
    next_color: usize,
    start_times: HashMap<String, Instant>,
    active_agents: HashSet<String>,
}

impl EventRenderer {
    fn new() -> Self {
        Self {
            color_map: HashMap::new(),
            next_color: 0,
            start_times: HashMap::new(),
            active_agents: HashSet::new(),
        }
    }

    fn agent_color(&mut self, name: &str) -> console::Color {
        *self.color_map.entry(name.to_string()).or_insert_with(|| {
            let c = COLORS[self.next_color % COLORS.len()];
            self.next_color += 1;
            c
        })
    }

    fn name_tag(&mut self, name: &str) -> String {
        let c = self.agent_color(name);
        let display = if name.len() > 16 { &name[..floor_char_boundary(name, 16)] } else { name };
        format!(
            "  {} {}",
            style("│").fg(c),
            style(format!("{:<16}", display)).fg(c).bold(),
        )
    }

    fn elapsed_str(&self, name: &str) -> String {
        if let Some(start) = self.start_times.get(name) {
            let d = start.elapsed();
            if d.as_secs() >= 60 {
                format!(" · {}m{:.0}s", d.as_secs() / 60, d.as_secs() % 60)
            } else if d.as_secs_f64() >= 1.0 {
                format!(" · {:.1}s", d.as_secs_f64())
            } else {
                String::new()
            }
        } else {
            String::new()
        }
    }

    fn handle(&mut self, event: AgentEvent) {
        match event {
            // ── Team lifecycle: only show the header ──
            AgentEvent::TeamSpawned { .. } => {
                // Suppressed -- the Team Plan panel printed by turn.rs is enough
            }

            // Suppressed: teammate spawn/idle/shutdown are pure noise
            AgentEvent::TeammateSpawned { ref name, .. } => {
                self.active_agents.insert(name.clone());
                self.start_times.insert(name.clone(), Instant::now());
                // silent -- shown in Team Plan panel
            }
            AgentEvent::TeammateIdle { ref name, .. } => {
                self.active_agents.remove(name);
                // silent
            }
            AgentEvent::AgentShutdown { ref name, .. } => {
                self.active_agents.remove(name);
                // silent
            }

            // ── Tasks: show start and completion only ──
            AgentEvent::TaskStarted { ref name, ref title, .. } => {
                self.active_agents.insert(name.clone());
                self.start_times.insert(name.clone(), Instant::now());
                let c = self.agent_color(name);
                let tag = self.name_tag(name);
                // Clear any lingering progress line
                eprint!("\r\x1b[K");
                let _ = io::stderr().flush();
                eprintln!("{} {} {}", tag, style("▸").fg(c), style(title).white());
            }
            AgentEvent::TaskCompleted { ref name, tokens_used, tool_calls, .. } => {
                self.active_agents.remove(name);
                let elapsed = self.elapsed_str(name);
                eprint!("\r\x1b[K");
                let _ = io::stderr().flush();
                let tag = self.name_tag(name);
                eprintln!(
                    "{} {} {} · {} tool {}{}",
                    tag,
                    style("✓").green(),
                    style(format!("↓{}", format_token_count(tokens_used))).dim(),
                    tool_calls,
                    if tool_calls == 1 { "use" } else { "uses" },
                    style(&elapsed).dim(),
                );
            }
            AgentEvent::TaskFailed { ref name, ref error, .. } => {
                self.active_agents.remove(name);
                eprint!("\r\x1b[K");
                let _ = io::stderr().flush();
                let tag = self.name_tag(name);
                eprintln!("{} {} {}", tag, style("✗").red(), style(truncate(error, 80)).red());
            }

            // ── Suppressed: per-tool-call events are shown via SubAgentProgress ──
            AgentEvent::ToolCall { .. } => {}
            AgentEvent::ToolResult { .. } => {}
            AgentEvent::Thinking { .. } => {}
            AgentEvent::TaskCreated { .. } => {}
            AgentEvent::TeammateMessage { .. } => {}

            // ── Plan events: keep (rare, meaningful) ──
            AgentEvent::PlanSubmitted { ref name, ref plan_preview, .. } => {
                let tag = self.name_tag(name);
                eprintln!("{} {} {}", tag, style("plan submitted").yellow(), style(truncate(plan_preview, 60)).dim());
            }
            AgentEvent::PlanApproved { ref name, .. } => {
                let tag = self.name_tag(name);
                eprintln!("{} {}", tag, style("plan approved").green());
            }
            AgentEvent::PlanRejected { ref name, ref feedback, .. } => {
                let tag = self.name_tag(name);
                eprintln!("{} {} {}", tag, style("plan rejected").yellow(), style(truncate(feedback, 60)).dim());
            }

            // ── Subagent lifecycle: spawn + single progress line + completion ──
            AgentEvent::SubAgentSpawned { ref name, ref description, .. } => {
                self.active_agents.insert(name.clone());
                self.start_times.insert(name.clone(), Instant::now());
                let c = self.agent_color(name);
                let desc = if description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", truncate(description, 60))
                };
                eprint!("\r\x1b[K");
                let _ = io::stderr().flush();
                eprintln!(
                    "  {} {} {}{}",
                    style("⎿").cyan(),
                    style("Subagent").bold(),
                    style(name).fg(c).bold(),
                    style(desc).dim(),
                );
            }

            // Single in-place progress line (overwrites itself -- no new lines)
            AgentEvent::SubAgentProgress { ref name, iteration, max_turns, ref current_tool, tokens_so_far, .. } => {
                let c = self.agent_color(name);
                let display = if name.len() > 16 { &name[..floor_char_boundary(name, 16)] } else { name.as_str() };
                let tool_info = current_tool.as_deref().map(|t| format!(" · {}", t)).unwrap_or_default();
                eprint!(
                    "\r\x1b[K  {} {} {} {}/{}{}  ↓{}",
                    style("│").fg(c),
                    style(format!("{:<16}", display)).fg(c).bold(),
                    style("◐").fg(c),
                    iteration + 1,
                    max_turns,
                    style(&tool_info).dim(),
                    style(format_token_count(tokens_so_far)).dim(),
                );
                let _ = io::stderr().flush();
            }

            AgentEvent::SubAgentCompleted { ref name, tokens_used, tool_calls, .. } => {
                self.active_agents.remove(name);
                let elapsed = self.elapsed_str(name);
                eprint!("\r\x1b[K");
                let _ = io::stderr().flush();
                let tag = self.name_tag(name);
                eprintln!(
                    "{} {} ↓{} · {} tool {}{}",
                    tag,
                    style("✓").green(),
                    style(format_token_count(tokens_used)).dim(),
                    tool_calls,
                    if tool_calls == 1 { "use" } else { "uses" },
                    style(&elapsed).dim(),
                );
                // No final_content preview -- too spammy
            }
            AgentEvent::SubAgentFailed { ref name, ref error, .. } => {
                self.active_agents.remove(name);
                eprint!("\r\x1b[K");
                let _ = io::stderr().flush();
                let tag = self.name_tag(name);
                eprintln!("{} {} {}", tag, style("✗").red(), style(truncate(error, 80)).red());
            }
            AgentEvent::SubAgentUpdate { .. } => {
                // silent -- progress line is enough
            }

            AgentEvent::MemoryCompacted { ref strategy, messages_before, messages_after, tokens_saved, .. } => {
                eprintln!(
                    "  {} {}→{} messages ({} saved, {})",
                    style("↻").dim(),
                    messages_before,
                    messages_after,
                    format_token_count(tokens_saved),
                    style(strategy).dim(),
                );
            }

            _ => {}
        }
    }
}

/// Run the event handler that renders agent events to stderr (or NDJSON to stdout).
pub async fn run_event_handler(
    mut event_rx: mpsc::UnboundedReceiver<AgentEvent>,
    json_mode: bool,
) {
    let mut renderer = EventRenderer::new();

    while let Some(event) = event_rx.recv().await {
        // JSON mode: emit structured NDJSON, skip stderr
        if json_mode {
            match &event {
                AgentEvent::TeamSpawned { teammate_count } => {
                    emit_ndjson(&NdjsonEvent::TeamSpawned { teammate_count: *teammate_count });
                }
                AgentEvent::SubAgentSpawned { name, description, .. } => {
                    emit_ndjson(&NdjsonEvent::SubagentSpawned { name: name.clone(), description: description.clone() });
                }
                AgentEvent::SubAgentProgress { name, iteration, max_turns, current_tool, tokens_so_far, .. } => {
                    emit_ndjson(&NdjsonEvent::SubagentProgress { name: name.clone(), iteration: *iteration, max_turns: *max_turns, current_tool: current_tool.clone(), tokens_so_far: *tokens_so_far });
                }
                AgentEvent::SubAgentCompleted { name, tokens_used, iterations, tool_calls, .. } => {
                    emit_ndjson(&NdjsonEvent::SubagentCompleted { name: name.clone(), tokens_used: *tokens_used, iterations: *iterations, tool_calls: *tool_calls });
                }
                AgentEvent::SubAgentFailed { name, error, .. } => {
                    emit_ndjson(&NdjsonEvent::SubagentFailed { name: name.clone(), error: error.clone() });
                }
                AgentEvent::TaskStarted { name, title, .. } => {
                    emit_ndjson(&NdjsonEvent::TaskStarted { name: name.clone(), title: title.clone() });
                }
                AgentEvent::TaskCompleted { name, tokens_used, .. } => {
                    emit_ndjson(&NdjsonEvent::TaskCompleted { name: name.clone(), title: String::new(), tokens_used: *tokens_used });
                }
                AgentEvent::TaskFailed { name, error, .. } => {
                    emit_ndjson(&NdjsonEvent::TaskFailed { name: name.clone(), title: String::new(), error: error.clone() });
                }
                _ => {}
            }
            continue;
        }

        // Interactive mode
        renderer.handle(event);
    }
}
