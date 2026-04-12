//! LLM-callable tools for entering and exiting Plan mode and UltraPlan mode.
//!
//! These tools let the agent *programmatically* switch modes without the user
//! having to type `/plan` or `/ultraplan`.  The shared [`ModeState`] struct is
//! read by the REPL loop in `main.rs` so that tool-filter and system-prompt
//! changes take effect on the very next iteration.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use sdk_core::error::{SdkError, SdkResult};
use sdk_core::traits::tool::{Tool, ToolDefinition};
use sdk_core::types::agent_mode::AgentMode;
use sdk_core::types::ultra_plan::{UltraPlanState, next_phase};

/// Shared mutable state that both LLM tools and the REPL loop can access.
#[derive(Clone)]
pub struct ModeState {
    pub agent_mode: Arc<Mutex<AgentMode>>,
    pub ultra_plan: Arc<Mutex<Option<UltraPlanState>>>,
}

impl ModeState {
    pub fn new(mode: AgentMode, ultra: Option<UltraPlanState>) -> Self {
        Self {
            agent_mode: Arc::new(Mutex::new(mode)),
            ultra_plan: Arc::new(Mutex::new(ultra)),
        }
    }
}

// ─── enter_plan_mode ────────────────────────────────────────────────────────

pub struct EnterPlanModeTool {
    pub state: ModeState,
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "enter_plan_mode".to_string(),
            description: "Enter read-only plan mode. In plan mode only exploration tools are \
                available (read_file, glob, grep, search_files, list_directory, web_search, \
                spawn_subagent). Use this before implementing complex changes so you can \
                thoroughly understand the codebase first. Call exit_plan_mode when ready to \
                implement."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Brief reason for entering plan mode (shown to user)."
                    }
                },
                "required": ["reason"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let reason = arguments["reason"]
            .as_str()
            .unwrap_or("exploring before implementing")
            .to_string();

        // Check if already in ultraplan
        {
            let ultra = self.state.ultra_plan.lock().map_err(|e| SdkError::ToolExecution {
                tool_name: "enter_plan_mode".into(),
                message: format!("lock error: {e}"),
            })?;
            if ultra.is_some() {
                return Ok(json!({
                    "error": "Cannot enter plan mode while UltraPlan is active. Use exit_ultraplan first."
                }));
            }
        }

        let mut mode = self.state.agent_mode.lock().map_err(|e| SdkError::ToolExecution {
            tool_name: "enter_plan_mode".into(),
            message: format!("lock error: {e}"),
        })?;

        if *mode == AgentMode::Plan {
            return Ok(json!({ "status": "already_in_plan_mode" }));
        }

        *mode = AgentMode::Plan;

        Ok(json!({
            "status": "plan_mode_activated",
            "reason": reason,
            "message": "Plan mode is now active. Only read-only tools are available. Call exit_plan_mode when you are ready to implement."
        }))
    }
}

// ─── exit_plan_mode ─────────────────────────────────────────────────────────

pub struct ExitPlanModeTool {
    pub state: ModeState,
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exit_plan_mode".to_string(),
            description: "Exit plan mode and return to normal mode with full tool access. \
                Call this after you have finished exploring and are ready to implement."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let mut mode = self.state.agent_mode.lock().map_err(|e| SdkError::ToolExecution {
            tool_name: "exit_plan_mode".into(),
            message: format!("lock error: {e}"),
        })?;

        if *mode != AgentMode::Plan {
            return Ok(json!({ "status": "not_in_plan_mode" }));
        }

        *mode = AgentMode::Normal;

        Ok(json!({
            "status": "plan_mode_deactivated",
            "message": "Full tool access restored. You can now implement your plan."
        }))
    }
}

// ─── enter_ultraplan ────────────────────────────────────────────────────────

pub struct EnterUltraPlanTool {
    pub state: ModeState,
}

