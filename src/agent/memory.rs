use std::path::PathBuf;

use chrono::Utc;

use crate::error::{AgentId, SdkError, SdkResult};
use crate::types::memory::MemoryEntry;

pub struct MemoryStore {
    base_dir: PathBuf,
}

impl MemoryStore {
    pub fn new(base_dir: PathBuf) -> SdkResult<Self> {
        std::fs::create_dir_all(&base_dir).map_err(SdkError::Io)?;
        Ok(Self { base_dir })
    }

    fn key_path(&self, key: &str) -> PathBuf {
        let safe_key: String = key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base_dir.join(format!("{}.json", safe_key))
    }

    pub fn read(&self, key: &str) -> SdkResult<Option<MemoryEntry>> {
        let path = self.key_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let entry: MemoryEntry = serde_json::from_str(&content)?;
        Ok(Some(entry))
    }

    pub fn write(&self, key: &str, value: serde_json::Value, agent_id: AgentId) -> SdkResult<()> {
        let entry = MemoryEntry {
            key: key.to_string(),
            value,
            written_by: agent_id,
            written_at: Utc::now(),
        };
        let content = serde_json::to_string_pretty(&entry)?;
        let path = self.key_path(key);
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn list_keys(&self) -> SdkResult<Vec<String>> {
        let mut keys = Vec::new();
        if self.base_dir.exists() {
            for entry in std::fs::read_dir(&self.base_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(key) = name.strip_suffix(".json") {
                    keys.push(key.to_string());
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}
