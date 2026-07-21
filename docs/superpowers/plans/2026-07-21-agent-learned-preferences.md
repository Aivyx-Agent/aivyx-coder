# Agent Learned Preferences (`remember_preference`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the agent propose edits to its own global `AGENTS.md` (the
one already loaded into every session's system prompt), gated through
the exact same `ConfirmationGate` review every other write already goes
through — no new approval machinery.

**Architecture:** A new `ActionKind::Memory` (always confirm-gated, never
cached, unconditionally denied under `--auto`), a new narrow tool
`remember_preference` that resolves its own fixed target path server-side,
and a new default `deny_paths` entry protecting the whole config
directory from the *generic* file tools now that this narrow, dedicated
path around it exists.

**Tech Stack:** Rust — no new external dependencies; everything reuses
existing crates (`aivyx-sandbox`, `aivyx-config`, `aivyx-tools`).

## Global Constraints

- Design authority: `docs/superpowers/specs/2026-07-21-agent-learned-preferences-design.md`
  (approved 2026-07-21). Every task below traces to a numbered Decision
  or Change in that doc.
- **Load-bearing correctness requirement (Decision 6)**: a `Memory`
  action must never populate or be satisfied by the Always-Allow cache,
  in *interactive* mode — not just autonomous mode. `PermissionKey::from_request`
  builds an `Other { action, description }` key
  (`crates/aivyx-sandbox/src/confirmation.rs:41-53`), and
  `remember_preference`'s target description is a fixed constant string
  — so without this, the first "Always Allow" click would silently
  auto-approve every future, unreviewed rewrite of the file. Task 1's
  own test suite must prove this directly, not just that the feature
  compiles.
- `ActionKind::Memory` must be unconditionally denied under `--auto`
  (autonomous mode), mirroring `ActionKind::McpTool`'s existing
  unconditional-deny arm exactly (`crates/aivyx-sandbox/src/confirmation.rs:236-244`).
- `remember_preference` is the *only* sanctioned path to the global
  config directory once the new `deny_paths` default lands — verify the
  generic `write_file`/`edit_file`/`delete_file`/`read_file`/`grep` tools
  are genuinely blocked from it, and that `remember_preference` itself is
  genuinely exempt (its `PermissionTarget::Other` target is structurally
  outside `deny_paths` matching, which only matches `PermissionTarget::Path` —
  `ConfirmationGate::is_denied`, `confirmation.rs:130-134`).
- Run `cargo test --workspace` and `cargo clippy --workspace --all-targets`
  after every task before committing (per this repo's `CLAUDE.md`).

---

### Task 1: `ActionKind::Memory`, `ConfirmationGate` handling, `deny_paths` default

**Files:**
- Modify: `crates/aivyx-sandbox/src/lib.rs:54-74` (`ActionKind` enum)
- Modify: `crates/aivyx-sandbox/src/confirmation.rs` (autonomous-mode branch + interactive-mode cache path)
- Modify: `crates/aivyx-config/src/lib.rs:521` (`PermissionSettings::default`)
- Test: `crates/aivyx-sandbox/src/confirmation.rs`'s existing `#[cfg(test)] mod tests`
- Test: `crates/aivyx-config/src/lib.rs`'s existing `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `ActionKind::Memory` variant (consumed by Task 2's tool);
  `ConfirmationGate` correctly resolving it in both autonomous and
  interactive mode (consumed by Task 3's E2E test and by any future
  caller).

This task has no dependency on Tasks 2/3 and can be fully verified in
isolation — every test here constructs a `PermissionRequest` by hand,
none of them need the real tool to exist yet.

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-sandbox/src/confirmation.rs`'s existing
`#[cfg(test)] mod tests` block, right after
`autonomous_mode_denies_mcp_tool_calls_unconditionally` (whose exact
`FakePrompter`/`ConfirmationGate::new` construction pattern these three
new tests mirror directly — both already exist in that module):

