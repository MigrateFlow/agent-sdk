use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::error::{SdkError, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};

/// Tool that validates Mermaid diagram syntax by shelling out to a Node.js
/// CLI script. The agent can use this to verify diagrams before writing them,
/// and fix any syntax errors the validator reports.
pub struct VerifyMermaidTool {
    /// Path to the verify-mermaid-cli.mjs script.
    pub script_path: PathBuf,
    /// Working directory for running the script (needs node_modules access).
    pub work_dir: PathBuf,
}

#[async_trait]
impl Tool for VerifyMermaidTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "verify_mermaid".to_string(),
            description: "Validate Mermaid diagram syntax. Pass the raw mermaid source (without ``` fences) and receive { \"valid\": true } or { \"valid\": false, \"error\": \"...\" }. Use this tool to check every mermaid diagram before writing it to a file. If the diagram is invalid, fix the syntax error described in the error message and verify again.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "diagram": {
                        "type": "string",
                        "description": "The Mermaid diagram source code to validate (without ```mermaid fences)"
                    }
                },
                "required": ["diagram"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let diagram = arguments["diagram"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "verify_mermaid".to_string(),
                message: "Missing 'diagram' argument".to_string(),
            })?;

        if diagram.trim().is_empty() {
            return Ok(json!({ "valid": false, "error": "Empty diagram" }));
        }

        // Spawn the Node.js validator script
        let mut child = tokio::process::Command::new("node")
            .arg(&self.script_path)
            .current_dir(&self.work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SdkError::ToolExecution {
                tool_name: "verify_mermaid".to_string(),
                message: format!("Failed to spawn mermaid validator: {}", e),
            })?;

        // Write diagram to stdin
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(diagram.as_bytes()).await.map_err(|e| SdkError::ToolExecution {
                tool_name: "verify_mermaid".to_string(),
                message: format!("Failed to write to validator stdin: {}", e),
            })?;
            drop(stdin); // Close stdin to signal EOF
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| SdkError::ToolExecution {
            tool_name: "verify_mermaid".to_string(),
            message: "Mermaid validation timed out after 30s".to_string(),
        })?
        .map_err(|e| SdkError::ToolExecution {
            tool_name: "verify_mermaid".to_string(),
            message: format!("Validator process error: {}", e),
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Try to parse the JSON output from the validator
        match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            Ok(result) => Ok(result),
            Err(_) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Ok(json!({
                    "valid": false,
                    "error": format!(
                        "Validator returned non-JSON output. stdout: {}, stderr: {}",
                        stdout.trim(),
                        stderr.trim()
                    )
                }))
            }
        }
    }
}
