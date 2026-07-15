# MCP Client Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give aivyx-coder a full MCP (Model Context Protocol) client — tools, resources, and prompts — over stdio transport, so the model can use arbitrary user-configured third-party servers alongside its hand-written tools.

**Architecture:** A new `crates/aivyx-tools/src/mcp/` module (mirroring the existing `lsp/` module's `transport.rs`/`protocol.rs`/`mod.rs` split) implements an MCP stdio JSON-RPC 2.0 client. Each discovered MCP tool becomes a `Tool` trait adapter registered under `mcp__<server>__<tool>`, always confirm-gated via a new `ActionKind::McpTool`. Resources and prompts surface through four fixed, always-`Read`-tier meta-tools rather than per-item registration. `main.rs` connects to every configured server concurrently at startup, bounded per-server by a timeout, before `ToolRegistry` is built.

**Tech Stack:** Rust, tokio (process/time/sync — already workspace dependencies), serde_json, schemars. No new external crate.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-16-mcp-support-design.md` — read it in full before starting; every task below implements a piece of it exactly.
- stdio transport only. No HTTP/SSE in this plan.
- Tools, resources, AND prompts are all in scope (not tools-only).
- Every discovered MCP tool call uses a new `ActionKind::McpTool`, always confirm-gated — never auto-allowed, regardless of anything the server claims about itself (no `readOnlyHint`/`destructiveHint` trust, no per-server trust config).
- The four resources/prompts meta-tools (`list_mcp_resources`, `read_mcp_resource`, `list_mcp_prompts`, `get_mcp_prompt`) use `ActionKind::Read` with `mutates_outside_session() = false` — auto-allowed, available in Plan Mode.
- MCP tool adapters do NOT override `mutates_outside_session()` — the trait default (`true`) applies, so they are hidden in Plan Mode like `Write`/`Execute`/`Delete` tools.
- Discovery (spawn + `initialize` handshake + `tools/list`/`resources/list`/`prompts/list`) happens once, eagerly, per configured server, concurrently, before `ToolRegistry` is built — bounded by that server's own `timeout_secs`. A server that fails or times out is skipped with a one-time warning (`tracing::warn!` + `events_tx.send(aivyx_core::AgentEvent::Error(...))`), never blocking the rest of startup.
- A dead connection (mid-session crash) is respawned on next use, re-running only the `initialize` handshake — NOT full rediscovery (`tools/list` etc. only ever run once, at startup, to populate the static `ToolRegistry`).
- Non-text content blocks (images, audio, embedded resources) render as a placeholder string (`[non-text content of type "X" omitted]`), never decoded.
- An MCP `isError: true` tool result maps to `ToolOutput::Error`, not `Ok`.
- Config: `[[mcp.servers]]` array-of-tables in `config.toml`, reachable as `Settings.mcp.servers: Vec<McpServerConfig>`. No top-level enable flag — an empty list is the complete off-switch.
- No new external crate dependency for the MCP client itself (hand-rolled, matching the LSP client's own precedent).
- All HTTP/process-touching tests run against an in-process test double (`tokio::io::duplex()` pairs, mirroring `lsp/transport.rs`'s and `lsp/mod.rs`'s own test patterns) — never a real spawned process in the automated test suite.

---

### Task 1: Foundations — `ActionKind::McpTool` + `McpServerConfig`/`McpSettings`

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs` (add `ActionKind::McpTool` variant)
- Modify: `crates/aivyx-sandbox/src/confirmation.rs` (add a test proving the new variant is confirm-gated, not auto-allowed)
- Modify: `crates/aivyx-config/src/lib.rs` (add `McpServerConfig`, `McpSettings`, and the new `Settings.mcp` field)

**Interfaces:**
- Produces: `aivyx_sandbox::ActionKind::McpTool` (a new unit variant on the existing `Copy, PartialEq, Eq, Hash` enum). `aivyx_config::McpServerConfig { name: String, command: String, args: Vec<String>, env: HashMap<String, String>, timeout_secs: u64 }` (default `timeout_secs = 30`, everything else empty). `aivyx_config::McpSettings { servers: Vec<McpServerConfig> }` (default empty). New field `Settings.mcp: McpSettings`.

- [ ] **Step 1: Add `ActionKind::McpTool`**

In `crates/aivyx-sandbox/src/lib.rs`, find the existing enum (currently `Read, Write, Execute, Delete, Internal`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Read,
    Write,
    Execute,
    Delete,
    /// Mutates only the agent's own in-session state (e.g. the task list) —
    /// no filesystem, process, or network effect. Auto-allowed like `Read`,
    /// but kept distinct so a tool that touches the outside world can't
    /// honestly describe itself this way, and so audit logs don't record an
    /// internal state change as a "Read" of anything.
    Internal,
}
```

Add a new variant after `Internal`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Read,
    Write,
    Execute,
    Delete,
    /// Mutates only the agent's own in-session state (e.g. the task list) —
    /// no filesystem, process, or network effect. Auto-allowed like `Read`,
    /// but kept distinct so a tool that touches the outside world can't
    /// honestly describe itself this way, and so audit logs don't record an
    /// internal state change as a "Read" of anything.
    Internal,
    /// A tool call dispatched to an external MCP (Model Context Protocol)
    /// server — arbitrary, user-configured third-party code whose actual
    /// behavior this project can't verify, regardless of anything the
    /// server itself claims (e.g. MCP's optional, advisory `readOnlyHint`
    /// annotation). Always confirm-gated, uniformly: there is no case where
    /// this is treated as auto-allowed, unlike every other `ActionKind`.
    /// Kept distinct from `Write`/`Execute` so audit logs and the
    /// confirmation modal can honestly say "this is an MCP call," not
    /// mislabel it as a filesystem write or local command execution.
    McpTool,
}
```

Do NOT add `ActionKind::McpTool` to `ConfirmationGate::check`'s auto-allow match arm in `crates/aivyx-sandbox/src/confirmation.rs` (`if matches!(request.action, ActionKind::Read | ActionKind::Internal) { return PermissionDecision::Allow; }`) — leave that line untouched. `McpTool` must fall through to the existing confirm-gated path below it (the plan-mode check, then autonomous-mode handling, then the Always-Allow cache / prompt flow), exactly like `Write`/`Execute`/`Delete` already do. This requires no code change in `confirmation.rs` — the fall-through is automatic once the new variant just isn't listed in that one `matches!` — but Step 2 below adds a test proving it.

- [ ] **Step 2: Add a test proving `McpTool` is confirm-gated**

In `crates/aivyx-sandbox/src/confirmation.rs`'s `#[cfg(test)] mod tests` block, add (matching the existing `reads_auto_allow_without_prompting`/`internal_actions_auto_allow_without_prompting` tests' exact shape, just asserting the opposite — that the prompter IS called):

