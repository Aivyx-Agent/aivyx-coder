# Aivyx-Skills Integration (Part 2: aivyx-coder) Design

## Context

Part 2 of a three-part cross-repo initiative: wire the newly-shipped,
shared `aivyx-skills` crate (`Aivyx-Agent/aivyx-skills`, public — a
default, system-level `SKILL.md`-format capability library, 5 real
bundled skills: `systematic-debugging`, `brainstorming-and-scoping`,
`writing-plans`, `self-review-before-done`, `clear-communication`) into
`aivyx-coder` so its own agent can discover and use them, plus an
optional project/user skill overlay. Part 1 (the crate itself) is
already shipped; Part 3 (`aivyx-pa`'s own integration) is separate,
later, unstarted scope in that other repo.

## Grounding

Read directly in the current codebase, not assumed:

- **`aivyx-coder` has no existing "skill" concept of any kind** —
  confirmed via a direct grep across `README.md`/`CLAUDE.md`/
  `ROADMAP.md` before any design started. A clean slate; no naming
  collision to reconcile (unlike `aivyx-pa`, which has its own
  unrelated, pre-existing agent-learned `Skill`/`LearnedSkill` system).
- **A directly reusable precedent already exists for "a persistent,
  low-cost system-prompt block, re-rendered each turn, no separate
  discovery tool call"**: `Agent`'s existing `repo_map_text: Option<String>`
  field (`crates/aivyx-core/src/agent/mod.rs`), set via
  `set_repo_map(map, budget_tokens)`, folded into `system_prompt_text()`
  every turn. `agents_files_text` (the `AGENTS.md` support) follows the
  identical shape. This is the model for how the model discovers which
  skills exist — no `list_skills` tool needed, the same way there's no
  `list_repo_map` tool.
- **Established precedent for a tool's own description interpolating
  real, current data** (not just relying on the system-prompt block
  alone): `spawn_specialist`'s `definition()` interpolates
  `specialist_roster_description(&self.config.team)` so the model learns
  valid member names directly from the tool result, belt-and-braces with
  whatever the system prompt separately says. `load_skill`'s own
  `definition()` will do the same for skill names.
- **Established config-section shape**: `TeamSettings` (`crates/aivyx-config/src/lib.rs:471+`)
  — a `pub enabled: bool` plus feature-specific fields, `#[serde(default)]`
  at the struct level so an absent `[team]` section in a user's
  `aivyx.toml` still parses. `resolved_roster_path()` — tilde-expansion
  via the existing `resolve_tilde_paths` helper, falling back to the raw
  string unchanged if resolution fails — is the established pattern for
  any settings field that's a filesystem path.
- **Established internal-tool permission classification**: tools whose
  target isn't a real project-relative filesystem path (so
  `PermissionTarget::Path` + `deny_paths` checking doesn't apply) use
  `internal_permission_request(tool_name)` (`ActionKind::Internal`,
  `PermissionTarget::Other(tool_name)`) — the pattern `spawn_specialist`/
  `query_specialist`/`close_specialist`/`decompose_task` already use. A
  skill name isn't a filesystem path, so `load_skill` follows this same
  pattern rather than `read_file`'s `ActionKind::Read` +
  `PermissionTarget::Path`. Both `Internal` and `Read` auto-allow at the
  same gate tier, so this is a classification-honesty choice, not a
  behavioral one.
- **`AGENTS.md` content — the closest existing analog to a skill
  overlay (repo-sourced text, folded into the system prompt) — is
  already run through `aivyx_sandbox::scan_for_injection_markers` before
  use**, confirmed directly (`crates/aivyx-core/src/agent/mod.rs:851,868`,
  both the user-level and project-level `AGENTS.md` paths), tagging the
  shared `InjectionTaint` on a match exactly like the repo map and editor
  context sources do. `aivyx-skills`'s own README explicitly flags that
  overlay directories read untrusted local filesystem content and defers
  the sanitization decision to whichever consumer integrates it — this
  is that decision, and the precedent above makes it a direct
  application of an already-established pattern, not a novel one.
- **`aivyx-skills` has no GitHub remote at design time** — resolved
  during this same brainstorming session: a public
  `Aivyx-Agent/aivyx-skills` remote was created and the existing local
  history pushed, specifically so this integration can pin it via `git`
  + `rev` exactly like `aivyx-confine`/`aivyx-checkpoint`/`aivyx-kvcache`/
  `aivyx-injection-guard` already are — no temporary path-dependency
  workaround needed.
- **`aivyx-skills`'s real public API** (confirmed directly from its own
  shipped `src/lib.rs`/`src/loader.rs`): `SkillLoader::new()`,
  `.with_project_dir(PathBuf) -> Self`, `.with_user_dir(PathBuf) -> Self`,
  `.list() -> Vec<SkillSummary>` (sorted by name, `{name, description,
  source}`), `.get(name: &str) -> Option<Skill>` (`{name, description,
  body, source}`). Both builders treat a nonexistent directory as "no
  overrides from this source," not an error — no defensive existence
  check needed on the `aivyx-coder` side before passing a configured path
  through.

