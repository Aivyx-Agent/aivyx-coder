# Editor/IDE Context Integration — Design

**Status:** Approved by user 2026-07-18. Scoped as a future ROADMAP phase — not
committed to an implementation timeline by this spec alone (see "Sequencing"
at the end).

## Context

aivyx-coder is a self-contained terminal (TUI) application — no editor/IDE
integration exists today. All file editing happens through the agent's own
tools (`write_file`, `edit_file`) operating directly on the filesystem, gated
by the existing `ConfirmationGate`/`ActionKind` permission-tier model. A
read-only LSP integration exists (`go_to_definition`, `find_references`, via a
`rust-analyzer` subprocess) for code intelligence, but it is not an editing
channel and has no relationship to any editor's UI.

**Problem this spec solves:** the agent has no live awareness of what the
user is currently looking at in their editor (open file, cursor position,
selection). A user has to paste code or describe a location in prose to say
"fix this function." Nothing else — this spec deliberately does not cover a
chat-panel/sidebar UI, in-editor diff review, two-way file sync, or any
specific editor's extension marketplace presence. Those are separate,
unscoped problems for a later spec if ever pursued.

Facts confirmed against the current codebase before this design was written:

- Session files are already keyed by a stable hash of the canonicalized
  `cwd`: `crates/aivyx-core/src/session.rs`'s `session_file_path` builds
  `state_dir.join("sessions").join(format!("{key}.json"))` where `key` is
  `"{sanitized-dirname}-{fnv1a-hash:016x}"`, under
  `directories::ProjectDirs::from("", "", "aivyx-coder").state_dir()`
  (`~/.local/state/aivyx-coder/` on Linux). This spec's context file follows
  the identical keying scheme, for consistency and because it needs the same
  "per-project, stable across canonicalization/symlinks" property.
- `AGENTS.md` is the closest existing precedent for "ambient context
  re-read and injected into the system prompt every turn, not just once at
  startup": `crates/aivyx-core/src/agent/mod.rs`'s `refresh_agents_files`
  (async, takes `cwd: &Path`, reads file(s), stores a `String` in
  `self.agents_files_text`); `assemble_messages` (same file) appends that
  text unconditionally after the verification/autonomous prompts and before
  `repo_map_text`. This spec's editor-context text follows the same
  refresh-then-append shape, appended after `repo_map_text` (last in the
  chain — it's the most ephemeral/specific-to-this-instant of everything
  injected).
- `deny_paths` checking is `pub(crate)` today, not `pub`:
  `crates/aivyx-sandbox/src/lib.rs:211`,
  `pub(crate) fn path_is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool`.
  `Agent` itself holds no `deny_paths` field today — every other consumer
  (`GrepTool::new(deny_paths.clone())`, `GitReadTool::new(deny_paths.clone())`,
  etc. in `crates/aivyx/src/main.rs`) receives it directly via its own
  constructor. This is a genuinely new requirement for `Agent`, not an
  existing capability to reuse as-is — see Changes section for the two
  concrete edits this implies.
- `AgentConfig`/`AgentError`/etc. and their siblings (`VerificationConfig`,
  `AgentsFileConfig`) live in `crates/aivyx-core/src/agent/types.rs`
  (post-codebase-cleanup split). `AgentsFileConfig` is the direct structural
  precedent for this spec's new config struct.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Problem scope**: live editor-context awareness only (open file, cursor,
   selection) — not a chat UI, not diff-review integration, not file sync.
2. **Editor scope**: editor-agnostic from the start. The deliverable is a
   versioned file-format contract any editor's integration could write to,
   not a plugin for one specific editor.
3. **Transport**: a polled local file, not a socket/IPC channel. No daemon,
   no listener lifecycle — matches this project's existing file-based
   patterns (sessions, config, `AGENTS.md`).
4. **Usage pattern**: auto-injected into the system prompt every turn (like
   `AGENTS.md`/repo map), not an on-demand tool call or user-invoked command.
5. **Injected content**: **metadata only** — file path, cursor line/column,
   selection line range. **Never the actual selected/file text.** The model
   calls `read_file` itself for real code, exactly as it already does for
   everything else. This is a deliberate security choice: it keeps this
   project's existing invariant intact (file content only ever enters the
   conversation through an explicit, visible tool call) and avoids
   introducing a new path for untrusted file content to land directly in
   the trusted system-prompt position every turn.