#[async_trait]
impl Tool for EnterUltraPlanTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "enter_ultraplan".to_string(),
            description: "Start a structured 4-phase UltraPlan workflow: \
                Research → Design → Review → Implement. Each phase restricts \
                available tools to enforce discipline. Use for large, complex tasks \
                that benefit from phased execution. Starts in the Research phase."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Brief reason for starting UltraPlan (shown to user)."
                    }
                },
                "required": ["reason"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let reason = arguments["reason"]
            .as_str()
            .unwrap_or("complex multi-phase task")
            .to_string();

        // Check if already in ultraplan
        {
            let ultra = self.state.ultra_plan.lock().map_err(|e| SdkError::ToolExecution {
                tool_name: "enter_ultraplan".into(),
                message: format!("lock error: {e}"),
            })?;
            if ultra.is_some() {
                return Ok(json!({
                    "error": "UltraPlan is already active. Use advance_ultraplan_phase or exit_ultraplan."
                }));
            }
        }

        // Set ultraplan state
        {
            let mut ultra = self.state.ultra_plan.lock().map_err(|e| SdkError::ToolExecution {
                tool_name: "enter_ultraplan".into(),
                message: format!("lock error: {e}"),
            })?;
            *ultra = Some(UltraPlanState::default());
        }

        // Also set agent mode to Normal (ultraplan overrides plan mode)
        {
            let mut mode = self.state.agent_mode.lock().map_err(|e| SdkError::ToolExecution {
                tool_name: "enter_ultraplan".into(),
                message: format!("lock error: {e}"),
            })?;
            *mode = AgentMode::Normal;
        }

        Ok(json!({
            "status": "ultraplan_started",
            "phase": "Research",
            "reason": reason,
            "message": "UltraPlan started in Research phase. Read-only exploration tools available. Use advance_ultraplan_phase to progress through: Research → Design → Review → Implement.",
            "phases": ["Research", "Design", "Review", "Implement"]
        }))
    }
}

// ─── advance_ultraplan_phase ────────────────────────────────────────────────

pub struct AdvanceUltraPlanPhaseTool {
    pub state: ModeState,
}

#[async_trait]
impl Tool for AdvanceUltraPlanPhaseTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "advance_ultraplan_phase".to_string(),
            description: "Advance to the next UltraPlan phase. Phases progress: \
                Research → Design → Review → Implement. Each phase adjusts available \
                tools. Cannot advance past Implement."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let mut ultra = self.state.ultra_plan.lock().map_err(|e| SdkError::ToolExecution {
            tool_name: "advance_ultraplan_phase".into(),
            message: format!("lock error: {e}"),
        })?;

        let state = match ultra.as_ref() {
            Some(s) => s.clone(),
            None => {
                return Ok(json!({
                    "error": "UltraPlan is not active. Use enter_ultraplan first."
                }));
            }
        };

        match next_phase(&state.phase) {
            Some(new_phase) => {
                let phase_name = new_phase.to_string();
                *ultra = Some(UltraPlanState { phase: new_phase });
                Ok(json!({
                    "status": "phase_advanced",
                    "phase": phase_name,
                    "message": format!("Advanced to {} phase.", phase_name)
                }))
            }
            None => Ok(json!({
                "status": "already_at_final_phase",
                "phase": "Implement",
                "message": "Already in the Implement phase (final). Use exit_ultraplan to return to normal mode."
            })),
        }
    }
}

// ─── exit_ultraplan ─────────────────────────────────────────────────────────

pub struct ExitUltraPlanTool {
    pub state: ModeState,
}

#[async_trait]
impl Tool for ExitUltraPlanTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "exit_ultraplan".to_string(),
            description: "Exit UltraPlan mode and return to normal mode with full tool access."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, _arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let mut ultra = self.state.ultra_plan.lock().map_err(|e| SdkError::ToolExecution {
            tool_name: "exit_ultraplan".into(),
            message: format!("lock error: {e}"),
        })?;

        if ultra.is_none() {
            return Ok(json!({ "status": "not_in_ultraplan" }));
        }

        *ultra = None;

        Ok(json!({
            "status": "ultraplan_exited",
            "message": "UltraPlan exited. Full tool access restored."
        }))
    }
}