```rust
#[tokio::test]
async fn mcp_tool_actions_are_confirm_gated_not_auto_allowed() {
    let prompter = Arc::new(FakePrompter {
        response: UserResponse::Allow,
        calls: AtomicUsize::new(0),
    });
    let gate = ConfirmationGate::new(
        prompter.clone(),
        vec![],
        vec![],
        PlanMode::new(),
        AutonomousMode::new(),
        PathBuf::from("/home/user/project"),
    );

    let request = PermissionRequest {
        tool_name: "mcp__filesystem__search_docs".to_string(),
        action: ActionKind::McpTool,
        target: PermissionTarget::Other("search_docs (server: filesystem)".to_string()),
        arguments_preview: serde_json::json!({}),
        preview: None,
    };

    let decision = gate.check(&request).await;
    assert_eq!(decision, PermissionDecision::Allow);
    // The point of this test: unlike Read/Internal, the prompter was
    // actually invoked — this call did NOT short-circuit to auto-allow.
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 3: Run the sandbox test suite**

Run: `cargo test -p aivyx-sandbox`
Expected: all tests pass, including the new `mcp_tool_actions_are_confirm_gated_not_auto_allowed`.

- [ ] **Step 4: Add `McpServerConfig` and `McpSettings` to `aivyx-config`**

In `crates/aivyx-config/src/lib.rs`, add `use std::collections::HashMap;` to the existing import block at the top of the file:

```rust
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
```

Immediately after `WebSettings`'s `impl Default for WebSettings` block (the one ending `allow_private_targets: false, } } }`), add:

```rust
/// One configured MCP (Model Context Protocol) server: spawned as a child
/// process, speaking JSON-RPC 2.0 over its stdin/stdout (stdio transport
/// only — see `docs/superpowers/specs/2026-07-16-mcp-support-design.md`'s
/// non-goals for remote/HTTP transport).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Used verbatim in the registered `mcp__<name>__<tool>` tool name and
    /// in the resources/prompts meta-tools' `server` filter argument.
    pub name: String,
    /// The executable to spawn.
    pub command: String,
    pub args: Vec<String>,
    /// Additional environment variables for the spawned process (e.g. API
    /// keys the server itself needs), merged over the agent's own
    /// environment.
    pub env: HashMap<String, String>,
    /// Bounds this server's spawn + `initialize` handshake + discovery
    /// (`tools/list`/`resources/list`/`prompts/list`) sequence at startup.
    /// A server that doesn't finish within this budget is skipped for the
    /// session with a warning, rather than blocking startup indefinitely.
    pub timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            timeout_secs: 30,
        }
    }
}

/// Wraps `servers` purely so `config.toml` can use `[[mcp.servers]]` array-
/// of-tables syntax rather than a flat `[[mcp_servers]]` at the top level.
/// No top-level `[mcp] enabled` flag: an empty `servers` list is already a
/// complete no-op (no servers to connect to, nothing to register), unlike
/// `[web] enabled`, which had to gate two tools that are otherwise always
/// statically present regardless of configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub servers: Vec<McpServerConfig>,
}
```

Then add `pub mcp: McpSettings,` as the new last field on the top-level `Settings` struct:

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
    pub agents_file: AgentsFileSettings,
    pub web: WebSettings,
    pub mcp: McpSettings,
}
```

- [ ] **Step 5: Write the failing tests**

In `crates/aivyx-config/src/lib.rs`'s `#[cfg(test)] mod tests` block (starting at the existing `#[cfg(test)]` near the end of the file), add:

```rust
#[test]
fn mcp_server_config_defaults() {
    let config = McpServerConfig::default();
    assert_eq!(config.name, "");
    assert_eq!(config.command, "");
    assert!(config.args.is_empty());
    assert!(config.env.is_empty());
    assert_eq!(config.timeout_secs, 30);
}

#[test]
fn settings_defaults_to_no_mcp_servers() {
    let settings = Settings::default();
    assert!(settings.mcp.servers.is_empty());
}

#[test]
fn mcp_servers_array_parses_from_toml() {
    let toml_str = r#"
        [[mcp.servers]]
        name = "filesystem"
        command = "npx"
        args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        timeout_secs = 15

        [mcp.servers.env]
        API_KEY = "secret"
    "#;
    let settings: Settings = toml::from_str(toml_str).unwrap();
    assert_eq!(settings.mcp.servers.len(), 1);
    let server = &settings.mcp.servers[0];
    assert_eq!(server.name, "filesystem");
    assert_eq!(server.command, "npx");
    assert_eq!(
        server.args,
        vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
    );
    assert_eq!(server.timeout_secs, 15);
    assert_eq!(server.env.get("API_KEY"), Some(&"secret".to_string()));
}
```

- [ ] **Step 6: Run the tests, expect PASS**

Run: `cargo test -p aivyx-config`
Expected: all tests pass, including the 3 new ones above.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs crates/aivyx-sandbox/src/confirmation.rs crates/aivyx-config/src/lib.rs
git commit -m "MCP support: ActionKind::McpTool + McpServerConfig/McpSettings (Task 1)"
```

---

### Task 2: MCP stdio transport

**Files:**
- Create: `crates/aivyx-tools/src/mcp/transport.rs`
- Create (empty placeholder, filled in Task 3): none yet — `mod.rs`/`protocol.rs` are Task 3's deliverable.

**Interfaces:**
- Consumes: `crate::ToolError` (existing).
- Produces: `pub(crate) struct McpConnection` with `pub(crate) fn new(reader, writer) -> Self`, `pub(crate) fn is_dead(&self) -> bool`, `pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, ToolError>`, `pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<(), ToolError>`. Task 3 constructs and uses this type.

This mirrors `crates/aivyx-tools/src/lsp/transport.rs`'s `Connection` almost exactly (pending-map request/response correlation, background read-loop task, `is_dead()` liveness, `Drop`-based cleanup) — the one real difference is wire framing: MCP's stdio transport uses **newline-delimited JSON** (one complete JSON-RPC message per line, terminated by `\n`), not LSP's `Content-Length: N\r\n\r\n` header block. Read `crates/aivyx-tools/src/lsp/transport.rs` first to see the pattern this mirrors.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/mcp/transport.rs` with just the test module first (so Step 2 proves it fails to compile/run before the real implementation exists):

```rust
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
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools mcp::transport`
Expected: FAIL to compile (`McpConnection::new`, `request`, `notify`, `is_dead`, `read_one_message` not yet defined).

- [ ] **Step 3: Implement `McpConnection`**

Add the implementation above the `#[cfg(test)]` module in the same file:

```rust
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
            Ok(value) => Ok(value),
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
                    let payload = value.get("result").cloned().unwrap_or(Value::Null);
                    let _ = tx.send(payload);
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
```

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools mcp::transport`
Expected: PASS — 3 tests (`request_resolves_when_a_matching_response_arrives`, `is_dead_becomes_true_once_the_reader_hits_eof`, `notify_writes_a_well_framed_message_with_no_id`).

- [ ] **Step 5: Wire the (currently empty) `mcp` module into the crate**

Create `crates/aivyx-tools/src/mcp/mod.rs` with just enough to compile for now (Task 3 fills in the rest):

```rust
mod transport;
```

In `crates/aivyx-tools/src/lib.rs`, add `mod mcp;` alphabetically among the existing `mod` lines (before `mod path_resolve;`, after `mod lsp;`):

```rust
mod checkpoint;
mod diff;
mod lsp;
mod mcp;
mod path_resolve;
mod process;
mod tools;
pub mod web;
pub mod wiki;
```

- [ ] **Step 6: Run the whole crate's tests to confirm nothing broke**

Run: `cargo test -p aivyx-tools`
Expected: all pre-existing tests still pass, plus the 3 new transport tests.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/mcp/ crates/aivyx-tools/src/lib.rs
git commit -m "MCP support: stdio transport, newline-delimited JSON-RPC (Task 2)"
```

---

### Task 3: MCP protocol types + `McpClient` lifecycle

**Files:**
- Create: `crates/aivyx-tools/src/mcp/protocol.rs`
- Modify: `crates/aivyx-tools/src/mcp/mod.rs`

