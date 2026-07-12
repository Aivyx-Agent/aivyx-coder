# Phase 11c: Autonomous Coding Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `aivyx --auto "<goal>"` work toward a goal unattended — no permission modals — while never weakening any existing interactive-mode security property.

**Architecture:** A new `AutonomousMode` shared flag (mirroring `PlanMode` exactly) adds one tier to `ConfirmationGate::check`: writes/edits must resolve inside `cwd`, and commands resolve only against the pre-approved Always-Allow cache with no prompt fallback. `Agent` gains a discard/rewind mechanism for exhausted verification, built on two new `GitCheckpointer` methods. The loop driver lives in `aivyx-tui`'s existing background task (the one thing that already owns sequential `run_turn` calls and can inspect cancellation without any cross-task race), not as a new standalone module — see Task 8 for why.

**Tech Stack:** Rust workspace (`aivyx-sandbox`, `aivyx-tools`, `aivyx-core`, `aivyx-config`, `aivyx-tui`, `aivyx` binary), tokio, git plumbing via `tokio::process::Command`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-12-phase-11c-autonomous-loop-design.md` — every task below implements a specific section of it; read it first if anything here is ambiguous.
- No existing interactive-mode behavior may change. Every new check is gated on `autonomous_mode.active()` and is a no-op otherwise.
- Match this codebase's existing conventions exactly: tests live inline in `#[cfg(test)] mod tests` at the bottom of the same file, not separate test files; doc comments explain *why*, not *what*; `tracing::warn!` for loud-but-non-fatal conditions.
- Run `cargo test --workspace` and `cargo clippy --workspace --all-targets` after every task; both must be clean before moving to the next task.
- Commit after every task (see each task's final step).

---

## Task 1: `AutonomousMode` shared flag

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs`

**Interfaces:**
- Produces: `pub struct AutonomousMode(Arc<AtomicBool>)` with `new() -> Self`, `active(&self) -> bool`, `set_active(&self, active: bool)` — the exact same public shape as `PlanMode` (no `toggle()`, since this project's `PlanMode::toggle()` exists only for the Ctrl+P keybinding, which `AutonomousMode` has no equivalent of).

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/aivyx-sandbox/src/lib.rs` (this file currently has no `#[cfg(test)] mod tests` block — add one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomous_mode_starts_inactive_and_can_be_activated() {
        let mode = AutonomousMode::new();
        assert!(!mode.active());
        mode.set_active(true);
        assert!(mode.active());
    }

    #[test]
    fn autonomous_mode_clones_share_state() {
        let mode = AutonomousMode::new();
        let clone = mode.clone();
        mode.set_active(true);
        assert!(clone.active(), "clones must observe the same underlying flag");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-sandbox autonomous_mode`
Expected: FAIL with `error[E0433]: failed to resolve: use of undeclared type 'AutonomousMode'` (or similar — the type doesn't exist yet).

- [ ] **Step 3: Add the type**

In `crates/aivyx-sandbox/src/lib.rs`, immediately after the existing `PlanMode` block (after its `impl PlanMode { ... }` closing brace, before `#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub enum UserResponse {`), insert:

```rust
/// Shared autonomous-mode flag: while active, `ConfirmationGate` resolves
/// every decision deterministically (pre-approved commands and in-worktree
/// edits allowed, everything else denied) instead of prompting — there is
/// no human to prompt. Set once at startup from the `--auto` CLI flag; not
/// expected to toggle mid-session (unlike `PlanMode`, no keybinding flips
/// it), but the same shared-flag shape keeps `ConfirmationGate`'s
/// consumption pattern uniform. See docs/superpowers/specs/
/// 2026-07-12-phase-11c-autonomous-loop-design.md.
///
/// `Relaxed` ordering, same rationale as `PlanMode`: no data is published
/// through this flag, so there is nothing for a stricter ordering to
/// synchronize.
#[derive(Debug, Clone, Default)]
pub struct AutonomousMode(Arc<AtomicBool>);

impl AutonomousMode {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set_active(&self, active: bool) {
        self.0.store(active, Ordering::Relaxed);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aivyx-sandbox autonomous_mode`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs
git commit -m "Phase 11c: add AutonomousMode shared flag"
```

---

## Task 2: `ConfirmationGate` autonomous-mode tier

**Files:**
- Modify: `crates/aivyx-sandbox/src/confirmation.rs`

**Interfaces:**
- Consumes: `AutonomousMode` from Task 1 (`use crate::AutonomousMode;`).
- Produces: `ConfirmationGate::new` gains a 5th parameter `autonomous_mode: AutonomousMode` (positioned after `plan_mode: PlanMode`, matching the spec's tier ordering — plan-mode deny is checked first). Every existing call site must be updated (this task updates the ones inside this file's own tests; Task 9 updates `main.rs`; the `agent.rs` test helpers in Task 5 do NOT construct `ConfirmationGate` directly, so they're unaffected).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/aivyx-sandbox/src/confirmation.rs` (after the existing `plain_deny_is_never_cached` test, before the closing `}` of `mod tests`):

```rust
    #[tokio::test]
    async fn autonomous_mode_allows_edits_inside_cwd() {
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
        );

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;

        assert_eq!(decision, PermissionDecision::Allow);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never prompt"
        );
    }

    #[tokio::test]
    async fn autonomous_mode_denies_edits_outside_cwd() {
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
        );

        let request = PermissionRequest {
            tool_name: "write_file".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(PathBuf::from("/etc/passwd")),
            arguments_preview: serde_json::json!({}),
            preview: None,
        };
        let decision = gate.check(&request).await;

        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a denial with a reason, got {decision:?}");
        };
        assert!(reason.contains("cwd") || reason.contains("worktree"), "reason: {reason}");
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn autonomous_mode_allows_pre_approved_commands_only() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![],
            vec![("cargo".to_string(), vec!["test".to_string()])],
            PlanMode::new(),
            autonomous_mode,
        );

        let approved = PermissionRequest {
            tool_name: "run_command".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["test".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
        };
        assert_eq!(gate.check(&approved).await, PermissionDecision::AllowAlways);

        let not_approved = PermissionRequest {
            target: PermissionTarget::Command {
                program: "cargo".to_string(),
                args: vec!["publish".to_string()],
            },
            ..approved
        };
        assert!(matches!(
            gate.check(&not_approved).await,
            PermissionDecision::Deny(_)
        ));
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            0,
            "autonomous mode must never fall back to prompting for an unapproved command"
        );
    }

    #[tokio::test]
    async fn autonomous_mode_denies_git_commit_with_no_special_casing() {
        // git_commit's target is a unique commit message every time (Phase 7's
        // design specifically to prevent blanket-approval), so it can never be
        // in the Always-Allow cache — this is a regression test proving the
        // denial falls out of that existing property, not code that could
        // later be "simplified" away.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![], PlanMode::new(), autonomous_mode);

        let request = PermissionRequest {
            tool_name: "git_commit".to_string(),
            action: ActionKind::Execute,
            target: PermissionTarget::Command {
                program: "git".to_string(),
                args: vec!["commit".to_string(), "-m".to_string(), "anything".to_string()],
            },
            arguments_preview: serde_json::json!({}),
            preview: None,
        };
        assert!(matches!(gate.check(&request).await, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn plan_mode_still_wins_over_autonomous_mode_if_both_are_active() {
        // Defense in depth: this is not an expected state (autonomous and
        // plan mode are mutually exclusive at the CLI level, enforced in
        // main.rs), but if it ever happened, the stricter mode must win.
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let plan_mode = PlanMode::new();
        plan_mode.set_active(true);
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(prompter.clone(), vec![], vec![], plan_mode, autonomous_mode);

        let decision = gate
            .check(&write_request("/home/user/project/src/a.rs"))
            .await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a plan-mode denial, got {decision:?}");
        };
        assert!(reason.contains("plan mode"));
    }

    #[tokio::test]
    async fn deny_paths_still_wins_over_autonomous_mode() {
        let prompter = Arc::new(FakePrompter {
            response: UserResponse::Allow,
            calls: AtomicUsize::new(0),
        });
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let gate = ConfirmationGate::new(
            prompter.clone(),
            vec![PathBuf::from("/home/user/project/secret")],
            vec![],
            PlanMode::new(),
            autonomous_mode,
        );

        let decision = gate
            .check(&write_request("/home/user/project/secret/key"))
            .await;
        let PermissionDecision::Deny(Some(reason)) = decision else {
            panic!("expected a deny_paths denial, got {decision:?}");
        };
        assert!(reason.contains("deny_paths"));
    }
```

Also add `AutonomousMode` to the `use super::*;` scope — since `mod tests { use super::*; ... }` already imports everything `pub(crate)`/`pub` from the parent module, and `AutonomousMode` will be `use crate::AutonomousMode;` in the parent module (added in Step 3), no extra import is needed in the test module itself.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-sandbox autonomous_mode`
Expected: FAIL to compile — `ConfirmationGate::new` takes 4 arguments, 5 were supplied.

- [ ] **Step 3: Implement the tier**

In `crates/aivyx-sandbox/src/confirmation.rs`, change the import line:

```rust
use crate::{
    ActionKind, PermissionDecision, PermissionGate, PermissionPrompter, PermissionRequest,
    PermissionTarget, PlanMode, UserResponse, path_is_denied,
};
```

to:

```rust
use crate::{
    ActionKind, AutonomousMode, PermissionDecision, PermissionGate, PermissionPrompter,
    PermissionRequest, PermissionTarget, PlanMode, UserResponse, path_is_denied,
};
```

Add a new constant right after `PLAN_MODE_DENIAL`:

```rust
/// Told to the model when an autonomous-mode edit/write target resolves
/// outside the worktree it was launched in. Unlike every other denial
/// reason in this gate, this one has no interactive-mode equivalent to
/// point back to (autonomous mode has no confirmation modal for a human to
/// reject it from) — see the "gap found during design" note in the Phase
/// 11c design doc for why this check exists at all.
const AUTONOMOUS_OUTSIDE_CWD_DENIAL: &str =
    "target is outside the autonomous session's worktree boundary — file edits in \
     autonomous mode are confined to the working directory the session was launched in.";
```

Change the `ConfirmationGate` struct to add the field:

```rust
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
    plan_mode: PlanMode,
    autonomous_mode: AutonomousMode,
    always_allow: Mutex<HashSet<PermissionKey>>,
}
```

Change `ConfirmationGate::new`'s signature and body:

```rust
    pub fn new(
        prompter: Arc<dyn PermissionPrompter>,
        deny_paths: Vec<PathBuf>,
        pre_approved_commands: Vec<(String, Vec<String>)>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
    ) -> Self {
        let always_allow = pre_approved_commands
            .into_iter()
            .map(|(program, args)| PermissionKey::Command { program, args })
            .collect();
        Self {
            prompter,
            deny_paths,
            plan_mode,
            autonomous_mode,
            always_allow: Mutex::new(always_allow),
        }
    }
```

Add a new private helper method, right after `is_denied`:

```rust
    /// The autonomous-mode edit boundary: a `Write`/`Delete` action on a
    /// `Path` target is only in-scope if the resolved path is at-or-under
    /// `cwd`. Only meaningful for autonomous mode — interactive mode relies
    /// on a human seeing the target in the confirmation modal instead (see
    /// the Phase 11c design doc's "gap found during design" section).
    fn is_outside_autonomous_worktree(&self, request: &PermissionRequest, cwd: &Path) -> bool {
        if !matches!(request.action, ActionKind::Write | ActionKind::Delete) {
            return false;
        }
        let PermissionTarget::Path(path) = &request.target else {
            return false;
        };
        !path.starts_with(cwd)
    }
```

This introduces a new problem: `ConfirmationGate::check` doesn't currently know `cwd` — nothing in `PermissionRequest` or `ConfirmationGate`'s fields carries it. Every tool's `permission_request` already resolves paths to *absolute* paths before building the `PermissionTarget::Path` (confirmed across `read_file.rs`, `write_file.rs`, `edit_file.rs` — all call `path_resolve::resolve(cwd, &args.path)` first), so `ConfirmationGate` needs its own copy of `cwd` to compare against. Add it as a new field, set once at construction (the working directory doesn't change mid-session):

Revise the struct and constructor once more — add `cwd: PathBuf`:

```rust
pub struct ConfirmationGate {
    prompter: Arc<dyn PermissionPrompter>,
    deny_paths: Vec<PathBuf>,
    plan_mode: PlanMode,
    autonomous_mode: AutonomousMode,
    cwd: PathBuf,
    always_allow: Mutex<HashSet<PermissionKey>>,
}
```

```rust
    pub fn new(
        prompter: Arc<dyn PermissionPrompter>,
        deny_paths: Vec<PathBuf>,
        pre_approved_commands: Vec<(String, Vec<String>)>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        cwd: PathBuf,
    ) -> Self {
        let always_allow = pre_approved_commands
            .into_iter()
            .map(|(program, args)| PermissionKey::Command { program, args })
            .collect();
        Self {
            prompter,
            deny_paths,
            plan_mode,
            autonomous_mode,
            cwd,
            always_allow: Mutex::new(always_allow),
        }
    }
```

Every test written in Step 1 above must pass `PathBuf::from("/home/user/project")` (or similar) as this new final `cwd` argument, and every `write_request(...)`-built path in those new tests must be *inside* that cwd for the "allow" case and outside it for the "deny" case. Go back and add `PathBuf::from("/home/user/project")` as the 6th argument to every `ConfirmationGate::new(...)` call added in Step 1 (they currently only have 5 arguments after adding `autonomous_mode` — add `cwd` as the 6th). The existing `write_request`/paths already used in Step 1 (`/home/user/project/src/a.rs` inside, `/etc/passwd` outside) already match this cwd correctly — no path changes needed, just add the extra constructor argument.

Also update every *pre-existing* test in this file's `mod tests` block that constructs `ConfirmationGate::new(...)` — they all currently pass 4 arguments; add `AutonomousMode::new()` (inactive) and `PathBuf::from("/home/user/project")` as the 5th and 6th arguments to each. Run `grep -n "ConfirmationGate::new(" crates/aivyx-sandbox/src/confirmation.rs` first to get the exact list — there are **12** pre-existing call sites (one in each of: `deny_paths_short_circuits_without_prompting`, `reads_auto_allow_without_prompting`, `internal_actions_auto_allow_without_prompting`, `always_allow_caches_per_exact_target_only`, `deny_paths_wins_over_read_auto_allow`, `always_allow_for_a_command_does_not_cover_a_different_argv_with_the_same_program`, `pre_approved_commands_skip_the_prompt_entirely`, `plan_mode_denies_mutations_even_when_cached_or_pre_approved`, `plan_mode_leaves_reads_and_internal_actions_untouched`, `deny_paths_still_win_inside_plan_mode`, `toggling_plan_mode_off_restores_cached_approvals_without_reprompting`, `plain_deny_is_never_cached`). Each call's last argument before the closing `)` is a `PlanMode`-typed expression (either `PlanMode::new()` inline, or a named `plan_mode`/`plan_mode.clone()` binding declared earlier in that test) — leave that argument exactly as-is and insert two new arguments immediately after it, before the closing `)`: `AutonomousMode::new()` then `PathBuf::from("/home/user/project")`. None of these 12 tests activate autonomous mode, so the exact cwd value is inert for them — it only needs to be a valid `PathBuf` for the signature to typecheck.

Finally, implement the tier itself in `check`. Change:

```rust
        // Everything past this point mutates or executes. The plan-mode
        // check MUST sit before the Always-Allow cache and the pre-approved
        // `allowed_commands` lookup below — an approval granted before plan
        // mode was entered must not leak through it.
        if self.plan_mode.active() {
            tracing::warn!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission denied: plan mode is active"
            );
            return PermissionDecision::Deny(Some(PLAN_MODE_DENIAL.to_string()));
        }

        let key = PermissionKey::from_request(request);
