# Move/Rename Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `move_file` tool that atomically relocates a file or directory, closing the last item in the 2026-07-22 capability-audit backlog (currently synthesized via read+write+delete with no atomicity guarantee).

**Architecture:** A new `ActionKind::Move` and `PermissionTarget::Move { from, to }` thread through the permission gate (deny_paths on both endpoints, autonomous-worktree boundary on both endpoints, exact-pair Always-Allow caching), every UI/protocol surface that renders a `PermissionTarget` (TUI modal, ACP, editor-approval), and a new `MoveFileTool` built on a single `tokio::fs::rename` (atomic for files and whole directory trees alike). Directory moves get an extra, security-critical recursive scan (not gitignore-filtered, unlike `grep`/`glob`'s own walk) so a `deny_paths` entry nested inside the tree can't be silently relocated out of protection.

**Tech Stack:** Rust, tokio, the `ignore` crate (already a dependency, used by `grep`/`glob`), `agent-client-protocol` 2.0 (already has an unused `ToolKind::Move`).

## Global Constraints

- Every `cargo test` invocation in this plan MUST include `-- --test-threads=1`. `aivyx-sandbox`'s confiner tests hang indefinitely under default parallelism in this sandboxed dev environment (confirmed, unrelated to any code change) — `cargo build`/`cargo clippy` are unaffected.
- Full spec: `docs/superpowers/specs/2026-07-27-move-rename-tool-design.md`. Three resolved decisions that shape every task below: (1) files **and** directories are in scope; (2) refuse outright if the destination already exists (no overwrite mode); (3) refuse on cross-filesystem `EXDEV` (no copy+delete fallback).
- Follow existing patterns exactly: `crates/aivyx-tools/src/tools/delete_file.rs` (single-mutation-tool shape, text/binary preview split) and `crates/aivyx-tools/src/tools/glob.rs` (`ignore::WalkBuilder` usage, capped-listing truncation) are the two templates every new tool code in this plan is modeled on.
- No new config surface (no `[move_file]` section) — a single atomic syscall has no tunables.

---

### Task 1: `ActionKind::Move` + `PermissionTarget::Move` in `aivyx-sandbox`

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs:56-110` (add `ActionKind::Move`, add `PermissionTarget::Move`)
- Modify: `crates/aivyx-sandbox/src/confirmation.rs` (`PermissionKey` enum + `from_request` at ~60-92; `is_denied` at ~162-167; `is_outside_autonomous_worktree` at ~174-182; the injection-taint guard at ~296-297; the autonomous-mode target dispatch at ~323-338)
- Modify: `crates/aivyx-sandbox/src/editor_approval.rs` (`ApprovalContent` enum at ~35-51; the target-string match in `build_pending_request` at ~130-134; the content match at ~136-169)
- Test: all three files' existing `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `aivyx_sandbox::ActionKind::Move`, `aivyx_sandbox::PermissionTarget::Move { from: PathBuf, to: PathBuf }` — every later task in this plan constructs/matches these exact names.

This task must land as one commit: `PermissionTarget`/`ActionKind` are `#[non_exhaustive]`-free enums with several exhaustive matches *inside this same crate*, so the crate won't compile again until all of them are updated together.

- [ ] **Step 1: Write the failing gate-level tests**

Add to `crates/aivyx-sandbox/src/confirmation.rs`'s `mod tests` (near the other `write_request`/`read_request` helpers, around line 463):

```rust
fn move_request(from: &str, to: &str) -> PermissionRequest {
    PermissionRequest {
        tool_name: "move_file".to_string(),
        action: ActionKind::Move,
        target: PermissionTarget::Move {
            from: PathBuf::from(from),
            to: PathBuf::from(to),
        },
        arguments_preview: serde_json::json!({}),
        preview: None,
        diff: None,
    }
}

#[tokio::test]
async fn deny_paths_blocks_a_move_whose_source_matches() {
    let prompter = Arc::new(FakePrompter {
        response: UserResponse::Allow,
        calls: AtomicUsize::new(0),
    });
    let gate = ConfirmationGate::new(
        prompter.clone(),
        vec![PathBuf::from("/home/user/.ssh")],
        vec![],
        PlanMode::new(),
        AutonomousMode::new(),
        PathBuf::from("/home/user/project"),
        false,
    );

    let decision = gate
        .check(&move_request(
            "/home/user/.ssh/id_ed25519",
            "/home/user/project/stolen_key",
        ))
        .await;

    assert!(matches!(decision, PermissionDecision::Deny(_)));
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deny_paths_blocks_a_move_whose_destination_matches() {
    let prompter = Arc::new(FakePrompter {
        response: UserResponse::Allow,
        calls: AtomicUsize::new(0),
    });
    let gate = ConfirmationGate::new(
        prompter.clone(),
        vec![PathBuf::from("/home/user/.config/aivyx-coder")],
        vec![],
        PlanMode::new(),
        AutonomousMode::new(),
        PathBuf::from("/home/user/project"),
        false,
    );

    let decision = gate
        .check(&move_request(
            "/home/user/project/notes.txt",
            "/home/user/.config/aivyx-coder/notes.txt",
        ))
        .await;

    assert!(matches!(decision, PermissionDecision::Deny(_)));
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn always_allow_for_a_move_does_not_cover_a_different_destination_with_the_same_source() {
    let prompter = Arc::new(FakePrompter {
        response: UserResponse::AllowAlways,
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

    let first = gate
        .check(&move_request("/home/user/project/a.rs", "/home/user/project/b.rs"))
        .await;
    assert_eq!(first, PermissionDecision::AllowAlways);
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

    // Same exact pair again: cached, no second prompt.
    let second = gate
        .check(&move_request("/home/user/project/a.rs", "/home/user/project/b.rs"))
        .await;
    assert_eq!(second, PermissionDecision::AllowAlways);
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

    // Same source, different destination: still prompts.
    let third = gate
        .check(&move_request("/home/user/project/a.rs", "/home/user/project/c.rs"))
        .await;
    assert_eq!(third, PermissionDecision::AllowAlways);
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn autonomous_mode_allows_a_move_within_the_worktree() {
    let prompter = Arc::new(FakePrompter {
        response: UserResponse::Deny,
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

    let decision = gate
        .check(&move_request(
            "/home/user/project/src/a.rs",
            "/home/user/project/src/b.rs",
        ))
        .await;

    assert_eq!(decision, PermissionDecision::Allow);
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn autonomous_mode_denies_a_move_whose_destination_is_outside_the_worktree() {
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

    let decision = gate
        .check(&move_request(
            "/home/user/project/src/a.rs",
            "/tmp/exfiltrated.rs",
        ))
        .await;

    let PermissionDecision::Deny(Some(reason)) = decision else {
        panic!("expected a denial with a reason, got {decision:?}");
    };
    assert!(reason.contains("cwd") || reason.contains("worktree"), "reason: {reason}");
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn autonomous_mode_denies_a_move_once_injection_taint_is_flagged() {
    let prompter = Arc::new(FakePrompter {
        response: UserResponse::Allow,
        calls: AtomicUsize::new(0),
    });
    let autonomous_mode = AutonomousMode::new();
    autonomous_mode.set_active(true);
    let injection_taint = InjectionTaint::new();
    injection_taint.flag(InjectionFinding {
        source: "read_file: notes.txt".to_string(),
        matched_pattern: "ignore previous instructions".to_string(),
        excerpt: "...".to_string(),
    });
    let gate = ConfirmationGate::new(
        prompter.clone(),
        vec![],
        vec![],
        PlanMode::new(),
        autonomous_mode,
        PathBuf::from("/home/user/project"),
        false,
    )
    .with_injection_taint(injection_taint);

    let decision = gate
        .check(&move_request(
            "/home/user/project/src/a.rs",
            "/home/user/project/src/b.rs",
        ))
        .await;

    let PermissionDecision::Deny(Some(reason)) = decision else {
        panic!("expected a denial with a reason, got {decision:?}");
    };
    assert!(reason.contains("notes.txt"), "reason: {reason}");
    assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
}
```

Add to `crates/aivyx-sandbox/src/editor_approval.rs`'s `mod tests` (near the other `builds_*_content_from_*` tests, around line 340):

```rust
#[test]
fn builds_move_content_from_the_move_target() {
    let request = PermissionRequest {
        tool_name: "move_file".to_string(),
        action: ActionKind::Move,
        target: PermissionTarget::Move {
            from: PathBuf::from("/project/old.rs"),
            to: PathBuf::from("/project/new.rs"),
        },
        arguments_preview: serde_json::json!({}),
        preview: Some("fn main() {}\n".to_string()),
        diff: None,
    };

    let pending = build_pending_request(&request, "req-1".to_string()).unwrap();
    assert_eq!(pending.target, "/project/old.rs -> /project/new.rs");
    let ApprovalContent::Move { from, to, preview } = pending.content else {
        panic!("expected Move content");
    };
    assert_eq!(from, "/project/old.rs");
    assert_eq!(to, "/project/new.rs");
    assert_eq!(preview, Some("fn main() {}\n".to_string()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-sandbox -- --test-threads=1`
Expected: FAIL to compile — `no variant named 'Move' found for enum 'ActionKind'` (and the same for `PermissionTarget`).

- [ ] **Step 3: Add the two enum variants**

In `crates/aivyx-sandbox/src/lib.rs`, add to `ActionKind` (after the `Interact` variant, before its closing brace, ~line 102):

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
    /// Relocating or renaming a file or directory (`move_file`). Neither a
    /// pure `Write` (the old path stops existing) nor a pure `Delete` (the
    /// content survives, just at a new path) — kept distinct so the
    /// confirmation modal, audit log, and autonomous-mode gating can
    /// describe it honestly, the same reasoning `ActionKind::Delete`'s own
    /// doc comment already gives for not folding deletion into `Write`.
    Move,
}
```

Change the `PermissionTarget` enum (~line 106-110) to:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionTarget {
    Path(PathBuf),
    Command { program: String, args: Vec<String> },
    Other(String),
    /// A move/rename's two endpoints. Kept structured (not folded into
    /// `Other(String)`) because `deny_paths` and the autonomous-mode
    /// worktree-boundary check both need real `PathBuf`s for *both* ends —
    /// approving `move a.rs b.rs` must not bless `move c.rs d.rs`, the same
    /// exact-target discipline `Command`'s cache key already documents.
    Move { from: PathBuf, to: PathBuf },
}
```

