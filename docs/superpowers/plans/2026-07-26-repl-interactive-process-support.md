# REPL / Interactive-Process Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `repl_start`/`repl_send`/`repl_stop` tools that hold a
persistent, piped-stdin/stdout child process open across multiple tool
calls, so the model can interact with a language REPL, a database/debugger
CLI, or poll a long-running dev server — something `run_command`/
`run_shell`'s one-shot-to-completion model can't do today.

**Architecture:** Three `Tool` impls in one new file
(`crates/aivyx-tools/src/tools/repl.rs`) share one piece of state
(`SharedRepl Session = Arc<tokio::sync::Mutex<Option<ReplSession>>>`), the
same "constructed once in `agent_builder.rs`, cloned into each tool"
pattern `set_tasks`'s shared task list already uses. A new
`ActionKind::Interact` lets `repl_send`/`repl_stop` skip the interactive
prompt after `repl_start`'s own `Execute`-tier approval, but is checked in
`ConfirmationGate` *after* the plan-mode/autonomous-mode denial tiers —
not alongside `Read`/`Internal`'s early auto-allow — so a session already
running when the user enters Plan mode stops accepting input immediately.

**Tech Stack:** Rust, `tokio::process` (piped stdin/stdout/stderr, no
PTY), `tokio::sync::Mutex` (already used by `mcp/mod.rs` and `lsp/mod.rs`
for the same "async work needed while holding exclusive access to a child
process" reason — holding the lock across an `.await` is required here and
is exactly what an async mutex is for, unlike the `std::sync::Mutex` this
project uses elsewhere for state that's only ever touched synchronously).

## Global Constraints

- Plain pipes, not a PTY (design decision — see the spec's Context
  section). No new dependency is needed or permitted for this.
- One process at a time. `repl_start` errors if a session is already
  running rather than replacing it.
- `repl_send`/`repl_stop` must never re-prompt the user (`ActionKind::
  Interact`, auto-allowed), but must be denied while Plan mode is active
  even if a session is already running, and denied in Autonomous mode.
- `repl_start` is hidden from the model entirely in Plan mode (trait
  default `mutates_outside_session() == true`, unchanged) and in
  Autonomous mode (add `"repl_start"` to `AUTONOMOUS_HIDDEN_TOOLS` in
  `crates/aivyx-core/src/agent/mod.rs:112`).
- Config section `[repl]`: `quiet_window_ms` (default `300`),
  `max_wait_secs` (default `10`), `idle_timeout_secs` (default `600`), all
  optional (`#[serde(default)]`, zero-config must work).
- No live process is ever persisted across `--resume` — `SessionState` is
  untouched by this plan.

---

### Task 1: `ActionKind::Interact` and its gate tier

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs` (the `ActionKind` enum, ~line
  56-91)
- Modify: `crates/aivyx-sandbox/src/confirmation.rs` (`ConfirmationGate::
  check`, ~line 218-390)
- Test: same files' `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `aivyx_sandbox::ActionKind::Interact` — a new enum variant,
  used by Task 3/4/5's `repl_send`/`repl_stop` `permission_request()`
  implementations.

- [ ] **Step 1: Write the failing gate-level tests**

Add to `crates/aivyx-sandbox/src/confirmation.rs`'s `#[cfg(test)] mod
tests` block (after `mcp_tool_actions_are_confirm_gated_not_auto_allowed`,
~line 1232):

```rust
    fn interact_request() -> PermissionRequest {
        PermissionRequest {
            tool_name: "repl_send".to_string(),
            action: ActionKind::Interact,
            target: PermissionTarget::Other("repl session".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        }
    }

    #[tokio::test]
    async fn interact_actions_auto_allow_in_act_mode_without_prompting() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Deny,
            calls: AtomicUsize::new(0),
        });
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&interact_request()).await;
        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "Interact must auto-allow without prompting"
        );
    }

    #[tokio::test]
    async fn interact_actions_are_denied_during_plan_mode() {
        // Regression test for the leak-through failure mode this tier's
        // placement specifically guards against: a session still running
        // when the user enters plan mode must stop accepting input
        // immediately, not keep auto-allowing because Interact "looks
        // like" Read/Internal.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            plan_mode,
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&interact_request()).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a plan-mode denial, got {decision:?}");
        };
        assert!(reason.contains("plan mode"));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn interact_actions_are_denied_in_autonomous_mode() {
        // FakePrompter is set to Allow to prove the denial isn't
        // accidentally coming from the prompter path — autonomous mode
        // must never reach it for an Interact action, mirroring
        // autonomous_mode_denies_mcp_tool_calls_unconditionally.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![],
            PlanMode::new(),
            autonomous_mode,
            PathBuf::from("/home/user/project"),
            false,
        );

        let decision = gate.check(&interact_request()).await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected an autonomous-mode denial, got {decision:?}");
        };
        assert!(reason.contains("autonomous"), "reason: {reason}");
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p aivyx-sandbox interact_actions -- --test-threads=1`
Expected: compile error — `ActionKind::Interact` does not exist yet.

- [ ] **Step 3: Add the `Interact` variant**

In `crates/aivyx-sandbox/src/lib.rs`, inside the `ActionKind` enum
(~line 56), add after the `Memory` variant (the last one, ending ~line 90):

```rust
    /// An already-approved interactive process (`repl_send`/`repl_stop`)
    /// continuing to talk to a process `repl_start` already put through
    /// the `Execute` tier. Auto-allowed like `Read`/`Internal` in Act
    /// mode, but — unlike them — checked *after* the plan-mode and
    /// autonomous-mode denial tiers in `ConfirmationGate::check`, not
    /// alongside them: an approval implied by an earlier `Execute` call
    /// must not let a live process keep accepting input once the user
    /// enters plan mode. Distinct from `Internal` (which is documented as
    /// "no filesystem, process, or network effect" — dishonest for a tool
    /// that writes to a live process's stdin) and from `Execute` (which
    /// would mean re-prompting on every send, defeating the point).
    Interact,
```

- [ ] **Step 4: Add the denial constant and the autonomous-mode denial branch**

In `crates/aivyx-sandbox/src/confirmation.rs`, after
`AUTONOMOUS_MEMORY_DENIAL`'s definition (~line 44), add:

```rust
/// Told to the model when a `repl_send`/`repl_stop` call reaches
/// autonomous mode. `repl_start` (the `Execute`-tier action that would
/// actually spawn the process) is already hidden from the model in
/// autonomous mode (`AUTONOMOUS_HIDDEN_TOOLS` in `aivyx-core`), so this is
/// defense in depth — the same reasoning already applied to
/// `git_commit`'s target being permanently non-cacheable as a backstop
/// even though it's also hidden.
const AUTONOMOUS_INTERACT_DENIAL: &str =
    "interacting with a REPL/process session cannot happen in autonomous mode";
