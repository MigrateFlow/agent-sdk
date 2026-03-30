# Rust Agent SDK Features

## Overview
The Rust Agent SDK is a multi-agent orchestration framework that enables teams of LLM-powered agents to work together on complex tasks. It provides both single-agent and multi-agent capabilities with ReAct loops, task management, and LLM integration.

## Core Components

### Agent Teams
- **Team Lead**: Central coordinator that manages teammates and orchestrates work
- **Teammates**: Independent agent instances with their own context windows
- **Shared Infrastructure**: Task store, message broker, and memory store for coordination
- **Architecture**: Team lead coordinates work while teammates claim tasks from shared lists

### Agent Loop
- **Single-Agent Control**: Low-level control for individual agents
- **Context Management**: Automatic context window management and compaction
- **Tool Integration**: Direct access to file system, commands, and custom tools
- **Event Streaming**: Real-time monitoring of agent activities

## LLM Integration

### Supported Providers
- **Anthropic Claude**: Full Claude API integration
- **OpenAI**: GPT model support
- **Extensible**: Easy to add new LLM providers by implementing the `LlmClient` trait

### Features
- **Rate Limiting**: Built-in rate limiting for API calls
- **Retry Logic**: Automatic retry with exponential backoff
- **Token Tracking**: Comprehensive token usage tracking
- **Chat Completion**: Full support for conversation-based interactions

## Agent Teams Capabilities

### Team Composition
- **Named Teammates**: Define specialized roles with specific prompts
- **Generic Pool**: Dynamic teammate allocation up to max parallel agents
- **Plan Approval Mode**: Require teammates to submit plans before implementation
- **Custom Prompt Builders**: Domain-specific system prompts per teammate

### Task Management
- **Explicit Tasks**: Structured work items with priority, dependencies, and metadata
- **Dependency Resolution**: Automatic blocking/unblocking based on task completion
- **File-Based Persistence**: Task states stored on disk with file locking
- **Task State Machine**: Pending → Claimed → InProgress → Completed/Failed/Blocked

### Communication
- **Direct Messaging**: Teammates can message each other through the broker
- **Broadcast Support**: Send messages to all teammates simultaneously
- **Shared Memory**: Key-value store for inter-agent coordination
- **Mailbox System**: File-backed messaging infrastructure

## Built-in Tools

### File System Tools
- **read_file**: Read files from source or work directories with pagination
- **write_file**: Write content to work directory with automatic directory creation
- **list_directory**: Directory listing with file type information
- **search_files**: Pattern-based file search with content matching

### Command Tools
- **run_command**: Execute shell commands with configurable timeouts
- **Allowlist Support**: Restrict available commands for security
- **Working Directory**: Commands execute in designated work directory

### Memory Tools
- **read_memory**: Access shared key-value store
- **write_memory**: Store data for inter-agent communication
- **list_memory**: Enumerate keys in shared memory store

### Context Tools
- **get_task_context**: Retrieve information about completed tasks
- **list_completed_tasks**: Get list of finished tasks with details

## Advanced Features

### Quality Gates (Hooks)
- **TeammateIdle**: Keep teammates active if they haven't completed work
- **TaskCreated**: Validate task definitions before creation
- **TaskCompleted**: Enforce criteria before marking tasks as done
- **Event-Based**: React to lifecycle events with custom logic

### Event Monitoring
- **Comprehensive Events**: TeamSpawned, TaskStarted, TaskCompleted, PlanSubmitted, etc.
- **Real-time Streaming**: Unbounded channels for monitoring agent activity
- **Rich Information**: Detailed event data for logging and metrics
- **External Integration**: Connect to UIs, dashboards, or logging systems

### Custom Extensions
- **Custom Tools**: Implement the `Tool` trait for new agent capabilities
- **Custom Prompt Builders**: Override system/user prompts for domain-specific behavior
- **Custom LLM Clients**: Integrate with any LLM service via the `LlmClient` trait
- **Hook System**: Enforce business rules and quality standards

## Usage Patterns

### Single-Agent Workflows
- **Simple Tasks**: Focused operations that don't require parallelism
- **Quick Operations**: Fast, single-threaded processing
- **Sequential Processing**: Linear task execution

### Multi-Agent Teams
- **Parallel Exploration**: Multiple teammates investigate different aspects simultaneously
- **Module Development**: Different teammates own separate pieces of work
- **Hypothesis Testing**: Competing theories tested in parallel
- **Cross-Layer Coordination**: Frontend, backend, and test changes coordinated separately

## Runtime Infrastructure

### File-Based Storage
- **Task Persistence**: Tasks stored in categorized directories (pending, in_progress, etc.)
- **Message Queues**: File-backed mailboxes for each agent
- **Shared Memory**: Key-value storage in JSON format
- **Lock Files**: Prevent race conditions during concurrent access

### Configuration
- **Source Root**: Read-only directory for source code access
- **Work Directory**: Write destination for generated content
- **Max Iterations**: Configurable loop limits to prevent infinite runs
- **Parallel Agents**: Maximum number of concurrent teammates

## Performance & Reliability

### Concurrency Control
- **File Locking**: Prevents race conditions when multiple agents access shared resources
- **Task Claiming**: Atomic task assignment to prevent duplicate work
- **Parallel Execution**: Efficient utilization of multiple agents
- **Resource Isolation**: Each teammate operates in its own context

### Error Handling
- **Retry Mechanisms**: Automatic recovery from transient failures
- **Graceful Degradation**: Continues operation despite partial failures
- **Comprehensive Logging**: Detailed tracing for debugging
- **Event Reporting**: Clear indication of success/failure states