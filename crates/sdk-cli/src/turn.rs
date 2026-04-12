use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use console::style;

use sdk_agent::subagent::SubAgentRegistry;
use sdk_core::background::{BackgroundResult, BackgroundResultKind};
use sdk_core::error::AgentId;
use sdk_core::events::AgentEvent;
use sdk_core::memory::MemoryStore;
use sdk_core::storage::AgentPaths;
use sdk_core::traits::llm_client::{LlmClient, StreamDelta};
use sdk_core::traits::tool::Tool;
use sdk_core::types::chat::ChatMessage;

use crate::display::{print_task_list, truncate};
use crate::format::{
    create_spinner, format_duration, format_result_preview, format_tool_label,
    lang_hint, print_team_plan, print_team_result_summary, truncate_tool_result,
};
use crate::ndjson::{emit_ndjson, NdjsonEvent};
use crate::session::CliTask;
use crate::tools::build_tools;

pub struct TurnStats {
    pub tokens: u64,
    pub tool_calls: usize,
    pub iterations: usize,
    pub duration: std::time::Duration,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    messages: &mut Vec<ChatMessage>,
    user_input: &str,
    llm_client: &Arc<dyn LlmClient>,
    llm_config: &sdk_core::config::LlmConfig,
    work_dir: &Path,
    max_iterations: usize,
    allow_all: bool,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    interrupt: Arc<AtomicBool>,
    subagent_registry: Arc<SubAgentRegistry>,
    json_mode: bool,
    tool_filter: Option<&[String]>,
    mcp_tools: &[Arc<dyn Tool>],
    paths: &AgentPaths,
    memory_store: Option<Arc<MemoryStore>>,
    cli_agent_id: AgentId,
) -> anyhow::Result<TurnStats> {
    let (background_tx, mut background_rx) =
        tokio::sync::mpsc::unbounded_channel::<BackgroundResult>();

    let tools = build_tools(
        work_dir, allow_all, llm_client.clone(), llm_config.clone(),
        event_tx, tasks.clone(), subagent_registry, Some(background_tx),
        tool_filter, mcp_tools, paths, memory_store, cli_agent_id,
    );
    let tool_defs = tools.definitions();
    let started = Instant::now();

    if json_mode {
        emit_ndjson(&NdjsonEvent::Started {
            tools: tool_defs.iter().map(|t| t.name.clone()).collect(),
        });
    }

    messages.push(ChatMessage::user(user_input));

    let mut total_tokens = 0u64;
    let mut tool_calls_count = 0usize;

    for iteration in 0..max_iterations {
        // Drain background results
        while let Ok(result) = background_rx.try_recv() {
            let kind_label = match &result.kind {
                BackgroundResultKind::SubAgent => "subagent",
                BackgroundResultKind::AgentTeam => "agent team",
                BackgroundResultKind::CompactionSummary { .. }
                | BackgroundResultKind::SubAgentPartial => { continue; }
            };
            let notification = format!(
                "[Background {} '{}' completed — {} tokens]\n\n{}",
                kind_label, result.name, result.tokens_used, result.content,
            );
            messages.push(ChatMessage::user(notification));
        }

        if interrupt.load(Ordering::Relaxed) {
            interrupt.store(false, Ordering::Relaxed);
            if !json_mode { eprintln!("\n  {}", style("Interrupted").yellow()); }
            return Ok(TurnStats { tokens: total_tokens, tool_calls: tool_calls_count, iterations: iteration + 1, duration: started.elapsed() });
        }

        let mut spinner = if json_mode { None } else { Some(create_spinner("Thinking…")) };
        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<StreamDelta>();
        let (streaming_started_tx, streaming_started_rx) = tokio::sync::oneshot::channel::<()>();

        let is_json = json_mode;
        let emit_handle = tokio::spawn(async move {
            let mut streaming_started = false;
            let mut started_tx = Some(streaming_started_tx);
            while let Some(delta) = delta_rx.recv().await {
                match delta {
                    StreamDelta::Text(text) => {
                        if is_json {
                            emit_ndjson(&NdjsonEvent::TextDelta { content: text });
                        } else {
                            if !streaming_started {
                                streaming_started = true;
                                if let Some(tx) = started_tx.take() { let _ = tx.send(()); }
                                tokio::task::yield_now().await;
                            }
                            eprint!("{}", text);
                            let _ = io::stderr().flush();
                        }
                    }
                    StreamDelta::Thinking(_) => {}
                }
            }
            streaming_started
        });

        let messages_snapshot = messages.clone();
        let llm_fut = llm_client.chat_stream(&messages_snapshot, &tool_defs, delta_tx);

        tokio::pin!(llm_fut);
        let result = tokio::select! {
            biased;
            _ = streaming_started_rx => {
                if let Some(s) = spinner.take() { s.finish_and_clear(); }
                llm_fut.await
            }
            res = &mut llm_fut => res,
        };

        let streamed = emit_handle.await.unwrap_or(false);
        if let Some(s) = spinner { s.finish_and_clear(); }

        if interrupt.load(Ordering::Relaxed) {
            interrupt.store(false, Ordering::Relaxed);
            if !json_mode { eprintln!("  {}", style("Interrupted").yellow()); }
            return Ok(TurnStats { tokens: total_tokens, tool_calls: tool_calls_count, iterations: iteration + 1, duration: started.elapsed() });
        }

        let (response, tokens) = result?;
        total_tokens += tokens;

        match response {
            ChatMessage::Assistant { ref content, ref tool_calls } if !tool_calls.is_empty() => {
                // Show thinking text
                if let Some(text) = content {
                    if !text.is_empty() {
                        if json_mode {
                            emit_ndjson(&NdjsonEvent::Thinking { content: text.clone(), iteration });
                        } else {
                            if streamed { eprint!("\r\x1b[K"); }
                            let thinking_lines: Vec<&str> = text.lines().collect();
                            let show_lines = thinking_lines.len().min(3);
                            for line in &thinking_lines[..show_lines] {
                                eprintln!("  {} {}", style("│").dim(), style(truncate(line, 100)).dim().italic());
                            }
                            if thinking_lines.len() > show_lines {
                                eprintln!("  {} {}", style("│").dim(), style(format!("… +{} more lines", thinking_lines.len() - show_lines)).dim());
                            }
                        }
                    }
                }

                messages.push(response.clone());

                if !json_mode && iteration > 0 {
                    eprintln!("  {}", style(format!("[iter {}]", iteration + 1)).dim());
                }

                let tc_count = tool_calls.len();
                for (tc_idx, tc) in tool_calls.iter().enumerate() {
                    let is_last_tc = tc_idx == tc_count - 1;

                    if json_mode {
                        emit_ndjson(&NdjsonEvent::ToolCall {
                            tool_call_id: tc.id.clone(),
                            tool_name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                            iteration,
                        });
                    } else {
                        let label = format_tool_label(&tc.function.name, &tc.function.arguments);
                        let connector = if is_last_tc { "└" } else { "├" };
                        eprintln!("  {} {}", style(connector).cyan(), label);

                        if tc.function.name == "spawn_agent_team" {
                            print_team_plan(&tc.function.arguments);
                        }

                        // write_file preview
                        if tc.function.name == "write_file" {
                            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                            if let Some(content) = args["content"].as_str() {
                                let path = args["path"].as_str().unwrap_or("?");
                                let lang = lang_hint(path);
                                let lines: Vec<&str> = content.lines().collect();
                                eprintln!("  {}  {}", style("│").dim(), style(format!("── {} ({} lines{}) ──", path, lines.len(), lang)).dim());
                                let show = lines.len().min(8);
                                for line in &lines[..show] {
                                    eprintln!("  {}  {}", style("│").dim(), style(truncate(line, 100)).dim());
                                }
                                if lines.len() > show {
                                    eprintln!("  {}  {}", style("│").dim(), style(format!("… +{} more lines", lines.len() - show)).dim());
                                }
                            }
                        }

                        // edit_file diff preview
                        if tc.function.name == "edit_file" {
                            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                            let old = args["old_string"].as_str().unwrap_or("");
                            let new = args["new_string"].as_str().unwrap_or("");
                            if !old.is_empty() || !new.is_empty() {
                                let path = args["path"].as_str().unwrap_or("?");
                                eprintln!("  {}  {}", style("│").dim(), style(format!("@@ {} @@", path)).cyan().dim());
                                let old_lines: Vec<&str> = old.lines().collect();
                                let new_lines: Vec<&str> = new.lines().collect();
                                let max_preview = 6;
                                for line in &old_lines[..old_lines.len().min(max_preview)] {
                                    eprintln!("  {}  {}", style("│").dim(), style(format!("- {}", truncate(line, 90))).red().dim());
                                }
                                if old_lines.len() > max_preview {
                                    eprintln!("  {}  {}", style("│").dim(), style(format!("  … +{} more", old_lines.len() - max_preview)).dim());
                                }
                                for line in &new_lines[..new_lines.len().min(max_preview)] {
                                    eprintln!("  {}  {}", style("│").dim(), style(format!("+ {}", truncate(line, 90))).green().dim());
                                }
                                if new_lines.len() > max_preview {
                                    eprintln!("  {}  {}", style("│").dim(), style(format!("  … +{} more", new_lines.len() - max_preview)).dim());
                                }
                            }
                        }
                    }

                    let args: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                    let tool_start = Instant::now();
                    let result = tools.execute(&tc.function.name, args).await;
                    let tool_elapsed = tool_start.elapsed();

                    let result_content = match &result {
                        Ok(val) => { let full = serde_json::to_string(val).unwrap_or_default(); truncate_tool_result(&full) }
                        Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                    };

                    if json_mode {
                        emit_ndjson(&NdjsonEvent::ToolResult {
                            tool_call_id: tc.id.clone(),
                            tool_name: tc.function.name.clone(),
                            content: result_content.clone(),
                            iteration,
                        });
                    } else {
                        let preview = format_result_preview(&tc.function.name, &result_content);
                        let timing = if tool_elapsed.as_secs_f64() > 1.0 { format!(" ({})", format_duration(tool_elapsed)) } else { String::new() };
                        eprintln!("    {}{}", style(&preview).dim(), style(&timing).dim());

                        if tc.function.name == "spawn_agent_team" {
                            print_team_result_summary(&result_content);
                        } else if tc.function.name == "update_task_list" {
                            let current = tasks.lock().expect("task list mutex poisoned").clone();
                            print_task_list(&current);
                        }
                    }

                    messages.push(ChatMessage::tool_result(&tc.id, &result_content));
                    tool_calls_count += 1;
                }
            }

            ChatMessage::Assistant { ref content, .. } => {
                let answer = content.clone().unwrap_or_default();
                messages.push(response);

                if json_mode {
                    emit_ndjson(&NdjsonEvent::Completed { final_content: answer, tokens_used: total_tokens, iterations: iteration + 1, tool_calls: tool_calls_count });
                } else if streamed {
                    eprintln!(); eprintln!();
                } else {
                    eprintln!();
                    for line in answer.lines() { eprintln!("{}", line); }
                    eprintln!();
                }
                return Ok(TurnStats { tokens: total_tokens, tool_calls: tool_calls_count, iterations: iteration + 1, duration: started.elapsed() });
            }

            other => {
                let text = other.text_content().unwrap_or("").to_string();
                messages.push(other);
                if json_mode {
                    emit_ndjson(&NdjsonEvent::Completed { final_content: text, tokens_used: total_tokens, iterations: iteration + 1, tool_calls: tool_calls_count });
                } else if !streamed {
                    eprintln!(); eprintln!("{}", text); eprintln!();
                } else {
                    eprintln!(); eprintln!();
                }
                return Ok(TurnStats { tokens: total_tokens, tool_calls: tool_calls_count, iterations: iteration + 1, duration: started.elapsed() });
            }
        }
    }

    if json_mode {
        emit_ndjson(&NdjsonEvent::Failed { error: format!("max iterations ({}) reached", max_iterations) });
    } else {
        eprintln!(); eprintln!("  {} Max iterations ({}) reached", style("⚠").yellow(), max_iterations);
    }
    Ok(TurnStats { tokens: total_tokens, tool_calls: tool_calls_count, iterations: max_iterations, duration: started.elapsed() })
}