**Interfaces:**
- Consumes: `transport::McpConnection` (Task 2) with its `new`/`is_dead`/`request`/`notify` methods.
- Produces: `pub struct McpClient` with `pub fn new(server_name: String, program: String, args: Vec<String>, env: Vec<(String, String)>) -> Self`, `pub fn server_name(&self) -> &str`, `pub async fn ensure_started(&self, cwd: &Path, confiner: &Arc<dyn ExecutionConfiner>) -> Result<(), ToolError>`, `pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, ToolError>`, `pub async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<(String, bool), ToolError>` (the `bool` is `isError`), `pub async fn list_resources(&self) -> Result<Vec<ResourceInfo>, ToolError>`, `pub async fn read_resource(&self, uri: &str) -> Result<String, ToolError>`, `pub async fn list_prompts(&self) -> Result<Vec<PromptInfo>, ToolError>`, `pub async fn get_prompt(&self, name: &str, arguments: Option<serde_json::Value>) -> Result<String, ToolError>`. Also `pub struct ToolInfo { pub name: String, pub description: String, pub input_schema: serde_json::Value }`, `pub struct ResourceInfo { pub uri: String, pub name: String, pub description: String }`, `pub struct PromptInfo { pub name: String, pub description: String, pub arguments: Vec<PromptArgumentInfo> }`, `pub struct PromptArgumentInfo { pub name: String, pub description: String, pub required: bool }`. Tasks 4 and 5 consume all of the above.

- [ ] **Step 1: Write the protocol types**

Create `crates/aivyx-tools/src/mcp/protocol.rs`:

```rust
//! Minimal MCP JSON-RPC result shapes — just enough of each method's
//! response to drive this client's tools/resources/prompts surface.
//! Mirrors `crate::lsp::protocol`'s minimal-parsing approach.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToolsListResult {
    #[serde(default)]
    pub(crate) tools: Vec<ToolInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContentBlock {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ToolCallResult {
    #[serde(default)]
    pub(crate) content: Vec<ContentBlock>,
    #[serde(default, rename = "isError")]
    pub(crate) is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceInfo {
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourcesListResult {
    #[serde(default)]
    pub(crate) resources: Vec<ResourceInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourceContent {
    #[serde(default)]
    pub(crate) uri: String,
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) blob: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResourcesReadResult {
    #[serde(default)]
    pub(crate) contents: Vec<ResourceContent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptArgumentInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<PromptArgumentInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptsListResult {
    #[serde(default)]
    pub(crate) prompts: Vec<PromptInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptMessage {
    #[serde(default)]
    pub(crate) role: String,
    pub(crate) content: ContentBlock,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PromptGetResult {
    #[serde(default)]
    pub(crate) messages: Vec<PromptMessage>,
}
```

- [ ] **Step 2: Write the failing tests for `McpClient`**

Replace `crates/aivyx-tools/src/mcp/mod.rs`'s content with (test module first):

```rust
mod protocol;
mod transport;

use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::ExecutionConfiner;
use tokio::sync::Mutex;

use crate::ToolError;
use transport::McpConnection;

pub use protocol::{PromptArgumentInfo, PromptInfo, ResourceInfo, ToolInfo};

struct Started {
    connection: Arc<McpConnection>,
    child: Option<tokio::process::Child>,
}

/// Owns one configured MCP server's spawned child process (or, in tests, an
/// in-memory transport wired directly) and its JSON-RPC session. One
/// `McpClient` per configured `[[mcp.servers]]` entry, shared via `Arc`
/// between every `McpToolAdapter`/meta-tool that talks to it.
pub struct McpClient {
    server_name: String,
    program: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    state: Mutex<Option<Started>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn server_task(
        mut server_reader: tokio::io::DuplexStream,
        mut server_writer: tokio::io::DuplexStream,
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut reader = BufReader::new(&mut server_reader);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let request: serde_json::Value = match serde_json::from_str(line.trim_end()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
                continue;
            };
            if let Some(id) = request.get("id") {
                let result = response_for(method);
                let response = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let mut body = serde_json::to_vec(&response).unwrap();
                body.push(b'\n');
                let _ = server_writer.write_all(&body).await;
                let _ = server_writer.flush().await;
            }
        }
    }

    async fn wired_client(
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
    ) -> McpClient {
        let (client_reader, server_writer) = tokio::io::duplex(65536);
        let (server_reader, client_writer) = tokio::io::duplex(65536);
        tokio::spawn(server_task(server_reader, server_writer, response_for));
        let client = McpClient::new(
            "test-server".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        );
        client.wire_for_test(client_reader, client_writer).await;
        client
    }

    #[tokio::test]
    async fn ensure_started_reports_a_clear_error_when_the_program_is_missing() {
        let client = McpClient::new(
            "broken".to_string(),
            "definitely-not-a-real-binary-xyz".to_string(),
            vec![],
            vec![],
        );
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(aivyx_sandbox::NoopConfiner);
        let err = client
            .ensure_started(Path::new("."), &confiner)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("broken"));
    }

    #[tokio::test]
    async fn ensure_started_reuses_an_already_healthy_session_without_respawning() {
        let client = wired_client(|_| serde_json::json!({})).await;
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(aivyx_sandbox::NoopConfiner);
        // The wired-for-test connection has no real child process behind
        // it, so a respawn attempt would try to spawn "unused" and fail —
        // two Ok(()) calls in a row prove the healthy session was reused,
        // not respawned.
        client.ensure_started(Path::new("."), &confiner).await.unwrap();
        client.ensure_started(Path::new("."), &confiner).await.unwrap();
    }

    #[tokio::test]
    async fn list_tools_parses_tools_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "tools/list");
            serde_json::json!({
                "tools": [
                    {"name": "search_docs", "description": "search the docs", "inputSchema": {"type": "object"}}
                ]
            })
        })
        .await;
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search_docs");
        assert_eq!(tools[0].description, "search the docs");
    }

    #[tokio::test]
    async fn call_tool_concatenates_text_content_blocks_and_reports_is_error() {
        let client = wired_client(|method| {
            assert_eq!(method, "tools/call");
            serde_json::json!({
                "content": [
                    {"type": "text", "text": "first"},
                    {"type": "text", "text": "second"},
                    {"type": "image", "data": "base64stuff"}
                ],
                "isError": false
            })
        })
        .await;
        let (text, is_error) = client
            .call_tool("search_docs", serde_json::json!({"q": "rust"}))
            .await
            .unwrap();
        assert!(text.contains("first"));
        assert!(text.contains("second"));
        assert!(text.contains("[non-text content of type \"image\" omitted]"));
        assert!(!is_error);
    }

    #[tokio::test]
    async fn call_tool_reports_is_error_true_when_the_server_says_so() {
        let client = wired_client(|_| {
            serde_json::json!({
                "content": [{"type": "text", "text": "boom"}],
                "isError": true
            })
        })
        .await;
        let (text, is_error) = client
            .call_tool("search_docs", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(text, "boom");
        assert!(is_error);
    }

    #[tokio::test]
    async fn list_resources_parses_resources_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "resources/list");
            serde_json::json!({
                "resources": [
                    {"uri": "file:///a.txt", "name": "a.txt", "description": "a file"}
                ]
            })
        })
        .await;
        let resources = client.list_resources().await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, "file:///a.txt");
    }

    #[tokio::test]
    async fn read_resource_renders_text_and_binary_placeholder_content() {
        let client = wired_client(|method| {
            assert_eq!(method, "resources/read");
            serde_json::json!({
                "contents": [
                    {"uri": "file:///a.txt", "text": "hello"},
                    {"uri": "file:///b.png", "blob": "base64stuff"}
                ]
            })
        })
        .await;
        let text = client.read_resource("file:///a.txt").await.unwrap();
        assert!(text.contains("hello"));
        assert!(text.contains("[binary resource content at file:///b.png omitted]"));
    }

    #[tokio::test]
    async fn list_prompts_parses_prompts_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "prompts/list");
            serde_json::json!({
                "prompts": [
                    {"name": "summarize", "description": "summarize text", "arguments": [{"name": "text", "required": true}]}
                ]
            })
        })
        .await;
        let prompts = client.list_prompts().await.unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "summarize");
        assert_eq!(prompts[0].arguments[0].name, "text");
        assert!(prompts[0].arguments[0].required);
    }

    #[tokio::test]
    async fn get_prompt_renders_expanded_messages_from_a_scripted_response() {
        let client = wired_client(|method| {
            assert_eq!(method, "prompts/get");
            serde_json::json!({
                "messages": [
                    {"role": "user", "content": {"type": "text", "text": "summarize this"}}
                ]
            })
        })
        .await;
        let text = client
            .get_prompt("summarize", Some(serde_json::json!({"text": "hi"})))
            .await
            .unwrap();
        assert!(text.contains("user"));
        assert!(text.contains("summarize this"));
    }
}
```

