//! Language Server Protocol integration.
//!
//! This module provides a minimal LSP client plus a lazy-spawning manager that
//! maps files to language servers using a JSON manifest (`.agent/lsp.json`).

pub mod client;

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{SdkError, SdkResult};

pub use client::{ChildLspClient, LspClient};

/// Specification for how to launch a single language server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Manifest of configured LSP servers, keyed by a canonical language id such
/// as `"rust"` or `"typescript"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LspConfig {
    #[serde(flatten)]
    pub servers: HashMap<String, ServerSpec>,
}

impl LspConfig {
    /// Load a manifest from disk. Returns an error if the file is present but
    /// invalid; callers that want to treat "missing" as "no config" should
    /// check `path.exists()` first.
    pub fn load(path: &Path) -> SdkResult<Self> {
        let text = std::fs::read_to_string(path).map_err(SdkError::Io)?;
        let cfg: LspConfig = serde_json::from_str(&text)?;
        Ok(cfg)
    }

    /// Look up a server spec by canonical language id.
    pub fn server_for(&self, language: &str) -> Option<&ServerSpec> {
        self.servers.get(language)
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// Map a file extension to a canonical language id. Returns `None` for
/// unknown extensions so tools can surface a clear error.
pub fn language_id_for_path(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "sh" | "bash" => "shellscript",
        "lua" => "lua",
        "json" => "json",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        _ => return None,
    })
}

/// Build a `file://` URI from an absolute path. Returns `None` if the path is
/// not representable.
pub fn path_to_uri(path: &Path) -> Option<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let s = absolute.to_str()?;
    // Basic percent-encoding for spaces; sufficient for local dev paths.
    let encoded = s.replace(' ', "%20");
    Some(if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        // Windows-style path; prepend an extra slash.
        format!("file:///{encoded}")
    })
}

/// Lazily spawns and caches one `ChildLspClient` per configured language.
pub struct LspManager {
    config: LspConfig,
    root_uri: String,
    clients: HashMap<String, ChildLspClient>,
}

impl LspManager {
    pub fn new(config: LspConfig, root_uri: String) -> Self {
        Self {
            config,
            root_uri,
            clients: HashMap::new(),
        }
    }

    /// Infer the language for `file_path` and return a mutable client for it,
    /// spawning the server on first use.
    pub async fn client_for(
        &mut self,
        file_path: &Path,
    ) -> SdkResult<&mut ChildLspClient> {
        let language = language_id_for_path(file_path).ok_or_else(|| {
            SdkError::ToolExecution {
                tool_name: "lsp".to_string(),
                message: format!(
                    "Unsupported file extension for LSP: {}",
                    file_path.display()
                ),
            }
        })?;
        self.client_for_language(language).await
    }

    /// Get or spawn the client for a specific language id.
    pub async fn client_for_language(
        &mut self,
        language: &str,
    ) -> SdkResult<&mut ChildLspClient> {
        if !self.clients.contains_key(language) {
            let spec = self.config.server_for(language).ok_or_else(|| {
                SdkError::ToolExecution {
                    tool_name: "lsp".to_string(),
                    message: format!("No LSP server configured for language '{language}'"),
                }
            })?;
            let client = ChildLspClient::spawn(&spec.command, &spec.args, &self.root_uri).await?;
            self.clients.insert(language.to_string(), client);
        }
        Ok(self.clients.get_mut(language).expect("just inserted"))
    }

    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }

    pub fn config(&self) -> &LspConfig {
        &self.config
    }
}

/// Convert a work-dir path into a workspace `rootUri`.
pub fn work_dir_to_root_uri(work_dir: &Path) -> SdkResult<String> {
    path_to_uri(work_dir).ok_or_else(|| SdkError::Config(
        format!("Could not build file:// URI for {}", work_dir.display()),
    ))
}

/// Helper used by tools and tests: build a `file://` URI or a clear error.
pub fn file_uri_for(path: &Path) -> SdkResult<String> {
    path_to_uri(path).ok_or_else(|| SdkError::ToolExecution {
        tool_name: "lsp".to_string(),
        message: format!("Path is not representable as URI: {}", path.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_id_examples() {
        assert_eq!(language_id_for_path(Path::new("a/b/c.rs")), Some("rust"));
        assert_eq!(language_id_for_path(Path::new("x.ts")), Some("typescript"));
        assert_eq!(language_id_for_path(Path::new("x.unknown")), None);
    }

    #[test]
    fn load_missing_manifest_errors() {
        let result = LspConfig::load(Path::new("/definitely/does/not/exist.json"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_manifest_shape() {
        let text = r#"{
            "rust": { "command": "rust-analyzer", "args": [] },
            "typescript": { "command": "typescript-language-server", "args": ["--stdio"] }
        }"#;
        let cfg: LspConfig = serde_json::from_str(text).unwrap();
        assert_eq!(cfg.server_for("rust").unwrap().command, "rust-analyzer");
        assert_eq!(
            cfg.server_for("typescript").unwrap().args,
            vec!["--stdio".to_string()]
        );
        assert!(cfg.server_for("python").is_none());
    }

    #[test]
    fn path_to_uri_basics() {
        let uri = path_to_uri(Path::new("/tmp/foo/bar.rs")).unwrap();
        assert_eq!(uri, "file:///tmp/foo/bar.rs");
    }
}
