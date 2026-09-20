# Nonagon-Style Team — Phase 5 (Entry Point) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `DelegateToSpecialistTool` (Phase 2, shipped but
unreachable) actually reachable: a new, minimal `TeamSettings.enabled`
config knob, off by default, and conditional registration in
`agent_builder.rs` mirroring `delegate_task`'s own unconditional-once-
registered precedent exactly. No new toggle, keybinding, or slash
command.

**Architecture:** Two tasks. (1) `TeamSettings` in `aivyx-config`,
shaped like `CouncilSettings` (`#[serde(default)]`, off by default) but
with a single `enabled: bool` field — no `configured()` predicate
needed since there's nothing multi-part to check. (2) `agent_builder.rs`
gains `aivyx-team` as a direct dependency and, when
`settings.team.enabled`, builds `DelegateToSpecialistConfig` (mirroring
`DelegateTaskConfig`'s exact construction — same shared collaborators,
`parent_registry` cloned before this tool is registered onto the real
registry, same recursion-prevention shape) using
`aivyx_team::default_coding_roster()`, then registers
`DelegateToSpecialistTool`.

**Tech Stack:** Rust, reusing `aivyx-config`'s existing settings-struct
conventions and `agent_builder.rs`'s existing tool-registration pattern.

## Global Constraints

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` must stay
  clean. `cargo fmt --check` on touched files only — **use file-scoped
  `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024
  <path>` if a fix is ever needed, never a package-scoped `cargo fmt -p
  <crate>` command.** (This exact mistake happened once already in
  Phase 2's own build — a package-scoped fmt run silently reformatted
  five unrelated files carrying this repo's pre-existing drift, and had
  to be reverted. Do not repeat it.)
- `TeamSettings` is deliberately minimal this phase — just `enabled:
  bool`. Do not add a custom-roster-loading mechanism, a `configured()`
  predicate, or any other field — those are explicitly out of this
  phase's scope per the design spec.
- No new runtime toggle, keybinding, or slash command. Once
  `team.enabled = true` in `config.toml`, `delegate_to_specialist` is
  simply always available for the whole session, exactly like
  `delegate_task` already is.
- `DelegateToSpecialistConfig`'s `max_iterations` field reuses
  `settings.sub_agent.max_iterations` — the same existing config knob
  `delegate_task` itself uses (`crates/aivyx-config/src/lib.rs:148-160`,
  `SubAgentSettings`) — do not add a separate, redundant iteration-budget
  setting for team delegation.
- Existing test conventions in `agent_builder.rs`'s own `#[cfg(test)]
  mod tests` (re-read it before Task 2) only cover small, independently
  callable helper functions (`build_llm_backend`, `kv_cache_props_client`)
  — nothing in that file currently unit-tests `build_agent`'s full tool-
  registration flow end to end (it requires a real LLM backend, cwd, and
  much more setup than these smaller helpers). Task 2 follows this same
  boundary: no new automated test asserting "the tool is in the registry
  when enabled" — verified instead via a real build + a manual smoke
  check, matching how the rest of `build_agent`'s ~30 other
  `registry.register(...)` calls are verified today (not individually
  unit-tested).

---

## Task 1: `TeamSettings`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Produces: `pub struct TeamSettings { pub enabled: bool }`
  (`#[derive(Debug, Clone, Serialize, Deserialize)]`, `#[serde(default)]`,
  `Default` impl with `enabled: false`); `Settings` gains
  `pub team: TeamSettings,`.

- [ ] **Step 1: Read the real current `Settings` struct and `CouncilSettings`**

Read `crates/aivyx-config/src/lib.rs` around the `Settings` struct
definition (previously seen at lines ~36-51) and `CouncilSettings`
(previously seen at lines ~405-443) — re-verify these are still the
real current lines/shape before editing, since other work may have
landed since this plan's own research.

- [ ] **Step 2: Write the failing test**

```rust
#[cfg(test)]
mod team_settings_tests {
    use super::*;

    #[test]
    fn team_settings_defaults_to_disabled() {
        assert!(!TeamSettings::default().enabled);
    }

    #[test]
    fn team_settings_round_trips_through_toml_when_enabled() {
        let toml_str = "[team]\nenabled = true\n";
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert!(settings.team.enabled);
    }

    #[test]
    fn team_settings_defaults_when_section_omitted() {
        let settings: Settings = toml::from_str("").unwrap();
        assert!(!settings.team.enabled);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config team_settings_tests
```

