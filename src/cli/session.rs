//! Session persistence and task-list types shared between the CLI binary and
//! the slash-command framework.
//!
//! These types are extracted from `src/bin/agent.rs` so that reusable slash
//! commands (e.g. `/clear`, `/compact`, `/cost`, `/tasks`) can operate on them
//! through [`crate::cli::commands::CommandContext`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::AGENT_DIR;
use crate::storage::AgentPaths;
use crate::types::chat::ChatMessage;

/// Visible task row rendered by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliTask {
    pub title: String,
    pub status: String,
}

/// Structure persisted to disk for the single-agent CLI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliSessionData {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tasks: Vec<CliTask>,
}

/// Return the default session-file path for a given working directory.
pub fn default_session_path(work_dir: &Path) -> PathBuf {
    AgentPaths::for_work_dir(work_dir)
        .map(|paths| paths.cli_session_path())
        .unwrap_or_else(|_| work_dir.join(AGENT_DIR).join("session.json"))
}

/// Load a persisted session if the system prompt still matches.
///
/// Returns `None` if the file does not exist, cannot be parsed, or the stored
/// system prompt differs from `system_prompt`.
pub fn load_session(path: &Path, system_prompt: &str) -> Option<CliSessionData> {
    let content = std::fs::read_to_string(path).ok()?;
    let session = serde_json::from_str::<CliSessionData>(&content)
        .ok()
        .or_else(|| {
            serde_json::from_str::<Vec<ChatMessage>>(&content)
                .ok()
                .map(|messages| CliSessionData {
                    messages,
                    tasks: Vec::new(),
                })
        })?;

    match session.messages.first() {
        Some(ChatMessage::System { content }) if content == system_prompt => Some(session),
        _ => None,
    }
}

/// Persist a CLI session to disk, creating parent directories as needed.
pub fn save_session(
    path: &Path,
    messages: &[ChatMessage],
    tasks: &[CliTask],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let session = CliSessionData {
        messages: messages.to_vec(),
        tasks: tasks.to_vec(),
    };
    let json = serde_json::to_string(&session)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}
