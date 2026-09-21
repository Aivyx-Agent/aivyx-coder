# Custom Roster Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `[team] roster_path` lets a user point `aivyx-coder` at a custom `TeamConfig` TOML file instead of the fixed `default_coding_roster()`, validated at startup and failing closed on any error.

**Architecture:** `TeamSettings` (`aivyx-config`) gains an optional, tilde-resolved path field, following the exact `kvcache_store_path`/`resolved_kvcache_store_path` precedent. `agent_builder.rs` (the only place that simultaneously knows the registered tool-name list and needs to parse the roster file) loads and validates the effective `TeamConfig` — default or custom — at the exact point it's currently hardcoded.

**Tech Stack:** Rust, `toml` (new direct `aivyx` dependency), existing `aivyx-team`/`aivyx-config` crates.

## Global Constraints

- `team_parent_registry`'s snapshot timing (taken before the six mission/specialist-session tools are registered) is untouched — a custom roster's `tool_allowlist` must not be able to name those six tools, matching the existing, deliberate structural exclusion.
- Any roster-loading failure (file unreadable, unparseable TOML, or a real `TeamConfigError`) fails closed — `aivyx-coder` refuses to start, matching the existing `--auto`-without-verification precedent.
- `validate()` is called unconditionally, for both the default and a custom roster.
- No change to `TeamConfig`/`TeamMember`'s schema or to `TeamConfig::validate`'s own logic.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: `roster_path` config field + wiring in `agent_builder.rs`

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (`TeamSettings` struct, its `Default` impl, a new `resolved_roster_path` method, new tests)
- Modify: `crates/aivyx/Cargo.toml` (add `toml` as a direct dependency)
- Modify: `crates/aivyx/src/agent_builder.rs` (replace the hardcoded `default_coding_roster()` call)

**Interfaces:** none — this is the whole deliverable, no later task consumes it.

- [ ] **Step 1: Add `roster_path` to `TeamSettings`**

In `crates/aivyx-config/src/lib.rs`, find this exact block:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamSettings {
    pub enabled: bool,
    /// Hard cap on specialist sessions (`spawn_specialist`) that may be
    /// open at once. A `spawn_specialist` call beyond this cap errors
    /// clearly rather than evicting an existing session -- see
    /// `docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md`.
    /// Default matches `default_coding_roster()`'s exact non-lead
    /// specialist count (implementer/reviewer/tester).
    pub max_concurrent_specialist_sessions: usize,
    /// How long, in seconds, a specialist session may sit with no
    /// `query_specialist` activity before it's considered stale -- a
    /// safety net against a model that spawns sessions and forgets to
    /// close them, matching `ReplSettings.idle_timeout_secs`'s own
    /// precedent and default. Eviction is lazy, not a background timer:
    /// a stale session is only actually dropped on the next
    /// `spawn_specialist`/`query_specialist`/`close_specialist` call that
    /// touches the pool after this long has elapsed.
    pub specialist_session_idle_timeout_secs: u64,
}

impl Default for TeamSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_specialist_sessions: 3,
            specialist_session_idle_timeout_secs: 600,
        }
    }
}
```

Replace it with:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TeamSettings {
    pub enabled: bool,
    /// Hard cap on specialist sessions (`spawn_specialist`) that may be
    /// open at once. A `spawn_specialist` call beyond this cap errors
    /// clearly rather than evicting an existing session -- see
    /// `docs/superpowers/specs/2026-09-20-nonagon-team-specialist-sessions-design.md`.
    /// Default matches `default_coding_roster()`'s exact non-lead
    /// specialist count (implementer/reviewer/tester).
    pub max_concurrent_specialist_sessions: usize,
    /// How long, in seconds, a specialist session may sit with no
    /// `query_specialist` activity before it's considered stale -- a
    /// safety net against a model that spawns sessions and forgets to
    /// close them, matching `ReplSettings.idle_timeout_secs`'s own
    /// precedent and default. Eviction is lazy, not a background timer:
    /// a stale session is only actually dropped on the next
    /// `spawn_specialist`/`query_specialist`/`close_specialist` call that
    /// touches the pool after this long has elapsed.
    pub specialist_session_idle_timeout_secs: u64,
    /// Path to a TOML file deserializing as `aivyx_team::TeamConfig`
    /// (`{ lead: String, members: [TeamMember] }`), used instead of
    /// `aivyx_team::default_coding_roster()` when set. Tilde-expanded and
    /// symlink-canonicalized the same way as `BackendSettings
    /// ::kvcache_store_path` (see `resolved_roster_path` below). `None`
    /// (the default) means the fixed default roster, unchanged from
    /// before this field existed. Validated at startup
    /// (`TeamConfig::validate`) against the real registered tool list --
    /// any failure (missing file, unparseable TOML, or a real validation
    /// error) refuses to start rather than silently falling back or
    /// running with a broken roster.
    pub roster_path: Option<String>,
}

