//! Minimal client for the Model Context Protocol (MCP).
//!
//! MCP is a JSON-RPC 2.0 protocol carried over NDJSON framed stdio by
//! default. This module implements just enough of the client side to spawn
//! a server process, negotiate `initialize`, list its tools, and invoke
//! them. See <https://modelcontextprotocol.io> for the full spec.

pub mod client;
pub mod stdio;

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::SdkResult;

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
    /// Load and parse an MCP manifest from disk. A missing file returns an
    /// error — callers that want "missing is OK" should check existence first.
    pub fn load(path: &Path) -> SdkResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: McpConfig = serde_json::from_str(&content)?;
        Ok(config)
    }
}
