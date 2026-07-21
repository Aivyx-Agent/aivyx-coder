# Agent Learned Preferences (`remember_preference`) — Design

**Status:** Approved by user 2026-07-21.

## Context

The user asked to scope something like the sibling Aivyx Agent
platform's "Profile/Personality/Soul/Skills" system for aivyx-coder.
Research into that sibling project (a different, unrelated codebase —
`/home/julian/Projects/Rust/aivyx`) found it isn't four systems: it's
**Profile** (static, operator-declared config) and **Persona** (dynamic,
learned, HMAC-chained delta log requiring operator approval via a
reflection step) — "Soul" is explicitly just the product-vision name for
Persona, and "Skills" is one category riding the same delta chain, not a
fourth system. That architecture exists to solve a specific problem —
*a long-lived, autonomous, self-modifying personal assistant that must
never silently rewrite its own identity* — that aivyx-coder does not
share (no cross-session autonomous identity, deterministic-by-design,
no hidden state).

Investigation before any design work found the "static" half already
shipped, just unlabeled: `~/.config/aivyx-coder/AGENTS.md` (distinct
from the per-project `<cwd>/AGENTS.md`) is already loaded and folded
into every turn's system prompt, with its own token budget
(`Settings::agents_file_path()`, `crates/aivyx-config/src/lib.rs:629`;
`Agent::set_agents_file`, `crates/aivyx-core/src/agent/mod.rs:332`;
`AgentsFileConfig`, `crates/aivyx-core/src/agent/types.rs:126`). A user
can already write voice/style/preference instructions into that file
today and get them applied in every project. So this spec is narrowly
scoped to the one confirmed real gap: **the agent has no way to update
that file itself** — there is no "learning" without a human manually
editing it.

Facts confirmed against the current codebase before this design was
written:

- **`write_file`/`edit_file`/`delete_file` are not Landlock-confined.**
  `ctx.confiner.confine(command)` is called only in
  `crates/aivyx-tools/src/tools/run_command.rs:118` and
  `run_shell.rs:94` — Landlock/seccomp confinement applies exclusively
  to spawned child processes for those two tools. The file-writing tools
  call `tokio::fs::write`/equivalent directly
  (`crates/aivyx-tools/src/tools/write_file.rs`), with no OS-level path
  restriction at all — only `ConfirmationGate`'s `deny_paths` check and
  the interactive-approval tier bound them. `path_resolve::resolve`
  (`crates/aivyx-tools/src/path_resolve.rs`) explicitly supports and
  tests absolute paths passing through unchanged. This means, before
  this spec, generic `write_file`/`edit_file`/`delete_file` could
  *already* target `~/.config/aivyx-coder/AGENTS.md` (or `config.toml`,
  which can hold `backend.api_key`) — gated only by whether the user
  notices in the diff. Default `deny_paths` is
  `["~/.ssh", "~/.aws"]` (`PermissionSettings::default`,
  `crates/aivyx-config/src/lib.rs:521`) and does not cover it.
- **`ConfirmationGate::is_denied` only matches `PermissionTarget::Path`**
  (`crates/aivyx-sandbox/src/confirmation.rs:130-134`) — a
  `PermissionTarget::Other(_)` target is structurally exempt from
  `deny_paths`, regardless of what deny rule exists.
