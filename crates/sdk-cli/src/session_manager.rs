//! Session lifecycle management: listing, creation, PID tracking, and recovery.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sdk_core::error::{SdkError, SdkResult};

/// Metadata persisted alongside session data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: SessionStatus,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub turn_count: usize,
    #[serde(default)]
    pub token_count: u64,
    #[serde(default)]
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    Active,
    Idle,
    Interrupted,
    Completed,
}

impl Default for SessionStatus {
    fn default() -> Self {
        Self::Idle
    }
}

impl Default for SessionMetadata {
    fn default() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: now.clone(),
            updated_at: now,
            status: SessionStatus::Idle,
            description: String::new(),
            turn_count: 0,
            token_count: 0,
            message_count: 0,
        }
    }
}

/// JSON structure written to `{session_id}.pid` files.
#[derive(Debug, Serialize, Deserialize)]
struct PidRecord {
    pid: u32,
    session_id: String,
    started_at: String,
    status: String,
}

pub struct SessionManager;

impl SessionManager {
    /// List all sessions in a sessions directory by scanning `*.json` files.
    ///
    /// Returns sessions sorted by `updated_at` descending (most recent first).
    pub fn list_sessions(sessions_dir: &Path) -> SdkResult<Vec<SessionMetadata>> {
        let read = match std::fs::read_dir(sessions_dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(SdkError::Io(e)),
        };

        let mut sessions: Vec<SessionMetadata> = Vec::new();

        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Try to extract metadata from CliSessionData
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(meta) = val.get("metadata").and_then(|m| {
                    serde_json::from_value::<SessionMetadata>(m.clone()).ok()
                }) {
                    sessions.push(meta);
                } else {
                    // Build a minimal metadata entry from the file itself
                    let id = Self::session_id_from_path(&path);
                    let file_meta = std::fs::metadata(&path).ok();
                    let modified = file_meta
                        .and_then(|m| m.modified().ok())
                        .map(|t| {
                            let dt: chrono::DateTime<chrono::Utc> = t.into();
                            dt.to_rfc3339()
                        })
                        .unwrap_or_default();

                    let message_count = val
                        .get("messages")
                        .and_then(|m| m.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);

                    sessions.push(SessionMetadata {
                        id,
                        created_at: modified.clone(),
                        updated_at: modified,
                        status: SessionStatus::Idle,
                        description: String::new(),
                        turn_count: 0,
                        token_count: 0,
                        message_count,
                    });
                }
            }
        }

        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    /// Create a new session file with a UUID name. Returns `(path, session_id)`.
    pub fn create_session(
        sessions_dir: &Path,
        description: &str,
    ) -> SdkResult<(PathBuf, String)> {
        std::fs::create_dir_all(sessions_dir).map_err(SdkError::Io)?;

        let mut meta = SessionMetadata::default();
        meta.description = description.to_string();

        let path = sessions_dir.join(format!("{}.json", meta.id));

        let session = crate::session::CliSessionData {
            messages: Vec::new(),
            tasks: Vec::new(),
            metadata: Some(meta.clone()),
            mode: sdk_core::types::agent_mode::AgentMode::Normal,
            ultra_plan: None,
        };

        let json = serde_json::to_string(&session)
            .map_err(|e| SdkError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        std::fs::write(&path, json).map_err(SdkError::Io)?;

        Ok((path, meta.id))
    }

    /// Register this process as owning a session (PID file).
    pub fn register_pid(sessions_dir: &Path, session_id: &str) -> SdkResult<()> {
        let pid_path = sessions_dir.join(format!("{}.pid", session_id));
        let record = PidRecord {
            pid: std::process::id(),
            session_id: session_id.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            status: "active".to_string(),
        };
        let json = serde_json::to_string(&record)
            .map_err(|e| SdkError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        std::fs::write(&pid_path, json).map_err(SdkError::Io)?;
        Ok(())
    }

    /// Cleanup PID file on exit (best-effort, errors ignored).
    pub fn cleanup_pid(sessions_dir: &Path, session_id: &str) {
        let pid_path = sessions_dir.join(format!("{}.pid", session_id));
        let _ = std::fs::remove_file(pid_path);
    }

    /// Detect interrupted sessions whose owning process is no longer alive.
    pub fn detect_interrupted(sessions_dir: &Path) -> Vec<SessionMetadata> {
        let read = match std::fs::read_dir(sessions_dir) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        let mut interrupted = Vec::new();

        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pid") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let record: PidRecord = match serde_json::from_str(&content) {
                Ok(r) => r,
                Err(_) => continue,
            };

            if !is_pid_alive(record.pid) {
                // Load the matching session metadata if available
                let session_path = sessions_dir.join(format!("{}.json", record.session_id));
                let mut meta = if let Ok(content) = std::fs::read_to_string(&session_path) {
                    serde_json::from_str::<serde_json::Value>(&content)
                        .ok()
                        .and_then(|v| v.get("metadata").cloned())
                        .and_then(|m| serde_json::from_value::<SessionMetadata>(m).ok())
                        .unwrap_or_else(|| SessionMetadata {
                            id: record.session_id.clone(),
                            ..Default::default()
                        })
                } else {
                    SessionMetadata {
                        id: record.session_id.clone(),
                        ..Default::default()
                    }
                };

                meta.status = SessionStatus::Interrupted;
                interrupted.push(meta);

                // Clean up the stale PID file
                let _ = std::fs::remove_file(&path);
            }
        }

        interrupted
    }

    /// Attempt to repair a session file by removing unresolved tool uses.
    pub fn repair_session(path: &Path) -> SdkResult<()> {
        let content = std::fs::read_to_string(path).map_err(SdkError::Io)?;

        let mut val: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
            SdkError::Config(format!(
                "Cannot parse session file {}: {}",
                path.display(),
                e
            ))
        })?;

        // Remove trailing tool_use messages that lack a matching tool result
        if let Some(messages) = val.get_mut("messages").and_then(|m| m.as_array_mut()) {
            while let Some(last) = messages.last() {
                if last.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(content) = last.get("content") {
                        if content.is_array() {
                            let has_tool_use = content.as_array().map_or(false, |arr| {
                                arr.iter().any(|block| {
                                    block.get("type").and_then(|t| t.as_str())
                                        == Some("tool_use")
                                })
                            });
                            if has_tool_use {
                                messages.pop();
                                continue;
                            }
                        }
                    }
                }
                break;
            }
        }

        let json = serde_json::to_string(&val)
            .map_err(|e| SdkError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        std::fs::write(path, json).map_err(SdkError::Io)?;

        Ok(())
    }

    /// Extract session ID from a session file path (filename without extension).
    pub fn session_id_from_path(path: &Path) -> String {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{:x}", fxhash(path.to_string_lossy().as_bytes())))
    }
}