## Decisions

**1. `[skills]` is a new config section, on by default.** `enabled:
bool` (default `true`) — the one exception to this project's usual
"off until configured" posture for a new feature (`[team]`/`[council]`/
`[architect]`), deliberately, since "default, system-level" is the whole
point of `aivyx-skills`'s own framing; a fresh install should get real
skill guidance with zero setup. `project_dir: Option<String>`, `user_dir:
Option<String>` — both optional, tilde-resolved via the same
`resolve_tilde_paths` helper `resolved_roster_path()` already uses,
mirroring that exact method's shape (`resolved_project_skills_dir()`/
`resolved_user_skills_dir()`).

**2. `Agent` gains a `skills_text: Option<String>` field**, set via a
new `pub fn set_skills(&mut self, listing: String)` setter (mirroring
`set_repo_map`'s exact convention: optional, post-construction,
constructor unchanged), folded into `system_prompt_text()` as a short,
fixed block — one line per skill, `name — description` — appended after
the existing repo-map/AGENTS.md sections. `agent_builder.rs` computes
this listing once at startup from `SkillLoader::list()` (already
sorted), only when `[skills] enabled = true`; `Agent` itself has no
`SkillLoader` dependency or awareness of the crate at all — it only ever
sees the pre-rendered `String`, keeping `aivyx-core` decoupled from the
specific skill-library crate (matching this project's established
"`aivyx-core` does not depend on `aivyx-config`" boundary discipline,
generalized here to "doesn't depend on `aivyx-skills` either" — the
rendering happens in `agent_builder.rs`, the one place that already
knows how to turn `Settings` into concrete `Agent` state).

**3. A new tool, `load_skill`**, registered in `agent_builder.rs` only
when `[skills] enabled = true`, alongside the `SkillLoader` construction
from Decision 2. Takes one argument, `skill: String`; returns the
matched skill's full `body` as `ToolOutput::Ok`, or a clear
`ToolOutput::Error` naming the requested (unknown) name and listing the
real available names (mirroring `spawn_specialist`'s own "unknown member,
valid specialists: ..." error shape) if no skill matches. `definition()`
interpolates the real, current skill-name list (from the same
`SkillLoader` the tool itself holds), belt-and-braces with the
system-prompt block from Decision 2. `mutates_outside_session()` returns
`false` (a skill load touches no project state — stays visible in Plan
mode, no checkpoint). `permission_request()` uses
`internal_permission_request("load_skill")` (Grounding — `ActionKind::Internal`,
auto-allow, no confirmation prompt).

**4. Dependency**: `crates/aivyx/Cargo.toml` gains `aivyx-skills = { git
= "https://github.com/Aivyx-Agent/aivyx-skills", rev = "<pinned-sha>" }`,
matching `aivyx-confine`/`aivyx-checkpoint`/`aivyx-kvcache`/
`aivyx-injection-guard`'s exact existing declaration shape exactly (a
pinned git rev, not a version range — this project's established
convention for its own small shared crates, which aren't published to
crates.io).

**5. `load_skill`'s tool result is scanned for injection markers when
the matched skill did NOT come from the bundled set.** `Skill.source ==
SkillSource::Bundled` (this crate's own shipped, reviewed content, never
user-influenceable) skips the scan entirely — pure overhead with no
security value, matching how the tool's own fixed prompt text is never
scanned either. `Skill.source == SkillSource::User` or `Project` (both
read from a real, external, potentially-untrusted filesystem location)
runs `scan_for_injection_markers(&skill.body, "skill: <name>")` before
returning the body as a tool result, tagging the agent's shared
`InjectionTaint` on a match — exactly `AGENTS.md`'s own established
two-tier treatment (project/user text scanned, the tool's own fixed
content never is), applied here to a directly analogous threat shape.

## What this spec does not decide

- Any change to `aivyx-skills` itself (the crate's own API, content, or
  format) — consumed exactly as shipped.
- `aivyx-pa`'s own integration (Part 3) — separate, later, in that
  other repo.
- Any TUI/ACP display surface for skills (e.g. a dedicated panel showing
  "available skills," or a keybinding) — out of scope; the system-prompt
  block and the tool's own error messages are the only surfaces.
- Making the skill *body* itself subject to further processing (e.g.
  variable substitution, nested skill references) — a skill's body is
  returned verbatim, exactly as `aivyx-skills::Skill.body` provides it.
- Any change to `aivyx-injection-guard`/`scan_for_injection_markers`
  itself, or to how `AGENTS.md`/repo-map/editor-context sources are
  scanned — Decision 5 only adds a new call site using the existing,
  unmodified mechanism.
