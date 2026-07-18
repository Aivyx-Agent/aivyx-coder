# Editor Approval Integration ("Context Out") — Design

**Status:** Approved by user 2026-07-19. Follow-on to
`docs/superpowers/specs/2026-07-18-editor-context-integration-design.md`
("context in," merged to `main` at commit `9076cfe`), which deliberately
scoped this problem out.

## Context

Editor-context-integration gave the agent read-only awareness of the user's
editor (open file, cursor, selection), injected as metadata into the system
prompt. It explicitly left "in-editor diff review or approval of proposed
edits" out of scope. This spec covers exactly that: letting the user review
and answer a pending permission request (a write, an edit, a delete, or a
shell command) from their editor, not just the terminal.

**Problem this spec solves:** today, `ConfirmationGate::check` blocks on
exactly one input — the TUI's own modal, answered by keypress. A user deep
in their editor has to context-switch to the terminal to review a diff and
press Allow/Deny. This spec adds a second, equally-trusted way to answer
that same pending decision, from the editor, without giving up the terminal
path.

Facts confirmed against the current codebase before this design was written:

- `ConfirmationGate::check(&self, request: &PermissionRequest) -> PermissionDecision`
  (`crates/aivyx-sandbox/src/confirmation.rs:146`) currently awaits exactly
  one thing: `self.prompter.prompt(request).await` (line 258), where
  `prompter: Arc<dyn PermissionPrompter>` and `PermissionPrompter::prompt`
  (`crates/aivyx-sandbox/src/lib.rs:159-160`) is a single `async fn` returning
  `UserResponse`. In the TUI, `TuiPrompter` (`crates/aivyx-tui/src/permission.rs:24-45`)
  implements this via an in-process `oneshot::channel` bridged to the render
  loop. `ConfirmationGate` itself already stores `cwd: PathBuf` as a field
  (`confirmation.rs:85`, alongside `deny_paths`/`plan_mode`/`autonomous_mode`)
  — so the new file-path helpers Change 3 introduces need no new plumbing to
  reach `cwd`; no signature change to `check` is required, only its body.
  This spec changes `check` to race the existing `prompter.prompt` await
  against a new polling future — see Change 1.
- `PermissionDecision` (`crates/aivyx-sandbox/src/lib.rs:68-77`) is
  `Allow | AllowAlways | Deny(Option<String>)`.
- `ActionKind` (`crates/aivyx-sandbox/src/lib.rs:37-59`) has six variants:
  `Read | Write | Execute | Delete | Internal | McpTool`. Only
  `Write | Execute | Delete | McpTool` ever reach the interactive-prompt
  step at all — `Read`/`Internal` auto-allow earlier in the gate's fixed
  tier order (`deny_paths` hard block → `Read`/`Internal` auto-allow →
  plan-mode deny → Always-Allow cache → interactive prompt), confirmed by
  reading `confirmation.rs`'s `check` body directly. This spec's scope is
  therefore automatically bounded to the four action kinds that can
  actually generate a pending request in the first place.
- `PermissionTarget` (`crates/aivyx-sandbox/src/lib.rs:61-66`) is
  `Path(PathBuf) | Command { program: String, args: Vec<String> } | Other(String)`.
  `PermissionRequest` (lines 25-35) already carries
  `preview: Option<String>` — a pre-rendered text summary (for
  `write_file`/`edit_file`, this is the output of `unified_diff(label, old,
  new)`, `crates/aivyx-tools/src/diff.rs:1-12`, called from
  `write_file.rs:50`/`edit_file.rs:140`) — but **not** the raw `old`/`new`
  strings themselves. This spec's decision to hand the editor structured
  content (not a pre-rendered diff string) requires a new field on
  `PermissionRequest` — see Change 2.
- Always-Allow caching (`confirmation.rs`) uses a private `PermissionKey`
  enum (`Path { action, path } | Command { program, args } | Other {
  action, description }`, lines 40-72) in a `Mutex<HashSet<PermissionKey>>`
  (line 86), constructed via `PermissionKey::from_request()`. Editor-side
  Always-Allow feeds this identical cache/key — no new cache is introduced.
