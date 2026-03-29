use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::error::SdkResult;
use crate::traits::tool::{Tool, ToolDefinition};
use crate::task::store::TaskStore;

pub struct GetTaskContextTool {
    pub task_store: Arc<TaskStore>,
}

#[async_trait]
impl Tool for GetTaskContextTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_task_context".to_string(),
            description: "Get the result and notes from a completed task.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "The UUID of the completed task" }
                },
                "required": ["task_id"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let task_id_str = arguments["task_id"].as_str().unwrap_or("");
        if task_id_str.is_empty() {
            return Ok(json!({"error": "Missing 'task_id' argument"}));
        }

        let task_id: uuid::Uuid = match task_id_str.parse() {
            Ok(id) => id,
            Err(_) => return Ok(json!({"error": "Invalid task_id format"})),
        };

        match self.task_store.read_task(task_id) {
            Ok(task) => {
                let status = format!("{:?}", task.status);
                Ok(json!({
                    "task_id": task_id_str,
                    "title": task.title,
                    "status": status,
                    "target_file": task.target_file.to_string_lossy(),
                    "result": task.result.as_ref().map(|r| json!({
                        "notes": r.notes,
                        "file_changes": r.file_changes.len(),
                        "tokens_used": r.llm_tokens_used
                    }))
                }))
            }
            Err(_) => Ok(json!({ "error": format!("Task {} not found", task_id_str) })),
        }
    }
}

pub struct ListCompletedTasksTool {
    pub task_store: Arc<TaskStore>,
}

#[async_trait]
impl Tool for ListCompletedTasksTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_completed_tasks".to_string(),
            description: "List all completed tasks with their IDs, titles, and target files.".to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let tasks = self.task_store.list_tasks_in_dir("completed")?;
        let summaries: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                json!({
                    "task_id": t.id.to_string(),
                    "title": t.title,
                    "target_file": t.target_file.to_string_lossy(),
                    "notes": t.result.as_ref().map(|r| r.notes.as_str()).unwrap_or("")
                })
            })
            .collect();

        Ok(json!({ "tasks": summaries, "count": summaries.len() }))
    }
}