/// Simple hash for fallback ID generation.
fn fxhash(data: &[u8]) -> u64 {
    let mut h: u64 = 0;
    for &b in data {
        h = h.wrapping_mul(0x100000001b3).wrapping_add(b as u64);
    }
    h
}

/// Check whether a process with the given PID is still running.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_from_path_uses_stem() {
        let p = Path::new("/tmp/sessions/abc123.json");
        assert_eq!(SessionManager::session_id_from_path(p), "abc123");
    }

    #[test]
    fn default_metadata_has_uuid() {
        let m = SessionMetadata::default();
        assert!(!m.id.is_empty());
        assert_eq!(m.status, SessionStatus::Idle);
    }

    #[test]
    fn list_sessions_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = SessionManager::list_sessions(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn list_sessions_nonexistent_dir() {
        let result = SessionManager::list_sessions(Path::new("/nonexistent/path")).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn create_and_list_session() {
        let dir = tempfile::tempdir().unwrap();
        let (path, id) = SessionManager::create_session(dir.path(), "test session").unwrap();
        assert!(path.exists());

        let sessions = SessionManager::list_sessions(dir.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, id);
        assert_eq!(sessions[0].description, "test session");
    }

    #[test]
    fn pid_register_and_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = "test-session";
        SessionManager::register_pid(dir.path(), session_id).unwrap();

        let pid_path = dir.path().join(format!("{}.pid", session_id));
        assert!(pid_path.exists());

        SessionManager::cleanup_pid(dir.path(), session_id);
        assert!(!pid_path.exists());
    }

    #[test]
    fn repair_session_removes_trailing_tool_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");

        let data = serde_json::json!({
            "messages": [
                {"role": "system", "content": "hello"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "read_file", "input": {}}
                ]}
            ],
            "tasks": []
        });

        std::fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        SessionManager::repair_session(&path).unwrap();

        let repaired: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let messages = repaired["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
    }
}
