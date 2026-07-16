//! MCP stdio transport: newline-delimited JSON-RPC 2.0 framing and
//! request/response correlation, generic over already-open reader/writer
//! halves so it can be driven by either a real child process's
//! stdin/stdout (production) or a `tokio::io::duplex()` pair (tests) with
//! no subprocess involved. Mirrors `crate::lsp::transport::Connection`'s
//! shape (pending-map correlation, background read loop, `is_dead`
//! liveness, `Drop`-based cleanup) — MCP's wire framing differs from LSP's
//! `Content-Length` header block: one complete JSON-RPC message per line,
//! newline-terminated, no header.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

use crate::ToolError;

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

pub(crate) struct McpConnection {
    writer: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    pending: PendingMap,
    next_id: AtomicI64,
    reader_task: tokio::task::JoinHandle<()>,
}

impl McpConnection {
    pub(crate) fn new(
        reader: impl AsyncRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
    ) -> Self {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_task = pending.clone();
        let reader_task = tokio::spawn(read_loop(reader, pending_for_task));
        Self {
            writer: Mutex::new(Box::new(writer)),
            pending,
            next_id: AtomicI64::new(1),
            reader_task,
        }
    }

    /// True once the background reader task has exited — the read side
    /// only exits on EOF (child died / pipe closed) or an unrecoverable
    /// framing error, so this is the liveness signal callers use to decide
    /// whether to respawn.
    pub(crate) fn is_dead(&self) -> bool {
        self.reader_task.is_finished()
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(err) = write_framed(&mut *self.writer.lock().await, &message).await {
            self.pending.lock().await.remove(&id);
            return Err(ToolError::ExecutionFailed(format!(
                "failed to write MCP request: {err}"
            )));
        }

        match rx.await {
            // `value` is the full raw JSON-RPC response envelope (see
            // `read_loop`), not just its `result` field — so an
            // error-shaped response can be told apart from a genuine
            // (possibly empty/null) success result instead of both
            // collapsing to `Value::Null`.
            Ok(value) => match value.get("error") {
                Some(error) => {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("<no message>");
                    let code = error.get("code").and_then(Value::as_i64);
                    let described = match code {
                        Some(code) => format!("MCP server returned an error (code {code}): {message}"),
                        None => format!("MCP server returned an error: {message}"),
                    };
                    Err(ToolError::ExecutionFailed(described))
                }
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            },
            Err(_) => Err(ToolError::ExecutionFailed(
                "MCP server closed the connection before responding".to_string(),
            )),
        }
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<(), ToolError> {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        write_framed(&mut *self.writer.lock().await, &message)
            .await
            .map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to write MCP notification: {err}"))
            })
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

async fn write_framed(
    writer: &mut (impl AsyncWrite + Unpin + ?Sized),
    message: &Value,
) -> std::io::Result<()> {
    let mut body = serde_json::to_vec(message).expect("Value always serializes");
    body.push(b'\n');
    writer.write_all(&body).await?;
    writer.flush().await
}

async fn read_loop(reader: impl AsyncRead + Unpin, pending: PendingMap) {
    let mut reader = BufReader::new(reader);
    loop {
        match read_one_message(&mut reader).await {
            Ok(Some(value)) => {
                // Only a genuine *response* (has "result" or "error", no
                // "method") resolves a pending sender. A server
                // notification (has "method", no "id") — e.g.
                // `notifications/message` or
                // `notifications/resources/list_changed` — is
                // intentionally left unanswered: this project re-lists
                // resources/prompts on demand rather than subscribing to
                // change notifications (see the design's Non-Goals).
                let is_response = value.get("method").is_none()
                    && (value.get("result").is_some() || value.get("error").is_some());
                if is_response
                    && let Some(id) = value.get("id").and_then(Value::as_i64)
                    && let Some(tx) = pending.lock().await.remove(&id)
                {
                    // Send the whole raw envelope (not just `result`) so
                    // `request()` can distinguish a genuine `"error"`
                    // response from a successful-but-empty `"result"` —
                    // both used to collapse to `Value::Null` here.
                    let _ = tx.send(value);
                }
            }
            Ok(None) => break, // EOF: child exited or pipe closed
            Err(_) => break,   // malformed framing: treat as dead, matching EOF
        }
    }
}

pub(crate) async fn read_one_message<R>(reader: &mut R) -> std::io::Result<Option<Value>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // EOF before a full line
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Tolerate a stray blank line between messages rather than
            // failing the whole connection over cosmetic whitespace.
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        return Ok(Some(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write_raw_line(writer: &mut (impl AsyncWrite + Unpin), message: Value) {
        let mut body = serde_json::to_vec(&message).unwrap();
        body.push(b'\n');
        writer.write_all(&body).await.unwrap();
        writer.flush().await.unwrap();
    }

    #[tokio::test]
    async fn request_resolves_when_a_matching_response_arrives() {
        let (client_reader, mut server_writer) = tokio::io::duplex(4096);
        let (server_reader, client_writer) = tokio::io::duplex(4096);
        let connection = McpConnection::new(client_reader, client_writer);

        let drain_task = tokio::spawn(async move {
            let mut server_reader = server_reader;
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut server_reader, &mut buf).await;
        });

        let request_fut = connection.request("tools/list", serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        write_raw_line(
            &mut server_writer,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}),
        )
        .await;

        let result = request_fut.await.unwrap();
        assert_eq!(result, serde_json::json!({"tools": []}));
        drain_task.await.unwrap();
    }