- [ ] **Step 4: Fix `confirmation.rs`'s four affected spots**

Change the `PermissionKey` enum (~line 60-73) to:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PermissionKey {
    Path {
        action: ActionKind,
        path: PathBuf,
    },
    Command {
        program: String,
        args: Vec<String>,
    },
    Other {
        action: ActionKind,
        description: String,
    },
    /// No `action` field, same reasoning as `Command`'s own key: a `Move`
    /// target only ever pairs with `ActionKind::Move`, so the shape is
    /// already 1:1 without it.
    Move {
        from: PathBuf,
        to: PathBuf,
    },
}
```

Change `PermissionKey::from_request` (~line 76-91) to:

```rust
    fn from_request(request: &PermissionRequest) -> Self {
        match &request.target {
            PermissionTarget::Path(path) => PermissionKey::Path {
                action: request.action,
                path: path.clone(),
            },
            PermissionTarget::Command { program, args } => PermissionKey::Command {
                program: program.clone(),
                args: args.clone(),
            },
            PermissionTarget::Other(description) => PermissionKey::Other {
                action: request.action,
                description: description.clone(),
            },
            PermissionTarget::Move { from, to } => PermissionKey::Move {
                from: from.clone(),
                to: to.clone(),
            },
        }
    }
```

Change `is_denied` (~line 162-167) to:

```rust
    fn is_denied(&self, request: &PermissionRequest) -> bool {
        match &request.target {
            PermissionTarget::Path(path) => path_is_denied(path, &self.deny_paths),
            PermissionTarget::Move { from, to } => {
                path_is_denied(from, &self.deny_paths) || path_is_denied(to, &self.deny_paths)
            }
            PermissionTarget::Command { .. } | PermissionTarget::Other(_) => false,
        }
    }
