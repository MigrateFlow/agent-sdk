//! Code-intelligence tools that delegate to a shared `LspManager`.
//!
//! Three tools are exposed:
//!
//! - `lsp_goto_definition`
//! - `lsp_find_references`
//! - `lsp_document_symbols`
//!
//! All three infer the language from the file's extension, call
//! `textDocument/didOpen` so the server has the buffer, then issue the
//! corresponding LSP request. Positions are 0-indexed, matching the LSP
//! protocol.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{Mutex, MutexGuard};

use crate::error::{SdkError, SdkResult};
use crate::lsp::{file_uri_for, language_id_for_path, LspManager};
use crate::traits::tool::{Tool, ToolDefinition};

/// Handle that lets multiple tools share the same underlying `LspManager`.
pub type SharedLspManager = Arc<Mutex<LspManager>>;

fn resolve_path(work_dir: &Path, source_root: &Path, raw: &str) -> SdkResult<PathBuf> {
    let as_path = Path::new(raw);
    let candidate = if as_path.is_absolute() {
        as_path.to_path_buf()
    } else {
        let from_source = source_root.join(as_path);
        if from_source.exists() {
            from_source
        } else {
            work_dir.join(as_path)
        }
    };
    if !candidate.exists() {
        return Err(SdkError::ToolExecution {
            tool_name: "lsp".to_string(),
            message: format!("File not found: {}", candidate.display()),
        });
    }
    Ok(candidate)
}

fn string_arg<'a>(args: &'a Value, key: &str, tool: &str) -> SdkResult<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| SdkError::ToolExecution {
            tool_name: tool.to_string(),
            message: format!("Missing '{key}' argument"),
        })
}

fn u32_arg(args: &Value, key: &str, tool: &str) -> SdkResult<u32> {
    let value = args.get(key).and_then(Value::as_u64).ok_or_else(|| {
        SdkError::ToolExecution {
            tool_name: tool.to_string(),
            message: format!("Missing or non-integer '{key}' argument"),
        }
    })?;
    Ok(value as u32)
}

/// Acquire the manager, spawn the right client, open the buffer, and return a
/// live guard plus URI so the caller can issue the follow-up request without
/// re-locking. `language` is inferred from the file extension.
async fn open_and_lock<'a>(
    manager: &'a SharedLspManager,
    work_dir: &Path,
    source_root: &Path,
    raw_file: &str,
) -> SdkResult<(MutexGuard<'a, LspManager>, &'static str, String)> {
    let path = resolve_path(work_dir, source_root, raw_file)?;
    let language = language_id_for_path(&path).ok_or_else(|| SdkError::ToolExecution {
        tool_name: "lsp".to_string(),
        message: format!("Unsupported file extension: {}", path.display()),
    })?;
    let uri = file_uri_for(&path)?;
    let text = tokio::fs::read_to_string(&path).await.map_err(SdkError::Io)?;

    let mut guard = manager.lock().await;
    let client = guard.client_for_language(language).await?;
    client.did_open(&uri, language, &text).await?;
    Ok((guard, language, uri))
}


/// `lsp_goto_definition`
pub struct LspGotoDefinitionTool {
    pub manager: SharedLspManager,
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
}

#[async_trait]
impl Tool for LspGotoDefinitionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp_goto_definition".to_string(),
            description: "Ask the configured language server for the definition of the symbol at the given position. Line and column are 0-indexed."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Path to the source file (relative to the repo root or absolute)" },
                    "line": { "type": "integer", "description": "0-indexed line number", "minimum": 0 },
                    "column": { "type": "integer", "description": "0-indexed UTF-16 column", "minimum": 0 }
                },
                "required": ["file", "line", "column"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> SdkResult<Value> {
        let tool = "lsp_goto_definition";
        let raw_file = string_arg(&arguments, "file", tool)?;
        let line = u32_arg(&arguments, "line", tool)?;
        let column = u32_arg(&arguments, "column", tool)?;

        let (mut guard, language, uri) =
            open_and_lock(&self.manager, &self.work_dir, &self.source_root, raw_file).await?;
        let client = guard.client_for_language(language).await?;
        let locations = client.goto_definition(&uri, line, column).await?;
        Ok(json!({ "locations": locations }))
    }
}

/// `lsp_find_references`
pub struct LspFindReferencesTool {
    pub manager: SharedLspManager,
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
}

#[async_trait]
impl Tool for LspFindReferencesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp_find_references".to_string(),
            description: "Find all references to the symbol at the given position via the configured language server. Line and column are 0-indexed."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string" },
                    "line": { "type": "integer", "minimum": 0 },
                    "column": { "type": "integer", "minimum": 0 },
                    "include_declaration": { "type": "boolean", "description": "Whether to include the declaration itself (default: true)" }
                },
                "required": ["file", "line", "column"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> SdkResult<Value> {
        let tool = "lsp_find_references";
        let raw_file = string_arg(&arguments, "file", tool)?;
        let line = u32_arg(&arguments, "line", tool)?;
        let column = u32_arg(&arguments, "column", tool)?;
        let include_declaration = arguments
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let (mut guard, language, uri) =
            open_and_lock(&self.manager, &self.work_dir, &self.source_root, raw_file).await?;
        let client = guard.client_for_language(language).await?;
        let locations = client
            .find_references(&uri, line, column, include_declaration)
            .await?;
        Ok(json!({ "locations": locations }))
    }
}

/// `lsp_document_symbols`
pub struct LspDocumentSymbolsTool {
    pub manager: SharedLspManager,
    pub work_dir: PathBuf,
    pub source_root: PathBuf,
}

#[async_trait]
impl Tool for LspDocumentSymbolsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp_document_symbols".to_string(),
            description: "List the symbols defined in the given file via the configured language server. Returns a hierarchical `DocumentSymbol[]` when supported, otherwise a flat `SymbolInformation[]`."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string" }
                },
                "required": ["file"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> SdkResult<Value> {
        let tool = "lsp_document_symbols";
        let raw_file = string_arg(&arguments, "file", tool)?;

        let (mut guard, language, uri) =
            open_and_lock(&self.manager, &self.work_dir, &self.source_root, raw_file).await?;
        let client = guard.client_for_language(language).await?;
        let symbols = client.document_symbols_raw(&uri).await?;
        Ok(json!({ "symbols": symbols }))
    }
}