```rust
    fn memory_request() -> PermissionRequest {
        PermissionRequest {
            tool_name: "remember_preference".to_string(),
            action: ActionKind::Memory,
            target: PermissionTarget::Other("your global preferences (AGENTS.md)".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        }
    }

    #[tokio::test]
    async fn autonomous_mode_denies_memory_actions_unconditionally() {
        // Mirrors autonomous_mode_denies_mcp_tool_calls_unconditionally
        // exactly: the FakePrompter is configured to Allow, to prove the
        // denial isn't accidentally coming from the prompter path —
        // autonomous mode must never reach it at all for a Memory action.
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

        let decision = gate.check(&memory_request()).await;
        assert!(matches!(decision, PermissionDecision::Deny(_)));
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn memory_actions_are_never_cached_even_after_always_allow() {
        // Mirrors always_allow_caches_per_exact_target_only's structure,
        // but proves the OPPOSITE property for ActionKind::Memory: two
        // calls with the identical (fixed) target must both reach the
        // prompter — `calls` must be 2, not 1 — since a fixed target
        // description means "same target" would otherwise wrongly imply
        // "same proposed content" the way it correctly does for
        // PermissionTarget::Path.
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

        let first = gate.check(&memory_request()).await;
        assert_eq!(first, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        let second = gate.check(&memory_request()).await;
        assert_eq!(second, PermissionDecision::AllowAlways);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn memory_actions_are_not_affected_by_deny_paths() {
        let gate = ConfirmationGate::new(
            Arc::new(FakePrompter {
                response: UserResponse::Allow,
                calls: AtomicUsize::new(0),
            }),
            vec![PathBuf::from("/home/user/.config/aivyx-coder")],
            vec![],
            PlanMode::new(),
            AutonomousMode::new(),
            PathBuf::from("/home/user/project"),
            false,
        );

        // A Memory action's target is PermissionTarget::Other, never
        // Path — is_denied (confirmation.rs:130-134) only matches Path,
        // so this must be false regardless of deny_paths content.
        assert!(!gate.is_denied(&memory_request()));
    }
```

Add to `crates/aivyx-config/src/lib.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn default_deny_paths_includes_the_config_directory() {
        assert!(
            PermissionSettings::default()
                .deny_paths
                .contains(&"~/.config/aivyx-coder".to_string())
        );
    }
```

- [ ] **Step 2: Run to verify RED**

Run: `cargo test -p aivyx-sandbox -- --test-threads=1` and `cargo test -p aivyx-config -- --test-threads=1`
Expected: FAIL — `ActionKind::Memory` doesn't exist yet, compile errors.

- [ ] **Step 3: Add `ActionKind::Memory`**

`crates/aivyx-sandbox/src/lib.rs`, after the existing `McpTool` variant (ends at line 74 per the current file):

```rust
    /// The agent proposing an update to its own global, cross-project
    /// preferences file (`remember_preference`). Always confirm-gated —
    /// like `McpTool`, this is uniformly never auto-allowed, and unlike
    /// every other `ActionKind`, a call is never satisfied *or* recorded
    /// by the Always-Allow cache even in interactive mode (see
    /// `ConfirmationGate::check`): the target description is a fixed
    /// constant string regardless of what content is actually being
    /// proposed, so caching it would silently bless every future,
    /// unreviewed rewrite after the first approval. Unconditionally
    /// denied under `--auto` for the same reason `McpTool` is — this
    /// file persists and applies to every future project, unlike an
    /// in-worktree edit `--auto`'s checkpoint/rollback safety net
    /// already covers.
    Memory,
```

- [ ] **Step 4: Add the autonomous-mode denial arm**

`crates/aivyx-sandbox/src/confirmation.rs`, add a new constant near the existing `AUTONOMOUS_MCP_TOOL_DENIAL`:

```rust
/// Told to the model when a `remember_preference` call reaches autonomous
/// mode. Same reasoning as `AUTONOMOUS_MCP_TOOL_DENIAL`: there is no human
/// to review the proposed change, and this file's effect isn't scoped to
/// the current worktree the way `is_outside_autonomous_worktree` already
/// bounds ordinary Write/Delete actions.
const AUTONOMOUS_MEMORY_DENIAL: &str =
    "remembering preferences requires interactive confirmation and cannot happen in autonomous mode";
```

