# Verification Test-Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scope the enforced-verification fix-and-retry loop's interim reruns to just the files touched since edits became unverified, via a user-configured command template, while a mandatory full-suite run remains the final gate — closing the last item in the 2026-07-22 capability-audit backlog.

**Architecture:** A new optional `[verification] scoped_command` config field resolves (at startup, in `agent_builder.rs`) to a `CommandSpec` + confiner handle, handed to `Agent`. A new touched-paths accumulator on `Agent` collects resolved paths from every mutating file tool across the whole unverified-edits window. Each retry-loop iteration tries the scoped command first (if configured and paths are known) — executed directly, bypassing `ConfirmationGate`, since its args vary every call and the gate's exact-args Always-Allow cache can't accommodate that without defeating the point — and only runs the existing, unchanged, gate-checked full command as confirmation once the scoped run passes. Along the way, this plan also fixes a real pre-existing bug found during design: `patch_file`/`delete_file`/`move_file` currently never trigger enforced verification at all (only `edit_file`/`write_file` do), via a new dedicated constant that doesn't disturb the unrelated existing constant it currently (incorrectly) shares.

**Tech Stack:** Rust, existing `aivyx-tools::process` execution primitive (visibility widened for cross-crate reuse), no new dependencies.

## Global Constraints

- Every `cargo test` invocation MUST include `-- --test-threads=1`.
- Full spec: `docs/superpowers/specs/2026-07-28-verification-test-selection-design.md`. Four resolved decisions shape this plan: (1) interim retries use a fast scoped command, one full run is still mandatory before a batch is declared verified; (2) scoping is a user-configured command template with a `{touched_paths}` placeholder, no built-in test-framework detection; (3) the placeholder expands into multiple separate argv entries, never a joined string; (4) the scoped run bypasses `ConfirmationGate` for this one internal call — its args vary every retry, which the gate's exact-`(program, args)` Always-Allow cache cannot accommodate without either prompting every retry (interactive) or denying outright (autonomous, which never prompts).
- `{touched_paths}` is matched as an **exact, whole-argv-entry token** — an `args` entry that literally equals `"{touched_paths}"` is replaced by N entries; other entries pass through unchanged. Not partial-string interpolation.
- Touched paths are substituted **relative to `cwd`**, not absolute — matches how a human would actually write a scoped command's expected input.
- The design doc's own worked example uses `pytest` (verified empirically to accept file paths natively), not `cargo` (verified empirically that `cargo test <file-path>` matches zero tests and reports a trivial, silent "0 passed" success) — carry this distinction into any documentation written in this plan.
- Sub-agents (`delegate_task`/`DelegateTaskConfig`) are explicitly out of scope — `Agent::set_verification`'s existing signature and the sub-agent call site are not touched; scoped verification is wired in via a new, separate, additive method the parent agent's own construction calls.
- One `verify_retries` slot is consumed per retry-loop iteration regardless of whether that iteration runs one command (full only) or two (scoped then full) — no new counter, no double-counting.

---

### Task 1: Config — `scoped_command` field

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (`VerificationSettings` struct and its `Default` impl, currently lines 61-78)

**Interfaces:**
- Produces: `aivyx_config::VerificationSettings.scoped_command: Option<String>`, defaulting to `None` — consumed by `agent_builder.rs` in Task 8.

- [ ] **Step 1: Write the failing test**

Find `VerificationSettings`'s existing tests (search `crates/aivyx-config/src/lib.rs` for a `mod tests` block with a test asserting `VerificationSettings::default()` or deserializing a `[verification]` TOML snippet — follow that file's own existing test style exactly). Add:

```rust
#[test]
fn scoped_command_defaults_to_none() {
    let settings = VerificationSettings::default();
    assert_eq!(settings.scoped_command, None);
}

#[test]
fn scoped_command_deserializes_when_present() {
    let toml = r#"
        command = "test"
        scoped_command = "test_scoped"
    "#;
    let settings: VerificationSettings = toml::from_str(toml).unwrap();
    assert_eq!(settings.scoped_command, Some("test_scoped".to_string()));
}

#[test]
fn scoped_command_defaults_to_none_when_absent_from_toml() {
    let toml = r#"command = "test""#;
    let settings: VerificationSettings = toml::from_str(toml).unwrap();
    assert_eq!(settings.scoped_command, None);
}
```