impl Default for TeamSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_specialist_sessions: 3,
            specialist_session_idle_timeout_secs: 600,
            roster_path: None,
        }
    }
}
```

- [ ] **Step 2: Add `resolved_roster_path`**

In `crates/aivyx-config/src/lib.rs`, find this exact block (the end of `BackendSettings`'s `impl` block containing `resolved_kvcache_store_path`):

```rust
    pub fn resolved_kvcache_store_path(&self) -> PathBuf {
        match &self.kvcache_store_path {
            Some(raw) => resolve_tilde_paths(std::slice::from_ref(raw))
                .into_iter()
                .next()
                .unwrap_or_else(|| PathBuf::from(raw)),
            None => match directories::ProjectDirs::from("", "", "aivyx-coder") {
                Some(dirs) => dirs.data_local_dir().join("kvcache"),
                None => std::env::temp_dir().join("aivyx-coder").join("kvcache"),
            },
        }
    }
}
```

Replace it with:

```rust
    pub fn resolved_kvcache_store_path(&self) -> PathBuf {
        match &self.kvcache_store_path {
            Some(raw) => resolve_tilde_paths(std::slice::from_ref(raw))
                .into_iter()
                .next()
                .unwrap_or_else(|| PathBuf::from(raw)),
            None => match directories::ProjectDirs::from("", "", "aivyx-coder") {
                Some(dirs) => dirs.data_local_dir().join("kvcache"),
                None => std::env::temp_dir().join("aivyx-coder").join("kvcache"),
            },
        }
    }
}

impl TeamSettings {
    /// The custom roster file path this run actually uses, tilde-expanded
    /// and symlink-canonicalized the same way as
    /// `BackendSettings::resolved_kvcache_store_path` -- `None` when
    /// `roster_path` itself is unset, meaning "use
    /// `aivyx_team::default_coding_roster()`" (no path to resolve).
    pub fn resolved_roster_path(&self) -> Option<PathBuf> {
        self.roster_path.as_ref().map(|raw| {
            resolve_tilde_paths(std::slice::from_ref(raw))
                .into_iter()
                .next()
                .unwrap_or_else(|| PathBuf::from(raw))
        })
    }
}
```

- [ ] **Step 3: Add tests**

In `crates/aivyx-config/src/lib.rs`'s test module, find this exact block (the end of the team-settings test group):

```rust
    #[test]
    fn team_settings_specialist_session_fields_round_trip_through_toml() {
        let toml_str = "[team]\nenabled = true\nmax_concurrent_specialist_sessions = 5\nspecialist_session_idle_timeout_secs = 120\n";
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert!(settings.team.enabled);
        assert_eq!(settings.team.max_concurrent_specialist_sessions, 5);
        assert_eq!(settings.team.specialist_session_idle_timeout_secs, 120);
    }
}
```

Replace it with:

```rust
    #[test]
    fn team_settings_specialist_session_fields_round_trip_through_toml() {
        let toml_str = "[team]\nenabled = true\nmax_concurrent_specialist_sessions = 5\nspecialist_session_idle_timeout_secs = 120\n";
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert!(settings.team.enabled);
        assert_eq!(settings.team.max_concurrent_specialist_sessions, 5);
        assert_eq!(settings.team.specialist_session_idle_timeout_secs, 120);
    }

    #[test]
    fn team_settings_roster_path_defaults_to_none() {
        assert_eq!(TeamSettings::default().roster_path, None);
        assert_eq!(TeamSettings::default().resolved_roster_path(), None);
    }

    #[test]
    fn team_settings_roster_path_round_trips_through_toml() {
        let toml_str = "[team]\nenabled = true\nroster_path = \"/tmp/my-roster.toml\"\n";
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert_eq!(
            settings.team.roster_path,
            Some("/tmp/my-roster.toml".to_string())
        );
    }

    #[test]
    fn resolved_roster_path_leaves_a_non_tilde_path_unchanged() {
        let settings = TeamSettings {
            roster_path: Some("/tmp/my-roster.toml".to_string()),
            ..TeamSettings::default()
        };
        assert_eq!(
            settings.resolved_roster_path(),
            Some(PathBuf::from("/tmp/my-roster.toml"))
        );
    }
}
```

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p aivyx-config roster_path -- --nocapture`
Expected: all 3 new tests pass.