```

to:

```rust
        // Everything past this point mutates or executes. The plan-mode
        // check MUST sit before the Always-Allow cache and the pre-approved
        // `allowed_commands` lookup below — an approval granted before plan
        // mode was entered must not leak through it.
        if self.plan_mode.active() {
            tracing::warn!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission denied: plan mode is active"
            );
            return PermissionDecision::Deny(Some(PLAN_MODE_DENIAL.to_string()));
        }

        // Autonomous mode: resolve deterministically, never prompt (there is
        // no human to prompt). Checked before the Always-Allow cache lookup
        // below because the cwd-boundary check applies to Write/Delete
        // targets that the cache path doesn't otherwise examine.
        if self.autonomous_mode.active() {
            if self.is_outside_autonomous_worktree(request, &self.cwd) {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: outside the autonomous worktree boundary"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_OUTSIDE_CWD_DENIAL.to_string()));
            }
            let key = PermissionKey::from_request(request);
            return if self.always_allow.lock().unwrap().contains(&key) {
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
                    "not pre-approved, and autonomous mode has no one to prompt — add this to \
                     [[permissions.allowed_commands]] if it should be allowed"
                        .to_string(),
                ))
            };
        }

        let key = PermissionKey::from_request(request);
```

Note this correctly falls through the Write/Delete-on-Path case too: the cwd check runs first (denying if outside), and if it passes, the SAME `always_allow` cache lookup used for commands is *also* consulted for paths — which is harmless because no path ever gets seeded into that cache in autonomous mode (`pre_approved_commands` only ever contains `Command` keys, never `Path` keys), so a path that passed the cwd check would incorrectly deny here. Fix: the cwd check must directly `Allow` a Path target that passes, not fall through to the cache lookup. Revise the block above — replace the `let key = ...; return if ... { AllowAlways } else { Deny }` portion with a match on the target type:

```rust
        if self.autonomous_mode.active() {
            if self.is_outside_autonomous_worktree(request, &self.cwd) {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: outside the autonomous worktree boundary"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_OUTSIDE_CWD_DENIAL.to_string()));
            }
            return match &request.target {
                // Already passed the cwd-boundary check above (or wasn't a
                // Write/Delete-on-Path at all, e.g. a Read/Internal target
                // reaching here would be unusual since tier 2 already
                // caught those — but Other targets like set_tasks's aren't
                // Path/Command, so they fall here too and must be allowed).
                PermissionTarget::Path(_) | PermissionTarget::Other(_) => {
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
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-sandbox`
Expected: PASS, all tests in the crate (the 6 new ones plus every pre-existing one, now updated for the 2 new constructor arguments).

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p aivyx-sandbox --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-sandbox/src/confirmation.rs
git commit -m "Phase 11c: ConfirmationGate autonomous-mode tier (cwd boundary + no-prompt commands)"
```

---

## Task 3: `GitCheckpointer::latest_ref` and `restore_to`

**Files:**
- Modify: `crates/aivyx-tools/src/checkpoint.rs`

**Interfaces:**
- Consumes: `self.git(&args, &envs, cancellation)` (existing private method, line ~175), `self.git_dir`, `self.cwd`, `self.last_tree` (existing fields).
- Produces: `pub async fn latest_ref(&self, cancellation: &CancellationToken) -> Option<String>`, `pub async fn restore_to(&self, ref_name: &str, cancellation: &CancellationToken) -> Result<(), String>`.

**Implementation note carried over from the design doc**: a plain `git checkout <ref> -- .` does not delete files added since the checkpoint. The sequence below (`add -A` into the private index to capture the *current* dirty state, then `read-tree --reset -u <ref>`) is the standard git idiom for a full worktree reset to an arbitrary tree without touching the real HEAD/index — `read-tree`'s deletion logic works by diffing the index it starts from against the tree it resets to, so the index must first reflect what's actually on disk. The tests below are the verification the design doc asked for.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/aivyx-tools/src/checkpoint.rs` (after the existing `retention_prunes_the_oldest_refs` test):

```rust
    #[tokio::test]
    async fn latest_ref_returns_the_most_recent_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cp = GitCheckpointer::detect(dir.path(), vec![]).await.unwrap();
        assert!(
            cp.latest_ref(&CancellationToken::new()).await.is_none(),
            "no checkpoints taken yet"
        );

        std::fs::write(dir.path().join("tracked.txt"), "v2\n").unwrap();
        cp.checkpoint("write_file", &CancellationToken::new()).await;
        let first = cp.latest_ref(&CancellationToken::new()).await.unwrap();

        std::fs::write(dir.path().join("tracked.txt"), "v3\n").unwrap();
        cp.checkpoint("write_file", &CancellationToken::new()).await;
        let second = cp.latest_ref(&CancellationToken::new()).await.unwrap();

        assert_ne!(first, second, "the ref must advance after a new checkpoint");
    }

    #[tokio::test]
    async fn restore_to_reverts_modified_content() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cp = GitCheckpointer::detect(dir.path(), vec![]).await.unwrap();
        let before = cp.latest_ref(&CancellationToken::new()).await;
        assert!(before.is_none());

        // Checkpoint the known-good state, then make a bad edit.
        cp.checkpoint("write_file", &CancellationToken::new()).await;
        let good_ref = cp.latest_ref(&CancellationToken::new()).await.unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "broken\n").unwrap();

        cp.restore_to(&good_ref, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "v1\n",
            "content must revert to what the checkpoint captured"
        );
    }

    #[tokio::test]
    async fn restore_to_deletes_files_added_since_the_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cp = GitCheckpointer::detect(dir.path(), vec![]).await.unwrap();

        cp.checkpoint("write_file", &CancellationToken::new()).await;
        let good_ref = cp.latest_ref(&CancellationToken::new()).await.unwrap();

        // Simulate a discarded experiment that created a brand-new file —
        // this is exactly what a plain `git checkout <ref> -- .` would fail
        // to clean up, since checkout only updates paths present in <ref>.
        std::fs::write(dir.path().join("newly_created.txt"), "oops\n").unwrap();

        cp.restore_to(&good_ref, &CancellationToken::new())
            .await
            .unwrap();

        assert!(
            !dir.path().join("newly_created.txt").exists(),
            "restore_to must delete files created since the checkpoint"
        );
    }

    #[tokio::test]
    async fn restore_to_leaves_head_and_index_untouched() {
        // Same promise checkpointing itself makes — a rewind must not
        // surprise the user's own git workflow.
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cp = GitCheckpointer::detect(dir.path(), vec![]).await.unwrap();
        cp.checkpoint("write_file", &CancellationToken::new()).await;
        let good_ref = cp.latest_ref(&CancellationToken::new()).await.unwrap();

        let head_before = run_git(dir.path(), &["rev-parse", "HEAD"], &[]).await.unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "broken\n").unwrap();

        cp.restore_to(&good_ref, &CancellationToken::new())
            .await
            .unwrap();

        let head_after = run_git(dir.path(), &["rev-parse", "HEAD"], &[]).await.unwrap();
        assert_eq!(head_before, head_after);
        // The real index must show no staged changes from the restore.
        let status = run_git(dir.path(), &["status", "--porcelain"], &[]).await.unwrap();
        assert_eq!(status.trim(), "", "restore_to must not touch the real index");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-tools latest_ref -- --test-threads=1` and `cargo test -p aivyx-tools restore_to -- --test-threads=1`
Expected: FAIL to compile — `latest_ref`/`restore_to` don't exist on `GitCheckpointer` yet. (`--test-threads=1` matters for the real-git tests in this file generally, matching this file's existing pattern of real subprocess execution against temp repos — not strictly required to *see* the compile failure, but use it once implementing so real-git tests don't interfere with each other's working directories.)

- [ ] **Step 3: Implement the methods**

In `crates/aivyx-tools/src/checkpoint.rs`, add these two methods to `impl GitCheckpointer`, right after the existing `prune` method (before the private `async fn git(...)` helper):

```rust
    /// The most recent checkpoint ref, or `None` if none have been taken
    /// yet. Reuses `for-each-ref`'s default lexical sort — checkpoint ref
    /// names are zero-padded-millis-prefixed, so lexical order is
    /// chronological order, the same property `prune` above already relies
    /// on for its retention cutoff.
    pub async fn latest_ref(&self, cancellation: &CancellationToken) -> Option<String> {
        let list_args: Vec<String> = vec![
            "for-each-ref".into(),
            "--format=%(refname)".into(),
            "refs/aivyx/checkpoints/".into(),
        ];
        let refs = self.git(&list_args, &[], cancellation).await.ok()?;
        refs.lines()
            .filter(|l| !l.is_empty())
            .next_back()
            .map(str::to_string)
    }

    /// Restores the worktree to exactly match `ref_name`'s tree — including
    /// deleting files created since that checkpoint, which a plain
    /// `git checkout <ref> -- .` would not do. Uses the same private index
    /// checkpointing itself uses (`GIT_INDEX_FILE`-scoped), never touching
    /// the user's real index, HEAD, or branch. See the Phase 11c design doc
    /// for why this exact sequence (stage the current dirty state, then
    /// `read-tree --reset -u`) is needed rather than a simpler checkout.
    pub async fn restore_to(
        &self,
        ref_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        let index_dir = self.git_dir.join("aivyx");
        std::fs::create_dir_all(&index_dir).map_err(|e| e.to_string())?;
        let index = index_dir.join("index");
        let index_env: Vec<(&str, &str)> =
            vec![("GIT_INDEX_FILE", index.to_str().ok_or("non-utf8 git dir")?)];

        // Stage the CURRENT (post-experiment, possibly broken) worktree
        // into the private index first, so read-tree below knows what to
        // remove as well as what to restore — its deletion logic diffs the
        // index it's resetting FROM against the tree it's resetting TO.
        let add_args: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), ".".into()];
        self.git(&add_args, &index_env, cancellation).await?;

        let reset_args: Vec<String> = vec![
            "read-tree".into(),
            "--reset".into(),
            "-u".into(),
            ref_name.to_string(),
        ];
        self.git(&reset_args, &index_env, cancellation).await?;

        // The dedup cache no longer reflects the worktree (which just
        // changed out from under it) — invalidate rather than compute the
        // restored tree's oid; a harmless extra checkpoint next time beats
        // a false "identical, skip it" that would silently miss a real
        // change.
        *self.last_tree.lock().unwrap() = None;
        Ok(())
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-tools checkpoint:: -- --test-threads=1`
Expected: PASS, all tests in `checkpoint.rs` including the 4 new ones.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p aivyx-tools --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/checkpoint.rs
git commit -m "Phase 11c: GitCheckpointer::latest_ref and restore_to"
```

