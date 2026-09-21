# Specialist Deny-Paths Attenuation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A specialist (`delegate_to_specialist`/`spawn_specialist`) gets a genuinely scoped `ConfirmationGate` and `ExecutionConfiner` — built from the lead's `deny_paths` unioned with the specialist's own `TeamMember.extra_deny_paths` — instead of sharing the lead's exact, unattenuated instances.

**Architecture:** A new, self-contained `aivyx-core` module (`specialist_enforcement.rs`) bundles the raw construction ingredients `agent_builder.rs` already assembles for the lead's own gate/confiner, plus one function that builds a fresh, specialist-scoped pair per delegation call. `DelegateToSpecialistConfig`/`SpecialistSessionsConfig` each swap their `gate`/`confiner` fields for one `enforcement` field; the two call sites that build a specialist's `Agent` call the new function instead of cloning the shared instances.

**Tech Stack:** Rust, existing `aivyx-sandbox`/`aivyx-team`/`aivyx-core` crates.

## Global Constraints

- `plan_mode`/`autonomous_mode`/`injection_taint` stay the *same shared* handles as the lead — not per-specialist copies.
- A specialist's Always-Allow cache starts fresh (only re-seeded from `pre_approved_commands`) — this is intended, not a bug to fix.
- `compute_specialist_registry`'s tool-name attenuation is unchanged — this plan only fixes path scope.
- No change to `aivyx_team::TeamMember`'s schema or `effective_deny_paths` — both already correct.
- Existing delegation tests (mechanics, not deny-paths-specific) must keep passing with unchanged assertions after switching from the `AllowAllGate` test mock to a real, permissively-configured `ConfirmationGate`.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: `specialist_enforcement.rs` — the scoped gate/confiner builder

**Files:**
- Create: `crates/aivyx-core/src/specialist_enforcement.rs`
- Modify: `crates/aivyx-core/src/lib.rs` (register the new module)

**Interfaces:**
- Produces: `pub struct SpecialistEnforcementIngredients { .. }` (all fields `pub`, `#[derive(Clone)]`), `pub fn scoped_gate_and_confiner(ingredients: &SpecialistEnforcementIngredients, member: &aivyx_team::TeamMember, cwd: &std::path::Path) -> (std::sync::Arc<dyn aivyx_sandbox::PermissionGate>, std::sync::Arc<dyn aivyx_sandbox::ExecutionConfiner>)` — both consumed by Task 2.

- [ ] **Step 1: Write `specialist_enforcement.rs`**

Create `crates/aivyx-core/src/specialist_enforcement.rs`:

