use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::error::SdkResult;
use crate::traits::tool::{Tool, ToolDefinition};

pub struct SearchFilesTool {
    pub source_root: PathBuf,
}

#[async_trait]
impl Tool for SearchFilesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "search_files".to_string(),
            description: "Search for files by glob pattern and/or search for text content within files.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_pattern": { "type": "string", "description": "Glob pattern (e.g., '**/*.rs')" },
                    "content_pattern": { "type": "string", "description": "Text pattern to search within files" },
                    "max_results": { "type": "integer", "description": "Maximum results (default: 20)" }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let file_pattern = arguments["file_pattern"].as_str();
        let content_pattern = arguments["content_pattern"].as_str();
        let max_results = arguments["max_results"].as_u64().unwrap_or(20) as usize;

        if file_pattern.is_none() && content_pattern.is_none() {
            return Ok(json!({ "error": "At least one of 'file_pattern' or 'content_pattern' must be provided" }));
        }

        let mut matching_files: Vec<PathBuf> = Vec::new();

        if let Some(pattern) = file_pattern {
            let full_pattern = format!("{}/{}", self.source_root.display(), pattern);
            match glob::glob(&full_pattern) {
                Ok(paths) => {
                    for entry in paths.flatten() {
                        if entry.is_file() {
                            matching_files.push(entry);
                        }
                    }
                }
                Err(e) => return Ok(json!({ "error": format!("Invalid glob pattern: {}", e) })),
            }
        }

        if file_pattern.is_none() && content_pattern.is_some() {
            let full_pattern = format!("{}/**/*", self.source_root.display());
            if let Ok(paths) = glob::glob(&full_pattern) {
                for entry in paths.flatten() {
                    if entry.is_file() {
                        matching_files.push(entry);
                    }
                }
            }
        }

        if let Some(pattern) = content_pattern {
            let mut results = Vec::new();

            for file_path in &matching_files {
                if results.len() >= max_results {
                    break;
                }

                if let Ok(content) = tokio::fs::read_to_string(file_path).await {
                    let mut file_matches = Vec::new();
                    for (line_num, line) in content.lines().enumerate() {
                        if line.contains(pattern) {
                            file_matches.push(json!({ "line": line_num + 1, "text": line.trim() }));
                        }
                    }
                    if !file_matches.is_empty() {
                        let rel_path = file_path
                            .strip_prefix(&self.source_root)
                            .unwrap_or(file_path)
                            .to_string_lossy();
                        results.push(json!({ "file": rel_path, "matches": file_matches }));
                    }
                }
            }

            Ok(json!({
                "results": results,
                "total_files_searched": matching_files.len(),
                "files_with_matches": results.len()
            }))
        } else {
            let results: Vec<String> = matching_files
                .iter()
                .take(max_results)
                .map(|p| {
                    p.strip_prefix(&self.source_root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .to_string()
                })
                .collect();

            Ok(json!({ "files": results, "total_matches": matching_files.len(), "shown": results.len() }))
        }
    }
}
