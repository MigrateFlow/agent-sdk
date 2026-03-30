# CLI Usage

The repository ships a binary named `agent` in [src/bin/agent.rs](/Users/ThangLT4/Desktop/code/rust-agent-sdk/src/bin/agent.rs).

It runs a conversational ReAct loop with tool access and optional team spawning.

## Start The CLI

```bash
cargo run --bin agent
```

One-shot mode:

```bash
cargo run --bin agent -- "inspect src/lib.rs and summarize public exports"
```

Interactive REPL mode:

```bash
cargo run --bin agent
```

## Flags

```text
-p, --provider <PROVIDER>          claude or openai
-m, --model <MODEL>                model id
-d, --dir <DIR>                    working directory, default "."
    --max-tokens <TOKENS>          default 16384
    --max-iterations <N>           default 50
    --system <PROMPT>              override system prompt
    --session <PATH>               custom REPL session file
    --allow-all-commands           legacy flag; commands are already unrestricted
```

Examples:

```bash
cargo run --bin agent -- -p openai -m gpt-4o
cargo run --bin agent -- -d /path/to/repo "review the main module"
cargo run --bin agent -- --session /tmp/agent-session.json
```

## Environment Resolution

Provider selection logic:

1. `--provider` if present
2. `LLM_PROVIDER=openai`
3. `OPENAI_API_KEY` present while `ANTHROPIC_API_KEY` is absent
4. otherwise Claude

Model selection logic:

1. `--model` if present
2. provider-specific env var: `ANTHROPIC_MODEL` or `OPENAI_MODEL`
3. `LLM_MODEL`
4. provider default

Current defaults:

- Claude: `claude-sonnet-4-20250514`
- OpenAI: `gpt-4o`

## Built-In CLI Tools

The CLI registers:

- `read_file`
- `write_file`
- `list_directory`
- `search_files`
- `web_search`
- `run_command`
- `update_task_list`
- `spawn_agent_team`

`run_command` can execute any shell command available inside the current environment.

`--allow-all-commands` is now effectively a no-op and kept only for backward compatibility.

## REPL Commands

Interactive mode supports:

- `/help`
- `/clear`
- `/new`
- `/tasks`
- `/status`
- `/quit`
- `/exit`
- `/q`

Session behavior:

- interactive mode now persists conversation history by default at `~/.agent/projects/<project>/sessions/cli-session.json`
- the current single-agent `Task` list is persisted in the same session file
- restarting the CLI in the same working directory resumes that conversation if the system prompt matches
- `/clear` and `/new` both reset the session back to the system prompt only
- `/compact` now selects a compaction strategy dynamically based on the current conversation shape
- `/tasks` shows the current visible `Task` list
- `/status` shows the active session file and current message count

One-shot mode still uses a fresh in-memory conversation and exits when the prompt completes.

## Team Spawning From The CLI

The CLI prompt explicitly tells the model that it may call `spawn_agent_team` for complex tasks.

When a team is spawned, the CLI now prints a plan preview before execution:

- teammate list and roles
- numbered task list
- declared dependencies
- auto-assignment mode

After the team starts, the CLI also prints a short task-assignment summary from the tool result before streaming teammate events.

Use cases where the current design makes sense:

- parallel code review
- feature work split across independent files
- multi-step analysis where one agent gathers context and others implement

The `spawn_agent_team` tool expects JSON like this:

```json
{
  "teammates": [
    {
      "name": "reader",
      "role": "Read the crate and identify public APIs"
    },
    {
      "name": "writer",
      "role": "Write documentation from the implementation details",
      "require_plan_approval": true
    }
  ],
  "auto_assign": true,
  "tasks": [
    {
      "title": "Inspect exports",
      "description": "Review src/lib.rs and summarize public modules.",
      "target_file": "docs/exports.md",
      "priority": 0
    },
    {
      "title": "Write docs",
      "description": "Create user-facing docs based on the export summary.",
      "target_file": "docs/usage.md",
      "depends_on": [0],
      "priority": 1
    }
  ]
}
```

Current behavior of `spawn_agent_team`:

- rejects duplicate `target_file` ownership
- optionally auto-assigns tasks to teammates based on keyword matching
- treats likely integration tasks as depending on all earlier tasks when no explicit dependencies are supplied
- runs a `TeamLead` directly using the current LLM client

## Output Model

The CLI prints:

- assistant thinking previews
- tool call previews
- tool result previews
- team lifecycle events
- final answer text
- token and tool call usage per turn

The event printer also shows teammate activity when a team is spawned.