- Session/editor-context file keying precedent (`session.rs`'s
  `session_file_path`, reused verbatim by editor-context's
  `editor_context_file_path`) is reused a third time here, same
  canonicalized-cwd FNV-1a hash, new subdirectory.
- Tool-call execution is confirmed sequential
  (`crates/aivyx-core/src/agent/mod.rs:1304-1339`'s `for` loop `.await`s
  each `dispatch` fully before the next iteration begins,
  `crates/aivyx-tools/src/lib.rs:202-245`'s `dispatch_inner` checks the gate
  once per call) — so at most one pending request/response file pair is
  ever in flight per project. No queueing or multi-request keying is
  needed.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Core problem**: "see the diff before approving" — not a live two-way
   buffer sync, not a post-hoc change notification. The editor becomes a
   second way to *answer a still-pending* decision, nothing more.
2. **Approval surface**: the editor is a fully alternate approval surface,
   not merely a read-only preview that still requires a terminal keypress.
3. **Trust model**: editor-side approval is fully equivalent to terminal
   approval — same local user, same machine, no new trust boundary is
   actually being crossed (just a second input surface for the one that
   already exists). `ConfirmationGate` treats a matching response-file
   decision exactly like the `oneshot` reply.
4. **Transport**: two polled files (request + response), not a socket.
   Latency is not the binding constraint (a human reads a diff on
   human-paced timescales regardless), so the added complexity of a
   socket's lifecycle (creation, cleanup, permissions, reconnect) isn't
   justified. Reuses the exact polling shape editor-context already
   established.
5. **Request scope**: every pending request the gate can raise (not just
   diff-bearing writes) — `Write`, `Delete`, `Execute`, and `McpTool`
   requests (the four that ever reach the interactive-prompt step) can all
   be answered from the editor, each with content appropriate to its kind
   (see Change 3).
6. **Race handling**: first decision wins. Both surfaces can be showing the
   same pending request simultaneously; whichever resolves first
   (terminal keypress or a matching response file) wins, the other is
   dropped. No editor connected → behavior is unchanged from today,
   exactly mirroring editor-context's "absent file = no-op" precedent.
7. **Diff content shape**: structured `old_content`/`new_content` (full
   text, not a unified-diff string), so an editor's own native diff view
   can render it idiomatically, rather than a preformatted text blob.
8. **Always-Allow**: the editor can answer with the same three options the
   terminal offers (Allow / Deny / Always-Allow), not a reduced two-option
   set — kept a true peer of the terminal, not a lesser view.
9. **Content sensitivity**: unlike editor-context (deliberately
   metadata-only), this pending-request file necessarily contains real file
   content or command text. Both files are 0600 (matching
   `config.toml`/session-file convention), written only while a request is
   genuinely pending, and deleted the instant it resolves (approve, deny,
   or superseded by the other surface) — not left on disk longer than
   strictly necessary.
10. **Default state**: `[editor_approval] enabled = true` by default, same
    as `editor_context` — the feature is inert without an active external
    process participating (writing to the response file), so "enabled"
    alone grants no new capability. (Noted during brainstorming: this is a
    deliberate, reasoned exception to this project's general preference for
    conservative security defaults when a real trust boundary is being
    extended — the exception holds specifically because inertness-without-a-
    participant is verifiably true here, not assumed.)

## Changes

### 1. `ConfirmationGate::check` races two futures

`crates/aivyx-sandbox/src/confirmation.rs`. Today, `check` calls
`self.prompter.prompt(request).await` directly after the cache-miss path.
This becomes a `tokio::select!` between that existing future and a new one
that:

- Writes the pending-request file (see Change 3 for its content), only if
  `editor_approval.enabled` (a new field the gate needs — see Change 4) and
  the project's context-file directory is resolvable (mirrors
  `editor_context_file_path`'s own `Option<PathBuf>` return — no directory
  resolvable means no editor-approval participation possible, not an
  error).
- Polls for the response file on the same interval editor-context already
  uses for its own polling.
- On finding a response file whose `request_id` matches the one just
  written, parses `decision` (`allow` / `deny` / `always_allow`) into a
  `PermissionDecision`.