Expected: compile error — `TeamSettings`/`Settings.team` don't exist
yet.

- [ ] **Step 4: Implement**

Add, near `CouncilSettings` (same file, matching its placement/style):

```rust
/// `delegate_to_specialist` (Nonagon-style team delegation, ROADMAP.md
/// Phase 5): off until explicitly enabled -- a feature that grants the
/// lead a new tool with real (attenuated, but real) file/command access
/// should not be live out of the box, matching `[council]`/`[architect]`'s
/// own "off until configured" posture. Deliberately minimal this phase:
/// just a switch, no custom-roster config -- when enabled, the lead's
/// specialist team is always `aivyx_team::default_coding_roster()`. See
/// `docs/superpowers/specs/2026-09-20-nonagon-team-entry-point-design.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamSettings {
    pub enabled: bool,
}

impl Default for TeamSettings {
    fn default() -> Self {
        Self { enabled: false }
    }
}
```

Add `pub team: TeamSettings,` to the `Settings` struct, near `sub_agent:
SubAgentSettings` (the closest related existing field — `delegate_task`'s
own config knob).

- [ ] **Step 5: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config team_settings_tests
```

Expected: all 3 pass.

- [ ] **Step 6: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config
cargo clippy -p aivyx-config --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/aivyx-config/src/lib.rs
```

(If the file-scoped fmt check reports drift, confirm before fixing that
it's genuinely from THIS task's own edit, not pre-existing — this file
had no known pre-existing drift as of Phase 1's own build, but
re-verify rather than assume.)

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-config/src/lib.rs
git commit -m "feat: add TeamSettings -- off-by-default switch for Nonagon-style team delegation

Minimal this phase: a single enabled bool, matching [council]/
[architect]'s own 'off until configured' posture. No custom-roster
config yet -- when enabled, the lead's team is always
aivyx_team::default_coding_roster() (Task 2 wires this in)."
```

---

## Task 2: Wire `DelegateToSpecialistTool` into `agent_builder.rs`

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: `TeamSettings` (Task 1), `aivyx_team::default_coding_roster()`,
  `aivyx_core::{DelegateToSpecialistConfig, DelegateToSpecialistTool}`
  (already exported from `aivyx-core`, Phase 2).

- [ ] **Step 1: Add `aivyx-team` as a dependency of the `aivyx` binary crate**

Read `crates/aivyx/Cargo.toml` in full first, and read
`crates/aivyx-core/Cargo.toml`'s own `aivyx-team = { path = "../aivyx-team" }`
line (added in Phase 2) to match its exact style. Add the same line to
`crates/aivyx/Cargo.toml`'s `[dependencies]`.

- [ ] **Step 2: Read the real current `delegate_task` registration block in full**

Read `crates/aivyx/src/agent_builder.rs` around lines 576-642 (the
`sub_agent_registry`/`mcp_registry` clones, `DelegateTaskConfig`
construction, `registry.register(Arc::new(aivyx_core::DelegateTaskTool::new(...)))`)
— re-verify this is still the real current code (line numbers may have
shifted) before writing the new block below it.

- [ ] **Step 3: Add the conditional registration, immediately after `delegate_task`'s own registration**

```rust
// Nonagon-style team delegation (ROADMAP.md Phase 5): off by default,
// see TeamSettings' own doc comment. When enabled, the lead gains
// delegate_to_specialist as an ordinary tool -- no toggle, no
// keybinding, always available for the whole session once configured,
// exactly like delegate_task above. Registered *after* delegate_task
// (not before) so this tool's own name never appears in delegate_task's
// sub_agent_registry snapshot above -- matches that snapshot's own
// "clone before registering the delegation tool itself" recursion-
// prevention shape, applied here too via team_parent_registry below.
if settings.team.enabled {
    // Cloned *before* delegate_to_specialist itself is registered onto
    // `registry`, for the same structural reason sub_agent_registry/
    // mcp_registry are cloned before delegate_task is registered above
    // -- a specialist's own attenuated registry (computed per-call by
    // compute_specialist_registry from this snapshot) must never be
    // able to include delegate_to_specialist itself, or recursive
    // delegation becomes possible.
    let team_parent_registry = registry.clone();
    registry.register(Arc::new(aivyx_core::DelegateToSpecialistTool::new(
        aivyx_core::DelegateToSpecialistConfig {
            llm: Arc::clone(&llm),
            gate: Arc::clone(&gate),
            confiner: Arc::clone(&confiner),
            checkpointer: checkpointer.clone(),
            repo_map: repo_map.clone(),
            events_tx: events_tx.clone(),
            parent_registry: team_parent_registry,
            team: aivyx_team::default_coding_roster(),
            plan_mode: plan_mode.clone(),
            autonomous_mode: autonomous_mode.clone(),
            injection_taint: injection_taint.clone(),
            context_tokens: settings.backend.context_tokens,
            edit_format,
            verification: verification.clone(),
            max_iterations: settings.sub_agent.max_iterations,
            broker_mode,
        },
    )));
}
```

