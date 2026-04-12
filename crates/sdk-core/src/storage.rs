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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_paths() -> (tempfile::TempDir, AgentPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AgentPaths::for_work_dir(dir.path()).expect("build paths");
        (dir, paths)
    }

    #[test]
    fn for_work_dir_canonicalizes_existing_path() {
        let (dir, paths) = make_paths();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(paths.project_config_dir(), canonical.join(AGENT_DIR));
    }

    #[test]
    fn for_work_dir_joins_nonexistent_path_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not_yet_here");
        let paths = AgentPaths::for_work_dir(&missing).unwrap();
        // work_dir should have been joined with cwd since missing didn't exist.
        // Since `dir.path()` is absolute, the join just yields `missing` itself.
        assert!(paths.project_config_dir().starts_with(missing));
    }

    #[test]
    fn project_paths_are_under_work_dir_agent_dir() {
        let (_d, paths) = make_paths();
        let base = paths.project_config_dir();
        assert_eq!(paths.project_settings_path(), base.join("settings.json"));
        assert_eq!(
            paths.project_local_settings_path(),
            base.join("settings.local.json")
        );
        assert_eq!(paths.project_mcp_config_path(), base.join("mcp.json"));
        assert_eq!(paths.project_lsp_config_path(), base.join("lsp.json"));
    }

    #[test]
    fn user_paths_live_under_home_agent_dir() {
        let (_d, paths) = make_paths();
        let home = dirs::home_dir().unwrap().join(AGENT_DIR);
        assert_eq!(paths.user_root_dir(), home);
        assert_eq!(paths.user_settings_path(), home.join("settings.json"));
        assert_eq!(paths.projects_dir(), home.join("projects"));
        assert_eq!(paths.teams_dir(), home.join("teams"));
        assert_eq!(paths.tasks_dir(), home.join("tasks"));
    }

    #[test]
    fn project_state_layout_matches_spec() {
        let (_d, paths) = make_paths();
        let root = paths.projects_dir().join(paths.project_key());
        assert_eq!(paths.project_state_dir(), root);
        assert_eq!(paths.project_tasks_dir(), root.join("tasks"));
        assert_eq!(paths.project_mailbox_dir(), root.join("mailbox"));
        assert_eq!(paths.project_memory_dir(), root.join("memory"));
        assert_eq!(paths.project_sessions_dir(), root.join("sessions"));
        assert_eq!(
            paths.cli_session_path(),
            root.join("sessions").join("cli-session.json")
        );
    }

    #[test]
    fn team_paths_compose_correctly() {
        let (_d, paths) = make_paths();
        let name = "team-abc";
        let team = paths.teams_dir().join(name);
        assert_eq!(paths.team_dir(name), team);
        assert_eq!(paths.team_config_path(name), team.join("config.json"));
        assert_eq!(paths.team_mailbox_dir(name), team.join("mailbox"));
        assert_eq!(paths.team_memory_dir(name), team.join("memory"));
        assert_eq!(paths.team_tasks_dir(name), paths.tasks_dir().join(name));
    }

    #[test]
    fn new_team_name_is_deterministic_prefix_unique_suffix() {
        let (_d, paths) = make_paths();
        let a = paths.new_team_name();
        let b = paths.new_team_name();
        assert_ne!(a, b);
        assert!(a.starts_with(paths.project_key()));
        // Suffix should be 8 hex chars after the trailing dash.
        let suffix = a.rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), 8);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn project_key_contains_slug_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        // Directory name includes non-alphanumeric chars.
        let custom = dir.path().join("weird name!");
        std::fs::create_dir(&custom).unwrap();
        let paths = AgentPaths::for_work_dir(&custom).unwrap();
        let key = paths.project_key();
        // Non-alnum chars replaced with `-`, so "weird name!" -> "weird-name-"
        assert!(key.starts_with("weird-name-"), "key was {key}");
        // Ends with 16 hex chars.
        let hash = key.rsplit('-').next().unwrap();
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn different_work_dirs_get_different_project_keys() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let p1 = AgentPaths::for_work_dir(d1.path()).unwrap();
        let p2 = AgentPaths::for_work_dir(d2.path()).unwrap();
        assert_ne!(p1.project_key(), p2.project_key());
    }

    #[test]
    fn git_dir_parent_shares_project_key_across_children() {
        // Simulate a git repo by placing a `.git` dir at the root; two subdirs
        // should resolve to the same project_key.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let a = dir.path().join("sub_a");
        let b = dir.path().join("sub_b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();

        let pa = AgentPaths::for_work_dir(&a).unwrap();
        let pb = AgentPaths::for_work_dir(&b).unwrap();
        assert_eq!(pa.project_key(), pb.project_key());
    }

    #[test]
    fn git_file_gitdir_and_commondir_resolve_shared_key() {
        // Simulate a git worktree: work_dir/.git is a file pointing to a
        // gitdir that has a commondir pointing to the main repo.
        let root = tempfile::tempdir().unwrap();
        let main_git = root.path().join("mainrepo/.git");
        std::fs::create_dir_all(&main_git).unwrap();
        let worktree_gitdir = root.path().join("mainrepo/.git/worktrees/wt1");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        // commondir -> relative ../../
        std::fs::write(worktree_gitdir.join("commondir"), "../../").unwrap();

        let worktree = root.path().join("wt1");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!(
                "gitdir: {}\n",
                worktree_gitdir.display()
            ),
        )
        .unwrap();

        // The main repo itself
        let main_paths = AgentPaths::for_work_dir(&root.path().join("mainrepo")).unwrap();
        // The linked worktree should resolve the same common dir, giving the
        // same project_key.
        let wt_paths = AgentPaths::for_work_dir(&worktree).unwrap();
        assert_eq!(main_paths.project_key(), wt_paths.project_key());
    }
}