---

## Task 4: `ToolExecutor` checkpoint-query/restore wrapper methods

**Files:**
- Modify: `crates/aivyx-tools/src/lib.rs`

**Interfaces:**
- Consumes: `GitCheckpointer::latest_ref`/`restore_to` from Task 3, `self.checkpointer: Option<Arc<GitCheckpointer>>` (existing field).
- Produces: `pub async fn latest_checkpoint_ref(&self, cancellation: &CancellationToken) -> Option<String>`, `pub async fn restore_to_checkpoint(&self, ref_name: &str, cancellation: &CancellationToken) -> Result<(), String>` on `ToolExecutor`. These exist so `aivyx-core`'s `Agent` never needs to import `aivyx_tools::GitCheckpointer` directly — `ToolExecutor` stays the sole encapsulation boundary around checkpointing, matching how it already hides `checkpointer` as a private field.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/aivyx-tools/src/lib.rs` (after the existing `dispatch_checkpoints_before_mutating_tools_only` test):

```rust
    #[tokio::test]
    async fn latest_checkpoint_ref_and_restore_delegate_to_the_checkpointer() {
        use crate::checkpoint::test_support::init_repo;

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let cwd = dir.path().canonicalize().unwrap();

        let registry = ToolRegistry::new();
        let mut executor = ToolExecutor::new(
            registry,
            Arc::new(AllowAllForThisTest),
            Arc::new(NoopConfiner),
        );

        // No checkpointer configured: both wrappers degrade gracefully.
        assert!(
            executor
                .latest_checkpoint_ref(&CancellationToken::new())
                .await
                .is_none()
        );
        assert!(
            executor
                .restore_to_checkpoint("whatever", &CancellationToken::new())
                .await
                .is_err()
        );

        executor.set_checkpointer(Arc::new(
            GitCheckpointer::detect(&cwd, vec![]).await.unwrap(),
        ));
        std::fs::write(cwd.join("tracked.txt"), "v2\n").unwrap();
        executor
            .checkpointer
            .as_ref()
            .unwrap()
            .checkpoint("test", &CancellationToken::new())
            .await;

        let ref_name = executor
            .latest_checkpoint_ref(&CancellationToken::new())
            .await
            .expect("a checkpoint was just taken");

        std::fs::write(cwd.join("tracked.txt"), "broken\n").unwrap();
        executor
            .restore_to_checkpoint(&ref_name, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
            "v2\n"
        );
    }

    struct AllowAllForThisTest;
    #[async_trait]
    impl PermissionGate for AllowAllForThisTest {
        async fn check(&self, _r: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
```

Note: this test reaches `executor.checkpointer` directly (a private field) — since the test module is `mod tests { use super::*; ... }` inside the *same file* as `ToolExecutor`'s definition, this is a same-module access and compiles fine without any visibility change.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-tools latest_checkpoint_ref_and_restore -- --test-threads=1`
Expected: FAIL to compile — `latest_checkpoint_ref`/`restore_to_checkpoint` don't exist on `ToolExecutor` yet.

