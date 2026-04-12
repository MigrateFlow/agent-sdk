use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use sdk_core::error::{AgentId, SdkError, SdkResult};
use sdk_core::traits::tool::{Tool, ToolDefinition};
use sdk_core::types::task::Task;
use sdk_task::task::store::TaskStore;

pub struct CreateTaskTool {
    pub task_store: Arc<TaskStore>,
    pub agent_id: AgentId,
}

#[derive(Debug, Deserialize)]
struct CreateTaskRequest {
    title: String,
    description: String,
    target_file: String,
    #[serde(default)]
    priority: u32,
}

#[async_trait]
impl Tool for CreateTaskTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "create_task".to_string(),
            description: "Create a new task for another teammate to work on. Use this when \
                you discover additional work that should be done in parallel by another agent. \
                Only create tasks for genuinely independent work items."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short task title" },
                    "description": { "type": "string", "description": "Detailed task description" },
                    "target_file": { "type": "string", "description": "Primary output file for this task" },
                    "priority": { "type": "integer", "description": "Priority (lower = higher, default: 0)" }
                },
                "required": ["title", "description", "target_file"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let req: CreateTaskRequest =
            serde_json::from_value(arguments).map_err(|e| SdkError::ToolExecution {
                tool_name: "create_task".to_string(),
                message: format!("Invalid arguments: {}", e),
            })?;

        let task = Task::new(
            &Uuid::new_v4().to_string(),
            &req.title,
            &req.description,
            &req.target_file,
        )
        .with_priority(req.priority);

        self.task_store.create_task(&task)?;

        Ok(json!({
            "status": "created",
            "task_id": task.id.to_string(),
            "title": req.title,
            "message": "Task created and available for another teammate to claim."
        }))
    }
}
