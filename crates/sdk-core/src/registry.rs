use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{SdkError, SdkResult};
use crate::traits::tool::{Tool, ToolDefinition};

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let def = tool.definition();
        self.tools.insert(def.name.clone(), tool);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> SdkResult<serde_json::Value> {
        let tool = self.tools.get(name).ok_or_else(|| SdkError::ToolExecution {
            tool_name: name.to_string(),
            message: format!("Tool '{}' not found in registry", name),
        })?;

        tool.execute(arguments).await
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Look up a tool by name, returning a reference to the trait object.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct EchoTool {
        name: String,
    }

    #[async_trait]
    impl Tool for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.clone(),
                description: format!("echo {}", self.name),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            arguments: serde_json::Value,
        ) -> SdkResult<serde_json::Value> {
            Ok(serde_json::json!({"tool": self.name, "echo": arguments}))
        }
    }

    #[test]
    fn new_and_default_produce_empty_registry() {
        let reg = ToolRegistry::new();
        assert!(reg.is_empty());
        assert!(reg.definitions().is_empty());
        assert!(reg.tool_names().is_empty());

        let def = ToolRegistry::default();
        assert!(def.is_empty());
    }

    #[test]
    fn register_and_list_definitions() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool { name: "alpha".into() }));
        reg.register(Arc::new(EchoTool { name: "beta".into() }));
        assert!(!reg.is_empty());

        let mut names: Vec<_> = reg.tool_names().into_iter().collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);

        let mut def_names: Vec<_> = reg.definitions().into_iter().map(|d| d.name).collect();
        def_names.sort();
        assert_eq!(def_names, vec!["alpha", "beta"]);
    }

    #[test]
    fn registering_same_name_replaces_existing_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool { name: "x".into() }));
        reg.register(Arc::new(EchoTool { name: "x".into() }));
        assert_eq!(reg.tool_names().len(), 1);
    }

    #[tokio::test]
    async fn execute_runs_registered_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool { name: "echo".into() }));
        let result = reg
            .execute("echo", serde_json::json!({"k": "v"}))
            .await
            .unwrap();
        assert_eq!(result["tool"], "echo");
        assert_eq!(result["echo"]["k"], "v");
    }

    #[tokio::test]
    async fn execute_missing_tool_returns_tool_execution_error() {
        let reg = ToolRegistry::new();
        let err = reg
            .execute("ghost", serde_json::json!({}))
            .await
            .unwrap_err();
        match err {
            SdkError::ToolExecution { tool_name, message } => {
                assert_eq!(tool_name, "ghost");
                assert!(message.contains("not found"));
            }
            other => panic!("expected ToolExecution error, got {other:?}"),
        }
    }
}