```rust
//! Builds a specialist-scoped `PermissionGate`/`ExecutionConfiner` pair —
//! see `docs/superpowers/specs/2026-09-21-specialist-deny-paths-attenuation-design.md`.
//! Before this module existed, `delegate_to_specialist`/`spawn_specialist`
//! passed the lead's own shared gate/confiner straight through to a
//! specialist's `Agent`, so `TeamMember.extra_deny_paths` (part of the
//! schema since `aivyx-team`'s Foundation phase) was never actually
//! enforced anywhere.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_sandbox::{
    AutonomousMode, ConfirmationGate, ExecutionConfiner, InjectionTaint, PermissionGate,
    PermissionPrompter, PlanMode,
};

/// Everything needed to build a fresh, correctly-configured
/// `ConfirmationGate`/`ExecutionConfiner` pair, mirroring exactly what
/// `agent_builder.rs` already assembles once for the lead's own gate —
/// constructed once per process there, cloned cheaply (every field is an
/// `Arc`, a small `Vec`, or a `bool`) into each specialist-delegation
/// tool's config.
#[derive(Clone)]
pub struct SpecialistEnforcementIngredients {
    pub prompter: Arc<dyn PermissionPrompter>,
    /// The lead's own resolved `deny_paths` — unioned with each
    /// specialist's own `extra_deny_paths` at call time, never mutated
    /// here.
    pub base_deny_paths: Vec<PathBuf>,
    pub pre_approved_commands: Vec<(String, Vec<String>)>,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub editor_approval_enabled: bool,
    pub injection_taint: InjectionTaint,
    pub extra_read_paths: Vec<PathBuf>,
    pub require_enforcement: bool,
}

/// Builds a fresh `ConfirmationGate` + `ExecutionConfiner` pair scoped to
/// `member`'s own effective deny-paths (the lead's `base_deny_paths`
/// unioned with `member.extra_deny_paths`, deduplicated) — everything
/// else (`prompter`, `plan_mode`, `autonomous_mode`, `injection_taint`,
/// `pre_approved_commands`, `editor_approval_enabled`) matches the lead's
/// own construction exactly. `cwd` is taken per-call (from the caller's
/// own `ToolExecutionContext`) rather than stored in
/// `SpecialistEnforcementIngredients`, since it's already available at
/// every real call site and storing a second copy risks staleness.
pub fn scoped_gate_and_confiner(
    ingredients: &SpecialistEnforcementIngredients,
    member: &aivyx_team::TeamMember,
    cwd: &Path,
) -> (Arc<dyn PermissionGate>, Arc<dyn ExecutionConfiner>) {
    let mut deny_paths = ingredients.base_deny_paths.clone();
    for resolved in resolve_tilde_paths(&member.extra_deny_paths) {
        if !deny_paths.contains(&resolved) {
            deny_paths.push(resolved);
        }
    }

    let gate: Arc<dyn PermissionGate> = Arc::new(
        ConfirmationGate::new(
            Arc::clone(&ingredients.prompter),
            deny_paths.clone(),
            ingredients.pre_approved_commands.clone(),
            ingredients.plan_mode.clone(),
            ingredients.autonomous_mode.clone(),
            cwd.to_path_buf(),
            ingredients.editor_approval_enabled,
        )
        .with_injection_taint(ingredients.injection_taint.clone()),
    );
    let confiner = aivyx_sandbox::default_confiner(
        cwd,
        &ingredients.extra_read_paths,
        &deny_paths,
        ingredients.require_enforcement,
    );
    (gate, confiner)
}

/// Mirrors `aivyx-config`'s own private `resolve_tilde_paths` exactly
/// (tilde expansion + symlink canonicalization) — duplicated rather than
/// adding a new `aivyx-core` -> `aivyx-config` dependency for one small
/// helper, the same justified-duplication shape `aivyx-config` itself
/// already uses for `resolve_symlinks` (borrowed from `aivyx-tools`, per
/// its own doc comment). Keep the three in sync if any changes.
fn resolve_tilde_paths(raw_paths: &[String]) -> Vec<PathBuf> {
    let home_dir = directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());

    raw_paths
        .iter()
        .filter_map(|raw| {
            let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                home_dir.as_ref().map(|home| home.join(rest))
            } else if raw == "~" {
                home_dir.clone()
            } else if raw.starts_with('~') {
                tracing::warn!(
                    entry = %raw,
                    "unsupported ~username path syntax in extra_deny_paths; skipping this entry"
                );
                None
            } else {
                Some(PathBuf::from(raw))
            };
            expanded.map(|path| resolve_symlinks(&path))
        })
        .collect()
}

/// Canonicalizes as much of `path` as exists, then re-appends whatever
/// doesn't — mirrors `aivyx-config`'s own private `resolve_symlinks`
/// exactly (itself already a documented duplicate of `aivyx-tools`'s
/// `path_resolve::resolve_symlinks`).
fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut result = canonical;
            for component in tail.into_iter().rev() {
                result.push(component);
            }
            return result;
        }

        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_sandbox::{PermissionDecision, PermissionRequest, PermissionTarget};
    use async_trait::async_trait;

    struct AlwaysAllowPrompter;
    #[async_trait]
    impl PermissionPrompter for AlwaysAllowPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }

    fn member(name: &str, extra_deny_paths: &[&str]) -> aivyx_team::TeamMember {
        aivyx_team::TeamMember {
            name: name.to_string(),
            role: "Specialist".to_string(),
            persona: "You specialize.".to_string(),
            tool_allowlist: vec![],
            extra_deny_paths: extra_deny_paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn ingredients(base_deny_paths: Vec<PathBuf>) -> SpecialistEnforcementIngredients {
        SpecialistEnforcementIngredients {
            prompter: Arc::new(AlwaysAllowPrompter),
            base_deny_paths,
            pre_approved_commands: vec![],
            plan_mode: PlanMode::new(),
            autonomous_mode: AutonomousMode::new(),
            editor_approval_enabled: false,
            injection_taint: InjectionTaint::new(),
            extra_read_paths: vec![],
            require_enforcement: false,
        }
    }

    #[tokio::test]
    async fn a_member_with_no_extra_deny_paths_only_gets_the_base_list_blocked() {
        let base = PathBuf::from("/tmp/base-denied");
        let ing = ingredients(vec![base.clone()]);
        let m = member("implementer", &[]);
        let (gate, _confiner) = scoped_gate_and_confiner(&ing, &m, Path::new("/tmp"));

        let decision = gate
            .check(&PermissionRequest {
                tool_name: "write_file".to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Path(base.join("secret.txt")),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            })
            .await;
        assert!(
            matches!(decision, PermissionDecision::Deny { .. }),
            "the base deny_paths entry must still be enforced for a member with no extras"
        );
    }

    #[tokio::test]
    async fn a_members_own_extra_deny_paths_are_hard_blocked() {
        let dir = std::env::temp_dir().join(format!(
            "specialist-enforcement-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ing = ingredients(vec![]);
        let m = member("implementer", &[dir.to_string_lossy().as_ref()]);
        let (gate, _confiner) = scoped_gate_and_confiner(&ing, &m, Path::new("/tmp"));

        let decision = gate
            .check(&PermissionRequest {
                tool_name: "write_file".to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Path(dir.join("secret.txt")),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            })
            .await;
        assert!(
            matches!(decision, PermissionDecision::Deny { .. }),
            "a member's own extra_deny_paths entry must be hard-blocked, even though it's \
             absent from the lead's own base_deny_paths"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_path_outside_both_the_base_and_extra_lists_is_unaffected() {
        let base = PathBuf::from("/tmp/base-denied");
        let ing = ingredients(vec![base]);
        let m = member("implementer", &["/tmp/also-denied"]);
        let (gate, _confiner) = scoped_gate_and_confiner(&ing, &m, Path::new("/tmp"));

        let decision = gate
            .check(&PermissionRequest {
                tool_name: "write_file".to_string(),
                action: aivyx_sandbox::ActionKind::Write,
                target: PermissionTarget::Path(PathBuf::from("/tmp/totally-unrelated.txt")),
                arguments_preview: serde_json::json!({}),
                preview: None,
                diff: None,
            })
            .await;
        assert!(
            !matches!(decision, PermissionDecision::Deny { .. }),
            "a path outside every deny list must not be blocked by this tier"
        );
    }

    #[test]
    fn different_members_of_the_same_call_get_independently_scoped_deny_lists() {
        // Regression guard against a shared-mutable-state bug: calling
        // scoped_gate_and_confiner for one member must never leak into
        // what a second, unrelated member (called against the same
        // ingredients) ends up with.
        let ing = ingredients(vec![PathBuf::from("/tmp/base")]);
        let alice = member("alice", &["/tmp/alice-only"]);
        let bob = member("bob", &["/tmp/bob-only"]);
        let (_alice_gate, _) = scoped_gate_and_confiner(&ing, &alice, Path::new("/tmp"));
        let (_bob_gate, _) = scoped_gate_and_confiner(&ing, &bob, Path::new("/tmp"));
        // ingredients.base_deny_paths itself must be unchanged after both calls.
        assert_eq!(ing.base_deny_paths, vec![PathBuf::from("/tmp/base")]);
    }
}
```

