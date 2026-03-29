//! Agent team example: create a team with tasks and let teammates work.
//!
//! Like telling Claude Code: "Create an agent team to build a config module
//! and a server module." The lead spawns teammates, they claim tasks from
//! the shared task list, and coordinate through messaging.
//!
//! ```bash
//! export ANTHROPIC_API_KEY="sk-ant-..."
//! cargo run --example multi_agent
//! ```

use agent_sdk::agent::team::AgentTeam;
use agent_sdk::config::{AgentConfig, LlmConfig};
use agent_sdk::types::task::Task;
use agent_sdk::AgentEvent;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter("agent_sdk=info")
        .init();

    let work_dir = std::path::PathBuf::from("./tmp/agent_output");
    let source_root = std::fs::canonicalize(".")?;

    // Create tasks with dependencies
    let task1 = Task::new(
        "generate",
        "Create a Config struct",
        "Create a Rust Config struct with fields: host (String), port (u16), debug (bool). \
         Include Default impl and a load_from_env() constructor.",
        "src/config.rs",
    )
    .with_priority(0)
    .with_context(json!({ "language": "rust" }));

    let task2 = Task::new(
        "generate",
        "Create a Server struct",
        "Create a Rust Server struct that takes a Config and has a start() method.",
        "src/server.rs",
    )
    .with_dependencies(vec![task1.id]) // waits for config
    .with_priority(1);

    let task3 = Task::new(
        "generate",
        "Create main.rs",
        "Create a main.rs that creates a Config, builds a Server, and starts it.",
        "src/main.rs",
    )
    .with_dependencies(vec![task1.id, task2.id]) // waits for both
    .with_priority(2);

    // Event monitoring
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::TeamSpawned { teammate_count } => {
                    println!("[team] Spawned {teammate_count} teammates");
                }
                AgentEvent::TeammateSpawned { name, .. } => println!("[team] + {name}"),
                AgentEvent::TaskStarted { title, .. } => println!("[task] Started: {title}"),
                AgentEvent::ToolCall { tool_name, .. } => println!("  [tool] {tool_name}"),
                AgentEvent::TaskCompleted { tokens_used, tool_calls, .. } => {
                    println!("  [done] {tokens_used} tokens, {tool_calls} tool calls");
                }
                AgentEvent::TaskFailed { error, .. } => println!("  [fail] {error}"),
                AgentEvent::TeammateIdle { .. } => println!("  [idle] Teammate waiting"),
                AgentEvent::ShutdownRequested { .. } => println!("  [stop] Shutdown requested"),
                _ => {}
            }
        }
    });

    println!("Creating team with 3 tasks...\n");

    // Create team, add tasks, and run
    let result = AgentTeam::new(
        LlmConfig::default(),
        AgentConfig {
            max_parallel_agents: 3,
            max_loop_iterations: 30,
            ..Default::default()
        },
    )
    .source_root(source_root)
    .work_dir(&work_dir)
    .event_channel(tx)
    .add_task(task1)
    .add_task(task2)
    .add_task(task3)
    .run("Build a simple Rust server project")
    .await?;

    println!("\n--- Result ---");
    println!("Tokens: {}", result.total_tokens());

    Ok(())
}