(If the file's existing tests deserialize via a different helper than `toml::from_str` directly — e.g. via the full `Settings` struct — match whatever pattern is already there instead; the plan's job is the field, not the exact test-harness shape if one already exists.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-config -- --test-threads=1 scoped_command`
Expected: FAIL to compile — `no field 'scoped_command' on type 'VerificationSettings'`

- [ ] **Step 3: Add the field**

Change `VerificationSettings` (lines 61-69) to:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerificationSettings {
    pub command: Option<String>,
    /// How many (edit, re-verify) cycles may fail before the agent gives up
    /// on this round of edits and lets the turn end anyway, with a loud
    /// notice rather than silence. Clamped to a minimum of 1 by the agent.
    pub max_auto_verify_retries: u32,
    /// Another `[[permissions.allowed_commands]]` entry name, whose `args`
    /// may contain the literal token `"{touched_paths}"` — substituted at
    /// runtime with the files touched since edits became unverified, one
    /// argv entry per path. `None` (the default) means every retry always
    /// runs the full `command`, exactly as before this field existed.
    pub scoped_command: Option<String>,
}
```

Change the `Default` impl (lines 71-78) to:

```rust
impl Default for VerificationSettings {
    fn default() -> Self {
        Self {
            command: None,
            max_auto_verify_retries: 3,
            scoped_command: None,
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-config -- --test-threads=1 scoped_command`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Add [verification] scoped_command config field"
```

---

### Task 2: Widen `aivyx-tools::process::run`'s visibility for cross-crate reuse

**Files:**
- Modify: `crates/aivyx-tools/src/process.rs` (the `run` function, currently `pub(crate) async fn run` at line 47)
- Modify: `crates/aivyx-tools/src/lib.rs` (the `pub use process::CommandSpec;` line)

**Interfaces:**
- Produces: `aivyx_tools::run(command: tokio::process::Command, timeout: Duration, cancellation: CancellationToken) -> Result<ToolOutput, ToolError>` — a crate-public function, now callable from `aivyx-core` (Task 6). Its behavior is completely unchanged — this task is a visibility-only change, no logic touched.

This task has no new tests of its own — `run`'s existing behavior is already covered by `run_command.rs`'s existing test suite, which continues to pass unchanged since the function's body isn't modified. Verification here is purely "does the workspace still build and does `run_command`'s existing suite still pass."

- [ ] **Step 1: Widen visibility**

In `crates/aivyx-tools/src/process.rs`, change:

```rust
pub(crate) async fn run(
```

to:

```rust
pub async fn run(
```

(Only this one function's visibility changes — `kill_process_group`, `drain_capped_tail`, `format_output`, `format_stream`, `MAX_OUTPUT_BYTES`, and `CommandSpec` itself are untouched; `CommandSpec` is already `pub`.)

- [ ] **Step 2: Re-export it**

In `crates/aivyx-tools/src/lib.rs`, change:

```rust
pub use process::CommandSpec;
```

to:

```rust
pub use process::{CommandSpec, run};
```

- [ ] **Step 3: Verify the workspace still builds and existing tests pass**

Run: `cargo build -p aivyx-tools`
Expected: builds cleanly

Run: `cargo test -p aivyx-tools -- --test-threads=1 run_command`
Expected: PASS (all pre-existing `run_command.rs` tests, unchanged)

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-tools/src/process.rs crates/aivyx-tools/src/lib.rs
git commit -m "Export aivyx_tools::run for cross-crate reuse by the scoped-verification bypass path"
```

---

### Task 3: `VerificationKind` and `ScopedVerificationConfig` types

**Files:**
- Modify: `crates/aivyx-core/src/agent/types.rs` (`VerificationConfig`, currently lines 73-83)
- Modify: `crates/aivyx-core/Cargo.toml` (confirm `aivyx-tools` and `aivyx-sandbox` are already dependencies — they are, per existing imports elsewhere in this crate; no change expected, listed here only so the implementer checks rather than assumes)

**Interfaces:**
- Produces: `pub(crate) enum VerificationKind { Full, Scoped }` (derives `Debug, Clone, Copy, PartialEq, Eq`); `pub(crate) struct ScopedVerificationConfig { pub(crate) spec: aivyx_tools::CommandSpec, pub(crate) confiner: std::sync::Arc<dyn aivyx_sandbox::ExecutionConfiner> }` (derives `Clone`, with a manual `Debug` impl since `Arc<dyn ExecutionConfiner>` doesn't implement it); `VerificationConfig` gains `pub(crate) scoped: Option<ScopedVerificationConfig>`.
- Consumed by: Task 6 (`run_scoped_verification`, the per-kind output comparison), Task 7 (the retry-loop orchestration, `run_verification_attempt`), Task 8 (`Agent::set_scoped_verification`).

- [ ] **Step 1: Write the failing test**

Add to `crates/aivyx-core/src/agent/types.rs` (create a `#[cfg(test)] mod tests` block if one doesn't already exist in this file — check first; if `types.rs` has no tests today, add the block at the end of the file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::NoopConfiner;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn scoped_verification_config_is_cloneable_and_debug_formattable() {
        let scoped = ScopedVerificationConfig {
            spec: aivyx_tools::CommandSpec {
                name: "test_scoped".to_string(),
                program: "pytest".to_string(),
                args: vec!["{touched_paths}".to_string()],
                timeout: Duration::from_secs(30),
            },
            confiner: Arc::new(NoopConfiner),
        };
        let cloned = scoped.clone();
        assert_eq!(cloned.spec.name, "test_scoped");
        // Must not panic and must not attempt to format the confiner itself.
        let debug_text = format!("{scoped:?}");
        assert!(debug_text.contains("test_scoped"));
    }

    #[test]
    fn verification_config_carries_an_optional_scoped_config() {
        let config = VerificationConfig {
            command_name: "test".to_string(),
            max_retries: 3,
            scoped: None,
        };
        assert!(config.scoped.is_none());
    }
}
```

(`aivyx_sandbox::NoopConfiner` is a real, already-public type — `crates/aivyx-sandbox/src/lib.rs`'s `pub struct NoopConfiner; impl ExecutionConfiner for NoopConfiner { .. }` — used exactly this way throughout `crates/aivyx-core/src/agent/tests.rs`'s existing harness. Do not invent a local mock confiner; `aivyx-core`'s `Cargo.toml` already depends on `aivyx-sandbox`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-core -- --test-threads=1 scoped_verification_config`
Expected: FAIL to compile — `VerificationKind`/`ScopedVerificationConfig` don't exist, `VerificationConfig` has no `scoped` field.

- [ ] **Step 3: Add the types**

Change `VerificationConfig` (`crates/aivyx-core/src/agent/types.rs`, currently lines 73-83) to:

```rust
/// Configures the enforced verification loop (ROADMAP.md Phase 12 Part B):
/// after file edits, before a turn is allowed to end, the named
/// `allowed_commands` entry is auto-run via `run_command`.
#[derive(Debug, Clone)]
pub(crate) struct VerificationConfig {
    pub(crate) command_name: String,
    /// Clamped to a minimum of 1 by `Agent::set_verification` — a 0 here
    /// would report "still failing" without ever actually attempting a
    /// verification run.
    pub(crate) max_retries: u32,
    /// Set via `Agent::set_scoped_verification` (called separately, after
    /// `set_verification` establishes the base config this field attaches
    /// to). `None` means every retry always runs the full `command_name`,
    /// exactly as before this feature existed. See
    /// docs/superpowers/specs/2026-07-28-verification-test-selection-design.md.
    pub(crate) scoped: Option<ScopedVerificationConfig>,
}

/// Which command produced a `run_auto_verification`-style result — tags
/// `Agent::last_verification_output` so the "what's new since last
/// attempt" comparison never compares a scoped run's (small, targeted)
/// output against a full run's (large, comprehensive) one, which would
/// produce a misleading note dominated by irrelevant noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerificationKind {
    Full,
    Scoped,
}

/// The scoped-verification command and the confiner to sandbox it with —
/// bundled together since the scoped run bypasses `ToolExecutor::dispatch`
/// entirely (see the design doc's Decision 4) and must apply the same
/// confinement `dispatch` would have applied, manually.
#[derive(Clone)]
pub(crate) struct ScopedVerificationConfig {
    pub(crate) spec: aivyx_tools::CommandSpec,
    pub(crate) confiner: std::sync::Arc<dyn aivyx_sandbox::ExecutionConfiner>,
}

impl std::fmt::Debug for ScopedVerificationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedVerificationConfig")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1 scoped_verification_config`
Expected: PASS

Run: `cargo build -p aivyx-core` — this will very likely FAIL at this point, because `VerificationConfig`'s only other construction site (`Agent::set_verification`, `crates/aivyx-core/src/agent/mod.rs:329-334`) doesn't yet set the new `scoped` field. Fix it now, in this same task, so the crate builds:

Change `set_verification` (lines 329-334) to:

```rust
    pub fn set_verification(&mut self, command_name: String, max_retries: u32) {
        self.verification = Some(VerificationConfig {
            command_name,
            max_retries: max_retries.max(1),
            scoped: None,
        });
    }
```

Re-run: `cargo build -p aivyx-core`
Expected: builds cleanly

Re-run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS — the full existing `aivyx-core` suite, not just this task's new tests, since `set_verification`'s change could otherwise silently affect existing verification tests.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/agent/mod.rs
git commit -m "Add VerificationKind and ScopedVerificationConfig types"
```

---

### Task 4: Fix the pre-existing verification-trigger gap (patch_file/delete_file/move_file)

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (new constant near `PROMPTED_EDIT_HIDDEN_TOOLS` at line 78; the `is_edit_call` check at line 1472)

**Interfaces:**
- Produces: `const VERIFICATION_TRIGGER_TOOLS: &[&str]` — consumed only at the one call site this task changes.

This task is independent of the scoped-verification machinery — it's a standalone bug fix (confirmed with the user during design) that also happens to matter for Task 5's accumulator (which would otherwise be silently inert for 3 of 5 mutating file tools). It can be verified and committed on its own.

- [ ] **Step 1: Write the failing test**

`crates/aivyx-core/src/agent/tests.rs` already has an "enforced verification (Phase 12 Part B)" section (search for that exact comment) with a working harness: `verify_command_spec(name, exit_ok)` builds a trivial `sh -c "exit 0|1"` `CommandSpec`, `auto_verify_calls(history)` counts synthetic verification tool calls, and `build_agent(responses, registry, max_iters)` constructs a ready `(Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>)`. Add this test in that same section, right after `a_passing_verification_completes_the_turn_without_an_extra_round_trip` — it mirrors that test's exact shape, swapping `write_file` for `delete_file`:

```rust
#[tokio::test]
async fn delete_file_triggers_enforced_verification_same_as_edit_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("gone.txt"), "bye\n").unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::DeleteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", true,
    )])));

    let delete_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "delete_file".to_string(),
            arguments: serde_json::json!({ "path": "gone.txt" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _) =
        build_agent(vec![delete_call, text_response("done")], registry, 10);
    agent.set_verification("verify".to_string(), 3);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        auto_verify_calls(&agent.history),
        1,
        "delete_file must trigger enforced verification, same as edit_file/write_file already do"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-core -- --test-threads=1 <new_test_name>`
Expected: FAIL — verification never triggers, because `delete_file`/`patch_file` aren't in `PROMPTED_EDIT_HIDDEN_TOOLS`.

- [ ] **Step 3: Add the fix**

Add a new constant near `PROMPTED_EDIT_HIDDEN_TOOLS` (`crates/aivyx-core/src/agent/mod.rs:78`) — do **not** modify `PROMPTED_EDIT_HIDDEN_TOOLS` itself, which has an unrelated purpose (hiding `edit_file`/`write_file`'s native tool-call forms specifically while `EditFormat::Prompted` is active) that must not also start hiding `patch_file`/`delete_file`/`move_file`:

```rust
/// Tools whose successful, `Ok`-outcome call sets `unverified_edits` (Phase
/// 12 Part B) — every tool that mutates a file's content or existence.
/// Deliberately a separate list from `PROMPTED_EDIT_HIDDEN_TOOLS` above,
/// which this check used to (incorrectly) share: that constant's purpose is
/// hiding `edit_file`/`write_file`'s native tool-call forms while
/// `EditFormat::Prompted` synthesizes SEARCH/REPLACE blocks into them
/// instead — unrelated to which tools should trigger enforced verification,
/// and `patch_file`/`delete_file`/`move_file` have nothing to do with
/// SEARCH/REPLACE block synthesis. Sharing the list meant those three tools
/// never triggered verification at all until this fix.
const VERIFICATION_TRIGGER_TOOLS: &[&str] =
    &["edit_file", "write_file", "patch_file", "delete_file", "move_file"];
```

Change line 1472 from:

```rust
                let is_edit_call = PROMPTED_EDIT_HIDDEN_TOOLS.contains(&call.name.as_str());
```

to:

```rust
                let is_edit_call = VERIFICATION_TRIGGER_TOOLS.contains(&call.name.as_str());
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS — the new test, and every pre-existing test (specifically confirm any test asserting `plan_definitions`/prompted-mode tool-hiding behavior around `PROMPTED_EDIT_HIDDEN_TOOLS`'s *other* use site, line 1130, is untouched and still passes — this task must not change what's hidden in prompted mode).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Fix: patch_file/delete_file/move_file never triggered enforced verification"
```

---

### Task 5: Touched-paths accumulator

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (`Agent` struct fields ~lines 200-237; `Agent::new` ~lines 256-293; the edit-tracking loop ~lines 1467-1491; the three `unverified_edits = false` reset sites: the pass path ~lines 1348-1350, and the autonomous-rewind-success path ~line 1377 — **not** the interactive-exhaustion-without-rewind path ~line 1401, where `unverified_edits` deliberately stays `true`)

**Interfaces:**
- Produces: `Agent.verification_touched_paths: Vec<PathBuf>` (private field), a private `fn record_touched_path(&mut self, path: PathBuf)` (dedups on insert), a free function `fn touched_path_for(call: &ToolCall, cwd: &Path) -> Option<PathBuf>`. Consumed by Task 7's `run_verification_attempt` (the retry-loop orchestration reads `self.verification_touched_paths`); the pure `substitute_touched_paths` function this field's contents get passed into is added in Task 6.

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-core/src/agent/tests.rs`'s "enforced verification (Phase 12 Part B)" section, using the same `build_agent`/`verify_command_spec`/`ToolCallComplete` pattern the existing tests there already use:

Test 1 — the accumulator collects `move_file`'s **destination**, not its source. Uses a *failing* verify command with `max_retries: 1` deliberately — a passing one would immediately clear the accumulator (Step 3 below adds that clear), leaving nothing to observe; interactive-mode exhaustion-without-rewind leaves `unverified_edits` (and thus the accumulator) alone, so this is the right way to inspect the populated state directly:

```rust
#[tokio::test]
async fn move_file_contributes_its_destination_not_its_source_to_touched_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.txt"), "hi\n").unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::MoveFileTool::new(vec![])));
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", false,
    )])));

    let move_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "move_file".to_string(),
            arguments: serde_json::json!({ "from": "old.txt", "to": "new.txt" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    // Two filler no-tool-call responses after move_call: with max_retries:
    // 1, the retry-check runs after "done" (fails), then needs one more
    // response ("still trying") to trigger the second (exhausting) check —
    // matches the existing `the_very_first_verification_call_ever_has_
    // nothing_to_compare_against` test's identical script shape for the
    // same max_retries: 1.
    let (mut agent, _rx, _) = build_agent(
        vec![move_call, text_response("done"), text_response("still trying")],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 1);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let expected_dest = dir.path().join("new.txt");
    let unexpected_source = dir.path().join("old.txt");
    assert!(
        agent.verification_touched_paths.contains(&expected_dest),
        "expected {expected_dest:?} in {:?}",
        agent.verification_touched_paths
    );
    assert!(
        !agent.verification_touched_paths.contains(&unexpected_source),
        "the source path must not be tracked — it no longer exists after the move"
    );
}
```

Test 2 — the accumulator persists across multiple failed retry iterations without resetting between them. `max_retries: 2` with a always-failing verify command needs exactly 3 no-tool-call filler responses to drive it to exhaustion (one to end the tool-calling phase and trigger the first check, then one more per subsequent check — trace it against `a_failing_verification_feeds_back_and_retries_until_exhausted`'s existing script, which uses the identical N=max_retries+1 pattern, to confirm this count before assuming it):

```rust
#[tokio::test]
async fn touched_paths_accumulate_across_multiple_retry_iterations() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", false,
    )])));

    let write_a = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "a.txt", "content": "a\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let write_b = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c2".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "b.txt", "content": "b\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _) = build_agent(
        vec![
            write_a,
            write_b,
            text_response("trying"),
            text_response("still trying"),
            text_response("giving up"),
        ],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 2);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert!(agent.verification_touched_paths.contains(&dir.path().join("a.txt")));
    assert!(agent.verification_touched_paths.contains(&dir.path().join("b.txt")));
}
```

Test 3 — the accumulator is cleared once a verification run passes:

```rust
#[tokio::test]
async fn touched_paths_are_cleared_once_verification_passes() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", true,
    )])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _) =
        build_agent(vec![write_call, text_response("done")], registry, 10);
    agent.set_verification("verify".to_string(), 3);

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    assert!(
        agent.verification_touched_paths.is_empty(),
        "a passing verification must clear the accumulator, not carry stale paths into the next batch"
    );
}
```

Test 4 — `touched_path_for` as a pure unit test, independent of the full `Agent` harness:
```rust
#[test]
fn touched_path_for_uses_the_path_argument_for_ordinary_edit_tools() {
    let call = ToolCall {
        id: ToolCallId("c1".to_string()),
        name: "edit_file".to_string(),
        arguments: serde_json::json!({ "path": "src/foo.rs" }),
        source: ToolCallSource::Native,
    };
    let cwd = Path::new("/project");
    assert_eq!(touched_path_for(&call, cwd), Some(PathBuf::from("/project/src/foo.rs")));
}

