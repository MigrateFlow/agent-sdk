//! Bridge an MCP server's tools into the SDK's `Tool` trait.
//!
//! Each tool discovered via `tools/list` is wrapped in an `McpTool` and
//! registered with the normal `ToolRegistry`. Tool names are namespaced as
//! `mcp__<server>__<tool>` to match the convention used by Claude Code.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use sdk_core::error::{SdkError, SdkResult};
use sdk_protocols::mcp::{McpClient, McpContentBlock, McpToolSpec};
use sdk_core::traits::tool::{Tool, ToolDefinition};

/// Build the namespaced tool name exposed to the LLM.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", sanitize(server), sanitize(tool))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// A `Tool` impl that forwards calls to a remote MCP server.
pub struct McpTool<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    pub client: Arc<Mutex<McpClient<R, W>>>,
    pub spec: McpToolSpec,
    pub server_name: String,
}

#[async_trait]
impl<R, W> Tool for McpTool<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: mcp_tool_name(&self.server_name, &self.spec.name),
            description: if self.spec.description.is_empty() {
                format!("MCP tool `{}` from server `{}`", self.spec.name, self.server_name)
            } else {
                self.spec.description.clone()
            },
            parameters: self.spec.input_schema.clone(),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let client = self.client.lock().await;
        let result = client
            .call_tool(&self.spec.name, arguments)
            .await
            .map_err(|e| SdkError::ToolExecution {
                tool_name: mcp_tool_name(&self.server_name, &self.spec.name),
                message: e.to_string(),
            })?;

        let text = flatten_content(&result.content);
        Ok(json!({
            "content": text,
            "is_error": result.is_error,
        }))
    }
}

/// Flatten MCP content blocks into a single string for the LLM.
fn flatten_content(blocks: &[McpContentBlock]) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let McpContentBlock::Text { text } = block {
            parts.push(text.clone());
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_joins_text_blocks_with_newline() {
        let blocks = vec![
            McpContentBlock::Text { text: "hello".into() },
            McpContentBlock::Text { text: "world".into() },
        ];
        assert_eq!(flatten_content(&blocks), "hello\nworld");
    }

    #[test]
    fn mcp_tool_name_sanitizes_components() {
        assert_eq!(mcp_tool_name("weather-v1", "get_forecast"), "mcp__weather-v1__get_forecast");
        assert_eq!(mcp_tool_name("weird name", "x.y"), "mcp__weird_name__x_y");
    }
}
