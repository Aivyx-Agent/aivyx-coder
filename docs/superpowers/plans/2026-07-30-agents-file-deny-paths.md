# `Agent::refresh_agents_files` `deny_paths` Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `Agent::refresh_agents_files` the same `deny_paths`
enforcement `refresh_editor_context` already has — a denied global or
project `AGENTS.md` file's content must not reach the system prompt.

**Architecture:** Add a `deny_paths: Vec<PathBuf>` field to
`AgentsFileConfig`, thread it through `Agent::set_agents_file`, wire the
same global `deny_paths` list `agent_builder.rs` already passes to
`set_editor_context`, and check both `AGENTS.md` paths against it before
reading — mirroring `refresh_editor_context`'s check-then-read ordering
and silent-skip behavior exactly.

**Tech Stack:** Rust, no new dependencies.

## Global Constraints

- `aivyx_sandbox::path_is_denied` itself must not change — it already
  correctly handles both absolute/tilde-prefixed entries and
  basename-glob patterns.
- A denied `AGENTS.md` is silently skipped (same as missing/unreadable) —
  no user-facing notice, unlike the existing over-budget notice. This was
  an explicit user decision during design, not an oversight.
- `refresh_editor_context`, `EditorContextConfig`, and the over-budget
  notice logic are all out of scope — unchanged.

---

### Task 1: Add `deny_paths` to `AgentsFileConfig` and enforce it in `refresh_agents_files`

**Files:**
- Modify: `crates/aivyx-core/src/agent/types.rs`
- Modify: `crates/aivyx-core/src/agent/mod.rs`
- Modify: `crates/aivyx-core/src/agent/tests.rs`
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: the existing `aivyx_sandbox::path_is_denied(path: &Path,
  deny_paths: &[PathBuf]) -> bool` function (unchanged).
- Produces: `Agent::set_agents_file(&mut self, global_path: Option<PathBuf>,
  budget_tokens: u32, deny_paths: Vec<PathBuf>)` — a breaking signature
  change to an existing `pub` method, so every call site in this workspace
  must be updated in this same task (Step 3 below enumerates all of them).

- [ ] **Step 1: Write the failing tests**

Add to `crates/aivyx-core/src/agent/tests.rs`, directly after the existing
`neither_file_present_injects_nothing` test (ends at line 1008 today):

```rust
#[tokio::test]
async fn a_denied_project_agents_md_is_excluded_but_global_still_appears() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("AGENTS.md"),
        "PROJECT_SECRET_INSTRUCTIONS",
    )
    .unwrap();
    let global_dir = tempfile::tempdir().unwrap();
    let global_path = global_dir.path().join("AGENTS.md");
    std::fs::write(&global_path, "Always write terse commit messages.").unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_agents_file(
        Some(global_path),
        1024,
        vec![dir.path().join("AGENTS.md")],
    );

    agent
        .run_turn("hi".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("PROJECT_SECRET_INSTRUCTIONS"));
    assert!(system.contains("Always write terse commit messages."));
}

#[tokio::test]
async fn a_denied_global_agents_md_is_excluded_but_project_still_appears() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "Use tabs, not spaces.").unwrap();
    let global_dir = tempfile::tempdir().unwrap();
    let global_path = global_dir.path().join("AGENTS.md");
    std::fs::write(&global_path, "GLOBAL_SECRET_INSTRUCTIONS").unwrap();

    let (mut agent, _rx, mock) = build_agent(
        vec![vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        }]],
        ToolRegistry::new(),
        10,
    );
    agent.set_agents_file(Some(global_path.clone()), 1024, vec![global_path]);

    agent
        .run_turn("hi".to_string(), dir.path(), CancellationToken::new())
        .await
        .unwrap();

    let received = mock.received.lock().unwrap();
    let system = received[0].messages[0].text_content();
    assert!(!system.contains("GLOBAL_SECRET_INSTRUCTIONS"));
    assert!(system.contains("Use tabs, not spaces."));
}
```

These two tests won't compile yet — `set_agents_file` doesn't take a
third argument. That's expected; Step 3 makes them (and every other
existing call site) compile again, and this pair specifically exercises
the new deny behavior.