In `ConfirmationGate::check`'s autonomous-mode branch, immediately after the existing `if request.action == ActionKind::McpTool { ... }` block (currently ending at line 244):

```rust
            if request.action == ActionKind::Memory {
                tracing::warn!(
                    tool = %request.tool_name,
                    action = ?request.action,
                    target = ?request.target,
                    "permission denied: remember_preference call in autonomous mode"
                );
                return PermissionDecision::Deny(Some(AUTONOMOUS_MEMORY_DENIAL.to_string()));
            }
```

- [ ] **Step 5: Add the interactive-mode cache-skip**

In the same file, find the interactive-mode tail (currently starting with `let key = PermissionKey::from_request(request);` around line 295). Replace:

```rust
        let key = PermissionKey::from_request(request);
        if self.always_allow.lock().unwrap().contains(&key) {
            tracing::info!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission allowed (cached Always-Allow)"
            );
            return PermissionDecision::AllowAlways;
        }

        let decision = match self.resolve_via_prompter_or_editor(request).await {
            UserResponse::Allow => PermissionDecision::Allow,
            UserResponse::AllowAlways => {
                self.always_allow.lock().unwrap().insert(key);
                PermissionDecision::AllowAlways
            }
            UserResponse::Deny => {
                PermissionDecision::Deny(Some("the user denied this action".to_string()))
            }
        };
```

with:

```rust
        // `Memory` actions never participate in the Always-Allow cache,
        // in either direction — see ActionKind::Memory's doc comment for
        // why (the target description is fixed regardless of proposed
        // content, so caching would silently bless every future rewrite).
        let never_cached = request.action == ActionKind::Memory;
        let key = PermissionKey::from_request(request);
        if !never_cached && self.always_allow.lock().unwrap().contains(&key) {
            tracing::info!(
                tool = %request.tool_name,
                action = ?request.action,
                target = ?request.target,
                "permission allowed (cached Always-Allow)"
            );
            return PermissionDecision::AllowAlways;
        }

        let decision = match self.resolve_via_prompter_or_editor(request).await {
            UserResponse::Allow => PermissionDecision::Allow,
            UserResponse::AllowAlways => {
                if !never_cached {
                    self.always_allow.lock().unwrap().insert(key);
                }
                PermissionDecision::AllowAlways
            }
            UserResponse::Deny => {
                PermissionDecision::Deny(Some("the user denied this action".to_string()))
            }
        };
```

(The rest of the function — the `match &decision { ... }` tracing tail — is unchanged.)

- [ ] **Step 6: Add the `deny_paths` default entry**

`crates/aivyx-config/src/lib.rs:521`, change:

```rust
            deny_paths: vec!["~/.ssh".to_string(), "~/.aws".to_string()],
```

to:

```rust
            deny_paths: vec![
                "~/.ssh".to_string(),
                "~/.aws".to_string(),
                "~/.config/aivyx-coder".to_string(),
            ],
```

- [ ] **Step 7: Run to verify GREEN**

Run: `cargo test -p aivyx-sandbox -- --test-threads=1` and `cargo test -p aivyx-config -- --test-threads=1`
Expected: PASS, including the three new `confirmation.rs` tests and the one new `lib.rs` test. Also run `cargo test --workspace -- --test-threads=1` to confirm nothing else broke (the new `deny_paths` default is a behavior change to a widely-used default — check for any existing test that asserted the *old* two-entry list verbatim, e.g. a test literally comparing `PermissionSettings::default().deny_paths` against a two-element vec; if found, update it to the new three-element list rather than treating it as a regression).

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-sandbox/src/lib.rs crates/aivyx-sandbox/src/confirmation.rs crates/aivyx-config/src/lib.rs
git commit -m "$(cat <<'EOF'
Add ActionKind::Memory and deny_paths default for the config directory