Verify every field name/type against `DelegateToSpecialistConfig`'s real
current definition in `crates/aivyx-core/src/delegate_to_specialist.rs`
(Phase 2) before trusting this snippet verbatim — re-read that struct
directly. Also verify every local variable referenced here (`llm`,
`gate`, `confiner`, `checkpointer`, `repo_map`, `events_tx`, `plan_mode`,
`autonomous_mode`, `injection_taint`, `edit_format`, `verification`,
`broker_mode`) is still in scope at this point in `build_agent` with
these exact names — they were all confirmed present at Phase 2's own
grounding pass, but re-verify directly since this plan's research
predates the actual edit.

- [ ] **Step 4: Verify the workspace builds and existing tests still pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
rustfmt --edition 2024 --check crates/aivyx/src/agent_builder.rs
```

If the fmt check reports drift, determine whether it's from this task's
own new lines or pre-existing (this file's fmt cleanliness was not
independently re-verified since Phase 2's own build, which touched a
different file) — fix only this task's own lines if so, using
file-scoped `rustfmt --edition 2024 crates/aivyx/src/agent_builder.rs`
(never a package-scoped `cargo fmt -p aivyx` command).

- [ ] **Step 5: Manual smoke check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build -p aivyx
```

Then, with a real local LLM backend reachable (if available in this
environment) or by inspecting the built binary's behavior as far as
practical without one:

1. Confirm `--help` still runs cleanly (`./target/debug/aivyx-coder --help`)
   — sanity that the binary still starts.
2. If a real backend is reachable, start the TUI with a temporary config
   directory (`XDG_CONFIG_HOME=<tempdir>`) containing a `config.toml`
   with `[team]\nenabled = true` plus a valid `[backend]` section, and
   confirm the agent doesn't error at startup. Report honestly whether
   you could go further than this (e.g. actually prompting the model to
   use `delegate_to_specialist` and observing a real specialist
   delegation round-trip) — this may not be practical in a sandboxed
   environment with no real model access, and that's an acceptable,
   expected limit to note rather than something to fake.
3. If no real backend is reachable at all, the build succeeding plus
   `--help` running is the practical ceiling for this task's manual
   verification — state this plainly in your report rather than
   claiming untested behavior works.

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx/Cargo.toml crates/aivyx/src/agent_builder.rs
git commit -m "feat: register delegate_to_specialist when [team] enabled

Finally makes Phase 2's DelegateToSpecialistTool reachable. Config-gated
(off by default, TeamSettings.enabled), no runtime toggle -- mirrors
delegate_task's own unconditional-once-registered precedent exactly.
Uses aivyx_team::default_coding_roster() as the team; a custom-roster
config format is explicitly out of this phase's scope. Registered after
delegate_task so its own sub_agent_registry snapshot (taken earlier)
never includes delegate_to_specialist, and delegate_to_specialist's own
parent_registry snapshot (taken here) never includes itself --
recursive delegation stays structurally impossible on both tools."
```

---

## Final verification

- [ ] Run the complete workspace check once more, after both tasks:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: everything clean/passing.

- [ ] Confirm via `grep -n "team.enabled\|DelegateToSpecialist" crates/aivyx/src/agent_builder.rs`
  that the new registration block is genuinely present and gated on
  `settings.team.enabled` — a final, direct sanity check that this
  phase's whole point (making the tool reachable, but only when
  configured) actually landed.

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this spec does not
decide" section.)

- A custom-roster config format — deferred; `TeamSettings.enabled: bool`
  is deliberately the only knob this phase adds.
- Phase 3 (mission structure/DAG) and Phase 4 (message bus) — separate,
  later, unscoped work.
- Phase 6 (TUI missions surface) — separate, later work; no new TUI
  surface this phase.
- Deny-paths attenuation for specialists — still deferred from Phase 2.