- [ ] **Step 3: Run to confirm it fails**

Run: `cargo test -p aivyx-tools mcp::`
Expected: FAIL to compile (`McpClient::new`, `wire_for_test`, `ensure_started`, `list_tools`, `call_tool`, `list_resources`, `read_resource`, `list_prompts`, `get_prompt` not yet defined).

- [ ] **Step 4: Implement `McpClient`**

Add to `crates/aivyx-tools/src/mcp/mod.rs`, above the `#[cfg(test)]` module:

```rust
impl McpClient {
    pub fn new(server_name: String, program: String, args: Vec<String>, env: Vec<(String, String)>) -> Self {
        Self {
            server_name,
            program,
            args,
            env,
            state: Mutex::new(None),
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    #[cfg(test)]
    pub(crate) async fn wire_for_test(
        &self,
        reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
    ) {
        *self.state.lock().await = Some(Started {
            connection: Arc::new(McpConnection::new(reader, writer)),
            child: None,
        });
    }

    /// Ensures a live, initialized MCP session exists, spawning (or
    /// respawning, if the previous process died) as needed. A respawn only
    /// redoes the `initialize` handshake — never full rediscovery
    /// (`tools/list`/`resources/list`/`prompts/list` only ever run once, at
    /// startup, to populate the static `ToolRegistry`) — on the assumption
    /// a given server binary's capability set is stable across its own
    /// restarts.
    pub async fn ensure_started(
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

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(cwd)
            .envs(self.env.iter().cloned());
        command = confiner.confine(command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|err| {
            ToolError::ExecutionFailed(format!(
                "MCP server \"{}\" ({}) not found or failed to start: {err}",
                self.server_name, self.program
            ))
        })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let connection = McpConnection::new(stdout, stdin);

        initialize(&connection, &self.server_name).await?;

        *guard = Some(Started {
            connection: Arc::new(connection),
            child: Some(child),
        });
        Ok(())
    }

    async fn connection(&self) -> Result<Arc<McpConnection>, ToolError> {
        let guard = self.state.lock().await;
        guard
            .as_ref()
            .map(|s| Arc::clone(&s.connection))
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "MCP server \"{}\" is not connected — call ensure_started first",
                    self.server_name
                ))
            })
    }

    pub async fn list_tools(&self) -> Result<Vec<protocol::ToolInfo>, ToolError> {
        let connection = self.connection().await?;
        let result = connection.request("tools/list", serde_json::json!({})).await?;
        let parsed: protocol::ToolsListResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed tools/list response: {err}"))
        })?;
        Ok(parsed.tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<(String, bool), ToolError> {
        let connection = self.connection().await?;
        let params = serde_json::json!({ "name": name, "arguments": arguments });
        let result = connection.request("tools/call", params).await?;
        let parsed: protocol::ToolCallResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed tools/call response: {err}"))
        })?;
        Ok((render_content_blocks(&parsed.content), parsed.is_error))
    }

    pub async fn list_resources(&self) -> Result<Vec<protocol::ResourceInfo>, ToolError> {
        let connection = self.connection().await?;
        let result = connection
            .request("resources/list", serde_json::json!({}))
            .await?;
        let parsed: protocol::ResourcesListResult =
            serde_json::from_value(result).map_err(|err| {
                ToolError::ExecutionFailed(format!("malformed resources/list response: {err}"))
            })?;
        Ok(parsed.resources)
    }

    pub async fn read_resource(&self, uri: &str) -> Result<String, ToolError> {
        let connection = self.connection().await?;
        let params = serde_json::json!({ "uri": uri });
        let result = connection.request("resources/read", params).await?;
        let parsed: protocol::ResourcesReadResult =
            serde_json::from_value(result).map_err(|err| {
                ToolError::ExecutionFailed(format!("malformed resources/read response: {err}"))
            })?;
        Ok(render_resource_contents(&parsed.contents))
    }

    pub async fn list_prompts(&self) -> Result<Vec<protocol::PromptInfo>, ToolError> {
        let connection = self.connection().await?;
        let result = connection
            .request("prompts/list", serde_json::json!({}))
            .await?;
        let parsed: protocol::PromptsListResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed prompts/list response: {err}"))
        })?;
        Ok(parsed.prompts)
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<serde_json::Value>,
    ) -> Result<String, ToolError> {
        let connection = self.connection().await?;
        let mut params = serde_json::json!({ "name": name });
        if let Some(arguments) = arguments {
            params["arguments"] = arguments;
        }
        let result = connection.request("prompts/get", params).await?;
        let parsed: protocol::PromptGetResult = serde_json::from_value(result).map_err(|err| {
            ToolError::ExecutionFailed(format!("malformed prompts/get response: {err}"))
        })?;
        Ok(render_prompt_messages(&parsed.messages))
    }
}

async fn initialize(connection: &McpConnection, server_name: &str) -> Result<(), ToolError> {
    let params = serde_json::json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "aivyx-coder", "version": env!("CARGO_PKG_VERSION") },
    });
    connection.request("initialize", params).await.map_err(|err| {
        ToolError::ExecutionFailed(format!(
            "MCP server \"{server_name}\" failed to initialize: {err}"
        ))
    })?;
    connection
        .notify("notifications/initialized", serde_json::json!({}))
        .await?;
    Ok(())
}

fn render_content_blocks(blocks: &[protocol::ContentBlock]) -> String {
    let mut rendered = String::new();
    for block in blocks {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        if block.kind == "text" {
            rendered.push_str(block.text.as_deref().unwrap_or(""));
        } else {
            rendered.push_str(&format!(
                "[non-text content of type \"{}\" omitted]",
                block.kind
            ));
        }
    }
    rendered
}

fn render_resource_contents(contents: &[protocol::ResourceContent]) -> String {
    let mut rendered = String::new();
    for content in contents {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        if let Some(text) = &content.text {
            rendered.push_str(text);
        } else if content.blob.is_some() {
            rendered.push_str(&format!(
                "[binary resource content at {} omitted]",
                content.uri
            ));
        }
    }
    rendered
}

fn render_prompt_messages(messages: &[protocol::PromptMessage]) -> String {
    messages
        .iter()
        .map(|m| {
            if m.content.kind == "text" {
                format!("{}: {}", m.role, m.content.text.clone().unwrap_or_default())
            } else {
                format!(
                    "{}: [non-text content of type \"{}\" omitted]",
                    m.role, m.content.kind
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

- [ ] **Step 5: Run to confirm it passes**

Run: `cargo test -p aivyx-tools mcp::`
Expected: PASS — 10 tests total (all of Step 2's tests).

- [ ] **Step 6: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/mcp/
git commit -m "MCP support: protocol types + McpClient spawn/handshake/discovery/respawn (Task 3)"
```

---

### Task 4: `McpToolAdapter` — MCP tool → `Tool` trait

