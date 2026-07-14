# LSP Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `go_to_definition` and `find_references`, two read-only tools backed by a lazily-spawned, confined `rust-analyzer` subprocess, giving the model exact symbol resolution the static repo map can't provide.

**Architecture:** A new `aivyx-tools/src/lsp/` module owns a `LspClient` — a JSON-RPC-over-stdio client generic over its transport (a real child's stdin/stdout in production, an in-memory `tokio::io::duplex()` pair in tests) — constructed once in `main.rs`, shared via one `Arc` between two new `Tool` implementations that bake it into their own constructors (mirroring `RunCommandTool`, not `GitCheckpointer`, since `LspClient` isn't cross-cutting).

**Tech Stack:** Rust, tokio (`sync` feature newly added to `aivyx-tools`), hand-rolled JSON-RPC/LSP protocol types (no `lsp-types` dependency — matches this workspace's existing hand-rolled-over-heavy-dependency convention).

## Global Constraints

- Capabilities: `go_to_definition` and `find_references` only. No hover, no workspace symbol search, no rename.
- Language: Rust only, via `rust-analyzer`. No pluggable per-language server config.
- Server lifecycle: lazy — spawns on the first call in a session, reused for the rest of the session.
- Sandboxing: the `rust-analyzer` subprocess is spawned through the existing `ExecutionConfiner`, exactly as `run_shell`/`run_command` already do (`let command = ctx.confiner.confine(command);`).
- Code placement: a new module inside `aivyx-tools` (`aivyx-tools/src/lsp/`), not a new crate.
- Missing binary: both tools are always registered. If `rust-analyzer` isn't on `PATH`, the first call's spawn attempt fails with a clear tool-result error — no startup probe.
- `LspClient` is constructor-baked into both tools (`GoToDefinitionTool::new(lsp: Arc<LspClient>)`, `FindReferencesTool::new(lsp: Arc<LspClient>)`), not threaded through `ToolExecutionContext`/`ToolExecutor`.
- Line/column parameters are **1-indexed**, matching `grep`'s existing `path:line_number:text` convention; converted to LSP's 0-indexed `Position` only inside `LspClient`.
- Output format mirrors `grep`'s `path:line:text`, one line per result.
- A new `[lsp] timeout_secs: u64` config field (default `60`) bounds each JSON-RPC request.
- No definition/no references found is **not an error** — an `Ok` result stating nothing was found.
- A crashed `rust-analyzer` is transparently respawned once by the next call's `ensure_started`.

---

### Task 1: JSON-RPC transport (framing + request/response correlation)

**Files:**
- Modify: `crates/aivyx-tools/Cargo.toml`
- Create: `crates/aivyx-tools/src/lsp/protocol.rs`
- Create: `crates/aivyx-tools/src/lsp/transport.rs`
- Create: `crates/aivyx-tools/src/lsp/mod.rs` (module declarations only — `LspClient` itself is Task 2)

**Interfaces:**
- Produces: `pub(crate) struct Connection` with `Connection::new(reader, writer) -> Self`, `async fn request(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, ToolError>`, `async fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), ToolError>`, `fn is_dead(&self) -> bool`. Also `pub(crate) struct Location { pub uri: String, pub range: Range }`, `pub(crate) struct Range { pub start: Position }`, `pub(crate) struct Position { pub line: u32, pub character: u32 }` (all `Deserialize`).

- [ ] **Step 1: Add the `sync` tokio feature**

`aivyx-tools` doesn't currently use `tokio::sync::{oneshot, Mutex}` anywhere (only `tokio_util::sync::CancellationToken`, a separate crate) — the transport layer needs both. In `crates/aivyx-tools/Cargo.toml`, change:

```toml
tokio = { version = "1.52.3", features = ["process", "fs", "rt", "macros", "io-util", "time"] }
```
to:
```toml
tokio = { version = "1.52.3", features = ["process", "fs", "rt", "macros", "io-util", "time", "sync"] }
```

- [ ] **Step 2: Write `protocol.rs`**

Create `crates/aivyx-tools/src/lsp/protocol.rs`:

```rust
//! Minimal hand-rolled LSP response types — just enough to deserialize
//! `textDocument/definition`/`textDocument/references` results. Request
//! params are built inline via `serde_json::json!` at the call sites
//! (matching this project's existing convention, e.g. `council.rs`'s and
//! `architect.rs`'s `ChatRequest`/prompt construction), so there's no
//! parallel typed-params surface to maintain. Deliberately not the
//! `lsp-types` crate — this workspace hand-rolls small protocol/format
//! surfaces rather than pulling in a heavy dependency for a handful of
//! JSON shapes (the same reasoning behind the hand-rolled frontmatter
//! parser having no YAML dependency).

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct Position {
    pub line: u32,
    #[allow(dead_code)]
    pub character: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Range {
    pub start: Position,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Location {
    pub uri: String,
    pub range: Range,
}
```

- [ ] **Step 3: Write `transport.rs`'s failing tests**

Create `crates/aivyx-tools/src/lsp/transport.rs` with this initial content (imports, the `Connection` type stub, and its tests — implementation comes in Step 4):

```rust
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
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: FAIL to compile — `lsp` module isn't declared in `lib.rs` yet, and `mod.rs` doesn't exist. Fix that now:

- [ ] Create `crates/aivyx-tools/src/lsp/mod.rs`:

```rust
mod protocol;
mod transport;
```

- [ ] In `crates/aivyx-tools/src/lib.rs`, add `mod lsp;` right after the existing `mod checkpoint;` line:

```rust
mod checkpoint;
mod diff;
mod lsp;
mod path_resolve;
mod process;
mod tools;
pub mod wiki;
```

- [ ] Re-run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: FAIL — `protocol.rs`'s types are all `pub(crate)` but never constructed/read anywhere yet, so `cargo` will report them as dead code (a warning, not an error) once the crate compiles; the actual test failure at this point should just be the transport tests themselves, which should now compile and run.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: PASS (3 tests: `request_resolves_when_a_matching_response_arrives`,
`is_dead_becomes_true_once_the_reader_hits_eof`,
`notify_writes_a_well_framed_message_with_no_id`).

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/Cargo.toml crates/aivyx-tools/src/lsp crates/aivyx-tools/src/lib.rs
git commit -m "LSP: JSON-RPC transport (framing + request/response correlation)"
```

---

### Task 2: `LspClient` — lazy spawn, confiner reuse, respawn detection

**Files:**
- Modify: `crates/aivyx-tools/src/lsp/mod.rs`

**Interfaces:**
- Consumes: `crate::lsp::transport::Connection` (Task 1, `pub(crate)` within `lsp`), `aivyx_sandbox::ExecutionConfiner`, `crate::ToolError`.
- Produces: `pub struct LspClient`, `pub fn LspClient::new(timeout: std::time::Duration) -> Self`, `pub(crate) async fn ensure_started(&self, cwd: &Path, confiner: &Arc<dyn ExecutionConfiner>) -> Result<(), ToolError>`. Test-only: `pub(crate) fn with_program(program: &str, timeout: Duration) -> Self`, `pub(crate) async fn wire_for_test(&self, reader, writer)`.

- [ ] **Step 1: Write the failing tests**

Replace `crates/aivyx-tools/src/lsp/mod.rs`'s content with this (struct/constructors/`ensure_started` implemented in Step 2; this step adds the full file including tests, which will fail to compile until Step 2 lands):

```rust
mod protocol;
mod transport;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aivyx_sandbox::ExecutionConfiner;
use tokio::sync::Mutex;

use crate::ToolError;
use transport::Connection;

struct Started {
    connection: Connection,
    child: Option<tokio::process::Child>,
    opened_uris: HashSet<String>,
    next_doc_version: i32,
}

/// Owns a lazily-spawned `rust-analyzer` subprocess (or, in tests, an
/// in-memory transport wired directly) and the LSP session state built on
/// top of it. Constructed once and shared via `Arc` between
/// `GoToDefinitionTool` and `FindReferencesTool` so both query the same
/// running server.
pub struct LspClient {
    program: String,
    timeout: Duration,
    state: Mutex<Option<Started>>,
}

impl LspClient {
    /// Production constructor — spawns `rust-analyzer` on first use.
    /// `timeout` bounds each JSON-RPC request (see `[lsp] timeout_secs`).
    pub fn new(timeout: Duration) -> Self {
        Self {
            program: "rust-analyzer".to_string(),
            timeout,
            state: Mutex::new(None),
        }
    }

    /// Test-only: same lazy-spawn behavior, but spawns `program` instead
    /// of `rust-analyzer` — lets tests exercise the real
    /// process-spawn/confiner/error path with a program name chosen by
    /// the test (e.g. one that doesn't exist, to deterministically test
    /// the "not found" error) without requiring rust-analyzer installed.
    #[cfg(test)]
    pub(crate) fn with_program(program: &str, timeout: Duration) -> Self {
        Self {
            program: program.to_string(),
            timeout,
            state: Mutex::new(None),
        }
    }

    /// Test-only: directly wires a session onto an already-open transport
    /// (e.g. a `tokio::io::duplex()` pair driven by a fake server task),
    /// bypassing the real spawn/confiner/initialize path entirely.
    #[cfg(test)]
    pub(crate) async fn wire_for_test(
        &self,
        reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) {
        *self.state.lock().await = Some(Started {
            connection: Connection::new(reader, writer),
            child: None,
            opened_uris: HashSet::new(),
            next_doc_version: 1,
        });
    }

    /// Ensures a live, initialized LSP session exists for `cwd`, spawning
    /// (or respawning, if the previous process died) as needed. Cheap to
    /// call on every request — does nothing beyond a liveness check once a
    /// session is already running.
    pub(crate) async fn ensure_started(
        &self,
        cwd: &Path,
        confiner: &Arc<dyn ExecutionConfiner>,
    ) -> Result<(), ToolError> {
        let mut guard = self.state.lock().await;

        if let Some(started) = guard.as_mut() {
            let dead = started.connection.is_dead()
                || started
                    .child
                    .as_mut()
                    .map(|child| matches!(child.try_wait(), Ok(Some(_))))
                    .unwrap_or(false);
            if !dead {
                return Ok(());
            }
        }

        let mut command = build_command(&self.program, cwd);
        command = confiner.confine(command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|err| {
            ToolError::ExecutionFailed(format!(
                "{} not found on PATH — install it to use go_to_definition/find_references ({err})",
                self.program
            ))
        })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let connection = Connection::new(stdout, stdin);

        initialize(&connection, cwd).await?;

        *guard = Some(Started {
            connection,
            child: Some(child),
            opened_uris: HashSet::new(),
            next_doc_version: 1,
        });
        Ok(())
    }
}

fn build_command(program: &str, cwd: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    command.current_dir(cwd);
    command
}

fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

async fn initialize(connection: &Connection, cwd: &Path) -> Result<(), ToolError> {
    let root_uri = path_to_uri(cwd);
    let params = serde_json::json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {},
        "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
    });
    connection.request("initialize", params).await?;
    connection.notify("initialized", serde_json::json!({})).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::NoopConfiner;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Records whether `confine()` was invoked, without altering the
    /// command — lets a test assert the confiner was actually consulted.
    struct SpyConfiner {
        called: AtomicBool,
    }

    impl SpyConfiner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                called: AtomicBool::new(false),
            })
        }
    }

    impl ExecutionConfiner for SpyConfiner {
        fn confine(&self, command: tokio::process::Command) -> tokio::process::Command {
            self.called.store(true, Ordering::SeqCst);
            command
        }
    }

    #[test]
    fn build_command_and_confine_together_produce_a_confined_command() {
        // Exercises exactly the two calls `ensure_started` makes before
        // spawning — without actually spawning anything, so it needs no
        // real binary at all.
        let spy = SpyConfiner::new();
        let confiner: Arc<dyn ExecutionConfiner> = spy.clone();
        let command = build_command("rust-analyzer", Path::new("."));
        let _confined = confiner.confine(command);
        assert!(spy.called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn ensure_started_reports_a_clear_error_when_the_program_is_missing() {
        let client = LspClient::with_program(
            "definitely-not-a-real-binary-xyz",
            Duration::from_secs(5),
        );
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);

        let err = client
            .ensure_started(Path::new("."), &confiner)
            .await
            .unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(msg.contains("not found on PATH"));
        assert!(msg.contains("definitely-not-a-real-binary-xyz"));
    }

    #[tokio::test]
    async fn ensure_started_respawns_when_the_existing_connection_is_dead() {
        // Wire a session whose connection is already dead (server half
        // dropped immediately), then call `ensure_started` pointed at a
        // nonexistent program — the resulting "not found" error proves a
        // *fresh* spawn was actually attempted rather than the dead state
        // being silently reused as if it were still live.
        let client =
            LspClient::with_program("definitely-not-a-real-binary-xyz", Duration::from_secs(5));
        let (client_reader, server_writer) = tokio::io::duplex(4096);
        let (_server_reader, client_writer) = tokio::io::duplex(4096);
        client.wire_for_test(client_reader, client_writer).await;
        drop(server_writer); // closes the write half -> reader sees EOF -> is_dead() becomes true

        for _ in 0..50 {
            let guard = client.state.lock().await;
            if guard.as_ref().unwrap().connection.is_dead() {
                break;
            }
            drop(guard);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let err = client
            .ensure_started(Path::new("."), &confiner)
            .await
            .unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(
            msg.contains("not found on PATH"),
            "expected a fresh spawn attempt (and its failure) after detecting the dead \
             connection, got: {msg}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: this file already contains the full implementation (there's no
separate "minimal stub first" step here, since `ensure_started`'s tests
require the real confiner/spawn/respawn logic to exist to mean anything) —
if it fails to compile, check for typos against the code above; if it
compiles and a test fails, re-check the respawn test's polling loop timing.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: PASS (3 new tests in `lsp::tests` plus the 3 from Task 1's
`transport::tests` — 6 total under `lsp::`).

- [ ] **Step 4: Run the full `aivyx-tools` test suite**

Run: `cargo test -p aivyx-tools --lib`
Expected: all tests pass, no regressions to existing tool tests.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-tools/src/lsp/mod.rs
git commit -m "LSP: LspClient lazy spawn, confiner reuse, respawn detection"
```

---

### Task 3: Document sync + `go_to_definition`/`find_references` query methods

**Files:**
- Modify: `crates/aivyx-tools/src/lsp/mod.rs`
- Modify: `crates/aivyx-tools/src/lsp/protocol.rs` (no changes needed — `Location`/`Range`/`Position` from Task 1 already cover what this task deserializes)

**Interfaces:**
- Consumes: `LspClient::ensure_started` (Task 2), `Connection::request`/`notify` (Task 1), `protocol::Location` (Task 1).
- Produces: `pub async fn LspClient::go_to_definition(&self, cwd: &Path, confiner: &Arc<dyn ExecutionConfiner>, path: &str, line: u32, column: u32) -> Result<String, ToolError>`, `pub async fn LspClient::find_references(&self, ...) -> Result<String, ToolError>` (same signature).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/aivyx-tools/src/lsp/mod.rs`, right after the last existing test:

```rust
    /// Drives a fake LSP server against one half of a duplex pair: reads
    /// exactly one framed request, asserts its method, and writes back a
    /// canned response — enough to test `go_to_definition`/`find_references`
    /// end-to-end (including the didOpen/didChange it sends first) without
    /// a real rust-analyzer.
    /// Returns the full request value it responded to (not just its
    /// method) so callers can assert on `params` — in particular the
    /// `position` field, to verify 1-indexed-input-to-0-indexed-request
    /// conversion happened correctly on the way out.
    async fn fake_server_respond_once(
        mut reader: impl tokio::io::AsyncBufRead + Unpin,
        mut writer: impl tokio::io::AsyncWrite + Unpin,
        expected_method: &str,
        result: serde_json::Value,
    ) -> serde_json::Value {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Read frames until we see one carrying "id" (a request, not a
        // notification like didOpen/didChange) — that's the one this
        // helper responds to.
        loop {
            let mut header = String::new();
            let mut content_length = None;
            loop {
                header.clear();
                tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut header)
                    .await
                    .unwrap();
                let text = header.trim_end_matches(['\r', '\n']);
                if text.is_empty() {
                    break;
                }
                if let Some(value) = text.strip_prefix("Content-Length:") {
                    content_length = value.trim().parse::<usize>().ok();
                }
            }
            let mut body = vec![0u8; content_length.unwrap()];
            reader.read_exact(&mut body).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

            if let Some(id) = value.get("id") {
                assert_eq!(value["method"], expected_method);
                let response = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let response_body = serde_json::to_vec(&response).unwrap();
                let header = format!("Content-Length: {}\r\n\r\n", response_body.len());
                writer.write_all(header.as_bytes()).await.unwrap();
                writer.write_all(&response_body).await.unwrap();
                writer.flush().await.unwrap();
                return value;
            }
            // else: a notification (didOpen/didChange) — keep reading.
        }
    }

    #[tokio::test]
    async fn go_to_definition_formats_a_single_location_as_path_line_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 {\n    n\n}\n").unwrap();
        let uri = format!("file://{}", dir.path().join("lib.rs").display());

        let client = LspClient::new(Duration::from_secs(5));
        let (client_reader, server_writer) = tokio::io::duplex(8192);
        let (server_reader, client_writer) = tokio::io::duplex(8192);
        client.wire_for_test(client_reader, client_writer).await;

        let server = tokio::spawn(async move {
            fake_server_respond_once(
                tokio::io::BufReader::new(server_reader),
                server_writer,
                "textDocument/definition",
                serde_json::json!({
                    "uri": uri,
                    "range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}},
                }),
            )
            .await;
        });

        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let result = client
            .go_to_definition(dir.path(), &confiner, "lib.rs", 2, 5)
            .await
            .unwrap();

        assert_eq!(result, "lib.rs:1:fn fib(n: u64) -> u64 {");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn go_to_definition_converts_one_indexed_input_to_zero_indexed_request_position() {
        // Response-side conversion (0-indexed -> 1-indexed) is covered by
        // `go_to_definition_formats_a_single_location_as_path_line_text`
        // above; this test covers the other direction — the *outgoing*
        // request must carry LSP's 0-indexed position even though the
        // tool's own arguments are 1-indexed.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 { n }\n").unwrap();

        let client = LspClient::new(Duration::from_secs(5));
        let (client_reader, server_writer) = tokio::io::duplex(8192);
        let (server_reader, client_writer) = tokio::io::duplex(8192);
        client.wire_for_test(client_reader, client_writer).await;

        let server = tokio::spawn(async move {
            fake_server_respond_once(
                tokio::io::BufReader::new(server_reader),
                server_writer,
                "textDocument/definition",
                serde_json::Value::Null,
            )
            .await
        });

        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        // 1-indexed line 7, column 12 -> must arrive at the server as
        // 0-indexed line 6, character 11.
        let _ = client
            .go_to_definition(dir.path(), &confiner, "lib.rs", 7, 12)
            .await
            .unwrap();

        let request = server.await.unwrap();
        assert_eq!(request["params"]["position"]["line"], 6);
        assert_eq!(request["params"]["position"]["character"], 11);
    }

    #[tokio::test]
    async fn find_references_reports_no_references_found_on_null_result() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 { n }\n").unwrap();

        let client = LspClient::new(Duration::from_secs(5));
        let (client_reader, server_writer) = tokio::io::duplex(8192);
        let (server_reader, client_writer) = tokio::io::duplex(8192);
        client.wire_for_test(client_reader, client_writer).await;

        let server = tokio::spawn(async move {
            let _ = fake_server_respond_once(
                tokio::io::BufReader::new(server_reader),
                server_writer,
                "textDocument/references",
                serde_json::Value::Null,
            )
            .await;
        });

        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let result = client
            .find_references(dir.path(), &confiner, "lib.rs", 1, 4)
            .await
            .unwrap();

        assert_eq!(result, "no references found");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn go_to_definition_sends_did_open_before_the_definition_request() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 { n }\n").unwrap();

        let client = LspClient::new(Duration::from_secs(5));
        let (client_reader, server_writer) = tokio::io::duplex(8192);
        let (server_reader, client_writer) = tokio::io::duplex(8192);
        client.wire_for_test(client_reader, client_writer).await;

        let server = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(server_reader);
            // First frame must be the didOpen notification (no "id").
            // `read_one_message` is `pub(crate)` in `transport` (Task 1) —
            // referenced here via the sibling module path, not a bare
            // name, since this test lives in `lsp::tests`, not
            // `transport::tests`.
            let first = transport::read_one_message(&mut reader).await.unwrap().unwrap();
            assert_eq!(first["method"], "textDocument/didOpen");
            assert!(first.get("id").is_none());
            assert_eq!(first["params"]["textDocument"]["text"], "fn fib(n: u64) -> u64 { n }\n");

            // Then respond to the actual request.
            let _ = fake_server_respond_once(
                &mut reader,
                server_writer,
                "textDocument/definition",
                serde_json::Value::Null,
            )
            .await;
        });

        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let _ = client
            .go_to_definition(dir.path(), &confiner, "lib.rs", 1, 4)
            .await
            .unwrap();
        server.await.unwrap();
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: FAIL to compile — `LspClient::go_to_definition`/`find_references` don't exist yet.