```

Inside `ConfirmationGate::check`'s `if self.autonomous_mode.active() {`
block, right after the existing `ActionKind::Memory` check (~line 268-276),
add:

```rust
            if request.action == ActionKind::Interact {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: Interact call in autonomous mode"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_INTERACT_DENIAL.to_string()));
            }
```

- [ ] **Step 5: Add the post-autonomous-mode auto-allow tier**

Still in `check`, right after the `if self.autonomous_mode.active() { ...
}` block closes (its last line is `};` ~line 344, immediately before the
comment `// \`Memory\` actions never participate in the Always-Allow
cache,`), insert:

```rust
        // An already-approved REPL/process session (repl_send/repl_stop)
        // continuing to interact with a process repl_start already put
        // through the Execute tier above. Checked here — after plan-mode
        // and autonomous-mode denial, both of which already returned
        // above if active — not alongside the Read/Internal auto-allow
        // near the top of this function, which sits BEFORE plan-mode
        // specifically so a pre-plan-mode approval can't leak through it
        // (see that check's own comment). If Interact auto-allowed in
        // that same early tier, a session still running when the user
        // enters plan mode would keep silently accepting input during
        // it — exactly the leak-through failure mode that comment guards
        // against, just via a live process instead of a cached decision.
        if request.action == ActionKind::Interact {
            return PermissionDecision::Allow;
        }

```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-sandbox -- --test-threads=1`
Expected: all tests pass, including the 3 new ones and every pre-existing
test in this file (this change must not alter any existing behavior —
`Interact` is a new variant no existing code path produces).

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs crates/aivyx-sandbox/src/confirmation.rs
git commit -m "Add ActionKind::Interact with a gate tier after plan/autonomous denial"
```

---

### Task 2: `[repl]` config section

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`
- Test: same file's `#[cfg(test)] mod tests` (if present) or a new
  `#[cfg(test)]` block at the end of the `ReplSettings` definition

**Interfaces:**
- Consumes: nothing new.
- Produces: `aivyx_config::ReplSettings { quiet_window_ms: u64, max_wait_secs: u64, idle_timeout_secs: u64 }`,
  a new `pub repl: ReplSettings` field on `Settings`. Task 6 (`agent_builder.rs`)
  reads `settings.repl.quiet_window_ms` etc. and converts to `Duration`.

- [ ] **Step 1: Check the existing test pattern for a settings struct's defaults**

Run: `grep -n "mod tests" crates/aivyx-config/src/lib.rs` to confirm
whether defaults are tested inline near each struct or in one shared test
module at the end of the file, then follow that same placement for the new
test in Step 2.

- [ ] **Step 2: Write the failing test**

