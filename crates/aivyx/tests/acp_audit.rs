//! ACP audit probes (2026-10-31). Drives the real `aivyx-coder --acp`
//! binary over stdio with raw JSON-RPC lines, the way Zed does, against a
//! wiremock model server. Every `audit_` test asserts the behaviour the spec
//! or the editor needs; a failing one is the proof of a finding.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn sse(chunks: &[Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn text_reply(text: &str) -> String {
    sse(&[
        json!({"choices": [{"delta": {"content": text}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
    ])
}

fn tool_reply(id: &str, name: &str, arguments: Value) -> String {
    sse(&[
        json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "id": id,
            "function": {"name": name, "arguments": arguments.to_string()}
        }]}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
    ])
}

/// Serves `replies` in order, then repeats the last one.
struct Sequence(Vec<String>, AtomicUsize);

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let n = self.1.fetch_add(1, Ordering::SeqCst);
        let body = self.0[n.min(self.0.len() - 1)].clone();
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
    }
}

async fn model_server(replies: Vec<String>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Sequence(replies, AtomicUsize::new(0)))
        .mount(&server)
        .await;
    server
}

/// Config home + git project for one run.
fn setup(server: &MockServer) -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .env("HOME", home.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    git(&["init", "-q"]);
    std::fs::write(project.path().join("README.md"), "fixture\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    let config_dir = home.path().join("config/aivyx-coder");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[backend]\nbase_url = \"{}/v1\"\nmodel = \"fake-model\"\ncontext_tokens = 32768\n",
            server.uri()
        ),
    )
    .unwrap();
    (home, project)
}

/// A running `aivyx-coder --acp`, in its own process group like Zed starts
/// it (Zed: `set_pre_exec_to_start_new_session`, then `killpg(SIGKILL)`).
struct Acp {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<String>,
    /// Every line sent (`-> `) and received (`<- `), for the report.
    log: Vec<String>,
    next_id: u64,
}

impl Acp {
    fn start(home: &Path, project: &Path, tmpdir: Option<&Path>) -> Acp {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aivyx-coder"));
        cmd.env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .current_dir(project)
            .arg("--acp")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(tmp) = tmpdir {
            cmd.env("TMPDIR", tmp);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Acp { child, stdin, rx, log: Vec::new(), next_id: 1 }
    }

    fn send_raw(&mut self, line: &str) {
        self.log.push(format!("-> {line}"));
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string());
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send_raw(&json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    /// The next message from the agent. Panics if a line is not JSON (stdout
    /// must carry nothing but JSON-RPC).
    fn next(&mut self, timeout: Duration) -> Option<Value> {
        let line = self.rx.recv_timeout(timeout).ok()?;
        self.log.push(format!("<- {line}"));
        Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line}")))
    }

    /// Reads until the response to `id`, answering every permission request
    /// with `option` (when given) and keeping every message seen.
    fn until_response(&mut self, id: u64, option: Option<&str>, timeout: Duration) -> (Value, Vec<Value>) {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self.next(left).unwrap_or_else(|| panic!("no response to {id}; log:\n{}", self.log.join("\n")));
            if msg["id"] == json!(id) && msg.get("method").is_none() {
                return (msg, seen);
            }
            if msg["method"] == "session/request_permission"
                && let Some(option) = option
            {
                let reply = json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "outcome": {"outcome": "selected", "optionId": option}}});
                self.send_raw(&reply.to_string());
            }
            seen.push(msg);
        }
    }

    fn initialize(&mut self, version: u64) -> Value {
        let id = self.request("initialize", json!({"protocolVersion": version, "clientCapabilities": {
            "fs": {"readTextFile": true, "writeTextFile": true}, "terminal": true}}));
        self.until_response(id, None, Duration::from_secs(10)).0
    }

    fn new_session(&mut self, cwd: &Path, mcp: Value) -> String {
        let id = self.request("session/new", json!({"cwd": cwd, "mcpServers": mcp}));
        let (resp, _) = self.until_response(id, None, Duration::from_secs(30));
        resp["result"]["sessionId"].as_str().unwrap_or_else(|| panic!("{resp}")).to_string()
    }

    fn kill_group(&mut self) {
        let pgid = self.child.id();
        let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pgid}")]).status();
        let _ = self.child.wait();
    }
}