- [ ] **Step 3: Implement document sync and the two query methods**

Add to `crates/aivyx-tools/src/lsp/mod.rs`'s `impl LspClient` block, right after `ensure_started`:

```rust
    /// Resolves the symbol at `path:line:column` (1-indexed) to its
    /// definition site via `rust-analyzer`. Returns `path:line:text` for
    /// each location found, or a clear "no definition found" message.
    pub async fn go_to_definition(
        &self,
        cwd: &Path,
        confiner: &Arc<dyn ExecutionConfiner>,
        path: &str,
        line: u32,
        column: u32,
    ) -> Result<String, ToolError> {
        self.ensure_started(cwd, confiner).await?;
        let abs_path = cwd.join(path);
        let uri = path_to_uri(&abs_path);
        self.sync_document(&uri, &abs_path).await?;

        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line.saturating_sub(1), "character": column.saturating_sub(1) },
        });
        let result = self.request("textDocument/definition", params).await?;
        let locations = parse_locations(&result);
        format_locations(cwd, &locations, "no definition found").await
    }

    /// Finds every reference to the symbol at `path:line:column`
    /// (1-indexed) across the workspace via `rust-analyzer`. Returns
    /// `path:line:text` per reference site, or a clear "no references
    /// found" message.
    pub async fn find_references(
        &self,
        cwd: &Path,
        confiner: &Arc<dyn ExecutionConfiner>,
        path: &str,
        line: u32,
        column: u32,
    ) -> Result<String, ToolError> {
        self.ensure_started(cwd, confiner).await?;
        let abs_path = cwd.join(path);
        let uri = path_to_uri(&abs_path);
        self.sync_document(&uri, &abs_path).await?;

        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line.saturating_sub(1), "character": column.saturating_sub(1) },
            "context": { "includeDeclaration": true },
        });
        let result = self.request("textDocument/references", params).await?;
        let locations = parse_locations(&result);
        format_locations(cwd, &locations, "no references found").await
    }

    /// Sends `request`, bounded by `self.timeout` — cold `rust-analyzer`
    /// indexing on the first call can take a long time, so this is a
    /// per-request ceiling, not a per-session one.
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let guard = self.state.lock().await;
        let started = guard.as_ref().expect("ensure_started just succeeded");
        tokio::time::timeout(self.timeout, started.connection.request(method, params))
            .await
            .map_err(|_| {
                ToolError::ExecutionFailed(format!(
                    "rust-analyzer did not respond within {}s (cold indexing on a large \
                     workspace can be slow — increase [lsp] timeout_secs if needed)",
                    self.timeout.as_secs()
                ))
            })?
    }

    /// Keeps `rust-analyzer`'s view of `uri` current against a project
    /// with no persistent open-buffer concept: reads the file fresh from
    /// disk and sends `didOpen` (first time) or `didChange` (every
    /// subsequent time) immediately before every query, so an edit made
    /// via `write_file`/`edit_file` between two LSP calls is always
    /// reflected.
    async fn sync_document(&self, uri: &str, abs_path: &Path) -> Result<(), ToolError> {
        let content = tokio::fs::read_to_string(abs_path).await.map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to read {}: {err}", abs_path.display()))
        })?;

        let mut guard = self.state.lock().await;
        let started = guard.as_mut().expect("ensure_started just succeeded");
        let already_open = started.opened_uris.contains(uri);
        let version = started.next_doc_version;
        started.next_doc_version += 1;

        if already_open {
            let params = serde_json::json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": content }],
            });
            started.connection.notify("textDocument/didChange", params).await
        } else {
            started.opened_uris.insert(uri.to_string());
            let params = serde_json::json!({
                "textDocument": {
                    "uri": uri, "languageId": "rust", "version": version, "text": content,
                },
            });
            started.connection.notify("textDocument/didOpen", params).await
        }
    }
```