- [ ] **Step 2: Register the new module**

In `crates/aivyx-core/src/lib.rs`, find this exact line:

```rust
pub mod session;
```

Replace it with:

```rust
pub mod session;
pub mod specialist_enforcement;
```

(Alphabetical placement matches this file's existing ordering — `session` then `specialist_enforcement` then `specialist_sessions`, confirm this ordering is correct by checking the surrounding lines before applying; if `specialist_sessions` isn't immediately after `session` in the real file, insert the new line immediately before `specialist_sessions` instead, wherever that actually sits.)

- [ ] **Step 3: Run the new module's tests**

Run: `cargo test -p aivyx-core specialist_enforcement -- --nocapture`
Expected: all 4 tests pass.

- [ ] **Step 4: Format and commit**

```bash
rustfmt --edition 2024 crates/aivyx-core/src/specialist_enforcement.rs
rustfmt --edition 2024 crates/aivyx-core/src/lib.rs
git add crates/aivyx-core/src/specialist_enforcement.rs crates/aivyx-core/src/lib.rs
git commit -m "feat: add specialist-scoped gate/confiner builder"
```

---

### Task 2: Wire it into delegation, update `agent_builder.rs`, adapt tests

**Files:**
- Modify: `crates/aivyx-core/src/delegate_to_specialist.rs`
- Modify: `crates/aivyx-core/src/specialist_sessions.rs`
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `SpecialistEnforcementIngredients`, `scoped_gate_and_confiner` (Task 1).