#[test]
fn touched_path_for_uses_the_to_argument_for_move_file() {
    let call = ToolCall {
        id: ToolCallId("c1".to_string()),
        name: "move_file".to_string(),
        arguments: serde_json::json!({ "from": "old.rs", "to": "new.rs" }),
        source: ToolCallSource::Native,
    };
    let cwd = Path::new("/project");
    assert_eq!(touched_path_for(&call, cwd), Some(PathBuf::from("/project/new.rs")));
}

#[test]
fn touched_path_for_returns_none_for_a_call_with_no_recognized_path_argument() {
    let call = ToolCall {
        id: ToolCallId("c1".to_string()),
        name: "set_tasks".to_string(),
        arguments: serde_json::json!({ "tasks": [] }),
        source: ToolCallSource::Native,
    };
    assert_eq!(touched_path_for(&call, Path::new("/project")), None);
}
```

(Place the pure `touched_path_for` tests in whichever of `agent/mod.rs`'s own `#[cfg(test)]` inline tests or `agent/tests.rs` already hosts similar small pure-function tests like `describe_tool_call_target`'s or `new_lines_note`'s own tests, if any exist — match that location.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-core -- --test-threads=1 touched_path`
Expected: FAIL to compile — `touched_path_for`/`verification_touched_paths` don't exist yet.