- **Autonomous mode (`--auto`) auto-allows `Write`/`Delete` actions on
  `Other` targets by default** — confirmed by reading
  `ConfirmationGate::check`'s autonomous-mode branch in full
  (`crates/aivyx-sandbox/src/confirmation.rs:230-268`): only
  `ActionKind::McpTool` gets an explicit, unconditional deny in that
  branch; every other `Write`/`Delete` on a `Path` *or* `Other` target
  otherwise falls through to `PermissionDecision::Allow` once past the
  (Path-only) cwd-boundary check. `McpTool` exists as its own
  `ActionKind` variant specifically so this unconditional-deny case can
  be expressed at all (doc comment,
  `crates/aivyx-sandbox/src/lib.rs:65-74`: "Always confirm-gated,
  uniformly: there is no case where this is treated as auto-allowed").
  Reusing `ActionKind::Write` for the new tool would silently let
  `--auto` (no human present at all) rewrite the user's cross-project
  global preferences unattended — unacceptable given this file persists
  and applies to every future session, unlike an in-worktree edit
  `--auto`'s existing checkpoint/rollback safety net already covers.
- `WriteFileTool`'s permission-request pattern
  (`crates/aivyx-tools/src/tools/write_file.rs:40-79`) — read current
  content, build a `unified_diff` + `DiffContent { old_content,
  new_content }`, handle "file doesn't exist yet" and "exists but not
  UTF-8" as distinct cases — is the direct model for the new tool's
  `permission_request`, just retargeted at a server-resolved fixed path
  instead of a model-supplied one.
- Config precedent: `AgentsFileSettings`/`EditorApprovalSettings`
  (`crates/aivyx-config/src/lib.rs:141-199`) — a simple `enabled: bool`
  (plus `budget_tokens` for `AgentsFileSettings`) struct, default `true`
  for both, added to the top-level `Settings` struct
  (`crates/aivyx-config/src/lib.rs:34-51`).

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Scope**: static persona/voice config is out of scope — it already
   exists (global `AGENTS.md`). This spec covers only the self-learning
   fast-follow: the agent proposing edits to that existing file.
2. **Mechanism**: no new approval machinery, no HMAC chain, no
   reflection step. The agent proposes an edit through a normal,
   `ConfirmationGate`-gated tool call — identical trust tier to any
   other `Write` action, just with its own `ActionKind` (see Decision 5)
   so autonomous mode can treat it correctly.
3. **New dedicated tool** (`remember_preference`) rather than extending
   `write_file`/`edit_file`'s reach: the model never needs to know or
   construct `~/.config/aivyx-coder/AGENTS.md`'s exact path (resolved
   server-side), the capability gets its own independent config flag,
   and tool-list intent is unambiguous ("propose an update to your
   long-term cross-project preferences") rather than relying on the
   model correctly inferring that a special absolute path passed to a
   generic write tool means something different from an ordinary
   project file write.
4. **Trigger timing**: both explicit request ("remember that...") *and*
   autonomous proposal when the model notices a clear, repeated
   pattern — not gated to explicit-request-only. System-prompt guidance
   (Change 4) instructs restraint: propose only for a genuinely
   repeated pattern, not a one-off, and don't re-propose something
   already declined in the same session.
5. **New `ActionKind::Memory`** (not `ActionKind::Write`), mirroring
   `McpTool`'s existing precedent exactly: always confirm-gated, no
   pre-approval/Always-Allow-cache tier (Decision 6), and — the
   concrete reason this variant is required, not just a taxonomy
   preference — unconditionally denied in autonomous mode, closing the
   real gap found in Context above. Named `Memory`, not `Persona`,
   deliberately: this spec doesn't introduce a "Persona" concept: the
   underlying artifact is still just `AGENTS.md`.
6. **No Always-Allow caching for this action — requires an explicit code
   change, corrected during this design's self-review.** Every
   `remember_preference` call must be reviewed individually; blanket-approving
   future edits after approving one would defeat the "you see every
   change" property this design exists to preserve. This is **not**
   automatic from picking `ActionKind::Memory`: `PermissionKey::from_request`
   builds an `Other { action, description }` key
   (`crates/aivyx-sandbox/src/confirmation.rs`'s `PermissionKey` enum),
   and `remember_preference`'s `PermissionTarget::Other(...)` description
   is a fixed constant string (Change 4) — so without an explicit fix,
   the *first* "Always Allow" click would cache a key that matches every
   future call regardless of content, silently auto-approving every
   later rewrite of the file forever. (Re-verified against
   `ConfirmationGate::check`'s interactive-mode path, lines 295-309:
   *every* `ActionKind` reaches the cache lookup/insert there —
   `McpTool` is not exempt in interactive mode, only in the
   autonomous-mode branch. My first draft of this section incorrectly
   assumed `McpTool`'s autonomous-mode exemption also applied here.)
   The precedent this actually should follow is `git_commit`'s: per
   `README.md`'s Security model section, `git_commit`'s target varies by
   commit message specifically so Always-Allow can never blanket-approve
   future, different commits. For `remember_preference`, the simpler
   equivalent is to skip the cache lookup/insert entirely for
   `ActionKind::Memory` in `ConfirmationGate::check`'s interactive-mode
   path (Change 2) — every call always reaches the interactive prompt,
   full stop, regardless of any prior "Always Allow."
7. **Hardening found during design, approved as in-scope**: add
   `~/.config/aivyx-coder` to default `deny_paths`
   (`PermissionSettings::default`), closing the pre-existing gap where
   generic `write_file`/`edit_file`/`delete_file`/`read_file`/`grep`
   could already reach the agent's own config directory (including a
   possible `api_key` in `config.toml`). `remember_preference`'s
   `PermissionTarget::Other` target is structurally exempt from this
   deny rule (Context, `is_denied` only matches `Path`), so it remains
   the sole sanctioned path to that one file. This is a default-only
   change — existing users' already-materialized `config.toml` won't
   pick it up automatically; noted in Out of Scope.
8. **No bespoke backup/rollback**: the existing checkpoint mechanism
   (`refs/aivyx/checkpoints/<ts>`) is a git-worktree snapshot of the
   *current project's* repo and structurally cannot cover a file outside
   any project. Given the file is small, human-readable prose, the
   pre-write diff review is the safety net; no new backup machinery.
   `remember_preference` participates in this project's *multi-file
   batch rollback* only in the trivial sense that it's Write-like — but
   since it's the only tool touching this specific file, and the
   rollback mechanism itself is checkpoint-based (see above), a
   mid-batch failure alongside a `remember_preference` call has nothing
   to roll the memory-file edit back *to* if it's not project-repo-scoped.
   Documented as a known limitation, not fixed here.

## Changes

### 1. `PermissionSettings::default()` gains a `deny_paths` entry

`crates/aivyx-config/src/lib.rs:521`, add `"~/.config/aivyx-coder"` to
the existing `vec!["~/.ssh".to_string(), "~/.aws".to_string()]` literal.

### 2. New `ActionKind::Memory` variant

`crates/aivyx-sandbox/src/lib.rs:54-74`, add a variant with a doc
comment mirroring `McpTool`'s, explaining the "always confirm-gated in
every mode, never cached, unconditionally denied in autonomous mode"
contract. Two separate changes to `ConfirmationGate::check`:

- **Autonomous-mode branch** (lines 230-268): a new explicit arm —
  `if request.action == ActionKind::Memory { ... unconditional deny ...
  }` — placed alongside (not replacing) the existing `McpTool` check,
  with its own denial-reason constant (e.g. `AUTONOMOUS_MEMORY_DENIAL`,
  mirroring `AUTONOMOUS_MCP_TOOL_DENIAL`).
- **Interactive-mode cache path** (lines 295-309): before the
  `PermissionKey::from_request`/`always_allow.lock().unwrap().contains(&key)`
  lookup, add `if request.action != ActionKind::Memory` (or an
  equivalent early branch) so a `Memory` action always calls
  `resolve_via_prompter_or_editor` directly and, on
  `UserResponse::AllowAlways`, does **not** insert into
  `self.always_allow` — see Decision 6 for why this is required, not
  optional.

### 3. New `[persona] enabled` setting

`crates/aivyx-config/src/lib.rs`, a new `PersonaSettings { enabled: bool
}` struct (mirroring `EditorApprovalSettings` exactly — no
`budget_tokens`, since this doesn't add new system-prompt content, it
gates a tool registration), default `true`, added to the top-level
`Settings` struct. (Section name `[persona]` even though the design
avoids the word "Persona" as a concept — this is the config-file-facing
toggle for "let the agent update its own AGENTS.md," and `[persona]`
reads clearly to a user in `config.toml` without needing "memory" to
collide with any other planned config section.)

### 4. New tool: `RememberPreferenceTool`

`crates/aivyx-tools/src/tools/remember_preference.rs`, modeled directly
on `write_file.rs`'s structure:

- **Args**: `{ content: String }` — the full proposed new content of the
  global `AGENTS.md`. No `path` argument; the tool resolves
  `Settings::agents_file_path()` itself. If that can't be resolved (no
  config dir — the same `None` case `Agent::set_agents_file` already
  handles), the tool isn't registered at all (see Change 5), so this
  can't occur inside `execute`.
- **`permission_request`**: read the *actual* current file content from
  disk (empty string if it doesn't exist yet — first-run case), build a
  `unified_diff` + `DiffContent` exactly like `WriteFileTool` does. This
  is what makes the diff trustworthy regardless of what the model
  "thinks" the current content is — the same reason `write_file` always
  reads from disk rather than trusting model-supplied "old" text.
  `PermissionRequest { action: ActionKind::Memory, target:
  PermissionTarget::Other("your global preferences (AGENTS.md)".to_string()),
  preview, diff, .. }`.
- **`execute`**: write `args.content` to the resolved path (creating the
  parent directory if needed, matching `write_file`'s existing
  behavior).
- **`mutates_outside_session()`**: default `true` (fail-closed) is
  correct and needs no override — this *does* mutate something outside
  the session, just not inside the project worktree, so it's
  automatically excluded from plan mode's tool list, matching every
  other mutating tool.
- **Tool description** (goes to the model): something like *"Propose an
  update to your own long-term memory of the user's preferences and
  working style — stored in a file that's automatically included in
  every future project, not just this one. Use this when the user
  explicitly asks you to remember something, or when you notice a
  clear, repeated pattern worth remembering (not a one-off). The user
  reviews every change as a diff before it's saved."*

### 5. Registration, gated by two conditions

In `crates/aivyx/src/agent_builder.rs` (the shared construction path
both the TUI and `aivyx-acp` already use, per the ACP integration —
this capability is available identically to both frontends with zero
extra work), register `RememberPreferenceTool` only when **all three**
hold: `settings.persona.enabled`, `settings.agents_file.enabled` (no
point letting the agent write preferences into a file this session
never reads back into context — avoids a confusing "I asked it to
remember something and it said yes, but nothing changed" outcome if a
user has AGENTS.md loading itself turned off), and
`Settings::agents_file_path()` resolving to `Some`. Mirrors the existing
`if settings.editor_context.enabled { ... }` /
`if !command_specs.is_empty() { registry.register(...) }` conditional-registration
pattern already used in this function.

### 6. README

New short subsection under wherever `AGENTS.md` is currently documented,
explaining: the global file already existed; this adds one new tool
letting the agent propose edits to it, gated the same way as any other
write; `[persona] enabled` (default on) to disable.

## Out of scope for this spec

- Any renaming/rebranding of `AGENTS.md` itself, or a new file format —
  it stays exactly what it is today (free-text markdown, no schema).
- Project-level (`<cwd>/AGENTS.md`) editing via this tool — scoped to
  the global file only. A project's own conventions are either already
  human-authored or editable via the ordinary `write_file`/`edit_file`
  path (`<cwd>/AGENTS.md` is inside the project, so `deny_paths` doesn't
  need to touch it).
- Migrating existing users' already-materialized `config.toml` to
  include the new `deny_paths` default — this is a `Default::default()`
  change, so it only applies to fresh installs; an existing user who
  wants the hardening needs to add the entry themselves or regenerate
  their config. Worth a one-line README callout, not a migration script.
- Effectiveness scoring, provenance tracking, or any other piece of the
  sibling project's `LearnedSkill`/Persona-delta machinery — this spec
  is one tool and one deny-list entry, nothing more.
- A `Skills` concept distinct from this (reusable named
  procedures/capability packs) — noted as a plausible separate future
  project, not started here.

## Testing / verification

- Unit tests for `RememberPreferenceTool::permission_request`: builds a
  correct diff against real current file content (existing file,
  missing file, and — mirroring `write_file`'s existing binary-file
  edge case — unreadable-as-UTF-8 existing file), and that
  `PermissionTarget`/`ActionKind` are exactly `Other(_)`/`Memory`.
- `ConfirmationGate` unit tests: a `Memory`-action request is
  unconditionally denied under autonomous mode regardless of target
  content (mirroring the existing
  `autonomous_mode_denies_mcp_tool_calls_unconditionally` test almost
  exactly); a `Memory`-action request is *not* affected by `deny_paths`
  even when the new default entry is active (since its target isn't a
  `Path`); confirm the new default `deny_paths` entry actually blocks a
  `write_file`/`read_file` call targeting a path under
  `~/.config/aivyx-coder`. **Load-bearing regression test**: an
  `UserResponse::AllowAlways` response to one `Memory`-action request
  must NOT cause a second, later `Memory`-action request (same fixed
  `PermissionTarget::Other` description, different diff content) to be
  auto-approved from the cache — assert the second call still reaches
  the prompter. This is the test that would have caught this design's
  own first-draft error (see Decision 6).
- Live E2E: a real model, given a clear instruction to remember a
  stated preference, calls `remember_preference`; approving the diff
  updates the real file; a fresh session in a different project
  directory shows the updated content folded into its system prompt
  (proving the "applies everywhere, not just this project" property end
  to end, not just at the config layer).

## Sequencing

Single implementation unit — small enough not to need decomposition:
one new `ActionKind` variant + one `ConfirmationGate` branch, one
`deny_paths` default entry, one new tool, one new config flag, wired
into the one shared `agent_builder.rs` construction path both frontends
already use.
