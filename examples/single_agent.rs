//! Low-level example: use AgentLoop directly for a single-agent task.
//!
//! This is the building block underneath AgentTeam. Use it when you need
//! full control over the agent's tools, system prompt, and conversation.
//!
//! ```bash
//! export ANTHROPIC_API_KEY="sk-ant-..."
//! cargo run --example single_agent
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use agent_sdk::config::LlmConfig;
use agent_sdk::error::SdkResult;
use agent_sdk::tools::fs_tools::{ListDirectoryTool, ReadFileTool};
use agent_sdk::tools::registry::ToolRegistry;
use agent_sdk::traits::tool::{Tool, ToolDefinition};
use agent_sdk::AgentLoop;

/// Example custom tool.
pub struct CalculatorTool;

#[async_trait]
impl Tool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "calculator".to_string(),
            description: "Perform basic math: add, subtract, multiply, divide".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["add", "subtract", "multiply", "divide"] },
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["operation", "a", "b"]
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> SdkResult<serde_json::Value> {
        let op = args["operation"].as_str().unwrap_or("add");
        let a = args["a"].as_f64().unwrap_or(0.0);
        let b = args["b"].as_f64().unwrap_or(0.0);
        let result = match op {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" if b != 0.0 => a / b,
            "divide" => return Ok(json!({ "error": "Division by zero" })),
            _ => return Ok(json!({ "error": format!("Unknown: {op}") })),
        };
        Ok(json!({ "result": result }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter("agent_sdk=debug")
        .init();

    let work_dir = std::fs::canonicalize(".")?;
    let config = LlmConfig::default();
    let client = agent_sdk::llm::create_client(&config)?;

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ReadFileTool {
        source_root: work_dir.clone(),
        work_dir: work_dir.clone(),
    }));
    tools.register(Arc::new(ListDirectoryTool {
        source_root: work_dir.clone(),
        work_dir: work_dir.clone(),
    }));
    tools.register(Arc::new(CalculatorTool));

    let mut agent = AgentLoop::new(
        Uuid::new_v4(),
        client,
        tools,
        "You are a helpful assistant. Use tools to answer questions.".to_string(),
        20,
    );

    let result = agent
        .run("List the files in src/ and compute 1337 * 42.".to_string())
        .await?;

    println!("\n{}", result.final_content);
    println!(
        "\n(iterations: {}, tool calls: {}, tokens: {})",
        result.iterations, result.tool_calls_count, result.total_tokens
    );
    Ok(())
}
