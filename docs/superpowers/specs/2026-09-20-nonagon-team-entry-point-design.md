# Nonagon-Style Team — Phase 5 (Entry Point) Design

## Context

Phases 1 (Foundation, `aivyx-team` crate) and 2 (Pool + Delegation,
`DelegateToSpecialistTool`) are both shipped on `main`, but Phase 2's
tool is deliberately unreachable — never registered in `agent_builder.rs`.
This spec covers making it reachable: config-gated registration, no new
runtime toggle, reusing an already-shipped, closely-matching pattern
(`/council`'s "off until configured" `CouncilSettings` shape) rather
than inventing a new one.

**Sequencing note**: the original 6-phase roadmap put this phase
(originally numbered 5) *after* Phase 3 (Mission structure — a real
decompose→delegate→verify→synthesize DAG) and Phase 4 (message bus).
Neither exists yet; this phase is being done out of that original order.
Confirmed with the project owner: this phase does **not** assume or
require Phase 3/4's machinery — the lead simply gains
`delegate_to_specialist` as an ordinary tool it can call via its own
normal reasoning, the same way `delegate_task` already works today, with
no rigid mission structure imposed. Phase 3, whenever it happens, adds
real DAG-based decomposition on top of this same foundation later.

## Grounding

Read directly, not assumed:

- `aivyx-coder/crates/aivyx-core/src/agent/mod.rs:1258-1290` —
  `Agent::run_turn`'s real dispatch chain: `/council`, `/wiki`,
  `/architect` are each intercepted via their own `parse_command`,
  before the input can enter LLM history, each dispatching to a
  dedicated `run_*_turn` method; anything not matching falls through to
  `run_turn_inner` (the normal tool-calling loop). This is the
  established shape for a *new dedicated turn type* — deliberately NOT
  what this phase uses (see Decisions).
- `aivyx-coder/crates/aivyx-config/src/lib.rs:405-443` —
  `CouncilSettings`'s real shape: `#[derive(Serialize, Deserialize)]`,
  `#[serde(default)]`, a `configured()` predicate
  (`self.members.len() >= 2 && self.chairman.is_some()`), off until the
  user explicitly sets it up in `config.toml`. `run_council_turn`
  (`agent/mod.rs:1298-1311`) checks `self.council` (an `Option`, `None`
  until `set_council` is called) and emits a helpful "here's how to
  enable yourself" message when unconfigured, never an error.
- `aivyx-coder/crates/aivyx/src/agent_builder.rs:808` (`agent.set_council(Council {...})`) —
  confirms `agent_builder.rs` is where config-to-runtime wiring for this
  class of feature actually happens: conditionally built only if
  `CouncilSettings::configured()` is true.
- `aivyx-coder/crates/aivyx/src/agent_builder.rs:622-640` (Phase 2's own
  grounding, re-confirmed) — `DelegateTaskTool` is registered
  **unconditionally**, with **no toggle, no config gate, no keybinding**
  — it's simply always available once the binary is built. This is the
  precedent this phase follows for `DelegateToSpecialistTool`, once
  config-enabled — not `plan_mode`'s `Arc<AtomicBool>` runtime-toggle
  shape (`aivyx-sandbox/src/lib.rs:171-186`), which exists to let a
  *human* flip a persistent flag mid-session for a property
  (read-only-ness) every tool call must check — team mode has no
  analogous "flip this on the fly" need once it's config-enabled.
- **Audit trail is already covered by Phase 2's design, no new work
  needed.** A specialist's own tool calls (e.g. `write_file`) run through
  *its own* `ToolExecutor`, constructed with the *same, shared*
  checkpointer as the parent (`DelegateToSpecialistConfig.checkpointer`,
  confirmed in Phase 2's `execute()` — mirrors `delegate_task`'s
  identical pattern). Each specialist tool call checkpoints exactly like
  any other mutating call — the original roadmap's "audit trail via
  existing checkpoints" sub-goal was already satisfied the moment Phase
  2 shipped.
- Confirmed via `grep`: neither `crates/aivyx/Cargo.toml` nor
  `crates/aivyx-core/src/lib.rs`'s public re-exports currently expose
  `aivyx-team` to the `aivyx` binary crate — `agent_builder.rs` needs
  `aivyx-team` added as its own direct dependency to call
  `aivyx_team::default_coding_roster()`.

## Decisions

**1. `TeamSettings` in `config.toml`, shaped like `CouncilSettings`.**
Minimal for this phase — no custom-roster file format yet:

```rust
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

Off by default, matching `CouncilSettings`/`ArchitectSettings`'s
established "off until configured" posture — a feature that grants the
lead a new tool with real (attenuated but real) file/command access
should not be live out of the box.

**2. No `configured()`-style predicate needed** — `enabled` is already a
plain bool, unlike `CouncilSettings`'s multi-field "are enough parts
present" check.

**3. `agent_builder.rs` gains `aivyx-team` as a direct dependency**, and
conditionally builds + registers `DelegateToSpecialistTool` when
`settings.team.enabled`, mirroring `DelegateTaskConfig`'s exact
construction pattern (same shared `llm`/`gate`/`confiner`/`checkpointer`/
`repo_map`/`events_tx`/`plan_mode`/`autonomous_mode`/`injection_taint`/
`context_tokens`/`edit_format`/`verification`/`broker_mode` fields,
`parent_registry` = a clone of the registry *before* this tool itself is
registered onto it, exactly like `delegate_task`'s own recursion
prevention) plus `team: aivyx_team::default_coding_roster()`. When
`enabled` is `false` (default), nothing changes from today's behavior —
`DelegateToSpecialistTool` stays unregistered, exactly as it is right
now.

**4. No runtime toggle, no keybinding, no new slash command.** Once
`team.enabled = true` in `config.toml`, `delegate_to_specialist` is
simply always available to the lead for the whole session — the same
posture `delegate_task` already has. The lead's own ordinary
tool-calling reasoning decides when to use it, the same way it already
decides when to use `delegate_task`. Confirmed with the project owner as
the intended shape (not a `/council`-style dedicated turn-type dispatch,
and not a `plan_mode`-style mid-session flip).

**5. Audit trail: no new work.** Already covered by Phase 2's shared-
checkpointer design (see Grounding) — this phase adds nothing here.

## What this spec does not decide

- A custom-roster config format (loading a `TeamConfig` from a file path
  or inline `[[team.member]]` TOML sections instead of always using
  `default_coding_roster()`) — deferred; `TeamSettings.enabled: bool`
  is deliberately the only knob this phase adds.
- Phase 3 (mission structure/DAG) and Phase 4 (message bus) — separate,
  later, unscoped work, not assumed or required by this phase.
- Phase 6 (TUI missions surface) — separate, later work; this phase adds
  no new TUI surface at all (no status indicator, no keybinding — see
  Decision 4).
- Deny-paths attenuation for specialists — still deferred from Phase 2,
  unchanged by this phase.