- [ ] **Step 3: Add the field, the helpers, and the wiring**

Add the field to the `Agent` struct, right after `last_verification_output` (currently ending at line 229):

```rust
    /// Resolved paths (relative-to-`cwd` at substitution time, stored
    /// absolute) touched by a mutating file tool while `unverified_edits`
    /// is `true` — accumulated across the *whole* unverified-edits window,
    /// not per-retry-attempt, since a later edit made in response to a
    /// failed scoped run might need an *earlier* touched file's tests
    /// re-confirmed too, not just the newest one. Cleared only when the
    /// window closes the same way `unverified_edits` itself does: a
    /// passing final verification, or a successful autonomous-mode
    /// rewind — *not* on interactive-mode exhaustion without a rewind,
    /// where `unverified_edits` deliberately stays `true` for a future
    /// turn to keep trying. A `Vec` with dedup-on-insert (not a `HashSet`)
    /// so args built from it are in a deterministic, testable order.
    verification_touched_paths: Vec<PathBuf>,
```

Add `verification_touched_paths: Vec::new(),` to `Agent::new`'s constructor, right after `last_verification_output: None,` (line 289).

Add a private method, anywhere among `Agent`'s other `impl` methods (e.g. right after `set_verification`):

```rust
    /// Adds `path` to `verification_touched_paths` unless it's already
    /// present — the same file can legitimately be touched more than once
    /// across a multi-retry window.
    fn record_touched_path(&mut self, path: PathBuf) {
        if !self.verification_touched_paths.contains(&path) {
            self.verification_touched_paths.push(path);
        }
    }
```

Add the free function near `describe_tool_call_target` (`crates/aivyx-core/src/agent/mod.rs:1667`):

```rust
/// The path a successful `VERIFICATION_TRIGGER_TOOLS` call should
/// contribute to `Agent::verification_touched_paths` — `move_file`'s
/// destination (`to`), since its source no longer exists after a
/// successful move and testing a nonexistent path would be meaningless;
/// every other trigger tool's ordinary `path` argument. Not a full
/// `path_resolve`-style canonicalization — this is bookkeeping for a
/// diagnostic test command's arguments, not a filesystem-access decision,
/// so a plain `cwd.join` is enough (the tool call itself already went
/// through real path resolution when it executed).
fn touched_path_for(call: &ToolCall, cwd: &Path) -> Option<PathBuf> {
    let raw = if call.name == "move_file" {
        call.arguments.get("to").and_then(|v| v.as_str())?
    } else {
        call.arguments.get("path").and_then(|v| v.as_str())?
    };
    Some(cwd.join(raw))
}
```

Wire it into the edit-tracking loop (`crates/aivyx-core/src/agent/mod.rs:1467-1491`). Change:

```rust
                let is_edit_call = VERIFICATION_TRIGGER_TOOLS.contains(&call.name.as_str());
                let was_already_unverified = self.unverified_edits;
                let call_description = describe_tool_call_target(&call);
                let mut result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                    if self.autonomous_mode.active() && !was_already_unverified {
```

to:

```rust
                let is_edit_call = VERIFICATION_TRIGGER_TOOLS.contains(&call.name.as_str());
                let touched_path = if is_edit_call {
                    touched_path_for(&call, cwd)
                } else {
                    None
                };
                let was_already_unverified = self.unverified_edits;
                let call_description = describe_tool_call_target(&call);
                let mut result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                    if let Some(path) = touched_path {
                        self.record_touched_path(path);
                    }
                    if self.autonomous_mode.active() && !was_already_unverified {
```

(Only the lines shown change — everything else in that block, including the rest of the `if self.autonomous_mode.active() ...` body, is untouched.)

Add the clear at the two window-closing reset sites. First, the pass path (`crates/aivyx-core/src/agent/mod.rs`, currently around lines 1347-1350):

```rust
                        if passed {
                            self.unverified_edits = false;
                            self.verify_retries = 0;
                            self.pre_experiment_ref = None;
                            self.verification_touched_paths.clear();
                            self.emit(AgentEvent::TurnComplete);
                            return Ok(());
                        }
```

Second, the autonomous-rewind-success path (currently around line 1377, inside the `Ok(()) => { self.unverified_edits = false; ... }` arm):

```rust
                            Ok(()) => {
                                self.unverified_edits = false;
                                self.verification_touched_paths.clear();
                                self.emit(AgentEvent::Error(format!(
```

Do **not** add a clear to the interactive-exhaustion-without-rewind branch (the `else` arm around line 1401-1408) — `unverified_edits` deliberately stays `true` there, and the touched paths remain relevant for whatever future turn picks up verification again.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS — this task's new tests plus the entire existing `aivyx-core` suite (this touches shared control flow, so a full-crate run, not just a filtered one, is the right check here).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Add the verification_touched_paths accumulator"
```

---

### Task 6: `substitute_touched_paths` and the scoped-run bypass execution

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (new imports; a new pure function near `touched_path_for`; a new `run_scoped_verification` method near `run_auto_verification`; `run_auto_verification`'s `last_verification_output` update, currently lines 763-771)

**Interfaces:**
- Consumes: `aivyx_tools::run` (Task 2), `ScopedVerificationConfig`/`VerificationKind` (Task 3).
- Produces: `fn substitute_touched_paths(args: &[String], touched_paths: &[PathBuf], cwd: &Path) -> Vec<String>`; `async fn Agent::run_scoped_verification(&mut self, scoped: ScopedVerificationConfig, touched_paths: &[PathBuf], cwd: &Path, cancellation: &CancellationToken) -> bool`. Consumed by Task 7's retry-loop orchestration (which this task does *not* yet wire in — that's Task 7, so this task's new method is unreachable dead code until then, verified by its own direct unit tests in the meantime).

- [ ] **Step 1: Write the failing tests**

Add near `touched_path_for` (or in `agent/tests.rs`, matching wherever Task 5's pure-function tests landed):

```rust
#[test]
fn substitute_touched_paths_expands_the_placeholder_into_multiple_argv_entries() {
    let args = vec!["test".to_string(), "{touched_paths}".to_string()];
    let touched = vec![PathBuf::from("/project/a.rs"), PathBuf::from("/project/b.rs")];
    let result = substitute_touched_paths(&args, &touched, Path::new("/project"));
    assert_eq!(result, vec!["test", "a.rs", "b.rs"]);
}