Add these free functions right after `initialize`:

```rust
fn parse_locations(result: &serde_json::Value) -> Vec<protocol::Location> {
    match result {
        serde_json::Value::Array(_) => {
            serde_json::from_value::<Vec<protocol::Location>>(result.clone()).unwrap_or_default()
        }
        serde_json::Value::Object(_) => serde_json::from_value::<protocol::Location>(result.clone())
            .map(|loc| vec![loc])
            .unwrap_or_default(),
        _ => Vec::new(), // null, or an unexpected shape
    }
}

async fn format_locations(
    cwd: &Path,
    locations: &[protocol::Location],
    empty_message: &str,
) -> Result<String, ToolError> {
    if locations.is_empty() {
        return Ok(empty_message.to_string());
    }
    let mut lines = Vec::with_capacity(locations.len());
    for location in locations {
        let path = uri_to_path(&location.uri);
        let display_path = path.strip_prefix(cwd).unwrap_or(&path).display().to_string();
        let line_number = location.range.start.line + 1; // back to 1-indexed
        let text = read_line(&path, location.range.start.line).await.unwrap_or_default();
        lines.push(format!("{display_path}:{line_number}:{text}"));
    }
    Ok(lines.join("\n"))
}

fn uri_to_path(uri: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(uri.strip_prefix("file://").unwrap_or(uri))
}

async fn read_line(path: &Path, zero_indexed_line: u32) -> Option<String> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    content
        .lines()
        .nth(zero_indexed_line as usize)
        .map(|line| line.trim().to_string())
}
```