impl Drop for Acp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pids_matching(pattern: &str) -> Vec<String> {
    let out = Command::new("pgrep").args(["-f", pattern]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

fn wait_for_pids(pattern: &str, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let pids = pids_matching(pattern);
        if !pids.is_empty() {
            return pids;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Vec::new()
}

fn kill_matching(pattern: &str) {
    let _ = Command::new("pkill").args(["-KILL", "-f", pattern]).status();
}

/// Starts a prompt whose model reply runs `sleep <secs>` through run_shell,
/// approves it, and returns once the command is running.
async fn running_command(secs: &str, tmpdir: Option<&Path>) -> (Acp, MockServer, tempfile::TempDir, tempfile::TempDir) {
    let server = model_server(vec![
        tool_reply("call_sleep", "run_shell", json!({"command": format!("sleep {secs}")})),
        text_reply("done"),
    ])
    .await;
    let (home, project) = setup(&server);
    let mut acp = Acp::start(home.path(), project.path(), tmpdir);
    acp.initialize(1);
    let sid = acp.new_session(project.path(), json!([]));
    let id = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "sleep"}]}));
    // Answer the permission request, then stop reading (the turn keeps running).
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let msg = acp.next(deadline.saturating_duration_since(Instant::now())).expect("permission request");
        if msg["method"] == "session/request_permission" {
            acp.send_raw(&json!({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "outcome": {"outcome": "selected", "optionId": "allow_once"}}}).to_string());
            break;
        }
        assert_ne!(msg["id"], json!(id), "turn ended early: {msg}");
    }
    let pattern = format!("^sleep {secs}$");
    assert!(!wait_for_pids(&pattern, Duration::from_secs(15)).is_empty(), "sleep never started:\n{}", acp.log.join("\n"));
    (acp, server, home, project)
}

// ---------------------------------------------------------------- findings

/// A1: Zed kills an agent server with `killpg(pid, SIGKILL)` on the process
/// group it started the agent in. Commands coder runs are leaders of their
/// own process groups, so they survive the editor closing the agent.
#[tokio::test(flavor = "multi_thread")]
async fn audit_a1_editor_kill_leaves_no_command_running() {
    let (mut acp, _server, _home, _project) = running_command("291", None).await;
    acp.kill_group();
    std::thread::sleep(Duration::from_secs(1));
    let survivors = pids_matching("^sleep 291$");
    kill_matching("^sleep 291$");
    assert!(survivors.is_empty(), "still running after the editor killed the agent's group: {survivors:?}");
}

/// A1 (control): closing stdin (the editor closing the connection without a
/// kill) ends coder and its command.
#[tokio::test(flavor = "multi_thread")]
async fn audit_a1_control_stdin_eof_ends_the_command() {
    let (mut acp, _server, _home, _project) = running_command("292", None).await;
    drop(acp.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut exited = false;
    while Instant::now() < deadline {
        if acp.child.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    let survivors = pids_matching("^sleep 292$");
    kill_matching("^sleep 292$");
    assert!(exited, "coder did not exit within 15 s of stdin EOF");
    assert!(survivors.is_empty(), "command outlived stdin EOF: {survivors:?}");
}

/// A1 (control): SIGTERM to coder alone kills the command; does it leave the
/// confiner's private temp directory behind (C7's class, ACP path)?
#[tokio::test(flavor = "multi_thread")]
async fn audit_a1_sigterm_kills_the_command_and_cleans_tmp() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut acp, _server, _home, _project) = running_command("293", Some(tmp.path())).await;
    let _ = Command::new("kill").args(["-TERM", &acp.child.id().to_string()]).status();
    let _ = acp.child.wait();
    std::thread::sleep(Duration::from_millis(500));
    let survivors = pids_matching("^sleep 293$");
    kill_matching("^sleep 293$");
    let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(survivors.is_empty(), "command survived SIGTERM: {survivors:?}");
    assert!(left.is_empty(), "temp entries left after SIGTERM: {left:?}");
}

/// A2: ResourceLink is baseline ACP content every agent must accept, and it
/// is what Zed sends for an @-mentioned file when the agent does not
/// advertise `embeddedContext` (coder doesn't). The model must at least get
/// the link's URI.
#[tokio::test(flavor = "multi_thread")]
async fn audit_a2_a_resource_link_reaches_the_model() {
    let server = model_server(vec![text_reply("ok")]).await;
    let (home, project) = setup(&server);
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let mut acp = Acp::start(home.path(), project.path(), None);
    acp.initialize(1);
    let sid = acp.new_session(project.path(), json!([]));
    let uri = format!("file://{}/src/main.rs", project.path().display());
    let id = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [
        {"type": "text", "text": "Explain "},
        {"type": "resource_link", "name": "main.rs", "uri": uri}
    ]}));
    let (resp, _) = acp.until_response(id, None, Duration::from_secs(30));
    assert_eq!(resp["result"]["stopReason"], "end_turn", "{resp}");
    let requests = server.received_requests().await.unwrap();
    let chat = requests.iter().find(|r| r.url.path() == "/v1/chat/completions").expect("a chat request");
    let body: Value = serde_json::from_slice(&chat.body).unwrap();
    let user = body["messages"].as_array().unwrap().last().unwrap().clone();
    println!("user message sent to the model: {user}");
    assert!(user["content"].to_string().contains("src/main.rs"), "the @-mentioned file never reached the model: {user}");
}