- [ ] **Step 2: Add the `deny_paths` field to `AgentsFileConfig`**

In `crates/aivyx-core/src/agent/types.rs`, replace:

```rust
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
}
```

with:

```rust
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
    pub(crate) deny_paths: Vec<PathBuf>,
}
```

- [ ] **Step 3: Update `set_agents_file` and `refresh_agents_files`**

In `crates/aivyx-core/src/agent/mod.rs`, replace:

```rust
    /// Enables `AGENTS.md` support: `global_path` is the resolved
    /// user-global location (`None` if `Settings::agents_file_path()`
    /// couldn't resolve one), applied identically per turn alongside the
    /// project-level `<cwd>/AGENTS.md`. `budget_tokens` applies to each
    /// file independently.
    pub fn set_agents_file(&mut self, global_path: Option<PathBuf>, budget_tokens: u32) {
        self.agents_file_config = Some(AgentsFileConfig {
            global_path,
            budget_tokens,
        });
    }
```

with:

```rust
    /// Enables `AGENTS.md` support: `global_path` is the resolved
    /// user-global location (`None` if `Settings::agents_file_path()`
    /// couldn't resolve one), applied identically per turn alongside the
    /// project-level `<cwd>/AGENTS.md`. `budget_tokens` applies to each
    /// file independently. `deny_paths` is checked against both file
    /// paths before either is ever read, same as every other
    /// path-reporting source in this project.
    pub fn set_agents_file(
        &mut self,
        global_path: Option<PathBuf>,
        budget_tokens: u32,
        deny_paths: Vec<PathBuf>,
    ) {
        self.agents_file_config = Some(AgentsFileConfig {
            global_path,
            budget_tokens,
            deny_paths,
        });
    }
```

Then, in the same file, update the doc comment on `refresh_agents_files`
and the function body. Replace:

```rust
    /// Re-reads both `AGENTS.md` files off the async runtime. Best-effort:
    /// any read error for either file (missing, permission denied, not
    /// valid UTF-8) just means that source contributes nothing — this
    /// never fails the turn. Called once per turn (not once per LLM
    /// round-trip), mirroring `refresh_repo_map`'s cadence exactly.
    async fn refresh_agents_files(&mut self, cwd: &Path) {
        let Some(config) = &self.agents_file_config else {
            return;
        };
        let budget_chars = (config.budget_tokens as f64 * self.chars_per_token) as usize;
        let global_path = config.global_path.clone();
        let project_path = cwd.join("AGENTS.md");
        let budget_tokens = config.budget_tokens;

        let mut sections: Vec<String> = Vec::new();
        let mut over_budget_labels: Vec<&str> = Vec::new();

        if let Some(path) = &global_path
            && let Ok(content) = tokio::fs::read_to_string(path).await
        {
```

with:

```rust
    /// Re-reads both `AGENTS.md` files off the async runtime. Best-effort:
    /// any read error for either file (missing, permission denied, not
    /// valid UTF-8) just means that source contributes nothing — this
    /// never fails the turn. Called once per turn (not once per LLM
    /// round-trip), mirroring `refresh_repo_map`'s cadence exactly. A
    /// path denied via `config.deny_paths` is treated identically to a
    /// missing file — silently skipped, no notice — mirroring
    /// `refresh_editor_context`'s own denied-path handling.
    async fn refresh_agents_files(&mut self, cwd: &Path) {
        let Some(config) = &self.agents_file_config else {
            return;
        };
        let budget_chars = (config.budget_tokens as f64 * self.chars_per_token) as usize;
        let global_path = config.global_path.clone();
        let project_path = cwd.join("AGENTS.md");
        let budget_tokens = config.budget_tokens;

        let mut sections: Vec<String> = Vec::new();
        let mut over_budget_labels: Vec<&str> = Vec::new();

        if let Some(path) = &global_path
            && !aivyx_sandbox::path_is_denied(path, &config.deny_paths)
            && let Ok(content) = tokio::fs::read_to_string(path).await
        {
```

Then, a few lines further down, replace:

