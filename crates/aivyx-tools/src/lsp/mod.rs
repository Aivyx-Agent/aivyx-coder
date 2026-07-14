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

    #[tokio::test]
    async fn ensure_started_reuses_an_already_healthy_session_without_respawning() {
        // Wire a *live* session (both duplex halves stay open) pointed at a
        // program that can never exist. If either call to `ensure_started`
        // incorrectly decided the session was dead and tried to respawn, it
        // would immediately fail trying to spawn the nonexistent binary —
        // so two `Ok(())`s prove the healthy session was reused both times,
        // not respawned.
        let client =
            LspClient::with_program("definitely-not-a-real-binary-xyz", Duration::from_secs(5));
        let (client_reader, _server_writer) = tokio::io::duplex(4096);
        let (_server_reader, client_writer) = tokio::io::duplex(4096);
        client.wire_for_test(client_reader, client_writer).await;

        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);

        client
            .ensure_started(Path::new("."), &confiner)
            .await
            .expect("first call should reuse the already-healthy wired session");
        client
            .ensure_started(Path::new("."), &confiner)
            .await
            .expect("second call should also reuse the already-healthy wired session");
    }

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
}
