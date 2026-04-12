use std::path::PathBuf;

use chrono::Utc;

use crate::error::{AgentId, SdkError, SdkResult};
use crate::types::memory::{MemoryEntry, MemoryType};

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
        let raw = std::fs::read_to_string(&path)?;
        let mut entry: MemoryEntry = serde_json::from_str(&raw)?;

        // Migrate old format: if content is empty but value is set, convert
        if entry.content.is_empty() {
            if let Some(ref val) = entry.value {
                entry.content = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
            }
        }

        Ok(Some(entry))
    }

    /// Write a structured memory entry.
    pub fn write(
        &self,
        key: &str,
        content: &str,
        agent_id: AgentId,
        name: Option<String>,
        description: Option<String>,
        memory_type: MemoryType,
    ) -> SdkResult<()> {
        let entry = MemoryEntry {
            key: key.to_string(),
            name,
            description,
            memory_type,
            content: content.to_string(),
            value: None,
            written_by: agent_id,
            written_at: Utc::now(),
        };
        let serialized = serde_json::to_string_pretty(&entry)?;
        let path = self.key_path(key);
        std::fs::write(&path, serialized)?;
        // Keep the index up to date
        let _ = self.update_index();
        Ok(())
    }

    /// Legacy write method for backward compatibility.
    pub fn write_value(
        &self,
        key: &str,
        value: serde_json::Value,
        agent_id: AgentId,
    ) -> SdkResult<()> {
        let content = match &value {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        self.write(key, &content, agent_id, None, None, MemoryType::default())
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

    /// Delete a memory entry. Returns true if the entry existed.
    pub fn delete(&self, key: &str) -> SdkResult<bool> {
        let path = self.key_path(key);
        if path.exists() {
            std::fs::remove_file(&path).map_err(SdkError::Io)?;
            let _ = self.update_index();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Search memories by type and/or keyword.
    pub fn search(
        &self,
        memory_type: Option<&MemoryType>,
        keyword: Option<&str>,
    ) -> SdkResult<Vec<MemoryEntry>> {
        let keys = self.list_keys()?;
        let mut results = Vec::new();

        for key in keys {
            if let Some(entry) = self.read(&key)? {
                // Filter by type
                if let Some(mt) = memory_type {
                    if entry.memory_type != *mt {
                        continue;
                    }
                }

                // Filter by keyword (case-insensitive search across key, content, description)
                if let Some(kw) = keyword {
                    let kw_lower = kw.to_lowercase();
                    let matches = entry.key.to_lowercase().contains(&kw_lower)
                        || entry.content.to_lowercase().contains(&kw_lower)
                        || entry
                            .description
                            .as_ref()
                            .map(|d| d.to_lowercase().contains(&kw_lower))
                            .unwrap_or(false)
                        || entry
                            .name
                            .as_ref()
                            .map(|n| n.to_lowercase().contains(&kw_lower))
                            .unwrap_or(false);
                    if !matches {
                        continue;
                    }
                }

                results.push(entry);
            }
        }
        Ok(results)
    }

    /// Generate the MEMORY.md index content.
    pub fn generate_index(&self) -> SdkResult<String> {
        let keys = self.list_keys()?;
        let mut lines = vec!["# Memory Index".to_string(), String::new()];

        for key in &keys {
            if let Some(entry) = self.read(key)? {
                let desc = entry.description.as_deref().unwrap_or("(no description)");
                let type_label = entry.memory_type.to_string();
                lines.push(format!("- **{}** [{}]: {}", key, type_label, desc));
            }
        }

        if lines.len() <= 2 {
            lines.push("(no memories stored)".to_string());
        }

        Ok(lines.join("\n"))
    }

    /// Write or update the MEMORY.md index file in the memory directory.
    pub fn update_index(&self) -> SdkResult<()> {
        let index = self.generate_index()?;
        let index_path = self.base_dir.join("MEMORY.md");
        std::fs::write(&index_path, index)?;
        Ok(())
    }

    /// Load the MEMORY.md index for system prompt injection.
    /// Returns None if no index exists.
    pub fn load_index(&self) -> SdkResult<Option<String>> {
        let index_path = self.base_dir.join("MEMORY.md");
        if index_path.exists() {
            let content = std::fs::read_to_string(&index_path)?;
            if content.trim().is_empty() || content.contains("(no memories stored)") {
                return Ok(None);
            }
            Ok(Some(content))
        } else {
            Ok(None)
        }
    }
}