```rust
        if let Ok(content) = tokio::fs::read_to_string(&project_path).await {
```

with:

```rust
        if !aivyx_sandbox::path_is_denied(&project_path, &config.deny_paths)
            && let Ok(content) = tokio::fs::read_to_string(&project_path).await
        {
```

Everything else in the function (the body of both `if` blocks, the
over-budget notification loop, and the final `sections.len()` match)
stays exactly as-is.

- [ ] **Step 4: Update every other `set_agents_file` call site to compile**

Every one of these calls passes only two arguments today and must gain a
third — `vec![]` (no denial in play; these tests aren't testing
`deny_paths`). Run this from the repository root:

```bash
sed -i \
  -e 's/agent\.set_agents_file(None, 1024)/agent.set_agents_file(None, 1024, vec![])/' \
  -e 's/agent\.set_agents_file(None, 5)/agent.set_agents_file(None, 5, vec![])/' \
  -e 's/agent\.set_agents_file(Some(global_path), 1024)/agent.set_agents_file(Some(global_path), 1024, vec![])/' \
  crates/aivyx-core/src/agent/tests.rs
```

Then update the one production call site. In
`crates/aivyx/src/agent_builder.rs`, replace:

```rust
    if settings.agents_file.enabled {
        let global_path = aivyx_config::Settings::agents_file_path().ok();
        agent.set_agents_file(global_path, settings.agents_file.budget_tokens);
    }
```

with:

```rust
    if settings.agents_file.enabled {
        let global_path = aivyx_config::Settings::agents_file_path().ok();
        agent.set_agents_file(global_path, settings.agents_file.budget_tokens, deny_paths.clone());
    }
```

(`deny_paths` is already in scope here — it's the same binding
`set_editor_context` uses three lines below, both originating from
`settings.permissions.resolved_deny_paths()` earlier in this function.)

- [ ] **Step 5: Verify every call site was updated**

```bash
grep -rn "set_agents_file(" crates/ | grep -v "vec!\[" | grep -v "fn set_agents_file" | grep -v "Deliberately not calling"
```

Expected: no output. (The one remaining un-updated line should be the
`// Deliberately not calling agent.set_agents_file(...)` comment in
`tests.rs`, which the `grep -v` above already excludes — if anything else
prints, a call site was missed.)

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-core --test-threads=1 agents_file`
Expected: all `agents_file`/`agents_md` tests pass, including the two new
ones from Step 1. (`--test-threads=1` per this project's known
sandboxed-test-hang issue.)

Then run the full crate suite to catch any other affected test:

Run: `cargo test -p aivyx-core --test-threads=1`
Expected: all tests pass.

Then confirm the workspace still builds end-to-end:

Run: `cargo build --workspace`
Expected: builds cleanly (confirms `aivyx-config` and `aivyx` compile
against the new `set_agents_file` signature).

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-core/src/agent/types.rs crates/aivyx-core/src/agent/mod.rs \
        crates/aivyx-core/src/agent/tests.rs crates/aivyx/src/agent_builder.rs
git commit -m "Add deny_paths enforcement to Agent::refresh_agents_files"
```

---

### Task 2: Documentation and backlog closure

**Files:**
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only); should be the last task since
  it describes the finished fix.

- [ ] **Step 1: Update ROADMAP.md**

In `ROADMAP.md`, delete this entire paragraph (in the "## Backlog"
section — it is currently the last content in the file):

```markdown
**Found at the `wiki_pointer_lines` `deny_paths` enforcement fix's own
final whole-branch review (2026-07-30), logged rather than expanding
that fix's scope mid-review**: `Agent::refresh_agents_files`
(`crates/aivyx-core/src/agent/mod.rs`) reads both the global and
project `AGENTS.md` files and splices their content into the system
prompt every turn with **no `deny_paths` check at all** — unlike its
sibling `refresh_editor_context`, which explicitly calls
`aivyx_sandbox::path_is_denied` before surfacing anything.
`AgentsFileConfig` (`crates/aivyx-core/src/agent/types.rs`) has no
`deny_paths` field at all, so there's currently no way to wire a check
in without adding one. Low realistic severity in practice (an
`AGENTS.md` a user wrote themselves is unlikely to also be a
credentials file), but it's the same "content reaches the prompt
without any deny check" shape this feature just closed for wiki pages,
in a different function.
```

