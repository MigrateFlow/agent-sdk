# CLAUDE.md

This repository is a single-crate Rust project named `agent-sdk`. It provides:

- A library for single-agent and multi-agent orchestration
- A CLI binary named `agent`
- Built-in tooling for filesystem access, shell commands, search, web search, task context, memory, and spawning agent teams

## What To Read First

When starting work, read these in roughly this order:

1. `README.md`
2. `docs/README.md`
3. `src/lib.rs`
4. The module you are changing

The docs in `docs/` are current and useful. In particular:

- `docs/getting-started.md`
- `docs/single-agent.md`
- `docs/teams.md`
- `docs/cli.md`
- `docs/extending.md`
- `docs/reference.md`

## High-Level Architecture

Top-level modules:

- `src/lib.rs`: public exports and main SDK surface
- `src/agent/`: agent loop, team orchestration, teammates, hooks, events, shared memory
- `src/task/`: task store, dependency graph, file locking, watcher
- `src/mailbox/`: inter-agent messaging
- `src/llm/`: Claude and OpenAI clients, retry, rate limiting
- `src/tools/`: built-in tools and tool registry
- `src/traits/`: extension traits such as `Tool`, `LlmClient`, `PromptBuilder`
- `src/types/`: shared runtime types
- `src/storage.rs`: project-local vs user-local runtime path layout
- `src/bin/agent.rs`: interactive CLI / one-shot entrypoint

Primary public entrypoints:

- `AgentTeam`: high-level orchestration API
- `AgentLoop`: lower-level single-agent ReAct loop
- `create_client(...)`: LLM client factory

## Important Repo-Specific Behavior

- `AgentTeam::run(goal)` accepts a `goal` string which is threaded into each teammate's system prompt as `Team goal: <goal>` so teammates share a common objective. Additionally, if no tasks are pre-seeded via `add_task(...)` and `goal` is non-empty, a single root task is auto-seeded from the goal so the team has immediate work to claim.
- `AgentTeam::run_single(...)` is the simplest programmatic path for one-agent work.
- The CLI is separate from `AgentTeam`; it runs its own conversational loop and can call `spawn_agent_team`.
- `run_command` is effectively unrestricted by default. In the CLI, `--allow-all-commands` is kept for compatibility and is effectively a no-op.
- Project-local config belongs under `.agent/`.
- Mutable runtime state is user-local under `~/.agent/`, especially:
  - `~/.agent/projects/<project-key>/sessions/`
  - `~/.agent/projects/<project-key>/tasks/`
  - `~/.agent/teams/<team-name>/`
  - `~/.agent/tasks/<team-name>/`
- The project key is derived from the repository path or Git common dir, so worktrees/subdirectories may share state intentionally.
- Task claiming uses file locking. If changing task flow, also inspect `src/task/file_lock.rs` and `src/task/store.rs`.

## Default Development Workflow

Use these commands from the repo root:

```bash
cargo test -q
cargo run --bin agent
cargo run --bin agent -- "inspect src/lib.rs and summarize public exports"
```

Useful targeted commands:

```bash
cargo test <name>
cargo run --bin agent -- -p openai -m gpt-4o
```

Current repository baseline as of 2026-03-30:

- `cargo test -q` passes
- `cargo fmt --check` fails due to existing formatting drift across the repository
- `cargo clippy --all-targets --all-features -- -D warnings` fails on existing warnings/errors

Do not assume fmt/clippy are green before your change. If you touch a file, keep edits localized and avoid repo-wide cleanup unless explicitly requested.

## When Editing Code

- Prefer small, surgical changes.
- Preserve the existing async/Tokio design.
- Keep public API changes deliberate; `src/lib.rs` is the crate’s exported surface.
- If you add or change runtime behavior, update the relevant doc in `docs/`.
- If you change a built-in tool, inspect both the tool implementation and any CLI/event display code that describes it.
- If you change team behavior, inspect:
  - `src/agent/team.rs`
  - `src/agent/team_lead.rs`
  - `src/agent/teammate.rs`
  - `src/task/store.rs`
  - `src/mailbox/broker.rs`
- If you change single-agent loop behavior, inspect:
  - `src/agent/agent_loop.rs`
  - `src/prompts.rs`
  - `src/tools/registry.rs`
  - `src/types/chat.rs`

## Extension Points

The intended extension seams are:

- `Tool`
- `PromptBuilder`
- `Hook`
- `LlmClient`

If implementing custom behavior, use `docs/extending.md` as the canonical guide.

Testing guidance already matches the codebase:

1. Use a mock `LlmClient`
2. Use a temporary `work_dir`
3. Register only the tools needed
4. Assert on files, task JSON, and emitted events

## CLI Notes

The CLI supports both REPL and one-shot modes. Relevant facts:

- Binary: `src/bin/agent.rs`
- Session persistence defaults to `~/.agent/projects/<project>/sessions/cli-session.json`
- REPL commands include `/help`, `/clear`, `/compact`, `/tasks`, `/status`, `/quit`
- Built-in CLI tools are:
  - `read_file`
  - `write_file`
  - `edit_file` — surgical string replacement (old_string → new_string)
  - `list_directory`
  - `glob` — fast file pattern matching, mtime-sorted
  - `grep` — content search with context lines and output modes
  - `search_files`
  - `web_search`
  - `run_command`
  - `todo_write` — ephemeral task tracking within conversations
  - `update_task_list`
  - `spawn_agent_team`
  - Memory tools (team mode): `read_memory`, `write_memory`, `list_memory`, `search_memory`, `delete_memory`

If you change tool names, schemas, or output shapes, inspect the CLI formatting helpers in `src/bin/agent.rs`.

## File Map For Common Tasks

- Public API question: `src/lib.rs`
- Provider selection / defaults: `src/config.rs`, `src/llm/`
- Team execution flow: `src/agent/team.rs`, `src/agent/team_lead.rs`
- Single-agent ReAct loop: `src/agent/agent_loop.rs`
- Task persistence / dependencies: `src/task/store.rs`, `src/task/graph.rs`
- Runtime path layout: `src/storage.rs`
- Built-in tools: `src/tools/`
- Prompt text: `src/prompts.rs`
- Docs to update: `docs/`

## Practical Guidance For Claude Code

- Start by reading implementation, not just README examples.
- Prefer `rg` for navigation.
- Validate behavioral changes with focused tests when possible.
- For doc-sensitive changes, keep README and `docs/` consistent.
- Be careful with repo state under `.agent/` vs user state under `~/.agent/`; they are intentionally different.
- Avoid broad formatting-only diffs unless the user explicitly asks for cleanup.
- When you complete a user-requested feature and the user has not asked you to avoid Git commits, create a commit for that feature with a focused, descriptive commit message.
- Do not bundle unrelated work into the same commit. Keep commits scoped to one feature or one coherent fix.
- Treat this `CLAUDE.md` file as living repository memory. When you learn durable repo-specific facts, workflows, caveats, or conventions that will help future work, update `CLAUDE.md` in the same change when appropriate.
- Only add durable knowledge to `CLAUDE.md`. Do not add temporary debugging notes, one-off experiment results, or user-specific ephemeral instructions.