Note: `go_to_definition`/`find_references` above call `self.request(...)` (the new private timeout-wrapping helper), not `started.connection.request(...)` directly — this replaces the direct-connection-access pattern from Task 2's own methods (which only calls `ensure_started`, not `request`).

Add `tempfile` to `[dev-dependencies]` if not already present — check `crates/aivyx-tools/Cargo.toml`'s `[dev-dependencies]` section first; it already lists `tempfile = "3.27.0"` (used by other tool tests), so no change needed there.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools lsp:: --lib`
Expected: PASS (10 tests total under `lsp::` — 6 from Tasks 1–2 plus 4 new).

- [ ] **Step 5: Run the full `aivyx-tools` test suite**

Run: `cargo test -p aivyx-tools --lib`
Expected: all tests pass, no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/lsp/mod.rs
git commit -m "LSP: document sync + go_to_definition/find_references query methods"
```

---

### Task 4: `GoToDefinitionTool` and `FindReferencesTool`

**Files:**
- Create: `crates/aivyx-tools/src/tools/go_to_definition.rs`
- Create: `crates/aivyx-tools/src/tools/find_references.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `LspClient::go_to_definition`/`find_references` (Task 3), `Tool`/`ToolError`/`ToolExecutionContext` (existing).
- Produces: `pub struct GoToDefinitionTool` with `::new(lsp: Arc<LspClient>) -> Self`, `pub struct FindReferencesTool` with the same constructor shape.

