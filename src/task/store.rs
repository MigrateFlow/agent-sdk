use std::path::PathBuf;

use chrono::Utc;
use tracing::{debug, info, warn};

use crate::error::{AgentId, SdkError, SdkResult, TaskId};
use crate::types::task::{Task, TaskResult, TaskStatus};

use super::file_lock::FileLock;

/// File-backed task store. Tasks live in status-based directories:
/// `{base}/tasks/{pending,in_progress,completed,failed}/{task_id}.json`
pub struct TaskStore {
    base_dir: PathBuf,
}

impl TaskStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn init(&self) -> SdkResult<()> {
        for subdir in &["pending", "in_progress", "completed", "failed"] {
            std::fs::create_dir_all(self.tasks_dir().join(subdir))?;
        }
        Ok(())
    }

    fn tasks_dir(&self) -> PathBuf {
        self.base_dir.join("tasks")
    }

    fn task_path(&self, status_dir: &str, task_id: TaskId) -> PathBuf {
        self.tasks_dir()
            .join(status_dir)
            .join(format!("{}.json", task_id))
    }

    fn lock_path(&self, status_dir: &str, task_id: TaskId) -> PathBuf {
        self.tasks_dir()
            .join(status_dir)
            .join(format!("{}.lock", task_id))
    }

    pub fn create_task(&self, task: &Task) -> SdkResult<()> {
        let path = self.task_path("pending", task.id);
        let json = serde_json::to_string_pretty(task)?;
        std::fs::write(&path, json)?;
        debug!(task_id = %task.id, "Created task: {}", task.title);
        Ok(())
    }

    pub fn try_claim_next(
        &self,
        agent_id: AgentId,
        completed_task_ids: &[TaskId],
    ) -> SdkResult<Option<Task>> {
        let pending_dir = self.tasks_dir().join("pending");

        let mut entries: Vec<_> = std::fs::read_dir(&pending_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
            })
            .collect();

        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            let task_id_str = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let task_id: TaskId = match task_id_str.parse() {
                Ok(id) => id,
                Err(_) => continue,
            };

            let lock_path = self.lock_path("pending", task_id);
            let lock = match FileLock::try_acquire(&lock_path)? {
                Some(lock) => lock,
                None => continue,
            };

            let content = std::fs::read_to_string(&path)?;
            let mut task: Task = serde_json::from_str(&content)?;

            let deps_resolved = task
                .dependencies
                .iter()
                .all(|dep_id| completed_task_ids.contains(dep_id));

            if !deps_resolved {
                drop(lock);
                continue;
            }

            task.status = TaskStatus::Claimed {
                agent_id,
                at: Utc::now(),
            };
            task.updated_at = Utc::now();

            let new_path = self.task_path("in_progress", task_id);
            let json = serde_json::to_string_pretty(&task)?;
            std::fs::write(&path, &json)?;
            std::fs::rename(&path, &new_path)?;

            let new_lock_path = self.lock_path("in_progress", task_id);
            let _ = std::fs::rename(&lock_path, &new_lock_path);

            info!(task_id = %task_id, agent_id = %agent_id, "Task claimed: {}", task.title);
            drop(lock);
            return Ok(Some(task));
        }

        Ok(None)
    }

    pub fn mark_in_progress(&self, task_id: TaskId, agent_id: AgentId) -> SdkResult<()> {
        let path = self.task_path("in_progress", task_id);
        let content = std::fs::read_to_string(&path)?;
        let mut task: Task = serde_json::from_str(&content)?;

        task.status = TaskStatus::InProgress {
            agent_id,
            started_at: Utc::now(),
        };
        task.updated_at = Utc::now();

        let json = serde_json::to_string_pretty(&task)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    pub fn complete_task(
        &self,
        task_id: TaskId,
        agent_id: AgentId,
        result: TaskResult,
    ) -> SdkResult<()> {
        let src = self.task_path("in_progress", task_id);
        let content = std::fs::read_to_string(&src)?;
        let mut task: Task = serde_json::from_str(&content)?;

        task.status = TaskStatus::Completed {
            agent_id,
            completed_at: Utc::now(),
        };
        task.result = Some(result);
        task.updated_at = Utc::now();

        let dst = self.task_path("completed", task_id);
        let json = serde_json::to_string_pretty(&task)?;
        std::fs::write(&src, &json)?;
        std::fs::rename(&src, &dst)?;

        let _ = std::fs::remove_file(self.lock_path("in_progress", task_id));

        info!(task_id = %task_id, "Task completed");
        Ok(())
    }

    pub fn fail_task(
        &self,
        task_id: TaskId,
        agent_id: AgentId,
        error: String,
    ) -> SdkResult<()> {
        let src = self.task_path("in_progress", task_id);
        let content = std::fs::read_to_string(&src)?;
        let mut task: Task = serde_json::from_str(&content)?;

        task.retry_count += 1;
        task.updated_at = Utc::now();

        if task.retry_count < task.max_retries {
            task.status = TaskStatus::Pending;
            let dst = self.task_path("pending", task_id);
            let json = serde_json::to_string_pretty(&task)?;
            std::fs::write(&src, &json)?;
            std::fs::rename(&src, &dst)?;
            warn!(task_id = %task_id, retry = task.retry_count, "Task failed, retrying");
        } else {
            task.status = TaskStatus::Failed {
                agent_id,
                error,
                failed_at: Utc::now(),
            };
            let dst = self.task_path("failed", task_id);
            let json = serde_json::to_string_pretty(&task)?;
            std::fs::write(&src, &json)?;
            std::fs::rename(&src, &dst)?;
            warn!(task_id = %task_id, "Task permanently failed");
        }

        let _ = std::fs::remove_file(self.lock_path("in_progress", task_id));
        Ok(())
    }

    pub fn read_task(&self, task_id: TaskId) -> SdkResult<Task> {
        for dir in &["pending", "in_progress", "completed", "failed"] {
            let path = self.task_path(dir, task_id);
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                return Ok(serde_json::from_str(&content)?);
            }
        }
        Err(SdkError::TaskNotFound { task_id })
    }

    pub fn list_all_tasks(&self) -> SdkResult<Vec<Task>> {
        let mut tasks = Vec::new();
        for dir in &["pending", "in_progress", "completed", "failed"] {
            tasks.extend(self.list_tasks_in_dir(dir)?);
        }
        Ok(tasks)
    }

    pub fn list_tasks_in_dir(&self, status_dir: &str) -> SdkResult<Vec<Task>> {
        let dir = self.tasks_dir().join(status_dir);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut tasks = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let content = std::fs::read_to_string(&path)?;
                match serde_json::from_str::<Task>(&content) {
                    Ok(task) => tasks.push(task),
                    Err(e) => warn!("Failed to parse task file {:?}: {}", path, e),
                }
            }
        }
        Ok(tasks)
    }

    pub fn completed_task_ids(&self) -> SdkResult<Vec<TaskId>> {
        Ok(self
            .list_tasks_in_dir("completed")?
            .into_iter()
            .map(|t| t.id)
            .collect())
    }

    pub fn recover_orphaned_tasks(&self) -> SdkResult<usize> {
        let in_progress = self.list_tasks_in_dir("in_progress")?;
        let mut recovered = 0;

        for mut task in in_progress {
            task.status = TaskStatus::Pending;
            task.retry_count += 1;
            task.updated_at = Utc::now();

            let src = self.task_path("in_progress", task.id);
            let dst = self.task_path("pending", task.id);
            let json = serde_json::to_string_pretty(&task)?;
            std::fs::write(&src, &json)?;
            std::fs::rename(&src, &dst)?;

            let _ = std::fs::remove_file(self.lock_path("in_progress", task.id));

            recovered += 1;
            info!(task_id = %task.id, "Recovered orphaned task");
        }

        Ok(recovered)
    }

    pub fn summary(&self) -> SdkResult<TaskSummary> {
        Ok(TaskSummary {
            pending: self.list_tasks_in_dir("pending")?.len(),
            in_progress: self.list_tasks_in_dir("in_progress")?.len(),
            completed: self.list_tasks_in_dir("completed")?.len(),
            failed: self.list_tasks_in_dir("failed")?.len(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct TaskSummary {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
    pub failed: usize,
}

impl TaskSummary {
    pub fn total(&self) -> usize {
        self.pending + self.in_progress + self.completed + self.failed
    }

    pub fn is_done(&self) -> bool {
        self.pending == 0 && self.in_progress == 0
    }
}