**Files:**
- Create: `crates/aivyx-tools/src/tools/mcp_tool.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `crate::mcp::{McpClient, ToolInfo}` (Task 3).
- Produces: `pub struct McpToolAdapter` with `pub fn new(client: Arc<McpClient>, server_name: &str, tool_info: ToolInfo) -> Self`. Registered by Task 6's `main.rs` wiring under the adapter's own computed name.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/mcp_tool.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;

use crate::mcp::{McpClient, ToolInfo};
use crate::{Tool, ToolError, ToolExecutionContext};

/// Wraps one MCP tool discovered from `tools/list` on a specific server,
/// presenting it through the ordinary `Tool` trait so it's callable exactly
/// like any hand-written tool. Registered under `mcp__<server>__<tool>`.
/// Does NOT override `mutates_outside_session()` — the trait default
/// (`true`) applies, so MCP tools are hidden in Plan Mode like
/// `Write`/`Execute`/`Delete` tools, since this project can't verify what a
/// third-party server's tool actually does.
pub struct McpToolAdapter {
    client: Arc<McpClient>,
    registered_name: String,
    tool_info: ToolInfo,
}

impl McpToolAdapter {
    pub fn new(client: Arc<McpClient>, server_name: &str, tool_info: ToolInfo) -> Self {
        let registered_name = format!("mcp__{server_name}__{}", tool_info.name);
        Self {
            client,
            registered_name,
            tool_info,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.registered_name.clone(),
            description: if self.tool_info.description.is_empty() {
                format!(
                    "MCP tool \"{}\" from server \"{}\".",
                    self.tool_info.name,
                    self.client.server_name()
                )
            } else {
                self.tool_info.description.clone()
            },
            parameters_schema: self.tool_info.input_schema.clone(),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.registered_name.clone(),
            action: ActionKind::McpTool,
            target: PermissionTarget::Other(format!(
                "{} (server: {})",
                self.tool_info.name,
                self.client.server_name()
            )),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        self.client.ensure_started(&ctx.cwd, &ctx.confiner).await?;
        let (text, is_error) = self.client.call_tool(&self.tool_info.name, arguments).await?;
        if is_error {
            Ok(ToolOutput::Error(text))
        } else {
            Ok(ToolOutput::Ok(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn tool_info() -> ToolInfo {
        ToolInfo {
            name: "search_docs".to_string(),
            description: "search the docs".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    async fn server_task(
        mut server_reader: tokio::io::DuplexStream,
        mut server_writer: tokio::io::DuplexStream,
        response: serde_json::Value,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut reader = BufReader::new(&mut server_reader);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let request: serde_json::Value = match serde_json::from_str(line.trim_end()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
                continue;
            };
            if let Some(id) = request.get("id") {
                let result = if method == "tools/call" {
                    response.clone()
                } else {
                    serde_json::json!({})
                };
                let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let mut body = serde_json::to_vec(&msg).unwrap();
                body.push(b'\n');
                let _ = server_writer.write_all(&body).await;
                let _ = server_writer.flush().await;
            }
        }
    }

    async fn wired_client(response: serde_json::Value) -> Arc<crate::mcp::McpClient> {
        let (client_reader, server_writer) = tokio::io::duplex(65536);
        let (server_reader, client_writer) = tokio::io::duplex(65536);
        tokio::spawn(server_task(server_reader, server_writer, response));
        let client = crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        );
        client.wire_for_test(client_reader, client_writer).await;
        Arc::new(client)
    }

    #[test]
    fn definition_uses_the_prefixed_name_and_passes_through_the_servers_schema() {
        let client = Arc::new(crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        assert_eq!(adapter.name(), "mcp__docs__search_docs");
        let def = adapter.definition();
        assert_eq!(def.name, "mcp__docs__search_docs");
        assert_eq!(def.description, "search the docs");
        assert_eq!(def.parameters_schema, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn permission_request_always_declares_action_kind_mcp_tool() {
        let client = Arc::new(crate::mcp::McpClient::new(
            "docs".to_string(),
            "unused".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let request = adapter
            .permission_request(&serde_json::json!({"q": "rust"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::McpTool);
        assert!(matches!(request.target, PermissionTarget::Other(_)));
    }

    #[tokio::test]
    async fn execute_returns_ok_with_the_servers_text_content() {
        let client = wired_client(serde_json::json!({
            "content": [{"type": "text", "text": "found 3 matches"}],
            "isError": false
        }))
        .await;
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let dir = tempfile::tempdir().unwrap();
        let output = adapter
            .execute(serde_json::json!({"q": "rust"}), &ctx(dir.path()))
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text == "found 3 matches"));
    }

    #[tokio::test]
    async fn execute_maps_is_error_true_to_tool_output_error() {
        let client = wired_client(serde_json::json!({
            "content": [{"type": "text", "text": "no docs configured"}],
            "isError": true
        }))
        .await;
        let adapter = McpToolAdapter::new(client, "docs", tool_info());
        let dir = tempfile::tempdir().unwrap();
        let output = adapter
            .execute(serde_json::json!({"q": "rust"}), &ctx(dir.path()))
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(text) if text == "no docs configured"));
    }

    #[tokio::test]
    async fn execute_propagates_a_connection_failure_as_tool_error() {
        let client = Arc::new(crate::mcp::McpClient::new(
            "broken".to_string(),
            "definitely-not-a-real-binary-xyz".to_string(),
            vec![],
            vec![],
        ));
        let adapter = McpToolAdapter::new(client, "broken", tool_info());
        let dir = tempfile::tempdir().unwrap();
        let err = adapter
            .execute(serde_json::json!({}), &ctx(dir.path()))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools tools::mcp_tool`
Expected: FAIL to compile (the module isn't wired into `tools/mod.rs` yet).

- [ ] **Step 3: Wire the module in**

In `crates/aivyx-tools/src/tools/mod.rs`, add alphabetically:

```rust
mod mcp_tool;
```

and:

```rust
pub use mcp_tool::McpToolAdapter;
```

(keeping the existing alphabetical ordering of both the `mod` list and the `pub use` list — `mcp_tool` sorts between `grep` and `read_file`).

In `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{ ... };` block, add `McpToolAdapter` alphabetically:

```rust
pub use tools::{
    EditFileTool, FindReferencesTool, GitCommitTool, GitReadTool, GlobTool, GoToDefinitionTool,
    GrepTool, McpToolAdapter, ReadFileTool, RunCommandTool, RunShellTool, SetTasksTool,
    WebFetchTool, WebSearchTool, WriteFileTool,
};
```

Also add `pub use mcp::{McpClient, ToolInfo};` right after the existing `pub use lsp::LspClient;` line:

```rust
pub use checkpoint::GitCheckpointer;
pub use lsp::LspClient;
pub use mcp::{McpClient, ToolInfo};
pub use process::CommandSpec;
```

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools tools::mcp_tool`
Expected: PASS — 5 tests (`definition_uses_the_prefixed_name_and_passes_through_the_servers_schema`, `permission_request_always_declares_action_kind_mcp_tool`, `execute_returns_ok_with_the_servers_text_content`, `execute_maps_is_error_true_to_tool_output_error`, `execute_propagates_a_connection_failure_as_tool_error`).

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/mcp_tool.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "MCP support: McpToolAdapter (Tool trait for discovered MCP tools) (Task 4)"
```

---

### Task 5: Resources & prompts meta-tools

**Files:**
- Create: `crates/aivyx-tools/src/tools/mcp_meta.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs`
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `crate::mcp::McpClient` (Task 3), specifically `server_name()`, `list_resources()`, `read_resource()`, `list_prompts()`, `get_prompt()`.
- Produces: `pub struct ListMcpResourcesTool`, `pub struct ReadMcpResourceTool`, `pub struct ListMcpPromptsTool`, `pub struct GetMcpPromptTool` — each `pub fn new(clients: Vec<Arc<McpClient>>) -> Self`. Registered by Task 6's `main.rs` under their fixed names (`list_mcp_resources`, `read_mcp_resource`, `list_mcp_prompts`, `get_mcp_prompt`).

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/mcp_meta.rs`:

```rust
use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::mcp::McpClient;
use crate::{Tool, ToolError, ToolExecutionContext};

fn find_client<'a>(clients: &'a [Arc<McpClient>], server: &str) -> Option<&'a Arc<McpClient>> {
    clients.iter().find(|c| c.server_name() == server)
}

