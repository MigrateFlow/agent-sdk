//! Git worktree isolation for subagents.
//!
//! When a subagent is configured with `IsolationMode::Worktree`, it runs inside
//! a dedicated git worktree so its file-system changes are isolated from the
//! main working tree. After the subagent finishes, the worktree is either
//! cleaned up (no changes) or preserved for the user to merge.

use std::path::{Path, PathBuf};

use sdk_core::error::{AgentId, SdkError, SdkResult};
use serde::{Deserialize, Serialize};

/// Controls whether a subagent runs in the main working tree or an isolated
/// git worktree.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    #[default]
    None,
    Worktree,
}

/// Handle returned after creating a worktree — carries the path and branch
/// name so the caller can point the subagent at the worktree and clean up
/// afterwards.
pub struct WorktreeHandle {
    pub path: PathBuf,
    pub branch: String,
}

/// Create a git worktree for a subagent.
///
/// The worktree lives under `<repo_root>/.agent-worktrees/` and gets its own
/// branch named `agent/<short_id>-<sanitized_name>`.
pub async fn create_worktree(
    repo_root: &Path,
    agent_id: AgentId,
    name: &str,
) -> SdkResult<WorktreeHandle> {
    let short_id = &agent_id.to_string()[..8];
    let sanitized_name = name.replace(
        |c: char| !c.is_ascii_alphanumeric() && c != '-',
        "-",
    );
    let branch = format!("agent/{}-{}", short_id, sanitized_name);
    let worktree_path = repo_root.join(format!(
        ".agent-worktrees/{}-{}",
        short_id, sanitized_name
    ));

    // Ensure parent directory exists.
    tokio::fs::create_dir_all(worktree_path.parent().unwrap_or(repo_root))
        .await
        .map_err(SdkError::Io)?;

    let wt_str = worktree_path.to_str().unwrap_or("");
    let output = tokio::process::Command::new("git")
        .args(["worktree", "add", "-b", &branch, wt_str, "HEAD"])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(SdkError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SdkError::ToolExecution {
            tool_name: "worktree".to_string(),
            message: format!("Failed to create worktree: {}", stderr),
        });
    }

    Ok(WorktreeHandle {
        path: worktree_path,
        branch,
    })
}

/// Returns `true` when the worktree has uncommitted changes (staged or
/// unstaged).
pub async fn has_uncommitted_changes(worktree_path: &Path) -> bool {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .await;

    match output {
        Ok(o) => !o.stdout.is_empty(),
        Err(_) => false,
    }
}

/// Clean up a worktree after the subagent finishes.
///
/// If `has_changes` is `false` the worktree and its branch are removed. When
/// the subagent left changes behind, both are kept so the user can review and
/// merge them.
pub async fn cleanup_worktree(
    repo_root: &Path,
    handle: &WorktreeHandle,
    has_changes: bool,
) -> SdkResult<()> {
    if !has_changes {
        let wt_str = handle.path.to_str().unwrap_or("");
        // Remove the worktree directory.
        let _ = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force", wt_str])
            .current_dir(repo_root)
            .output()
            .await;

        // Delete the branch.
        let _ = tokio::process::Command::new("git")
            .args(["branch", "-D", &handle.branch])
            .current_dir(repo_root)
            .output()
            .await;
    }
    // When has_changes is true we intentionally keep both worktree and branch.
    Ok(())
}
