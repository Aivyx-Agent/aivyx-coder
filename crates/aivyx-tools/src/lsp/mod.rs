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
}