- [ ] **Step 1: Replace `DelegateToSpecialistConfig`'s `gate`/`confiner` fields**

In `crates/aivyx-core/src/delegate_to_specialist.rs`, find this exact block:

```rust
pub struct DelegateToSpecialistConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub gate: Arc<dyn PermissionGate>,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
```

Replace it with:

```rust
pub struct DelegateToSpecialistConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
```

- [ ] **Step 2: Use `scoped_gate_and_confiner` at the specialist-construction call site**

In `crates/aivyx-core/src/delegate_to_specialist.rs`, find this exact block:

```rust
        let specialist_registry = compute_specialist_registry(member, &self.config.parent_registry);
        let mut sub_executor = ToolExecutor::new(
            specialist_registry,
            Arc::clone(&self.config.gate),
            Arc::clone(&self.config.confiner),
        );
```

Replace it with:

```rust
        let specialist_registry = compute_specialist_registry(member, &self.config.parent_registry);
        let (gate, confiner) = crate::specialist_enforcement::scoped_gate_and_confiner(
            &self.config.enforcement,
            member,
            &ctx.cwd,
        );
        let mut sub_executor = ToolExecutor::new(specialist_registry, gate, confiner);
```

- [ ] **Step 3: Update `delegate_to_specialist.rs`'s test helper**

In `crates/aivyx-core/src/delegate_to_specialist.rs`'s test module, find this exact block:

```rust
    struct AllowAllGate;
    #[async_trait]
    impl PermissionGate for AllowAllGate {
        async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
```

Replace it with:

```rust
    struct AlwaysAllowPrompter;
    #[async_trait]
    impl aivyx_sandbox::PermissionPrompter for AlwaysAllowPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
```

Then find this exact block (the `base_config` test helper):

```rust
    fn base_config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
    ) -> DelegateToSpecialistConfig {
        DelegateToSpecialistConfig {
            llm,
            gate: Arc::new(AllowAllGate),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
```

Replace it with:

