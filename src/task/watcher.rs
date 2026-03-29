use std::sync::Arc;
use std::time::Duration;

use tokio::time;
use tracing::debug;

use crate::error::SdkResult;
use crate::task::store::{TaskStore, TaskSummary};

pub struct TaskWatcher {
    task_store: Arc<TaskStore>,
    poll_interval: Duration,
}

impl TaskWatcher {
    pub fn new(task_store: Arc<TaskStore>, poll_interval_ms: u64) -> Self {
        Self {
            task_store,
            poll_interval: Duration::from_millis(poll_interval_ms),
        }
    }

    pub async fn wait_for_completion(&self) -> SdkResult<TaskSummary> {
        loop {
            let summary = self.task_store.summary()?;

            debug!(
                pending = summary.pending,
                in_progress = summary.in_progress,
                completed = summary.completed,
                failed = summary.failed,
                "Task status"
            );

            if summary.is_done() {
                return Ok(summary);
            }

            time::sleep(self.poll_interval).await;
        }
    }

    pub fn current_summary(&self) -> SdkResult<TaskSummary> {
        self.task_store.summary()
    }
}
