use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::error::{SdkError, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};

const DEFAULT_MAX_LINES: usize = 500;

pub struct ReadFileTool {
    pub source_root: PathBuf,
    pub work_dir: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read the contents of a file. The path is relative to the repository root. For large files, use offset/max_lines to read in chunks.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path to the file" },
                    "offset": { "type": "integer", "description": "Line number to start reading from (0-based, default: 0)" },
                    "max_lines": { "type": "integer", "description": "Maximum number of lines to return (default: 500)" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let path = arguments["path"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "read_file".to_string(),
                message: "Missing 'path' argument".to_string(),
            })?;

        let offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
        let max_lines = arguments["max_lines"].as_u64().unwrap_or(DEFAULT_MAX_LINES as u64) as usize;

        let full_path = self.source_root.join(path);
        let full_path = if full_path.exists() {
            full_path
        } else {
            let work_path = self.work_dir.join(path);
            if work_path.exists() {
                work_path
            } else {
                return Ok(json!({ "error": format!("File not found: {}", path) }));
            }
        };

        let canonical = full_path.canonicalize().map_err(|e| SdkError::ToolExecution {
            tool_name: "read_file".to_string(),
            message: format!("Cannot resolve path: {}", e),
        })?;

        let source_canonical = self.source_root.canonicalize().unwrap_or_else(|_| self.source_root.clone());
        let work_canonical = self.work_dir.canonicalize().unwrap_or_else(|_| self.work_dir.clone());

        if !canonical.starts_with(&source_canonical) && !canonical.starts_with(&work_canonical) {
            return Ok(json!({ "error": "Path escapes allowed directories" }));
        }

        match tokio::fs::read_to_string(&canonical).await {
            Ok(content) => {
                let all_lines: Vec<&str> = content.lines().collect();
                let total_lines = all_lines.len();
                let start = offset.min(total_lines);
                let end = (start + max_lines).min(total_lines);
                let slice = &all_lines[start..end];
                let truncated = end < total_lines;
                let result_content = slice.join("\n");

                let mut result = json!({
                    "content": result_content,
                    "lines": total_lines,
                    "path": path,
                    "offset": start,
                    "lines_returned": slice.len(),
                });

                if truncated {
                    result["truncated"] = json!(true);
                    result["next_offset"] = json!(end);
                    result["note"] = json!(format!(
                        "File has {} lines, showing lines {}-{}. Use offset={} to read more.",
                        total_lines, start + 1, end, end
                    ));
                }

                Ok(result)
            }
            Err(e) => Ok(json!({ "error": format!("Failed to read file: {}", e) })),
        }
    }
}

pub struct WriteFileTool {
    pub work_dir: PathBuf,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Write content to a file in the output directory. Creates parent directories as needed.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path for the output file" },
                    "content": { "type": "string", "description": "The full file content to write" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let path = arguments["path"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "write_file".to_string(),
                message: "Missing 'path' argument".to_string(),
            })?;

        let content = arguments["content"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "write_file".to_string(),
                message: "Missing 'content' argument".to_string(),
            })?;

        let full_path = self.work_dir.join(path);

        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| SdkError::ToolExecution {
                tool_name: "write_file".to_string(),
                message: format!("Failed to create directories: {}", e),
            })?;
        }

        tokio::fs::write(&full_path, content).await.map_err(|e| SdkError::ToolExecution {
            tool_name: "write_file".to_string(),
            message: format!("Failed to write file: {}", e),
        })?;

        let lines = content.lines().count();
        Ok(json!({
            "path": path,
            "lines_written": lines,
            "bytes_written": content.len()
        }))
    }
}

pub struct ListDirectoryTool {
    pub source_root: PathBuf,
    pub work_dir: PathBuf,
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_directory".to_string(),
            description: "List files and subdirectories in a directory. Path is relative to repository root.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative directory path (use '.' for root)" }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let path = arguments["path"].as_str().unwrap_or(".");

        let full_path = self.source_root.join(path);
        if !full_path.is_dir() {
            return Ok(json!({ "error": format!("Not a directory: {}", path) }));
        }

        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(&full_path).await.map_err(|e| SdkError::ToolExecution {
            tool_name: "list_directory".to_string(),
            message: format!("Failed to read directory: {}", e),
        })?;

        while let Some(entry) = dir.next_entry().await.map_err(|e| SdkError::ToolExecution {
            tool_name: "list_directory".to_string(),
            message: format!("Failed to read entry: {}", e),
        })? {
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type().await.ok();
            let kind = if ft.as_ref().is_some_and(|f| f.is_dir()) { "directory" } else { "file" };
            entries.push(json!({ "name": name, "type": kind }));
        }

        entries.sort_by(|a, b| {
            let a_name = a["name"].as_str().unwrap_or("");
            let b_name = b["name"].as_str().unwrap_or("");
            a_name.cmp(b_name)
        });

        Ok(json!({ "path": path, "entries": entries, "count": entries.len() }))
    }
}