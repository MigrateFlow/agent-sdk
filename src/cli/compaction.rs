//! Message compaction logic used by the `/compact` slash command.

use crate::cli::display::truncate;
use crate::types::chat::ChatMessage;

#[derive(Debug, Clone, Copy)]
struct CliCompactionProfile {
    keep_recent: usize,
    tool_limit: usize,
    assistant_limit: usize,
    compress_user_messages: bool,
}

fn select_cli_compaction_profile(
    messages: &[ChatMessage],
) -> (&'static str, CliCompactionProfile) {
    let total = messages.len().max(1);
    let tool_count = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
        .count();
    let tool_ratio = tool_count as f64 / total as f64;
    let assistant_ratio = assistant_count as f64 / total as f64;

    if total >= 60 || tool_ratio >= 0.35 {
        return (
            "aggressive",
            CliCompactionProfile {
                keep_recent: 5,
                tool_limit: 120,
                assistant_limit: 120,
                compress_user_messages: true,
            },
        );
    }

    if assistant_ratio >= 0.45 {
        return (
            "conservative",
            CliCompactionProfile {
                keep_recent: 8,
                tool_limit: 350,
                assistant_limit: 250,
                compress_user_messages: false,
            },
        );
    }

    (
        "default",
        CliCompactionProfile {
            keep_recent: 6,
            tool_limit: 200,
            assistant_limit: 150,
            compress_user_messages: false,
        },
    )
}

/// Compact large tool/assistant/user messages in-place using a dynamic
/// profile. Returns `(compacted_entry_count, strategy_label)`.
pub fn compact_conversation(messages: &mut Vec<ChatMessage>) -> (usize, &'static str) {
    let before = messages.len();
    if before <= 4 {
        return (0, "none");
    }

    let (strategy, profile) = select_cli_compaction_profile(messages);
    let keep_tail = profile.keep_recent.min(before - 1);
    let compact_end = before - keep_tail;

    for i in 1..compact_end {
        match &messages[i] {
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                if content.len() > profile.tool_limit {
                    let summary = format!("[compacted: {} chars]", content.len());
                    messages[i] = ChatMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        content: summary,
                    };
                }
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } if content
                .as_ref()
                .is_some_and(|c| c.len() > profile.assistant_limit) =>
            {
                let short = content
                    .as_ref()
                    .map(|c| truncate(c, profile.assistant_limit));
                messages[i] = ChatMessage::Assistant {
                    content: short,
                    tool_calls: tool_calls.clone(),
                };
            }
            ChatMessage::User { content }
                if profile.compress_user_messages && content.len() > 200 =>
            {
                messages[i] = ChatMessage::User {
                    content: truncate(content, 150),
                };
            }
            _ => {}
        }
    }

    (compact_end.saturating_sub(1), strategy)
}