Whichever branch of the `select!` resolves first wins; `tokio::select!`
already cancels the other branch's future by drop, which is exactly the "the
loser is dropped" behavior Decision 6 requires. **Both files are deleted
in both branches** — whether the terminal answered first (making the
now-unanswered pending-request file moot) or the editor did (making the
oneshot's now-unread reply moot, though the TUI-side prompter already
handles an unread `oneshot` sender being dropped as "fail closed to Deny"
per existing TUI documentation — this spec doesn't change that fallback,
it just means the editor's decision reaches `check`'s return value first).

### 2. `PermissionRequest` gains a `diff` field

`crates/aivyx-sandbox/src/lib.rs`. New field on the existing struct:

```rust
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: ActionKind,
    pub target: PermissionTarget,
    pub arguments_preview: serde_json::Value,
    pub preview: Option<String>,
    pub diff: Option<DiffContent>,  // new
}

pub struct DiffContent {
    pub old_content: String,
    pub new_content: String,
}
```

`write_file.rs:50` and `edit_file.rs:140` already compute `old`/`new`
strings at the exact point they call `unified_diff(...)` for the existing
`preview` field — this change is populating `diff` alongside `preview` from
data those call sites already have in scope, not new computation.
`delete_file`'s tool populates `diff` with `old_content` set and
`new_content` as an empty string (Change 3 documents how the pending-request
file distinguishes "delete" from "replace-with-empty" via a separate
`will_delete` flag, not by inferring it from an empty `new_content`).

### 3. Pending-request / response file schema