After deletion, confirm the "## Backlog" section's last remaining
paragraph is the "No new capability opportunities were found in test
quality or the security/gate-tier-order/Landlock dimension this pass..."
one — leave that as the new end of the file.

Then, in the "## Current status" section, find the `wiki_pointer_lines`
"shipped" paragraph added by the prior feature (it ends with "...no new
logic, no new dependency.") followed by:

```markdown

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Insert a new "shipped" paragraph between them:

```markdown

**`Agent::refresh_agents_files` `deny_paths` enforcement — shipped.**
Found at the `wiki_pointer_lines` fix's own final review: both the
global and project `AGENTS.md` files were spliced into the system
prompt every turn with no `deny_paths` check at all — unlike the
sibling `refresh_editor_context`, which already checked. Fixed by
adding a `deny_paths` field to `AgentsFileConfig` and checking both
paths (via the same global `deny_paths` list every other tool already
reuses) before either file is read; a denied file is silently skipped,
matching `refresh_editor_context`'s own behavior. No new config
surface, no new dependency.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.

## Backlog — capability opportunities, not yet scheduled
```

Also update the `_Last updated:_` line at the top of `ROADMAP.md` to
today's actual date, if it is not already today's date.

- [ ] **Step 2: Add a HISTORY.md chapter**

In `docs/HISTORY.md`, append a new chapter after the "### `wiki_pointer_lines`
`deny_paths` enforcement — ✅ shipped" chapter (the last chapter in the
file):

```markdown
### `Agent::refresh_agents_files` `deny_paths` enforcement — ✅ shipped

Found at the `wiki_pointer_lines` `deny_paths` enforcement fix's own final
whole-branch review (2026-07-30) and logged to `ROADMAP.md`'s backlog
rather than fixed mid-review, since it was out of that fix's stated
scope: `Agent::refresh_agents_files`
(`crates/aivyx-core/src/agent/mod.rs`) read both the global and project
`AGENTS.md` files and spliced their content into the system prompt every
turn, with no `deny_paths` check at all — unlike its sibling
`refresh_editor_context`, which already called
`aivyx_sandbox::path_is_denied` before surfacing anything.
`AgentsFileConfig` had no `deny_paths` field at all, so there was no way
to wire a check in without adding one. Low realistic severity (an
`AGENTS.md` a user wrote themselves is unlikely to also be a credentials
file), but it was the same "content reaches the prompt without any deny
check" shape found in `aivyx-repomap`'s `collect_tags` and
`wiki_pointer_lines` — now a fourth function with the same gap.

**The fix**: mirrored `EditorContextConfig`/`refresh_editor_context`
exactly. `AgentsFileConfig` gained a `deny_paths: Vec<PathBuf>` field;
`Agent::set_agents_file` gained a matching parameter; `agent_builder.rs`
passes the same global `deny_paths` list already passed to
`set_editor_context` one line below it — no new settings field, no new
config surface. `refresh_agents_files` checks both the global and
project `AGENTS.md` paths against `deny_paths` before reading either,
silently skipping a denied one exactly like a missing file — no
user-facing notice, unlike the existing over-budget notice, since
denying a path is the user's own configuration choice rather than a
misconfiguration.

With this shipped, this is the fourth and — as far as this audit
lineage has traced — last instance of the "content reaches the system
prompt without a `deny_paths` check" gap shape found across
`aivyx-repomap` and `aivyx-core`'s context-injection sources. The
pattern of briefing a final whole-branch review to explicitly hunt for
"the same shape of gap elsewhere" has now found a real, previously
unknown instance in four consecutive features and is considered
established practice for this project's review dispatches going
forward.
```

- [ ] **Step 3: Commit**

```bash
git add ROADMAP.md docs/HISTORY.md
git commit -m "Document Agent::refresh_agents_files deny_paths enforcement"
```
