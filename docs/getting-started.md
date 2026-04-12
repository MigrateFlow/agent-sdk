# Getting Started

## Install

Add the crate to your project:

```toml
[dependencies]
agent-orchestrator-sdk = "0.1"
tokio = { version = "1", features = ["full"] }
anyhow = "1"
serde_json = "1"
```

## Choose A Provider

`agent-orchestrator-sdk` currently supports:

- `LlmProvider::Claude`
- `LlmProvider::OpenAi`

The SDK resolves credentials from config first, then from environment variables:

| Provider | API key env | Base URL env | Default base URL |
| --- | --- | --- | --- |
| Claude | `ANTHROPIC_API_KEY` | `ANTHROPIC_API_BASE_URL` | `https://api.anthropic.com` |
| OpenAI | `OPENAI_API_KEY` | `OPENAI_API_BASE_URL` | `https://api.openai.com` |

Model selection is explicit in `LlmConfig`.

## Minimal Single-Agent Program

For most integrations, start with `AgentTeam::run_single(...)`:

```rust
use agent_sdk::{AgentConfig, AgentTeam, LlmConfig, LlmProvider};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let llm = LlmConfig {
        provider: LlmProvider::Claude,
        model: "claude-sonnet-4-20250514".to_string(),
        ..LlmConfig::default()
    };

    let agent = AgentTeam::new(llm, AgentConfig::default())
        .source_root(".")
        .work_dir(".");

    let result = agent.run_single("Summarize the repository layout").await?;

    println!("{}", result.final_content);
    println!("tokens: {}", result.total_tokens);
    Ok(())
}
```

`run_single(...)` automatically registers these tools:

- `read_file`
- `write_file`
- `list_directory`
- `search_files`
- `run_command`

## Minimal Team Program

For parallel work, add teammates and explicit tasks:

```rust
use agent_sdk::{AgentConfig, AgentTeam, LlmConfig, Task};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let parse = Task::new(
        "analysis",
        "Inspect current API layout",
        "Review the existing modules and summarize the public surface.",
        "docs/api-notes.md",
    );

    let write = Task::new(
        "docs",
        "Write usage docs",
        "Create end-user documentation based on the API summary.",
        "docs/usage.md",
    )
    .with_dependencies(vec![parse.id]);

    let result = AgentTeam::new(LlmConfig::default(), AgentConfig::default())
        .source_root(".")
        .work_dir(".")
        .add_teammate("api-reader", "Review Rust modules and identify public APIs")
        .add_teammate("docs-writer", "Write concise SDK usage documentation")
        .add_task(parse)
        .add_task(write)
        .run("Document the SDK")
        .await?;

    println!("total tokens: {}", result.total_tokens());
    Ok(())
}
```

Note: `run("Document the SDK")` adds the provided goal as a root task when no tasks are pre-seeded and also prefixes the goal into each teammate's system prompt (as `Team goal: <goal>`) so teammates share the high-level objective in their context. If you pre-seed tasks with `add_task(...)`, the goal is only included in teammates' system prompts and does not overwrite explicit tasks.

## LLM Configuration

`LlmConfig` controls provider selection, throttling, request limits, and retries:

```rust
use agent_sdk::{LlmConfig, LlmProvider};

let llm = LlmConfig {
    provider: LlmProvider::OpenAi,
    model: "gpt-4o".to_string(),
    max_tokens: 8_192,
    requests_per_minute: 60,
    tokens_per_minute: 120_000,
    http_timeout_secs: 120,
    max_retries: 3,
    retry_base_delay_ms: 1_000,
    api_key: None,
    api_base_url: None,
};
```

Fields:

- `provider`: Claude or OpenAI
- `model`: provider-specific model id
- `max_tokens`: per-response limit sent to the provider
- `requests_per_minute`: local rate limiter
- `tokens_per_minute`: available in config, but current clients enforce request-rate limiting only
- `api_key`: optional override for env-based auth
- `api_base_url`: optional override for default provider endpoint
- `http_timeout_secs`: per-request timeout
- `max_retries`: transient retry count for 429 and 5xx behavior
- `retry_base_delay_ms`: retry backoff base delay

## Agent Runtime Configuration

`AgentConfig` controls loop and team behavior:

```rust
use agent_sdk::AgentConfig;

let config = AgentConfig {
    max_parallel_agents: 4,
    poll_interval_ms: 200,
    max_task_retries: 3,
    max_loop_iterations: 50,
    max_context_tokens: 200_000,
    max_idle_cycles: 50,
    plan_approval_timeout_secs: 300,
    command_timeout_secs: 30,
};
```

## Run The CLI

The repository includes a binary named `agent`:

```bash
cargo run --bin agent
```

One-shot mode:

```bash
cargo run --bin agent -- "summarize src/lib.rs"
```

Provider override:

```bash
cargo run --bin agent -- -p openai -m gpt-4o "inspect this project"
```

See [cli.md](cli.md) for the full CLI flow.