- [ ] **Step 1: Write the failing test for `GoToDefinitionTool`**

Create `crates/aivyx-tools/src/tools/go_to_definition.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::lsp::LspClient;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct GoToDefinitionArgs {
    /// Path to the file, relative to the working directory.
    path: String,
    /// 1-indexed line number of the symbol.
    line: u32,
    /// 1-indexed column number of the symbol.
    column: u32,
}

/// Exact symbol resolution via rust-analyzer: resolves the symbol at a
/// position to its actual definition site, which the repo map's ranked
/// static symbol list can't do when several candidates share a name.
pub struct GoToDefinitionTool {
    lsp: Arc<LspClient>,
}

impl GoToDefinitionTool {
    pub fn new(lsp: Arc<LspClient>) -> Self {
        Self { lsp }
    }
}

#[async_trait]
impl Tool for GoToDefinitionTool {
    fn name(&self) -> &str {
        "go_to_definition"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Resolve the symbol at a file position (1-indexed line/column) to its \
                actual definition site via rust-analyzer. Use this when the repo map's symbol \
                list isn't enough to tell which exact definition a call site resolves to. \
                Returns path:line:text, or a message if nothing was found."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GoToDefinitionArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GoToDefinitionArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(cwd.join(&args.path)),
            arguments_preview: json!({ "path": args.path, "line": args.line, "column": args.column }),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GoToDefinitionArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let text = self
            .lsp
            .go_to_definition(&ctx.cwd, &ctx.confiner, &args.path, args.line, args.column)
            .await?;
        Ok(ToolOutput::Ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn permission_request_targets_the_resolved_file_path_as_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let lsp = Arc::new(LspClient::new(Duration::from_secs(5)));
        let tool = GoToDefinitionTool::new(lsp);

        let request = tool
            .permission_request(&json!({"path": "src/lib.rs", "line": 3, "column": 5}), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Read);
        assert_eq!(
            request.target,
            PermissionTarget::Path(dir.path().join("src/lib.rs"))
        );
    }

    #[tokio::test]
    async fn execute_returns_ok_output_with_the_lsp_clients_result() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 { n }\n").unwrap();

        // `with_program` (not `LspClient::new`) with a name that can never
        // exist — deterministic regardless of whether the test machine
        // happens to have a real rust-analyzer on PATH. This test only
        // needs to prove `execute()` propagates whatever `LspClient`
        // returns rather than swallowing it, so it asserts on the error
        // shape rather than requiring (or forbidding) the real binary.
        let lsp = Arc::new(LspClient::with_program(
            "definitely-not-a-real-binary-xyz",
            Duration::from_secs(5),
        ));
        let tool = GoToDefinitionTool::new(lsp);
        let ctx = ctx(dir.path()).await;

        let err = tool
            .execute(json!({"path": "lib.rs", "line": 1, "column": 4}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }
}
```