#[derive(Deserialize, JsonSchema)]
struct ServerFilterArgs {
    /// Restrict to one configured MCP server by name; omit to aggregate
    /// across every connected server.
    server: Option<String>,
}

/// Unlike arbitrary MCP tools (see `McpToolAdapter`), MCP's resources and
/// prompts primitives are protocol-guaranteed read-only, so all four
/// meta-tools in this file use `ActionKind::Read` and are available in
/// Plan Mode (`mutates_outside_session() == false`) — the read-only
/// guarantee comes from what a resource/prompt *is* under the MCP spec, not
/// from trusting any individual server's self-description.
pub struct ListMcpResourcesTool {
    clients: Vec<Arc<McpClient>>,
}

impl ListMcpResourcesTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &str {
        "list_mcp_resources"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "List resources exposed by connected MCP servers (uri, name, \
                description). Pass `server` to restrict to one server, or omit to aggregate \
                across all of them."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ServerFilterArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp resources".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ServerFilterArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let targets: Vec<&Arc<McpClient>> = match &args.server {
            Some(name) => match find_client(&self.clients, name) {
                Some(client) => vec![client],
                None => {
                    return Ok(ToolOutput::Error(format!(
                        "no connected MCP server named \"{name}\""
                    )));
                }
            },
            None => self.clients.iter().collect(),
        };

        let mut lines = Vec::new();
        for client in targets {
            let resources = client.list_resources().await?;
            for resource in resources {
                lines.push(format!(
                    "{} | {} | {} | {}",
                    client.server_name(),
                    resource.uri,
                    resource.name,
                    resource.description
                ));
            }
        }
        if lines.is_empty() {
            Ok(ToolOutput::Ok("no MCP resources found".to_string()))
        } else {
            Ok(ToolOutput::Ok(lines.join("\n")))
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct ReadResourceArgs {
    /// Which MCP server the URI belongs to.
    server: String,
    /// The resource URI to fetch, as returned by list_mcp_resources.
    uri: String,
}

pub struct ReadMcpResourceTool {
    clients: Vec<Arc<McpClient>>,
}

impl ReadMcpResourceTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &str {
        "read_mcp_resource"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Fetch one MCP resource's content by server name and URI (see \
                list_mcp_resources). Binary resources render as a placeholder note, not \
                decoded."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReadResourceArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp resource".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadResourceArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let Some(client) = find_client(&self.clients, &args.server) else {
            return Ok(ToolOutput::Error(format!(
                "no connected MCP server named \"{}\"",
                args.server
            )));
        };
        let text = client.read_resource(&args.uri).await?;
        Ok(ToolOutput::Ok(text))
    }
}

pub struct ListMcpPromptsTool {
    clients: Vec<Arc<McpClient>>,
}

impl ListMcpPromptsTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for ListMcpPromptsTool {
    fn name(&self) -> &str {
        "list_mcp_prompts"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "List prompt templates exposed by connected MCP servers (name, \
                description, declared arguments). Pass `server` to restrict to one server, or \
                omit to aggregate across all of them."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ServerFilterArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp prompts".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ServerFilterArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let targets: Vec<&Arc<McpClient>> = match &args.server {
            Some(name) => match find_client(&self.clients, name) {
                Some(client) => vec![client],
                None => {
                    return Ok(ToolOutput::Error(format!(
                        "no connected MCP server named \"{name}\""
                    )));
                }
            },
            None => self.clients.iter().collect(),
        };

