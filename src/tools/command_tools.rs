use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::error::{SdkError, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};

pub struct RunCommandTool {
    pub work_dir: PathBuf,
    pub allowed_commands: Vec<String>,
}

impl RunCommandTool {
    pub fn with_defaults(work_dir: PathBuf) -> Self {
        Self {
            work_dir,
            allowed_commands: vec![],
        }
    }

    pub fn with_commands(work_dir: PathBuf, allowed: Vec<String>) -> Self {
        Self {
            work_dir,
            allowed_commands: allowed,
        }
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        let desc = if self.allowed_commands.is_empty() {
            "Execute any shell command in the working directory.".to_string()
        } else {
            format!(
                "Execute a shell command in the working directory. Allowed commands: {}",
                self.allowed_commands.join(", ")
            )
        };
        ToolDefinition {
            name: "run_command".to_string(),
            description: desc,
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command to execute" },
                    "timeout_secs": { "type": "integer", "description": "Timeout in seconds (default: 30)" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let command = arguments["command"]
            .as_str()
            .ok_or_else(|| SdkError::ToolExecution {
                tool_name: "run_command".to_string(),
                message: "Missing 'command' argument".to_string(),
            })?;

        let timeout_secs = arguments["timeout_secs"].as_u64().unwrap_or(30);

        // Check whitelist (empty = allow all)
        if !self.allowed_commands.is_empty() {
            let executable = command.split_whitespace().next().unwrap_or("");
            if !self.allowed_commands.iter().any(|c| c == executable) {
                return Ok(json!({
                    "error": format!(
                        "Command '{}' is not allowed. Allowed: {}",
                        executable,
                        self.allowed_commands.join(", ")
                    )
                }));
            }
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&self.work_dir)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let max_len = 4000;
                let stdout_truncated = if stdout.len() > max_len {
                    format!("{}... (truncated, {} total bytes)", &stdout[..max_len], stdout.len())
                } else {
                    stdout.to_string()
                };
                let stderr_truncated = if stderr.len() > max_len {
                    format!("{}... (truncated, {} total bytes)", &stderr[..max_len], stderr.len())
                } else {
                    stderr.to_string()
                };

                Ok(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_truncated,
                    "stderr": stderr_truncated
                }))
            }
            Ok(Err(e)) => Ok(json!({ "error": format!("Failed to execute command: {}", e) })),
            Err(_) => Ok(json!({ "error": format!("Command timed out after {}s", timeout_secs) })),
        }
    }
}
