use crate::error::AgentId;
use crate::types::task::Task;

/// Events that can trigger hooks.
#[derive(Debug, Clone)]
pub enum HookEvent {
    /// A teammate has finished its current work and is about to go idle.
    /// Return `HookResult::Reject` with feedback to keep it working.
    TeammateIdle {
        agent_id: AgentId,
        name: String,
        tasks_completed: usize,
    },

    /// A task is being created in the task store.
    /// Return `HookResult::Reject` to prevent creation.
    TaskCreated {
        task: Task,
    },

    /// A task is being marked as completed.
    /// Return `HookResult::Reject` to prevent completion and send feedback.
    TaskCompleted {
        task: Task,
        agent_id: AgentId,
    },
}

/// Result of a hook evaluation.
#[derive(Debug, Clone)]
pub enum HookResult {
    /// Allow the action to proceed.
    Continue,
    /// Reject the action with feedback (equivalent to exit code 2 in Claude Code).
    Reject { feedback: String },
}

/// Trait for implementing quality gates and lifecycle hooks.
///
/// Hooks run synchronously during agent team execution. They can inspect
/// events and either allow them to proceed or reject them with feedback.
pub trait Hook: Send + Sync {
    fn on_event(&self, event: &HookEvent) -> HookResult;
}

/// A collection of hooks that are evaluated in order.
pub struct HookRegistry {
    hooks: Vec<Box<dyn Hook>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn add(&mut self, hook: impl Hook + 'static) {
        self.hooks.push(Box::new(hook));
    }

    /// Evaluate all hooks for an event. Returns `Reject` on the first rejection.
    pub fn evaluate(&self, event: &HookEvent) -> HookResult {
        for hook in &self.hooks {
            if let HookResult::Reject { feedback } = hook.on_event(event) {
                return HookResult::Reject { feedback };
            }
        }
        HookResult::Continue
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}
