//! MCP (Model Context Protocol) JSON-RPC client.
//!
//! Speaks the minimal subset of the MCP stdio protocol that we need to
//! register remote tools: `initialize`, `notifications/initialized`,
//! `tools/list`, `tools/call`.

use std::sync::atomic::{AtomicI64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use sdk_core::error::{SdkError, SdkResult};
use super::stdio::StdioTransport;

pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion", default)]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    #[serde(rename = "serverInfo", default)]
    pub server_info: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default = "default_empty_schema")]
    pub input_schema: serde_json::Value,
}

fn default_empty_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object", "properties": {} })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    #[serde(default)]
    pub content: Vec<McpContentBlock>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContentBlock {
    Text { text: String },
    #[serde(other)]
    Other,
}

/// A connected MCP client. Issues requests and correlates them with the
/// server's responses by numeric id.
pub struct McpClient<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    transport: StdioTransport<R, W>,
    next_id: AtomicI64,
    server_name: String,
    initialize_result: Option<InitializeResult>,
}

impl<R, W> McpClient<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(transport: StdioTransport<R, W>, server_name: impl Into<String>) -> Self {
        Self {
            transport,
            next_id: AtomicI64::new(1),
            server_name: server_name.into(),
            initialize_result: None,
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn capabilities(&self) -> Option<&serde_json::Value> {
        self.initialize_result.as_ref().map(|r| &r.capabilities)
    }

    /// Perform the MCP handshake: send `initialize`, read the result, then
    /// send the `notifications/initialized` notification.
    pub async fn initialize(&mut self) -> SdkResult<InitializeResult> {
        let params = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "rust-agent-sdk",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });

        let result_value = self.request("initialize", params).await?;
        let result: InitializeResult = serde_json::from_value(result_value)?;

        // Notify the server that we're ready.
        self.transport
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
            }))
            .await?;

        self.initialize_result = Some(result.clone());
        Ok(result)
    }

    /// Fetch the list of tools the server exposes.
    pub async fn list_tools(&self) -> SdkResult<Vec<McpToolSpec>> {
        let result = self.request("tools/list", serde_json::json!({})).await?;
        let tools = result
            .get("tools")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        let tools: Vec<McpToolSpec> = serde_json::from_value(tools)?;
        Ok(tools)
    }

    /// Invoke a tool and return its structured result.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> SdkResult<McpToolResult> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });
        let result = self.request("tools/call", params).await?;
        let result: McpToolResult = serde_json::from_value(result)?;
        Ok(result)
    }

    /// Send a JSON-RPC request and return the `result` field.
    ///
    /// Any non-response traffic (notifications from the server, mismatched
    /// ids) is silently skipped.
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> SdkResult<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.transport.send(request).await?;

        loop {
            let message = self.transport.recv().await?;

            // Skip notifications (no `id` field).
            if message.get("id").is_none() {
                continue;
            }

            // Skip responses that don't match our id (shouldn't happen with a
            // single in-flight request, but be defensive).
            let matches_id = message
                .get("id")
                .and_then(|v| v.as_i64())
                .is_some_and(|v| v == id);
            if !matches_id {
                continue;
            }

            if let Some(err) = message.get("error") {
                let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
                let msg = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown MCP error");
                return Err(SdkError::ToolExecution {
                    tool_name: format!("mcp:{}:{}", self.server_name, method),
                    message: format!("JSON-RPC error {}: {}", code, msg),
                });
            }

            let result = message.get("result").cloned().ok_or_else(|| {
                SdkError::LlmResponseParse(format!(
                    "MCP response for `{}` missing `result` field",
                    method
                ))
            })?;
            return Ok(result);
        }
    }
}
