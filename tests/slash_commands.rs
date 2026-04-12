use std::sync::{Arc, Mutex};

use agent_sdk::cli::session::CliTask;
use agent_sdk::{
    AgentPaths, ChatMessage, CommandContext, CommandOutcome, SdkError, SdkResult, SlashCommand,
    SlashCommandRegistry,
};
use async_trait::async_trait;
use tempfile::TempDir;

/// Minimal stub used to verify custom-command registration.
struct EchoCommand;

#[async_trait]
impl SlashCommand for EchoCommand {
    fn name(&self) -> &str {
        "echo"
    }

    fn help(&self) -> &str {
        "echo back the given arguments"
    }

    async fn execute(
        &self,
        _ctx: &mut CommandContext<'_>,
        args: &str,
    ) -> SdkResult<CommandOutcome> {
        Ok(CommandOutcome::Output(args.to_string()))
    }
}

struct TestHarness {
    _tmp: TempDir,
    paths: AgentPaths,
    messages: Vec<ChatMessage>,
    tasks: Arc<Mutex<Vec<CliTask>>>,
    session_path: std::path::PathBuf,
    system_prompt: String,
    total_tokens: u64,
    tool_calls: usize,
    turns: usize,
    agent_mode: agent_sdk::AgentMode,
    ultra_plan: Option<agent_sdk::UltraPlanState>,
}

impl TestHarness {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let paths = AgentPaths::for_work_dir(tmp.path()).expect("AgentPaths");
        let session_path = tmp.path().join("session.json");
        Self {
            _tmp: tmp,
            paths,
            messages: vec![ChatMessage::system("test system prompt")],
            tasks: Arc::new(Mutex::new(Vec::new())),
            session_path,
            system_prompt: "test system prompt".to_string(),
            total_tokens: 0,
            tool_calls: 0,
            turns: 0,
            agent_mode: agent_sdk::AgentMode::Normal,
            ultra_plan: None,
        }
    }

    fn context(&mut self) -> CommandContext<'_> {
        CommandContext {
            messages: &mut self.messages,
            tasks: self.tasks.clone(),
            paths: &self.paths,
            session_path: self.session_path.clone(),
            system_prompt: &self.system_prompt,
            total_tokens: &mut self.total_tokens,
            tool_calls: &mut self.tool_calls,
            turns: &mut self.turns,
            agent_mode: &mut self.agent_mode,
            cache_state: None,
            ultra_plan: &mut self.ultra_plan,
        }
    }
}

#[tokio::test]
async fn echo_command_returns_output_outcome() {
    let mut registry = SlashCommandRegistry::new();
    registry.register(Arc::new(EchoCommand));

    let mut harness = TestHarness::new();
    let mut ctx = harness.context();

    let outcome = registry
        .dispatch("/echo hello", &mut ctx)
        .await
        .expect("dispatch ok")
        .expect("outcome present");

    match outcome {
        CommandOutcome::Output(text) => assert_eq!(text, "hello"),
        other => panic!("expected Output, got {:?}", other),
    }
}

#[tokio::test]
async fn unknown_command_returns_config_error() {
    // Design choice: dispatch returns `Err(SdkError::Config)` for an input
    // that starts with `/` but matches no registered command. The REPL
    // surfaces the error text to the user.
    let registry = SlashCommandRegistry::new();

    let mut harness = TestHarness::new();
    let mut ctx = harness.context();

    let err = registry
        .dispatch("/unknown", &mut ctx)
        .await
        .expect_err("unknown command should error");

    match err {
        SdkError::Config(msg) => assert!(msg.contains("/unknown"), "msg: {msg}"),
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[tokio::test]
async fn non_slash_input_returns_none() {
    let registry = SlashCommandRegistry::builtin();

    let mut harness = TestHarness::new();
    let mut ctx = harness.context();

    let result = registry
        .dispatch("hello world, this is a regular prompt", &mut ctx)
        .await
        .expect("dispatch ok");

    assert!(result.is_none(), "non-slash input must yield Ok(None)");
}

#[tokio::test]
async fn help_lists_all_builtin_commands() {
    let registry = SlashCommandRegistry::builtin();

    let mut harness = TestHarness::new();
    let mut ctx = harness.context();

    let outcome = registry
        .dispatch("/help", &mut ctx)
        .await
        .expect("dispatch ok")
        .expect("outcome present");

    let text = match outcome {
        CommandOutcome::Output(t) => t,
        other => panic!("expected Output, got {:?}", other),
    };

    for name in ["/help", "/clear", "/compact", "/tasks", "/cost", "/status", "/quit"] {
        assert!(
            text.contains(name),
            "expected help text to contain {name}, got:\n{text}"
        );
    }
}

#[tokio::test]
async fn clear_wipes_messages_and_returns_clear_outcome() {
    let registry = SlashCommandRegistry::builtin();

    let mut harness = TestHarness::new();
    harness.messages.push(ChatMessage::user("first"));
    harness.messages.push(ChatMessage::assistant("reply"));
    harness.tasks.lock().unwrap().push(CliTask {
        title: "t".into(),
        status: "pending".into(),
    });
    harness.total_tokens = 123;
    harness.tool_calls = 4;
    harness.turns = 2;

    {
        let mut ctx = harness.context();
        let outcome = registry
            .dispatch("/clear", &mut ctx)
            .await
            .expect("dispatch ok")
            .expect("outcome present");

        assert!(matches!(outcome, CommandOutcome::Clear));
    }

    // Only the system message should remain.
    assert_eq!(harness.messages.len(), 1);
    assert!(matches!(&harness.messages[0], ChatMessage::System { .. }));
    assert!(harness.tasks.lock().unwrap().is_empty());
    assert_eq!(harness.total_tokens, 0);
    assert_eq!(harness.tool_calls, 0);
    assert_eq!(harness.turns, 0);
    assert!(
        harness.session_path.exists(),
        "/clear should persist the cleared session"
    );
}