- [ ] **Step 3: Implement the wrappers**

In `crates/aivyx-tools/src/lib.rs`, add these two methods to `impl ToolExecutor`, right after `set_checkpointer`:

```rust
    /// The most recent checkpoint ref, or `None` if no checkpointer is
    /// configured (checkpointing disabled, or `cwd` isn't a git repo) or
    /// none has been taken yet. `Agent` (Phase 11c's autonomous discard
    /// path) uses this to remember "state right before the first unverified
    /// edit" without needing to know `GitCheckpointer` exists.
    pub async fn latest_checkpoint_ref(&self, cancellation: &CancellationToken) -> Option<String> {
        self.checkpointer.as_ref()?.latest_ref(cancellation).await
    }

    /// Restores the worktree to `ref_name` — see
    /// `GitCheckpointer::restore_to` for exactly what that means. `Err` if
    /// no checkpointer is configured (nothing to restore from) or the
    /// underlying git operation fails.
    pub async fn restore_to_checkpoint(
        &self,
        ref_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        let checkpointer = self
            .checkpointer
            .as_ref()
            .ok_or("no checkpointer is configured")?;
        checkpointer.restore_to(ref_name, cancellation).await
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p aivyx-tools latest_checkpoint_ref_and_restore -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: Run full crate tests and clippy**

Run: `cargo test -p aivyx-tools -- --test-threads=1 && cargo clippy -p aivyx-tools --all-targets`
Expected: PASS / clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tools/src/lib.rs
git commit -m "Phase 11c: ToolExecutor checkpoint query/restore wrappers"
```

---

## Task 5: `Agent` plumbing — `autonomous_mode`, `last_turn_paused`, tool filtering, system prompt

**Files:**
- Modify: `crates/aivyx-core/src/agent.rs`

**Interfaces:**
- Consumes: `AutonomousMode` from Task 1 (`use aivyx_sandbox::{AutonomousMode, PlanMode};`).
- Produces: `Agent::new` gains a new parameter `autonomous_mode: AutonomousMode` (positioned right after `plan_mode: PlanMode`); new `pub fn last_turn_paused(&self) -> bool` accessor. **Every existing `Agent::new` call site in this file's tests, and in `main.rs` (Task 9), must be updated** — this task only updates the ones in this file.

This task is plumbing only (no discard/rewind logic yet — that's Task 6, which depends on Task 4's `ToolExecutor` wrappers).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/aivyx-core/src/agent.rs`, after `a_paused_turn_resumes_cleanly_from_a_follow_up_message` and before the `// ----- enforced verification (Phase 12 Part B) -----` comment:

```rust
    // ----- autonomous mode (Phase 11c) -----

    fn build_autonomous_agent(
        responses: Vec<Vec<StreamEvent>>,
        registry: ToolRegistry,
    ) -> (Agent, UnboundedReceiver<AgentEvent>, Arc<MockBackend>, AutonomousMode) {
        let (tx, rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(responses));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            PlanMode::new(),
            autonomous_mode.clone(),
            tx,
        );
        (agent, rx, mock, autonomous_mode)
    }

    #[tokio::test]
    async fn last_turn_paused_reflects_the_most_recent_turn_outcome() {
        // build_autonomous_agent's max_tool_iterations (10) is too high to
        // pause on a single tool call, so this test builds its own agent
        // directly with max_tool_iterations: 1 instead of using that helper.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        let (tx, _rx2) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![vec![
            StreamEvent::ToolCallComplete(tool_call("c", "read_file")),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ]]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let executor = ToolExecutor::new(registry, gate, confiner);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 1,
                ..Default::default()
            },
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );

        assert!(!agent.last_turn_paused(), "false before any turn has run");
        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();
        assert!(agent.last_turn_paused(), "the 1-iteration cap must have paused this turn");

        agent
            .run_turn(
                "continue".to_string(),
                Path::new("."),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // Second call exhausts the mock queue -> a Stop response with no
        // tool calls -> TurnComplete, not another pause.
        assert!(!agent.last_turn_paused(), "a normal completion must clear the flag");
    }

    #[tokio::test]
    async fn autonomous_mode_hides_run_shell_and_git_commit() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::ReadFileTool));
        registry.register(Arc::new(aivyx_tools::RunShellTool));
        registry.register(Arc::new(aivyx_tools::GitCommitTool::new(vec![])));
        registry.register(Arc::new(aivyx_tools::GitReadTool::new(vec![])));

        let (mut agent, _rx, mock, _) =
            build_autonomous_agent(vec![text_response("hi")], registry);

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let tool_names: Vec<&str> = received[0].tools.iter().map(|d| d.name.as_str()).collect();
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"git_read"));
        assert!(!tool_names.contains(&"run_shell"), "run_shell must be hidden");
        assert!(!tool_names.contains(&"git_commit"), "git_commit must be hidden");
    }

    #[tokio::test]
    async fn autonomous_mode_appends_the_autonomous_prompt_note() {
        let (mut agent, _rx, mock, _) =
            build_autonomous_agent(vec![text_response("hi")], ToolRegistry::new());

        agent
            .run_turn("go".to_string(), Path::new("."), CancellationToken::new())
            .await
            .unwrap();

        let received = mock.received.lock().unwrap();
        let system = received[0].messages[0].text_content();
        assert!(system.contains("unattended"), "system prompt: {system}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-core autonomous`
Expected: FAIL to compile — `AutonomousMode` unresolved, `Agent::new` arity mismatch, `last_turn_paused` doesn't exist.

- [ ] **Step 3: Implement the plumbing**

In `crates/aivyx-core/src/agent.rs`, change the import:

```rust
use aivyx_sandbox::PlanMode;
```

to:

```rust
use aivyx_sandbox::{AutonomousMode, PlanMode};
```

Add a new constant, right after `VERIFICATION_PROMPT`:

```rust
/// The tools hidden from the model while autonomous mode is active — belt-
/// and-braces with `ConfirmationGate`'s independent denial of both (Phase
/// 11c): `run_shell` would almost always be denied anyway (only an exact
/// `allowed_commands` match could pass), and `git_commit` is *always*
/// denied in autonomous mode (checkpoints are the record; a human reviews
/// and commits afterward) — offering either would just invite the small-
/// model retry-loop-on-unavailable-action failure mode Phase 8 already
/// found and designed plan mode around.
const AUTONOMOUS_HIDDEN_TOOLS: &[&str] = &["run_shell", "git_commit"];

/// Appended to the system prompt while autonomous mode is active.
const AUTONOMOUS_PROMPT: &str = "You are running unattended (autonomous mode): no human will \
approve your actions. Edits inside the project directory are automatically approved — do not \
wait for confirmation, it will never come. Verification runs automatically after your edits; if \
it fails, fix the issue based on the output fed back to you. Use set_tasks to track your plan: \
marking every task done is how you signal the goal is achieved and this session should stop. \
Leaving tasks incomplete means you will be prompted to continue working toward the goal.";
```

Add two new fields to `struct Agent`, right after `plan_mode: PlanMode,`:

```rust
    /// Read at every request assembly (tool list + system-prompt note) and
    /// consulted by the discard/rewind path (Task 6); the gate holds its
    /// own clone for enforcement. See ROADMAP.md Phase 11c.
    autonomous_mode: AutonomousMode,
    /// Set to `true` right before emitting `AgentEvent::TurnPaused`, `false`
    /// at the start of every `run_turn`/`run_council_turn` call and right
    /// before emitting `AgentEvent::TurnComplete`. Exists so a caller that
    /// owns `agent: Agent` directly (the autonomous driver, Task 8) can
    /// synchronously tell "did the turn I just ran pause or complete"
    /// without racing the `AgentEvent` stream across tasks — see the Phase
    /// 11c design doc / plan for why event-stream inference is unreliable
    /// here.
    last_turn_paused: bool,
```

Update `Agent::new`'s signature and body:

```rust
    pub fn new(
        llm: std::sync::Arc<dyn LlmBackend>,
        executor: ToolExecutor,
        system_prompt: impl Into<String>,
        config: AgentConfig,
        tasks: Arc<Mutex<Vec<Task>>>,
        plan_mode: PlanMode,
        autonomous_mode: AutonomousMode,
        events_tx: UnboundedSender<AgentEvent>,
    ) -> Self {
        Self {
            llm,
            executor,
            system_prompt: system_prompt.into(),
            history: Vec::new(),
            // A misconfigured 0 would otherwise make every turn a silent
            // no-op (the tool-loop range would simply never iterate).
            max_tool_iterations: config.max_tool_iterations.max(1),
            // A 0 here would make the budget indicator meaningless and
            // trigger compaction constantly; clamp to a floor.
            context_limit: config.context_tokens.max(1),
            last_request_chars: None,
            chars_per_token: DEFAULT_CHARS_PER_TOKEN,
            history_truncated: false,
            tasks,
            session_path: None,
            plan_mode,
            autonomous_mode,
            last_turn_paused: false,
            repo_map: None,
            repo_map_text: None,
            edit_format: config.edit_format,
            synthetic_seq: 0,
            council: None,
            verification: None,
            unverified_edits: false,
            verify_retries: 0,
            events_tx,
        }
    }

    /// Whether the most recent `run_turn` call paused (Phase 12A's
    /// `AgentEvent::TurnPaused`) rather than completing normally. See the
    /// `last_turn_paused` field's doc comment for why this exists.
    pub fn last_turn_paused(&self) -> bool {
        self.last_turn_paused
    }
```

Update `assemble_messages` to append `AUTONOMOUS_PROMPT`. Change:

```rust
        // Not taught during plan mode — no edits happen there, so
        // verification can never actually fire (see `run_turn_inner`).
        if self.verification.is_some() && !self.plan_mode.active() {
            system.push_str("\n\n");
            system.push_str(VERIFICATION_PROMPT);
        }
        if let Some(map) = &self.repo_map_text {
```

to:

```rust
        // Not taught during plan mode — no edits happen there, so
        // verification can never actually fire (see `run_turn_inner`).
        if self.verification.is_some() && !self.plan_mode.active() {
            system.push_str("\n\n");
            system.push_str(VERIFICATION_PROMPT);
        }
        if self.autonomous_mode.active() {
            system.push_str("\n\n");
            system.push_str(AUTONOMOUS_PROMPT);
        }
        if let Some(map) = &self.repo_map_text {
```

Update the tool-list assembly in `run_turn_inner` to filter `AUTONOMOUS_HIDDEN_TOOLS`. Change:

```rust
                tools: {
                    let mut tools = if self.plan_mode.active() {
                        self.executor.plan_definitions()
                    } else {
                        self.executor.definitions()
                    };
                    if self.edit_format == EditFormat::Prompted {
                        tools.retain(|d| !PROMPTED_EDIT_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    tools
                },
```

to:

```rust
                tools: {
                    let mut tools = if self.plan_mode.active() {
                        self.executor.plan_definitions()
                    } else {
                        self.executor.definitions()
                    };
                    if self.edit_format == EditFormat::Prompted {
                        tools.retain(|d| !PROMPTED_EDIT_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    if self.autonomous_mode.active() {
                        tools.retain(|d| !AUTONOMOUS_HIDDEN_TOOLS.contains(&d.name.as_str()));
                    }
                    tools
                },
```

Set `last_turn_paused = false` at the start of a turn and `true`/`false` at each exit point. In `run_turn` (the public wrapper), change:

```rust
    pub async fn run_turn(
        &mut self,
        user_input: String,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        // Commands are intercepted here, before the input can enter LLM
        // history — the raw `/council …` text is an instruction to aivyx,
        // not part of the conversation the model should see.
        let result = match crate::council::parse_command(&user_input) {
```

to:

```rust
    pub async fn run_turn(
        &mut self,
        user_input: String,
        cwd: &Path,
        cancellation: CancellationToken,
    ) -> Result<(), AgentError> {
        // Reset before every turn: a stale `true` from a previous paused
        // turn must not be misread as "this turn paused too" if this turn
        // takes a different path (e.g. a /council command, which never
        // pauses).
        self.last_turn_paused = false;
        // Commands are intercepted here, before the input can enter LLM
        // history — the raw `/council …` text is an instruction to aivyx,
        // not part of the conversation the model should see.
        let result = match crate::council::parse_command(&user_input) {
```

In `run_turn_inner`, at the `TurnPaused` emission site, change:

```rust
            if iteration == self.max_tool_iterations {
                // Hitting the cap while the model was still actively
                // dispatching tool calls (every call up to and including
                // this iteration already has a matching result in
                // history) is a pause, not a failure — see
                // `AgentEvent::TurnPaused`'s doc comment and ROADMAP.md
                // Phase 12 Part A.
                self.emit(AgentEvent::TurnPaused(format!(
                    "reached the {}-round-trip cap for this turn while still working — \
                     send another message to continue; nothing has been lost.",
                    self.max_tool_iterations
                )));
                return Ok(());
            }
```

to:

```rust
            if iteration == self.max_tool_iterations {
                // Hitting the cap while the model was still actively
                // dispatching tool calls (every call up to and including
                // this iteration already has a matching result in
                // history) is a pause, not a failure — see
                // `AgentEvent::TurnPaused`'s doc comment and ROADMAP.md
                // Phase 12 Part A.
                self.last_turn_paused = true;
                self.emit(AgentEvent::TurnPaused(format!(
                    "reached the {}-round-trip cap for this turn while still working — \
                     send another message to continue; nothing has been lost.",
                    self.max_tool_iterations
                )));
                return Ok(());
            }
```

`last_turn_paused` is already correctly `false` for every `TurnComplete` exit path in `run_turn_inner` and `run_council_turn`, since it's reset to `false` once at the top of `run_turn` and only ever set `true` at the single `TurnPaused` site above — no further changes needed at the `TurnComplete` emission sites.

Finally, update every pre-existing `Agent::new(...)` call site in this file's test module to add `AutonomousMode::new()` as the new argument (positioned right after the `plan_mode` argument, before `tx`/`events_tx`). There are 3 such sites: `build_agent_with_config` (the shared helper), the standalone construction inside `plan_mode_filters_tools_and_annotates_the_system_prompt_per_request`, and the standalone construction inside `a_set_tasks_call_surfaces_tasks_updated_and_persists_the_session`. Also add `use aivyx_sandbox::{..., AutonomousMode, ...};` to the test module's existing `use aivyx_sandbox::{ActionKind, ExecutionConfiner, NoopConfiner, PermissionDecision, PermissionGate, PermissionRequest, PermissionTarget, PlanMode};` import line — change it to:

```rust
    use aivyx_sandbox::{
        ActionKind, AutonomousMode, ExecutionConfiner, NoopConfiner, PermissionDecision,
        PermissionGate, PermissionRequest, PermissionTarget, PlanMode,
    };
```

In `build_agent_with_config`, change:

```rust
        let agent = Agent::new(
            llm,
            executor,
            "system",
            config,
            Arc::default(),
            PlanMode::new(),
            tx,
        );
```

to:

```rust
        let agent = Agent::new(
            llm,
            executor,
            "system",
            config,
            Arc::default(),
            PlanMode::new(),
            AutonomousMode::new(),
            tx,
        );
```

In `plan_mode_filters_tools_and_annotates_the_system_prompt_per_request`, change:

```rust
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            plan_mode.clone(),
            tx,
        );
```

to:

```rust
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            plan_mode.clone(),
            AutonomousMode::new(),
            tx,
        );
```

For `a_set_tasks_call_surfaces_tasks_updated_and_persists_the_session`, locate its `Agent::new(...)` call (it constructs `Agent::new` directly, not via a helper, passing `Arc::clone(&tasks)` and `PlanMode::new()`) and add `AutonomousMode::new()` as the argument immediately after the `PlanMode::new()` (or whatever plan-mode expression it uses) argument, before the final `tx`/`events_tx` argument — following the exact same pattern as the two edits above.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core`
Expected: PASS, every test in the crate (all pre-existing tests continue passing with the updated constructor call, plus the 3 new autonomous-mode tests).

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p aivyx-core --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-core/src/agent.rs
git commit -m "Phase 11c: Agent autonomous-mode plumbing (flag, last_turn_paused, tool filter, prompt)"
```

---

## Task 6: `Agent` discard/rewind on exhausted verification (autonomous only)

**Files:**
- Modify: `crates/aivyx-core/src/agent.rs`

**Interfaces:**
- Consumes: `ToolExecutor::latest_checkpoint_ref`/`restore_to_checkpoint` from Task 4, `Agent::autonomous_mode` from Task 5.
- Produces: `Agent` gains a `pre_experiment_ref: Option<String>` field (private); the verification-exhausted branch in `run_turn_inner` performs a rewind when autonomous mode is active.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/aivyx-core/src/agent.rs`, in the "autonomous mode (Phase 11c)" section added in Task 5, after `autonomous_mode_appends_the_autonomous_prompt_note`:

```rust
    #[tokio::test]
    async fn autonomous_mode_discards_and_rewinds_on_exhausted_verification() {
        let dir = tempfile::tempdir().unwrap();
        // Real git repo, matching the checkpoint tests' own fixture style —
        // the discard path exercises real GitCheckpointer plumbing, not a
        // mock, since that's exactly the piece this test must prove works.
        crate::agent::tests_support::init_repo_for_checkpoints(dir.path()).await;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(aivyx_tools::WriteFileTool));
        registry.register(Arc::new(RunCommandTool::new(vec![CommandSpec {
            name: "verify".to_string(),
            program: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 1".to_string()], // always fails
            timeout: Duration::from_secs(5),
        }])));

        let write_call = vec![
            StreamEvent::ToolCallComplete(ToolCall {
                id: ToolCallId("c1".to_string()),
                name: "write_file".to_string(),
                arguments: serde_json::json!({ "path": "new.txt", "content": "hi\n" }),
                source: ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ];
        let (tx, _rx) = unbounded_channel();
        let mock = Arc::new(MockBackend::new(vec![
            write_call,
            text_response("done"),
            text_response("still trying"),
        ]));
        let llm: Arc<dyn LlmBackend> = mock.clone();
        let gate: Arc<dyn PermissionGate> = Arc::new(AllowAllGate);
        let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
        let mut executor = ToolExecutor::new(registry, gate, confiner);
        executor.set_checkpointer(Arc::new(
            aivyx_tools::GitCheckpointer::detect(dir.path(), vec![])
                .await
                .unwrap(),
        ));
        let autonomous_mode = AutonomousMode::new();
        autonomous_mode.set_active(true);
        let mut agent = Agent::new(
            llm,
            executor,
            "system",
            AgentConfig {
                max_tool_iterations: 10,
                ..Default::default()
            },
            Arc::default(),
            PlanMode::new(),
            autonomous_mode,
            tx,
        );
        agent.set_verification("verify".to_string(), 1);

        agent
            .run_turn("go".to_string(), dir.path(), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            !dir.path().join("new.txt").exists(),
            "the file created by the discarded experiment must be gone after rewind"
        );
    }
```

This test needs a real-git fixture helper reachable from `agent.rs`'s test module — `aivyx-tools`'s `checkpoint::test_support::init_repo` is `pub(crate)` to `aivyx-tools`, not visible from `aivyx-core`. Add a small equivalent directly in `agent.rs`'s test module instead of trying to reach across the crate boundary — replace the `crate::agent::tests_support::init_repo_for_checkpoints(dir.path()).await;` line above with an inline helper. Add this function inside `mod tests` (anywhere before the test that uses it, e.g. right after the `auto_verify_calls` helper from Phase 12B):

```rust
    async fn init_git_repo(dir: &Path) {
        for argv in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "test"],
            vec!["config", "user.email", "test@test.invalid"],
        ] {
            tokio::process::Command::new("git")
                .args(&argv)
                .current_dir(dir)
                .output()
                .await
                .unwrap();
        }
        std::fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        tokio::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
        tokio::process::Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(dir)
            .output()
            .await
            .unwrap();
    }