Prerequisite for remember_preference (docs/superpowers/specs/
2026-07-21-agent-learned-preferences-design.md): Memory actions are
unconditionally denied under --auto (mirroring McpTool) and never
populate or read from the Always-Allow cache even interactively, since
the fixed target description would otherwise let one approval silently
bless every future, unreviewed rewrite. deny_paths now also protects
~/.config/aivyx-coder from the generic write_file/edit_file/delete_file/
read_file/grep tools, closing a pre-existing gap found during design.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `PersonaSettings` config + the `remember_preference` tool

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (new `PersonaSettings` struct + `Settings` field)
- Create: `crates/aivyx-tools/src/tools/remember_preference.rs`
- Modify: `crates/aivyx-tools/src/tools/mod.rs` (or wherever existing tool modules are declared/re-exported — check `crates/aivyx-tools/src/lib.rs`'s existing `pub use tools::{...}` list, e.g. where `WriteFileTool` is exported, and add `RememberPreferenceTool` alongside it)

**Interfaces:**
- Consumes: `ActionKind::Memory` (Task 1).
- Produces: `pub struct RememberPreferenceTool` with `pub fn new(path: PathBuf) -> Self`, implementing the `Tool` trait — consumed by Task 3's registration code.

- [ ] **Step 1: Add `PersonaSettings`**

`crates/aivyx-config/src/lib.rs`, near `EditorApprovalSettings` (whose exact pattern this mirrors):

```rust
/// Gates whether the agent can propose edits to its own global
/// `AGENTS.md` via the `remember_preference` tool (see
/// docs/superpowers/specs/2026-07-21-agent-learned-preferences-design.md).
/// Defaults to `true` — same reasoning as `EditorApprovalSettings`: every
/// use is still individually gated by `ConfirmationGate`, so the
/// capability alone grants nothing without the model choosing to use it
/// and the user approving that specific call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonaSettings {
    pub enabled: bool,
}

impl Default for PersonaSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

Add `pub persona: PersonaSettings,` to the `Settings` struct's field list (alongside `pub editor_approval: EditorApprovalSettings,`).

- [ ] **Step 2: Write the failing tests for `remember_preference`**

`crates/aivyx-tools/src/tools/remember_preference.rs`:

```rust
use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, DiffContent, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::diff::unified_diff;
use crate::{Tool, ToolError, ToolExecutionContext};

const TARGET_DESCRIPTION: &str = "your global preferences (AGENTS.md)";

#[derive(Deserialize, JsonSchema)]
struct RememberPreferenceArgs {
    /// The complete new content of your global AGENTS.md file — not a
    /// diff, not an append, the full replacement text.
    content: String,
}

/// Lets the agent propose an update to a single, fixed, server-resolved
/// path (the global `AGENTS.md` — see `Settings::agents_file_path()`),
/// never a model-supplied one. See `docs/superpowers/specs/
/// 2026-07-21-agent-learned-preferences-design.md`.
pub struct RememberPreferenceTool {
    path: PathBuf,
}

impl RememberPreferenceTool {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl Tool for RememberPreferenceTool {
    fn name(&self) -> &str {
        "remember_preference"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Propose an update to your own long-term memory of the user's \
                preferences and working style — stored in a file that's automatically included \
                in every future project, not just this one. Use this when the user explicitly \
                asks you to remember something, or when you notice a clear, repeated pattern \
                worth remembering (not a one-off — don't propose from a single occurrence, and \
                don't re-propose something already declined this session). The user reviews \
                every change as a diff before it's saved."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(
                RememberPreferenceArgs
            )),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: RememberPreferenceArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let (preview, diff) = match std::fs::read_to_string(&self.path) {
            Ok(old) => (
                Some(unified_diff(&self.path.display().to_string(), &old, &args.content)),
                Some(DiffContent {
                    old_content: old,
                    new_content: args.content.clone(),
                }),
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (
                Some(unified_diff(&self.path.display().to_string(), "", &args.content)),
                Some(DiffContent {
                    old_content: String::new(),
                    new_content: args.content.clone(),
                }),
            ),
            Err(_) => (
                Some(format!(
                    "WARNING: {} already exists but could not be read as text. This write will \
                     overwrite it entirely.",
                    self.path.display()
                )),
                None,
            ),
        };

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Memory,
            target: PermissionTarget::Other(TARGET_DESCRIPTION.to_string()),
            arguments_preview: json!({}),
            preview,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RememberPreferenceArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, &args.content).await?;

        Ok(ToolOutput::Ok(format!(
            "updated {} ({} bytes)",
            self.path.display(),
            args.content.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_request_targets_memory_action_and_other_target() {
        let tool = RememberPreferenceTool::new(PathBuf::from("/nonexistent/AGENTS.md"));
        let request = tool
            .permission_request(&json!({"content": "be terse"}), Path::new("/irrelevant"))
            .unwrap();

        assert_eq!(request.action, ActionKind::Memory);
        assert_eq!(
            request.target,
            PermissionTarget::Other(TARGET_DESCRIPTION.to_string())
        );
    }

    #[test]
    fn permission_request_diffs_against_real_existing_content_not_model_supplied_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "old preference\n").unwrap();
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "new preference\n"}), Path::new("/irrelevant"))
            .unwrap();

        let diff = request.diff.expect("expected a structured diff");
        assert_eq!(diff.old_content, "old preference\n");
        assert_eq!(diff.new_content, "new preference\n");
    }

    #[test]
    fn permission_request_handles_a_not_yet_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist-yet").join("AGENTS.md");
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "first preference\n"}), Path::new("/irrelevant"))
            .unwrap();

        let diff = request.diff.expect("expected a structured diff");
        assert_eq!(diff.old_content, "");
        assert_eq!(diff.new_content, "first preference\n");
    }

    // No shared `ToolExecutionContext` test helper exists in this crate —
    // every tool's own test module defines a local one (e.g.
    // `crates/aivyx-tools/src/tools/git_read.rs`'s `fn ctx(dir: &Path)`).
    // Mirror that exact pattern here rather than inventing a shared one
    // this task doesn't need.
    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn execute_writes_the_proposed_content_to_the_fixed_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("AGENTS.md");
        let tool = RememberPreferenceTool::new(path.clone());

        tool.execute(json!({"content": "remembered\n"}), &ctx(dir.path()))
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "remembered\n");
    }

    // Mirrors write_file.rs's existing_binary_file_gets_a_warning_instead_of_a_silent_diff
    // / existing_binary_file_has_no_structured_diff tests exactly — same
    // fallback path, same fixed byte sequence, just retargeted at this
    // tool's fixed path instead of a `path` argument.
    #[test]
    fn existing_non_utf8_file_gets_a_warning_instead_of_a_silent_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "hello\n"}), Path::new("/irrelevant"))
            .unwrap();

        let preview = request.preview.expect("expected a warning preview, not None");
        assert!(preview.contains("WARNING"));
    }

    #[test]
    fn existing_non_utf8_file_has_no_structured_diff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, [0xFF, 0xFE, 0x00, 0xD8, 0x00]).unwrap();
        let tool = RememberPreferenceTool::new(path);

        let request = tool
            .permission_request(&json!({"content": "hello\n"}), Path::new("/irrelevant"))
            .unwrap();

        assert!(request.diff.is_none(), "a non-UTF-8 file has no text diff content");
    }
}
```

- [ ] **Step 3: Register the module and export the type**

Add `mod remember_preference;` and `pub use remember_preference::RememberPreferenceTool;`
to wherever `crates/aivyx-tools/src/lib.rs` already declares/exports
`write_file`/`WriteFileTool` (mirror that exact pattern).

- [ ] **Step 4: Run to verify RED then GREEN**

Run: `cargo test -p aivyx-tools -- --test-threads=1`
Expected: initial compile/RED failures are normal (fix against the real
`Tool` trait signature and `ToolExecutionContext` construction helper if
either differs from what's written above), then all 6 new tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs crates/aivyx-tools/src/tools/remember_preference.rs crates/aivyx-tools/src/lib.rs
git commit -m "$(cat <<'EOF'
Add PersonaSettings config and the remember_preference tool

The tool proposes a full-content update to a fixed, server-resolved
path (the global AGENTS.md) rather than a model-supplied one, and
always diffs against the real current on-disk content rather than
trusting the model's own idea of what's already there — same pattern
write_file already uses, just retargeted at one specific path.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Wire registration + README docs + live E2E

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs`
- Modify: `README.md` (near the existing `AGENTS.md` documentation, `README.md:224`)

