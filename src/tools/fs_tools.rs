use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::error::{SdkError, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};

const DEFAULT_MAX_LINES: usize = 2000;

/// Known binary / image extensions that should not be read as text.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "svg", "tiff", "tif",
];

const BINARY_EXTENSIONS: &[&str] = &[
    "exe", "dll", "so", "dylib", "o", "a", "lib", "bin", "class", "jar",
    "war", "ear", "pyc", "pyo", "wasm", "deb", "rpm", "dmg", "iso",
    "zip", "gz", "bz2", "xz", "tar", "7z", "rar",
    "mp3", "mp4", "avi", "mov", "mkv", "flv", "wav", "ogg", "flac",
    "ttf", "otf", "woff", "woff2", "eot",
    "sqlite", "db",
];

const PDF_EXTENSION: &str = "pdf";

/// Read a file as a String, auto-detecting encoding.
/// Tries UTF-8 first; falls back to Shift-JIS if the bytes are not valid UTF-8
/// (common in Japanese enterprise Java codebases).
pub async fn read_file_auto_encoding(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = tokio::fs::read(path).await?;

    // Fast path: try UTF-8 first
    match String::from_utf8(bytes.clone()) {
        Ok(s) => return Ok(s),
        Err(_) => {}
    }

    // Fallback: decode as Shift-JIS
    let (cow, _encoding, had_errors) = encoding_rs::SHIFT_JIS.decode(&bytes);
    if !had_errors {
        return Ok(cow.into_owned());
    }

    // Last resort: lossy UTF-8
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Check if a file is likely binary by examining the first 512 bytes for null bytes.
fn is_binary_content(bytes: &[u8]) -> bool {
    let check_len = bytes.len().min(512);
    bytes[..check_len].contains(&0)
}

/// Get the file extension in lowercase.
fn file_extension(path: &std::path::Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
}

pub struct ReadFileTool {
    pub source_root: PathBuf,
    pub work_dir: PathBuf,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file from the local filesystem. By default reads up to \
                          2000 lines from the beginning. Use offset/limit to read specific \
                          ranges. Results use line number prefixes. Detects images, PDFs, \
                          and binary files. Can only read files, not directories."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-based, default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: 2000)"
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "Alias for limit (deprecated, use limit instead)"
                    }
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

        // Support both 'limit' (new) and 'max_lines' (legacy)
        let limit = arguments["limit"]
            .as_u64()
            .or_else(|| arguments["max_lines"].as_u64())
            .unwrap_or(DEFAULT_MAX_LINES as u64) as usize;

        // Offset is now 1-based (0 also accepted for backward compat)
        let raw_offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
        let offset = if raw_offset > 0 { raw_offset - 1 } else { 0 };

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

        // Check file extension for special types
        let ext = file_extension(&canonical);

        // Handle image files
        if let Some(ref e) = ext {
            if IMAGE_EXTENSIONS.contains(&e.as_str()) {
                let meta = tokio::fs::metadata(&canonical).await.ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                return Ok(json!({
                    "path": path,
                    "type": "image",
                    "format": e,
                    "size_bytes": size,
                    "note": "This is an image file. Content cannot be displayed as text."
                }));
            }
        }

        // Handle PDF files
        if ext.as_deref() == Some(PDF_EXTENSION) {
            let meta = tokio::fs::metadata(&canonical).await.ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            return Ok(json!({
                "path": path,
                "type": "pdf",
                "size_bytes": size,
                "note": "This is a PDF file. Use an external tool to extract text content."
            }));
        }

        // Handle known binary extensions
        if let Some(ref e) = ext {
            if BINARY_EXTENSIONS.contains(&e.as_str()) {
                let meta = tokio::fs::metadata(&canonical).await.ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                return Ok(json!({
                    "path": path,
                    "type": "binary",
                    "format": e,
                    "size_bytes": size,
                    "note": "This is a binary file. Content cannot be displayed as text."
                }));
            }
        }

        // Read file bytes and check for binary content
        let bytes = match tokio::fs::read(&canonical).await {
            Ok(b) => b,
            Err(e) => return Ok(json!({ "error": format!("Failed to read file: {}", e) })),
        };

        if is_binary_content(&bytes) {
            return Ok(json!({
                "path": path,
                "type": "binary",
                "size_bytes": bytes.len(),
                "note": "This file contains binary content and cannot be displayed as text."
            }));
        }

        // Decode text content
        let content = match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => {
                // Try Shift-JIS
                let (cow, _encoding, had_errors) = encoding_rs::SHIFT_JIS.decode(&bytes);
                if !had_errors {
                    cow.into_owned()
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            }
        };

        let all_lines: Vec<&str> = content.lines().collect();
        let total_lines = all_lines.len();

        // Handle empty file
        if total_lines == 0 {
            return Ok(json!({
                "content": "<system-reminder>Warning: the file exists but the contents are empty.</system-reminder>",
                "lines": 0,
                "path": path,
            }));
        }

        // Handle offset past end of file
        if offset >= total_lines {
            return Ok(json!({
                "content": format!(
                    "<system-reminder>Warning: the file exists but is shorter than the provided \
                     offset ({}). The file has {} lines.</system-reminder>",
                    offset + 1,
                    total_lines
                ),
                "lines": total_lines,
                "path": path,
            }));
        }

        let start = offset;
        let end = (start + limit).min(total_lines);
        let slice = &all_lines[start..end];
        let truncated = end < total_lines;

        // Format with line numbers: right-padded to 6 chars + arrow separator
        // Matches Claude Code format: "     1→content" (compact: "1\tcontent")
        let numbered_lines: Vec<String> = slice
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let num = start + i + 1;
                let num_str = num.to_string();
                if num_str.len() >= 6 {
                    format!("{}→{}", num_str, line)
                } else {
                    format!("{:>6}→{}", num, line)
                }
            })
            .collect();
        let result_content = numbered_lines.join("\n");

        let mut result = json!({
            "content": result_content,
            "lines": total_lines,
            "path": path,
            "offset": start + 1,
            "lines_returned": slice.len(),
        });

        if truncated {
            result["truncated"] = json!(true);
            result["next_offset"] = json!(end + 1);
            result["note"] = json!(format!(
                "File has {} lines, showing lines {}-{}. Use offset={} to read more.",
                total_lines,
                start + 1,
                end,
                end + 1
            ));
        }

        Ok(result)
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
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| SdkError::ToolExecution {
                    tool_name: "write_file".to_string(),
                    message: format!("Failed to create directories: {}", e),
                })?;
        }

        tokio::fs::write(&full_path, content)
            .await
            .map_err(|e| SdkError::ToolExecution {
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
        let mut dir = tokio::fs::read_dir(&full_path)
            .await
            .map_err(|e| SdkError::ToolExecution {
                tool_name: "list_directory".to_string(),
                message: format!("Failed to read directory: {}", e),
            })?;

        while let Some(entry) = dir.next_entry().await.map_err(|e| SdkError::ToolExecution {
            tool_name: "list_directory".to_string(),
            message: format!("Failed to read entry: {}", e),
        })? {
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type().await.ok();
            let kind = if ft.as_ref().is_some_and(|f| f.is_dir()) {
                "directory"
            } else {
                "file"
            };
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
