//! Named team example: explicitly define teammates with roles.
//!
//! Like telling Claude Code: "Create a team with a security reviewer,
//! a performance reviewer, and a test coverage checker."
//!
//! ```bash
//! export ANTHROPIC_API_KEY="sk-ant-..."
//! cargo run --example named_team
//! ```

use agent_sdk::agent::team::AgentTeam;
use agent_sdk::config::{AgentConfig, LlmConfig};
use agent_sdk::types::task::Task;
use agent_sdk::{AgentEvent, Hook, HookEvent, HookResult};
use serde_json::json;

/// Example hook: require all completed tasks to mention testing.
struct RequireTestsHook;

impl Hook for RequireTestsHook {
    fn on_event(&self, event: &HookEvent) -> HookResult {
        if let HookEvent::TaskCompleted { task, .. } = event {
            if let Some(result) = &task.result {
                if !result.notes.to_lowercase().contains("test") {
                    return HookResult::Reject {
                        feedback: "Task rejected: must include test coverage".to_string(),
                    };
                }
            }
        }
        HookResult::Continue
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter("agent_sdk=info")
        .init();

    let work_dir = std::path::PathBuf::from("./tmp/team_output");
    let source_root = std::fs::canonicalize(".")?;

    // Create tasks for the team
    let task1 = Task::new(
        "review", "Security review of auth module",
        "Review src/ for security vulnerabilities. Focus on input validation.",
        "reviews/security.md",
    ).with_context(json!({ "focus": "security" }));

    let task2 = Task::new(
        "review", "Performance review",
        "Review src/ for performance issues. Check for unnecessary allocations.",
        "reviews/performance.md",
    ).with_context(json!({ "focus": "performance" }));

    let task3 = Task::new(
        "review", "Test coverage check",
        "Check test coverage for src/. Identify untested code paths.",
        "reviews/test_coverage.md",
    ).with_context(json!({ "focus": "testing" }));

    // Event monitoring
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::TeammateSpawned { name, .. } => println!("[+] {name}"),
                AgentEvent::TaskStarted { title, .. } => println!("[>] {title}"),
                AgentEvent::TaskCompleted { .. } => println!("[v] Completed"),
                AgentEvent::PlanSubmitted { plan_preview, .. } => println!("[plan] {plan_preview}"),
                AgentEvent::PlanApproved { .. } => println!("[plan] Approved"),
                AgentEvent::PlanRejected { feedback, .. } => println!("[plan] Rejected: {feedback}"),
                AgentEvent::HookRejected { event_name, feedback } => {
                    println!("[hook] {event_name}: {feedback}");
                }
                _ => {}
            }
        }
    });

    // Build a team with named, role-specific teammates
    let result = AgentTeam::new(
        LlmConfig::default(),
        AgentConfig {
            max_parallel_agents: 3,
            max_loop_iterations: 20,
            ..Default::default()
        },
    )
    .source_root(source_root)
    .work_dir(&work_dir)
    .event_channel(tx)
    .add_hook(RequireTestsHook)
    // Each teammate has a specific role
    .add_teammate("security-reviewer", "You are a security expert. Focus on vulnerabilities.")
    .add_teammate("perf-reviewer", "You are a performance engineer. Focus on efficiency.")
    .add_teammate_with_plan_approval("test-reviewer", "You review test coverage. Plan first.")
    // Tasks for the team to work on
    .add_task(task1)
    .add_task(task2)
    .add_task(task3)
    .run("Review the codebase from all angles")
    .await?;

    println!("\n--- Result ---");
    println!("Tokens: {}", result.total_tokens());

    Ok(())
}
