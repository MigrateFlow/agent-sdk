use std::collections::HashMap;
use std::io::{self, Write};

use console::style;
use tokio::sync::mpsc;

use sdk_core::events::AgentEvent;

use crate::display::{floor_char_boundary, format_token_count, truncate};
use crate::format::{format_result_preview, format_tool_label};
use crate::ndjson::{emit_ndjson, NdjsonEvent};

const COLORS: &[console::Color] = &[
    console::Color::Magenta,
    console::Color::Blue,
    console::Color::Yellow,
    console::Color::Green,
    console::Color::Cyan,
    console::Color::Red,
];

fn agent_color(
    name: &str,
    map: &mut HashMap<String, console::Color>,
    next: &mut usize,
) -> console::Color {
    *map.entry(name.to_string()).or_insert_with(|| {
        let c = COLORS[*next % COLORS.len()];
        *next += 1;
        c
    })
}

fn name_tag(name: &str, color: console::Color) -> String {
    let display = if name.len() > 16 { &name[..floor_char_boundary(name, 16)] } else { name };
    format!("  {} {}", style("│").fg(color), style(format!("{:<16}", display)).fg(color).bold())
}

/// Run the event handler that renders agent events to stderr (or NDJSON to stdout).
pub async fn run_event_handler(
    mut event_rx: mpsc::UnboundedReceiver<AgentEvent>,
    json_mode: bool,
) {
    let mut color_map = HashMap::<String, console::Color>::new();
    let mut next_color = 0usize;

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

        // Interactive mode: render to stderr
        match event {
            AgentEvent::TeamSpawned { teammate_count } => {
                eprintln!();
                eprintln!("  {} {}", style("⎿").cyan(), style(format!("Agent Team ({} teammates)", teammate_count)).cyan().bold());
            }
            AgentEvent::TeammateSpawned { ref name, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("    {} {}", style("⎿").fg(c), style(name).fg(c).bold());
            }
            AgentEvent::TaskStarted { ref name, ref title, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!();
                eprintln!("{} {} {}", name_tag(name, c), style("▸").fg(c), style(title).white());
            }
            AgentEvent::Thinking { ref name, ref content, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{}   {}", name_tag(name, c), style(truncate(content, 80)).dim().italic());
            }
            AgentEvent::ToolCall { ref name, ref tool_name, ref arguments, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                let label = format_tool_label(tool_name, arguments);
                eprintln!("{}   {} {}", name_tag(name, c), style("⎿").fg(c), label);
            }
            AgentEvent::ToolResult { ref name, ref tool_name, ref result_preview, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                let preview = format_result_preview(tool_name, result_preview);
                eprintln!("{}     {}", name_tag(name, c), style(&preview).dim());
            }
            AgentEvent::TaskCompleted { ref name, tokens_used, tool_calls, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {} · {} tool {}", name_tag(name, c), style("✓").green(), style(format!("{} tokens", format_token_count(tokens_used))).dim(), tool_calls, if tool_calls == 1 { "use" } else { "uses" });
            }
            AgentEvent::TaskFailed { ref name, ref error, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {}", name_tag(name, c), style("✗").red(), style(truncate(error, 80)).red());
            }
            AgentEvent::PlanSubmitted { ref name, ref plan_preview, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {}", name_tag(name, c), style("plan submitted").yellow(), style(truncate(plan_preview, 60)).dim());
            }
            AgentEvent::PlanApproved { ref name, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {}", name_tag(name, c), style("plan approved").green());
            }
            AgentEvent::PlanRejected { ref name, ref feedback, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {}", name_tag(name, c), style("plan rejected").yellow(), style(truncate(feedback, 60)).dim());
            }
            AgentEvent::TeammateIdle { ref name, tasks_completed, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {}", name_tag(name, c), style("…").dim(), style(format!("idle ({} tasks done)", tasks_completed)).dim());
            }
            AgentEvent::AgentShutdown { ref name, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {}", name_tag(name, c), style("done").dim());
            }
            AgentEvent::SubAgentSpawned { ref name, ref description, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                let desc = if description.is_empty() { String::new() } else { format!(" — {}", truncate(description, 60)) };
                eprintln!();
                eprintln!("  {} {} {}{}", style("⎿").cyan(), style("Subagent").bold(), style(name).fg(c).bold(), style(desc).dim());
            }
            AgentEvent::SubAgentCompleted { ref name, tokens_used, tool_calls, ref final_content, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {} · {} tool {}", name_tag(name, c), style("✓").green(), style(format!("{} tokens", format_token_count(tokens_used))).dim(), tool_calls, if tool_calls == 1 { "use" } else { "uses" });
                if !final_content.is_empty() {
                    let lines: Vec<&str> = final_content.lines().take(3).collect();
                    for line in &lines {
                        eprintln!("{}   {}", name_tag(name, c), style(truncate(line, 80)).dim());
                    }
                    let total_lines = final_content.lines().count();
                    if total_lines > 3 {
                        eprintln!("{}   {}", name_tag(name, c), style(format!("… +{} more lines", total_lines - 3)).dim());
                    }
                }
            }
            AgentEvent::SubAgentFailed { ref name, ref error, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {}", name_tag(name, c), style("✗").red(), style(truncate(error, 80)).red());
            }
            AgentEvent::SubAgentUpdate { ref name, ref content, is_final, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                let marker = if is_final { "✔" } else { "…" };
                eprintln!("{} {} {}", name_tag(name, c), style(marker).fg(c), style(truncate(content, 80)).dim());
            }
            AgentEvent::SubAgentProgress { ref name, iteration, max_turns, ref current_tool, tokens_so_far, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                let tool_info = current_tool.as_deref().map(|t| format!(" · {}", t)).unwrap_or_default();
                eprint!("\r{}   {} {}/{}{} · {}", name_tag(name, c), style("◐").fg(c), iteration + 1, max_turns, style(&tool_info).dim(), style(format!("{} tokens", format_token_count(tokens_so_far))).dim());
                let _ = io::stderr().flush();
            }
            AgentEvent::TaskCreated { ref name, ref title, .. } => {
                let c = agent_color(name, &mut color_map, &mut next_color);
                eprintln!("{} {} {}", name_tag(name, c), style("+").fg(c), style(format!("new task: \"{}\"", truncate(title, 60))).dim());
            }
            AgentEvent::MemoryCompacted { ref strategy, messages_before, messages_after, tokens_saved, .. } => {
                eprintln!("  {} {} {}→{} messages ({} saved, {})", style("↻").dim(), style("compacted:").dim(), messages_before, messages_after, format_token_count(tokens_saved), style(strategy).dim());
            }
            AgentEvent::TeammateMessage { ref from_name, ref content_preview, .. } => {
                let c = agent_color(from_name, &mut color_map, &mut next_color);
                eprintln!("{}   {} {}", name_tag(from_name, c), style("→").fg(c), style(truncate(content_preview, 60)).dim());
            }
            _ => {}
        }
    }
}