        let mut lines = Vec::new();
        for client in targets {
            let prompts = client.list_prompts().await?;
            for prompt in prompts {
                let arg_names: Vec<&str> =
                    prompt.arguments.iter().map(|a| a.name.as_str()).collect();
                lines.push(format!(
                    "{} | {} | {} | args: [{}]",
                    client.server_name(),
                    prompt.name,
                    prompt.description,
                    arg_names.join(", ")
                ));
            }
        }
        if lines.is_empty() {
            Ok(ToolOutput::Ok("no MCP prompts found".to_string()))
        } else {
            Ok(ToolOutput::Ok(lines.join("\n")))
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct GetPromptArgs {
    /// Which MCP server the prompt belongs to.
    server: String,
    /// The prompt's name, as returned by list_mcp_prompts.
    name: String,
    /// Arguments the prompt template declares, if any.
    arguments: Option<serde_json::Value>,
}

pub struct GetMcpPromptTool {
    clients: Vec<Arc<McpClient>>,
}

impl GetMcpPromptTool {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl Tool for GetMcpPromptTool {
    fn name(&self) -> &str {
        "get_mcp_prompt"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Fetch one MCP prompt template's expanded content by server and name \
                (see list_mcp_prompts), with optional arguments. Returns the resolved messages \
                as text for you to read and use directly."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GetPromptArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other("mcp prompt".to_string()),
            arguments_preview: arguments.clone(),
            preview: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GetPromptArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let Some(client) = find_client(&self.clients, &args.server) else {
            return Ok(ToolOutput::Error(format!(
                "no connected MCP server named \"{}\"",
                args.server
            )));
        };
        let text = client.get_prompt(&args.name, args.arguments).await?;
        Ok(ToolOutput::Ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn server_task(
        mut server_reader: tokio::io::DuplexStream,
        mut server_writer: tokio::io::DuplexStream,
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut reader = BufReader::new(&mut server_reader);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let request: serde_json::Value = match serde_json::from_str(line.trim_end()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
                continue;
            };
            if let Some(id) = request.get("id") {
                let result = response_for(method);
                let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                let mut body = serde_json::to_vec(&msg).unwrap();
                body.push(b'\n');
                let _ = server_writer.write_all(&body).await;
                let _ = server_writer.flush().await;
            }
        }
    }

    async fn wired_client(
        name: &str,
        response_for: impl Fn(&str) -> serde_json::Value + Send + 'static,
    ) -> Arc<McpClient> {
        let (client_reader, server_writer) = tokio::io::duplex(65536);
        let (server_reader, client_writer) = tokio::io::duplex(65536);
        tokio::spawn(server_task(server_reader, server_writer, response_for));
        let client = McpClient::new(name.to_string(), "unused".to_string(), vec![], vec![]);
        client.wire_for_test(client_reader, client_writer).await;
        Arc::new(client)
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::path::PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn list_mcp_resources_aggregates_across_all_connected_servers() {
        let a = wired_client("a", |_| {
            serde_json::json!({"resources": [{"uri": "file:///a.txt", "name": "a", "description": "d"}]})
        })
        .await;
        let b = wired_client("b", |_| {
            serde_json::json!({"resources": [{"uri": "file:///b.txt", "name": "b", "description": "d"}]})
        })
        .await;
        let tool = ListMcpResourcesTool::new(vec![a, b]);
        let output = tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let ToolOutput::Ok(text) = output else { panic!("expected Ok") };
        assert!(text.contains("file:///a.txt"));
        assert!(text.contains("file:///b.txt"));
    }

    #[tokio::test]
    async fn list_mcp_resources_filters_to_one_server_when_given() {
        let a = wired_client("a", |_| {
            serde_json::json!({"resources": [{"uri": "file:///a.txt", "name": "a", "description": "d"}]})
        })
        .await;
        let b = wired_client("b", |_| {
            serde_json::json!({"resources": [{"uri": "file:///b.txt", "name": "b", "description": "d"}]})
        })
        .await;
        let tool = ListMcpResourcesTool::new(vec![a, b]);
        let output = tool
            .execute(serde_json::json!({"server": "a"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(text) = output else { panic!("expected Ok") };
        assert!(text.contains("file:///a.txt"));
        assert!(!text.contains("file:///b.txt"));
    }

    #[tokio::test]
    async fn list_mcp_resources_reports_ok_with_a_clear_message_when_empty() {
        let a = wired_client("a", |_| serde_json::json!({"resources": []})).await;
        let tool = ListMcpResourcesTool::new(vec![a]);
        let output = tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text == "no MCP resources found"));
    }

    #[tokio::test]
    async fn read_mcp_resource_returns_the_named_servers_content() {
        let a = wired_client("a", |_| serde_json::json!({"contents": [{"uri": "file:///a.txt", "text": "hello"}]})).await;
        let tool = ReadMcpResourceTool::new(vec![a]);
        let output = tool
            .execute(
                serde_json::json!({"server": "a", "uri": "file:///a.txt"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text.contains("hello")));
    }

    #[tokio::test]
    async fn read_mcp_resource_errors_clearly_for_an_unknown_server() {
        let a = wired_client("a", |_| serde_json::json!({})).await;
        let tool = ReadMcpResourceTool::new(vec![a]);
        let output = tool
            .execute(
                serde_json::json!({"server": "nonexistent", "uri": "file:///a.txt"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(msg) if msg.contains("nonexistent")));
    }

    #[tokio::test]
    async fn list_mcp_prompts_aggregates_across_all_connected_servers() {
        let a = wired_client("a", |_| {
            serde_json::json!({"prompts": [{"name": "p1", "description": "d", "arguments": []}]})
        })
        .await;
        let tool = ListMcpPromptsTool::new(vec![a]);
        let output = tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text.contains("p1")));
    }

    #[tokio::test]
    async fn get_mcp_prompt_returns_the_expanded_prompt_text() {
        let a = wired_client("a", |_| {
            serde_json::json!({"messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}]})
        })
        .await;
        let tool = GetMcpPromptTool::new(vec![a]);
        let output = tool
            .execute(serde_json::json!({"server": "a", "name": "p1"}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(text) if text.contains("hi")));
    }

    #[test]
    fn all_four_meta_tools_do_not_mutate_outside_session() {
        assert!(!ListMcpResourcesTool::new(vec![]).mutates_outside_session());
        assert!(!ReadMcpResourceTool::new(vec![]).mutates_outside_session());
        assert!(!ListMcpPromptsTool::new(vec![]).mutates_outside_session());
        assert!(!GetMcpPromptTool::new(vec![]).mutates_outside_session());
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p aivyx-tools tools::mcp_meta`
Expected: FAIL to compile (module not yet wired into `tools/mod.rs`).

- [ ] **Step 3: Wire the module in**

In `crates/aivyx-tools/src/tools/mod.rs`, add alphabetically (`mcp_meta` sorts before `mcp_tool`):

```rust
mod mcp_meta;
mod mcp_tool;
```

and:

```rust
pub use mcp_meta::{GetMcpPromptTool, ListMcpPromptsTool, ListMcpResourcesTool, ReadMcpResourceTool};
pub use mcp_tool::McpToolAdapter;
```

In `crates/aivyx-tools/src/lib.rs`'s `pub use tools::{ ... };` block, add the four new names alphabetically:

```rust
pub use tools::{
    EditFileTool, FindReferencesTool, GetMcpPromptTool, GitCommitTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, McpToolAdapter,
    ReadFileTool, ReadMcpResourceTool, RunCommandTool, RunShellTool, SetTasksTool, WebFetchTool,
    WebSearchTool, WriteFileTool,
};
```

- [ ] **Step 4: Run to confirm it passes**

Run: `cargo test -p aivyx-tools tools::mcp_meta`
Expected: PASS — 8 tests (`list_mcp_resources_aggregates_across_all_connected_servers`, `list_mcp_resources_filters_to_one_server_when_given`, `list_mcp_resources_reports_ok_with_a_clear_message_when_empty`, `read_mcp_resource_returns_the_named_servers_content`, `read_mcp_resource_errors_clearly_for_an_unknown_server`, `list_mcp_prompts_aggregates_across_all_connected_servers`, `get_mcp_prompt_returns_the_expanded_prompt_text`, `all_four_meta_tools_do_not_mutate_outside_session`).

- [ ] **Step 5: Run the whole crate's tests and clippy**

Run: `cargo test -p aivyx-tools`
Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: all pass, clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/tools/mcp_meta.rs crates/aivyx-tools/src/tools/mod.rs crates/aivyx-tools/src/lib.rs
git commit -m "MCP support: resources/prompts meta-tools (Task 5)"
```

---

### Task 6: `main.rs` wiring — connect, discover, register

**Files:**
- Modify: `crates/aivyx/src/main.rs`
- Modify: `crates/aivyx/Cargo.toml` (add `"time"` to the existing `tokio` feature list — needed for `tokio::time::timeout`, not currently enabled for the `aivyx` binary crate)

**Interfaces:**
- Consumes: `aivyx_tools::{McpClient, McpToolAdapter, ListMcpResourcesTool, ReadMcpResourceTool, ListMcpPromptsTool, GetMcpPromptTool}` (Tasks 3–5), `settings.mcp.servers: Vec<aivyx_config::McpServerConfig>` (Task 1).
- Produces: nothing further downstream — this is the final integration point.

- [ ] **Step 1: Add the `"time"` tokio feature**

In `crates/aivyx/Cargo.toml`, change:

```toml
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "signal"] }
```

to:

```toml
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "signal", "time"] }
```

- [ ] **Step 2: Extend the import list**

In `crates/aivyx/src/main.rs`, extend the existing `aivyx_tools::{...}` import to include the new types:

```rust
use aivyx_tools::{
    CommandSpec, EditFileTool, FindReferencesTool, GetMcpPromptTool, GitCheckpointer,
    GitCommitTool, GitReadTool, GlobTool, GoToDefinitionTool, GrepTool, ListMcpPromptsTool,
    ListMcpResourcesTool, LspClient, McpClient, McpToolAdapter, ReadFileTool, ReadMcpResourceTool,
    RunCommandTool, RunShellTool, SetTasksTool, ToolExecutor, ToolRegistry, WebFetchTool,
    WebSearchTool, WriteFileTool,
};
```

- [ ] **Step 3: Add the discovery + registration block**

In `crates/aivyx/src/main.rs`, immediately after the existing web-tools registration block (right after the `if settings.web.enabled { ... }` block, which itself comes right after the `RunCommandTool` conditional block, all inside the same tool-registration section that starts with `let mut registry = ToolRegistry::new();`), add:

```rust
    // Every configured server connects concurrently, each bounded by its
    // own `timeout_secs` — a slow or broken server can't hang startup or
    // delay every other server's tools from becoming available. A server
    // that fails or times out is skipped with a one-time warning (both a
    // log line and a surfaced AgentEvent::Error, matching how a
    // context-window mismatch is reported above) rather than aborting the
    // whole session.
    let mut mcp_discovery = tokio::task::JoinSet::new();
    for server in settings.mcp.servers.clone() {
        let confiner = Arc::clone(&confiner);
        let cwd = cwd.clone();
        mcp_discovery.spawn(async move {
            let client = Arc::new(McpClient::new(
                server.name.clone(),
                server.command.clone(),
                server.args.clone(),
                server.env.clone().into_iter().collect(),
            ));
            let timeout = Duration::from_secs(server.timeout_secs);
            let outcome = tokio::time::timeout(timeout, async {
                client.ensure_started(&cwd, &confiner).await?;
                client.list_tools().await
            })
            .await;
            (server.name, client, outcome)
        });
    }

    let mut mcp_clients: Vec<Arc<McpClient>> = Vec::new();
    while let Some(joined) = mcp_discovery.join_next().await {
        let (server_name, client, outcome) =
            joined.expect("MCP discovery task panicked");
        match outcome {
            Ok(Ok(tools)) => {
                for tool_info in tools {
                    registry.register(Arc::new(McpToolAdapter::new(
                        Arc::clone(&client),
                        &server_name,
                        tool_info,
                    )));
                }
                mcp_clients.push(client);
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    server = %server_name,
                    error = %err,
                    "MCP server failed to connect/discover tools — skipping for this session"
                );
                let _ = events_tx.send(aivyx_core::AgentEvent::Error(format!(
                    "MCP server \"{server_name}\" failed to connect: {err} — its tools are \
                     unavailable this session"
                )));
            }
            Err(_) => {
                tracing::warn!(
                    server = %server_name,
                    "MCP server startup timed out — skipping for this session"
                );
                let _ = events_tx.send(aivyx_core::AgentEvent::Error(format!(
                    "MCP server \"{server_name}\" timed out during startup — its tools are \
                     unavailable this session"
                )));
            }
        }
    }

    if !mcp_clients.is_empty() {
        registry.register(Arc::new(ListMcpResourcesTool::new(mcp_clients.clone())));
        registry.register(Arc::new(ReadMcpResourceTool::new(mcp_clients.clone())));
        registry.register(Arc::new(ListMcpPromptsTool::new(mcp_clients.clone())));
        registry.register(Arc::new(GetMcpPromptTool::new(mcp_clients.clone())));
    }
```

This requires `McpServerConfig` (already `#[derive(Debug, Clone, ...)]` from Task 1) to be `Clone` for `settings.mcp.servers.clone()` and the per-server `server.clone()`-equivalent captures — already satisfied.

- [ ] **Step 4: Build**

Run: `cargo build --workspace`
Expected: clean build. If it fails on the `tokio::task::JoinSet` import, note that `JoinSet` lives at `tokio::task::JoinSet` and needs no explicit `use` beyond what's shown above (fully qualified in the block itself).

- [ ] **Step 5: Manual smoke check — disabled (empty) case**

There is no way to unit-test `main.rs`'s own wiring in isolation (matching this project's established precedent for wiring tasks — see the web-tools and LSP integration phases' own Task 5/main.rs steps). Verify by inspection and a manual run instead:

1. With `[mcp]` absent (or `servers = []`) in `config.toml`, run the built binary and confirm no `mcp__*`/`list_mcp_resources`/etc. tools appear — grep the built binary's tool list via a throwaway debug print, or trust the code path: `settings.mcp.servers` is empty ⇒ the `for server in settings.mcp.servers.clone()` loop body never runs ⇒ `mcp_clients` stays empty ⇒ the `if !mcp_clients.is_empty()` block never runs. Confirm this reasoning holds by reading the block once more after Step 3.
2. Configure one `[[mcp.servers]]` entry pointing at a nonexistent `command` (e.g. `command = "definitely-not-a-real-mcp-server-xyz"`), run the binary, and confirm: (a) the binary still starts and the TUI opens normally, (b) a warning appears (check `tracing`'s log output, or the TUI's own notice surface fed by `events_tx`), (c) every other tool (built-ins) is still registered and usable.

- [ ] **Step 6: Run the full workspace test suite and clippy**

Run: `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets`
Expected: all tests pass (no new automated tests in this task — this is a wiring-only task, matching the web-tools plan's own main.rs task), clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx/src/main.rs crates/aivyx/Cargo.toml
git commit -m "MCP support: wire server discovery + tool/meta-tool registration into main.rs (Task 6)"
```

---

### Task 7: Docs + live E2E + final check

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`

**Interfaces:**
- Consumes: the complete feature from Tasks 1–6.

- [ ] **Step 1: Live E2E through the real binary**

Using the same PTY-harness pattern as the most recent phases (a `python-pyte`-driven PTY session, verifying results via the persisted session JSON rather than raw screen text — raw ANSI-stripped screen capture has repeatedly proven misleading for this project's TUI). This step needs a real, small MCP server to test against. This plan cannot pin that server in advance — resolve whichever is actually reachable in the environment this task runs in: a lightweight reference server (e.g. one of the official `@modelcontextprotocol/server-*` packages runnable via `npx` if Node/npx is available in the environment) or a small purpose-built throwaway stdio JSON-RPC script if no suitable package is reachable (matching how the web-tools phase self-hosted a SearXNG instance via Docker when public instances were unusable).

Confirm, through the real TUI, configuring one server in `config.toml`'s `[[mcp.servers]]`:

1. A direct message asking the model to call one of that server's tools — the tool call and result appear live in the transcript, **with a confirmation modal** (matching `ActionKind::McpTool`'s always-confirm-gated tier — this is the one new tool-permission behavior this phase introduces, so seeing the modal appear is the most important thing to verify), and the returned text is real content from the server, not an error (verified via the persisted session JSON, per this project's established grading method).
2. A direct message asking the model to call `list_mcp_resources` (or `list_mcp_prompts`, if the chosen test server exposes prompts instead/as well) — the tool call and result appear live, with **no confirmation modal** (matching the meta-tools' `ActionKind::Read` auto-allow tier), and the returned results are real.
3. If the test server dies mid-session (e.g. manually kill its process, or configure a very short `timeout_secs` for a deliberately slow server and let discovery fail), confirm the session's other tools remain usable and a clear warning was surfaced — this exercises the skip-with-warning path from Task 6.

- [ ] **Step 2: Update `README.md`**

Add a new paragraph after the `web_fetch`/`web_search` paragraph, documenting: MCP client support (tools + resources + prompts, stdio only), the new `ActionKind::McpTool` always-confirm-gated tier (contrasted with the four `list_mcp_resources`/`read_mcp_resource`/`list_mcp_prompts`/`get_mcp_prompt` meta-tools' `Read`-tier auto-allow), the `mcp__<server>__<tool>` naming convention, the eager-at-startup discovery with per-server timeout and skip-with-warning behavior, the respawn-on-dead behavior (re-running only the handshake, not rediscovery), and the `[[mcp.servers]]` config section's fields and defaults.

- [ ] **Step 3: Update `ROADMAP.md`**

In the Phase 9 section, insert a "built and live-verified" paragraph (matching the style of the `AGENTS.md`/`web_fetch`/`web_search` entries immediately above it), covering: this closing out the MCP Support item explicitly queued after the tool/capability audit; the scope decision (stdio only, tools+resources+prompts all in v1, not phased); the new `ActionKind::McpTool` and why it's always confirm-gated regardless of server self-description (the core trust decision of this phase, a genuine fork decided the conservative way, unlike web_fetch/web_search's auto-allow); the four meta-tools' Read-tier treatment and why that doesn't contradict the tools decision; the eager-at-startup concurrent discovery design forced by `ToolRegistry` being a static list; the respawn-on-dead behavior mirroring the LSP client; the hand-rolled newline-delimited-JSON transport (no new dependency, mirroring the LSP client's own precedent); the test count delta; and the live E2E results.

- [ ] **Step 4: Final full-workspace check**

Run: `cargo test --workspace`
Expected: all tests pass. Confirm the actual new-test delta by comparing against the baseline recorded at the start of Task 1 (`cargo test --workspace` count immediately before Task 1's first commit) — expected approximately 30 new tests (Task 1: 4, Task 2: 3, Task 3: 10, Task 4: 5, Task 5: 8, Tasks 6–7: 0), but state the real counted number in the docs, not this estimate, the same way the web-tools phase corrected its own projected count against the real one.

Run: `cargo clippy --workspace --all-targets`
Expected: clean, no warnings.

- [ ] **Step 5: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Docs: MCP client support + live E2E verification (Phase 9)"
```
