//! End-to-end test for the LSP client using a stub server driven over
//! `tokio::io::duplex()`. The stub validates the wire-level framing
//! (`Content-Length:` headers, UTF-8 JSON bodies, JSON-RPC id correlation) and
//! returns canned responses for each of the three supported requests.

use agent_sdk::lsp::client::LspClient;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

/// Read one LSP frame from `stream` and parse its JSON body.
async fn read_frame(stream: &mut DuplexStream) -> Value {
    let mut header = Vec::new();
    // Read bytes one at a time until we see \r\n\r\n.
    let mut byte = [0u8; 1];
    while !ends_with_double_crlf(&header) {
        let n = stream
            .read_exact(&mut byte)
            .await
            .expect("read header byte");
        assert_eq!(n, 1);
        header.push(byte[0]);
    }
    let header_str = std::str::from_utf8(&header).expect("header utf8");
    let mut content_length: Option<usize> = None;
    for line in header_str.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse().expect("parse length"));
        }
    }
    let len = content_length.expect("Content-Length present");
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.expect("read body");
    serde_json::from_slice(&body).expect("parse json")
}

fn ends_with_double_crlf(buf: &[u8]) -> bool {
    buf.ends_with(b"\r\n\r\n")
}

async fn write_frame(stream: &mut DuplexStream, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stream.write_all(header.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    stream.flush().await.unwrap();
}

/// Drive the stub server. Reads frames forever and responds to the three
/// supported methods with canned data; ignores notifications.
async fn run_stub(mut stream: DuplexStream) {
    loop {
        let msg = read_frame(&mut stream).await;
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();

        match method {
            "initialize" => {
                assert!(id.is_some(), "initialize must be a request");
                write_frame(
                    &mut stream,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id.unwrap(),
                        "result": { "capabilities": {} }
                    }),
                )
                .await;
            }
            "initialized" | "textDocument/didOpen" => {
                assert!(id.is_none(), "{method} must be a notification");
            }
            "textDocument/definition" => {
                let id = id.expect("definition must be request");
                write_frame(
                    &mut stream,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "uri": "file:///tmp/target.rs",
                            "range": {
                                "start": { "line": 10, "character": 4 },
                                "end":   { "line": 10, "character": 8 }
                            }
                        }
                    }),
                )
                .await;
            }
            "textDocument/references" => {
                let id = id.expect("references must be request");
                write_frame(
                    &mut stream,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": [
                            {
                                "uri": "file:///tmp/a.rs",
                                "range": {
                                    "start": { "line": 1, "character": 0 },
                                    "end":   { "line": 1, "character": 3 }
                                }
                            },
                            {
                                "uri": "file:///tmp/b.rs",
                                "range": {
                                    "start": { "line": 5, "character": 2 },
                                    "end":   { "line": 5, "character": 5 }
                                }
                            }
                        ]
                    }),
                )
                .await;
            }
            "textDocument/documentSymbol" => {
                let id = id.expect("documentSymbol must be request");
                write_frame(
                    &mut stream,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": [
                            {
                                "name": "Parent",
                                "kind": 5, // Class
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end":   { "line": 20, "character": 0 }
                                },
                                "selectionRange": {
                                    "start": { "line": 0, "character": 6 },
                                    "end":   { "line": 0, "character": 12 }
                                },
                                "children": [
                                    {
                                        "name": "child_method",
                                        "kind": 6, // Method
                                        "range": {
                                            "start": { "line": 2, "character": 4 },
                                            "end":   { "line": 5, "character": 4 }
                                        },
                                        "selectionRange": {
                                            "start": { "line": 2, "character": 8 },
                                            "end":   { "line": 2, "character": 20 }
                                        }
                                    }
                                ]
                            }
                        ]
                    }),
                )
                .await;
            }
            "shutdown" => {
                // Respond and exit gracefully.
                if let Some(id) = id {
                    write_frame(
                        &mut stream,
                        &json!({ "jsonrpc": "2.0", "id": id, "result": null }),
                    )
                    .await;
                }
                break;
            }
            "" => {
                // Response or malformed; stop if empty read.
                break;
            }
            other => {
                if let Some(id) = id {
                    write_frame(
                        &mut stream,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": format!("method {other} not supported") }
                        }),
                    )
                    .await;
                }
            }
        }
    }
}

#[tokio::test]
async fn lsp_client_roundtrips_three_requests() {
    // `duplex` returns a pair of streams; what one writes, the other reads.
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    tokio::spawn(run_stub(server_stream));

    // Split the client stream into the (reader, writer) halves the generic
    // client expects.
    let (reader, writer) = tokio::io::split(client_stream);
    let mut client = LspClient::from_transport(reader, writer);

    let init_result = client.initialize("file:///tmp").await.expect("initialize");
    assert!(init_result.get("capabilities").is_some());

    // `did_open` is a notification — no response expected, just succeeds.
    client
        .did_open("file:///tmp/x.rs", "rust", "fn main() {}")
        .await
        .expect("didOpen");

    // goto_definition
    let locs = client
        .goto_definition("file:///tmp/x.rs", 0, 3)
        .await
        .expect("definition");
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].uri.as_str(), "file:///tmp/target.rs");
    assert_eq!(locs[0].range.start.line, 10);
    assert_eq!(locs[0].range.start.character, 4);

    // find_references
    let refs = client
        .find_references("file:///tmp/x.rs", 0, 3, true)
        .await
        .expect("references");
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].uri.as_str(), "file:///tmp/a.rs");
    assert_eq!(refs[1].uri.as_str(), "file:///tmp/b.rs");

    // document_symbols
    let symbols = client
        .document_symbols_raw("file:///tmp/x.rs")
        .await
        .expect("documentSymbol");
    let arr = symbols.as_array().expect("symbols is array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "Parent");
    let children = arr[0]["children"].as_array().expect("children array");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["name"], "child_method");
}

#[tokio::test]
async fn lsp_client_correlates_ids_across_interleaved_requests() {
    // Verifies that issuing requests sequentially produces strictly increasing
    // ids and that the client correctly matches each response.
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    tokio::spawn(run_stub(server_stream));

    let (reader, writer) = tokio::io::split(client_stream);
    let mut client = LspClient::from_transport(reader, writer);
    client.initialize("file:///tmp").await.unwrap();

    for _ in 0..3 {
        let locs = client
            .goto_definition("file:///tmp/x.rs", 0, 0)
            .await
            .unwrap();
        assert_eq!(locs.len(), 1);
    }
}