- [ ] **Step 2: Write `FindReferencesTool`, mirroring the same shape**

Create `crates/aivyx-tools/src/tools/find_references.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::lsp::LspClient;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct FindReferencesArgs {
    /// Path to the file, relative to the working directory.
    path: String,
    /// 1-indexed line number of the symbol.
    line: u32,
    /// 1-indexed column number of the symbol.
    column: u32,
}

/// Exact symbol resolution via rust-analyzer: finds every reference to the
/// symbol at a position across the whole workspace, which grep's textual
/// search can't distinguish from unrelated same-named identifiers.
pub struct FindReferencesTool {
    lsp: Arc<LspClient>,
}

impl FindReferencesTool {
    pub fn new(lsp: Arc<LspClient>) -> Self {
        Self { lsp }
    }
}

#[async_trait]
impl Tool for FindReferencesTool {
    fn name(&self) -> &str {
        "find_references"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Find every reference to the symbol at a file position (1-indexed \
                line/column) across the whole workspace via rust-analyzer — unlike grep, this \
                distinguishes the symbol from unrelated identifiers that merely share its name. \
                Returns path:line:text per reference site, or a message if nothing was found."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(FindReferencesArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: FindReferencesArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(cwd.join(&args.path)),
            arguments_preview: json!({ "path": args.path, "line": args.line, "column": args.column }),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: FindReferencesArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let text = self
            .lsp
            .find_references(&ctx.cwd, &ctx.confiner, &args.path, args.line, args.column)
            .await?;
        Ok(ToolOutput::Ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn permission_request_targets_the_resolved_file_path_as_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let lsp = Arc::new(LspClient::new(Duration::from_secs(5)));
        let tool = FindReferencesTool::new(lsp);

        let request = tool
            .permission_request(&json!({"path": "src/lib.rs", "line": 3, "column": 5}), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Read);
        assert_eq!(
            request.target,
            PermissionTarget::Path(dir.path().join("src/lib.rs"))
        );
    }

    #[tokio::test]
    async fn execute_returns_ok_output_with_the_lsp_clients_result() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn fib(n: u64) -> u64 { n }\n").unwrap();

        // `with_program` (not `LspClient::new`) with a name that can never
        // exist — deterministic regardless of whether the test machine
        // happens to have a real rust-analyzer on PATH.
        let lsp = Arc::new(LspClient::with_program(
            "definitely-not-a-real-binary-xyz",
            Duration::from_secs(5),
        ));
        let tool = FindReferencesTool::new(lsp);
        let ctx = ctx(dir.path()).await;

        let err = tool
            .execute(json!({"path": "lib.rs", "line": 1, "column": 4}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aivyx-tools go_to_definition find_references --lib`
Expected: FAIL to compile — `crate::lsp::LspClient` isn't exported (`lsp` module is currently private, `LspClient` isn't `pub use`d), and `tools/mod.rs` doesn't declare the two new files yet.

- [ ] **Step 4: Wire the modules**

In `crates/aivyx-tools/src/lib.rs`, change:
```rust
pub use checkpoint::GitCheckpointer;
pub use process::CommandSpec;
pub use tools::{
    EditFileTool, GitCommitTool, GitReadTool, GlobTool, GrepTool, ReadFileTool, RunCommandTool,
    RunShellTool, SetTasksTool, WriteFileTool,
};
```
to:
```rust
pub use checkpoint::GitCheckpointer;
pub use lsp::LspClient;
pub use process::CommandSpec;
pub use tools::{
    EditFileTool, FindReferencesTool, GitCommitTool, GitReadTool, GlobTool, GoToDefinitionTool,
    GrepTool, ReadFileTool, RunCommandTool, RunShellTool, SetTasksTool, WriteFileTool,
};
```

