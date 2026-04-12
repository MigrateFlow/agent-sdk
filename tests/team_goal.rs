//! End-to-end tests for `AgentTeam::run(goal)` goal threading.
//!
//! Verifies two properties of Unit 1:
//! 1. When the caller supplies a `goal` but no tasks, a root task is
//!    auto-seeded whose description is the goal.
//! 2. Whether or not tasks were pre-seeded, the goal appears in each
//!    teammate's system prompt as a `Team goal:` line.

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;

use agent_sdk::{
    AgentConfig, AgentTeam, ChatMessage, LlmClient, LlmConfig, SdkResult, Task, ToolDefinition,
};

/// Minimal mock LLM: records every system prompt it sees and always returns
/// an assistant message with no tool calls so the agent loop terminates on
/// the first iteration.
struct MockLlm {
    /// First-message system prompts captured from each `chat(...)` call.
    captured_system_prompts: Arc<Mutex<Vec<String>>>,
}

impl MockLlm {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                captured_system_prompts: Arc::clone(&captured),
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmClient for MockLlm {
    async fn ask(&self, _system: &str, _user_message: &str) -> SdkResult<(String, u64)> {
        Ok(("APPROVED".to_string(), 1))
    }

    async fn chat(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolDefinition],
    ) -> SdkResult<(ChatMessage, u64)> {
        if let Some(ChatMessage::System { content }) = messages.first() {
            self.captured_system_prompts
                .lock()
                .unwrap()
                .push(content.clone());
        }
        Ok((ChatMessage::assistant("done"), 1))
    }
}

fn fast_config() -> AgentConfig {
    AgentConfig {
        max_parallel_agents: 1,
        poll_interval_ms: 10,
        max_task_retries: 0,
        max_loop_iterations: 2,
        max_idle_cycles: 2,
        ..AgentConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_with_goal_auto_seeds_root_task_when_task_list_empty() {
    let source = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();

    let (mock, captured) = MockLlm::new();
    let team = AgentTeam::new(LlmConfig::default(), fast_config())
        .source_root(source.path().to_path_buf())
        .work_dir(work.path().to_path_buf())
        .llm_client(Arc::new(mock))
        .add_teammate("solo", "");

    let result = team.run("do X").await.expect("team run failed");

    // The auto-seeded root task must show up in the summary.
    match result {
        agent_sdk::agent::team::TeamResult::Team(summary) => {
            assert_eq!(
                summary.total_tasks, 1,
                "expected one auto-seeded root task, got {}",
                summary.total_tasks
            );
        }
        other => panic!("expected TeamResult::Team, got {:?}", other),
    }

    let prompts = captured.lock().unwrap().clone();
    assert!(
        !prompts.is_empty(),
        "mock LLM should have seen at least one system prompt"
    );
    assert!(
        prompts.iter().any(|p| p.contains("do X")),
        "system prompt should carry the goal 'do X'; captured prompts: {:?}",
        prompts
    );
    assert!(
        prompts.iter().any(|p| p.contains("Team goal:")),
        "system prompt should carry the 'Team goal:' marker; captured prompts: {:?}",
        prompts
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_with_goal_preserves_preseeded_tasks_and_still_injects_goal() {
    let source = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();

    let (mock, captured) = MockLlm::new();
    let seeded = Task::new(
        "custom",
        "Seeded task title",
        "Seeded task description body",
        "out.txt",
    );

    let team = AgentTeam::new(LlmConfig::default(), fast_config())
        .source_root(source.path().to_path_buf())
        .work_dir(work.path().to_path_buf())
        .llm_client(Arc::new(mock))
        .add_teammate("solo", "")
        .add_task(seeded);

    let result = team
        .run("ship the feature")
        .await
        .expect("team run failed");

    match result {
        agent_sdk::agent::team::TeamResult::Team(summary) => {
            // Only the one pre-seeded task should exist — goal must not
            // overwrite or append another root task.
            assert_eq!(
                summary.total_tasks, 1,
                "pre-seeded task list must not be augmented when goal is set"
            );
        }
        other => panic!("expected TeamResult::Team, got {:?}", other),
    }

    let prompts = captured.lock().unwrap().clone();
    assert!(
        prompts.iter().any(|p| p.contains("ship the feature")),
        "goal should still be injected into teammate system prompt; got: {:?}",
        prompts
    );
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("Seeded task title") || p.contains("Seeded task description")),
        "pre-seeded task should drive the teammate's task prompt; got: {:?}",
        prompts
    );
}