```rust
    fn base_config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
    ) -> DelegateToSpecialistConfig {
        DelegateToSpecialistConfig {
            llm,
            enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients {
                prompter: Arc::new(AlwaysAllowPrompter),
                base_deny_paths: vec![],
                pre_approved_commands: vec![],
                plan_mode: PlanMode::new(),
                autonomous_mode: AutonomousMode::new(),
                editor_approval_enabled: false,
                injection_taint: InjectionTaint::new(),
                extra_read_paths: vec![],
                require_enforcement: false,
            },
            checkpointer: None,
```

Note: `PermissionDecision` must already be imported in this test module (it was used by the old `AllowAllGate` impl) — confirm it still is after this edit; `NoopConfiner` may become an unused import after this change if nothing else in the file's test module uses it directly — check with `cargo check` in Step 6 below and remove the import only if the compiler actually flags it unused, not preemptively.

- [ ] **Step 4: Add a real regression test proving `extra_deny_paths` is enforced end-to-end**

In `crates/aivyx-core/src/delegate_to_specialist.rs`'s test module, find the test `delegating_to_a_known_member_returns_the_specialists_final_answer` (or whichever test is placed immediately after `base_config`) and add this new test immediately after it:

```rust
    #[tokio::test]
    async fn a_specialists_own_extra_deny_paths_blocks_a_write_the_lead_could_do() {
        let dir = tempfile::tempdir().unwrap();
        let denied = dir.path().join("denied");
        std::fs::create_dir_all(&denied).unwrap();

        // finish_reason: Stop (not ToolCalls) so the specialist's turn
        // completes naturally after this one round -- the outer
        // "continue" loop never needs a second mock response, which
        // keeps this test focused purely on whether the write is
        // actually blocked, not on simulating a realistic multi-round
        // model conversation.
        let mock = Arc::new(MockBackend::new(vec![vec![
            StreamEvent::ToolCallComplete(aivyx_types::ToolCall {
                id: aivyx_types::ToolCallId("c1".to_string()),
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": denied.join("secret.txt").to_string_lossy(),
                    "content": "leaked",
                }),
                source: aivyx_types::ToolCallSource::Native,
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
        ]]));

        let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
        let team = simple_team();
        let mut config = base_config(mock, events_tx, team);
        config.enforcement.base_deny_paths = vec![];
        // The specialist's own extra_deny_paths, not the lead's -- proves
        // this is genuinely per-member, not just a copy of the lead's list.
        config.team.members[1].extra_deny_paths = vec![denied.to_string_lossy().to_string()];
        config.parent_registry = {
            let mut registry = ToolRegistry::new();
            registry.register(Arc::new(WriteFileTool));
            registry
        };

        let tool = DelegateToSpecialistTool::new(config);
        let ctx = ToolExecutionContext {
            cwd: dir.path().to_path_buf(),
            confiner: Arc::new(NoopConfiner),
            cancellation: CancellationToken::new(),
        };
        let result = tool
            .execute(
                serde_json::json!({ "member": "implementer", "task": "write the secret" }),
                &ctx,
            )
            .await
            .unwrap();
        // Structural check only (not the specialist's exact synthesized
        // text, which depends on mock-backend round-tripping details not
        // relevant to what this test is actually proving) -- the real
        // proof is the filesystem assertion below.
        assert!(
            matches!(result, ToolOutput::Ok(_)),
            "expected Ok, got {result:?}"
        );
        assert!(
            !denied.join("secret.txt").exists(),
            "the write must have been blocked by the specialist's own scoped gate -- if this \
             file exists, extra_deny_paths was not actually enforced"
        );
    }
```

