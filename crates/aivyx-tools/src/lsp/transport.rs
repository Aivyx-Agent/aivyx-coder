//! JSON-RPC-over-stdio framing and request/response correlation, generic
//! over already-open reader/writer halves so it can be driven by either a
//! real child process's stdin/stdout (production) or a `tokio::io::duplex()`
//! pair (tests) with no subprocess involved.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

use crate::ToolError;

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

pub(crate) struct Connection {
    writer: Mutex<Box<dyn AsyncWrite + Unpin + Send>>,
    pending: PendingMap,
    next_id: AtomicI64,
    reader_task: tokio::task::JoinHandle<()>,
}

impl Connection {
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
                "failed to write LSP request: {err}"
            )));
        }

        match rx.await {
            Ok(value) => Ok(value),
            Err(_) => Err(ToolError::ExecutionFailed(
                "LSP server closed the connection before responding".to_string(),
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
                ToolError::ExecutionFailed(format!("failed to write LSP notification: {err}"))
            })
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

async fn write_framed(
    writer: &mut (impl AsyncWrite + Unpin + ?Sized),
    message: &Value,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(message).expect("Value always serializes");
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

async fn read_loop(reader: impl AsyncRead + Unpin, pending: PendingMap) {
    let mut reader = BufReader::new(reader);
    loop {
        match read_one_message(&mut reader).await {
            Ok(Some(value)) => {
                if let Some(id) = value.get("id").and_then(Value::as_i64)
                    && let Some(tx) = pending.lock().await.remove(&id)
                {
                    let payload = value.get("result").cloned().unwrap_or(Value::Null);
                    let _ = tx.send(payload);
                }
                // Notifications from the server (no "id") are dropped —
                // this project only issues requests it awaits; no
                // server-initiated notification matters for
                // go_to_definition/find_references.
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
    use tokio::io::AsyncBufReadExt;

    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // EOF before a full header block
        }
        let text = line.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            break; // blank line ends the header block
        }
        if let Some(value) = text.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = content_length else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "LSP message missing Content-Length header",
        ));
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt as _;

    /// Writes one framed JSON-RPC message directly to a raw writer — the
    /// same wire format `Connection` itself produces, used here to drive
    /// the read side independently of `Connection::request`/`notify`.
    async fn write_raw_frame(writer: &mut (impl AsyncWrite + Unpin), message: Value) {
        let body = serde_json::to_vec(&message).unwrap();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        writer.write_all(header.as_bytes()).await.unwrap();
        writer.write_all(&body).await.unwrap();
        writer.flush().await.unwrap();
    }

    #[tokio::test]
    async fn request_resolves_when_a_matching_response_arrives() {
        let (client_reader, mut server_writer) = tokio::io::duplex(4096);
        let (server_reader, client_writer) = tokio::io::duplex(4096);
        let connection = Connection::new(client_reader, client_writer);

        // Drain what the connection wrote (the request) so the duplex
        // buffer doesn't fill, then reply with a canned response.
        let drain_task = tokio::spawn(async move {
            let mut server_reader = server_reader;
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut server_reader, &mut buf).await;
        });

        let request_fut = connection.request("textDocument/definition", serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        write_raw_frame(
            &mut server_writer,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}),
        )
        .await;

        let result = request_fut.await.unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
        drain_task.await.unwrap();
    }

    #[tokio::test]
    async fn is_dead_becomes_true_once_the_reader_hits_eof() {
        let (client_reader, server_writer) = tokio::io::duplex(4096);
        let (_server_reader, client_writer) = tokio::io::duplex(4096);
        let connection = Connection::new(client_reader, client_writer);

        assert!(!connection.is_dead());
        drop(server_writer); // closes the write half -> reader sees EOF

        // The reader task's exit is async; poll briefly rather than assert
        // instantly, since `is_finished()` only flips after the task is
        // actually scheduled and observes EOF.
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
        let connection = Connection::new(client_reader, client_writer);
        let _ = &mut server_writer_unused; // keep the pair alive

        let read_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_reader);
            read_one_message(&mut reader).await.unwrap().unwrap()
        });

        connection
            .notify("textDocument/didOpen", serde_json::json!({"uri": "file:///a.rs"}))
            .await
            .unwrap();

        let received = read_task.await.unwrap();
        assert_eq!(received["method"], "textDocument/didOpen");
        assert!(received.get("id").is_none());
        assert_eq!(received["params"]["uri"], "file:///a.rs");
    }
}
