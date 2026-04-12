use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant_with_tools(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self::Assistant {
            content,
            tool_calls,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }

    pub fn is_final_answer(&self) -> bool {
        matches!(self, ChatMessage::Assistant { tool_calls, .. } if tool_calls.is_empty())
    }

    pub fn text_content(&self) -> Option<&str> {
        match self {
            ChatMessage::System { content } => Some(content),
            ChatMessage::User { content } => Some(content),
            ChatMessage::Assistant { content, .. } => content.as_deref(),
            ChatMessage::Tool { content, .. } => Some(content),
        }
    }

    pub fn char_len(&self) -> usize {
        match self {
            ChatMessage::System { content } => content.len(),
            ChatMessage::User { content } => content.len(),
            ChatMessage::Assistant { content, tool_calls } => {
                let c = content.as_ref().map_or(0, |s| s.len());
                let t: usize = tool_calls
                    .iter()
                    .map(|tc| tc.function.name.len() + tc.function.arguments.len() + tc.id.len())
                    .sum();
                c + t
            }
            ChatMessage::Tool {
                content,
                tool_call_id,
            } => content.len() + tool_call_id.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn default_tool_type() -> String {
    "function".to_string()
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments string
    pub arguments: String,
}
