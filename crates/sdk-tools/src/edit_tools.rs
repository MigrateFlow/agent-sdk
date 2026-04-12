use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use sdk_core::error::{SdkError, SdkResult};
use sdk_core::traits::tool::{Tool, ToolDefinition};

use super::fs_tools::read_file_auto_encoding;

pub struct EditFileTool {
    pub source_root: PathBuf,
    pub work_dir: PathBuf,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Perform a surgical text replacement in a file. Finds the exact \
                          `old_string` and replaces it with `new_string`. Much more efficient \
                          than rewriting entire files. The edit will fail if `old_string` is \
                          not found, or if it appears more than once (unless `replace_all` is \
                          set to true)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to edit"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to find and replace"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurrences (default: false, which requires exactly one match)"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let path = arguments["path"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "edit_file".to_string(),
                message: "Missing 'path' argument".to_string(),
            })?;

        let old_string = arguments["old_string"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "edit_file".to_string(),
                message: "Missing 'old_string' argument".to_string(),
            })?;

        let new_string = arguments["new_string"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "edit_file".to_string(),
                message: "Missing 'new_string' argument".to_string(),
            })?;

        let replace_all = arguments["replace_all"].as_bool().unwrap_or(false);

        // Resolve path: try source_root first, then work_dir
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

        // Security: ensure path doesn't escape allowed directories
        let canonical = full_path.canonicalize().map_err(|e| SdkError::ToolExecution {
            tool_name: "edit_file".to_string(),
            message: format!("Cannot resolve path: {}", e),
        })?;

        let source_canonical = self
            .source_root
            .canonicalize()
            .unwrap_or_else(|_| self.source_root.clone());
        let work_canonical = self
            .work_dir
            .canonicalize()
            .unwrap_or_else(|_| self.work_dir.clone());

        if !canonical.starts_with(&source_canonical) && !canonical.starts_with(&work_canonical) {
            return Ok(json!({ "error": "Path escapes allowed directories" }));
        }

        // Read current content
        let content = match read_file_auto_encoding(&canonical).await {
            Ok(c) => c,
            Err(e) => return Ok(json!({ "error": format!("Failed to read file: {}", e) })),
        };

        if old_string == new_string {
            return Ok(json!({ "error": "old_string and new_string are identical" }));
        }

        // Count occurrences
        let count = content.matches(old_string).count();

        if count == 0 {
            return Ok(json!({
                "error": "old_string not found in file",
                "path": path
            }));
        }

        if count > 1 && !replace_all {
            return Ok(json!({
                "error": format!(
                    "old_string found {} times in the file. Provide more surrounding \
                     context to make it unique, or set replace_all: true to replace \
                     all occurrences.",
                    count
                ),
                "path": path,
                "occurrences": count
            }));
        }

        // Perform replacement
        let replacements = if replace_all { count } else { 1 };
        let new_content = content.replacen(old_string, new_string, replacements);

        // Write back
        tokio::fs::write(&canonical, &new_content)
            .await
            .map_err(|e| SdkError::ToolExecution {
                tool_name: "edit_file".to_string(),
                message: format!("Failed to write file: {}", e),
            })?;

        Ok(json!({
            "path": path,
            "replacements_made": replacements,
            "old_length": content.len(),
            "new_length": new_content.len()
        }))
    }
}
