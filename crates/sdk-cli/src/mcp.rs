use std::path::Path;
use std::sync::Arc;

use console::style;
use sdk_core::traits::tool::Tool;
use sdk_protocols::mcp::{McpClient, McpConfig, McpServerSpec, StdioTransport};
use sdk_tools::mcp_tools::McpTool;

use crate::display::display_path;

/// Load and initialize all MCP servers declared in `.agent/mcp.json`.
/// Returns the list of tools to register. Failures for individual servers
/// are logged and skipped.
pub async fn load_mcp_tools(work_dir: &Path, json_mode: bool) -> Vec<Arc<dyn Tool>> {
    let paths = match sdk_core::storage::AgentPaths::for_work_dir(work_dir) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let config_path = paths.project_mcp_config_path();

    let config = match McpConfig::load(&config_path) {
        Ok(c) => c,
        Err(sdk_core::error::SdkError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Vec::new();
        }
        Err(e) => {
            if !json_mode {
                eprintln!(
                    "  {} failed to read {}: {}",
                    style("⚠").yellow(),
                    display_path(&config_path),
                    e,
                );
            }
            return Vec::new();
        }
    };

    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();
    for server in &config.servers {
        match spawn_and_register_mcp_server(server).await {
            Ok(tools) => {
                if !json_mode && !tools.is_empty() {
                    eprintln!(
                        "  {} mcp server {} ({} tool{})",
                        style("✓").green(),
                        style(&server.name).cyan(),
                        tools.len(),
                        if tools.len() == 1 { "" } else { "s" },
                    );
                }
                all_tools.extend(tools);
            }
            Err(e) => {
                if !json_mode {
                    eprintln!(
                        "  {} mcp server {} failed: {}",
                        style("⚠").yellow(),
                        style(&server.name).cyan(),
                        e,
                    );
                }
            }
        }
    }
    all_tools
}

async fn spawn_and_register_mcp_server(
    server: &McpServerSpec,
) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
    let mut cmd = tokio::process::Command::new(&server.command);
    cmd.args(&server.args)
        .envs(&server.env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{}`: {}", server.command, e))?;
    let transport = StdioTransport::from_child(child)?;
    let mut client = McpClient::new(transport, server.name.clone());
    client.initialize().await?;
    let specs = client.list_tools().await?;

    let client = Arc::new(tokio::sync::Mutex::new(client));
    let mut tools: Vec<Arc<dyn Tool>> = Vec::with_capacity(specs.len());
    for spec in specs {
        tools.push(Arc::new(McpTool {
            client: client.clone(),
            spec,
            server_name: server.name.clone(),
        }));
    }
    Ok(tools)
}