Add (near wherever `VerificationSettings`'s or `AutonomousSettings`'s own
default-value test lives, matching this file's existing convention):

```rust
    #[test]
    fn repl_settings_default_to_a_usable_zero_config_shape() {
        let settings = ReplSettings::default();
        assert_eq!(settings.quiet_window_ms, 300);
        assert_eq!(settings.max_wait_secs, 10);
        assert_eq!(settings.idle_timeout_secs, 600);
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p aivyx-config repl_settings_default -- --test-threads=1`
Expected: compile error — `ReplSettings` does not exist yet.

- [ ] **Step 4: Add `ReplSettings` and wire it into `Settings`**

In `crates/aivyx-config/src/lib.rs`, add `pub repl: ReplSettings,` to the
`Settings` struct (~line 51, right after `pub persona: PersonaSettings,`):

```rust
    pub persona: PersonaSettings,
    pub repl: ReplSettings,
```

Then add the new struct, following the same shape as `AutonomousSettings`
(~line 79-101):

```rust
/// REPL/interactive-process support (`repl_start`/`repl_send`/
/// `repl_stop`): timing knobs for deciding when a call has "enough"
/// output to return, and for auto-killing a forgotten session. All
/// optional with usable defaults — zero-config works out of the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplSettings {
    /// How long output must be silent (no new bytes) before `repl_send`
    /// returns, in milliseconds.
    pub quiet_window_ms: u64,
    /// Hard per-call backstop, in seconds, in case output never goes
    /// quiet (e.g. a build tool spewing output continuously).
    pub max_wait_secs: u64,
    /// Auto-kill a session with no `repl_send` activity for this long, in
    /// seconds — a safety net against a forgotten session lingering
    /// indefinitely.
    pub idle_timeout_secs: u64,
}

impl Default for ReplSettings {
    fn default() -> Self {
        Self {
            quiet_window_ms: 300,
            max_wait_secs: 10,
            idle_timeout_secs: 600,
        }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p aivyx-config -- --test-threads=1`
Expected: all tests pass, including the new one.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Add [repl] config section (quiet_window_ms/max_wait_secs/idle_timeout_secs)"
```

---

### Task 3: `repl.rs` scaffolding + `repl_start`

**Files:**
- Create: `crates/aivyx-tools/src/tools/repl.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs` (register the new
  submodule — check its exact re-export style first, see Step 1)
- Modify: `crates/aivyx-tools/src/process.rs` (widen `kill_process_group`'s
  visibility)
- Test: `crates/aivyx-tools/src/tools/repl.rs`'s own `#[cfg(test)] mod
  tests`

**Interfaces:**
- Consumes: `crate::process::MAX_OUTPUT_BYTES` (already `pub(crate)`, no
  change needed), `crate::process::kill_process_group` (widened to
  `pub(crate)` in Step 2 below), `aivyx_sandbox::{ActionKind,
  PermissionRequest, PermissionTarget}`, `crate::{Tool, ToolError,
  ToolExecutionContext}`.
- Produces (all `pub`, used by Task 4/5/6):
  - `pub type SharedReplSession = Arc<tokio::sync::Mutex<Option<ReplSession>>>;`
  - `pub fn new_shared_repl_session() -> SharedReplSession`
  - `pub struct ReplSession { .. }` (fields stay private — only the type
    name needs to be public so `SharedReplSession` can be named elsewhere)
  - `pub struct ReplStartTool { .. }` with
    `pub fn new(session: SharedReplSession, quiet_window: Duration, max_wait: Duration, idle_timeout: Duration) -> Self`
  - Private helpers `spawn_output_reader`, `wait_for_quiet`, `drain_output`,
    `format_exit_status`, `idle_watcher` — Task 4/5 reuse these directly
    (same module, not `pub`, just visible within `repl.rs`).

- [ ] **Step 1: Check how `tools/mod.rs` declares and re-exports submodules**

Run: `cat crates/aivyx-tools/src/tools/mod.rs` and note the exact pattern
(likely `mod set_tasks; pub use set_tasks::SetTasksTool;` repeated per
file) — Step 5 below must match it exactly.

- [ ] **Step 2: Widen `kill_process_group`'s visibility**

In `crates/aivyx-tools/src/process.rs` (~line 128), change:

```rust
fn kill_process_group(child: &tokio::process::Child) {
```

to:

```rust
pub(crate) fn kill_process_group(child: &tokio::process::Child) {
```

(No test needed for this alone — its existing callers in this same file
are unaffected, and Task 4/5's tests exercise it indirectly through
`repl_stop`/the idle-watcher.)

- [ ] **Step 3: Write the failing test for `repl_start`**

Create `crates/aivyx-tools/src/tools/repl.rs` with just enough to compile
the test against, by writing the test first against the full intended
API (this file does not exist yet, so this step and Step 4 below are
necessarily combined into one file — write the test module at the bottom
of the new file exactly as shown, then Step 4 fills in everything above
it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    fn short_timing() -> (Duration, Duration, Duration) {
        // (quiet_window, max_wait, idle_timeout) — short so tests run fast.
        (Duration::from_millis(80), Duration::from_secs(3), Duration::from_secs(120))
    }

    /// A deterministic fake "REPL": reads lines from stdin, echoes each
    /// back prefixed "echo: ", and exits with status 7 if sent "quit" —
    /// available everywhere via `sh`, no reliance on python/node being
    /// installed.
    fn fake_repl_args() -> (String, Vec<String>) {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "while IFS= read -r line; do \
                     if [ \"$line\" = \"quit\" ]; then exit 7; fi; \
                     echo \"echo: $line\"; \
                 done"
                    .to_string(),
            ],
        )
    }

    #[tokio::test]
    async fn repl_start_reports_the_program_and_any_immediate_output() {
        let (quiet_window, max_wait, idle_timeout) = short_timing();
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(session, quiet_window, max_wait, idle_timeout);
        let (program, args) = fake_repl_args();

        let output = tool
            .execute(serde_json::json!({ "program": program, "args": args }), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(text.contains("sh"));
    }

    #[tokio::test]
    async fn repl_start_errors_if_a_session_is_already_running() {
        let (quiet_window, max_wait, idle_timeout) = short_timing();
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(session, quiet_window, max_wait, idle_timeout);
        let (program, args) = fake_repl_args();
        let args_json = serde_json::json!({ "program": program, "args": args });

        tool.execute(args_json.clone(), &ctx()).await.unwrap();
        let second = tool.execute(args_json, &ctx()).await.unwrap();

        let aivyx_types::ToolOutput::Error(text) = second else {
            panic!("expected Error output for a double start");
        };
        assert!(text.contains("already running"));
    }

    #[tokio::test]
    async fn permission_request_is_execute_with_a_command_target() {
        let session = new_shared_repl_session();
        let tool = ReplStartTool::new(
            session,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(1),
        );
        let request = tool
            .permission_request(
                &serde_json::json!({ "program": "python3", "args": ["-i"] }),
                std::path::Path::new("."),
            )
            .unwrap();
        assert_eq!(request.action, ActionKind::Execute);
        assert_eq!(
            request.target,
            PermissionTarget::Command {
                program: "python3".to_string(),
                args: vec!["-i".to_string()],
            }
        );
    }

    #[test]
    fn repl_start_mutates_outside_session_by_default() {
        // Trait default (true), unchanged — hidden in Plan mode like every
        // other mutating tool. Not overridden anywhere in this file.
        let tool = ReplStartTool::new(
            new_shared_repl_session(),
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(1),
        );
        assert!(tool.mutates_outside_session());
    }

    #[tokio::test]
    async fn an_idle_session_is_auto_killed_after_the_configured_timeout() {
        let session = new_shared_repl_session();
        // idle_timeout shorter than the watcher's 1s poll interval isn't
        // meaningful — use 1s idle_timeout so the watcher's very first
        // wake-up already sees it expired, and poll (well past 1s) for the
        // session to clear.
        let tool = ReplStartTool::new(
            Arc::clone(&session),
            Duration::from_millis(50),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        let (program, args) = fake_repl_args();
        tool.execute(serde_json::json!({ "program": program, "args": args }), &ctx())
            .await
            .unwrap();

        assert!(session.lock().await.is_some(), "session should be running right after start");

        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            session.lock().await.is_none(),
            "session should have been auto-killed by the idle watcher"
        );
    }

    #[tokio::test]
    async fn dropping_a_repl_session_kills_the_process_group() {
        // Constructs a `ReplSession` directly (this test lives inside
        // `repl`'s own `mod tests`, so private fields are visible) rather
        // than through `ReplStartTool`, so dropping it here isn't
        // entangled with the shared `Arc` other tool instances also hold
        // a clone of.
        let (program, args) = fake_repl_args();
        let mut command = tokio::process::Command::new(&program);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let pid = child.id().unwrap() as i32;

        let session = ReplSession {
            child,
            stdin,
            output: Arc::new(std::sync::Mutex::new(VecDeque::new())),
            last_activity: Instant::now(),
            program,
            args,
        };
        drop(session);

        tokio::time::sleep(Duration::from_millis(200)).await;

        // Signal 0: existence check only, no signal actually delivered.
        // A non-zero return (ESRCH) is expected once SIGKILL has taken
        // effect and the process is gone.
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        assert!(!alive, "process should have been killed when ReplSession was dropped");
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail to compile**

Run: `cargo test -p aivyx-tools repl:: -- --test-threads=1`
Expected: compile error — the module doesn't exist / none of the types
are defined yet.

- [ ] **Step 5: Implement the scaffolding and `ReplStartTool`**

At the top of `crates/aivyx-tools/src/tools/repl.rs` (above the test
module written in Step 3), write:

```rust
use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;

use crate::{Tool, ToolError, ToolExecutionContext};

/// Shared, process-lifetime-scoped slot for the one REPL session this
/// project supports at a time (see the design spec's "Concurrency"
/// decision). `tokio::sync::Mutex`, not `std::sync::Mutex` — every tool in
/// this file needs to hold the lock across `.await` points (writing to
/// stdin, reaping the child on stop/idle-timeout), which is exactly what
/// `mcp/mod.rs` and `lsp/mod.rs` already use `tokio::sync::Mutex` for.
pub type SharedReplSession = Arc<AsyncMutex<Option<ReplSession>>>;

/// Constructed once in `agent_builder.rs` and cloned into all three
/// `repl_*` tool instances — same pattern as `SetTasksTool`'s shared task
/// list.
pub fn new_shared_repl_session() -> SharedReplSession {
    Arc::new(AsyncMutex::new(None))
}

/// One live, persistent child process and everything needed to interact
/// with it across multiple tool calls. Never persisted (no `Serialize`) —
/// a REPL session is process-lifetime-scoped, not conversation-lifetime-
/// scoped, and does not survive `--resume`.
pub struct ReplSession {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    /// Combined stdout+stderr, continuously appended to by two background
    /// reader tasks spawned in `ReplStartTool::execute` — this is what
    /// makes polling a long-running process (no `repl_send` input, just
    /// checking for new output) work, since output keeps accumulating
    /// even between calls. A plain `std::sync::Mutex`, not the async one
    /// above: every touch of this buffer is a short, synchronous
    /// append-or-drain, never held across an `.await`.
    output: Arc<std::sync::Mutex<VecDeque<u8>>>,
    last_activity: Instant,
    program: String,
    args: Vec<String>,
}

/// Process-exit safety net: kills the process group whenever a
/// `ReplSession` value is dropped, however that happens — including
/// `aivyx-coder`'s own shutdown (Ctrl+C-to-quit or a normal quit), when
/// the `Arc`-shared state holding it is torn down along with the rest of
/// `Agent`. Deliberately NOT `Command::kill_on_drop(true)` (used by
/// `mcp/mod.rs` for its own child processes) — that only kills the direct
/// child PID, not the whole process group, so a backgrounded grandchild
/// (`npm run dev` spawning its own child watcher) would survive as an
/// orphan. `Drop::drop` is synchronous, so this can only kill, not reap
/// (`.wait()` is async) — that's fine: every code path that removes a
/// session from the shared slot on purpose (`idle_watcher`, `repl_stop`,
/// `repl_send`'s exit detection) already reaps explicitly; this impl
/// firing again afterward on the same, already-dead process group is a
/// harmless no-op (`kill` on an already-gone pid just returns `ESRCH`).
impl Drop for ReplSession {
    fn drop(&mut self) {
        crate::process::kill_process_group(&self.child);
    }
}

/// Reads `reader` to EOF (or error), appending every chunk into `output`
/// and evicting the oldest bytes past `crate::process::MAX_OUTPUT_BYTES` —
/// same tail-cap eviction technique as `process.rs`'s `drain_capped_tail`,
/// but persistent (runs for the process's whole lifetime, appending
/// across many `repl_send` calls) rather than one-shot-to-EOF, which is
/// why this is a fresh implementation rather than a call to that function.
fn spawn_output_reader(
    mut reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    output: Arc<std::sync::Mutex<VecDeque<u8>>>,
) {
    tokio::spawn(async move {
        let mut scratch = [0u8; 8192];
        loop {
            match reader.read(&mut scratch).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let mut buf = output.lock().unwrap();
                    buf.extend(scratch[..n].iter().copied());
                    if buf.len() > crate::process::MAX_OUTPUT_BYTES {
                        let excess = buf.len() - crate::process::MAX_OUTPUT_BYTES;
                        buf.drain(0..excess);
                    }
                }
            }
        }
    });
}

/// Polls `output` every 20ms, resetting a "quiet since" timer on every
/// change in length, and returns once that timer exceeds `quiet_window`
/// with nothing new — or once `max_wait` has elapsed in total regardless,
/// whichever comes first. This is how `repl_start`/`repl_send` decide
/// "the process is done producing output for this call" with no
/// per-program prompt-pattern knowledge (see the design spec's "Output
/// timing" decision).
async fn wait_for_quiet(
    output: &Arc<std::sync::Mutex<VecDeque<u8>>>,
    quiet_window: Duration,
    max_wait: Duration,
    cancellation: &tokio_util::sync::CancellationToken,
) {
    const POLL_INTERVAL: Duration = Duration::from_millis(20);
    let start = Instant::now();
    let mut last_len = output.lock().unwrap().len();
    let mut last_change = Instant::now();
    loop {
        if cancellation.is_cancelled() || start.elapsed() >= max_wait {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        let current_len = output.lock().unwrap().len();
        if current_len != last_len {
            last_len = current_len;
            last_change = Instant::now();
        } else if last_change.elapsed() >= quiet_window {
            return;
        }
    }
}

/// Takes everything currently buffered, leaving the buffer empty — a
/// "drain," never a "peek," so output is never reported twice across
/// calls.
fn drain_output(output: &Arc<std::sync::Mutex<VecDeque<u8>>>) -> String {
    let mut buf = output.lock().unwrap();
    let drained: VecDeque<u8> = std::mem::take(&mut *buf);
    String::from_utf8_lossy(&drained.into_iter().collect::<Vec<u8>>()).into_owned()
}

fn format_exit_status(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

/// Auto-kills a forgotten session — a safety net, not a limit on how long
/// a legitimate long-running process may stay useful (as long as
/// `repl_send` is called at least once per `idle_timeout`, this never
/// fires). Spawned once by `ReplStartTool::execute`, right after the new
/// session is stored (see the comment at that call site for why the
/// ordering matters). Exits on its own once the session it's watching is
/// gone, however that happened (explicit `repl_stop`, spontaneous exit
/// detected by `repl_send`/`repl_stop`, or this same watcher's own
/// idle-kill) — there is only ever one session and one watcher at a time,
/// so no generation counter is needed to tell "my session" apart from "a
/// different, later session."
async fn idle_watcher(state: SharedReplSession, idle_timeout: Duration) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut guard = state.lock().await;
        let idle = match guard.as_ref() {
            None => return,
            Some(session) => session.last_activity.elapsed() >= idle_timeout,
        };
        if !idle {
            continue;
        }
        let mut session = guard.take().expect("checked Some above");
        drop(guard);
        crate::process::kill_process_group(&session.child);
        let _ = session.child.wait().await;
        return;
    }
}

#[derive(Deserialize, JsonSchema)]
struct ReplStartArgs {
    /// The program to run, e.g. "python3", "psql", "npm".
    program: String,
    /// Arguments to pass, e.g. ["-i"] for python3's interactive flag, or
    /// ["run", "dev"] for `npm run dev`. Empty array if none needed.
    #[serde(default)]
    args: Vec<String>,
}

/// Starts a persistent, piped-stdin/stdout/stderr child process and
/// stores it in the shared slot. `ActionKind::Execute` — goes through the
/// normal gate (prompt / Always-Allow cache / pre-approved
/// `allowed_commands`), gets checkpointed (trait default
/// `mutates_outside_session() == true`, not overridden), and is hidden
/// from the model in Plan mode (same mechanism) and Autonomous mode
/// (`AUTONOMOUS_HIDDEN_TOOLS` in `aivyx-core`).
pub struct ReplStartTool {
    session: SharedReplSession,
    quiet_window: Duration,
    max_wait: Duration,
    idle_timeout: Duration,
}

impl ReplStartTool {
    pub fn new(
        session: SharedReplSession,
        quiet_window: Duration,
        max_wait: Duration,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            session,
            quiet_window,
            max_wait,
            idle_timeout,
        }
    }
}