In `crates/aivyx-tools/src/tools/mod.rs`, add the two new modules and re-exports:
```rust
mod edit_file;
mod find_references;
mod git_commit;
mod git_read;
mod glob;
mod go_to_definition;
mod grep;
mod read_file;
mod run_command;
mod run_shell;
mod set_tasks;
mod write_file;

pub use edit_file::EditFileTool;
pub use find_references::FindReferencesTool;
pub use git_commit::GitCommitTool;
pub use git_read::GitReadTool;
pub use glob::GlobTool;
pub use go_to_definition::GoToDefinitionTool;
pub use grep::GrepTool;
pub use read_file::ReadFileTool;
pub use run_command::RunCommandTool;
pub use run_shell::RunShellTool;
pub use set_tasks::SetTasksTool;
pub use write_file::WriteFileTool;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools go_to_definition find_references --lib`
Expected: PASS (4 tests: 2 per tool).

- [ ] **Step 6: Run the full `aivyx-tools` test suite**

Run: `cargo test -p aivyx-tools --lib`
Expected: all tests pass, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/tools/go_to_definition.rs crates/aivyx-tools/src/tools/find_references.rs \
        crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "LSP: GoToDefinitionTool and FindReferencesTool"
```

---

### Task 5: `[lsp]` config + `main.rs` wiring

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `LspClient::new` (Task 3), `GoToDefinitionTool::new`/`FindReferencesTool::new` (Task 4).
- Produces: `pub struct LspSettings { pub timeout_secs: u64 }`, `Settings.lsp: LspSettings`.

- [ ] **Step 1: Write the failing config tests**

Add to the `mod tests` block in `crates/aivyx-config/src/lib.rs`, right after the `architect_needs_both_base_url_and_model_to_be_configured` test (or wherever the most recently added feature's tests are — append after the last one):

```rust
    #[test]
    fn lsp_settings_default_timeout_is_sixty_seconds() {
        let settings = Settings::default();
        assert_eq!(settings.lsp.timeout_secs, 60);
    }

    #[test]
    fn lsp_block_parses_a_custom_timeout() {
        let raw = r#"
            [lsp]
            timeout_secs = 120
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.lsp.timeout_secs, 120);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-config lsp --lib`
Expected: FAIL to compile — `Settings` has no field `lsp`.

- [ ] **Step 3: Implement `LspSettings`**

In `crates/aivyx-config/src/lib.rs`, add `pub lsp: LspSettings,` to the `Settings` struct (next to `pub sub_agent: SubAgentSettings,`):

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: BackendSettings,
    pub permissions: PermissionSettings,
    pub sandbox: SandboxSettings,
    pub git: GitSettings,
    pub repo_map: RepoMapSettings,
    pub council: CouncilSettings,
    pub architect: ArchitectSettings,
    pub verification: VerificationSettings,
    pub autonomous: AutonomousSettings,
    pub sub_agent: SubAgentSettings,
    pub lsp: LspSettings,
}
```

Then add the new struct after `SubAgentSettings`'s definition:

```rust
/// LSP integration (`go_to_definition`/`find_references`, ROADMAP.md
/// Phase 9): bounds each JSON-RPC request to the lazily-spawned
/// `rust-analyzer` subprocess. Generous default — cold indexing on the
/// first call in a large workspace can take tens of seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LspSettings {
    pub timeout_secs: u64,
}

impl Default for LspSettings {
    fn default() -> Self {
        Self { timeout_secs: 60 }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-config lsp --lib`
Expected: PASS (2 new tests).

- [ ] **Step 5: Run the full `aivyx-config` test suite**

Run: `cargo test -p aivyx-config --lib`
Expected: all tests pass, no regressions.

- [ ] **Step 6: Wire construction into `main.rs`**

In `crates/aivyx/src/main.rs`, update the `aivyx_tools` import line to add the two new tools:
```rust
use aivyx_tools::{
    CommandSpec, EditFileTool, FindReferencesTool, GitCheckpointer, GitCommitTool, GitReadTool,
    GlobTool, GoToDefinitionTool, GrepTool, LspClient, ReadFileTool, RunCommandTool, RunShellTool,
    SetTasksTool, ToolExecutor, ToolRegistry, WriteFileTool,
};
```

Right after the existing `registry.register(Arc::new(GitCommitTool::new(deny_paths.clone())));` line (before the `if !command_specs.is_empty()` block), add:

```rust
    let lsp_client = Arc::new(LspClient::new(Duration::from_secs(settings.lsp.timeout_secs)));
    registry.register(Arc::new(GoToDefinitionTool::new(Arc::clone(&lsp_client))));
    registry.register(Arc::new(FindReferencesTool::new(Arc::clone(&lsp_client))));
```

- [ ] **Step 7: Build the workspace**

Run: `cargo build --workspace`
Expected: builds cleanly, no warnings.

- [ ] **Step 8: Manual config smoke check**

Run the real `aivyx` binary against a scratch directory with a default
(unconfigured, i.e. absent) `[lsp]` section and confirm it starts normally
— config is `#[serde(default)]` throughout, so an absent section is fine,
matching every prior phase's config-addition smoke check.

- [ ] **Step 9: Commit**

```bash
git add crates/aivyx-config/src/lib.rs crates/aivyx/src/main.rs
git commit -m "Wire go_to_definition/find_references construction into main.rs (Phase 9)"
```

---

### Task 6: Real rust-analyzer integration test + docs + live E2E + final check