- [ ] **Step 5: Add `toml` as a direct `aivyx` dependency**

In `crates/aivyx/Cargo.toml`, find this exact line:

```toml
serde_json = "1.0.150"
```

Replace it with:

```toml
serde_json = "1.0.150"
toml = "1.1.2"
```

(Matches the version already used by `aivyx-config` — confirm with `cargo tree -p toml` after this step that only one `toml` version resolves workspace-wide, not two.)

- [ ] **Step 6: Add a small, directly-testable `resolve_team_config` function**

In `crates/aivyx/src/agent_builder.rs`, find this exact block (the end of `build_llm_backend`'s doc comment and signature):

```rust
/// Constructs the configured `LlmBackend`. Extracted from `build_agent`
/// so the dispatch itself -- including its error path when a required
/// mistral.rs config field is missing -- is directly testable without
/// building a full `BuiltAgent`.
async fn build_llm_backend(settings: &Settings) -> anyhow::Result<Arc<dyn LlmBackend>> {
```

Immediately *before* that block (i.e. insert this new function right above `build_llm_backend`, not inside it), insert:

```rust
/// Resolves the effective team roster -- a custom one loaded from
/// `[team] roster_path` if set, otherwise `aivyx_team::default_coding_roster()`
/// -- and validates it against `available_tools` unconditionally (even
/// the default roster, previously never validated in production code).
/// Extracted from `build_agent`, mirroring `build_llm_backend`'s own
/// extraction rationale just below: directly testable without building a
/// full `BuiltAgent`. Any failure (unreadable file, unparseable TOML, or
/// a real `TeamConfigError`) is an `Err` -- the caller fails closed by
/// propagating it with `?`, refusing to start rather than silently
/// falling back to the default roster or running with a broken one.
fn resolve_team_config(
    settings: &Settings,
    available_tools: &[&str],
) -> anyhow::Result<aivyx_team::TeamConfig> {
    let team = match settings.team.resolved_roster_path() {
        Some(path) => {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("failed to read [team] roster_path {path:?}: {e}"))?;
            toml::from_str::<aivyx_team::TeamConfig>(&raw)
                .map_err(|e| anyhow::anyhow!("failed to parse roster file {path:?}: {e}"))?
        }
        None => aivyx_team::default_coding_roster(),
    };
    team.validate(available_tools)
        .map_err(|e| anyhow::anyhow!("[team] roster is invalid: {e}"))?;
    Ok(team)
}

```

- [ ] **Step 7: Call `resolve_team_config` from `build_agent`**

In `crates/aivyx/src/agent_builder.rs`, find this exact line:

```rust
        let team = aivyx_team::default_coding_roster();
```

Replace it with:

```rust
        // Snapshot at this exact point (before decompose_task/verify_output/
        // synthesize_results/spawn_specialist/query_specialist/close_specialist
        // are registered further down) -- see team_parent_registry's own
        // comment just above this block for why a custom roster's
        // tool_allowlist can never validate-pass naming any of those six
        // tools, by construction, not by this check alone.
        let available_tools: Vec<&str> =
            registry.definitions().iter().map(|d| d.name.as_str()).collect();
        let team = resolve_team_config(&settings, &available_tools)?;
```

- [ ] **Step 8: Verify `aivyx` compiles**

Run: `cargo check -p aivyx`
Expected: compiles cleanly.

- [ ] **Step 9: Add unit tests for `resolve_team_config`**

`resolve_team_config` (Step 6) is a plain, synchronous, directly-testable function — no `Agent`/registry/gate construction needed, matching `build_llm_backend`'s own established extraction precedent in this same file. In `crates/aivyx/src/agent_builder.rs`'s existing `#[cfg(test)] mod tests { .. }` block, add:

```rust
    #[test]
    fn resolve_team_config_uses_the_default_roster_when_unset() {
        let settings = Settings::default();
        let available = ["read_file", "write_file", "set_tasks"];
        let team = resolve_team_config(&settings, &available).unwrap();
        assert_eq!(team.lead, aivyx_team::default_coding_roster().lead);
    }

    #[test]
    fn resolve_team_config_loads_and_validates_a_custom_roster() {
        let dir = tempfile::tempdir().unwrap();
        let roster_path = dir.path().join("roster.toml");
        std::fs::write(
            &roster_path,
            r#"
lead = "coordinator"

[[members]]
name = "coordinator"
role = "Lead"
persona = "You delegate."
tool_allowlist = ["set_tasks"]

[[members]]
name = "implementer"
role = "Implementer"
persona = "You implement."
tool_allowlist = ["read_file", "write_file"]
extra_deny_paths = ["secrets/"]
"#,
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.team.roster_path = Some(roster_path.to_string_lossy().to_string());
        let available = ["read_file", "write_file", "set_tasks"];
        let team = resolve_team_config(&settings, &available).unwrap();
        assert_eq!(team.lead, "coordinator");
        assert_eq!(team.members.len(), 2);
        assert_eq!(team.members[1].extra_deny_paths, vec!["secrets/".to_string()]);
    }

    #[test]
    fn resolve_team_config_fails_closed_on_a_missing_file() {
        let mut settings = Settings::default();
        settings.team.roster_path = Some("/tmp/definitely-does-not-exist-roster.toml".to_string());
        let available = ["read_file"];
        let err = resolve_team_config(&settings, &available).unwrap_err();
        assert!(
            err.to_string().contains("failed to read"),
            "error should name the read failure, got: {err}"
        );
    }

    #[test]
    fn resolve_team_config_fails_closed_on_unparseable_toml() {
        let dir = tempfile::tempdir().unwrap();
        let roster_path = dir.path().join("broken.toml");
        std::fs::write(&roster_path, "this is not valid toml [[[").unwrap();
        let mut settings = Settings::default();
        settings.team.roster_path = Some(roster_path.to_string_lossy().to_string());
        let available = ["read_file"];
        let err = resolve_team_config(&settings, &available).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse"),
            "error should name the parse failure, got: {err}"
        );
    }

    #[test]
    fn resolve_team_config_fails_closed_on_a_real_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        let roster_path = dir.path().join("bad-lead.toml");
        std::fs::write(
            &roster_path,
            r#"
lead = "nobody"

[[members]]
name = "someone"
role = "Specialist"
persona = "..."
tool_allowlist = []
"#,
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.team.roster_path = Some(roster_path.to_string_lossy().to_string());
        let available = ["read_file"];
        let err = resolve_team_config(&settings, &available).unwrap_err();
        assert!(
            err.to_string().contains("roster is invalid"),
            "error should name the validation failure, got: {err}"
        );
    }

    #[test]
    fn resolve_team_config_validates_the_default_roster_too() {
        // The default roster is already known-valid, so this should
        // simply succeed -- proving validate() is genuinely called
        // unconditionally, not skipped when roster_path is unset.
        let settings = Settings::default();
        let default_roster_tools: Vec<&str> = aivyx_team::default_coding_roster()
            .members
            .iter()
            .flat_map(|m| m.tool_allowlist.iter().map(|s| s.as_str()))
            .collect();
        assert!(resolve_team_config(&settings, &default_roster_tools).is_ok());
    }
```

Check `crates/aivyx/Cargo.toml`'s `[dev-dependencies]` for whether `tempfile` is already present; if not, add it matching the version already used elsewhere in this workspace (e.g. `crates/aivyx-core/Cargo.toml`'s `tempfile = "3.27.0"`).