#[async_trait]
impl Tool for ReplStartTool {
    fn name(&self) -> &str {
        "repl_start"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Start a persistent, interactive process (e.g. a language REPL like \
                `python3 -i`, a database CLI like `psql mydb`, or a long-running dev server like \
                `npm run dev`) and get any output it produces immediately (e.g. a startup banner) \
                back. Only one process may run at a time — call repl_stop before starting another. \
                Use repl_send to interact with it afterward."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReplStartArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: ReplStartArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: args.program,
                args: args.args,
            },
            arguments_preview: arguments.clone(),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReplStartArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        {
            let guard = self.session.lock().await;
            if let Some(existing) = guard.as_ref() {
                return Ok(ToolOutput::Error(format!(
                    "a REPL session is already running (`{} {}`) — call repl_stop first",
                    existing.program,
                    existing.args.join(" ")
                )));
            }
        }

        let mut command = tokio::process::Command::new(&args.program);
        command
            .args(&args.args)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut command = ctx.confiner.confine(command);
        let mut child = command.spawn().map_err(|err| {
            ToolError::ExecutionFailed(format!("failed to start `{}`: {err}", args.program))
        })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let output: Arc<std::sync::Mutex<VecDeque<u8>>> =
            Arc::new(std::sync::Mutex::new(VecDeque::new()));
        spawn_output_reader(stdout, Arc::clone(&output));
        spawn_output_reader(stderr, Arc::clone(&output));

