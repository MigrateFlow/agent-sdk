use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::config::AGENT_DIR;
use crate::error::{SdkError, SdkResult};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AgentPaths {
    work_dir: PathBuf,
    home_dir: PathBuf,
    project_key: String,
}

impl AgentPaths {
    pub fn for_work_dir(work_dir: &Path) -> SdkResult<Self> {
        let work_dir = canonicalize_for_storage(work_dir)?;
        let home_dir = dirs::home_dir()
            .ok_or_else(|| SdkError::Config("Could not resolve home directory".to_string()))?
            .join(AGENT_DIR);
        let identity_path = git_common_dir(&work_dir).unwrap_or_else(|| work_dir.clone());
        let project_key = project_key_for_path(&identity_path);

        Ok(Self {
            work_dir,
            home_dir,
            project_key,
        })
    }

    pub fn project_config_dir(&self) -> PathBuf {
        self.work_dir.join(AGENT_DIR)
    }

    pub fn project_settings_path(&self) -> PathBuf {
        self.project_config_dir().join("settings.json")
    }

    pub fn project_local_settings_path(&self) -> PathBuf {
        self.project_config_dir().join("settings.local.json")
    }

    pub fn project_mcp_config_path(&self) -> PathBuf {
        self.project_config_dir().join("mcp.json")
    }

    /// Path to the per-project LSP server manifest (`.agent/lsp.json`).
    pub fn project_lsp_config_path(&self) -> PathBuf {
        self.project_config_dir().join("lsp.json")
    }

    pub fn user_root_dir(&self) -> PathBuf {
        self.home_dir.clone()
    }

    pub fn user_settings_path(&self) -> PathBuf {
        self.home_dir.join("settings.json")
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.home_dir.join("projects")
    }

    pub fn project_state_dir(&self) -> PathBuf {
        self.projects_dir().join(&self.project_key)
    }

    pub fn project_tasks_dir(&self) -> PathBuf {
        self.project_state_dir().join("tasks")
    }

    pub fn project_mailbox_dir(&self) -> PathBuf {
        self.project_state_dir().join("mailbox")
    }

    pub fn project_memory_dir(&self) -> PathBuf {
        self.project_state_dir().join("memory")
    }

    pub fn project_sessions_dir(&self) -> PathBuf {
        self.project_state_dir().join("sessions")
    }

    pub fn cli_session_path(&self) -> PathBuf {
        self.project_sessions_dir().join("cli-session.json")
    }

    pub fn project_key(&self) -> &str {
        &self.project_key
    }

    pub fn teams_dir(&self) -> PathBuf {
        self.home_dir.join("teams")
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.home_dir.join("tasks")
    }

    pub fn new_team_name(&self) -> String {
        format!("{}-{}", self.project_key, &Uuid::new_v4().to_string()[..8])
    }

    pub fn team_dir(&self, team_name: &str) -> PathBuf {
        self.teams_dir().join(team_name)
    }

    pub fn team_config_path(&self, team_name: &str) -> PathBuf {
        self.team_dir(team_name).join("config.json")
    }

    pub fn team_mailbox_dir(&self, team_name: &str) -> PathBuf {
        self.team_dir(team_name).join("mailbox")
    }

    pub fn team_memory_dir(&self, team_name: &str) -> PathBuf {
        self.team_dir(team_name).join("memory")
    }

    pub fn team_tasks_dir(&self, team_name: &str) -> PathBuf {
        self.tasks_dir().join(team_name)
    }
}

fn canonicalize_for_storage(path: &Path) -> SdkResult<PathBuf> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(SdkError::Io);
    }

    let joined = std::env::current_dir().map_err(SdkError::Io)?.join(path);
    Ok(joined)
}

fn project_key_for_path(path: &Path) -> String {
    let label = path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("project");
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let hash = hasher.finish();

    format!("{slug}-{hash:016x}")
}

fn git_common_dir(work_dir: &Path) -> Option<PathBuf> {
    for dir in work_dir.ancestors() {
        let git_entry = dir.join(".git");

        if git_entry.is_dir() {
            return std::fs::canonicalize(git_entry).ok();
        }

        if git_entry.is_file() {
            let gitdir = resolve_gitdir_from_file(dir, &git_entry)?;
            let common = resolve_common_dir(&gitdir).unwrap_or(gitdir);
            return std::fs::canonicalize(common).ok();
        }
    }

    None
}

fn resolve_gitdir_from_file(base_dir: &Path, git_file: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(git_file).ok()?;
    let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
    let path = Path::new(gitdir);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };
    Some(resolved)
}

fn resolve_common_dir(gitdir: &Path) -> Option<PathBuf> {
    let commondir = gitdir.join("commondir");
    let content = std::fs::read_to_string(commondir).ok()?;
    let path = Path::new(content.trim());
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        gitdir.join(path)
    })
}