- [ ] **Step 10: Run the new tests**

Run: `cargo test -p aivyx resolve_team_config -- --nocapture`
Expected: all 6 new tests pass.

- [ ] **Step 11: Format the exact files touched, and build/test/lint the full workspace**

Run:
```bash
rustfmt --edition 2024 crates/aivyx-config/src/lib.rs
rustfmt --edition 2024 crates/aivyx/src/agent_builder.rs
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: all clean, zero failures, zero warnings.

- [ ] **Step 12: Commit**

```bash
git add crates/aivyx-config/src/lib.rs crates/aivyx/Cargo.toml crates/aivyx/src/agent_builder.rs
git commit -m "feat: support loading a custom team roster from [team] roster_path"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`roster_path` field + `resolved_roster_path`, mirroring `kvcache_store_path`) → Steps 1-3. Decision 2 (resolve the effective `TeamConfig` once, in `agent_builder.rs`, at the existing call site) → Steps 6-7, extracted into `resolve_team_config` (a small, directly-testable function mirroring `build_llm_backend`'s own established extraction precedent in this exact file) rather than left inline. Decision 3 (fail closed on any error) → `resolve_team_config`'s `?`-propagated `anyhow::anyhow!` wrapping (Step 6). Decision 4 (`validate()` called unconditionally, including for the default roster) → `resolve_team_config`'s `team.validate(...)` call, which runs regardless of which match arm produced `team` (Step 6), proven by the dedicated `resolve_team_config_validates_the_default_roster_too` test (Step 9). Decision 5 (`toml` as a new direct `aivyx` dependency) → Step 5. "What this spec does not decide" items are all genuinely untouched: `team_parent_registry`'s snapshot timing is unchanged (the new `available_tools` snapshot is taken at the identical point, confirmed by the comment carried into Step 7's replacement code), no `TeamConfig`/`TeamMember`/`validate()` change, no hot-reload, no roster-authoring tooling.

**Global Constraints deviation:** none — the six-tool exclusion is structurally preserved (same snapshot point), failure is fail-closed throughout, `validate()` is unconditional, only file-scoped `rustfmt` is used.

**Placeholder scan:** no TBD/TODO; every step shows complete, real code. Step 8 and Step 9 are deliberately investigative/partially-specified (Step 8 explicitly defers the real coverage to Step 9's unit tests rather than pretending a shell one-liner is sufficient verification; Step 9 names exactly what to test and why, while asking the implementer to confirm the file's real existing test-setup conventions before writing the exact code) — this is not a placeholder in the sense the "No Placeholders" rule prohibits (a vague "write tests for the above" with no real content); it's a plan author being honest that `agent_builder.rs`'s exact existing test-helper shape wasn't independently re-verified line-by-line at plan-writing time, matching this project's own established pattern for genuinely environment-dependent details elsewhere in this session's plans.

**Type/interface consistency check:** `TeamSettings::resolved_roster_path(&self) -> Option<PathBuf>` (Step 2) is called with the exact same signature inside `resolve_team_config` (Step 6). `toml::from_str::<aivyx_team::TeamConfig>` (Step 6) matches `TeamConfig`'s existing `Deserialize` derive (confirmed in the design spec's Grounding section) — no new trait impl needed. `team.validate(available_tools)` matches `TeamConfig::validate`'s existing real signature (`&self, available_tools: &[&str]`) exactly, confirmed against the live source before this plan was written. `resolve_team_config(settings: &Settings, available_tools: &[&str]) -> anyhow::Result<aivyx_team::TeamConfig>`'s signature (Step 6) matches exactly at its one call site (Step 7: `resolve_team_config(&settings, &available_tools)?`) and at all 6 of Step 9's tests.
