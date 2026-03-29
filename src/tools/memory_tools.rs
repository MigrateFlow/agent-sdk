use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::error::{AgentId, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};
use crate::agent::memory::MemoryStore;

pub struct ReadMemoryTool {
    pub memory_store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for ReadMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_memory".to_string(),
            description: "Read a value from the shared memory store.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "The memory key to read" }
                },
                "required": ["key"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let key = arguments["key"].as_str().unwrap_or("");
        if key.is_empty() {
            return Ok(json!({"error": "Missing 'key' argument"}));
        }

        match self.memory_store.read(key)? {
            Some(entry) => Ok(json!({
                "key": entry.key,
                "value": entry.value,
                "written_by": entry.written_by.to_string(),
                "written_at": entry.written_at.to_rfc3339()
            })),
            None => Ok(json!({ "key": key, "value": null, "found": false })),
        }
    }
}

pub struct WriteMemoryTool {
    pub memory_store: Arc<MemoryStore>,
    pub agent_id: AgentId,
}

#[async_trait]
impl Tool for WriteMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_memory".to_string(),
            description: "Write a value to the shared memory store.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "The memory key" },
                    "value": { "description": "The value to store" }
                },
                "required": ["key", "value"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let key = arguments["key"].as_str().unwrap_or("");
        if key.is_empty() {
            return Ok(json!({"error": "Missing 'key' argument"}));
        }

        let value = arguments.get("value").cloned().unwrap_or(serde_json::Value::Null);
        self.memory_store.write(key, value, self.agent_id)?;

        Ok(json!({ "key": key, "stored": true }))
    }
}

pub struct ListMemoryTool {
    pub memory_store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for ListMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_memory".to_string(),
            description: "List all keys in the shared memory store.".to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let keys = self.memory_store.list_keys()?;
        Ok(json!({ "keys": keys, "count": keys.len() }))
    }
}
