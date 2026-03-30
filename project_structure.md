# Rust Agent SDK - Project Structure Analysis

## Overview
The Rust Agent SDK is a multi-agent orchestration framework that allows teams of AI agents to work together on complex tasks. The SDK provides both single-agent and multi-agent team orchestration capabilities with built-in tools for file operations, shell commands, and inter-agent communication.

## Directory Structure
```
.
├── Cargo.toml          # Project manifest with dependencies
├── Cargo.lock          # Dependency lock file
├── README.md           # Main project documentation
├── .env               # Environment variables (likely ignored)
├── .gitignore         # Git ignore rules
├── docs/              # Comprehensive documentation
├── src/               # Source code
│   ├── agent/         # Core agent implementations
│   ├── bin/           # Binary entry points
│   ├── config.rs      # Configuration structures
│   ├── error.rs       # Error types and definitions
│   ├── lib.rs         # Library exports and module declarations
│   ├── llm/           # LLM provider integrations
│   ├── mailbox/       # Inter-agent messaging system
│   ├── prompts.rs     # Prompt templates
│   ├── task/          # Task management system
│   ├── tools/         # Built-in tools
│   ├── traits/        # Core trait definitions
│   └── types/         # Type definitions
├── target/            # Build artifacts (ignored)
└── .agent/            # Runtime data directory (created during execution)
```

## Key Components

### 1. Core Agent Module (`src/agent/`)
- `agent_loop.rs`: Core ReAct loop implementation for individual agents
- `team.rs`: High-level `AgentTeam` orchestrator
- `team_lead.rs`: Team lead implementation that coordinates teammates
- `teammate.rs`: Individual teammate implementation
- `memory.rs`: Shared memory store for inter-agent communication
- `events.rs`: Event system for monitoring agent activities
- `hooks.rs`: Lifecycle hooks for controlling agent behavior
- `context.rs`: Context management for agents
- `handle.rs`: Agent handles for external interaction
- `registry.rs`: Agent registry functionality

### 2. LLM Integration (`src/llm/`)
- `claude.rs`: Anthropic Claude API client
- `openai.rs`: OpenAI API client
- `rate_limiter.rs`: Rate limiting for API calls
- `retry.rs`: Retry logic for failed requests
- `util.rs`: Utility functions for LLM interactions

### 3. Task Management (`src/task/`)
- `store.rs`: Persistent task storage with file locking
- `graph.rs`: Task dependency graph management
- `watcher.rs`: Task state monitoring
- `file_lock.rs`: File-based locking mechanism for task claiming

### 4. Communication (`src/mailbox/`)
- `broker.rs`: Message routing and delivery system
- `mailbox.rs`: Individual agent mailboxes

### 5. Tools (`src/tools/`)
- `registry.rs`: Tool registration and execution
- `fs_tools.rs`: File system operations (read/write/list)
- `command_tools.rs`: Shell command execution
- `memory_tools.rs`: Shared memory operations
- `search_tools.rs`: File content searching
- `context_tools.rs`: Context-related operations
- `team_tools.rs`: Team spawning and management

### 6. Traits (`src/traits/`)
- `llm_client.rs`: Abstract LLM client interface
- `tool.rs`: Tool trait definition
- `prompt_builder.rs`: Prompt customization interface

### 7. Types (`src/types/`)
- `chat.rs`: Chat message structures
- `task.rs`: Task data structures
- `message.rs`: Inter-agent message formats
- `memory.rs`: Memory store data structures
- `file_change.rs`: File change tracking

## Architecture Patterns

### Multi-Agent Orchestration
The SDK implements a team-based architecture where:
- A **Team Lead** coordinates work and manages teammates
- **Teammates** work independently on assigned tasks
- Shared services include TaskStore, MemoryStore, and MessageBroker
- Tasks are claimed using file locking to prevent race conditions

### Dependency Injection
The system uses dependency injection for:
- LLM clients (Anthropic/OpenAI)
- Tool registries
- Configuration objects
- Event channels

### Async Runtime
Built on Tokio with:
- Asynchronous tool execution
- Non-blocking I/O operations
- Concurrent teammate processing

### Plugin Architecture
- Custom tools can implement the `Tool` trait
- Custom LLM clients can implement the `LlmClient` trait
- Hooks system allows for behavior modification
- Prompt builders can customize agent prompts

## Configuration and Setup

### Cargo.toml Dependencies
Key dependencies include:
- `tokio`: Async runtime
- `serde`/`serde_json`: Serialization
- `reqwest`: HTTP client for LLM APIs
- `uuid`: Unique identifier generation
- `clap`: CLI argument parsing
- `tracing`: Logging and diagnostics
- `petgraph`: Task dependency graphs
- `fs2`: File system locking

### Entry Points
- `src/bin/agent.rs`: Interactive CLI application
- Library exports via `src/lib.rs` for programmatic use

## Runtime Behavior

### File-Based Persistence
The system stores runtime data in the `.agent/` directory:
- Tasks in various states (pending, in_progress, completed, failed)
- Memory store for inter-agent communication
- Mailbox directories for each agent

### CLI Features
The interactive CLI includes:
- Slash commands (/help, /clear, /compact, /cost, /quit)
- Multi-line input support
- Real-time tool call visualization
- Token usage tracking
- Team spawn monitoring

## Extensibility Points

### Custom Tools
Developers can create new tools by implementing the `Tool` trait with:
- Definition method for schema
- Execute method for functionality

### Custom LLM Clients
New providers can be integrated by implementing the `LlmClient` trait

### Hooks System
Lifecycle hooks allow interception of:
- Task creation/completion
- Teammate idle states
- Plan submission/approval

### Event Monitoring
Event system enables real-time monitoring of:
- Agent activities
- Tool calls
- Task progress
- Team dynamics

## Design Philosophy

The SDK follows several key design principles:
- **Modularity**: Each component has a clear responsibility
- **Composability**: Components can be combined flexibly
- **Persistence**: Work state survives interruptions
- **Concurrency**: Multiple agents work in parallel
- **Extensibility**: Easy to add new capabilities
- **Observability**: Rich event and logging system