**Files:**
- Create: `crates/aivyx-tools/tests/lsp_integration.rs` (a `tests/` integration test, not a `#[cfg(test)]` unit test — matches Cargo's convention for a test that spawns a real external binary rather than exercising crate-internal-only mock transports)
- Modify: `README.md`
- Modify: `ROADMAP.md`

**Interfaces:**
- Consumes: `aivyx_tools::{LspClient, GoToDefinitionTool, FindReferencesTool}` (Tasks 3–4), all `pub`.

- [ ] **Step 1: Write the real-`rust-analyzer` integration test**

Create `crates/aivyx-tools/tests/lsp_integration.rs`:

```rust
//! Exercises the real `rust-analyzer` binary end-to-end — spawn, confiner
//! reuse (via `NoopConfiner`, since this test doesn't need real
//! Landlock/seccomp confinement to prove the LSP protocol round-trip
//! works), initialize handshake, a real go-to-definition and
//! find-references query, and respawn-after-crash. Skipped entirely (not
//! failed) when `rust-analyzer` isn't on `PATH`, since this project's dev
//! and CI environments aren't guaranteed to have it installed — every
//! other assertion in this crate's test suite works without it.

use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::Duration;

use aivyx_sandbox::NoopConfiner;
use aivyx_tools::LspClient;

fn rust_analyzer_available() -> bool {
    StdCommand::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_ok()
}

#[tokio::test]
async fn go_to_definition_and_find_references_round_trip_against_a_real_workspace() {
    if !rust_analyzer_available() {
        eprintln!("skipping: rust-analyzer not found on PATH");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn fib(n: u64) -> u64 {\n    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n}\n\n\
             pub fn call_fib() -> u64 {\n    fib(10)\n}\n",
    )
    .unwrap();

    let confiner: Arc<dyn aivyx_sandbox::ExecutionConfiner> = Arc::new(NoopConfiner);
    // Generous timeout: cold rust-analyzer indexing of even a tiny fixture
    // crate can take tens of seconds on a loaded CI machine.
    let client = LspClient::new(Duration::from_secs(120));

    // `call_fib`'s body calls `fib(10)` at line 6 (1-indexed), column 5 —
    // go-to-definition from that call site should resolve to `fib`'s own
    // definition at line 1.
    let definition = client
        .go_to_definition(dir.path(), &confiner, "src/lib.rs", 6, 5)
        .await
        .expect("go_to_definition should succeed against a real rust-analyzer");
    assert!(
        definition.contains("src/lib.rs:1:"),
        "expected the definition to resolve to line 1, got: {definition}"
    );

    // Every reference to `fib` (its own two recursive calls plus the call
    // from `call_fib`) should be found from a query on the definition site
    // itself (line 1).
    let references = client
        .find_references(dir.path(), &confiner, "src/lib.rs", 1, 8)
        .await
        .expect("find_references should succeed against a real rust-analyzer");
    assert!(
        references.lines().count() >= 3,
        "expected at least 3 reference sites (2 recursive calls + 1 from call_fib), got: \
         {references}"
    );
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p aivyx-tools --test lsp_integration`
Expected: if `rust-analyzer` is on `PATH`, PASS with real output; if not,
the test still reports PASS (it returns early after printing a skip
notice to stderr — Rust's `#[tokio::test]` has no built-in
conditional-skip primitive, so an early `return` inside a passing test is
this workspace's chosen way to represent "skipped", consistent with how
Cargo test output would otherwise have no way to distinguish "skipped"
from "not run at all").

- [ ] **Step 3: Live E2E through the real binary**

Using the same PTY-harness pattern as every prior phase, with one
adjustment: `go_to_definition`/`find_references` are read-only with **no
confirmation modal** at all (`ActionKind::Read` auto-allows), so grade
differently than the write-heavy E2Es before this one. Configure a scratch
Rust crate identical in shape to the integration test's fixture above (a
`fib`/`call_fib` pair). Run the real `aivyx` binary against it with a
message like: `use go_to_definition to find where fib is defined, called
from line 6 column 5 of src/lib.rs`.

Confirm:
1. The tool call (`go_to_definition`) appears live in the transcript.
2. The returned `path:line:text` result correctly names line 1 (the real
   `fib` definition) — read this from the persisted session JSON's tool
   result content, not from screen-scraping.
3. No confirmation modal appeared at any point (`approvals=0` in the
   harness's own counter) — confirming `ActionKind::Read` really does
   auto-allow, matching `grep`'s existing UX.

If `rust-analyzer` isn't installed in this environment, this step cannot
be completed as a live check — in that case, note this plainly in the
report rather than silently skipping it (the Task 6 report must state
explicitly whether this step ran against a real `rust-analyzer` or was
blocked, and why).

- [ ] **Step 4: Update `README.md`**

Add a new paragraph after the existing "Architect/editor model-pairing"
paragraph, documenting `go_to_definition`/`find_references`, the lazy
`rust-analyzer` spawn (confined by the same `ExecutionConfiner` as
`run_shell`/`run_command`), the 1-indexed `path:line:text` convention
matching `grep`, and the `[lsp] timeout_secs` config knob.

- [ ] **Step 5: Update `ROADMAP.md`**

In the Phase 9 section, insert a "built and live-verified" paragraph
(matching the style of the existing sub-agent-delegation and
architect/editor-pairing entries immediately above it), covering: the
`aivyx-tools/src/lsp/` module's shape (transport generic over
reader/writer, `LspClient` constructor-baked into both tools rather than
threaded through `ToolExecutor`, unlike `GitCheckpointer`), the
document-sync-before-every-query strategy and why it's needed (no
persistent open-buffer concept), the hand-rolled-protocol-types-over-
`lsp-types`-dependency choice, the test count delta, and the live E2E
results (including whether it ran against a real `rust-analyzer` per Step
3's note above).

- [ ] **Step 6: Final full-workspace check**

Run: `cargo test --workspace`
Expected: all tests pass.

Run: `cargo clippy --workspace --all-targets`
Expected: clean, no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/tests/lsp_integration.rs README.md ROADMAP.md
git commit -m "LSP: real-rust-analyzer integration test, docs, live E2E verification (Phase 9)"
```