```

Change `is_outside_autonomous_worktree` (~line 169-182) to:

```rust
    /// The autonomous-mode edit boundary: a `Write`/`Delete`/`Move` action
    /// is only in-scope if every path it touches is at-or-under `cwd` — for
    /// `Move` that means both `from` and `to`. Only meaningful for
    /// autonomous mode — interactive mode relies on a human seeing the
    /// target in the confirmation modal instead (see the Phase 11c design
    /// doc's "gap found during design" section).
    fn is_outside_autonomous_worktree(&self, request: &PermissionRequest, cwd: &Path) -> bool {
        if !matches!(
            request.action,
            ActionKind::Write | ActionKind::Delete | ActionKind::Move
        ) {
            return false;
        }
        match &request.target {
            PermissionTarget::Path(path) => !path.starts_with(cwd),
            PermissionTarget::Move { from, to } => !from.starts_with(cwd) || !to.starts_with(cwd),
            PermissionTarget::Command { .. } | PermissionTarget::Other(_) => false,
        }
    }
```

In `check()`, change the injection-taint guard (~line 296-297) to:

```rust
            if (matches!(
                request.action,
                ActionKind::Write | ActionKind::Delete | ActionKind::Move
            ) || matches!(request.target, PermissionTarget::Command { .. }))
                && let Some(finding) = self.injection_taint.current()
            {
```

In `check()`, change the autonomous-mode target dispatch match (~line 323-362) to fold `Move` into the same allow arm as `Path`/`Other` (it's already passed the worktree-boundary check above by this point):

```rust
            return match &request.target {
                // Already passed the cwd-boundary check above (or wasn't a
                // Write/Delete/Move-on-Path(-like) target at all, e.g. a
                // Read/Internal target reaching here would be unusual since
                // tier 2 already caught those — but Other targets like
                // set_tasks's aren't Path/Command, so they fall here too
                // and must be allowed).
                PermissionTarget::Path(_) | PermissionTarget::Other(_) | PermissionTarget::Move { .. } => {
                    tracing::info!(
                        tool = %request.tool_name,
                        action = ?request.action,
                        target = ?request.target,
                        "permission allowed (autonomous mode, within worktree)"
                    );
                    PermissionDecision::Allow
                }
                PermissionTarget::Command { .. } => {
                    let key = PermissionKey::from_request(request);
                    if self.always_allow.lock().unwrap().contains(&key) {
                        tracing::info!(
                            tool = %request.tool_name,
                            action = ?request.action,
                            target = ?request.target,
                            "permission allowed (autonomous mode, pre-approved)"
                        );
                        PermissionDecision::AllowAlways
                    } else {
                        tracing::warn!(
                            tool = %request.tool_name,
                            action = ?request.action,
                            target = ?request.target,
                            "permission denied: not pre-approved and autonomous mode never prompts"
                        );
                        PermissionDecision::Deny(Some(
                            "not pre-approved, and autonomous mode has no one to prompt — add \
                             this to [[permissions.allowed_commands]] if it should be allowed"
                                .to_string(),
                        ))
                    }
                }
            };
```

- [ ] **Step 5: Fix `editor_approval.rs`'s three affected spots**

Change the `ApprovalContent` enum (~line 33-51) to:

```rust
#[derive(Debug, Serialize)]
#[serde(tag = "action_kind", rename_all = "snake_case")]
pub(crate) enum ApprovalContent {
    Write {
        old_content: String,
        new_content: String,
    },
    Delete {
        old_content: String,
        will_delete: bool,
    },
    Execute {
        command: String,
        args: Vec<String>,
    },
    McpTool {
        description: String,
    },
    Move {
        from: String,
        to: String,
        preview: Option<String>,
    },
}
```

In `build_pending_request`, change the target-string match (~line 130-134) to:

```rust
    let target = match &request.target {
        PermissionTarget::Path(path) => path.display().to_string(),
        PermissionTarget::Command { program, args } => format!("{program} {}", args.join(" ")),
        PermissionTarget::Other(description) => description.clone(),
        PermissionTarget::Move { from, to } => format!("{} -> {}", from.display(), to.display()),
    };
```

In `build_pending_request`, add a `Move` arm to the content match (insert before the `Read | Internal | Memory | Interact` catch-all line, ~line 168):

```rust
        ActionKind::McpTool => ApprovalContent::McpTool {
            description: request.preview.clone().unwrap_or_else(|| target.clone()),
        },
        ActionKind::Move => {
            let PermissionTarget::Move { from, to } = &request.target else {
                return None;
            };
            ApprovalContent::Move {
                from: from.display().to_string(),
                to: to.display().to_string(),
                preview: request.preview.clone(),
            }
        }
        // `Memory` and `Interact` fall back to the terminal-only path like Read/Internal:
        // there's no `ApprovalContent` shape defined for them yet, and the
        // editor-approval channel simply not participating for these
        // requests is the documented "no editor connected" fallback above,
        // not a functional regression — the terminal prompt still runs.
        ActionKind::Read | ActionKind::Internal | ActionKind::Memory | ActionKind::Interact => return None,
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p aivyx-sandbox -- --test-threads=1`
Expected: PASS (all new tests plus every pre-existing test in the crate)

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs crates/aivyx-sandbox/src/confirmation.rs crates/aivyx-sandbox/src/editor_approval.rs
git commit -m "Add ActionKind::Move and PermissionTarget::Move with a gate tier"
```

---

### Task 2: Render `Move` in the TUI confirmation modal

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs:716-730` (`target_lines`)
- Test: `crates/aivyx-tui/src/app.rs`'s `mod tests` (~line 847)

**Interfaces:**
- Consumes: `aivyx_sandbox::PermissionTarget::Move { from, to }` (Task 1).

- [ ] **Step 1: Write the failing test**

Add near `command_target_with_embedded_newline_splits_into_visible_lines` (~line 924):

```rust
    #[test]
    fn move_target_renders_as_from_arrow_to() {
        let target = PermissionTarget::Move {
            from: PathBuf::from("/project/old.rs"),
            to: PathBuf::from("/project/new.rs"),
        };

        let lines = target_lines(&target);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "Move: /project/old.rs -> /project/new.rs");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-tui -- --test-threads=1 move_target_renders_as_from_arrow_to`
Expected: FAIL to compile — `non-exhaustive patterns: '&PermissionTarget::Move { .. }' not covered`

- [ ] **Step 3: Add the `Move` arm**

Change `target_lines` (~line 724-730):

```rust
    let (label, body) = match target {
        PermissionTarget::Path(path) => ("Target: ", path.display().to_string()),
        PermissionTarget::Command { program, args } => {
            ("Command: ", format!("{program} {}", args.join(" ")))
        }
        PermissionTarget::Other(description) => ("Target: ", description.clone()),
        PermissionTarget::Move { from, to } => {
            ("Move: ", format!("{} -> {}", from.display(), to.display()))
        }
    };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aivyx-tui -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "Render PermissionTarget::Move in the TUI confirmation modal"
```

---

### Task 3: Map `Move` in the ACP prompter

**Files:**
- Modify: `crates/aivyx-acp/src/prompter.rs:35-41` (`target_string`), `:84-90` (the `ToolKind` mapping)
- Test: `crates/aivyx-acp/src/prompter.rs`'s `mod tests` (~line 213)

**Interfaces:**
- Consumes: `aivyx_sandbox::PermissionTarget::Move { from, to }`, `aivyx_sandbox::ActionKind::Move` (Task 1).

- [ ] **Step 1: Write the failing test**

Add near `execute_request_carries_no_diff` (~line 263):

```rust
    #[test]
    fn move_request_maps_to_move_tool_kind_and_arrow_title() {
        let request = PermissionRequest {
            tool_name: "move_file".to_string(),
            action: ActionKind::Move,
            target: PermissionTarget::Move {
                from: PathBuf::from("/project/old.rs"),
                to: PathBuf::from("/project/new.rs"),
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let acp = permission_request_to_acp(sid(), &request, "call-1");
        assert_eq!(acp.tool_call.fields.kind, Some(ToolKind::Move));
        assert_eq!(
            acp.tool_call.fields.title.as_deref(),
            Some("/project/old.rs -> /project/new.rs")
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-acp -- --test-threads=1 move_request_maps_to_move_tool_kind_and_arrow_title`
Expected: FAIL to compile — non-exhaustive match on `PermissionTarget` and/or `ActionKind`

- [ ] **Step 3: Add the `Move` arms**

Change `target_string` (~line 35-41):

```rust
fn target_string(target: &PermissionTarget) -> String {
    match target {
        PermissionTarget::Path(path) => path.display().to_string(),
        PermissionTarget::Command { program, args } => format!("{program} {}", args.join(" ")),
        PermissionTarget::Other(description) => description.clone(),
        PermissionTarget::Move { from, to } => format!("{} -> {}", from.display(), to.display()),
    }
}
```

Change the `ToolKind` mapping inside `pending_tool_call` (~line 84-90):

```rust
    let kind = match request.action {
        ActionKind::Write => ToolKind::Edit,
        ActionKind::Delete => ToolKind::Delete,
        ActionKind::Execute => ToolKind::Execute,
        ActionKind::Move => ToolKind::Move,
        ActionKind::McpTool | ActionKind::Memory | ActionKind::Interact => ToolKind::Other,
        ActionKind::Read | ActionKind::Internal => ToolKind::Other,
    };
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aivyx-acp -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-acp/src/prompter.rs
git commit -m "Map ActionKind::Move to ACP's existing ToolKind::Move"
```

---

### Task 4: `MoveFileTool` — file case (permission request, preview, execute, EXDEV)

**Files:**
- Create: `crates/aivyx-tools/src/tools/move_file.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs` (add `mod move_file;` and `pub use move_file::MoveFileTool;`)

**Interfaces:**
- Consumes: `crate::path_resolve::resolve` (existing), `aivyx_sandbox::{ActionKind::Move, PermissionTarget::Move}` (Task 1).
- Produces: `pub struct MoveFileTool { .. }` with `pub fn new(deny_paths: Vec<PathBuf>) -> Self` — Task 5 (directory support) and Task 6 (registration) both call this exact constructor. `deny_paths` is threaded in now (unused by this task's file-only logic) so Task 5 doesn't need to change the constructor's signature.

This task covers files only. `execute`'s `tokio::fs::rename` already works correctly on a directory too (single atomic syscall for the whole tree) — Task 5 only adds the *preview* and the *security scan* for the directory case, not new rename logic.

- [ ] **Step 1: Write the failing tests**

Create `crates/aivyx-tools/src/tools/move_file.rs`:

```rust
use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct MoveFileArgs {
    /// Path to the file or directory to move, absolute or relative to the working directory.
    from: String,
    /// Destination path, absolute or relative to the working directory. Must not already exist.
    to: String,
}

/// `ActionKind::Move`'s only constructor. Atomic rename via a single
/// `tokio::fs::rename` — works for files and whole directory trees on the
/// same filesystem in one syscall. Refuses if `to` already exists (no
/// overwrite mode) or if `from`/`to` end up on different filesystems
/// (`EXDEV`/`CrossesDevices` is surfaced directly, no copy+delete
/// fallback) — see the design doc's resolved questions for why both are
/// refusals, not silent alternate behavior.
pub struct MoveFileTool {
    /// Only consulted for a directory `from` (see `find_denied_descendant`
    /// in the directory-support pass) — the top-level
    /// `ConfirmationGate::is_denied` check already covers a `from`/`to`
    /// that directly matches a `deny_paths` entry; this field exists so a
    /// directory move can also refuse when a denied path lives *nested
    /// inside* the tree being relocated, the same reasoning
    /// `GrepTool`/`GlobTool` already document for needing `deny_paths`
    /// beyond `permission_request`'s single-target check.
    deny_paths: Vec<PathBuf>,
}

impl MoveFileTool {
    pub fn new(deny_paths: Vec<PathBuf>) -> Self {
        Self { deny_paths }
    }
}

#[async_trait]
impl Tool for MoveFileTool {
    fn name(&self) -> &str {
        "move_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Move or rename a file or directory. Refuses if the destination \
                already exists (no overwrite) or if source and destination are on different \
                filesystems (no copy+delete fallback). Atomic on the same filesystem."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(MoveFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let from = resolve(cwd, &args.from);
        let to = resolve(cwd, &args.to);

        std::fs::metadata(&from).map_err(|_| {
            ToolError::ExecutionFailed(format!("{} does not exist", from.display()))
        })?;
        if std::fs::metadata(&to).is_ok() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} already exists — move_file refuses to overwrite; delete it first if that's \
                 intended",
                to.display()
            )));
        }

        let preview = match std::fs::read_to_string(&from) {
            Ok(content) => format!("Move {} to {}\n\n{}", from.display(), to.display(), content),
            Err(_) => format!(
                "WARNING: {} could not be read as text (binary file?). This will move it to {} \
                 unchanged.",
                from.display(),
                to.display()
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Move,
            target: PermissionTarget::Move { from, to },
            arguments_preview: json!({ "from": args.from, "to": args.to }),
            preview: Some(preview),
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let from = resolve(&ctx.cwd, &args.from);
        let to = resolve(&ctx.cwd, &args.to);

        tokio::fs::rename(&from, &to)
            .await
            .map_err(|err| map_rename_error(err, &from, &to))?;

        Ok(ToolOutput::Ok(format!("moved {} to {}", from.display(), to.display())))
    }
}

/// Split out of `execute` so the `EXDEV` mapping is unit-testable without
/// two real filesystems in CI — construct the `io::Error` directly instead.
fn map_rename_error(err: std::io::Error, from: &Path, to: &Path) -> ToolError {
    if err.kind() == std::io::ErrorKind::CrossesDevices {
        ToolError::ExecutionFailed(format!(
            "{} and {} are on different filesystems — move_file only performs an atomic \
             same-filesystem rename and does not fall back to copy+delete",
            from.display(),
            to.display()
        ))
    } else {
        ToolError::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn execute_moves_a_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let output = tool
            .execute(json!({ "from": "old.txt", "to": "new.txt" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("moved"), "text: {text}");
        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn permission_request_is_action_move_with_a_move_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "old.txt", "to": "new.txt" }), dir.path())
            .unwrap();

        assert_eq!(request.action, ActionKind::Move);
        let PermissionTarget::Move { from, to } = &request.target else {
            panic!("expected a Move target, got {:?}", request.target);
        };
        assert_eq!(from, &dir.path().join("old.txt"));
        assert_eq!(to, &dir.path().join("new.txt"));
    }

    #[test]
    fn preview_shows_source_content_for_a_text_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "important notes\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "old.txt", "to": "new.txt" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("important notes"));
        assert!(preview.contains("old.txt"));
        assert!(preview.contains("new.txt"));
    }

    #[test]
    fn preview_warns_instead_of_showing_content_for_a_binary_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("data.bin"), [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "data.bin", "to": "moved.bin" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
        assert!(preview.contains("binary"));
    }

    #[test]
    fn a_nonexistent_source_is_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result = tool.permission_request(
            &json!({ "from": "does-not-exist.txt", "to": "new.txt" }),
            dir.path(),
        );

        assert!(matches!(result, Err(ToolError::ExecutionFailed(_))));
    }

    #[test]
    fn an_existing_destination_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "already here\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result =
            tool.permission_request(&json!({ "from": "old.txt", "to": "new.txt" }), dir.path());

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("already exists"), "message: {message}");
    }

    #[test]
    fn cross_filesystem_rename_error_is_surfaced_clearly() {
        let err = std::io::Error::from(std::io::ErrorKind::CrossesDevices);
        let mapped = map_rename_error(err, Path::new("/a/old.txt"), Path::new("/b/new.txt"));
        let ToolError::ExecutionFailed(message) = mapped else {
            panic!("expected ExecutionFailed, got {mapped:?}");
        };
        assert!(message.contains("different filesystems"), "message: {message}");
    }

    #[test]
    fn other_io_errors_pass_through_unmapped() {
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let mapped = map_rename_error(err, Path::new("/a/old.txt"), Path::new("/b/new.txt"));
        assert!(matches!(mapped, ToolError::Io(_)));
    }
}
```

Register the new module in `crates/aivyx-tools/src/tools/mod.rs`: add `mod move_file;` alphabetically (after `mod mcp_tool;`, before `mod read_file;`) and `pub use move_file::MoveFileTool;` alphabetically in the `pub use` block (after `pub use mcp_tool::McpToolAdapter;`, before `pub use read_file::ReadFileTool;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-tools -- --test-threads=1 move_file`
Expected: FAIL — `move_file` module doesn't exist yet / not registered (since the file above is created in this same step, this really just confirms the harness: run it once *before* adding the `mod`/`pub use` lines in `mod.rs` to see the "module not found" error, then add those two lines).

- [ ] **Step 3: Verify tests pass**

Run: `cargo test -p aivyx-tools -- --test-threads=1 move_file`
Expected: PASS (8 tests: `execute_moves_a_file`, `permission_request_is_action_move_with_a_move_target`, `preview_shows_source_content_for_a_text_file`, `preview_warns_instead_of_showing_content_for_a_binary_file`, `a_nonexistent_source_is_rejected`, `an_existing_destination_is_rejected`, `cross_filesystem_rename_error_is_surfaced_clearly`, `other_io_errors_pass_through_unmapped`)

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-tools/src/tools/move_file.rs crates/aivyx-tools/src/tools/mod.rs
git commit -m "Add move_file tool (file case): permission request, preview, atomic rename"
```

