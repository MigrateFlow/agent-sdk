use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub original_content: Option<String>,
    pub new_content: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChangeType {
    Modified,
    Created,
    Deleted,
    Renamed { from: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub start_line: usize,
    pub end_line: usize,
    pub original: String,
    pub replacement: String,
    pub rationale: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_type_serializes_with_snake_case_tag() {
        let j = serde_json::to_value(&ChangeType::Modified).unwrap();
        assert_eq!(j, serde_json::json!({"type": "modified"}));

        let j = serde_json::to_value(&ChangeType::Created).unwrap();
        assert_eq!(j, serde_json::json!({"type": "created"}));

        let j = serde_json::to_value(&ChangeType::Deleted).unwrap();
        assert_eq!(j, serde_json::json!({"type": "deleted"}));

        let j =
            serde_json::to_value(&ChangeType::Renamed { from: PathBuf::from("old") }).unwrap();
        assert_eq!(j, serde_json::json!({"type": "renamed", "from": "old"}));
    }

    #[test]
    fn file_change_roundtrip() {
        let c = FileChange {
            path: PathBuf::from("lib.rs"),
            change_type: ChangeType::Modified,
            original_content: Some("old".into()),
            new_content: "new".into(),
            hunks: vec![Hunk {
                start_line: 1,
                end_line: 3,
                original: "a".into(),
                replacement: "b".into(),
                rationale: "because".into(),
            }],
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: FileChange = serde_json::from_str(&json).unwrap();
        assert_eq!(back.path, PathBuf::from("lib.rs"));
        assert_eq!(back.hunks.len(), 1);
        assert_eq!(back.hunks[0].rationale, "because");
    }
}
