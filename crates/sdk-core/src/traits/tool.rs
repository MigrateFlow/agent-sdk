use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::SdkResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value>;

    /// Returns true if this tool only reads data and never modifies state.
    /// Read-only tools are auto-approved without prompting.
    fn is_read_only(&self) -> bool { false }

    /// Returns true if this tool performs destructive operations (e.g. shell execution).
    /// Destructive tools always prompt and do not offer "always allow".
    fn is_destructive(&self) -> bool { false }
}
