# Custom Roster Loading Design

## Context

Nonagon's Entry Point phase deliberately shipped `[team] enabled = true`
without any way to configure a roster other than
`aivyx_team::default_coding_roster()` — tracked since as deferred-gap #1
of the original 7, chosen second in a user-approved close-out sequence
(deny-paths attenuation, done → custom-roster loading, now → `/clear`
reset → session persistence → specialist channel → ACP spoofability),
specifically because it's what makes the just-shipped `extra_deny_paths`
attenuation feature actually reachable/configurable beyond the one fixed
default roster.

## Grounding

Read directly in the current codebase, not assumed:

- **The schema and validation logic already exist, fully built and
  tested — just never wired to a loader.** `aivyx_team::TeamConfig {
  lead: String, members: Vec<TeamMember> }` and `TeamMember { name, role,
  persona, tool_allowlist, extra_deny_paths }` both already
  `#[derive(Serialize, Deserialize)]`. `TeamConfig::validate(&self,
  available_tools: &[&str]) -> Result<(), TeamConfigError>` (checking
  `UnknownLead`/`DuplicateMember`/`UnknownTool`) already exists — but
  `grep` confirms it's called *only* inside `aivyx-team`'s own unit
  tests, never in production code, not even for the shipped default
  roster.
- **`TeamSettings` (`crates/aivyx-config/src/lib.rs`) has no
  roster-loading field at all** — its own doc comment states plainly:
  "the lead's specialist team is always
  `aivyx_team::default_coding_roster()` — there's still no custom-roster
  config."
- **`agent_builder.rs` hardcodes the default roster** at the exact point
  `DelegateToSpecialistTool`/mission-structure/specialist-session tools
  are registered: `let team = aivyx_team::default_coding_roster();`,
  inside `if settings.team.enabled { .. }`.
- **A directly relevant comment already sits at that exact call site**,
  planted by an earlier phase: `team_parent_registry` (the snapshot a
  specialist's own attenuated tool registry is computed from) is cloned
  *before* `decompose_task`/`verify_output`/`synthesize_results`/
  `spawn_specialist`/`query_specialist`/`close_specialist` are
  registered onto the main `registry` — and the comment explicitly says
  "a phase adding custom rosters will need to revisit where this
  snapshot is taken if specialists should ever be granted them" —
  confirming that snapshot-timing question is a known, already-scoped-out
  boundary this spec does not need to (and should not) touch.
- **Existing tilde-resolution precedent**: `kvcache_store_path:
  Option<String>` + `resolved_kvcache_store_path(&self) -> PathBuf`
  (`aivyx-config`) is the established pattern for an optional,
  tilde-resolvable path field — `roster_path` follows it exactly.
- **`toml` (1.1.2) is already an `aivyx-config` dependency**, but `aivyx`
  (the binary crate, where `agent_builder.rs` lives — the only place that
  simultaneously knows the registered tool-name list *and* needs to parse
  the roster file) does not depend on it yet.
- **Existing fail-closed precedent**: `agent_builder.rs` already
  `anyhow::bail!`s when `--auto` is passed without
  `[verification].command` configured — "refuse to start rather than run
  degraded" is this project's established posture for a misconfiguration
  that would otherwise run silently wrong, not a new pattern this spec
  introduces.

## Decisions

**1. `TeamSettings` gains `pub roster_path: Option<String>`** (default
`None`, unchanged behavior — `default_coding_roster()` — when unset) and
a `resolved_roster_path(&self) -> Option<PathBuf>` method mirroring
`resolved_kvcache_store_path`'s exact tilde-expansion/symlink-
canonicalization pattern, returning `None` when `roster_path` itself is
`None` (no path to resolve).

**2. `agent_builder.rs` resolves the effective `TeamConfig` once**, at
the exact point `let team = aivyx_team::default_coding_roster();`
currently sits:

```rust
let team = match settings.team.resolved_roster_path() {
    Some(path) => {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read [team] roster_path {path:?}: {e}"))?;
        toml::from_str::<aivyx_team::TeamConfig>(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse roster file {path:?}: {e}"))?
    }
    None => aivyx_team::default_coding_roster(),
};
let available_tools: Vec<&str> = registry.definitions().iter().map(|d| d.name.as_str()).collect();
team.validate(&available_tools)
    .map_err(|e| anyhow::anyhow!("[team] roster is invalid: {e}"))?;
```

`available_tools` is derived from `registry` at this exact point — the
same snapshot `team_parent_registry` is cloned from immediately after,
before any mission/specialist-session tool is registered — so a custom
roster's `tool_allowlist` can never validate-pass naming one of those six
tools, matching the existing, deliberate structural exclusion.

**3. Any failure — file unreadable, unparseable TOML, or a real
`TeamConfigError`  — fails closed** via `anyhow::bail!`-equivalent
(`?` on an `anyhow::Result`-returning `build_agent`), refusing to start
rather than silently falling back to the default roster or running with
a broken one.

**4. `validate()` is now called unconditionally**, including for the
default roster (when `roster_path` is unset) — a deliberate, low-risk
defense-in-depth improvement bundled into this same change, since the
validation call path is being added regardless and the default roster is
already known-valid (covered by `aivyx-team`'s own tests) — this closes
a real, previously-latent gap where nothing in production code would
have caught a future accidental break of `default_coding_roster()` until
a delegation call failed confusingly at runtime.

**5. `aivyx` (the binary crate) gains a new direct `toml = "1.1.2"`
dependency**, matching the version already used by `aivyx-config`.

## What this spec does not decide

- Revisiting `team_parent_registry`'s snapshot timing so a custom
  roster's `tool_allowlist` *could* name one of the six mission/
  specialist-session tools — explicitly out of scope, per the
  already-planted comment at that call site; a real, separate,
  deliberately deferred question if ever revisited.
- Any change to `TeamConfig`/`TeamMember`'s schema, or to
  `TeamConfig::validate`'s own logic — both already correct and
  sufficient as-is.
- Hot-reloading a roster file while the process is running — the roster
  is resolved once, at agent-build time, like every other config value.
- A roster-authoring CLI/wizard, or bundling example roster files with
  the project — out of scope; a user writes their own TOML file by hand,
  informed by `TeamConfig`/`TeamMember`'s existing field docs and
  `default_coding_roster()`'s own source as a real, working example.
- Any change to how `extra_deny_paths` (shipped in the deny-paths
  attenuation initiative) or `tool_allowlist` are themselves *enforced* —
  this spec only makes a custom roster *loadable*; both are already
  correctly enforced once a `TeamConfig` (default or custom) reaches the
  existing delegation code.