6. **Reference plugin**: **out of scope for this phase.** Only the
   aivyx-coder side (schema definition, file reading, staleness/`deny_paths`
   handling, prompt injection) ships. No VS Code/Neovim/other plugin code.
   The schema is the contract a future plugin — by the user or a
   contributor — would implement against.

## Changes

### 1. Context file location and keying

New helper, `crates/aivyx-core/src/editor_context.rs` (new file — this
doesn't belong inside `agent/` since it's a standalone concern with its own
parsing/staleness logic, not part of the `Agent` orchestration state
machine itself; `agent/mod.rs` only calls into it, mirroring how `session.rs`
is a sibling module `Agent` calls into rather than inlined into `agent/`):

```rust
pub fn editor_context_file_path(cwd: &Path) -> Option<PathBuf> {
    // Identical construction to session::session_file_path, but under
    // an "editor-context" subdirectory instead of "sessions", and without
    // the human-readable name prefix (machine-written/read only, no
    // resume-by-eye use case) — just the hex hash:
    // state_dir.join("editor-context").join(format!("{hash:016x}.json"))
}
```

### 2. Versioned JSON schema

```json
{
  "schema_version": 1,
  "workspace_root": "/abs/path/to/project",
  "file": "src/foo.rs",
  "cursor": { "line": 42, "column": 8 },
  "selection": { "start_line": 40, "end_line": 45 },
  "updated_at": "2026-07-18T12:00:00Z"
}
```

- `schema_version`: integer, `1` today. A reader encountering any other
  value ignores the file entirely (treats it as absent) rather than
  attempting best-effort parsing — this is a machine-to-machine contract,
  not a human-edited config file, so silent partial-parsing of a future
  incompatible version is a worse failure mode than "no context this turn."
- `workspace_root`: absolute path. Must canonicalize to the same path as
  aivyx-coder's own `cwd` (same canonicalization the session keying already
  does) or the file is ignored — this is the mechanism preventing a stale
  file from a different, previously-open project leaking into the wrong
  session.
- `file`: relative to `workspace_root`.
- `cursor`: required. 1-indexed line and column, matching this project's
  existing convention for `go_to_definition`/`find_references`/`grep`
  output (`path:line:column`/`path:line:text`).
- `selection`: optional, omitted entirely (not `null`) when there is no
  active selection. 1-indexed, inclusive line range. No column-level
  selection granularity in v1 — line-range is enough for "fix this
  function" and keeps the schema simple; a future schema version can add
  column-level range if a real need shows up.
- `updated_at`: RFC 3339 timestamp. A file whose `updated_at` is more than
  5 minutes old (not configurable — a fixed, generous threshold; this is a
  staleness guard against a crashed/closed editor integration, not a
  precision timer) is treated as absent.

### 3. `EditorContextConfig` (new, in `crates/aivyx-core/src/agent/types.rs`)

Structural sibling of `AgentsFileConfig`:

```rust
pub(crate) struct EditorContextConfig {
    pub(crate) deny_paths: Vec<PathBuf>,
}
```

(Same `pub(crate)`-with-`pub(crate)`-fields visibility as `AgentsFileConfig`
and `VerificationConfig` — constructed and read by `impl Agent` in the
parent `agent/mod.rs` module, never referenced outside `aivyx-core`.)

### 4. `Agent::set_editor_context` (new, in `crates/aivyx-core/src/agent/mod.rs`)

Mirrors `set_agents_file`'s shape exactly:

```rust
pub fn set_editor_context(&mut self, deny_paths: Vec<PathBuf>) {
    self.editor_context_config = Some(EditorContextConfig { deny_paths });
}
```

Wired from `crates/aivyx/src/main.rs`, alongside the existing
`agent.set_agents_file(...)` call, passing the same `deny_paths.clone()`
already computed there (`settings.permissions.resolved_deny_paths()`) —
gated behind a new `[editor_context] enabled` config flag (default `true`;
this only ever activates *reading* a file that may not exist, so an
`enabled` default of `true` is safe the same way `repo_map.enabled`
defaults to `true` — the feature is a no-op until an editor integration
exists and writes the file).

### 5. `Agent::refresh_editor_context` (new, in `crates/aivyx-core/src/agent/mod.rs`)

Mirrors `refresh_agents_files`'s refresh-then-store-a-`String` shape,
called from the same place `refresh_agents_files` is currently called
(`run_turn_inner`, per-turn):

