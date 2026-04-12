# SDK Documentation

This folder documents the current `agent-orchestrator-sdk` API and runtime behavior in this repository.

The SDK has two main entrypoints:

- `AgentTeam` for high-level execution
- `AgentLoop` for low-level single-agent control

Use these documents in this order:

- [getting-started.md](getting-started.md): install, configure providers, run the CLI, and create your first program
- [single-agent.md](single-agent.md): `AgentTeam::run_single` and direct `AgentLoop` usage
- [teams.md](teams.md): teammates, tasks, dependencies, hooks, events, and shared infrastructure
- [cli.md](cli.md): the `agent` binary, flags, REPL commands, and tool access
- [extending.md](extending.md): custom tools, prompt builders, hooks, and LLM clients
- [reference.md](reference.md): compact API and runtime reference

## Current Shape Of The SDK

- LLM providers: Anthropic Claude and OpenAI
- Built-in runtime modes: single-agent and multi-agent team orchestration
- Built-in tools: file read/write, directory listing, search, shell commands, shared memory, task context, and team spawning
- Persistence model: project-shared `.agent/` config plus user-local runtime state under `~/.agent/`

## Important Current Behavior

- `AgentTeam::run(...)` executes the tasks you add with `add_task(...)`. The `goal` string is threaded into each teammate's system prompt (prefixed as `Team goal: <goal>`) and is included in their context; if no tasks are pre-seeded, a single root task is created from the goal so the team has work to claim.
- `AgentTeam::run_single(...)` is the simplest programmatic entrypoint for one-agent work.
- The CLI is separate from `AgentTeam`. It uses a conversational loop and can dynamically call `spawn_agent_team`.
- Team infrastructure is written under `~/.agent/teams/<team-name>/` and `~/.agent/tasks/<team-name>/`, while `.agent/` in the repo is reserved for shared config files.
