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
- **Two, DIFFERENT existing injection-scanning mechanisms, confirmed by
  reading both, not assumed to be the same thing**:
  1. `Agent::record_tool_result` (`crates/aivyx-core/src/agent/mod.rs:1144-1156`)
     scans EVERY dispatched tool's `ToolOutput::Ok` content generically,
     unconditionally, for ALL tools — confirmed via every call site
     (`agent/mod.rs:1224,1293,2188`, the shared tail of the main
     per-turn dispatch loop and `run_auto_verification`). No tool in
     `aivyx-tools` does its own scanning (confirmed: `grep -rl
     scan_for_injection_markers crates/aivyx-tools/src/tools/` returns
     nothing) — because none of them need to; the framework already
     does it for every tool result on their behalf. **This means
     `load_skill`'s own returned body needs NO bespoke scanning code at
     all** — it's a normal `ToolOutput::Ok`, automatically covered.
  2. Content injected DIRECTLY into the system prompt (bypassing tool
     dispatch entirely) is NOT covered by mechanism 1, and needs its own
     explicit scan call at the point it's read — confirmed by reading
     `AGENTS.md`'s own two call sites
     (`agent/mod.rs:850-854,867-870`, inside `refresh_agents_files`,
     which builds `agents_files_text` directly, never going through
     `record_tool_result`), each scanning that one source's content and
     tagging `self.injection_taint` on a match. The skill LISTING
     (Decision 2's `skills_text`) follows this exact same path — folded
     directly into `system_prompt_text()`, never a tool result — so an
     overlay-sourced skill's `description` field appearing in that
     listing needs the SAME explicit-scan treatment `AGENTS.md` gets,
     for the identical reason.
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

**5. The skill LISTING (`skills_text`, Decision 2) is scanned for
injection markers at the point `agent_builder.rs` builds it — `load_skill`'s
own tool result needs no bespoke scanning code at all.** `load_skill`
returns a normal `ToolOutput::Ok`, which `Agent::record_tool_result`
already scans unconditionally for every tool (Grounding); adding a second,
tool-specific scan there would be pure redundant overhead with no security
value. `skills_text`, by contrast, is folded directly into
`system_prompt_text()` and never passes through `record_tool_result` —
exactly `agents_files_text`'s own situation. So `agent_builder.rs`, when
rendering the listing from `SkillLoader::list()`, runs
`scan_for_injection_markers(&summary.description, "skill listing: <name>")`
on each overlay-sourced (`SkillSource::User` or `Project`) entry's
`description` before appending it to the listing, tagging the agent's
shared `InjectionTaint` on a match — matching `AGENTS.md`'s own established
two-tier treatment (project/user text scanned, bundled/fixed content never
is) applied to the identical "system-prompt-injected, bypasses
`record_tool_result`" threat shape. Bundled entries (`SkillSource::Bundled`)
skip the scan entirely, same rationale as `AGENTS.md`'s own fixed text.

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
  scanned — Decision 5 only adds one new call site, in `agent_builder.rs`'s
  skills-listing construction, using the existing, unmodified mechanism.
- Any bespoke injection-scanning logic inside the `load_skill` tool itself
  — its tool result is already covered by the generic, unconditional
  `record_tool_result` scan every tool gets (Grounding), so adding a
  second scan there would be redundant, not additive.