    #[tokio::test]
    async fn request_resolves_to_an_error_when_the_response_has_an_error_field() {
        // Regression test for the final-review finding: a JSON-RPC error
        // response used to be silently discarded and resolved as
        // `Ok(Value::Null)`, indistinguishable from a genuine empty
        // success. It must now surface as an `Err` carrying the server's
        // error message.
        let (client_reader, mut server_writer) = tokio::io::duplex(4096);
        let (server_reader, client_writer) = tokio::io::duplex(4096);
        let connection = McpConnection::new(client_reader, client_writer);

        let drain_task = tokio::spawn(async move {
            let mut server_reader = server_reader;
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut server_reader, &mut buf).await;
        });

        let request_fut = connection.request("tools/call", serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        write_raw_line(
            &mut server_writer,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32601, "message": "Method not found"}
            }),
        )
        .await;

        let err = request_fut.await.expect_err("expected an Err, got Ok");
        let message = err.to_string();
        assert!(
            message.contains("Method not found"),
            "error text missing the server's message: {message}"
        );
        drain_task.await.unwrap();
    }

    #[tokio::test]
    async fn is_dead_becomes_true_once_the_reader_hits_eof() {
        let (client_reader, server_writer) = tokio::io::duplex(4096);
        let (_server_reader, client_writer) = tokio::io::duplex(4096);
        let connection = McpConnection::new(client_reader, client_writer);

        assert!(!connection.is_dead());
        drop(server_writer);

        for _ in 0..50 {
            if connection.is_dead() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("connection did not report dead after EOF within 500ms");
    }

    #[tokio::test]
    async fn notify_writes_a_well_framed_message_with_no_id() {
        let (_client_reader, mut server_writer_unused) = tokio::io::duplex(4096);
        let (server_reader, client_writer) = tokio::io::duplex(4096);
        let (client_reader, _) = tokio::io::duplex(4096);
        let connection = McpConnection::new(client_reader, client_writer);
        let _ = &mut server_writer_unused;

        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            read_one_message(&mut reader).await.unwrap().unwrap()
        });

        connection
            .notify(
                "notifications/initialized",
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let received = read_task.await.unwrap();
        assert_eq!(received["method"], "notifications/initialized");
        assert!(received.get("id").is_none());
    }
}