This test requires `WriteFileTool` to be in scope — check the top of the test module for an existing `use aivyx_tools::...` line and add `WriteFileTool` to it if not already imported (or add a new `use aivyx_tools::WriteFileTool;` line if the module doesn't already import from `aivyx_tools` directly). Also requires `tempfile` — check `crates/aivyx-core/Cargo.toml`'s `[dev-dependencies]` for whether it's already present (used by other test files in this crate); if missing, add it matching the version already used elsewhere in this workspace (check e.g. `crates/aivyx-config/Cargo.toml` or `crates/aivyx-tools/Cargo.toml` for the exact version string already in use, and match it — do not introduce a different version).

- [ ] **Step 5: Repeat Steps 1-2 for `SpecialistSessionsConfig`/`build_specialist_agent`**

In `crates/aivyx-core/src/specialist_sessions.rs`, find this exact block:

```rust
pub struct SpecialistSessionsConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub gate: Arc<dyn PermissionGate>,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
```

Replace it with:

```rust
pub struct SpecialistSessionsConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
```

Then find this exact block:

```rust
fn build_specialist_agent(
    member: &aivyx_team::TeamMember,
    config: &SpecialistSessionsConfig,
) -> (
    Agent,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<String>>,
    BarrierSender,
) {
    let specialist_registry = compute_specialist_registry(member, &config.parent_registry);
    let mut sub_executor = ToolExecutor::new(
        specialist_registry,
        Arc::clone(&config.gate),
        Arc::clone(&config.confiner),
    );
```

Replace it with:

```rust
fn build_specialist_agent(
    member: &aivyx_team::TeamMember,
    config: &SpecialistSessionsConfig,
    cwd: &std::path::Path,
) -> (
    Agent,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<String>>,
    BarrierSender,
) {
    let specialist_registry = compute_specialist_registry(member, &config.parent_registry);
    let (gate, confiner) =
        crate::specialist_enforcement::scoped_gate_and_confiner(&config.enforcement, member, cwd);
    let mut sub_executor = ToolExecutor::new(specialist_registry, gate, confiner);
```

Then find this exact line (the one call site of `build_specialist_agent`):

```rust
            build_specialist_agent(member, &self.config);
```

Replace it with:

```rust
            build_specialist_agent(member, &self.config, &ctx.cwd);
```

- [ ] **Step 6: Update `specialist_sessions.rs`'s test helper**

In `crates/aivyx-core/src/specialist_sessions.rs`'s test module, find this exact block:

```rust
    struct AllowAllGate;
    #[async_trait]
    impl PermissionGate for AllowAllGate {
        async fn check(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
```

Replace it with:

```rust
    struct AlwaysAllowPrompter;
    #[async_trait]
    impl aivyx_sandbox::PermissionPrompter for AlwaysAllowPrompter {
        async fn prompt(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
```

Then find this exact block (the `config` test helper):

```rust
    fn config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
        pool: SpecialistSessionPool,
    ) -> SpecialistSessionsConfig {
        SpecialistSessionsConfig {
            llm,
            gate: Arc::new(AllowAllGate),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
```

Replace it with:

```rust
    fn config(
        llm: Arc<dyn LlmBackend>,
        events_tx: UnboundedSender<AgentEvent>,
        team: TeamConfig,
        pool: SpecialistSessionPool,
    ) -> SpecialistSessionsConfig {
        SpecialistSessionsConfig {
            llm,
            enforcement: crate::specialist_enforcement::SpecialistEnforcementIngredients {
                prompter: Arc::new(AlwaysAllowPrompter),
                base_deny_paths: vec![],
                pre_approved_commands: vec![],
                plan_mode: PlanMode::new(),
                autonomous_mode: AutonomousMode::new(),
                editor_approval_enabled: false,
                injection_taint: InjectionTaint::new(),
                extra_read_paths: vec![],
                require_enforcement: false,
            },
            checkpointer: None,
```

- [ ] **Step 7: Wire real ingredients in `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
    let gate: Arc<dyn PermissionGate> = Arc::new(
        ConfirmationGate::new(
            Arc::clone(&prompter),
            deny_paths.clone(),
            pre_approved_commands,
            plan_mode.clone(),
            autonomous_mode.clone(),
            cwd.clone(),
            settings.editor_approval.enabled,
        )
        .with_injection_taint(injection_taint.clone()),
    );
```

Replace it with:

```rust
    // Cloned before the move below -- also seeds every specialist's own
    // scoped gate (see `specialist_enforcement_ingredients` further down),
    // which needs its own copy of the same config-level trust list.
    let specialist_pre_approved_commands = pre_approved_commands.clone();
    let gate: Arc<dyn PermissionGate> = Arc::new(
        ConfirmationGate::new(
            Arc::clone(&prompter),
            deny_paths.clone(),
            pre_approved_commands,
            plan_mode.clone(),
            autonomous_mode.clone(),
            cwd.clone(),
            settings.editor_approval.enabled,
        )
        .with_injection_taint(injection_taint.clone()),
    );
```

Then find this exact block (immediately after, unchanged from before):

```rust
    let confiner = aivyx_sandbox::default_confiner(
        &cwd,
        &settings.sandbox.resolved_extra_read_paths(),
        &deny_paths,
        settings.sandbox.require_enforcement,
    );
```

Immediately after it (before the following `#[cfg(not(feature = "sandbox-backend"))]` block, which stays exactly where it is), insert:

```rust

    let specialist_enforcement_ingredients =
        aivyx_core::specialist_enforcement::SpecialistEnforcementIngredients {
            prompter: Arc::clone(&prompter),
            base_deny_paths: deny_paths.clone(),
            pre_approved_commands: specialist_pre_approved_commands,
            plan_mode: plan_mode.clone(),
            autonomous_mode: autonomous_mode.clone(),
            editor_approval_enabled: settings.editor_approval.enabled,
            injection_taint: injection_taint.clone(),
            extra_read_paths: settings.sandbox.resolved_extra_read_paths(),
            require_enforcement: settings.sandbox.require_enforcement,
        };
```

- [ ] **Step 8: Use the new ingredients where `DelegateToSpecialistConfig`/`SpecialistSessionsConfig` are constructed**

In `crates/aivyx/src/agent_builder.rs`, find this exact block:

```rust
        registry.register(Arc::new(aivyx_core::DelegateToSpecialistTool::new(
            aivyx_core::DelegateToSpecialistConfig {
                llm: Arc::clone(&llm),
                gate: Arc::clone(&gate),
                confiner: Arc::clone(&confiner),
                checkpointer: checkpointer.clone(),
```

Replace it with:

```rust
        registry.register(Arc::new(aivyx_core::DelegateToSpecialistTool::new(
            aivyx_core::DelegateToSpecialistConfig {
                llm: Arc::clone(&llm),
                enforcement: specialist_enforcement_ingredients.clone(),
                checkpointer: checkpointer.clone(),
```

Then find this exact block:

```rust
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
            llm: Arc::clone(&llm),
            gate: Arc::clone(&gate),
            confiner: Arc::clone(&confiner),
            checkpointer: checkpointer.clone(),
```

Replace it with:

```rust
        let specialist_sessions_config = aivyx_core::SpecialistSessionsConfig {
            llm: Arc::clone(&llm),
            enforcement: specialist_enforcement_ingredients,
            checkpointer: checkpointer.clone(),
```

(This is the last use of `specialist_enforcement_ingredients` in the function, so it's moved, not cloned, here — matches `gate`/`confiner`'s own prior last-use pattern.)

- [ ] **Step 9: Verify `aivyx-core` and `aivyx` compile**

Run: `cargo check -p aivyx-core --tests` then `cargo check -p aivyx`
Expected: both compile cleanly. Fix any remaining reference to the old `gate`/`confiner` fields the grep in Step 10 below would otherwise catch.

- [ ] **Step 10: Confirm no stray references to the removed fields remain**

Run: `grep -rn "\.config\.gate\|\.config\.confiner\|config: gate\|config: confiner" crates/aivyx-core/src/delegate_to_specialist.rs crates/aivyx-core/src/specialist_sessions.rs`
Expected: no output (every reference was replaced in Steps 1-2 and 5-6).

- [ ] **Step 11: Run the full affected test suites**

Run:
```bash
cargo test -p aivyx-core delegate_to_specialist -- --nocapture
cargo test -p aivyx-core specialist_sessions -- --nocapture
```
Expected: every test passes, including the new
`a_specialists_own_extra_deny_paths_blocks_a_write_the_lead_could_do` test
from Step 4, and every pre-existing test with assertions unchanged from
before this task.

- [ ] **Step 12: Format the exact files touched, and build/test/lint the full workspace**

Run:
```bash
rustfmt --edition 2024 crates/aivyx-core/src/delegate_to_specialist.rs
rustfmt --edition 2024 crates/aivyx-core/src/specialist_sessions.rs
rustfmt --edition 2024 crates/aivyx/src/agent_builder.rs
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: all clean, zero failures, zero warnings.

- [ ] **Step 13: Commit**

```bash
git add crates/aivyx-core/src/delegate_to_specialist.rs crates/aivyx-core/src/specialist_sessions.rs crates/aivyx/src/agent_builder.rs
git commit -m "fix: specialists get a genuinely scoped deny-paths gate and confiner"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`SpecialistEnforcementIngredients` + `scoped_gate_and_confiner`, `PathBuf`-native union not the string-based `effective_deny_paths`) → Task 1. Decision 2 (both configs swap `gate`/`confiner` for one `enforcement` field; `agent_builder.rs` constructs it once and clones it) → Task 2 Steps 1-2, 5, 7-8. Decision 3 (`plan_mode`/`autonomous_mode`/`injection_taint` stay the same shared handles) → every ingredients construction site passes `.clone()` of the existing shared instances, never a fresh one. Decision 4 (fresh Always-Allow cache per specialist, re-seeded only from `pre_approved_commands`) → falls out naturally from `ConfirmationGate::new` always starting with an empty `always_allow` beyond `pre_approved`, unchanged from its existing constructor behavior. "What this spec does not decide" items are all genuinely untouched: no `compute_specialist_registry` change, no `TeamMember`/`effective_deny_paths` change, no `GrepTool`/`GlobTool`/`RepoMap` reconstruction.