**Interfaces:**
- Consumes: `settings.persona.enabled`, `settings.agents_file.enabled` (Task 2), `aivyx_config::Settings::agents_file_path()` (pre-existing), `RememberPreferenceTool::new` (Task 2).

- [ ] **Step 1: Register the tool in `agent_builder.rs`**

Find the existing conditional tool-registration block:

```rust
    // Only registered when configured — an always-erroring tool offered to
    // the model would just be confusing noise for a project that hasn't
    // opted into any commands.
    if !command_specs.is_empty() {
        registry.register(Arc::new(RunCommandTool::new(command_specs.clone())));
    }
```

(`crates/aivyx/src/agent_builder.rs`, around where `RunCommandTool` is
conditionally registered.) Add immediately after it:

```rust
    // Registered only when persona learning + AGENTS.md loading are both
    // enabled, and a global AGENTS.md location can even be resolved —
    // otherwise the agent could "successfully" remember something into a
    // file this session never reads back into context.
    if settings.persona.enabled
        && settings.agents_file.enabled
        && let Ok(path) = aivyx_config::Settings::agents_file_path()
    {
        registry.register(Arc::new(aivyx_tools::RememberPreferenceTool::new(path)));
    }
```

Add `use aivyx_tools::RememberPreferenceTool;` to the file's existing
`use aivyx_tools::{...}` import block if `aivyx_tools::RememberPreferenceTool::new(...)`
isn't used fully-qualified as written above (either style is fine; match
whatever this file's existing convention is for other `aivyx_tools`
types — check whether it imports each tool type individually or
qualifies inline).

