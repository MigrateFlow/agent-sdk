//! Minimal client for the Model Context Protocol (MCP).

pub mod client;
pub mod stdio;

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use sdk_core::error::SdkResult;

pub use client::{
    InitializeResult, McpClient, McpContentBlock, McpToolResult, McpToolSpec, PROTOCOL_VERSION,
};
pub use stdio::StdioTransport;

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Top-level manifest loaded from `.agent/mcp.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerSpec>,
}

impl McpConfig {
    pub fn load(path: &Path) -> SdkResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: McpConfig = serde_json::from_str(&content)?;
        Ok(config)
    }
}
