//! Minimal async JSON-RPC/LSP client used by the `lsp_*` tools.
//!
//! The client is intentionally small: it supports the three requests the SDK
//! needs (`textDocument/definition`, `textDocument/references`,
//! `textDocument/documentSymbol`) plus `initialize` / `initialized` /
//! `textDocument/didOpen`. It is generic over any
//! `AsyncRead + AsyncWrite` transport so that tests can drive it with
//! `tokio::io::duplex()` without spawning a real server.

use std::process::Stdio;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::error::{SdkError, SdkResult};

/// Generic LSP client over any async transport.
pub struct LspClient<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    reader: BufReader<R>,
    writer: W,
    next_id: i64,
    /// Optional child process owned by the client when spawned from a
    /// command. Kept alive for its `kill_on_drop` and pipe ownership.
    #[allow(dead_code)]
    child: Option<Child>,
}

/// Convenience alias for a client connected to a child process via stdio.
pub type ChildLspClient = LspClient<ChildStdout, ChildStdin>;

impl<R, W> LspClient<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Build a client from an existing transport (used by tests).
    pub fn from_transport(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 0,
            child: None,
        }
    }

    /// Perform the `initialize` handshake followed by the `initialized`
    /// notification. `root_uri` is the workspace root (e.g. `file:///tmp/x`).
    pub async fn initialize(&mut self, root_uri: &str) -> SdkResult<Value> {
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {},
            "clientInfo": {
                "name": "agent-sdk",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let response = self.request("initialize", params).await?;
        self.notify("initialized", json!({})).await?;
        Ok(response)
    }

    /// Send `textDocument/didOpen` so the server has the buffer in memory.
    pub async fn did_open(
        &mut self,
        uri: &str,
        language_id: &str,
        text: &str,
    ) -> SdkResult<()> {
        let params = json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        });
        self.notify("textDocument/didOpen", params).await
    }

    /// Request `textDocument/definition`. Normalises `Location | Location[] |
    /// null` into a `Vec<Location>`.
    pub async fn goto_definition(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> SdkResult<Vec<lsp_types::Location>> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        });
        let value = self.request("textDocument/definition", params).await?;
        Ok(locations_from_value(value))
    }

    /// Request `textDocument/references`.
    pub async fn find_references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> SdkResult<Vec<lsp_types::Location>> {
        let params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": include_declaration },
        });
        let value = self.request("textDocument/references", params).await?;
        Ok(parse_or_empty::<Vec<lsp_types::Location>>(value))
    }

    /// Request `textDocument/documentSymbol`. Returns the raw JSON so callers
    /// can handle either `DocumentSymbol[]` or `SymbolInformation[]` shapes.
    pub async fn document_symbols_raw(&mut self, uri: &str) -> SdkResult<Value> {
        let params = json!({ "textDocument": { "uri": uri } });
        self.request("textDocument/documentSymbol", params).await
    }

    /// Send a JSON-RPC request and wait for its matching response.
    pub async fn request(&mut self, method: &str, params: Value) -> SdkResult<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send_message(&msg).await?;

        loop {
            let frame = self.read_frame().await?;
            if let Some(msg_id) = frame.get("id").and_then(|v| v.as_i64()) {
                if msg_id == id {
                    if let Some(err) = frame.get("error") {
                        return Err(SdkError::ToolExecution {
                            tool_name: method.to_string(),
                            message: err.to_string(),
                        });
                    }
                    return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
                }
                // Unknown id (server-originated request); ignore.
                continue;
            }
            // Notification; ignore and keep reading.
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn notify(&mut self, method: &str, params: Value) -> SdkResult<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send_message(&msg).await
    }

    async fn send_message<T: Serialize>(&mut self, msg: &T) -> SdkResult<()> {
        let body = serde_json::to_vec(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer
            .write_all(header.as_bytes())
            .await
            .map_err(SdkError::Io)?;
        self.writer.write_all(&body).await.map_err(SdkError::Io)?;
        self.writer.flush().await.map_err(SdkError::Io)?;
        Ok(())
    }

    async fn read_frame(&mut self) -> SdkResult<Value> {
        let mut content_length: Option<usize> = None;
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).await.map_err(SdkError::Io)?;
            if n == 0 {
                return Err(SdkError::ToolExecution {
                    tool_name: "lsp".to_string(),
                    message: "LSP server closed the connection".to_string(),
                });
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                let value: usize = rest.trim().parse().map_err(|_| SdkError::ToolExecution {
                    tool_name: "lsp".to_string(),
                    message: format!("Invalid Content-Length header: {trimmed:?}"),
                })?;
                content_length = Some(value);
            }
            // Ignore other headers (e.g. Content-Type).
        }

        let len = content_length.ok_or_else(|| SdkError::ToolExecution {
            tool_name: "lsp".to_string(),
            message: "Missing Content-Length header".to_string(),
        })?;

        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .await
            .map_err(SdkError::Io)?;
        let value: Value = serde_json::from_slice(&buf)?;
        Ok(value)
    }
}

impl ChildLspClient {
    /// Spawn an LSP server as a child process and perform the `initialize`
    /// handshake. The server is expected to speak LSP over stdio.
    pub async fn spawn(
        command: &str,
        args: &[String],
        root_uri: &str,
    ) -> SdkResult<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| SdkError::ToolExecution {
            tool_name: "lsp".to_string(),
            message: format!("Failed to spawn '{command}': {e}"),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| SdkError::ToolExecution {
            tool_name: "lsp".to_string(),
            message: "LSP child missing stdin".to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| SdkError::ToolExecution {
            tool_name: "lsp".to_string(),
            message: "LSP child missing stdout".to_string(),
        })?;

        let mut client = LspClient {
            reader: BufReader::new(stdout),
            writer: stdin,
            next_id: 0,
            child: Some(child),
        };
        client.initialize(root_uri).await?;
        Ok(client)
    }
}

fn locations_from_value(value: Value) -> Vec<lsp_types::Location> {
    if value.is_null() {
        return Vec::new();
    }
    if value.is_array() {
        return parse_or_empty::<Vec<lsp_types::Location>>(value);
    }
    // Single `Location` or `LocationLink`. Try Location first; fall back to
    // LocationLink (which has `targetUri`/`targetRange`).
    if let Ok(loc) = serde_json::from_value::<lsp_types::Location>(value.clone()) {
        return vec![loc];
    }
    if let Ok(link) = serde_json::from_value::<lsp_types::LocationLink>(value) {
        return vec![lsp_types::Location {
            uri: link.target_uri,
            range: link.target_range,
        }];
    }
    Vec::new()
}

fn parse_or_empty<T: DeserializeOwned + Default>(value: Value) -> T {
    if value.is_null() {
        return T::default();
    }
    serde_json::from_value(value).unwrap_or_default()
}
