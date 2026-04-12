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