---

### Task 5: `MoveFileTool` — directory support (recursive deny_paths scan + capped preview listing)

**Files:**
- Modify: `crates/aivyx-tools/src/tools/move_file.rs`

**Interfaces:**
- Consumes: `ignore::WalkBuilder` (already a dependency, used the same way in `glob.rs`), `crate::path_resolve::is_denied` (existing helper, same one `grep.rs`/`glob.rs` use).

- [ ] **Step 1: Write the failing tests**

Add to `move_file.rs`'s `mod tests`:

```rust
    #[tokio::test]
    async fn execute_moves_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let output = tool
            .execute(json!({ "from": "src", "to": "lib" }), &ctx(dir.path()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("moved"), "text: {text}");
        assert!(!dir.path().join("src").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lib/a.rs")).unwrap(),
            "fn a() {}\n"
        );
    }

    #[test]
    fn an_existing_destination_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("lib")).unwrap();

        let tool = MoveFileTool::new(vec![]);
        let result = tool.permission_request(&json!({ "from": "src", "to": "lib" }), dir.path());

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains("already exists"), "message: {message}");
    }

    #[test]
    fn preview_lists_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "fn b() {}\n").unwrap();

        let tool = MoveFileTool::new(vec![]);
        let request = tool
            .permission_request(&json!({ "from": "src", "to": "lib" }), dir.path())
            .unwrap();

        let preview = request.preview.expect("expected a preview");
        assert!(preview.contains("a.rs"));
        assert!(preview.contains("b.rs"));
    }

    #[test]
    fn a_directory_move_is_refused_when_a_deny_path_is_nested_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/.env"), "SECRET=1\n").unwrap();

        let tool = MoveFileTool::new(vec![dir.path().join("secrets/.env")]);
        let result = tool.permission_request(
            &json!({ "from": "secrets", "to": "archive/secrets" }),
            dir.path(),
        );

        let Err(ToolError::ExecutionFailed(message)) = result else {
            panic!("expected ExecutionFailed, got {result:?}");
        };
        assert!(message.contains(".env"), "message: {message}");
        // Nothing was moved — the walk happens before any mutation.
        assert!(dir.path().join("secrets/.env").exists());
    }

    #[test]
    fn a_gitignored_deny_path_nested_in_the_directory_is_still_caught() {
        // Regression test for the reason this scan can't reuse grep/glob's
        // own walk unmodified: a `.env` is exactly the kind of file that's
        // both deny_paths-worthy and routinely gitignored. If the scan were
        // gitignore-aware, a nested .env would be silently invisible to it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "secrets/.env\n").unwrap();
        std::fs::create_dir(dir.path().join("secrets")).unwrap();
        std::fs::write(dir.path().join("secrets/.env"), "SECRET=1\n").unwrap();

        let tool = MoveFileTool::new(vec![dir.path().join("secrets/.env")]);
        let result = tool.permission_request(
            &json!({ "from": "secrets", "to": "archive/secrets" }),
            dir.path(),
        );

        assert!(
            matches!(result, Err(ToolError::ExecutionFailed(_))),
            "a gitignored deny_paths entry must still block the move"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-tools -- --test-threads=1 move_file`