#[test]
fn substitute_touched_paths_leaves_args_without_the_token_unchanged() {
    let args = vec!["test".to_string(), "--verbose".to_string()];
    let touched = vec![PathBuf::from("/project/a.rs")];
    let result = substitute_touched_paths(&args, &touched, Path::new("/project"));
    assert_eq!(result, vec!["test", "--verbose"]);
}

#[test]
fn substitute_touched_paths_uses_paths_relative_to_cwd() {
    let args = vec!["{touched_paths}".to_string()];
    let touched = vec![PathBuf::from("/project/src/foo.rs")];
    let result = substitute_touched_paths(&args, &touched, Path::new("/project"));
    assert_eq!(result, vec!["src/foo.rs"]);
}

#[test]
fn substitute_touched_paths_does_not_partially_interpolate_a_token_embedded_in_a_larger_string() {
    let args = vec!["--filter={touched_paths}".to_string()];
    let touched = vec![PathBuf::from("/project/a.rs")];
    let result = substitute_touched_paths(&args, &touched, Path::new("/project"));
    // Exact-whole-entry match only — this arg is left completely unchanged,
    // not partially substituted.
    assert_eq!(result, vec!["--filter={touched_paths}"]);
}
```

For `run_scoped_verification`, since it needs a real spawnable process: add tests using plain, deterministic `sh` one-liners as fake scoped test runners (mirroring `repl.rs`'s own precedent of doing this rather than requiring `pytest`/`cargo` in the test/CI environment), built via `build_agent`'s existing harness with an empty registry (the scoped path never touches the registry — it bypasses `ToolExecutor` entirely):

```rust
#[tokio::test]
async fn run_scoped_verification_reports_pass_and_records_history() {
    let (mut agent, _rx, _mock) = build_agent(vec![], ToolRegistry::new(), 10);
    let dir = tempfile::tempdir().unwrap();

    let scoped = ScopedVerificationConfig {
        spec: CommandSpec {
            name: "fake_scoped".to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 0".to_string()],
            timeout: Duration::from_secs(5),
        },
        confiner: Arc::new(NoopConfiner),
    };

    let passed = agent
        .run_scoped_verification(scoped, &[], dir.path(), &CancellationToken::new())
        .await;

    assert!(passed);
    assert_eq!(
        auto_verify_calls(&agent.history),
        1,
        "the scoped run must be recorded as a synthetic AutoVerification call, same as the full-command path"
    );
}

#[tokio::test]
async fn run_scoped_verification_reports_failure_for_a_nonzero_exit() {
    let (mut agent, _rx, _mock) = build_agent(vec![], ToolRegistry::new(), 10);
    let dir = tempfile::tempdir().unwrap();

    let scoped = ScopedVerificationConfig {
        spec: CommandSpec {
            name: "fake_scoped".to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            timeout: Duration::from_secs(5),
        },
        confiner: Arc::new(NoopConfiner),
    };

    let passed = agent
        .run_scoped_verification(scoped, &[], dir.path(), &CancellationToken::new())
        .await;

    assert!(!passed);
}

#[tokio::test]
async fn run_scoped_verification_substitutes_touched_paths_into_argv() {
    let (mut agent, _rx, _mock) = build_agent(vec![], ToolRegistry::new(), 10);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();

    let scoped = ScopedVerificationConfig {
        spec: CommandSpec {
            name: "fake_scoped".to_string(),
            program: "sh".to_string(),
            // Fails unless its first positional arg is a path that exists
            // relative to cwd — proves the placeholder was substituted
            // with a real, cwd-relative touched path, not left literal.
            args: vec![
                "-c".to_string(),
                "test -f \"$1\"".to_string(),
                "sh".to_string(),
                "{touched_paths}".to_string(),
            ],
            timeout: Duration::from_secs(5),
        },
        confiner: Arc::new(NoopConfiner),
    };
    let touched = vec![dir.path().join("a.rs")];

    let passed = agent
        .run_scoped_verification(scoped, &touched, dir.path(), &CancellationToken::new())
        .await;

    assert!(
        passed,
        "the substituted path must resolve to a real file relative to cwd"
    );
}
```

(`ScopedVerificationConfig`, `CommandSpec`, `NoopConfiner`, `Duration`, `Arc`, `CancellationToken`, `ToolRegistry`, and `build_agent`/`auto_verify_calls` are all already imported/defined at the top of `crates/aivyx-core/src/agent/tests.rs` per its existing harness — no new imports needed for these three tests beyond what Task 3 already added to `types.rs`, since `tests.rs` uses `use super::*;` and `ScopedVerificationConfig`/`VerificationKind` will already be in scope via `mod.rs`'s own `use types::{...}` once Task 6's Step 3 below adds them there.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-core -- --test-threads=1 substitute_touched_paths`
Run: `cargo test -p aivyx-core -- --test-threads=1 run_scoped_verification`
Expected: FAIL to compile — neither function exists yet.

- [ ] **Step 3: Add the imports, the pure function, and the method**

Add `ExecutionConfiner` to the existing `aivyx_sandbox` import (`crates/aivyx-core/src/agent/mod.rs:6`):

```rust
use aivyx_sandbox::{AutonomousMode, ExecutionConfiner, InjectionTaint, PlanMode};
```

Add `Stdio` to the top-level `use std::` imports (currently just `use std::path::{Path, PathBuf}; use std::sync::{Arc, Mutex};`):

```rust
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
```

Import the two new types from `types`:

```rust
use types::{AgentsFileConfig, EditorContextConfig, ScopedVerificationConfig, VerificationConfig, VerificationKind};
```

Add the pure function near `touched_path_for`:

```rust
/// Expands the literal placeholder token `"{touched_paths}"` in `args`
/// into one argv entry per path in `touched_paths` (relative to `cwd`,
/// matching how a human would actually write a scoped command's expected
/// input, e.g. `pytest tests/test_foo.py` not an absolute path) — an
/// exact, whole-entry match only, never partial-string interpolation
/// (`--filter={touched_paths}` is left completely unchanged; a user
/// needing that shape writes a wrapper script instead).
fn substitute_touched_paths(args: &[String], touched_paths: &[PathBuf], cwd: &Path) -> Vec<String> {
    let relative: Vec<String> = touched_paths
        .iter()
        .map(|p| p.strip_prefix(cwd).unwrap_or(p).display().to_string())
        .collect();
    args.iter()
        .flat_map(|arg| {
            if arg == "{touched_paths}" {
                relative.clone()
            } else {
                vec![arg.clone()]
            }
        })
        .collect()
}
```

Add the new method near `run_auto_verification` (`crates/aivyx-core/src/agent/mod.rs:721`):