```

And change the test's first line from `crate::agent::tests_support::init_repo_for_checkpoints(dir.path()).await;` to `init_git_repo(dir.path()).await;`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p aivyx-core autonomous_mode_discards_and_rewinds -- --test-threads=1`
Expected: FAIL — either a compile error (`pre_experiment_ref` doesn't exist / no rewind logic) or, once it compiles, the assertion fails because the file is never deleted (no discard behavior yet).

- [ ] **Step 3: Implement the discard/rewind logic**

Add a new field to `struct Agent`, right after `verify_retries: u32,`:

```rust
    /// The checkpoint ref taken right before the first unverified edit of
    /// the current batch — the rewind target if verification exhausts its
    /// retries in autonomous mode. `None` when there's no unverified batch
    /// in flight, or once it's resolved (pass or rewind). Interactive mode
    /// never reads this field. See ROADMAP.md Phase 11c.
    pre_experiment_ref: Option<String>,
```

Add `pre_experiment_ref: None,` to `Agent::new`'s struct literal, right after `verify_retries: 0,`.

In the per-call dispatch loop, capture the pre-experiment ref the moment `unverified_edits` first becomes `true`. Change:

```rust
                let is_edit_call = PROMPTED_EDIT_HIDDEN_TOOLS.contains(&call.name.as_str());
                let result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                }
```

to:

```rust
                let is_edit_call = PROMPTED_EDIT_HIDDEN_TOOLS.contains(&call.name.as_str());
                let was_already_unverified = self.unverified_edits;
                let result = self
                    .executor
                    .dispatch(call, cwd, cancellation.clone())
                    .await;
                if is_edit_call && matches!(result.output, ToolOutput::Ok(_)) {
                    self.unverified_edits = true;
                    if self.autonomous_mode.active() && !was_already_unverified {
                        // First edit of a new batch: the checkpoint dispatch
                        // just took (ToolExecutor::dispatch checkpoints
                        // before every mutating call) is exactly "state
                        // right before this edit" — remember it as the
                        // rewind target if this batch's verification never
                        // passes.
                        self.pre_experiment_ref = self
                            .executor
                            .latest_checkpoint_ref(&cancellation)
                            .await;
                    }
                }
```

In the verification-exhausted branch, change:

```rust
                    // Retries exhausted for this round of edits: end the
                    // turn anyway (never block TurnComplete — the Phase 12
                    // Part B "loud notice, not a block" decision) but
                    // reset the retry budget rather than silently
                    // disabling verification for the rest of the session;
                    // `unverified_edits` stays true so the very next
                    // attempt to end a turn re-triggers this same check.
                    self.verify_retries = 0;
                    self.emit(AgentEvent::Error(format!(
                        "verification (`{}`) still failing after {} attempt(s) — ending the \
                         turn anyway. The worktree was checkpointed before each edit; `git log \
                         refs/aivyx/checkpoints/` to inspect or rewind.",
                        verification.command_name, verification.max_retries
                    )));
                }
```

to:

```rust
                    // Retries exhausted for this round of edits. Interactive
                    // mode: end the turn anyway (never block TurnComplete —
                    // the Phase 12 Part B "loud notice, not a block"
                    // decision) with the worktree left as-is for a human to
                    // inspect. Autonomous mode: no human is coming, so
                    // additionally discard — rewind to the pre-experiment
                    // checkpoint (Phase 11c) so the loop's next attempt
                    // starts from known-good state instead of building on
                    // top of a broken one.
                    self.verify_retries = 0;
                    if self.autonomous_mode.active()
                        && let Some(pre_experiment_ref) = self.pre_experiment_ref.take()
                    {
                        match self
                            .executor
                            .restore_to_checkpoint(&pre_experiment_ref, &cancellation)
                            .await
                        {
                            Ok(()) => {
                                self.unverified_edits = false;
                                self.emit(AgentEvent::Error(format!(
                                    "verification (`{}`) still failing after {} attempt(s) — \
                                     discarded this round of edits and restored the worktree to \
                                     the pre-experiment checkpoint.",
                                    verification.command_name, verification.max_retries
                                )));
                            }
                            Err(err) => {
                                // Rewind itself failed (e.g. a git error) —
                                // fall back to interactive mode's behavior:
                                // leave the state as-is and say so loudly,
                                // rather than silently pretending the
                                // discard happened.
                                self.emit(AgentEvent::Error(format!(
                                    "verification (`{}`) still failing after {} attempt(s), and \
                                     the automatic discard/rewind itself failed ({err}) — ending \
                                     the turn with the worktree left as-is. The worktree was \
                                     checkpointed before each edit; `git log \
                                     refs/aivyx/checkpoints/` to inspect or rewind manually.",
                                    verification.command_name, verification.max_retries
                                )));
                            }
                        }
                    } else {
                        self.emit(AgentEvent::Error(format!(
                            "verification (`{}`) still failing after {} attempt(s) — ending the \
                             turn anyway. The worktree was checkpointed before each edit; `git \
                             log refs/aivyx/checkpoints/` to inspect or rewind.",
                            verification.command_name, verification.max_retries
                        )));
                    }
                }
```

Also clear `pre_experiment_ref` on a *passing* verification, since a resolved batch (pass or discard) should never leave a stale ref behind for the next batch to misuse. Change:

```rust
                        if passed {
                            self.unverified_edits = false;
                            self.verify_retries = 0;
                            self.emit(AgentEvent::TurnComplete);
                            return Ok(());
                        }
```

to:

```rust
                        if passed {
                            self.unverified_edits = false;
                            self.verify_retries = 0;
                            self.pre_experiment_ref = None;
                            self.emit(AgentEvent::TurnComplete);
                            return Ok(());
                        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-core -- --test-threads=1`
Expected: PASS, every test in the crate, including the new discard/rewind test. (`--test-threads=1` here because the new test spawns real `git`/`sh` subprocesses against a real temp repo, matching this project's existing convention for such tests.)

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p aivyx-core --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-core/src/agent.rs
git commit -m "Phase 11c: Agent auto-discard/rewind on exhausted verification (autonomous mode)"
```

---

## Task 7: `[autonomous]` config section

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Produces: `pub struct AutonomousSettings { pub max_iterations: u32, pub max_duration_secs: u64 }`, `Settings.autonomous: AutonomousSettings`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/aivyx-config/src/lib.rs`, after `verification_command_parses_from_config`:

```rust
    #[test]
    fn autonomous_settings_have_conservative_defaults() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.autonomous.max_iterations, 20);
        assert_eq!(settings.autonomous.max_duration_secs, 3600);
    }

    #[test]
    fn autonomous_settings_parse_from_config() {
        let raw = r#"
            [autonomous]
            max_iterations = 5
            max_duration_secs = 600
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.autonomous.max_iterations, 5);
        assert_eq!(settings.autonomous.max_duration_secs, 600);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-config autonomous_settings`
Expected: FAIL to compile — `settings.autonomous` doesn't exist.

- [ ] **Step 3: Add the settings**

In `crates/aivyx-config/src/lib.rs`, add `pub autonomous: AutonomousSettings,` to `Settings`:

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
    pub verification: VerificationSettings,
    pub autonomous: AutonomousSettings,
}
```

Add the new struct right after `VerificationSettings`'s `impl Default` block:

```rust
/// Autonomous mode (`--auto "<goal>"`, ROADMAP.md Phase 11c): hard stops
/// for the unattended loop, independent of and in addition to
/// `permissions.max_tool_iterations_per_turn` (which bounds a single
/// turn's round-trips, not the whole autonomous session). Conservative
/// defaults so a bare `--auto` without further tuning cannot run
/// indefinitely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomousSettings {
    /// Total "continue" round-trips for the whole autonomous run.
    pub max_iterations: u32,
    /// Wall-clock ceiling, in seconds, for the whole autonomous run.
    pub max_duration_secs: u64,
}

