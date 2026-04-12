use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use sdk_core::error::{SdkError, SdkResult};
use sdk_core::traits::tool::{Tool, ToolDefinition};

pub struct GrepTool {
    pub source_root: PathBuf,
    pub default_head_limit: usize,
    pub max_file_size: u64,
}

impl GrepTool {
    pub fn new(source_root: PathBuf) -> Self {
        let defaults = sdk_core::config::ToolLimitsConfig::default();
        Self {
            source_root,
            default_head_limit: defaults.grep_head_limit,
            max_file_size: defaults.grep_max_file_size,
        }
    }
}

/// Check if a file is likely binary by reading the first 512 bytes.
fn is_likely_binary(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return true;
    };
    let mut buf = [0u8; 512];
    let Ok(n) = f.read(&mut buf) else {
        return true;
    };
    buf[..n].contains(&0)
}

/// Walk a directory recursively, collecting file paths that pass filters.
fn walk_files(root: &std::path::Path, file_glob: Option<&glob::Pattern>, max_file_size: u64) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_files_inner(root, root, file_glob, max_file_size, &mut files);
    files
}

fn walk_files_inner(
    current: &std::path::Path,
    root: &std::path::Path,
    file_glob: Option<&glob::Pattern>,
    max_file_size: u64,
    out: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden dirs and common uninteresting dirs
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "__pycache__"
        {
            continue;
        }

        if path.is_dir() {
            walk_files_inner(&path, root, file_glob, max_file_size, out);
        } else if path.is_file() {
            // Apply glob filter on relative path
            if let Some(pattern) = file_glob {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
                // Match against filename or full relative path
                if !pattern.matches(&rel) && !pattern.matches(&name) {
                    continue;
                }
            }

            // Skip large and binary files
            if let Ok(meta) = path.metadata() {
                if meta.len() > max_file_size {
                    continue;
                }
            }
            if is_likely_binary(&path) {
                continue;
            }

            out.push(path);
        }
    }
}

/// Simple pattern matcher that supports case-insensitive matching.
/// Uses string containment (not regex) to avoid adding dependencies.
struct PatternMatcher {
    pattern: String,
    case_insensitive: bool,
}

impl PatternMatcher {
    fn new(pattern: &str, case_insensitive: bool) -> Self {
        Self {
            pattern: if case_insensitive {
                pattern.to_lowercase()
            } else {
                pattern.to_string()
            },
            case_insensitive,
        }
    }

    fn is_match(&self, text: &str) -> bool {
        if self.case_insensitive {
            text.to_lowercase().contains(&self.pattern)
        } else {
            text.contains(&self.pattern)
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search file contents for a text pattern. Supports context lines, \
                          multiple output modes, file filtering, and case-insensitive search. \
                          Skips binary files and files larger than 1 MB."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Text pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Subdirectory to search within (relative to project root)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "File glob filter (e.g., '*.rs', '*.{ts,tsx}')"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Number of context lines before and after each match"
                    },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "description": "Output mode: 'content' shows matching lines, 'files_with_matches' shows file paths (default), 'count' shows match counts"
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 250)"
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let pattern_str = arguments["pattern"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "grep".to_string(),
                message: "Missing 'pattern' argument".to_string(),
            })?;

        let case_insensitive = arguments["case_insensitive"].as_bool().unwrap_or(false);
        let context = arguments["context"].as_u64().unwrap_or(0) as usize;
        let output_mode = arguments["output_mode"]
            .as_str()
            .unwrap_or("files_with_matches");
        let head_limit = arguments["head_limit"]
            .as_u64()
            .unwrap_or(self.default_head_limit as u64) as usize;
        let sub_path = arguments["path"].as_str().unwrap_or("");
        let file_glob_str = arguments["glob"].as_str();

        let matcher = PatternMatcher::new(pattern_str, case_insensitive);

        // Build file glob filter
        let file_glob = file_glob_str
            .map(|g| glob::Pattern::new(g).unwrap_or_else(|_| glob::Pattern::new("*").unwrap()));

        let search_root = if sub_path.is_empty() {
            self.source_root.clone()
        } else {
            self.source_root.join(sub_path)
        };

        if !search_root.is_dir() {
            return Ok(json!({ "error": format!("Directory not found: {}", sub_path) }));
        }

        let files = walk_files(&search_root, file_glob.as_ref(), self.max_file_size);

        match output_mode {
            "content" => {
                let mut results: Vec<serde_json::Value> = Vec::new();

                'outer: for file_path in &files {
                    let Ok(content) = std::fs::read_to_string(file_path) else {
                        continue;
                    };
                    let lines: Vec<&str> = content.lines().collect();

                    for (line_idx, line) in lines.iter().enumerate() {
                        if !matcher.is_match(line) {
                            continue;
                        }

                        let start = line_idx.saturating_sub(context);
                        let end = (line_idx + context + 1).min(lines.len());

                        let context_lines: Vec<serde_json::Value> = (start..end)
                            .map(|i| {
                                json!({
                                    "line_number": i + 1,
                                    "content": lines[i],
                                    "is_match": i == line_idx
                                })
                            })
                            .collect();

                        let rel_path = file_path
                            .strip_prefix(&self.source_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                            .to_string();

                        results.push(json!({
                            "file": rel_path,
                            "line_number": line_idx + 1,
                            "lines": context_lines
                        }));

                        if results.len() >= head_limit {
                            break 'outer;
                        }
                    }
                }

                Ok(json!({
                    "matches": results,
                    "total_shown": results.len()
                }))
            }
            "count" => {
                let mut results: Vec<serde_json::Value> = Vec::new();

                for file_path in &files {
                    let Ok(content) = std::fs::read_to_string(file_path) else {
                        continue;
                    };
                    let count = content.lines().filter(|l| matcher.is_match(l)).count();
                    if count > 0 {
                        let rel_path = file_path
                            .strip_prefix(&self.source_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                            .to_string();
                        results.push(json!({ "file": rel_path, "count": count }));

                        if results.len() >= head_limit {
                            break;
                        }
                    }
                }

                Ok(json!({
                    "results": results,
                    "files_with_matches": results.len()
                }))
            }
            _ => {
                // files_with_matches (default)
                let mut matched_files: Vec<String> = Vec::new();

                for file_path in &files {
                    let Ok(content) = std::fs::read_to_string(file_path) else {
                        continue;
                    };
                    if content.lines().any(|l| matcher.is_match(l)) {
                        let rel_path = file_path
                            .strip_prefix(&self.source_root)
                            .unwrap_or(file_path)
                            .to_string_lossy()
                            .to_string();
                        matched_files.push(rel_path);

                        if matched_files.len() >= head_limit {
                            break;
                        }
                    }
                }

                Ok(json!({
                    "files": matched_files,
                    "total_matches": matched_files.len()
                }))
            }
        }
    }
}
