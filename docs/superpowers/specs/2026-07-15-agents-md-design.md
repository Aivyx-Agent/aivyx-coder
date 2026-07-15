# AGENTS.md Project Instructions — Design

**Status:** Approved by user 2026-07-15. Highest-priority item surfaced by a
tool/capability audit against ROADMAP.md's Phase 9 tracking.

## Problem

Every comparable coding agent (Claude Code's `CLAUDE.md`, Cursor's
`.cursorrules`, Aider's `.aider.conf.yml` conventions, and the emerging
cross-tool `AGENTS.md` standard) lets a user state project-level guidance
once — conventions, architecture notes, "don't touch X," build/test
commands, style preferences — and have it auto-loaded into every session.
aivyx-coder has no equivalent: every session re-derives project context from
scratch (repo map, `grep`, `read_file`), with nothing letting persistent
guidance survive between sessions. This was identified as the single
highest-leverage gap in the current tool set, since it improves every
session rather than a specific workflow.

This design covers reading and injecting `AGENTS.md` content only.
aivyx-coder remains local-only-first per current project direction; nothing
here assumes or depends on cloud LLM provider support.

## File convention and scope

- **Filename**: `AGENTS.md` — the emerging cross-tool standard, not an
  aivyx-specific name. A repo already using it for another tool needs zero
  duplication; aivyx-coder reads the same file.
- **Two locations, both optional**:
  - Project: `<cwd>/AGENTS.md`.
  - User-global: `<config_dir>/AGENTS.md`, sibling to `config.toml` (same
    `ProjectDirs` resolution already used by `Settings::config_path()`) —
    for preferences that apply across every project ("always use tabs",
    "prefer terse commit messages").
- **No nested/subdirectory merging** (unlike Claude Code's walk-up-the-tree
  pattern) — exactly these two files, no more.
- **No frontmatter, no structured metadata** — plain prose, included as-is.
  This is simpler than `/wiki`'s pages: there's no staleness state to
  track, since the file's content is never machine-generated or partially
  regenerated.
- **Neither file existing is not an error or a warning** — silently
  contributes nothing, matching how an empty repo map degrades gracefully
  for a non-Rust project.

## Merge order and precedence

Global content renders first, project content second, each under its own
label:

```
User preferences (~/.config/aivyx-coder/AGENTS.md):
<global file content>

Project instructions (AGENTS.md):
<project file content>
```

A short connective note states that project-specific instructions take
precedence over user preferences if they conflict — general-then-specific,
later-wins, the same convention this project's own multi-repo workspace
`CLAUDE.md` setup already uses (a general workspace-level file explicitly
defers to each repo's own).

## Refresh timing

Refreshed **every turn**, not read once at startup — reusing the exact
mechanism `Agent::refresh_repo_map()` already established (called once per
turn, not once per LLM round-trip within a turn, for cost reasons). A user
who edits `AGENTS.md` mid-session sees the change apply on the very next
turn, no restart needed. This is a deliberate divergence from
`SYSTEM_PROMPT_PREAMBLE`/`TOOL_GUIDANCE_*`, which remain static (built once
at process start) — those describe how aivyx behaves in general, not
anything a user is expected to iterate on mid-session, so the tradeoff
differs.

Implementation shape:
- New `Agent` field: `agents_files_text: Option<String>`.
- New method `async fn refresh_agents_files(&mut self, cwd: &Path)`, called
  in `run_turn_inner` alongside the existing `refresh_repo_map()` call.
- Plain `tokio::fs::read_to_string` for both files — no `spawn_blocking`
  needed (unlike the repo map's tree-sitter parse + PageRank, this is pure
  I/O, not CPU-bound work).
- Any read error (missing file, permission denied, invalid UTF-8) for
  either file is treated as "no content from this source," never an
  `AgentError` — matching the "silently contributes nothing" behavior
  above.

## Prompt position

Appended in `Agent::assemble_messages()` alongside the other *behavioral*
directive blocks (`PLAN_MODE_PROMPT`, `VERIFICATION_PROMPT`,
`AUTONOMOUS_PROMPT`) — before the repo map, not after. Reasoning: `AGENTS.md`
content is "how to behave on this project," thematically closer to those
directive blocks than to the repo map's reference-data role (dense symbol
data the model consults as needed, distinct in kind from prose guidance).