impl Default for AutonomousSettings {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_duration_secs: 3600,
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-config`
Expected: PASS, all tests.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p aivyx-config --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Phase 11c: [autonomous] config section"
```

---

## Task 8: Driver integration in `aivyx-tui`

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs`

**Interfaces:**
- Consumes: `Agent::last_turn_paused()` from Task 5, `aivyx_core::{Task, TaskStatus}` (already imported in this file).
- Produces: `pub struct AutonomousRun { pub goal: String, pub max_iterations: u32, pub max_duration: std::time::Duration, pub tasks: Arc<Mutex<Vec<Task>>> }`; `aivyx_tui::run`'s signature gains a new final parameter `autonomous: Option<AutonomousRun>`.

**Architecture note — a refinement the design doc left open.** The spec described the driver abstractly as "new orchestration code in the aivyx binary crate," reacting to `AgentEvent`s. Working through the concrete integration surfaced a real problem: `agent_events_rx` (an `mpsc::UnboundedReceiver`) can only have one consumer, and it's already fully owned by this file's render loop — a separate driver task cannot also read from it. Worse, inferring "was this `TurnComplete` actually a `TurnComplete`, or a cancellation that also happens to emit `TurnComplete`" from the event stream alone is racy across tasks. The clean fix (this task): the driver logic lives *inside* the existing background task that already calls `agent.run_turn(...)` sequentially — the one place that can inspect its own `CancellationToken` and `Agent::last_turn_paused()` with zero cross-task races, because it's the same task that just awaited the call. No new task, no new channel, no event-stream inference.

- [ ] **Step 1: Write the failing test**

This task changes `run`'s signature, which no existing test directly exercises (this file's tests are all on free functions and `App` methods, not `run` itself, which requires a live terminal). Instead, write a unit test for the pure decision logic extracted into its own testable function — add to the `#[cfg(test)] mod tests` block at the bottom of `crates/aivyx-tui/src/app.rs`:

```rust
    fn done_task(id: u32) -> Task {
        Task {
            id,
            text: "x".to_string(),
            status: TaskStatus::Done,
        }
    }

    fn pending_task(id: u32) -> Task {
        Task {
            id,
            text: "x".to_string(),
            status: TaskStatus::Pending,
        }
    }

    #[test]
    fn goal_achieved_requires_at_least_one_task_and_all_done() {
        assert!(!goal_achieved(&[]), "no tasks ever set means never done");
        assert!(!goal_achieved(&[done_task(1), pending_task(2)]));
        assert!(goal_achieved(&[done_task(1), done_task(2)]));
    }

    #[test]
    fn next_autonomous_message_chooses_correctly() {
        assert_eq!(
            next_autonomous_message(true, &[]),
            Some("continue".to_string()),
            "a paused turn always continues, regardless of task state"
        );
        assert_eq!(
            next_autonomous_message(false, &[done_task(1)]),
            None,
            "goal achieved -> stop"
        );
        assert_eq!(
            next_autonomous_message(false, &[pending_task(1)]),
            Some("continue working toward the goal".to_string())
        );
        assert_eq!(
            next_autonomous_message(false, &[]),
            Some("continue working toward the goal".to_string()),
            "no tasks ever set -> keep going until budget exhausts, not stuck forever"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p aivyx-tui goal_achieved` and `cargo test -p aivyx-tui next_autonomous_message`
Expected: FAIL to compile — `goal_achieved`/`next_autonomous_message` don't exist yet.

- [ ] **Step 3: Implement the driver**

In `crates/aivyx-tui/src/app.rs`, change the import line:

```rust
use aivyx_core::{Agent, AgentEvent, SessionState, Task, TaskStatus};
```

to:

```rust
use aivyx_core::{Agent, AgentEvent, SessionState, Task, TaskStatus};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};
```

(`Mutex` is already imported as `std::sync::{Arc, Mutex}` at the top of this file for `active_cancellation` — check before adding a duplicate; if `Mutex` is already unqualified-imported, use it directly instead of adding `StdMutex`. Looking at the existing import `use std::sync::{Arc, Mutex};`, `Mutex` is already in scope — do not add the `StdMutex` alias; just add `use std::time::{Duration, Instant};` on its own.)

Add two pure, testable free functions near the top of the file, right after the `MAX_VISIBLE_TASKS` constant:

```rust
/// The autonomous driver's goal-achieved signal: every task in the list is
/// `Done`, and there is at least one task — an empty list means the model
/// never called `set_tasks` at all, which must not be misread as "nothing
/// to do, stop immediately." See ROADMAP.md Phase 11c.
fn goal_achieved(tasks: &[Task]) -> bool {
    !tasks.is_empty() && tasks.iter().all(|t| t.status == TaskStatus::Done)
}

/// What the autonomous driver sends next, given whether the turn that just
/// finished paused (Phase 12A) and the current task list. `None` means
/// stop the loop (goal achieved) — the caller is responsible for the
/// separate budget-exhaustion and cancellation stop conditions, which this
/// function doesn't know about.
fn next_autonomous_message(last_turn_paused: bool, tasks: &[Task]) -> Option<String> {
    if last_turn_paused {
        return Some("continue".to_string());
    }
    if goal_achieved(tasks) {
        return None;
    }
    Some("continue working toward the goal".to_string())
}
```

Add the public config struct, right after `pub struct ModalRequest`-adjacent public items — actually, place it right before `pub async fn run`:

```rust
/// Configures an unattended `--auto` session (ROADMAP.md Phase 11c). `tasks`
/// is a clone of the same `Arc<Mutex<Vec<Task>>>` handle already shared
/// between the `set_tasks` tool and the `Agent` — the driver reads it
/// directly rather than waiting on an `AgentEvent::TasksUpdated` event, so
/// its goal-achieved check always sees the current state.
pub struct AutonomousRun {
    pub goal: String,
    pub max_iterations: u32,
    pub max_duration: Duration,
    pub tasks: Arc<Mutex<Vec<Task>>>,
}
```

Now change `run`'s signature and the background task. Change:

```rust
pub async fn run(
    mut agent: Agent,
    mut agent_events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    mut permission_rx: PermissionModalReceiver,
    restored: Option<SessionState>,
    plan_mode: PlanMode,
) -> anyhow::Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let active_cancellation: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

    let background_cancellation = Arc::clone(&active_cancellation);
    tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            let cancellation = CancellationToken::new();
            *background_cancellation.lock().unwrap() = Some(cancellation.clone());
            let _ = agent.run_turn(input, &cwd, cancellation).await;
            *background_cancellation.lock().unwrap() = None;
        }
    });
```

to:

```rust
pub async fn run(
    mut agent: Agent,
    mut agent_events_rx: mpsc::UnboundedReceiver<AgentEvent>,
    cwd: PathBuf,
    mut permission_rx: PermissionModalReceiver,
    restored: Option<SessionState>,
    plan_mode: PlanMode,
    autonomous: Option<AutonomousRun>,
) -> anyhow::Result<()> {
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let active_cancellation: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));

    let background_cancellation = Arc::clone(&active_cancellation);
    tokio::spawn(async move {
        if let Some(autonomous) = autonomous {
            // Autonomous mode drives itself — it never waits on input_rx
            // (a human typing during an autonomous run has no effect
            // beyond appearing in the transcript locally; Ctrl+C is the
            // only supported intervention, matching the design doc's
            // scope).
            let deadline = Instant::now() + autonomous.max_duration;
            let mut iterations_used = 0u32;
            let mut next_message = Some(autonomous.goal.clone());
            while let Some(message) = next_message.take() {
                if iterations_used >= autonomous.max_iterations || Instant::now() >= deadline {
                    break;
                }
                iterations_used += 1;
                let cancellation = CancellationToken::new();
                *background_cancellation.lock().unwrap() = Some(cancellation.clone());
                let _ = agent.run_turn(message, &cwd, cancellation.clone()).await;
                *background_cancellation.lock().unwrap() = None;

                if cancellation.is_cancelled() {
                    // The user hit Ctrl+C wanting this to stop — do not
                    // send another message.
                    break;
                }
                let tasks_snapshot = autonomous.tasks.lock().unwrap().clone();
                next_message = next_autonomous_message(agent.last_turn_paused(), &tasks_snapshot);
            }
        } else {
            while let Some(input) = input_rx.recv().await {
                let cancellation = CancellationToken::new();
                *background_cancellation.lock().unwrap() = Some(cancellation.clone());
                let _ = agent.run_turn(input, &cwd, cancellation).await;
                *background_cancellation.lock().unwrap() = None;
            }
        }
    });
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p aivyx-tui`
Expected: PASS, all tests including the 2 new ones. (The crate will not yet compile standalone if `main.rs` still calls `aivyx_tui::run` with the old 6-argument signature — Task 9 fixes that call site. If `cargo test -p aivyx-tui` fails only because of the `aivyx` binary crate failing to build as a dependency check, that's expected until Task 9; verify specifically that `aivyx-tui`'s own compilation and its tests succeed via `cargo test -p aivyx-tui --lib`.)

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p aivyx-tui --all-targets`
Expected: clean (may show errors only from the `aivyx` binary's now-mismatched call site — that's expected and fixed in Task 9; focus on `aivyx-tui`'s own code being clean).

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "Phase 11c: autonomous driver loop inside the existing turn-execution task"
```

---

## Task 9: `main.rs` — `--auto` CLI flag, validation, and wiring

**Files:**
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `AutonomousMode` (Task 1), `ConfirmationGate::new`'s new signature (Task 2), `Agent::new`'s new signature (Task 5), `AutonomousSettings` (Task 7), `AutonomousRun`/`aivyx_tui::run`'s new signature (Task 8).

- [ ] **Step 1: Add the CLI flag and validation**

In `crates/aivyx/src/main.rs`, add to `struct Cli`, right after the `plan: bool` field:

```rust
    /// Run unattended toward a goal: no permission modals, edits and
    /// pre-approved commands auto-resolve, and the loop continues on its
    /// own until the goal is achieved (every task marked done) or the
    /// [autonomous] budget is exhausted. Mutually exclusive with --plan and
    /// --resume. Requires [verification].command to be configured.
    #[arg(long)]
    auto: Option<String>,
```

- [ ] **Step 2: Wire `AutonomousMode` into the gate**

Change:

```rust
    // One shared flag, three consumers: the gate enforces it, the agent
    // filters tools + annotates the system prompt by it, the TUI toggles it.
    let plan_mode = PlanMode::new();
    plan_mode.set_active(cli.plan);

    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        Arc::new(prompter),
        deny_paths.clone(),
        pre_approved_commands,
        plan_mode.clone(),
    ));
```

to:

```rust
    // One shared flag, three consumers: the gate enforces it, the agent
    // filters tools + annotates the system prompt by it, the TUI toggles it.
    let plan_mode = PlanMode::new();
    plan_mode.set_active(cli.plan);

    // --auto and --plan are contradictory (unattended action-taking vs.
    // enforced read-only); --auto and --resume are unsupported together in
    // this version (autonomous session state — unverified edits, retry
    // counts, the pre-experiment checkpoint ref — isn't part of
    // SessionState yet; see the Phase 11c design doc's non-goals).
    if cli.auto.is_some() && cli.plan {
        anyhow::bail!("--auto and --plan cannot be used together");
    }
    if cli.auto.is_some() && cli.resume {
        anyhow::bail!("--auto and --resume cannot be used together (not supported yet)");
    }
    let autonomous_mode = AutonomousMode::new();
    autonomous_mode.set_active(cli.auto.is_some());
    // Auto-approving edits is only defensible because deterministic
    // verification is the safety net — without it, "autonomous" would mean
    // "unchecked." Refuse to start rather than run degraded.
    if cli.auto.is_some() && settings.verification.command.is_none() {
        anyhow::bail!(
            "--auto requires [verification].command to be configured — auto-approving edits \
             with no verification check is not supported"
        );
    }

    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        Arc::new(prompter),
        deny_paths.clone(),
        pre_approved_commands,
        plan_mode.clone(),
        autonomous_mode.clone(),
        cwd.clone(),
    ));
```

- [ ] **Step 3: Wire `AutonomousMode` into `Agent::new`**

Change:

```rust
    let mut agent = Agent::new(
        llm,
        executor,
        system_prompt,
        AgentConfig {
            max_tool_iterations: settings.permissions.max_tool_iterations_per_turn,
            context_tokens: settings.backend.context_tokens,
            edit_format,
        },
        Arc::clone(&tasks),
        plan_mode.clone(),
        events_tx,
    );
```

to:

```rust
    let mut agent = Agent::new(
        llm,
        executor,
        system_prompt,
        AgentConfig {
            max_tool_iterations: settings.permissions.max_tool_iterations_per_turn,
            context_tokens: settings.backend.context_tokens,
            edit_format,
        },
        Arc::clone(&tasks),
        plan_mode.clone(),
        autonomous_mode.clone(),
        events_tx,
    );
```

- [ ] **Step 4: Build the `AutonomousRun` and pass it to `aivyx_tui::run`**

Change the import line:

```rust
use aivyx_sandbox::{ConfirmationGate, PermissionGate, PlanMode};
```

to:

```rust
use aivyx_sandbox::{AutonomousMode, ConfirmationGate, PermissionGate, PlanMode};
```

Change the final line of `main`:

```rust
    aivyx_tui::run(agent, events_rx, cwd, permission_rx, restored, plan_mode).await
```

to:

```rust
    let autonomous_run = cli.auto.map(|goal| aivyx_tui::AutonomousRun {
        goal,
        max_iterations: settings.autonomous.max_iterations,
        max_duration: Duration::from_secs(settings.autonomous.max_duration_secs),
        tasks: Arc::clone(&tasks),
    });
    aivyx_tui::run(
        agent,
        events_rx,
        cwd,
        permission_rx,
        restored,
        plan_mode,
        autonomous_run,
    )
    .await
```

`Duration` is already imported at the top of this file (`use std::time::Duration;`), so no new import is needed for that. `tasks` (the `Arc<Mutex<Vec<session::Task>>>`) is already in scope from earlier in `main`.

- [ ] **Step 5: Update `aivyx-tui`'s public re-export**

`AutonomousRun` needs to be reachable as `aivyx_tui::AutonomousRun` from `main.rs`. Check `crates/aivyx-tui/src/lib.rs`:

```bash
grep -n "pub use\|pub mod" crates/aivyx-tui/src/lib.rs
```

If `AutonomousRun` isn't already re-exported (it's defined `pub` inside `app.rs`, which is a private module per this crate's existing `lib.rs`), add it to whatever `pub use app::{...}` line already re-exports `run`. Read the exact current line first and add `AutonomousRun` to it (e.g. if the line reads `pub use app::run;`, change it to `pub use app::{AutonomousRun, run};`).

- [ ] **Step 6: Build and fix any remaining compile errors**

Run: `cargo build --workspace 2>&1 | tail -60`
Expected: clean build. If there are remaining errors (e.g. a missed call site), fix them following the same patterns as the edits above — every error at this point should be a straightforward argument-count/import mismatch, not a design question.

- [ ] **Step 7: Run the full workspace test suite and clippy**

Run: `cargo test --workspace 2>&1 | tail -40 && cargo clippy --workspace --all-targets 2>&1 | tail -40`
Expected: all tests pass, clippy clean.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx/src/main.rs crates/aivyx-tui/src/lib.rs
git commit -m "Phase 11c: --auto CLI flag, startup validation, and full wiring"
```

---

## Task 10: Live E2E verification and documentation

**Files:**
- Modify: `README.md` (new `--auto` section, mirroring how Plan mode and Enforced verification are documented)
- Modify: `ROADMAP.md` (mark Phase 11c built, matching the "designed + signed off" → "Built (date), as designed" pattern every other phase follows)

Matches this project's own verification bar: every phase gets a real, live run through the actual binary before being marked done, not just unit tests. Use the same Lemonade-managed llama-server setup and pty-harness pattern from the Phase 10/12 acceptance work if a local model is available; otherwise this task's live-check steps should be run manually by whoever executes this plan.

- [ ] **Step 1: Manual live E2E — happy path (goal achieved within budget)**

In a scratch git repo with `[[permissions.allowed_commands]]` and `[verification] command` configured (matching the Task 10 verification live-check pattern from Phase 12), plus a low `[autonomous] max_iterations` (e.g. 5) for a fast test run:

```bash
aivyx --auto "create a file called hello.txt containing exactly the word done, then mark your task list complete"
```

Expected, observed in the transcript: the goal is sent as the first message, the model creates the file (no permission modal), verification auto-runs and passes, the model calls `set_tasks` marking everything done, and the driver stops (no further "continue" sent) — confirm by checking the process exits or the TUI shows no further activity after that turn.

- [ ] **Step 2: Manual live E2E — cwd-boundary denial**

Ask the autonomous run to write outside the project directory (e.g. `"write a file to /tmp/should-be-denied.txt"`) and confirm the transcript shows a denial containing "worktree boundary" (not a permission modal, and not a silent no-op) — the model should see the denial reason and adapt (or the run continues toward budget exhaustion since the goal can't be achieved this way).

- [ ] **Step 3: Manual live E2E — discard/rewind on exhausted verification**

Configure `[verification] command` to point at a command that always fails (e.g. `program = "sh", args = ["-c", "exit 1"]`) and a low `max_auto_verify_retries` (e.g. 1). Run `aivyx --auto "create a file called wontpass.txt"` and confirm: the file briefly exists (created by the model), then after the retry is exhausted, the file is gone (`ls wontpass.txt` reports not found) and the transcript shows the "discarded this round of edits and restored the worktree" notice, not the interactive-mode "ending the turn anyway" wording.

- [ ] **Step 4: Manual live E2E — Ctrl+C stops the loop**

Start an autonomous run with a real multi-step goal and press Ctrl+C mid-turn; confirm the process does not send a further "continue" message afterward (no new turn starts).

- [ ] **Step 5: Update README.md**

Add a new subsection after the existing "Enforced verification" paragraph (found via `grep -n "Enforced verification" README.md`), documenting `--auto`, the trust profile summary (reuse the table from the design doc), the `[autonomous]` config block, and the hard requirement that `[verification].command` must be set. Follow the exact prose style of the surrounding Plan mode / Enforced verification paragraphs (one paragraph, config block reference, cross-reference to ROADMAP.md Phase 11c for the full rationale).

- [ ] **Step 6: Update ROADMAP.md**

Find the Phase 11c section (`grep -n "### Phase 11c" ROADMAP.md` — currently titled `Phase 11c — Autonomous research loop`, the original 2026-07-11 candidate-direction sketch). Add a "Built and live-verified (date)" paragraph immediately after the existing sketch, following the exact pattern every other completed phase uses (e.g. Phase 12 Part A's "Built and live-verified" paragraph): summarize what was built (the `AutonomousMode` gate tier, the discard/rewind mechanism, the driver's `last_turn_paused`-based race-free design — call out that refinement explicitly since it deviated from the design doc's more abstract framing), the test count added, and the results of the 4 live E2E checks from Steps 1-4 above.

- [ ] **Step 7: Final full-workspace check**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add README.md ROADMAP.md
git commit -m "Phase 11c: docs + live E2E verification"
```

---

## Self-Review

**Spec coverage** — every section of the design doc maps to a task:
- Architecture (`AutonomousMode` + gate tier) → Tasks 1-2.
- Trust profile table → Task 2 (edits/commands), Task 5 (tool hiding for `run_shell`/`git_commit`).
- Gap found during design (cwd boundary) → Task 2.
- Tool availability → Task 5.
- Loop driver & experiment lifecycle → Task 8 (with the race-condition refinement noted explicitly, not silently).
- Discard on exhausted verification → Tasks 3, 4, 6.
- Stopping conditions & config → Task 7 (config), Task 8 (enforcement).
- CLI → Task 9.
- System prompt → Task 5.
- Testing strategy → covered per-task (gate ordering tests in Task 2, cwd-boundary tests in Task 2, git_commit-auto-denies test in Task 2, tool-filtering test in Task 5, discard/rewind test in Task 6, driver logic tests in Task 8, live E2E in Task 10).
- Non-goals (headless, numeric metrics, structured log, autonomous resume) — no task builds any of these; confirmed absent by design, not by omission.

**Placeholder scan** — no "TBD"/"TODO"/"add appropriate error handling" anywhere in this plan; every step has complete, real code, including the git plumbing the spec explicitly left unverified (Task 3, resolved with a concrete, TDD-verified implementation).

**Type consistency** — `AutonomousMode` (Task 1) is the same type threaded through `ConfirmationGate::new` (Task 2), `Agent::new` (Task 5), and `main.rs` (Task 9). `AutonomousRun` (Task 8) is constructed once in `main.rs` (Task 9) with fields matching exactly what Task 8 defined. `ToolExecutor::latest_checkpoint_ref`/`restore_to_checkpoint` (Task 4) signatures match exactly how `Agent` calls them in Task 6.