- [ ] **Step 2: Build to confirm it compiles**

Run: `cargo build --workspace`
Expected: clean build. No new automated test is added in this step — the
tool's own logic is already covered by Task 2's tests, and this step is
pure wiring; the meaningful verification is the live E2E in Step 4.

- [ ] **Step 3: Add README documentation**

Near `README.md:224`'s existing `AGENTS.md` project instructions
section, add a short paragraph immediately after the existing
description of the global/project file split:

```markdown
**Learning over time**: the agent can propose updates to your *global*
`AGENTS.md` itself, via a dedicated `remember_preference` tool — either
because you asked it to remember something, or because it noticed a
clear, repeated pattern. Every proposed change goes through the exact
same review as any other file write: you see the diff, you approve or
deny it. Disable with `[persona] enabled = false`. Unlike every other
mutating tool, this one never uses the Always-Allow cache — you always
see every change to this file, individually, even if you've approved a
previous one. Not available in `--auto` (autonomous) mode: there's no
human to review the change.
```

- [ ] **Step 4: Live E2E verification**

With a real local LLM backend running: start `aivyx` in an empty scratch
project directory, tell it something like "remember that I always want
you to write tests before implementation code." Confirm: the model calls
`remember_preference`, the TUI's permission modal shows a real diff
against your actual current global `AGENTS.md` content (or against empty
content, if this is a fresh install), approving it writes the file.
Start a **second** `aivyx` session in a **different** scratch project
directory and confirm the new preference is visible in that session too
(proves the "applies to every future project" property end-to-end, not
just that the config layer works). If a real local LLM backend isn't
available in the environment executing this plan, note this step as a
required human follow-up in the final report, exactly like the ACP
integration's own Task 6 Step 7 precedent — do not skip it silently or
fabricate a result.

- [ ] **Step 5: Full workspace verification**

Run: `cargo build --workspace && cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs README.md
git commit -m "$(cat <<'EOF'
Wire remember_preference into agent_builder.rs; document in README

Registered only when [persona].enabled and [agents_file].enabled are
both true and a global AGENTS.md path resolves. Live-E2E verified: a
real model proposes a preference update, the diff renders correctly,
and the change is visible in a fresh session in a different project
directory.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```
