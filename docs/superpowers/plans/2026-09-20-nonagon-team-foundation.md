# Nonagon-Style Team — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `aivyx-coder` a `TeamConfig`/`TeamMember` schema and the
attenuation invariant ("a specialist can never exceed its lead," ported
from `aivyx-pa`'s Nonagon onto `aivyx-coder`'s own `deny_paths`/
`tool_allowlist` primitives) — schema-valid, fully tested, but with no
delegation tooling wired up yet (that's Phase 2, a separate future plan).

**Architecture:** A new, dependency-light `aivyx-team` crate (matching
the naming `aivyx-pa` already established for the same concept). Four
pieces: (1) the `TeamConfig`/`TeamMember` schema types, serde-derived,
loadable from TOML; (2) load-time validation (unknown lead, unknown tool
name, duplicate member names all rejected before any delegation could
ever happen); (3) two pure attenuation functions — a deny-paths union
and a tool-allowlist subset check — that Phase 2's real delegation code
will call, tested here against fixture data since there's no live tool
registry to integrate against yet; (4) a default, coding-shaped 4-role
roster shipped as ready-to-use config data.

**Tech Stack:** Rust, `serde`/`toml` (already workspace dependencies
elsewhere — reuse the same versions, don't introduce new ones).

## Global Constraints

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` must stay
  clean. `cargo fmt --check` on files this plan touches only (there is
  known, pre-existing, out-of-scope rustfmt drift elsewhere in this
  repo — do not fix it as a side effect of this plan).
- **`aivyx-team` stays dependency-light, deliberately decoupled from
  `aivyx-tools`/`aivyx-core`/`aivyx-sandbox` at this phase** — matching
  `aivyx-repomap`'s existing "deliberately zero-dependency on any other
  workspace crate" convention in this codebase. Validation and
  attenuation functions take caller-supplied `&[String]` (available tool
  names) and `&[PathBuf]` (the lead's `deny_paths`) as plain parameters,
  never importing a live `ToolRegistry`/`ConfirmationGate` type. Phase 2
  is where this crate gets wired into the real tool-dispatch path — do
  not do that wiring now, even if it looks convenient.
- The attenuation invariant is exactly: a specialist's `tool_allowlist`
  must be a subset of the caller-supplied available-tool-names list; a
  specialist's *effective* `deny_paths` is the union of the lead's
  `deny_paths` and the member's own `extra_deny_paths` (a specialist can
  only be handed more restriction, never less).
- Validation happens at `TeamConfig` load/construction time, not at any
  later "delegation time" (there is no delegation time yet in this
  phase) — an invalid config must be rejected the moment it's parsed.
- The default roster is `coordinator` (lead) → `implementer` →
  `reviewer` → `tester`, using real, currently-registered `aivyx-coder`
  tool names (verified directly against `crates/aivyx-tools/src/tools/*.rs`'s
  own `Tool::name()` implementations at plan-writing time — re-verify
  these are still the real names before Task 4, the tool registry may
  have changed): `delete_file`, `edit_file`, `find_references`,
  `generate_svg`, `generate_image`, `generate_3d`, `git_branch`,
  `git_commit`, `git_pr`, `git_push`, `git_read`, `glob`,
  `go_to_definition`, `grep`, `memory_forget`, `memory_read`,
  `memory_write`, `move_file`, `patch_file`, `read_file`,
  `remember_preference`, `repl_start`, `repl_send`, `repl_stop`,
  `run_command`, `run_shell`, `set_tasks`, `web_fetch`, `web_search`,
  `write_file` (plus a few MCP-bridge/search tools not needed for this
  roster).

---

## Task 1: `aivyx-team` crate skeleton + schema types

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-team/Cargo.toml`
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-team/src/lib.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/Cargo.toml` (add the new crate to `[workspace.members]`)

**Interfaces:**
- Produces: `pub struct TeamConfig { pub lead: String, pub members: Vec<TeamMember> }`,
  `pub struct TeamMember { pub name: String, pub role: String, pub persona: String, pub tool_allowlist: Vec<String>, pub extra_deny_paths: Vec<String> }`,
  both `#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]`,
  loadable via `toml::from_str::<TeamConfig>(...)`.

- [ ] **Step 1: Read an existing small crate's `Cargo.toml` for conventions**

Read `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-repomap/Cargo.toml`
(the workspace's other deliberately-standalone crate) to match this
workspace's real current `[package]` field style (`version.workspace = true`,
`edition.workspace = true`, `license.workspace = true`, etc.) and its
`serde`/`toml` dependency version pins — copy the exact versions already
used elsewhere in this workspace (check `crates/aivyx-config/Cargo.toml`
for its `serde`/`toml` versions too, since `aivyx-config` already parses
TOML config — match those exact version strings, don't guess new ones).

- [ ] **Step 2: Create `crates/aivyx-team/Cargo.toml`**

```toml
[package]
name = "aivyx-team"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde = { version = "1.0.228", features = ["derive"] }
toml = "0.9"

[dev-dependencies]
```

(Verify the exact `serde`/`toml` version strings against
`crates/aivyx-config/Cargo.toml`'s real current pins before finalizing —
the versions above are from this plan's own research and may have
drifted; match whatever `aivyx-config` actually uses today so the
workspace doesn't end up with two different pinned versions of the same
crate.)

- [ ] **Step 3: Add the crate to the workspace**

Read the root `/home/julian/Projects/Rust/aivyx-coder/Cargo.toml`'s
`[workspace.members]` list and add `"crates/aivyx-team"` to it, matching
the existing list's ordering/style.

- [ ] **Step 4: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_config_round_trips_through_toml() {
        let toml_str = r#"
            lead = "coordinator"

            [[members]]
            name = "coordinator"
            role = "Lead"
            persona = "You delegate, verify, and synthesize; you never execute tasks directly."
            tool_allowlist = ["set_tasks"]
            extra_deny_paths = []

            [[members]]
            name = "implementer"
            role = "Implementer"
            persona = "You write and edit code."
            tool_allowlist = ["read_file", "write_file", "edit_file", "grep", "glob", "run_command"]
            extra_deny_paths = ["secrets/"]
        "#;
        let config: TeamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.lead, "coordinator");
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.members[1].name, "implementer");
        assert_eq!(config.members[1].extra_deny_paths, vec!["secrets/".to_string()]);
    }

    #[test]
    fn team_member_defaults_extra_deny_paths_to_empty_when_omitted() {
        let toml_str = r#"
            lead = "solo"

            [[members]]
            name = "solo"
            role = "Lead"
            persona = "You work alone."
            tool_allowlist = []
        "#;
        let config: TeamConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.members[0].extra_deny_paths, Vec::<String>::new());
    }
}
```

- [ ] **Step 5: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
```

Expected: compile error — the crate/types don't exist yet.

- [ ] **Step 6: Implement**

```rust
//! `aivyx-team`: the Nonagon-style team schema and attenuation
//! invariant for `aivyx-coder`. Deliberately dependency-light --
//! see this crate's own `Cargo.toml` and the parent plan's Global
//! Constraints -- no dependency on `aivyx-tools`/`aivyx-core`/
//! `aivyx-sandbox`. Validation and attenuation take caller-supplied
//! data (available tool names, the lead's deny_paths) as plain
//! parameters; wiring this into a real tool registry and a real
//! delegation path is later, separate scope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub lead: String,
    pub members: Vec<TeamMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub name: String,
    pub role: String,
    pub persona: String,
    pub tool_allowlist: Vec<String>,
    #[serde(default)]
    pub extra_deny_paths: Vec<String>,
}
```

- [ ] **Step 7: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
```

Expected: both new tests pass.

- [ ] **Step 8: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test -p aivyx-team
cargo clippy -p aivyx-team --all-targets -- -D warnings
cargo fmt --check -p aivyx-team
```

- [ ] **Step 9: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add Cargo.toml crates/aivyx-team/Cargo.toml crates/aivyx-team/src/lib.rs
git commit -m "feat: add the aivyx-team crate skeleton and TeamConfig/TeamMember schema

Deliberately dependency-light (no aivyx-tools/aivyx-core/aivyx-sandbox
dependency), matching aivyx-repomap's existing standalone-crate
convention -- this phase ships schema + validation + attenuation
functions only, no delegation tooling yet (Phase 2, separate future
plan). TOML-loadable via serde/toml, matching the same versions
aivyx-config already pins."
```

---

## Task 2: Load-time validation

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-team/src/lib.rs`

**Interfaces:**
- Consumes: `TeamConfig`/`TeamMember` from Task 1.
- Produces: `pub fn validate(&self, available_tools: &[&str]) -> Result<(), TeamConfigError>`
  on `TeamConfig`; `pub enum TeamConfigError { UnknownLead(String), DuplicateMember(String), UnknownTool { member: String, tool: String } }`
  (`#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]`, with
  `#[error(...)]` messages on each variant).

- [ ] **Step 1: Add `thiserror` as a dependency**

Add `thiserror = "2"` to `crates/aivyx-team/Cargo.toml`'s
`[dependencies]` (verify the exact major version already used elsewhere
in this workspace — check `crates/aivyx-config/Cargo.toml` or
`crates/aivyx-llm/Cargo.toml` for the real current pin — match it, don't
guess a version that might conflict with the workspace's existing
resolved version).

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod validation_tests {
    use super::*;

    fn valid_config() -> TeamConfig {
        TeamConfig {
            lead: "coordinator".to_string(),
            members: vec![
                TeamMember {
                    name: "coordinator".to_string(),
                    role: "Lead".to_string(),
                    persona: "You delegate.".to_string(),
                    tool_allowlist: vec!["set_tasks".to_string()],
                    extra_deny_paths: vec![],
                },
                TeamMember {
                    name: "implementer".to_string(),
                    role: "Implementer".to_string(),
                    persona: "You write code.".to_string(),
                    tool_allowlist: vec!["read_file".to_string(), "write_file".to_string()],
                    extra_deny_paths: vec![],
                },
            ],
        }
    }

    #[test]
    fn valid_config_passes() {
        let config = valid_config();
        let available = ["set_tasks", "read_file", "write_file", "grep"];
        assert!(config.validate(&available).is_ok());
    }

    #[test]
    fn rejects_lead_not_present_among_members() {
        let mut config = valid_config();
        config.lead = "nobody".to_string();
        let available = ["set_tasks", "read_file", "write_file"];
        let err = config.validate(&available).unwrap_err();
        assert_eq!(err, TeamConfigError::UnknownLead("nobody".to_string()));
    }

    #[test]
    fn rejects_duplicate_member_names() {
        let mut config = valid_config();
        config.members.push(config.members[0].clone());
        let available = ["set_tasks", "read_file", "write_file"];
        let err = config.validate(&available).unwrap_err();
        assert_eq!(err, TeamConfigError::DuplicateMember("coordinator".to_string()));
    }

    #[test]
    fn rejects_tool_allowlist_entry_not_in_available_tools() {
        let mut config = valid_config();
        config.members[1].tool_allowlist.push("run_command".to_string());
        let available = ["set_tasks", "read_file", "write_file"]; // no run_command
        let err = config.validate(&available).unwrap_err();
        assert_eq!(
            err,
            TeamConfigError::UnknownTool {
                member: "implementer".to_string(),
                tool: "run_command".to_string(),
            }
        );
    }
}
```

- [ ] **Step 3: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team validation_tests
```

Expected: compile error — `validate`/`TeamConfigError` don't exist yet.

- [ ] **Step 4: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamConfigError {
    #[error("team's `lead` ({0:?}) does not match any member's name")]
    UnknownLead(String),
    #[error("duplicate member name: {0:?}")]
    DuplicateMember(String),
    #[error("member {member:?}'s tool_allowlist names an unavailable tool: {tool:?}")]
    UnknownTool { member: String, tool: String },
}

impl TeamConfig {
    /// Validates this config against the caller-supplied list of tool
    /// names actually available to the lead. Does not check
    /// `deny_paths`/`extra_deny_paths` shape (any string is accepted as
    /// a path pattern here) -- only structural validity: the lead
    /// exists, member names are unique, and every tool_allowlist entry
    /// is one the lead itself could grant.
    pub fn validate(&self, available_tools: &[&str]) -> Result<(), TeamConfigError> {
        if !self.members.iter().any(|m| m.name == self.lead) {
            return Err(TeamConfigError::UnknownLead(self.lead.clone()));
        }

        let mut seen = std::collections::HashSet::new();
        for member in &self.members {
            if !seen.insert(member.name.clone()) {
                return Err(TeamConfigError::DuplicateMember(member.name.clone()));
            }
        }

        for member in &self.members {
            for tool in &member.tool_allowlist {
                if !available_tools.contains(&tool.as_str()) {
                    return Err(TeamConfigError::UnknownTool {
                        member: member.name.clone(),
                        tool: tool.clone(),
                    });
                }
            }
        }

        Ok(())
    }
}
```

- [ ] **Step 5: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
```

Expected: all tests (Task 1's + Task 2's) pass.

- [ ] **Step 6: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
cargo clippy -p aivyx-team --all-targets -- -D warnings
cargo fmt --check -p aivyx-team
```

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-team/Cargo.toml crates/aivyx-team/src/lib.rs
git commit -m "feat: add TeamConfig::validate -- reject unknown lead, duplicate members, unknown tools

Fails at config-load time, matching this project's existing convention
of failing fast at load rather than first use (e.g.
[mcp_server].max_access_level). Does not yet check deny_paths shape --
any string is accepted as a path pattern; only structural validity
against a caller-supplied available-tool-names list."
```

---

## Task 3: The attenuation invariant — deny-paths union and tool-allowlist subset

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-team/src/lib.rs`

**Interfaces:**
- Consumes: `TeamMember` from Task 1.
- Produces: `pub fn effective_deny_paths(lead_deny_paths: &[String], member: &TeamMember) -> Vec<String>`
  (union, de-duplicated); `pub fn tool_allowlist_is_subset(member: &TeamMember, lead_tools: &[&str]) -> bool`
  (true iff every entry in `member.tool_allowlist` is present in
  `lead_tools` — note this is a *narrower*, per-member check than
  `TeamConfig::validate`'s "available to the lead at all" check; this
  function is what Phase 2's real delegation code will call per-member
  against the LEAD's own actual current tool set, which is a stronger
  and more specific invariant than validation-time's crate-wide
  available-tools list).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod attenuation_tests {
    use super::*;

    fn member(tool_allowlist: &[&str], extra_deny_paths: &[&str]) -> TeamMember {
        TeamMember {
            name: "implementer".to_string(),
            role: "Implementer".to_string(),
            persona: "You write code.".to_string(),
            tool_allowlist: tool_allowlist.iter().map(|s| s.to_string()).collect(),
            extra_deny_paths: extra_deny_paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn effective_deny_paths_unions_lead_and_member_paths() {
        let lead_deny_paths = vec![".env".to_string(), "*.pem".to_string()];
        let m = member(&["read_file"], &["secrets/"]);
        let mut effective = effective_deny_paths(&lead_deny_paths, &m);
        effective.sort();
        let mut expected = vec![".env".to_string(), "*.pem".to_string(), "secrets/".to_string()];
        expected.sort();
        assert_eq!(effective, expected);
    }

    #[test]
    fn effective_deny_paths_deduplicates_overlapping_entries() {
        let lead_deny_paths = vec![".env".to_string()];
        let m = member(&["read_file"], &[".env"]);
        let effective = effective_deny_paths(&lead_deny_paths, &m);
        assert_eq!(effective, vec![".env".to_string()]);
    }

    #[test]
    fn effective_deny_paths_with_no_extra_paths_equals_lead_paths() {
        let lead_deny_paths = vec![".env".to_string()];
        let m = member(&["read_file"], &[]);
        assert_eq!(effective_deny_paths(&lead_deny_paths, &m), lead_deny_paths);
    }

    #[test]
    fn tool_allowlist_is_subset_true_when_every_tool_is_available() {
        let m = member(&["read_file", "grep"], &[]);
        assert!(tool_allowlist_is_subset(&m, &["read_file", "grep", "write_file"]));
    }

    #[test]
    fn tool_allowlist_is_subset_false_when_a_tool_is_missing() {
        let m = member(&["read_file", "run_command"], &[]);
        assert!(!tool_allowlist_is_subset(&m, &["read_file", "grep"]));
    }

    #[test]
    fn tool_allowlist_is_subset_true_for_empty_allowlist() {
        let m = member(&[], &[]);
        assert!(tool_allowlist_is_subset(&m, &["read_file"]));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team attenuation_tests
```

Expected: compile error — the two functions don't exist yet.

- [ ] **Step 3: Implement**

```rust
/// A specialist's effective deny-list: the union of the lead's own
/// `deny_paths` and the member's `extra_deny_paths`, de-duplicated. A
/// specialist can only ever be handed *more* restriction than the lead
/// already has -- there is deliberately no way for a member's config to
/// remove one of the lead's own entries.
pub fn effective_deny_paths(lead_deny_paths: &[String], member: &TeamMember) -> Vec<String> {
    let mut effective: Vec<String> = lead_deny_paths.to_vec();
    for path in &member.extra_deny_paths {
        if !effective.contains(path) {
            effective.push(path.clone());
        }
    }
    effective
}

/// True iff every tool in `member`'s `tool_allowlist` is present in
/// `lead_tools` -- the direct analog of NT-02 ("a specialist can never
/// exceed its lead") for tool access, checked against the lead's own
/// actual current tool set (a stronger, more specific check than
/// `TeamConfig::validate`'s crate-wide available-tools check).
pub fn tool_allowlist_is_subset(member: &TeamMember, lead_tools: &[&str]) -> bool {
    member.tool_allowlist.iter().all(|t| lead_tools.contains(&t.as_str()))
}
```

- [ ] **Step 4: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
```

Expected: all tests across all three tasks pass.

- [ ] **Step 5: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
cargo clippy -p aivyx-team --all-targets -- -D warnings
cargo fmt --check -p aivyx-team
```

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-team/src/lib.rs
git commit -m "feat: add effective_deny_paths and tool_allowlist_is_subset -- the NT-02 analog

Pure functions, no live registry dependency (per this plan's Global
Constraints) -- Phase 2's real delegation code will call these against
the lead's actual current deny_paths/tool set. effective_deny_paths is
a de-duplicated union (lead's deny_paths ∪ member's extra_deny_paths);
tool_allowlist_is_subset mirrors aivyx-pa's NT-02 invariant
(specialist.tools ⊆ lead.tools) using aivyx-coder's own tool-name
strings instead of aivyx-pa's CapabilitySet/Scope types, which
aivyx-coder doesn't have."
```

---

## Task 4: Default coding-shaped roster

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-team/src/lib.rs`

**Interfaces:**
- Consumes: `TeamConfig`/`TeamMember`/`validate` from Tasks 1-2.
- Produces: `pub fn default_coding_roster() -> TeamConfig`.

- [ ] **Step 1: Re-verify the real tool names before writing this task**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
for f in crates/aivyx-tools/src/tools/*.rs; do
  awk '/fn name\(&self\)/{getline; print}' "$f"
done | sed 's/^\s*//' | sort
```

Confirm the tool names used below still match — this plan's own research
found them at planning time, but the registry may have changed since.
Adjust the roster's `tool_allowlist` entries if any name has been
renamed or removed, and note any such change in your report.

- [ ] **Step 2: Write the failing test**

```rust
#[cfg(test)]
mod roster_tests {
    use super::*;

    #[test]
    fn default_coding_roster_is_schema_valid() {
        let roster = default_coding_roster();
        // The full set of tool names this roster's members reference --
        // matches Task 4's Step 1 verification against the real
        // aivyx-tools registry at plan-writing time.
        let available = [
            "set_tasks", "read_file", "write_file", "edit_file", "grep", "glob",
            "run_command", "run_shell", "git_read", "git_commit",
        ];
        assert!(roster.validate(&available).is_ok());
    }

    #[test]
    fn default_coding_roster_has_the_four_expected_members() {
        let roster = default_coding_roster();
        let names: Vec<&str> = roster.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["coordinator", "implementer", "reviewer", "tester"]);
        assert_eq!(roster.lead, "coordinator");
    }

    #[test]
    fn default_coding_roster_coordinator_has_no_direct_execution_tools() {
        let roster = default_coding_roster();
        let coordinator = &roster.members[0];
        // Matches aivyx-pa's own Nonagon convention: the coordinator's
        // persona forbids direct execution -- its tool_allowlist should
        // not include any file-mutating or command-running tool.
        for forbidden in ["write_file", "edit_file", "run_command", "run_shell"] {
            assert!(
                !coordinator.tool_allowlist.contains(&forbidden.to_string()),
                "coordinator should not directly hold {forbidden}"
            );
        }
    }
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team roster_tests
```

Expected: compile error — `default_coding_roster` doesn't exist yet.

- [ ] **Step 4: Implement**

```rust
/// The default, coding-shaped roster this crate ships: `coordinator`
/// (lead, delegates/verifies/synthesizes -- never executes directly),
/// `implementer`, `reviewer`, `tester`. Ships as ready-to-use,
/// schema-valid config data even though no delegation tooling exists
/// yet to invoke it (Phase 2's job) -- matching aivyx-pa's own
/// Foundation-phase precedent of shipping role definitions ahead of the
/// tooling that uses them.
pub fn default_coding_roster() -> TeamConfig {
    TeamConfig {
        lead: "coordinator".to_string(),
        members: vec![
            TeamMember {
                name: "coordinator".to_string(),
                role: "Lead".to_string(),
                persona: "You coordinate a small coding team. You decompose \
                    the task, delegate pieces to implementer/reviewer/tester, \
                    verify their output, and synthesize the final result. You \
                    never write, edit, or run anything directly."
                    .to_string(),
                tool_allowlist: vec!["set_tasks".to_string()],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "implementer".to_string(),
                role: "Implementer".to_string(),
                persona: "You write and edit code to satisfy the task you were \
                    delegated. Read what you need, make the change, keep it \
                    minimal and focused."
                    .to_string(),
                tool_allowlist: vec![
                    "read_file".to_string(),
                    "write_file".to_string(),
                    "edit_file".to_string(),
                    "grep".to_string(),
                    "glob".to_string(),
                ],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "reviewer".to_string(),
                role: "Reviewer".to_string(),
                persona: "You review code changes for correctness, clarity, and \
                    whether they actually satisfy the delegated task. You never \
                    modify files yourself -- you report findings."
                    .to_string(),
                tool_allowlist: vec![
                    "read_file".to_string(),
                    "grep".to_string(),
                    "glob".to_string(),
                    "git_read".to_string(),
                ],
                extra_deny_paths: vec![],
            },
            TeamMember {
                name: "tester".to_string(),
                role: "Tester".to_string(),
                persona: "You verify a change actually works -- run the \
                    relevant tests or commands and report the real output, \
                    pass or fail."
                    .to_string(),
                tool_allowlist: vec![
                    "read_file".to_string(),
                    "run_command".to_string(),
                    "run_shell".to_string(),
                    "grep".to_string(),
                ],
                extra_deny_paths: vec![],
            },
        ],
    }
}
```

- [ ] **Step 5: Run to verify it passes**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-team
```

Expected: all tests across all four tasks pass.

- [ ] **Step 6: Run full crate + workspace check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check -p aivyx-team
```

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-team/src/lib.rs
git commit -m "feat: add default_coding_roster -- coordinator/implementer/reviewer/tester

A coding-shaped default, not aivyx-pa's generic 9-role office team --
matches what coding delegation actually looks like, closer to this
project's own subagent-driven-development convention. Ships as
ready-to-use, schema-valid config data even though Phase 2 (delegation
tooling, a separate future plan) is what actually invokes it."
```

---

## Final verification

- [ ] Run the complete workspace check once more, after all 4 tasks:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check -p aivyx-team
```

Expected: everything clean/passing.

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this spec does not
decide" section.)

- Phases 2-6 (delegation tooling, mission structure, message bus, entry
  point/audit trail, TUI surface) — each gets its own spec/plan cycle
  when reached.
- Whether `deny_paths` union computation happens eagerly or lazily in a
  real delegation path — deferred to Phase 2.
- Where a `TeamConfig` file actually lives / how it's loaded into a real
  running session — Phase 5's concern.
- Whether the default 4-role roster is final — open to revision once
  Phase 2's delegation tools exist and the roster can be exercised for
  real.