Also included in `Agent::prompt_chars()`'s size estimate, exactly as
`repo_map_text` already is — omitting it there would make the context-budget
indicator and compaction trigger silently blind to real prompt weight.

## Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsFileSettings {
    pub enabled: bool,
    pub budget_tokens: u32,
}

impl Default for AgentsFileSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_tokens: 1024,
        }
    }
}
```

Added as `pub agents_file: AgentsFileSettings` on the top-level `Settings`
struct. `enabled: true` by default — an absent file costs nothing at
runtime, so there is no reason to force an opt-in (matches `repo_map`'s own
default-on precedent). When `enabled: false`, `refresh_agents_files` is a
no-op regardless of what files exist.

A new `Settings::agents_file_path() -> Result<PathBuf, ConfigError>`
mirrors the existing `config_path()`, resolving `<config_dir>/AGENTS.md`.

## Budget and overflow handling

`[agents_file] budget_tokens` (default `1024`, matching `repo_map`'s own
default) is applied **per file independently**, not as a combined pool —
avoiding cross-file arbitration logic entirely.

Unlike the repo map (auto-generated, rankable, safe to truncate by
dropping lower-ranked symbols), `AGENTS.md` is hand-written prose with no
natural cut point. Silently chopping it mid-sentence would misrepresent
what guidance the model actually received — a materially worse failure
mode than a hand-authored `README.md` getting cut off, since the user has
no way to know which half of their instructions the model actually saw.

Resolution: **a file over its budget is still included in full**, but
emits exactly one `AgentEvent`/TUI notice per refresh where it's over
("`AGENTS.md` is 340 tokens over your configured budget — trim it or raise
`[agents_file] budget_tokens`"), using the same `chars_per_token`
calibration the rest of the agent's size estimation already uses. The user
always gets the complete guidance they wrote, plus a clear, actionable
signal to either trim it or raise the budget — never a silent, partial
truncation of content they authored deliberately.

## Testing strategy

- Unit tests covering all four presence combinations (neither /
  global-only / project-only / both present), verifying correct
  labeling and ordering (global before project) in the combined output.
- A test confirming the connective precedence note appears whenever both
  files are present, and is absent when only one is.
- Budget-exceeded test: full content is still included in the assembled
  prompt, and exactly one notice event fires (not one per turn if the
  agent doesn't re-refresh in between, not zero).
- Budget-within test: no notice event fires.
- A per-turn refresh test: editing file content between two simulated
  turns changes what the second turn's assembled prompt contains — proving
  the mechanism is genuinely dynamic, not cached from the first read.
- A `prompt_chars()` test confirming the combined `agents_files_text` is
  counted toward the size estimate.
- `[agents_file] enabled = false` test: no content included even when both
  files exist and are non-empty.
- Read-error handling test (e.g. a directory named `AGENTS.md` instead of
  a file, to trigger a real I/O error) — confirms graceful "no content
  from this source," not a turn-ending error.
- One live E2E through the real binary: an `AGENTS.md` with a distinctive,
  easily-verified instruction, confirming the model's actual behavior in a
  live session reflects it.

## Non-goals

- No nested/subdirectory `AGENTS.md` merging (Claude Code's multi-level
  walk-up-the-tree pattern) — exactly two fixed locations.
- No frontmatter or other structured metadata inside the file.
- No file-watcher (inotify or similar) — per-turn refresh already picks up
  edits without needing push notifications, at the cost of at most one
  turn's staleness, which matches the repo map's own refresh cadence.
- No separate enable flags for the global file vs. the project file — one
  `[agents_file] enabled` governs both.
- No cloud-provider-specific behavior of any kind — this feature is
  provider-agnostic by construction (it only ever appends text to the
  system prompt), and nothing in this design assumes or requires cloud LLM
  support.