```rust
    /// Runs the scoped verification command directly — sandboxed via the
    /// same `ExecutionConfiner` `run_command` would apply, but without
    /// going through `ConfirmationGate`/`ToolExecutor::dispatch` at all
    /// (see docs/superpowers/specs/
    /// 2026-07-28-verification-test-selection-design.md's Decision 4: its
    /// args vary every retry, which the gate's exact-`(program, args)`
    /// Always-Allow cache can't accommodate without defeating the point).
    /// Recorded into history identically to `run_auto_verification`'s
    /// full-command path — a synthetic assistant+tool message pair — so
    /// the model sees it as an ordinary round-trip, and participates in
    /// the same per-`VerificationKind` output comparison.
    async fn run_scoped_verification(
        &mut self,
        scoped: ScopedVerificationConfig,
        touched_paths: &[PathBuf],
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> bool {
        let args = substitute_touched_paths(&scoped.spec.args, touched_paths, cwd);

        self.synthetic_seq += 1;
        let call = ToolCall {
            id: ToolCallId(format!("auto-verify-scoped-{}", self.synthetic_seq)),
            name: "run_command".to_string(),
            arguments: serde_json::json!({ "command": scoped.spec.name, "scoped": true }),
            source: ToolCallSource::AutoVerification,
        };
        self.emit(AgentEvent::ToolCallDetected(call.clone()));
        self.history.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolCall(call.clone())],
            tool_call_id: None,
        });

        let mut command = tokio::process::Command::new(&scoped.spec.program);
        command
            .args(&args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let command = scoped.confiner.confine(command);

        let output = aivyx_tools::run(command, scoped.spec.timeout, cancellation.clone()).await;
        let mut result = ToolResult {
            call_id: call.id.clone(),
            output: output.unwrap_or_else(|err| ToolOutput::Error(err.to_string())),
        };
        let passed =
            matches!(&result.output, ToolOutput::Ok(text) if command_reported_success(text));

        if let ToolOutput::Ok(text) = &mut result.output {
            let current_text = text.clone();
            if !passed
                && let Some((VerificationKind::Scoped, previous)) = &self.last_verification_output
                && let Some(note) = new_lines_note(previous, &current_text)
            {
                text.push_str(&note);
            }
            self.last_verification_output = Some((VerificationKind::Scoped, current_text));
        }

        let source = format!("{} (scoped)", scoped.spec.name);
        self.record_tool_result(result, &source);
        passed
    }
```

Update `run_auto_verification`'s own `last_verification_output` handling (`crates/aivyx-core/src/agent/mod.rs:763-771`) to match the new tuple type. Change:

```rust
        if let ToolOutput::Ok(text) = &mut result.output {
            let current_text = text.clone();
            if !passed
                && let Some(previous) = &self.last_verification_output
                && let Some(note) = new_lines_note(previous, &current_text)
            {
                text.push_str(&note);
            }
            self.last_verification_output = Some(current_text);
        }
```

to:

```rust
        if let ToolOutput::Ok(text) = &mut result.output {
            let current_text = text.clone();
            if !passed
                && let Some((VerificationKind::Full, previous)) = &self.last_verification_output
                && let Some(note) = new_lines_note(previous, &current_text)
            {
                text.push_str(&note);
            }
            self.last_verification_output = Some((VerificationKind::Full, current_text));
        }
```

Also update the `last_verification_output` field's own type declaration (`crates/aivyx-core/src/agent/mod.rs:229`) — change:

```rust
    last_verification_output: Option<String>,
```

to:

```rust
    last_verification_output: Option<(VerificationKind, String)>,
```

(`Agent::new`'s `last_verification_output: None,` initializer, line 289, needs no change — `None` is valid for either type.)

Two **existing** tests in `crates/aivyx-core/src/agent/tests.rs` directly access this field and will fail to compile after the type change: `starts_broken_then_fixed_leaves_the_passing_result_unmodified` and `last_verification_output_updates_after_every_call_regardless_of_outcome`, both currently containing:

```rust
    assert_eq!(
        agent.last_verification_output.as_deref(),
        Some(results[1].as_str()),
```

(the exact `Some(...)` comparison text differs slightly between the two — one has a trailing message argument, the other doesn't; find both occurrences by searching the file for `last_verification_output.as_deref()`, there are exactly two). Change **both** occurrences from:

```rust
        agent.last_verification_output.as_deref(),
        Some(results[1].as_str()),
```

to:

```rust
        agent.last_verification_output.as_ref().map(|(_, text)| text.as_str()),
        Some(results[1].as_str()),
```

This preserves each test's original intent exactly (comparing only the stored *text*, ignoring which `VerificationKind` it was tagged — both these existing tests only ever exercise the full-command path, so it's always `VerificationKind::Full`, but asserting on the kind isn't what either test is about).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS — this task's new tests, plus the full existing suite (the `last_verification_output` type change affects `run_auto_verification`'s existing tests, so confirm those specifically still pass, not just compile — the two occurrences fixed above in particular).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Add substitute_touched_paths and the scoped-verification bypass-execution path"
```

---

### Task 7: Wire scoped verification into the retry loop

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (the retry-loop body, currently lines 1339-1357)

**Interfaces:**
- Produces: `async fn Agent::run_verification_attempt(&mut self, verification: &VerificationConfig, cwd: &Path, cancellation: &CancellationToken) -> bool` — the new orchestration entry point the retry loop calls instead of `run_auto_verification` directly.

- [ ] **Step 1: Write the failing tests**

Add to `agent/tests.rs`'s "enforced verification" section. **Test 1's coverage already exists**: the pre-existing `a_passing_verification_completes_the_turn_without_an_extra_round_trip` and `a_failing_verification_feeds_back_and_retries_until_exhausted` never call `set_scoped_verification`/set `verification.scoped`, so `run_verification_attempt`'s `if let Some(scoped) = ...` branch is never entered for them — if this task's change to the retry loop broke the no-scoping path, those two pre-existing tests would fail. No new test needed for that case; running the full existing suite in Step 4 below is what proves it.

`agent.verification` and `VerificationConfig.scoped` are accessible directly from these tests (both are private/`pub(crate)` fields of the `agent` module, and `agent::tests` is a child module of `agent` — Rust's privacy rules make ancestor-module-private items visible to descendant modules; the existing tests already rely on this for `agent.unverified_edits`/`agent.verify_retries`/`agent.last_verification_output`), so these tests set up scoping directly without needing `Agent::set_scoped_verification` (added in Task 8, after this task):

```rust
#[tokio::test]
async fn a_failing_scoped_run_skips_the_full_command_this_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    // The full command would pass if it ever ran — this test proves it
    // never does, since the scoped command fails first.
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", true,
    )])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _) = build_agent(
        vec![write_call, text_response("done"), text_response("still trying")],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 1);
    agent.verification.as_mut().unwrap().scoped = Some(ScopedVerificationConfig {
        spec: CommandSpec {
            name: "scoped".to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()],
            timeout: Duration::from_secs(5),
        },
        confiner: Arc::new(NoopConfiner),
    });

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    // Only the scoped attempt ran — exactly 1 AutoVerification call for
    // max_retries: 1, proving the full command never ran.
    assert_eq!(auto_verify_calls(&agent.history), 1);
    assert!(agent.unverified_edits, "the iteration must report failure");
}