        // Store the session BEFORE the quiet-window wait below (not
        // after): the idle watcher spawned right after this checks the
        // shared slot every 1s starting immediately, and `max_wait`
        // (default 10s) is longer than that — storing late would let the
        // watcher's very first wake-up see `None` and exit immediately,
        // permanently orphaning idle-timeout protection for this session.
        *self.session.lock().await = Some(ReplSession {
            child,
            stdin,
            output: Arc::clone(&output),
            last_activity: Instant::now(),
            program: args.program.clone(),
            args: args.args.clone(),
        });

        tokio::spawn(idle_watcher(Arc::clone(&self.session), self.idle_timeout));

        wait_for_quiet(&output, self.quiet_window, self.max_wait, &ctx.cancellation).await;
        let initial_output = drain_output(&output);

        Ok(ToolOutput::Ok(format!(
            "started `{} {}`\n{initial_output}",
            args.program,
            args.args.join(" ")
        )))
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools repl:: -- --test-threads=1`
Expected: all 6 tests in Step 3 pass. The idle-timeout test takes ~2.5s
(real time), which is expected and acceptable.

- [ ] **Step 7: Register the new submodule**

In `crates/aivyx-tools/src/tools/mod.rs`, following the exact pattern
found in Step 1, add `mod repl;` and re-export
`pub use repl::{ReplStartTool, SharedReplSession, new_shared_repl_session};`
(the other two tools' re-exports are added in Task 4/5). Also add
`pub use tools::{..., ReplStartTool, SharedReplSession, new_shared_repl_session, ...};`
to `crates/aivyx-tools/src/lib.rs`'s existing `pub use tools::{...};` list
(~line 32-38), inserted alphabetically alongside the other tool names.

- [ ] **Step 8: Run the full crate's tests to confirm nothing broke**

Run: `cargo test -p aivyx-tools -- --test-threads=1`
Expected: all tests pass (existing + new).

- [ ] **Step 9: Commit**

```bash
git add crates/aivyx-tools/src/tools/repl.rs crates/aivyx-tools/src/tools/mod.rs \
        crates/aivyx-tools/src/lib.rs crates/aivyx-tools/src/process.rs
git commit -m "Add repl_start: persistent process spawn with idle-timeout auto-cleanup"
```

---

### Task 4: `repl_send`

**Files:**
- Modify: `crates/aivyx-tools/src/tools/repl.rs` (append `ReplSendTool`
  and its tests — do not touch `ReplStartTool` or the shared scaffolding
  from Task 3)
- Modify: `crates/aivyx-tools/src/tools/mod.rs` and
  `crates/aivyx-tools/src/lib.rs` (add `ReplSendTool` to the same
  re-export lines Task 3 added)

**Interfaces:**
- Consumes: `SharedReplSession`, `ReplSession` (Task 3), `drain_output`,
  `wait_for_quiet`, `format_exit_status` (Task 3, same file).
- Produces: `pub struct ReplSendTool` with
  `pub fn new(session: SharedReplSession, quiet_window: Duration, max_wait: Duration) -> Self`,
  used by Task 6's `agent_builder.rs` wiring.

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-tools/src/tools/repl.rs`'s existing `#[cfg(test)] mod
tests` block (from Task 3), after the `ReplStartTool` tests:

```rust
    async fn started_session(
        quiet_window: Duration,
        max_wait: Duration,
    ) -> (SharedReplSession, ReplStartTool) {
        let session = new_shared_repl_session();
        let start_tool = ReplStartTool::new(
            Arc::clone(&session),
            quiet_window,
            max_wait,
            Duration::from_secs(120),
        );
        let (program, args) = fake_repl_args();
        start_tool
            .execute(serde_json::json!({ "program": program, "args": args }), &ctx())
            .await
            .unwrap();
        (session, start_tool)
    }

    #[tokio::test]
    async fn repl_send_writes_input_and_returns_the_echoed_response() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        let output = send_tool
            .execute(serde_json::json!({ "input": "hello" }), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(text.contains("echo: hello"), "got: {text}");
    }

