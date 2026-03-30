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
    --allow-all-commands           disable command allowlist
```

Examples:

```bash
cargo run --bin agent -- -p openai -m gpt-4o
cargo run --bin agent -- -d /path/to/repo "review the main module"
cargo run --bin agent -- --allow-all-commands "run the exact debug command you need"
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
- `run_command`
- `spawn_agent_team`

By default, `run_command` is limited to:

- `javac`
- `java`
- `mvn`
- `gradle`
- `cargo`
- `go`
- `npm`
- `node`
- `python`
- `python3`
- `cat`
- `head`
- `tail`
- `wc`
- `diff`
- `find`
- `grep`
- `ls`
- `tree`

`--allow-all-commands` removes that restriction.

## REPL Commands

Interactive mode supports:

- `/help`
- `/clear`
- `/quit`
- `/exit`
- `/q`

`/clear` resets the conversation back to the system prompt only.

## Team Spawning From The CLI

The CLI prompt explicitly tells the model that it may call `spawn_agent_team` for complex tasks.

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