#[tokio::test]
async fn a_passing_scoped_run_is_confirmed_by_a_full_run_that_can_still_fail() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(aivyx_tools::WriteFileTool));
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", false,
    )])));

    let write_call = vec![
        StreamEvent::ToolCallComplete(ToolCall {
            id: ToolCallId("c1".to_string()),
            name: "write_file".to_string(),
            arguments: serde_json::json!({ "path": "out.txt", "content": "hi\n" }),
            source: ToolCallSource::Native,
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
        },
    ];
    let (mut agent, _rx, _) = build_agent(
        vec![write_call, text_response("done"), text_response("still trying")],
        registry,
        10,
    );
    agent.set_verification("verify".to_string(), 1);
    agent.verification.as_mut().unwrap().scoped = Some(ScopedVerificationConfig {
        spec: CommandSpec {
            name: "scoped".to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 0".to_string()],
            timeout: Duration::from_secs(5),
        },
        confiner: Arc::new(NoopConfiner),
    });

    agent
        .run_turn("go".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    // Both the scoped (pass) and full (fail) commands ran in this single
    // retry-budget iteration — 2 AutoVerification calls for max_retries: 1.
    assert_eq!(
        auto_verify_calls(&agent.history),
        2,
        "a passing scoped run must still be confirmed by one full run"
    );
    assert!(
        agent.unverified_edits,
        "the iteration's overall result is the full command's (failing) outcome, not the scoped pass"
    );
}

#[tokio::test]
async fn run_verification_attempt_falls_back_to_full_only_when_no_paths_are_touched() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RunCommandTool::new(vec![verify_command_spec(
        "verify", true,
    )])));
    let (mut agent, _rx, _) = build_agent(vec![], registry, 10);
    agent.set_verification("verify".to_string(), 3);
    agent.verification.as_mut().unwrap().scoped = Some(ScopedVerificationConfig {
        spec: CommandSpec {
            name: "scoped".to_string(),
            program: "sh".to_string(),
            // Would leave a marker file if it ever ran — proves it doesn't.
            args: vec!["-c".to_string(), "touch scoped_ran".to_string()],
            timeout: Duration::from_secs(5),
        },
        confiner: Arc::new(NoopConfiner),
    });
    let verification = agent.verification.clone().unwrap();

    // verification_touched_paths is empty — build_agent never dispatched
    // any edit tool call, so run_verification_attempt is called directly
    // here rather than through run_turn, isolating this one fallback case.
    let passed = agent
        .run_verification_attempt(&verification, dir.path(), &CancellationToken::new())
        .await;

    assert!(
        passed,
        "with no touched paths, only the (passing) full command should run"
    );
    assert!(
        !dir.path().join("scoped_ran").exists(),
        "the scoped command must never run when there are no touched paths to scope to"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-core -- --test-threads=1 run_verification_attempt`
Expected: FAIL — either compile failure (method doesn't exist) or, if written against the still-unwired retry loop, behavioral failure.

- [ ] **Step 3: Add `run_verification_attempt` and wire it in**

Add the new method near `run_auto_verification`/`run_scoped_verification`:

```rust
    /// One retry-budget attempt: if a scoped command is configured and at
    /// least one path has been touched since edits became unverified, try
    /// it first (bypassing the gate — see `run_scoped_verification`'s own
    /// doc comment). A scoped pass is not yet a verified batch — one full
    /// run is still required as the final gate, preserving the same
    /// completeness guarantee this feature had before scoping existed. If
    /// the scoped run fails, the full command is skipped entirely for
    /// this iteration (that's the whole point of scoping: avoiding its
    /// cost when it wouldn't change the outcome). Exactly one
    /// `verify_retries` slot is consumed per call to this method by its
    /// caller, regardless of whether it runs one command or two.
    async fn run_verification_attempt(
        &mut self,
        verification: &VerificationConfig,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> bool {
        if let Some(scoped) = verification.scoped.clone()
            && !self.verification_touched_paths.is_empty()
        {
            let touched = self.verification_touched_paths.clone();
            let scoped_passed = self
                .run_scoped_verification(scoped, &touched, cwd, cancellation)
                .await;
            if !scoped_passed {
                return false;
            }
        }
        self.run_auto_verification(&verification.command_name, cwd, cancellation)
            .await
    }
```

Change the retry-loop body (`crates/aivyx-core/src/agent/mod.rs:1342-1346`) from:

```rust
                    if self.verify_retries < verification.max_retries {
                        self.verify_retries += 1;
                        let passed = self
                            .run_auto_verification(&verification.command_name, cwd, &cancellation)
                            .await;
```

to:

```rust
                    if self.verify_retries < verification.max_retries {
                        self.verify_retries += 1;
                        let passed = self
                            .run_verification_attempt(&verification, cwd, &cancellation)
                            .await;
```

(Everything after this line — the `if passed { ... }` block and the exhaustion branches below it — is unchanged; `verification` is already cloned into a local at the top of this `if let Some(verification) = self.verification.clone()` block, per the existing code shown in Task 5's excerpt, so passing `&verification` here is a reference to that existing local, not a new clone.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS — this task's new tests plus the full existing suite. This is the task that actually activates the feature end-to-end, so a full-crate run is essential, not optional.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs
git commit -m "Wire scoped verification into the retry loop"
```

---

### Task 8: `Agent::set_scoped_verification` and `agent_builder.rs` wiring

**Files:**
- Modify: `crates/aivyx-core/src/agent/mod.rs` (new public method near `set_verification`)
- Modify: `crates/aivyx/src/agent_builder.rs` (the verification-wiring block, currently lines 522-536)

**Interfaces:**
- Produces: `pub fn Agent::set_scoped_verification(&mut self, spec: aivyx_tools::CommandSpec, confiner: std::sync::Arc<dyn aivyx_sandbox::ExecutionConfiner>)`. Consumed by `agent_builder.rs`.

- [ ] **Step 1: Write the failing test**

Add to `agent/tests.rs`'s "enforced verification" section. As established in Task 7, `agent.verification` is directly readable from these tests (child-module privacy access), so no accessor needs inventing:

```rust
#[test]
fn set_scoped_verification_attaches_to_an_already_configured_verification() {
    let (mut agent, _rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
    agent.set_verification("test".to_string(), 3);

    agent.set_scoped_verification(
        CommandSpec {
            name: "test_scoped".to_string(),
            program: "pytest".to_string(),
            args: vec!["{touched_paths}".to_string()],
            timeout: Duration::from_secs(30),
        },
        Arc::new(NoopConfiner),
    );

    let scoped = agent.verification.as_ref().unwrap().scoped.as_ref();
    assert!(scoped.is_some());
    assert_eq!(scoped.unwrap().spec.name, "test_scoped");
}

#[test]
fn set_scoped_verification_before_set_verification_is_a_harmless_no_op() {
    let (mut agent, _rx, _) = build_agent(vec![], ToolRegistry::new(), 10);
    // set_verification was never called — verification is None.

    agent.set_scoped_verification(
        CommandSpec {
            name: "test_scoped".to_string(),
            program: "pytest".to_string(),
            args: vec![],
            timeout: Duration::from_secs(30),
        },
        Arc::new(NoopConfiner),
    );

    assert!(
        agent.verification.is_none(),
        "must stay disabled, not panic or silently enable itself"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-core -- --test-threads=1 set_scoped_verification`
Expected: FAIL to compile — the method doesn't exist yet.

- [ ] **Step 3: Add the method**

Add near `set_verification` (`crates/aivyx-core/src/agent/mod.rs:329`):

```rust
    /// Attaches scoped-verification support to an already-configured base
    /// verification (call `set_verification` first). A no-op if
    /// verification isn't enabled at all — scoping is meaningless without
    /// a base command to run as the final gate. See
    /// docs/superpowers/specs/2026-07-28-verification-test-selection-design.md.
    pub fn set_scoped_verification(
        &mut self,
        spec: aivyx_tools::CommandSpec,
        confiner: std::sync::Arc<dyn ExecutionConfiner>,
    ) {
        if let Some(verification) = &mut self.verification {
            verification.scoped = Some(ScopedVerificationConfig { spec, confiner });
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Wire it into `agent_builder.rs`**

Change the verification-wiring block (`crates/aivyx/src/agent_builder.rs:522-536`) from:

```rust
    // Enforced verification (ROADMAP.md Phase 12 Part B): configuring the
    // command alone is the opt-in, no separate enable flag. The name must
    // match an `allowed_commands` entry (the same trust tier `run_command`
    // itself uses) — warn loudly rather than silently doing nothing if it
    // doesn't, since a typo here would otherwise look like the feature is
    // enabled but never actually verify anything.
    if let Some((command, max_retries)) = &verification {
        agent.set_verification(command.clone(), *max_retries);
    } else if let Some(command) = &settings.verification.command {
        tracing::warn!(
            command = %command,
            "verification.command does not match any [[permissions.allowed_commands]] \
             entry name — enforced verification is disabled until this is fixed"
        );
    }
```

to:

```rust
    // Enforced verification (ROADMAP.md Phase 12 Part B): configuring the
    // command alone is the opt-in, no separate enable flag. The name must
    // match an `allowed_commands` entry (the same trust tier `run_command`
    // itself uses) — warn loudly rather than silently doing nothing if it
    // doesn't, since a typo here would otherwise look like the feature is
    // enabled but never actually verify anything.
    if let Some((command, max_retries)) = &verification {
        agent.set_verification(command.clone(), *max_retries);

        // Scoped verification (docs/superpowers/specs/
        // 2026-07-28-verification-test-selection-design.md): only
        // resolved if the base command above was itself valid — scoping
        // an already-disabled verification setup makes no sense. Same
        // "warn, don't silently do nothing" posture as the base command's
        // own validation, but scoping alone being misconfigured must not
        // disable verification entirely — it falls back to always
        // running the full command, exactly as if scoped_command had
        // never been set.
        if let Some(scoped_name) = &settings.verification.scoped_command {
            match command_specs.iter().find(|spec| &spec.name == scoped_name) {
                Some(spec) => {
                    agent.set_scoped_verification(spec.clone(), Arc::clone(&confiner));
                }
                None => {
                    tracing::warn!(
                        scoped_command = %scoped_name,
                        "verification.scoped_command does not match any \
                         [[permissions.allowed_commands]] entry name — scoped verification is \
                         unavailable this session (falling back to always running the full \
                         command)"
                    );
                }
            }
        }
    } else if let Some(command) = &settings.verification.command {
        tracing::warn!(
            command = %command,
            "verification.command does not match any [[permissions.allowed_commands]] \
             entry name — enforced verification is disabled until this is fixed"
        );
    }
```

- [ ] **Step 6: Build to confirm it compiles**

Run: `cargo build -p aivyx`
Expected: builds cleanly

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-core/src/agent/mod.rs crates/aivyx-core/src/agent/tests.rs crates/aivyx/src/agent_builder.rs
git commit -m "Add Agent::set_scoped_verification and wire it into agent_builder.rs"
```

---

### Task 9: Documentation

**Files:**
- Modify: `README.md` (the existing enforced-verification section)
- Modify: `ROADMAP.md` (remove the "Verification test-selection" backlog bullet — this closes the backlog entirely)

**Interfaces:** none (documentation only)

- [ ] **Step 1: Find the existing verification documentation**

Search `README.md` for its existing `[verification]`/enforced-verification section (the config-reference table and/or prose paragraph documenting `command`/`max_auto_verify_retries` — these already exist, per the design doc's Context section referencing "Phase 12 Part B").

- [ ] **Step 2: Add the `scoped_command` documentation**

Add a paragraph or config-table row (matching whatever format the existing `command`/`max_auto_verify_retries` documentation uses) covering: what `scoped_command` does, the `{touched_paths}` placeholder's exact-whole-token substitution semantics, the worked example below, and the gate-bypass note. (Shown here as one block using 4-backtick fencing since the content itself contains a fenced `toml` example — when actually writing it into `README.md`, use ordinary 3-backtick fences as normal, matching the surrounding file's own convention.)

````markdown
`scoped_command` (optional): another `[[permissions.allowed_commands]]`
entry name, whose `args` may contain the literal token `"{touched_paths}"`
— substituted at runtime with the files touched since edits became
unverified, one argv entry per path (relative to the project root), never
a joined string. When configured, interim retries in the fix-and-retry
loop run this faster, scoped command first; one full, unscoped run is
still required before a batch of edits is finally declared verified —
scoping speeds up iteration, it never weakens the final guarantee.

Example (works with `pytest`, which accepts file paths as test-selection
arguments natively):

```toml
[verification]
command = "test"
scoped_command = "test_scoped"

[[permissions.allowed_commands]]
name = "test_scoped"
program = "pytest"
args = ["{touched_paths}"]
```

Not every test runner supports path-based filtering this directly — `cargo
test <file-path>` in particular does **not** (verified: it silently
matches zero tests and reports success). For a `cargo`-based project,
`scoped_command` needs a small wrapper script that translates a file path
into an appropriate module-path filter instead of a bare `cargo test`
invocation.

The scoped run's arguments differ on every retry (different touched
files), so — unlike every other `run_command` invocation, including the
full `command` above — it does **not** go through the normal per-exact-
argument approval cache: it executes directly, sandboxed the same way,
on the reasoning that the only dynamic input is files the model already
had gated permission to edit. This is a deliberate, narrow exception to
this project's "the model never influences a `run_command` invocation's
arguments" invariant, documented here rather than left implicit.
````

- [ ] **Step 3: Remove the ROADMAP.md backlog item**

In `ROADMAP.md`, remove the entire "**Verification test-selection**" bullet and its introductory paragraph above it (the one currently reading "The remaining one is sized as its own feature..."), replacing that paragraph with a closing note that the backlog from the 2026-07-22 audit is now fully resolved. Read the current exact text first (it was last edited by the patch-apply-tool feature's own Task 2) before writing the replacement, since this plan can't show it verbatim without risking drift from whatever's actually there by the time this task runs.

- [ ] **Step 4: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Document scoped_command; close the verification test-selection backlog item"
```

---

### Task 10: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Build the whole workspace**

Run: `cargo build --workspace`
Expected: builds cleanly

- [ ] **Step 2: Run the whole test suite**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS — every test in every crate, including all tests added in Tasks 1-8

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets`
Expected: no warnings

- [ ] **Step 4: Manual smoke test (not automatable — requires a live TUI session with a real backend)**

Configure a real project with both `command` and `scoped_command` set, drive a real model through the release binary to make an edit that initially fails verification, and confirm: the scoped rerun visibly runs faster than the full command would, the model sees appropriately-labeled tool results for both scoped and full runs, and a deliberately-planted regression in a file *outside* the touched-paths set is still caught by the mandatory final full run. This is the live-E2E verification step the design doc calls for — record the result in a follow-up note, but it is not a `- [ ]` step this plan can check off automatically.

If any step fails, stop and fix before proceeding — do not commit on top of a failing workspace state.
