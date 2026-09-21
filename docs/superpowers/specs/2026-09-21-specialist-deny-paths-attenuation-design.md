# Specialist Deny-Paths Attenuation Design

## Context

Nonagon's Phase 2 ("Pool + delegation") final review deliberately deferred
a real gap, tracked since as deferred-gap #2 of 7: a specialist shares the
lead's *exact* `deny_paths` — only tool *visibility* is attenuated
(`compute_specialist_registry` excludes tool names by string match), not
filesystem scope. This spec closes it, chosen as the first of the 6
remaining deferred gaps to close, in a user-approved sequence.

## Grounding

Read directly in the current codebase, not assumed:

- **`aivyx-team`'s Foundation phase already shipped the union logic**:
  `effective_deny_paths(lead_deny_paths: &[String], member: &TeamMember)
  -> Vec<String>` (pure, tested) unions a lead's deny-path strings with
  `TeamMember.extra_deny_paths: Vec<String>` (a field that already exists
  in the schema, `#[serde(default)]`, so every existing roster/config
  stays valid unchanged). It has sat completely unused since Phase 1.
- **The gate is the primary, path-target-only enforcement point**:
  `ConfirmationGate` (`crates/aivyx-sandbox/src/confirmation.rs`) stores
  `deny_paths: Vec<PathBuf>` as a private field set once at
  `ConfirmationGate::new(...)`, checked as the *first* tier of `check()`
  — before even the read-auto-allow branch — for `PermissionTarget::Path`
  and `PermissionTarget::Move{from,to}` requests. Both
  `delegate_to_specialist.rs` and `specialist_sessions.rs` currently pass
  the *exact same* `Arc<dyn PermissionGate>` the lead uses into a
  specialist's own `Agent` (`Arc::clone(&gate)`) — a specialist's
  deny-paths enforcement today is literally the lead's own shared gate
  instance.
- **The gate alone is not sufficient — confirmed by reading `check()`
  itself**: its deny-paths tier only inspects `PermissionTarget::Path`/
  `Move` — a `run_command`/`run_shell` call's `PermissionTarget::Command`
  carries no path at all, so the gate's hard block never sees it. The
  *only* real enforcement against a specialist's shell command reading or
  writing an extra-denied path is `ExecutionConfiner` (Landlock's
  filesystem grants, built once in `agent_builder.rs` via
  `aivyx_sandbox::default_confiner(&cwd, extra_read_paths, &deny_paths,
  require_enforcement)`) — also currently shared, unattenuated, with
  specialists the same way the gate is. Confirmed with the project owner:
  both must be genuinely re-scoped, not just the gate.
- **`DelegateToSpecialistConfig` and `SpecialistSessionsConfig`
  (`aivyx-core`) are nearly identical structs**, both currently holding
  pre-built `gate: Arc<dyn PermissionGate>` and `confiner: Arc<dyn
  ExecutionConfiner>` fields alongside `plan_mode`/`autonomous_mode`/
  `injection_taint`/etc.
