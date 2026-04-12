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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_build_correct_variants() {
        assert!(matches!(ChatMessage::system("sys"), ChatMessage::System { .. }));
        assert!(matches!(ChatMessage::user("u"), ChatMessage::User { .. }));
        assert!(matches!(ChatMessage::assistant("a"), ChatMessage::Assistant { .. }));
        assert!(matches!(
            ChatMessage::tool_result("id", "r"),
            ChatMessage::Tool { .. }
        ));
    }

    #[test]
    fn assistant_with_tools_preserves_fields() {
        let tc = ToolCall::new("id-1", "read", "{\"x\":1}");
        let msg = ChatMessage::assistant_with_tools(Some("hi".into()), vec![tc.clone()]);
        match msg {
            ChatMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("hi"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "id-1");
                assert_eq!(tool_calls[0].call_type, "function");
                assert_eq!(tool_calls[0].function.name, "read");
                assert_eq!(tool_calls[0].function.arguments, "{\"x\":1}");
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn is_final_answer_true_only_for_assistant_without_tool_calls() {
        assert!(ChatMessage::assistant("done").is_final_answer());
        assert!(!ChatMessage::user("q").is_final_answer());
        assert!(!ChatMessage::system("s").is_final_answer());
        assert!(!ChatMessage::tool_result("id", "r").is_final_answer());
        let with_tools = ChatMessage::assistant_with_tools(
            None,
            vec![ToolCall::new("id", "t", "{}")],
        );
        assert!(!with_tools.is_final_answer());
    }

    #[test]
    fn text_content_returns_expected_string() {
        assert_eq!(ChatMessage::system("s").text_content(), Some("s"));
        assert_eq!(ChatMessage::user("u").text_content(), Some("u"));
        assert_eq!(ChatMessage::assistant("a").text_content(), Some("a"));
        assert_eq!(
            ChatMessage::tool_result("id", "r").text_content(),
            Some("r")
        );
        // Assistant with None content
        let none_assistant = ChatMessage::assistant_with_tools(None, vec![]);
        assert_eq!(none_assistant.text_content(), None);
    }

    #[test]
    fn char_len_covers_all_variants() {
        assert_eq!(ChatMessage::system("abc").char_len(), 3);
        assert_eq!(ChatMessage::user("hello").char_len(), 5);
        assert_eq!(ChatMessage::tool_result("id", "body").char_len(), 2 + 4);

        let msg = ChatMessage::assistant_with_tools(
            Some("hi".into()),
            vec![ToolCall::new("abc", "name", "args")],
        );
        // content=2 + id=3 + name=4 + args=4 = 13
        assert_eq!(msg.char_len(), 13);

        let none_content = ChatMessage::assistant_with_tools(None, vec![]);
        assert_eq!(none_content.char_len(), 0);
    }

    #[test]
    fn serde_tool_call_default_type_is_function() {
        // Old payloads without "type" should default to "function".
        let json = r#"{"id":"1","function":{"name":"n","arguments":"{}"}}"#;
        let tc: ToolCall = serde_json::from_str(json).unwrap();
        assert_eq!(tc.call_type, "function");
    }

    #[test]
    fn serde_chat_message_roundtrip_assistant_with_tools() {
        let orig = ChatMessage::assistant_with_tools(
            Some("ok".into()),
            vec![ToolCall::new("i", "n", "{}")],
        );
        let json = serde_json::to_string(&orig).unwrap();
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        match back {
            ChatMessage::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("ok"));
                assert_eq!(tool_calls.len(), 1);
            }
            _ => panic!("expected Assistant"),
        }
    }
}
