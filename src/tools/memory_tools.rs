use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agent::memory::MemoryStore;
use crate::error::{AgentId, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};
use crate::types::memory::MemoryType;

pub struct ReadMemoryTool {
    pub memory_store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for ReadMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_memory".to_string(),
            description: "Read a memory entry from the persistent store. Returns the full \
                          content, type, description, and metadata."
                .to_string(),
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
                "name": entry.name,
                "description": entry.description,
                "memory_type": entry.memory_type,
                "content": entry.content,
                "written_by": entry.written_by.to_string(),
                "written_at": entry.written_at.to_rfc3339()
            })),
            None => Ok(json!({ "key": key, "found": false })),
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
            description: "Write a memory entry to the persistent store. Memories persist \
                          across sessions. Use memory_type to categorize: 'user' for user \
                          preferences, 'feedback' for corrections, 'project' for ongoing work \
                          context, 'reference' for external resource pointers."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Unique key for this memory entry"
                    },
                    "content": {
                        "type": "string",
                        "description": "The memory content to store"
                    },
                    "name": {
                        "type": "string",
                        "description": "Human-readable name (optional, defaults to key)"
                    },
                    "description": {
                        "type": "string",
                        "description": "One-line description for the memory index"
                    },
                    "memory_type": {
                        "type": "string",
                        "enum": ["user", "feedback", "project", "reference"],
                        "description": "Memory category (default: 'reference')"
                    },
                    "value": {
                        "description": "Legacy: JSON value to store (use 'content' instead)"
                    }
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

        // Support both new 'content' string and legacy 'value' JSON
        let content = if let Some(c) = arguments["content"].as_str() {
            c.to_string()
        } else if let Some(val) = arguments.get("value") {
            match val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => {
                    return Ok(json!({"error": "Missing 'content' or 'value' argument"}));
                }
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            }
        } else {
            return Ok(json!({"error": "Missing 'content' or 'value' argument"}));
        };

        let name = arguments["name"].as_str().map(String::from);
        let description = arguments["description"].as_str().map(String::from);
        let memory_type = arguments["memory_type"]
            .as_str()
            .and_then(MemoryType::parse)
            .unwrap_or_default();

        self.memory_store
            .write(key, &content, self.agent_id, name, description, memory_type)?;

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
            description: "List all memory entries with their types and descriptions.".to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let keys = self.memory_store.list_keys()?;
        let mut entries = Vec::new();

        for key in &keys {
            if let Some(entry) = self.memory_store.read(key)? {
                entries.push(json!({
                    "key": entry.key,
                    "name": entry.name,
                    "memory_type": entry.memory_type,
                    "description": entry.description,
                }));
            }
        }

        Ok(json!({ "entries": entries, "count": entries.len() }))
    }
}

pub struct SearchMemoryTool {
    pub memory_store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for SearchMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search_memory".to_string(),
            description: "Search memories by type and/or keyword. Returns matching entries \
                          with their content."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "memory_type": {
                        "type": "string",
                        "enum": ["user", "feedback", "project", "reference"],
                        "description": "Filter by memory type"
                    },
                    "keyword": {
                        "type": "string",
                        "description": "Search keyword (matches against key, content, and description)"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let memory_type = arguments["memory_type"]
            .as_str()
            .and_then(MemoryType::parse);
        let keyword = arguments["keyword"].as_str();

        let results = self
            .memory_store
            .search(memory_type.as_ref(), keyword)?;

        let entries: Vec<serde_json::Value> = results
            .iter()
            .map(|e| {
                json!({
                    "key": e.key,
                    "name": e.name,
                    "memory_type": e.memory_type,
                    "description": e.description,
                    "content": e.content,
                    "written_at": e.written_at.to_rfc3339()
                })
            })
            .collect();

        Ok(json!({ "results": entries, "count": entries.len() }))
    }
}

pub struct DeleteMemoryTool {
    pub memory_store: Arc<MemoryStore>,
}

#[async_trait]
impl Tool for DeleteMemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "delete_memory".to_string(),
            description: "Delete a memory entry by key. Use this to remove outdated or \
                          incorrect memories."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "The memory key to delete" }
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

        let deleted = self.memory_store.delete(key)?;
        Ok(json!({ "key": key, "deleted": deleted }))
    }
}