- Return early (clear `self.editor_context_text` to `None`) if
  `self.editor_context_config` is `None` (feature disabled).
- Resolve the context file path via `editor_context::editor_context_file_path(cwd)`.
  Missing file, unparseable JSON, wrong/unrecognized `schema_version`, stale
  `updated_at`, or `workspace_root` not matching `cwd`'s own canonicalized
  form → treat as absent, clear `self.editor_context_text` to `None`. None
  of these are user-facing errors or notices (unlike `AGENTS.md`'s
  over-budget notice) — an editor integration not running, or a stale
  leftover file, is an entirely normal, silent state, not a misconfiguration
  worth surfacing.
- Otherwise, check `file` (resolved against `workspace_root`) against
  `config.deny_paths` via `aivyx_sandbox::path_is_denied` (requires changing
  that function's visibility from `pub(crate)` to `pub` in
  `crates/aivyx-sandbox/src/lib.rs` — the one cross-crate visibility change
  this spec requires). If denied, treat as absent (same silent-skip
  behavior — a denied path being open in the user's editor is not an error
  condition to report, just not something to surface).
- Otherwise, format and store:
  - No selection: `"Currently open in editor: {file}, cursor at line {line}."`
  - With selection: `"Currently open in editor: {file}, cursor at line
    {line}, with lines {start}-{end} selected."`

### 6. `assemble_messages` (modify, in `crates/aivyx-core/src/agent/mod.rs`)

Append `self.editor_context_text` after the existing `repo_map_text` block,
following the identical `if let Some(text) = &self.editor_context_text {
system.push_str("\n\n"); system.push_str(text); }` pattern already used for
`agents_files_text`/`repo_map_text`.

### 7. `crates/aivyx-sandbox/src/lib.rs` — visibility change

`pub(crate) fn path_is_denied` → `pub fn path_is_denied`. No behavior
change — purely a visibility widening so `aivyx-core` can call it. (This
crate is already a direct dependency of `aivyx-core`, so no new dependency
edge is introduced.)

### 8. Config reference documentation (README.md)

A new `[editor_context]` section documented alongside the existing
`[agents_file]`/`[repo_map]` sections in README's "Configuration reference,"
plus a new subsection (parallel to `AGENTS.md`'s own README subsection)
documenting the JSON schema (Change 2, verbatim) as the contract for anyone
building an editor integration.

## Out of scope for this spec

- Any specific editor's plugin/extension code (VS Code, Neovim, JetBrains,
  or otherwise) — Decision 6.
- A chat panel, sidebar, or any in-editor UI.
- In-editor diff review or approval of proposed edits.
- Two-way file sync or live-reload coordination between aivyx-coder's edits
  and an already-open editor buffer.
- Column-level selection granularity (only line ranges in v1 — see Change 2).
- A socket/IPC transport (Decision 3) — if a future need for lower-latency
  push updates emerges, that's a new schema version and a new spec, not an
  extension of this one.
- Making the staleness threshold or context-file location configurable —
  both are fixed constants in this version.

## Testing / verification

- Unit tests for `editor_context_file_path` (matches the existing
  `session_file_path` test style: same-input-same-path stability,
  canonicalization handling).
- Unit tests for the parsing/staleness/`workspace_root`-mismatch/
  `deny_paths` logic in `refresh_editor_context` — each rejection path
  (missing file, bad JSON, wrong schema version, stale timestamp, mismatched
  `workspace_root`, denied path) gets its own test confirming
  `editor_context_text` ends up `None` and no notification fires.
- Unit test confirming `assemble_messages` includes the formatted line in
  the system prompt when `editor_context_text` is `Some`, and confirming
  the *exact* wording contains no file content — only path/line/column
  text — as a regression guard for Decision 5's security property.
- Live E2E (through the real binary, PTY harness, per this project's
  established method): hand-write a valid context JSON file at the correct
  path before starting a session, send a message like "what line is my
  cursor on?", and confirm the model's answer reflects the injected
  metadata — proving the plumbing works end to end even with no real
  editor plugin involved yet.

## Sequencing

This spec is being written now, at the user's request, as a **scoped,
ready-to-build future phase** — not necessarily implemented in this same
session. Whether to proceed straight to `writing-plans` now or record this
as a queued `ROADMAP.md`/`docs/HISTORY.md` "candidate direction" (the same
treatment Phase 11's three candidate directions received) for later pickup
is the user's call at the review-gate step below, not decided by this spec.
