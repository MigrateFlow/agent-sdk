use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::AgentId;

/// Categorizes memories by their purpose, matching Claude Code's memory types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// Information about the user's role, goals, and preferences
    User,
    /// Guidance the user has given about how to approach work
    Feedback,
    /// Information about ongoing work, goals, initiatives
    Project,
    /// Pointers to where information can be found in external systems
    Reference,
}

impl Default for MemoryType {
    fn default() -> Self {
        MemoryType::Reference
    }
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::User => write!(f, "user"),
            MemoryType::Feedback => write!(f, "feedback"),
            MemoryType::Project => write!(f, "project"),
            MemoryType::Reference => write!(f, "reference"),
        }
    }
}

impl MemoryType {
    /// Parse a memory type from a string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "user" => Some(MemoryType::User),
            "feedback" => Some(MemoryType::Feedback),
            "project" => Some(MemoryType::Project),
            "reference" => Some(MemoryType::Reference),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: String,

    /// Human-readable name (defaults to key if not set)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// One-line description used for index and relevance matching
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Structured memory type
    #[serde(default)]
    pub memory_type: MemoryType,

    /// The actual memory content as a string
    #[serde(default)]
    pub content: String,

    /// Legacy field: kept for backward-compatible deserialization of old entries.
    /// New writes never produce this field.
    #[serde(default, skip_serializing)]
    pub value: Option<serde_json::Value>,

    pub written_by: AgentId,
    pub written_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_reference() {
        assert_eq!(MemoryType::default(), MemoryType::Reference);
    }

    #[test]
    fn display_matches_snake_case() {
        assert_eq!(MemoryType::User.to_string(), "user");
        assert_eq!(MemoryType::Feedback.to_string(), "feedback");
        assert_eq!(MemoryType::Project.to_string(), "project");
        assert_eq!(MemoryType::Reference.to_string(), "reference");
    }

    #[test]
    fn parse_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(MemoryType::parse("USER"), Some(MemoryType::User));
        assert_eq!(MemoryType::parse("Feedback"), Some(MemoryType::Feedback));
        assert_eq!(MemoryType::parse("project"), Some(MemoryType::Project));
        assert_eq!(MemoryType::parse("Reference"), Some(MemoryType::Reference));
        assert!(MemoryType::parse("other").is_none());
        assert!(MemoryType::parse("").is_none());
    }

    #[test]
    fn serde_roundtrip_entry_drops_value_field_on_write() {
        // `value` is skip_serializing, so it never appears in the output JSON.
        let entry = MemoryEntry {
            key: "k".into(),
            name: None,
            description: None,
            memory_type: MemoryType::default(),
            content: "body".into(),
            value: Some(serde_json::json!({"x": 1})),
            written_by: uuid::Uuid::new_v4(),
            written_at: chrono::Utc::now(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("value").is_none());
        // But deserializing a payload with value still populates the field.
        let back: MemoryEntry = serde_json::from_value(serde_json::json!({
            "key": "k",
            "content": "",
            "value": {"x": 1},
            "written_by": uuid::Uuid::new_v4(),
            "written_at": chrono::Utc::now(),
        }))
        .unwrap();
        assert!(back.value.is_some());
    }
}
