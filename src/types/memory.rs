use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::AgentId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: String,
    pub value: serde_json::Value,
    pub written_by: AgentId,
    pub written_at: DateTime<Utc>,
}