**Global Constraints deviation:** none — shared handles stay shared, tool-name attenuation untouched, `TeamMember` schema untouched, only file-scoped `rustfmt` used. The one deliberate, disclosed deviation from the spec's literal ingredient list (storing `cwd` as a per-call parameter of `scoped_gate_and_confiner` rather than a field of `SpecialistEnforcementIngredients`) is called out explicitly in Task 1 Step 1's doc comment and this section, with its rationale (avoids a redundant/potentially-stale snapshot when the real call-time `cwd` is already available via `ToolExecutionContext` at every call site).

**Placeholder scan:** no TBD/TODO; every step shows complete, real code; Task 2 Steps 3-4 and 6 include brief investigative notes ("check X, add only if the compiler flags it") rather than a placeholder — these name precisely what to verify and why, matching this project's own established plan-writing pattern for genuinely environment-dependent details (e.g. exact existing import lists) that a plan author reading a snapshot of the file can't fully guarantee stay accurate character-for-character through review cycles.

**Type/interface consistency check:** `SpecialistEnforcementIngredients` (Task 1) is referenced identically in both configs (Task 2 Steps 1, 5) and constructed identically at both test-helper sites (Task 2 Steps 3, 6) and the real `agent_builder.rs` site (Task 2 Step 7) — same field set, same types, in the same order for readability. `scoped_gate_and_confiner`'s 3-argument signature (`&SpecialistEnforcementIngredients`, `&TeamMember`, `&Path`) matches exactly at both real call sites (Task 2 Steps 2, 5) and all 4 of Task 1's own tests.
