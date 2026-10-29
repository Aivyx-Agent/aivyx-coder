//! The binary in headless `--auto` mode against a wiremock server speaking
//! the OpenAI streaming format: exit codes, stderr progress, stdout notice,
//! and the `--report` fields.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// One SSE body: `chunks` as `data:` lines, then `[DONE]`.
fn sse(chunks: &[serde_json::Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str(&format!("data: {chunk}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn usage(prompt: u32, completion: u32) -> serde_json::Value {
    serde_json::json!({"choices": [], "usage": {
        "prompt_tokens": prompt, "completion_tokens": completion
    }})
}

fn text_reply(text: &str) -> String {
    sse(&[
        serde_json::json!({"choices": [{"delta": {"content": text}, "finish_reason": null}]}),
        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        usage(100, 10),
    ])
}

fn tool_reply(name: &str, arguments: serde_json::Value) -> String {
    sse(&[
        serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0, "id": "call_1",
            "function": {"name": name, "arguments": arguments.to_string()}
        }]}, "finish_reason": null}]}),
        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        usage(100, 10),
    ])
}

/// Serves `first` once, then `rest` for every later chat request.
async fn model_server(first: String, rest: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(first, "text/event-stream"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(rest, "text/event-stream"))
        .with_priority(2)
        .mount(&server)
        .await;
    server
}

/// A config dir and project dir for one run against `server`.
fn setup(server: &MockServer, max_iterations: u32) -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    // --auto requires a git worktree (checkpoints). Isolated like the run itself.
    let git = |args: &[&str]| {
        let mut cmd = Command::new("git");
        isolate(&mut cmd, home.path());
        let out = cmd
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
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
            r#"[backend]
base_url = "{}/v1"
model = "fake-model"
context_tokens = 32768

[[permissions.allowed_commands]]
name = "test"
program = "true"
args = []

[[permissions.allowed_commands]]
name = "slow"
program = "sleep"
args = ["60"]

[sandbox]
require_enforcement = false

[verification]
command = "test"

[autonomous]
max_iterations = {max_iterations}
max_duration_secs = 120
"#,
            server.uri()
        ),
    )
    .unwrap();
    (home, project)
}

/// Points every per-user location at `home`, so a developer's real
/// `~/.config` or `~/.local` can never affect a run.
fn isolate(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"));
}

fn coder(home: &Path, project: &Path, report: &Path, extra: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aivyx-coder"));
    isolate(&mut cmd, home);
    cmd.current_dir(project)
        .args(["--auto", "Do the task.", "--report"])
        .arg(report)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Runs `cmd` to completion, killing it and failing the test after 60 s.
fn run_bounded(cmd: &mut Command) -> std::process::Output {
    let child = cmd.spawn().unwrap();
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
            panic!("the binary did not exit within 60 s");
        }
    }
}

fn read_report(report: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_exits_0_with_the_report() {
    let done = serde_json::json!({"tasks": [{"text": "do the task", "status": "done"}]});
    let server = model_server(tool_reply("set_tasks", done), text_reply("Done.")).await;
    let (home, project) = setup(&server, 5);
    let report = home.path().join("run.json");
    // Not --headless: piped stdout alone selects headless mode.
    let output = run_bounded(&mut coder(home.path(), project.path(), &report, &[]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");
    let notice = "autonomous run stopped: goal achieved after 1 iteration(s)";
    assert!(stdout.contains(notice), "{stdout}");
    assert!(stderr.contains("] iteration 1"), "{stderr}");
    assert!(stderr.contains("] tool set_tasks"), "{stderr}");
    assert!(stderr.contains("] tasks: 1/1 done"), "{stderr}");
    let json = read_report(&report);
    assert_eq!(json["report_version"], 1);
    assert_eq!(json["outcome"], "goal_achieved");
    assert_eq!(json["model"], "fake-model");
    assert_eq!(json["model_requests"], 2);
    assert_eq!(json["prompt_tokens"], 200);
    assert_eq!(json["coder_version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_never_finishes_exits_2() {
    let server = model_server(text_reply("Working on it."), text_reply("Still working.")).await;
    let (home, project) = setup(&server, 2);
    let report = home.path().join("run.json");
    let output = run_bounded(&mut coder(home.path(), project.path(), &report, &["--headless"]));
    assert_eq!(output.status.code(), Some(2), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(read_report(&report)["outcome"], "budget_exhausted");
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_stops_the_run_and_still_writes_the_report() {
    let server = model_server(
        tool_reply("run_command", serde_json::json!({"command": "slow"})),
        text_reply("ok"),
    )
    .await;
    let (home, project) = setup(&server, 5);
    let report = home.path().join("run.json");
    let mut child = coder(home.path(), project.path(), &report, &["--headless"]).spawn().unwrap();
    // Wait until the slow command is running, then send SIGTERM.
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "stderr closed before the command ran");
        if line.contains("tool run_command slow") {
            break;
        }
        assert!(Instant::now() < deadline, "the command never started");
    }
    std::thread::sleep(Duration::from_millis(500));
    let status = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(status.success());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            panic!("the run should stop well before sleep 60 ends");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(started.elapsed() < Duration::from_secs(20), "should stop well before sleep 60 ends");
    assert_eq!(status.code(), Some(130));
    assert_eq!(read_report(&report)["outcome"], "cancelled");
}

/// In headless mode an error that stops the run from starting must exit 3
/// ("an error"); status 1 is reserved for "goal achieved, a check failed".
#[test]
fn headless_setup_error_exits_3() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("config/aivyx-coder");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), "this is [not valid toml").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aivyx-coder"));
    isolate(&mut cmd, home.path());
    cmd.args(["--auto", "x", "--headless"])
        .current_dir(cwd.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = run_bounded(&mut cmd);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(stderr.contains("Error:"), "stderr: {stderr}");
}

// ---- audit 2026-10-28 ----

/// C7: headless `--auto` ends in `process::exit`, which skips destructors.
/// The confiner's private temp directory ("lives until aivyx-coder exits
/// normally") is therefore left behind after every run.
#[tokio::test(flavor = "multi_thread")]
async fn audit_c7_a_finished_headless_run_leaves_no_temp_dir_behind() {
    let done = serde_json::json!({"tasks": [{"text": "do the task", "status": "done"}]});
    let server = model_server(tool_reply("set_tasks", done), text_reply("Done.")).await;
    let (home, project) = setup(&server, 5);
    let report = home.path().join("run.json");
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = coder(home.path(), project.path(), &report, &["--headless"]);
    cmd.env("TMPDIR", tmp.path());
    let output = run_bounded(&mut cmd);
    assert_eq!(output.status.code(), Some(0));
    let left: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(left.is_empty(), "left behind in TMPDIR: {left:?}");
}