    #[tokio::test]
    async fn repl_send_with_no_input_only_polls_without_writing() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        let output = send_tool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(
            !text.contains("echo:"),
            "a poll with no input must not trigger any echoed output, got: {text}"
        );
    }

    #[tokio::test]
    async fn repl_send_errors_when_no_session_is_running() {
        let (quiet_window, max_wait, _) = short_timing();
        let session = new_shared_repl_session();
        let send_tool = ReplSendTool::new(session, quiet_window, max_wait);

        let output = send_tool
            .execute(serde_json::json!({ "input": "hello" }), &ctx())
            .await
            .unwrap();

        let aivyx_types::ToolOutput::Error(text) = output else {
            panic!("expected Error output");
        };
        assert!(text.contains("no REPL session is running"));
    }

    #[tokio::test]
    async fn repl_send_detects_and_reports_a_spontaneous_exit() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        // "quit" makes the fake REPL exit(7) on its own.
        send_tool
            .execute(serde_json::json!({ "input": "quit" }), &ctx())
            .await
            .unwrap();
        // The exit itself races the pipe closing; a short sleep lets the
        // child's exit status become observable via try_wait() on the
        // next call.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let output = send_tool
            .execute(serde_json::json!({}), &ctx())
            .await
            .unwrap();
        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output reporting the exit");
        };
        assert!(text.contains("exited with status 7"), "got: {text}");

        assert!(
            session.lock().await.is_none(),
            "state must be cleared so repl_start works again without an explicit repl_stop"
        );
    }

    #[test]
    fn repl_send_permission_request_is_interact() {
        let tool = ReplSendTool::new(
            new_shared_repl_session(),
            Duration::from_millis(1),
            Duration::from_millis(1),
        );
        let request = tool
            .permission_request(&serde_json::json!({ "input": "x" }), std::path::Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Interact);
    }

    #[test]
    fn repl_send_does_not_mutate_outside_session() {
        // No new checkpoint per send — see the design spec's reasoning
        // (mirrors git_read overriding to false despite touching the
        // outside world in a read-only way).
        let tool = ReplSendTool::new(
            new_shared_repl_session(),
            Duration::from_millis(1),
            Duration::from_millis(1),
        );
        assert!(!tool.mutates_outside_session());
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p aivyx-tools repl_send -- --test-threads=1`
Expected: compile error — `ReplSendTool` does not exist yet.

- [ ] **Step 3: Implement `ReplSendTool`**

Append to `crates/aivyx-tools/src/tools/repl.rs`, above the `#[cfg(test)]`
module (i.e. after `ReplStartTool`'s `impl Tool for ReplStartTool` block
from Task 3):

```rust
#[derive(Deserialize, JsonSchema)]
struct ReplSendArgs {
    /// Text to send to the running process's stdin, e.g. "print(1+1)". \
    /// Omit or send an empty string to just check for new output without \
    /// sending anything (useful for polling a long-running process like a \
    /// dev server).
    #[serde(default)]
    input: Option<String>,
}

/// Sends input to (or, with no input, just polls) the running session
/// started by `repl_start`. `ActionKind::Interact` — auto-allowed, no
/// re-prompt (see the design spec's "send-gating" decision and
/// `ActionKind::Interact`'s own doc comment for why this is safe: the
/// real boundary is `repl_start`'s own `Execute`-tier approval plus
/// Landlock/seccomp confinement on the process itself, not per-line
/// review). `mutates_outside_session()` overridden to `false` — no new
/// checkpoint per send.
pub struct ReplSendTool {
    session: SharedReplSession,
    quiet_window: Duration,
    max_wait: Duration,
}

impl ReplSendTool {
    pub fn new(session: SharedReplSession, quiet_window: Duration, max_wait: Duration) -> Self {
        Self {
            session,
            quiet_window,
            max_wait,
        }
    }
}

#[async_trait]
impl Tool for ReplSendTool {
    fn name(&self) -> &str {
        "repl_send"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Send input to the process started by repl_start and get its output \
                back. Omit input (or send an empty string) to just check for new output without \
                sending anything — useful for polling a long-running process. Errors if no \
                session is running."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReplSendArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Interact,
            target: PermissionTarget::Other("repl session".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReplSendArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let mut guard = self.session.lock().await;
        let Some(session) = guard.as_mut() else {
            return Ok(ToolOutput::Error(
                "no REPL session is running — call repl_start first".to_string(),
            ));
        };

        // Opportunistic, non-blocking: if the child already exited on its
        // own since the last call, report that instead of trying to
        // interact with a dead process.
        if let Ok(Some(status)) = session.child.try_wait() {
            let final_output = drain_output(&session.output);
            *guard = None;
            return Ok(ToolOutput::Ok(format!(
                "process exited with status {}\n{final_output}",
                format_exit_status(status)
            )));
        }

        session.last_activity = Instant::now();

        if let Some(input) = args.input.as_deref().filter(|s| !s.is_empty()) {
            let mut line = input.to_string();
            line.push('\n');
            if session.stdin.write_all(line.as_bytes()).await.is_err() {
                // Most likely a broken pipe in the narrow window since the
                // try_wait() check above — the process exited right then.
                let final_output = drain_output(&session.output);
                *guard = None;
                return Ok(ToolOutput::Ok(format!(
                    "process exited (broken pipe while sending input)\n{final_output}"
                )));
            }
        }

        wait_for_quiet(&session.output, self.quiet_window, self.max_wait, &ctx.cancellation).await;
        let output = drain_output(&session.output);
        Ok(ToolOutput::Ok(output))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools repl -- --test-threads=1`
Expected: all `repl::` tests pass, including Task 3's.

- [ ] **Step 5: Add the re-export**

In `crates/aivyx-tools/src/tools/mod.rs`, extend the `pub use repl::{...}`
line from Task 3 to include `ReplSendTool`. In `crates/aivyx-tools/src/
lib.rs`'s `pub use tools::{...}` list, add `ReplSendTool` alongside
`ReplStartTool`.

- [ ] **Step 6: Run the full crate's tests to confirm nothing broke**

Run: `cargo test -p aivyx-tools -- --test-threads=1`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/tools/repl.rs crates/aivyx-tools/src/tools/mod.rs \
        crates/aivyx-tools/src/lib.rs
git commit -m "Add repl_send: write-and-read with quiet-window output collection"
```

---

### Task 5: `repl_stop`

**Files:**
- Modify: `crates/aivyx-tools/src/tools/repl.rs` (append `ReplStopTool`
  and its tests)
- Modify: `crates/aivyx-tools/src/tools/mod.rs` and
  `crates/aivyx-tools/src/lib.rs` (add `ReplStopTool` to the same
  re-export lines)

**Interfaces:**
- Consumes: `SharedReplSession`, `drain_output`, `format_exit_status`,
  `crate::process::kill_process_group` (Task 3, same file/crate).
- Produces: `pub struct ReplStopTool` with
  `pub fn new(session: SharedReplSession) -> Self`, used by Task 6's
  `agent_builder.rs` wiring.

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-tools/src/tools/repl.rs`'s test module, after the
`ReplSendTool` tests:

```rust
    #[tokio::test]
    async fn repl_stop_kills_the_process_and_a_subsequent_send_errors() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let stop_tool = ReplStopTool::new(Arc::clone(&session));
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);

        let output = stop_tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(text.contains("stopped"), "got: {text}");
        assert!(session.lock().await.is_none());

        let after = send_tool
            .execute(serde_json::json!({ "input": "hi" }), &ctx())
            .await
            .unwrap();
        let aivyx_types::ToolOutput::Error(text) = after else {
            panic!("expected Error output after stop");
        };
        assert!(text.contains("no REPL session is running"));
    }

    #[tokio::test]
    async fn repl_stop_errors_when_no_session_is_running() {
        let stop_tool = ReplStopTool::new(new_shared_repl_session());
        let output = stop_tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Error(text) = output else {
            panic!("expected Error output");
        };
        assert!(text.contains("no REPL session is running"));
    }

    #[tokio::test]
    async fn repl_stop_reports_an_already_exited_process_instead_of_pretending_to_kill_it() {
        let (quiet_window, max_wait, _) = short_timing();
        let (session, _start) = started_session(quiet_window, max_wait).await;
        let send_tool = ReplSendTool::new(Arc::clone(&session), quiet_window, max_wait);
        let stop_tool = ReplStopTool::new(Arc::clone(&session));

        // Re-inject a session snapshot manually is unnecessary here: send
        // "quit" to make the fake REPL exit on its own, but WITHOUT
        // calling repl_send again afterward (which would already clear
        // state) — call repl_stop directly while the exited-but-not-yet-
        // observed child is still sitting in the shared slot.
        {
            let mut guard = session.lock().await;
            let s = guard.as_mut().unwrap();
            use tokio::io::AsyncWriteExt;
            s.stdin.write_all(b"quit\n").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        let output = stop_tool.execute(serde_json::json!({}), &ctx()).await.unwrap();
        let aivyx_types::ToolOutput::Ok(text) = output else {
            panic!("expected Ok output");
        };
        assert!(
            text.contains("already exited") && text.contains("7"),
            "got: {text}"
        );
        let _ = send_tool; // silence unused-var lint if not otherwise referenced
    }

    #[test]
    fn repl_stop_permission_request_is_interact() {
        let tool = ReplStopTool::new(new_shared_repl_session());
        let request = tool
            .permission_request(&serde_json::json!({}), std::path::Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Interact);
    }

    #[test]
    fn repl_stop_does_not_mutate_outside_session() {
        let tool = ReplStopTool::new(new_shared_repl_session());
        assert!(!tool.mutates_outside_session());
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

Run: `cargo test -p aivyx-tools repl_stop -- --test-threads=1`
Expected: compile error — `ReplStopTool` does not exist yet.

- [ ] **Step 3: Implement `ReplStopTool`**

Append to `crates/aivyx-tools/src/tools/repl.rs`, above the `#[cfg(test)]`
module, after `ReplSendTool`'s `impl Tool for ReplSendTool` block:

```rust
/// Stops the running session started by `repl_start`. `ActionKind::
/// Interact` (same reasoning as `ReplSendTool` — no re-prompt to stop
/// something already approved). `mutates_outside_session()` overridden to
/// `false`, same reasoning as `ReplSendTool`.
pub struct ReplStopTool {
    session: SharedReplSession,
}

impl ReplStopTool {
    pub fn new(session: SharedReplSession) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for ReplStopTool {
    fn name(&self) -> &str {
        "repl_stop"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Stop the process started by repl_start. Errors if no session is \
                running."
                .to_string(),
            parameters_schema: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Interact,
            target: PermissionTarget::Other("repl session".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let mut guard = self.session.lock().await;
        let Some(mut session) = guard.take() else {
            return Ok(ToolOutput::Error(
                "no REPL session is running — call repl_start first".to_string(),
            ));
        };
        drop(guard);

        if let Ok(Some(status)) = session.child.try_wait() {
            let final_output = drain_output(&session.output);
            return Ok(ToolOutput::Ok(format!(
                "process had already exited with status {} before repl_stop was called\n{final_output}",
                format_exit_status(status)
            )));
        }

        crate::process::kill_process_group(&session.child);
        let _ = session.child.wait().await;
        let final_output = drain_output(&session.output);
        Ok(ToolOutput::Ok(format!("process stopped\n{final_output}")))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools repl -- --test-threads=1`
Expected: all `repl::` tests pass (Task 3, 4, and 5's).

- [ ] **Step 5: Add the re-export**

In `crates/aivyx-tools/src/tools/mod.rs` and `crates/aivyx-tools/src/
lib.rs`, extend both `pub use` lists to include `ReplStopTool` alongside
`ReplStartTool`/`ReplSendTool`.

- [ ] **Step 6: Run the full crate's tests and clippy to confirm nothing broke**

Run: `cargo test -p aivyx-tools -- --test-threads=1 && cargo clippy -p aivyx-tools --all-targets`
Expected: all tests pass, no clippy warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/tools/repl.rs crates/aivyx-tools/src/tools/mod.rs \
        crates/aivyx-tools/src/lib.rs
git commit -m "Add repl_stop: kill the running session, reporting an already-exited process"
```

---

### Task 6: Wire the three tools into the running agent

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (`AUTONOMOUS_HIDDEN_TOOLS`,
  ~line 112)
- Modify: `crates/aivyx/src/agent_builder.rs` (tool registration, ~line
  223-235)
- Modify: `crates/aivyx-tools/src/lib.rs`'s
  `plan_definitions_offer_only_session_safe_tools` test (~line 254)

**Interfaces:**
- Consumes: `aivyx_tools::{ReplStartTool, ReplSendTool, ReplStopTool,
  new_shared_repl_session}` (Task 3/4/5), `settings.repl.{quiet_window_ms,
  max_wait_secs, idle_timeout_secs}` (Task 2).
- Produces: nothing new — this task only wires existing pieces together.

- [ ] **Step 1: Add `"repl_start"` to the autonomous-hidden list**

In `crates/aivyx-core/src/agent/mod.rs` (~line 104-112), change:

```rust
const AUTONOMOUS_HIDDEN_TOOLS: &[&str] = &["run_shell", "git_commit"];
```

to:

```rust
const AUTONOMOUS_HIDDEN_TOOLS: &[&str] = &["run_shell", "git_commit", "repl_start"];
```

Update the doc comment right above it (currently describes only
`run_shell`/`git_commit`'s reasoning) to add one sentence: `repl_start`
is hidden for the same reason — an unattended session has no one to
review an interactive process's arbitrary back-and-forth, and
`ActionKind::Interact` (what `repl_send`/`repl_stop` report) is
independently denied by `ConfirmationGate` in autonomous mode as a
backstop even though `repl_start` being hidden already makes a session
unreachable there.

- [ ] **Step 2: Register the tools in `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, find the existing
`registry.register(Arc::new(RunShellTool));` line (~line 229) and, right
after it (before `registry.register(Arc::new(SetTasksTool::new(...)));`),
add:

```rust
    let repl_session = aivyx_tools::new_shared_repl_session();
    let repl_quiet_window = Duration::from_millis(settings.repl.quiet_window_ms);
    let repl_max_wait = Duration::from_secs(settings.repl.max_wait_secs);
    let repl_idle_timeout = Duration::from_secs(settings.repl.idle_timeout_secs);
    registry.register(Arc::new(aivyx_tools::ReplStartTool::new(
        Arc::clone(&repl_session),
        repl_quiet_window,
        repl_max_wait,
        repl_idle_timeout,
    )));
    registry.register(Arc::new(aivyx_tools::ReplSendTool::new(
        Arc::clone(&repl_session),
        repl_quiet_window,
        repl_max_wait,
    )));
    registry.register(Arc::new(aivyx_tools::ReplStopTool::new(repl_session)));
```

Also add `ReplStartTool, ReplSendTool, ReplStopTool, new_shared_repl_session`
to the existing `use aivyx_tools::{...}` import list at the top of the
file (~line 25-29), alongside the other tool names already imported
there (check the exact existing import block first — this project imports
tool names individually rather than via a glob).

- [ ] **Step 3: Update the plan-mode tool-list test**

In `crates/aivyx-tools/src/lib.rs`'s
`plan_definitions_offer_only_session_safe_tools` test (~line 253-283),
add the three new tools to the registered set and update both the total
count and the expected plan-mode subset:

```rust
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));
        registry.register(Arc::new(GrepTool::new(vec![])));
        registry.register(Arc::new(GlobTool::new(vec![])));
        registry.register(Arc::new(RunShellTool));
        registry.register(Arc::new(SetTasksTool::new(Arc::default())));
        registry.register(Arc::new(GitReadTool::new(vec![])));
        registry.register(Arc::new(GitCommitTool::new(vec![])));
        registry.register(Arc::new(ReplStartTool::new(
            new_shared_repl_session(),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_secs(1),
        )));
        registry.register(Arc::new(ReplSendTool::new(
            new_shared_repl_session(),
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        )));
        registry.register(Arc::new(ReplStopTool::new(new_shared_repl_session())));

        let all: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        let plan: Vec<String> = registry
            .plan_definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();

        assert_eq!(all.len(), 12);
        assert_eq!(
            plan,
            vec![
                "read_file", "grep", "glob", "set_tasks", "git_read", "repl_send", "repl_stop"
            ]
        );
```

(`repl_start` must NOT appear in `plan` — it stays mutating/hidden;
`repl_send`/`repl_stop` must appear, since their overridden
`mutates_outside_session() == false` makes them plan-mode-safe by the
same mechanism `git_read` already uses. Note each of the three tools gets
its own independent `new_shared_repl_session()` call here — the test only
checks tool *definitions*, never actually starts a process, so a shared
slot across all three isn't needed for this particular test.)

- [ ] **Step 4: Run the full workspace build and test suite**

Run: `cargo build --workspace && cargo test --workspace -- --test-threads=1`
Expected: builds cleanly, all tests pass (this touches `aivyx-core`,
`aivyx`, and `aivyx-tools`, so run the whole workspace, not just one
crate).

- [ ] **Step 5: Run clippy on the whole workspace**

Run: `cargo clippy --workspace --all-targets`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx/src/agent_builder.rs \
        crates/aivyx-tools/src/lib.rs
git commit -m "Wire repl_start/repl_send/repl_stop into the agent and hide repl_start from --auto"
```

---

### Task 7: Documentation

**Files:**
- Modify: `README.md` (Tools table, Configuration reference, Known
  limitations)

**Interfaces:**
- Consumes: nothing (pure documentation, no code dependency).
- Produces: nothing consumed by a later task — this is the last task.

- [ ] **Step 1: Add the three new rows to the Tools table**

In `README.md`'s `## Tools` table (~line 679-698), add three rows after
the existing `mcp__<server>__<tool>` row (the table's last row):

```markdown
| `repl_start` | start a persistent process (e.g. a language REPL, `psql`, or a dev server) | prompt (then cacheable) |
| `repl_send` | send input to / poll output from the running process | none (auto-allowed once started) |
| `repl_stop` | stop the running process | none (auto-allowed once started) |
```

- [ ] **Step 2: Add the `[repl]` config block**

In `README.md`'s `## Configuration reference` TOML block (~line 900-904,
right after the commented-out `[verification]` block and before the
`/council` comment), add:

```toml
# REPL/interactive-process support (repl_start/repl_send/repl_stop):
# timing knobs for deciding when a call has "enough" output to return,
# and for auto-killing a forgotten session. All optional — zero-config
# works out of the box with the defaults shown.
# [repl]
# quiet_window_ms = 300     # how long output must be silent before repl_send returns
# max_wait_secs = 10        # hard per-call backstop, in case output never goes quiet
# idle_timeout_secs = 600   # auto-kill a session with no repl_send activity for this long
```

- [ ] **Step 3: Add the no-PTY Known Limitations bullet**

In `README.md`'s `## Known limitations` section, add a new bullet
(placement: anywhere in the existing bulleted list is fine, e.g. right
after the `AIVYX_DEBUG_LOG` bullet):

```markdown
- **`repl_start`/`repl_send` use plain pipes, not a real pseudo-terminal
  (PTY)**: a program that checks `isatty()` may behave differently than it
  would in a real terminal — disabled line-editing/readline history, no
  color, or in the worst case refusing to run non-interactively at all.
```

- [ ] **Step 4: Verify the README changes render sensibly**

Run: `grep -n "repl_start\|repl_send\|repl_stop" README.md` and read the
three touched sections back to confirm no stray formatting broke (no
build/test command applies to a docs-only change).

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "Document repl_start/repl_send/repl_stop: tools table, config, known limitations"
```

---

## After this plan

Per the design spec's "Testing / verification" section, do a manual live
E2E verification against the real bare-metal rig once all 7 tasks are
merged: drive a real `python3 -i` (or similar) through the actual release
binary, confirming a real back-and-forth exchange works, the process is
cleaned up on `repl_stop`, and — since `ActionKind::Interact` is the
newest and most novel trust-tier addition — that the Plan-mode
leak-through guard actually holds live (start a session, enter Plan mode
mid-conversation via Ctrl+P, confirm `repl_send` is denied with a
plan-mode reason). This is not a plan task since it needs a running local
LLM and cannot be automated in CI.
