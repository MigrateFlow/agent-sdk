//! Stdio transport for MCP (Model Context Protocol).
//!
//! Frames JSON-RPC 2.0 messages as NDJSON (one JSON object per line) over
//! any `AsyncRead + AsyncWrite` pair. Works with spawned child processes
//! (via `tokio::process::Child` stdin/stdout) or with in-process pipes like
//! `tokio::io::duplex` for testing.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use crate::error::{SdkError, SdkResult};

/// A bidirectional NDJSON transport over async reader/writer pair.
///
/// Each outgoing value is serialized to a single line terminated by `\n`.
/// Each incoming value is read as a single line and parsed as JSON.
pub struct StdioTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    reader: Mutex<BufReader<R>>,
    writer: Mutex<W>,
    // Optional child handle for processes we own — kept so that dropping the
    // transport terminates the server. Not needed for in-process transports.
    child: Mutex<Option<Child>>,
}

impl<R, W> StdioTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// Construct a transport from a reader and writer. No child process is
    /// associated — use `from_child` to bind to a spawned process.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: Mutex::new(BufReader::new(reader)),
            writer: Mutex::new(writer),
            child: Mutex::new(None),
        }
    }

    /// Send a single JSON value followed by a newline.
    pub async fn send(&self, value: serde_json::Value) -> SdkResult<()> {
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Read one line and parse it as a JSON value.
    pub async fn recv(&self) -> SdkResult<serde_json::Value> {
        let mut reader = self.reader.lock().await;
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(SdkError::Config(
                "MCP transport closed unexpectedly (EOF)".to_string(),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(line.trim_end_matches(['\r', '\n']))?;
        Ok(value)
    }
}

impl StdioTransport<ChildStdout, ChildStdin> {
    /// Bind a transport to a spawned child process' stdin/stdout.
    ///
    /// Takes ownership of the child so that dropping the transport kills the
    /// process.
    pub fn from_child(mut child: Child) -> SdkResult<Self> {
        let stdout = child.stdout.take().ok_or_else(|| {
            SdkError::Config("MCP child process has no piped stdout".to_string())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            SdkError::Config("MCP child process has no piped stdin".to_string())
        })?;
        Ok(Self {
            reader: Mutex::new(BufReader::new(stdout)),
            writer: Mutex::new(stdin),
            child: Mutex::new(Some(child)),
        })
    }
}

impl<R, W> Drop for StdioTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    fn drop(&mut self) {
        // Best-effort kill of any owned child process. `try_lock` avoids
        // blocking on drop; if the lock is held we leak the child.
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
    }
}