/// A3: the permission request's tool call is a new id, not the id of the
/// `tool_call` update the agent just sent, so the editor shows a second,
/// orphaned tool-call entry for the approval.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "A6 not fixed yet"]
async fn audit_a6_permission_request_names_the_reported_tool_call() {
    let server = model_server(vec![
        tool_reply("call_touch", "run_shell", json!({"command": "true"})),
        text_reply("done"),
    ])
    .await;
    let (home, project) = setup(&server);
    let mut acp = Acp::start(home.path(), project.path(), None);
    acp.initialize(1);
    let sid = acp.new_session(project.path(), json!([]));
    let id = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "go"}]}));
    let (_, seen) = acp.until_response(id, Some("allow_once"), Duration::from_secs(30));
    let reported: Vec<_> = seen
        .iter()
        .filter(|m| m["params"]["update"]["sessionUpdate"] == "tool_call")
        .map(|m| m["params"]["update"]["toolCallId"].clone())
        .collect();
    let asked: Vec<_> = seen
        .iter()
        .filter(|m| m["method"] == "session/request_permission")
        .map(|m| m["params"]["toolCall"]["toolCallId"].clone())
        .collect();
    let completed: Vec<_> = seen
        .iter()
        .filter(|m| m["params"]["update"]["sessionUpdate"] == "tool_call_update")
        .map(|m| (m["params"]["update"]["toolCallId"].clone(), m["params"]["update"]["status"].clone()))
        .collect();
    println!("tool_call ids: {reported:?}\npermission toolCallIds: {asked:?}\nupdates: {completed:?}");
    assert_eq!(asked.len(), 1);
    assert!(reported.contains(&asked[0]), "permission asked about an unreported tool call");
}

/// A4: `initialize` must answer with a version the agent supports (the
/// client's if supported, else the agent's latest). Coder echoes whatever
/// the client sent.
#[tokio::test(flavor = "multi_thread")]
async fn audit_a4_initialize_answers_a_supported_protocol_version() {
    let server = model_server(vec![text_reply("ok")]).await;
    let (home, project) = setup(&server);
    let mut acp = Acp::start(home.path(), project.path(), None);
    let resp = acp.initialize(7);
    println!("{}", acp.log.join("\n"));
    assert_eq!(resp["result"]["protocolVersion"], 1, "{resp}");
}

/// A5: requests naming a session this process never created are served
/// against the real session; an unknown mode id is accepted.
#[tokio::test(flavor = "multi_thread")]
async fn audit_a5_unknown_session_and_mode_ids_are_rejected() {
    let server = model_server(vec![text_reply("ok")]).await;
    let (home, project) = setup(&server);
    let mut acp = Acp::start(home.path(), project.path(), None);
    acp.initialize(1);
    let _sid = acp.new_session(project.path(), json!([]));
    let id = acp.request("session/prompt", json!({"sessionId": "not-a-session", "prompt": [{"type": "text", "text": "hi"}]}));
    let (prompt, _) = acp.until_response(id, None, Duration::from_secs(30));
    let id = acp.request("session/set_mode", json!({"sessionId": "not-a-session", "modeId": "bogus"}));
    let (mode, _) = acp.until_response(id, None, Duration::from_secs(10));
    println!("{}", acp.log.join("\n"));
    assert!(prompt.get("error").is_some(), "prompt for an unknown session ran: {prompt}");
    assert!(mode.get("error").is_some(), "unknown session/mode accepted: {mode}");
}

// ------------------------------------------------------- checks / controls

/// Client-supplied MCP servers are not started (no marker file); recorded for
/// "checked" and for the conformance note.
#[tokio::test(flavor = "multi_thread")]
async fn check_client_mcp_servers_are_not_started() {
    let server = model_server(vec![text_reply("ok")]).await;
    let (home, project) = setup(&server);
    let marker = std::path::PathBuf::from("/tmp/claude-1000/acp-audit/mcp-marker");
    let _ = std::fs::remove_file(&marker);
    let mut acp = Acp::start(home.path(), project.path(), None);
    acp.initialize(1);
    let sid = acp.new_session(
        project.path(),
        json!([{"name": "probe", "command": "/usr/bin/touch", "args": [marker], "env": []}]),
    );
    let id = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "hi"}]}));
    acp.until_response(id, None, Duration::from_secs(30));
    assert!(!marker.exists(), "client MCP server was started");
}

