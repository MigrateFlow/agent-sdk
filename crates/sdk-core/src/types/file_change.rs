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