- **`agent_builder.rs` already has every raw ingredient needed to build a
  gate/confiner pair, in scope, once**: `prompter`, the lead's `deny_paths:
  Vec<PathBuf>`, `pre_approved_commands: Vec<(String, Vec<String>)>`
  (currently *moved* into the lead's own `ConfirmationGate::new` call —
  needs cloning before that move to also seed a specialist's), `cwd`,
  `settings.editor_approval.enabled`, `settings.sandbox
  .resolved_extra_read_paths()`, `settings.sandbox.require_enforcement`.
- **Type mismatch between the two union functions**: `aivyx-team`'s
  `effective_deny_paths` operates on `&[String]` (raw, unresolved
  config-level path patterns) — but `agent_builder.rs`'s runtime
  `deny_paths` is already a resolved `Vec<PathBuf>` (tilde-expanded,
  symlink-canonicalized via `aivyx-config`'s private `resolve_tilde_paths`,
  called once for the lead at settings-load time). Re-deriving a
  `PathBuf`-native union (extend a clone of the lead's `Vec<PathBuf>` with
  each of the member's own resolved `extra_deny_paths` entries, dedup) is
  simpler and more correct here than round-tripping `PathBuf`s through
  strings just to call the existing string-based function — this spec
  does not reuse `aivyx_team::effective_deny_paths` for that reason.
  `aivyx-core` does not depend on `aivyx-config` (confirmed via
  `Cargo.toml`) and shouldn't gain that dependency just for one small
  resolution helper — a small, self-contained tilde/symlink-resolution
  helper is duplicated into the new module instead, following this
  project's own established precedent for justified small duplications
  across a real architectural boundary (`aivyx-repomap`'s glob-matching
  logic, `aivyx-llm`'s `slot_pool_lock.rs` duplicating `session.rs`'s
  FNV-1a — both accepted, documented choices in this same codebase).

## Decisions

**1. A new module, `crates/aivyx-core/src/specialist_enforcement.rs`**,
owns everything needed to build a specialist-scoped gate/confiner pair:

- `SpecialistEnforcementIngredients` — a small, cheaply-`Clone`-able
  struct bundling the raw pieces `agent_builder.rs` already has: `prompter:
  Arc<dyn PermissionPrompter>`, `base_deny_paths: Vec<PathBuf>` (the
  lead's own, unchanged), `pre_approved_commands: Vec<(String,
  Vec<String>)>` (a clone of the same list seeded into the lead's gate),
  `plan_mode: PlanMode`, `autonomous_mode: AutonomousMode`, `cwd: PathBuf`,
  `editor_approval_enabled: bool`, `injection_taint: InjectionTaint`,
  `extra_read_paths: Vec<PathBuf>`, `require_enforcement: bool`.
- `fn scoped_gate_and_confiner(ingredients: &SpecialistEnforcementIngredients,
  member: &aivyx_team::TeamMember) -> (Arc<dyn PermissionGate>, Arc<dyn
  ExecutionConfiner>)` — resolves `member.extra_deny_paths` (tilde
  expansion + symlink canonicalization, small duplicated helper) into
  `PathBuf`s, unions them with `ingredients.base_deny_paths` (dedup),
  constructs a *fresh* `ConfirmationGate` (mirroring `agent_builder.rs`'s
  own lead-construction call exactly, `.with_injection_taint(...)` reusing
  the *same shared* `injection_taint` instance — see Decision 3) and a
  *fresh* confiner via `aivyx_sandbox::default_confiner(...)` with the
  union'd paths.

**2. `DelegateToSpecialistConfig` and `SpecialistSessionsConfig` each
replace their `gate`/`confiner` fields with one
`enforcement: SpecialistEnforcementIngredients` field.** `agent_builder.rs`
constructs one `SpecialistEnforcementIngredients` value (cloning
`pre_approved_commands` before it's moved into the lead's own gate) and
clones it into both configs — cheap, since every field is an `Arc`, a
small `Vec`, or a `bool`/`PathBuf`. Each tool's `execute()`
(`delegate_to_specialist.rs`'s `DelegateToSpecialistTool::execute`,
`specialist_sessions.rs`'s `SpawnSpecialistTool::execute`) calls
`scoped_gate_and_confiner(&self.config.enforcement, member)` at the point
it currently reads `self.config.gate`/`self.config.confiner`, and passes
the *specialist-scoped* pair into that specialist's own `Agent`
construction instead of the shared lead instances.

**3. `plan_mode`/`autonomous_mode`/`injection_taint` stay the *same
shared* handles as the lead** — not per-specialist copies. These are
legitimately session-wide concerns (a specialist entering "plan mode"
independently of the lead, or having its own private prompt-injection
taint the lead never sees, would be a different and unwanted behavior
change, not part of this fix's scope).

**4. A specialist's Always-Allow cache starts fresh** (a brand-new
`ConfirmationGate` only ever seeds it from `pre_approved_commands` — the
user's config-level trust list, unchanged) rather than inheriting the
lead's session-earned interactive approvals. Confirmed with the project
owner as the intended behavior, not an oversight: the lead's session-earned
approvals were granted in the lead's own trust context and shouldn't
silently transfer to a narrower-scoped specialist.

## What this spec does not decide

- Tool-name visibility attenuation (`compute_specialist_registry`'s
  `exclude()`-by-name logic) — already correct, untouched.
- Reconstructing `GrepTool`/`GlobTool`/`RepoMap` with the specialist's own
  narrower `deny_paths` — confirmed with the project owner as
  deliberately out of scope for this pass: a specialist's search/listing
  results may still reveal matches inside an extra-denied path (a lesser
  leak — file *existence*/matched lines, not content), but any actual
  `read_file`/`write_file`/`edit_file`/`move_file`/`delete_file`/
  `run_command`/`run_shell` targeting that path is genuinely blocked by
  the scoped gate and confiner this spec adds.
- Any change to `aivyx_team::effective_deny_paths` itself, or to
  `TeamMember`'s schema — both already correct and unchanged; this spec
  only wires the union logic's *intent* into the real enforcement path
  using a `PathBuf`-native equivalent (see Grounding).
- Custom-roster loading, `/clear`'s `MissionPlan` reset, session
  persistence, the specialist-to-specialist channel, or ACP entry-prefix
  spoofability — the other 5 deferred gaps, separate scope, later in the
  approved sequence.
