# `Agent::refresh_agents_files` `deny_paths` Enforcement — Design

**Status:** Approved by user 2026-07-30.

## Context

Found at the `wiki_pointer_lines` `deny_paths` enforcement fix's own final
whole-branch review (2026-07-30) and logged to `ROADMAP.md`'s backlog
rather than fixed mid-review, since it was out of that fix's stated scope:
`Agent::refresh_agents_files` (`crates/aivyx-core/src/agent/mod.rs:520`)
reads both the global and project `AGENTS.md` files and splices their
content into the system prompt every turn, with **no `deny_paths` check at
all** — unlike its sibling `refresh_editor_context`
(`crates/aivyx-core/src/agent/mod.rs:441`), which explicitly calls
`aivyx_sandbox::path_is_denied` before surfacing anything.
`AgentsFileConfig` (`crates/aivyx-core/src/agent/types.rs:150`) has no
`deny_paths` field at all, so there's currently no way to wire a check in
without adding one.

Concretely: a user who denies a path matching either their global
`AGENTS.md` (`~/.config/aivyx-coder/AGENTS.md`, resolved via
`Settings::agents_file_path()`) or the project `<cwd>/AGENTS.md` would
still have that file's full content reach the model's system prompt via
`refresh_agents_files`. Low realistic severity (an `AGENTS.md` a user wrote
themselves is unlikely to also be a credentials file), but it's the same
"content reaches the prompt without any deny check" shape the last two
features closed for `aivyx-repomap`'s `collect_tags` and
`wiki_pointer_lines`, now found in a fourth function.

## Decisions

### Mirror `EditorContextConfig`/`refresh_editor_context` exactly — no new config schema

`AgentsFileConfig` gains a `deny_paths: Vec<PathBuf>` field, exactly
matching `EditorContextConfig`'s existing field of the same name and type:

```rust
pub(crate) struct AgentsFileConfig {
    pub(crate) global_path: Option<PathBuf>,
    pub(crate) budget_tokens: u32,
    pub(crate) deny_paths: Vec<PathBuf>,
}
```

`Agent::set_agents_file` gains a `deny_paths: Vec<PathBuf>` parameter:

```rust
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

`crates/aivyx/src/agent_builder.rs:473` passes the same global
`deny_paths` list already passed to `set_editor_context` one line below it
(`agent_builder.rs:479`) — no new settings field, no new config surface.
Both call sites already have the resolved list in scope as `deny_paths`
(from `settings.permissions.resolved_deny_paths()`, `agent_builder.rs:82`).

### `refresh_agents_files` checks both paths before reading, silently skips a denied one

Mirroring `refresh_editor_context`'s silent-skip behavior (approved by
user over the alternative of a user-facing notice): a denied `AGENTS.md`
is treated the same as a missing or unreadable one — it simply
contributes no section, with no notice. This keeps the two "file-based
context source" functions behaviorally consistent, and avoids treating a
user's own `deny_paths` configuration as a misconfiguration worth
flagging (unlike the existing over-budget notice, which flags something
the user likely didn't intend).

```rust
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
        // ...unchanged...
    }

    if !aivyx_sandbox::path_is_denied(&project_path, &config.deny_paths)
        && let Ok(content) = tokio::fs::read_to_string(&project_path).await
    {
        // ...unchanged...
    }

    // ...unchanged...
}
```

Both checks run before the corresponding `read_to_string` call, exactly
mirroring `refresh_editor_context`'s check-then-read ordering (never
read-then-discard).

## Out of scope for this spec

- Any change to `path_is_denied` itself, or to `EditorContextConfig`/
  `refresh_editor_context` — both already correct.
- A new config field or settings surface for AGENTS.md-specific
  `deny_paths` — the global list is reused, matching every other
  path-checking tool in this project.
- The over-budget notice's own behavior — unrelated to this fix.

## Testing / verification

Two unit tests added to `crates/aivyx-core/src/agent/mod.rs`'s existing
test module, mirroring the shape of `refresh_editor_context`'s existing
deny-path test:

- A `deny_paths` entry matching the project `AGENTS.md` path: confirms
  `agents_files_text` does not contain the project file's content after
  `refresh_agents_files` runs, while a non-denied global `AGENTS.md`'s
  content still appears.
- A `deny_paths` entry matching the global `AGENTS.md` path: confirms
  `agents_files_text` does not contain the global file's content, while a
  non-denied project `AGENTS.md`'s content still appears.

No live E2E follow-up needed — straightforward, fully unit-testable fix
with no external service dependency, same bar as `wiki_pointer_lines`.

## Documentation

`ROADMAP.md`'s backlog paragraph for this item is removed and replaced
with a "shipped" paragraph in Current status, matching every other closed
backlog item's treatment. `docs/HISTORY.md` gets a short narrative
chapter, matching the `wiki_pointer_lines` chapter's brevity.
