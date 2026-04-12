use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use sdk_core::error::{SdkError, SdkResult};
use sdk_core::traits::tool::{Tool, ToolDefinition};

pub struct GlobTool {
    pub source_root: PathBuf,
    pub max_results: usize,
}

impl GlobTool {
    pub fn new(source_root: PathBuf) -> Self {
        Self {
            source_root,
            max_results: sdk_core::config::ToolLimitsConfig::default().glob_max_results,
        }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn is_read_only(&self) -> bool { true }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".to_string(),
            description: "Fast file pattern matching. Returns file paths matching a glob \
                          pattern, sorted by modification time (most recent first). Use this \
                          to find files by name or extension."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match (e.g., '**/*.rs', 'src/**/*.ts')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Subdirectory to search within (relative to project root, default: project root)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let pattern = arguments["pattern"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "glob".to_string(),
                message: "Missing 'pattern' argument".to_string(),
            })?;

        let sub_path = arguments["path"].as_str().unwrap_or("");

        let base = if sub_path.is_empty() {
            self.source_root.clone()
        } else {
            self.source_root.join(sub_path)
        };

        let full_pattern = base.join(pattern).to_string_lossy().to_string();

        let paths = glob::glob(&full_pattern).map_err(|e| SdkError::ToolExecution {
            tool_name: "glob".to_string(),
            message: format!("Invalid glob pattern: {}", e),
        })?;

        // Collect paths with modification times for sorting
        let mut entries: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in paths {
            let path = match entry {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !path.is_file() {
                continue;
            }
            let mtime = path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.push((path, mtime));
        }

        let total_matches = entries.len();

        // Sort by mtime descending (most recent first)
        entries.sort_by(|a, b| b.1.cmp(&a.1));

        // Cap results
        entries.truncate(self.max_results);

        let files: Vec<String> = entries
            .iter()
            .filter_map(|(p, _)| {
                p.strip_prefix(&self.source_root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().to_string())
            })
            .collect();

        let shown = files.len();

        Ok(json!({
            "files": files,
            "total_matches": total_matches,
            "shown": shown
        }))
    }
}