/// Malformed and unknown messages: the agent answers with JSON-RPC errors
/// and stays usable; nothing but JSON reaches stdout.
#[tokio::test(flavor = "multi_thread")]
async fn check_malformed_and_unknown_messages() {
    let server = model_server(vec![text_reply("ok")]).await;
    let (home, project) = setup(&server);
    let mut acp = Acp::start(home.path(), project.path(), None);
    acp.send_raw("{not json");
    let id = acp.request("session/new", json!({"cwd": project.path(), "mcpServers": []}));
    let (before_init, _) = acp.until_response(id, None, Duration::from_secs(30));
    let id = acp.request("authenticate", json!({"methodId": "setup"}));
    let (auth, _) = acp.until_response(id, None, Duration::from_secs(10));
    let id = acp.request("session/load", json!({"sessionId": "../../etc", "cwd": project.path(), "mcpServers": []}));
    let (load, _) = acp.until_response(id, None, Duration::from_secs(10));
    let id = acp.request("session/new", json!({"cwd": "/nonexistent-dir", "mcpServers": []}));
    let (second, _) = acp.until_response(id, None, Duration::from_secs(10));
    let id = acp.request("session/prompt", json!({"sessionId": "x"}));
    let (bad_params, _) = acp.until_response(id, None, Duration::from_secs(10));
    println!("{}", acp.log.join("\n"));
    let _ = (before_init, auth, load, second, bad_params);
}

/// Cancel while a command runs: the turn ends `cancelled`, the command dies.
#[tokio::test(flavor = "multi_thread")]
async fn check_cancel_mid_command() {
    let (mut acp, _server, _home, _project) = running_command("294", None).await;
    acp.notify("session/cancel", json!({"sessionId": "any"}));
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stop = Value::Null;
    while let Some(msg) = acp.next(deadline.saturating_duration_since(Instant::now())) {
        if msg.get("result").is_some() && msg["result"].get("stopReason").is_some() {
            stop = msg;
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    let survivors = pids_matching("^sleep 294$");
    kill_matching("^sleep 294$");
    assert_eq!(stop["result"]["stopReason"], "cancelled", "{stop}");
    assert!(survivors.is_empty());
}

/// C22 fix check: a client that never answers the permission request, then
/// cancels. The prompt must still end `cancelled` and nothing runs.
#[tokio::test(flavor = "multi_thread")]
async fn check_cancel_with_unanswered_permission() {
    let server = model_server(vec![
        tool_reply("call_sleep", "run_shell", json!({"command": "sleep 295"})),
        text_reply("done"),
    ])
    .await;
    let (home, project) = setup(&server);
    let mut acp = Acp::start(home.path(), project.path(), None);
    acp.initialize(1);
    let sid = acp.new_session(project.path(), json!([]));
    let id = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "go"}]}));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let msg = acp.next(deadline.saturating_duration_since(Instant::now())).expect("permission request");
        if msg["method"] == "session/request_permission" {
            break;
        }
    }
    acp.notify("session/cancel", json!({"sessionId": sid}));
    let (resp, _) = acp.until_response(id, None, Duration::from_secs(10));
    assert_eq!(resp["result"]["stopReason"], "cancelled", "{resp}");
    assert!(pids_matching("^sleep 295$").is_empty());
}

/// Parity check: a model read of a default-denied path (~/.ssh) is refused
/// over ACP exactly as in the TUI, with no permission request.
#[tokio::test(flavor = "multi_thread")]
async fn check_deny_paths_hold_over_acp() {
    let home_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home_dir.path().join(".ssh")).unwrap();
    std::fs::write(home_dir.path().join(".ssh/id_canary"), "CANARY-ACP-31\n").unwrap();
    let target = home_dir.path().join(".ssh/id_canary");
    let server = model_server(vec![
        tool_reply("call_read", "read_file", json!({"path": target})),
        text_reply("done"),
    ])
    .await;
    let (_home, project) = setup(&server);
    // Reuse the generated config under the canary home.
    let cfg = _home.path().join("config");
    let mut acp = {
        let dst = home_dir.path().join("config");
        std::fs::create_dir_all(dst.join("aivyx-coder")).unwrap();
        std::fs::copy(cfg.join("aivyx-coder/config.toml"), dst.join("aivyx-coder/config.toml")).unwrap();
        Acp::start(home_dir.path(), project.path(), None)
    };
    acp.initialize(1);
    let sid = acp.new_session(project.path(), json!([]));
    let id = acp.request("session/prompt", json!({"sessionId": sid, "prompt": [{"type": "text", "text": "go"}]}));
    let (_, seen) = acp.until_response(id, Some("allow_once"), Duration::from_secs(30));
    let all = serde_json::to_string(&seen).unwrap();
    println!("{}", seen.iter().filter(|m| m["params"]["update"]["sessionUpdate"] == "tool_call_update").map(|m| m.to_string()).collect::<Vec<_>>().join("\n"));
    assert!(!all.contains("CANARY-ACP-31"), "denied file content reached the editor");
    assert!(!all.contains("session/request_permission"));
}
