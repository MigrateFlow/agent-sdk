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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn new_store() -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(dir.path().join("mem")).unwrap();
        (dir, store)
    }

    #[test]
    fn new_creates_base_dir() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c");
        assert!(!target.exists());
        let _ = MemoryStore::new(target.clone()).unwrap();
        assert!(target.is_dir());
    }

    #[test]
    fn read_returns_none_for_missing_key() {
        let (_d, store) = new_store();
        assert!(store.read("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write(
                "k1",
                "hello world",
                agent,
                Some("Name".into()),
                Some("Desc".into()),
                MemoryType::Project,
            )
            .unwrap();
        let got = store.read("k1").unwrap().expect("entry exists");
        assert_eq!(got.key, "k1");
        assert_eq!(got.content, "hello world");
        assert_eq!(got.name.as_deref(), Some("Name"));
        assert_eq!(got.description.as_deref(), Some("Desc"));
        assert_eq!(got.memory_type, MemoryType::Project);
        assert_eq!(got.written_by, agent);
    }

    #[test]
    fn key_path_sanitizes_unsafe_chars() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        // Slash and spaces should be mapped to '_', not escape the base dir.
        store
            .write("a/b c", "x", agent, None, None, MemoryType::default())
            .unwrap();
        let keys = store.list_keys().unwrap();
        assert!(keys.iter().any(|k| k == "a_b_c"));
    }

    #[test]
    fn write_value_with_string() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write_value("raw", serde_json::Value::String("plain".into()), agent)
            .unwrap();
        let got = store.read("raw").unwrap().unwrap();
        assert_eq!(got.content, "plain");
    }

    #[test]
    fn write_value_with_object_serializes_to_json_string() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        let val = serde_json::json!({"a": 1, "b": [true, false]});
        store.write_value("obj", val.clone(), agent).unwrap();
        let got = store.read("obj").unwrap().unwrap();
        let reparsed: serde_json::Value = serde_json::from_str(&got.content).unwrap();
        assert_eq!(reparsed, val);
    }

    #[test]
    fn read_migrates_legacy_value_field() {
        let (_d, store) = new_store();
        // Write a legacy on-disk entry with only `value`, empty content.
        let legacy = serde_json::json!({
            "key": "legacy",
            "value": "legacy-content",
            "written_by": Uuid::new_v4(),
            "written_at": chrono::Utc::now(),
        });
        let path = store.key_path("legacy");
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();

        let got = store.read("legacy").unwrap().unwrap();
        assert_eq!(got.content, "legacy-content");
    }

    #[test]
    fn read_migrates_legacy_value_field_from_object() {
        let (_d, store) = new_store();
        let legacy = serde_json::json!({
            "key": "legobj",
            "value": {"x": 42},
            "written_by": Uuid::new_v4(),
            "written_at": chrono::Utc::now(),
        });
        let path = store.key_path("legobj");
        std::fs::write(&path, serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let got = store.read("legobj").unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&got.content).unwrap();
        assert_eq!(parsed["x"], 42);
    }

    #[test]
    fn list_keys_is_sorted_and_filters_non_json() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        for k in ["c", "a", "b"] {
            store
                .write(k, k, agent, None, None, MemoryType::default())
                .unwrap();
        }
        // Drop an unrelated file that should be ignored.
        std::fs::write(store.base_dir.join("notes.txt"), "ignored").unwrap();
        let keys = store.list_keys().unwrap();
        // MEMORY.md has no `.json` suffix, so it should be filtered out.
        assert!(!keys.iter().any(|k| k == "notes"));
        let mut mem_keys: Vec<_> = keys.iter().filter(|k| ["a", "b", "c"].contains(&k.as_str())).collect();
        mem_keys.sort();
        assert_eq!(mem_keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn delete_returns_true_when_present_false_when_absent() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write("gone", "bye", agent, None, None, MemoryType::default())
            .unwrap();
        assert!(store.delete("gone").unwrap());
        assert!(!store.delete("gone").unwrap());
        assert!(store.read("gone").unwrap().is_none());
    }

    #[test]
    fn search_filters_by_type() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write("u1", "a", agent, None, None, MemoryType::User)
            .unwrap();
        store
            .write("f1", "b", agent, None, None, MemoryType::Feedback)
            .unwrap();
        let results = store.search(Some(&MemoryType::User), None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "u1");
    }

    #[test]
    fn search_filters_by_keyword_across_fields() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write(
                "alpha",
                "nothing interesting here",
                agent,
                Some("Alpha title".into()),
                Some("describes the WIDGET system".into()),
                MemoryType::Project,
            )
            .unwrap();
        store
            .write(
                "beta",
                "widget appears IN content",
                agent,
                None,
                None,
                MemoryType::Project,
            )
            .unwrap();
        store
            .write("gamma", "unrelated", agent, None, None, MemoryType::Project)
            .unwrap();

        // Keyword matches in description (case-insensitive)
        let r = store.search(None, Some("widget")).unwrap();
        let mut matched: Vec<_> = r.into_iter().map(|e| e.key).collect();
        matched.sort();
        assert_eq!(matched, vec!["alpha", "beta"]);

        // Keyword matches only in key
        let r = store.search(None, Some("gamm")).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key, "gamma");

        // Keyword matches in name field
        let r = store.search(None, Some("alpha title")).unwrap();
        assert_eq!(r.len(), 1);

        // No matches
        let r = store.search(None, Some("zzz-nope")).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn search_combines_type_and_keyword() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write("a", "foo", agent, None, None, MemoryType::User)
            .unwrap();
        store
            .write("b", "foo", agent, None, None, MemoryType::Feedback)
            .unwrap();
        let r = store
            .search(Some(&MemoryType::User), Some("foo"))
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key, "a");
    }

    #[test]
    fn generate_index_reports_empty_state() {
        let (_d, store) = new_store();
        let idx = store.generate_index().unwrap();
        assert!(idx.contains("(no memories stored)"));
    }

    #[test]
    fn generate_index_lists_entries_with_type_and_description() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write(
                "apikey",
                "content",
                agent,
                None,
                Some("where to find api keys".into()),
                MemoryType::Reference,
            )
            .unwrap();
        store
            .write("nodesc", "content", agent, None, None, MemoryType::User)
            .unwrap();
        let idx = store.generate_index().unwrap();
        assert!(idx.contains("**apikey** [reference]"));
        assert!(idx.contains("where to find api keys"));
        assert!(idx.contains("**nodesc** [user]"));
        assert!(idx.contains("(no description)"));
    }

    #[test]
    fn update_index_writes_memory_md() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write("x", "y", agent, None, None, MemoryType::default())
            .unwrap();
        // write() calls update_index internally; assert MEMORY.md exists.
        let idx_path = store.base_dir.join("MEMORY.md");
        assert!(idx_path.exists());
        let body = std::fs::read_to_string(idx_path).unwrap();
        assert!(body.contains("**x**"));
    }

    #[test]
    fn load_index_returns_none_when_empty_or_missing() {
        let (_d, store) = new_store();
        assert!(store.load_index().unwrap().is_none());

        // Write explicit empty index
        std::fs::write(store.base_dir.join("MEMORY.md"), "   \n").unwrap();
        assert!(store.load_index().unwrap().is_none());

        // Index containing "no memories stored" sentinel also returns None.
        std::fs::write(
            store.base_dir.join("MEMORY.md"),
            "# Memory Index\n\n(no memories stored)",
        )
        .unwrap();
        assert!(store.load_index().unwrap().is_none());
    }

    #[test]
    fn load_index_returns_content_when_populated() {
        let (_d, store) = new_store();
        let agent = Uuid::new_v4();
        store
            .write(
                "k",
                "v",
                agent,
                None,
                Some("d".into()),
                MemoryType::Project,
            )
            .unwrap();
        let body = store.load_index().unwrap().expect("populated index");
        assert!(body.contains("**k**"));
    }
}