New module `crates/aivyx-core/src/editor_approval.rs` (sibling of
`editor_context.rs`, same rationale — standalone parsing/lifecycle concern,
not part of `Agent`'s own state machine).

Path helper mirrors `editor_context_file_path` exactly, new subdirectory:

```rust
pub(crate) fn editor_approval_request_path(cwd: &Path) -> Option<PathBuf> {
    // state_dir.join("editor-approval").join(format!("{hash:016x}-request.json"))
}
pub(crate) fn editor_approval_response_path(cwd: &Path) -> Option<PathBuf> {
    // state_dir.join("editor-approval").join(format!("{hash:016x}-response.json"))
}
```

Pending-request file (written by aivyx-coder, read by the editor plugin),
schema versioned like editor-context:

```json
{
  "schema_version": 1,
  "request_id": "a1b2c3d4-...",
  "action_kind": "write",
  "target": "src/foo.rs",
  "old_content": "fn foo() {}\n",
  "new_content": "fn foo() -> i32 { 42 }\n"
}
```

- `action_kind`: one of `"write"`, `"delete"`, `"execute"`, `"mcp_tool"` —
  the lowercase-snake mirror of `ActionKind`'s four prompt-reaching variants.
- Content fields vary by `action_kind`:
  - `write`: `old_content` + `new_content` (old empty string for a
    brand-new file).
  - `delete`: `old_content` + `"will_delete": true` (no `new_content` key
    at all — distinguishing "delete" from "replace with empty file"
    structurally, not by inferring intent from an empty string).
  - `execute`: `"command": "git"`, `"args": ["commit", "-m", "..."]`
    (mirrors `PermissionTarget::Command`'s own field names) — no diff
    content; this is a genuine gap for `git_commit` specifically (its own
    diff isn't plumbed to the gate today — showing one is future work, not
    this spec).
  - `mcp_tool`: `"description": "..."` (whatever `PermissionTarget::Other`
    already carries as a human-readable string, reusing
    `request.preview`/`arguments_preview` rather than inventing new content).
- `request_id`: a fresh UUID per request. Given confirmed-sequential tool
  execution, this exists purely as a correlation/staleness guard (a
  response file left over from a crashed previous run, or answering a
  request that's already been superseded, is ignored by `request_id`
  mismatch) — not for concurrent-request disambiguation.

Response file (written by the editor plugin, read by aivyx-coder):

```json
{
  "schema_version": 1,
  "request_id": "a1b2c3d4-...",
  "decision": "allow"
}
```

`decision`: `"allow"` | `"deny"` | `"always_allow"`. A `request_id` that
doesn't match the currently-pending request is ignored (stale response,
same silent-skip philosophy as every rejection path in editor-context).

The pending-request file is written by aivyx-coder itself, which sets its
permissions to 0600 explicitly (matching the `config.toml`/session-file
convention) rather than relying on the process umask — this is new,
sensitivity-driven behavior this spec introduces; editor-context's own file
carries no comparable sensitivity (metadata only, per that spec's Decision
5) and was never a case where aivyx-coder controlled the file's creation at
all (it's written by the not-yet-existing editor plugin, not by
aivyx-coder). The response file, by contrast, is created by the editor
plugin — aivyx-coder can read it but cannot control the permissions it's
created with; this is a real, documented limitation for whoever eventually
builds a plugin against this schema (worth a one-line callout in whatever
reference documentation accompanies this schema, the same way `AGENTS.md`'s
README section calls out that the file is user-controlled content).

### 4. `EditorApprovalConfig`/`EditorApprovalSettings`

Structural siblings of `EditorContextConfig`
(`crates/aivyx-core/src/agent/types.rs`) and `EditorContextSettings`
(`crates/aivyx-config/src/lib.rs`) respectively — same `pub(crate)`
visibility pattern for the former, same `pub`-with-`Default` pattern for the
latter:

```rust
// agent/types.rs
pub(crate) struct EditorApprovalConfig {
    // no fields beyond a presence marker are needed yet — unlike
    // editor_context, this feature doesn't need its own deny_paths copy:
    // deny_paths is already enforced upstream of ConfirmationGate ever
    // being reached for a denied path at all (the hard-block tier runs
    // before the interactive-prompt tier this spec extends).
}
```

```rust
// aivyx-config/src/lib.rs
pub struct EditorApprovalSettings {
    pub enabled: bool,  // Default: true (Decision 10)
}
```

Wired from `crates/aivyx/src/main.rs`, alongside the existing
`agent.set_editor_context(...)` call — the exact wiring mechanism (which
struct the config flag ultimately reaches — `ConfirmationGate` itself,
since that's where Change 1 lives, not `Agent`) is a plan-level detail:
`ConfirmationGate` is constructed in `main.rs` already, so this is most
likely a constructor parameter or setter on `ConfirmationGate` directly
rather than routing through `Agent` at all, unlike editor-context (which
had to reach `Agent` because the injection point lives there). This
distinction is worth flagging explicitly for whoever writes the
implementation plan.

## Out of scope for this spec

- Two-way live buffer sync (Decision 1) — this is unchanged from
  editor-context-integration's own out-of-scope list.
- Any specific editor's plugin/extension code — still editor-agnostic,
  schema-only, matching editor-context's Decision 6.
- A socket/IPC transport (Decision 4).
- Showing a real diff for `git_commit` specifically (Change 3's `execute`
  content shape) — its diff isn't plumbed to the gate today; command text
  only.
- Any change to `Read`/`Internal` request handling — they auto-allow before
  reaching the interactive-prompt tier and are untouched by this spec.
- Making the polling interval, file locations, or 0600 permission choice
  configurable.
- A timeout distinct from today's — the terminal path already blocks
  indefinitely with no timeout; this spec doesn't add one for the editor
  path either (a stale pending-request file is only ever cleaned up by the
  request resolving one way or another, exactly as today).

## Testing / verification

- Unit tests for `editor_approval_request_path`/`editor_approval_response_path`
  (matching `editor_context_file_path`'s own test style).
- Unit tests for each `action_kind` content-shape assembly (write with a
  brand-new file, write with a modified file, delete, execute, mcp_tool).
- Unit test confirming a response file with a mismatched `request_id` is
  ignored (decision falls through to the terminal path).
- Unit test confirming both files are deleted after resolution, regardless
  of which surface answered first.
- Unit test confirming Always-Allow from the editor populates the exact
  same `PermissionKey` cache entry a terminal Always-Allow would.
- Live E2E (through the real binary, PTY harness, per this project's
  established method): trigger a real pending `write_file` confirmation,
  hand-write a matching response file with `"decision": "allow"` before
  touching the terminal at all, and confirm the write proceeds with zero
  terminal keypress — then confirm both files are gone afterward.

## Sequencing

Written now, at the user's request, as a scoped, ready-to-build follow-on —
not necessarily implemented in this same session. Whether to proceed
straight to `writing-plans` now, or record this as a queued
`ROADMAP.md`/`docs/HISTORY.md` candidate direction for later pickup, is the
user's call at the review-gate step below, not decided by this spec.
