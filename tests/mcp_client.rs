//! End-to-end test for the MCP client against an in-process mock server.
//!
//! We use `tokio::io::duplex` to avoid spawning a real process. The mock
//! server speaks the minimal JSON-RPC/NDJSON protocol required to exercise
//! `initialize`, `tools/list`, and `tools/call`.

use agent_sdk::mcp::{McpClient, StdioTransport};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Canned mock server. Reads one JSON-RPC request per line and responds.
/// Notifications (no `id`) are consumed silently.
async fn run_mock_server(stream: DuplexStream) {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = req.get("id").cloned();

        // Notifications carry no id — skip without replying.
        if id.is_none() {
            continue;
        }

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "mock", "version": "0.0.0" }
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo back the input text.",
                            "inputSchema": {
                                "type": "object",
                                "properties": { "text": { "type": "string" } },
                                "required": ["text"]
                            }
                        },
                        {
                            "name": "add",
                            "description": "Add two numbers.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "a": { "type": "number" },
                                    "b": { "type": "number" }
                                },
                                "required": ["a", "b"]
                            }
                        }
                    ]
                }
            }),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or(json!({}));
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                let text = match name {
                    "echo" => arguments
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    "add" => {
                        let a = arguments.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let b = arguments.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        format!("{}", a + b)
                    }
                    other => format!("unknown tool: {}", other),
                };
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    }
                })
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            }),
        };

        let mut line = response.to_string();
        line.push('\n');
        if write.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if write.flush().await.is_err() {
            break;
        }
    }
}

#[tokio::test]
async fn mcp_client_handshake_lists_and_calls_tools() {
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(run_mock_server(server_stream));

    let (client_read, client_write) = tokio::io::split(client_stream);
    let transport = StdioTransport::new(client_read, client_write);
    let mut client = McpClient::new(transport, "mock");

    // 1. initialize handshake succeeds + records capabilities
    let init = client.initialize().await.expect("initialize should succeed");
    assert_eq!(init.protocol_version, "2024-11-05");
    assert!(
        client.capabilities().is_some(),
        "capabilities should be recorded after initialize"
    );
    let caps = client.capabilities().unwrap();
    assert!(
        caps.get("tools").is_some(),
        "mock server advertises `tools` capability"
    );

    // 2. tools/list returns exactly the stubbed tool set
    let tools = client.list_tools().await.expect("tools/list should succeed");
    assert_eq!(tools.len(), 2, "expected two stubbed tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"add"));

    // 3. tools/call round-trips arguments and returns content text
    let echo = client
        .call_tool("echo", json!({ "text": "hello, mcp" }))
        .await
        .expect("echo call should succeed");
    assert!(!echo.is_error);
    assert_eq!(echo.content.len(), 1);
    match &echo.content[0] {
        agent_sdk::mcp::McpContentBlock::Text { text } => {
            assert_eq!(text, "hello, mcp");
        }
        _ => panic!("expected text content block"),
    }

    let sum = client
        .call_tool("add", json!({ "a": 2, "b": 3 }))
        .await
        .expect("add call should succeed");
    match &sum.content[0] {
        agent_sdk::mcp::McpContentBlock::Text { text } => {
            assert_eq!(text, "5");
        }
        _ => panic!("expected text content block"),
    }

    drop(client);
    let _ = server_task.await;
}