Expected: `execute_moves_a_directory` and `an_existing_destination_directory_is_rejected` PASS already (directory rename and the destination-exists check both already work generically); `preview_lists_directory_contents` FAILS (the file-only preview logic calls `read_to_string` on a directory, hits the binary-warning branch, which doesn't mention `a.rs`/`b.rs`); the two deny_paths tests FAIL (nothing scans for nested denials yet).

- [ ] **Step 3: Add the imports, the security scan, and the directory preview**

Add to the top of `move_file.rs`:

```rust
use ignore::WalkBuilder;

use crate::path_resolve::{is_denied, resolve};
```

(replaces the existing `use crate::path_resolve::resolve;` line)

Add these two functions above `#[cfg(test)]`:

```rust
/// Independent of `permission_request`'s own `from`/`to` top-level check
/// (`ConfirmationGate::is_denied`, which only sees the two endpoints) — a
/// nested `deny_paths` entry several levels inside a directory being moved
/// would otherwise silently relocate to a path `deny_paths` no longer
/// matches. `standard_filters(false)` is deliberate and load-bearing:
/// unlike `grep`/`glob`'s gitignore-aware walk (relevance, not security),
/// this scan must see every real entry regardless of `.gitignore` — see
/// `a_gitignored_deny_path_nested_in_the_directory_is_still_caught`.
fn find_denied_descendant(root: &Path, deny_paths: &[PathBuf]) -> Option<PathBuf> {
    for entry in WalkBuilder::new(root).standard_filters(false).build() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if is_denied(path, deny_paths) {
            return Some(path.to_path_buf());
        }
    }
    None
}

/// Cosmetic only (not security-relevant, unlike `find_denied_descendant`
/// above) — gitignore-aware and capped, matching `glob.rs`'s own
/// `MAX_PATHS` truncation convention, so a human isn't shown an unbounded
/// dump for a large directory.
const MAX_LISTED_ENTRIES: usize = 200;

fn directory_listing(root: &Path) -> String {
    let mut entries: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in WalkBuilder::new(root).build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if entries.len() >= MAX_LISTED_ENTRIES {
            truncated = true;
            break;
        }
        let path = entry.path();
        entries.push(path.strip_prefix(root).unwrap_or(path).display().to_string());
    }
    let mut output = entries.join("\n");
    if truncated {
        output.push_str(&format!(
            "\n... {MAX_LISTED_ENTRIES}+ files, showing first {MAX_LISTED_ENTRIES}"
        ));
    }
    if output.is_empty() {
        output = "(empty directory)".to_string();
    }
    output
}
```

Replace `permission_request`'s body (the existence checks through the preview construction) with:

```rust
    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let from = resolve(cwd, &args.from);
        let to = resolve(cwd, &args.to);

        let metadata = std::fs::metadata(&from).map_err(|_| {
            ToolError::ExecutionFailed(format!("{} does not exist", from.display()))
        })?;
        if std::fs::metadata(&to).is_ok() {
            return Err(ToolError::ExecutionFailed(format!(
                "{} already exists — move_file refuses to overwrite; delete it first if that's \
                 intended",
                to.display()
            )));
        }

        let preview = if metadata.is_dir() {
            if let Some(denied) = find_denied_descendant(&from, &self.deny_paths) {
                return Err(ToolError::ExecutionFailed(format!(
                    "{} is under a configured deny_paths entry — refusing to move a directory \
                     that contains it (moving would relocate it outside deny_paths' protection)",
                    denied.display()
                )));
            }
            format!(
                "Move directory {} to {}\n\n{}",
                from.display(),
                to.display(),
                directory_listing(&from)
            )
        } else {
            match std::fs::read_to_string(&from) {
                Ok(content) => {
                    format!("Move {} to {}\n\n{}", from.display(), to.display(), content)
                }
                Err(_) => format!(
                    "WARNING: {} could not be read as text (binary file?). This will move it to \
                     {} unchanged.",
                    from.display(),
                    to.display()
                ),
            }
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Move,
            target: PermissionTarget::Move { from, to },
            arguments_preview: json!({ "from": args.from, "to": args.to }),
            preview: Some(preview),
            diff: None,
        })
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-tools -- --test-threads=1 move_file`
Expected: PASS (all 13 tests in the module)

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-tools/src/tools/move_file.rs
git commit -m "Add move_file directory support: nested deny_paths scan + capped preview listing"
```

---

### Task 6: Wire `move_file` into the agent and document it

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs:24-31` (import), `:226` area (registration)
- Modify: `README.md` (Tools table, Known limitations, and a short prose paragraph matching `delete_file`'s own)
- Modify: `ROADMAP.md` (remove the "Move/rename tool" backlog bullet)

**Interfaces:**
- Consumes: `aivyx_tools::MoveFileTool::new(deny_paths: Vec<PathBuf>)` (Task 4/5), the existing `deny_paths` binding already in scope at the registration site (used identically by `GrepTool::new(deny_paths.clone())` etc. two lines above).

- [ ] **Step 1: Add the import**

In `crates/aivyx/src/agent_builder.rs`, change the `use aivyx_tools::{...}` block (line 24-31) to add `MoveFileTool` alphabetically:

```rust
use aivyx_tools::{
    CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool,
    GitBranchTool, GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, MoveFileTool, ReadFileTool, ReadMcpResourceTool, RememberPreferenceTool,
    ReplSendTool, ReplStartTool, ReplStopTool, RunCommandTool, RunShellTool, SetTasksTool,
    ToolExecutor, ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool, new_shared_repl_session,
};
```

- [ ] **Step 2: Register the tool**

In `crates/aivyx/src/agent_builder.rs`, add a line right after `registry.register(Arc::new(DeleteFileTool));` (~line 226):

```rust
    registry.register(Arc::new(DeleteFileTool));
    registry.register(Arc::new(MoveFileTool::new(deny_paths.clone())));
```

- [ ] **Step 3: Build to confirm it compiles**

Run: `cargo build -p aivyx`
Expected: builds cleanly

- [ ] **Step 4: Update README.md's Tools table**

In `README.md`, add a row right after the `delete_file` row (~line 691):

```markdown
| `delete_file` | delete a file | prompt (then cacheable) |
| `move_file` | move or rename a file or directory | prompt (then cacheable) |
```

- [ ] **Step 5: Add a README prose paragraph, matching `delete_file`'s own**

In `README.md`, add right after the `delete_file(path)` paragraph (~line 490, before "Build/test the workspace:"):

```markdown
**`move_file(from, to)`**: `ActionKind::Move`'s only constructor — closes
the last item in the second capability audit's backlog. Files and
directories both supported via a single atomic `tokio::fs::rename`. Refuses
outright if `to` already exists (no overwrite mode) and if `from`/`to` are
on different filesystems (`EXDEV`/`CrossesDevices` — no copy+delete
fallback, so the tool's atomicity guarantee stays honest). A directory move
additionally scans its full contents for any nested `deny_paths` entry
before prompting — not gitignore-filtered like `grep`/`glob`'s own walk,
since a `.env` is exactly the kind of file that's both `deny_paths`-worthy
and routinely gitignored, and a gitignore-aware scan would silently miss
exactly the case it exists to catch.
```

- [ ] **Step 6: Add a README Known limitations bullet**

In `README.md`, add to the "Known limitations" list (~line 970, after the Git specifics bullet):

```markdown
- **`move_file` cross-filesystem moves**: refused outright rather than
  transparently falling back to a recursive copy+delete, to keep the
  tool's atomicity guarantee honest. In practice this only bites when
  `from`/`to` resolve onto different mounted filesystems, which is rare
  for an in-project rename.
```

- [ ] **Step 7: Remove the closed backlog item from ROADMAP.md**

In `ROADMAP.md`, delete the "**Move/rename tool**" bullet (the first bullet under "## Backlog — capability opportunities, not yet scheduled").

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs README.md ROADMAP.md
git commit -m "Wire move_file into the agent; document it; close the backlog item"
```

---

### Task 7: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Build the whole workspace**

Run: `cargo build --workspace`
Expected: builds cleanly, no warnings treated as errors

- [ ] **Step 2: Run the whole test suite**

Run: `cargo test --workspace -- --test-threads=1`
Expected: PASS — every test in every crate, including all tests added in Tasks 1-6

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets`
Expected: no warnings

- [ ] **Step 4: Manual smoke test (not automatable — requires a live TUI session)**

Run `cargo run -p aivyx` in a real project directory, ask the model (or drive it directly by hand if no model is configured) to move a file, and confirm: the permission modal renders `Move: <from> -> <to>` and the preview text, approving it actually relocates the file, and the pre-mutation checkpoint fired (`git for-each-ref refs/aivyx/checkpoints/` shows a new entry). This is the live-E2E verification step the design doc calls for — record the result in a follow-up note or commit message, but it is not a `- [ ]` step this plan can check off automatically.

If any step fails, stop and fix before proceeding — do not commit on top of a failing workspace state.
