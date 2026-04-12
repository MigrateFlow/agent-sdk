use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use sdk_core::error::{SdkError, SdkResult};
use sdk_core::traits::tool::{Tool, ToolDefinition};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

pub struct TodoWriteTool {
    pub items: Arc<Mutex<Vec<TodoItem>>>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "todo_write".to_string(),
            description: "Create and manage a task list to track progress on multi-step work. \
                          Send the complete list each time (full replacement). Use this to \
                          break complex tasks into steps and show progress."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "The complete todo list (replaces existing list)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Unique identifier for the item"
                                },
                                "title": {
                                    "type": "string",
                                    "description": "Description of the task"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Current status of the task"
                                }
                            },
                            "required": ["id", "title", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let items_val = arguments
            .get("items")
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "todo_write".to_string(),
                message: "Missing 'items' argument".to_string(),
            })?;

        let new_items: Vec<TodoItem> =
            serde_json::from_value(items_val.clone()).map_err(|e| SdkError::ToolExecution {
                tool_name: "todo_write".to_string(),
                message: format!("Invalid items format: {}", e),
            })?;

        let count = new_items.len();
        let completed = new_items
            .iter()
            .filter(|i| matches!(i.status, TodoStatus::Completed))
            .count();
        let in_progress = new_items
            .iter()
            .filter(|i| matches!(i.status, TodoStatus::InProgress))
            .count();

        let mut items = self.items.lock().await;
        *items = new_items;

        Ok(json!({
            "count": count,
            "completed": completed,
            "in_progress": in_progress,
            "pending": count - completed - in_progress
        }))
    }
}
