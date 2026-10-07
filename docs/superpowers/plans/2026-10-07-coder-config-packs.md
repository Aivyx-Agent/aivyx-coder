# aivyx-coder Config Packs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `aivyx-coder pack install | use | off | list | remove | check | inspect`. A config pack's aivyx-coder part (an `AGENTS.md`, skills, an optional roster and MCP servers) is installed once, then switched on per project (or for every project) as a layer on top of the user's own setup, without editing any of the user's files.

**Architecture:** A new `crates/aivyx/src/packs.rs` (binary crate) owns the whole feature:
- installing (verify through the shared `aivyx-pack` crate, unpack to `~/.config/aivyx-coder/packs/<name>/<version>/`);
- the activation state, `~/.config/aivyx-coder/packs/active.toml`: which pack each project uses, keyed by canonical project path or `*` for all projects, plus the MCP servers the user approved;
- the checks;
- resolving the active pack for a working directory into a `PackLayer`.

At startup, `main` resolves the layer for the cwd and applies it to an in-memory copy of `Settings`: the roster if `[team] roster_path` is unset, skills into a free skills slot, and approved MCP servers appended. It also passes the pack's `AGENTS.md` to the agent as an extra instructions layer.

**Tech Stack:** Rust, clap (derive) subcommands, `aivyx-pack` (git dep, rev `6240c4fd142c3aec89b520fd8c5d47763c4131bf`), `aivyx-skills`, `toml`.

Spec: `aivyx-ecosystem/docs/superpowers/specs/2026-10-07-vertical-packs-design.md`, sub-project 3.

**Changes from the spec (explain to the user):**
1. aivyx-coder has no per-project config file, so `pack use` doesn't write `.aivyx/config.toml`. It records the choice in the user-level `active.toml`, keyed by the canonical project path (as sessions already are). Committed repository files can therefore never switch a pack on.
2. `aivyx-skills` has only `user` and `project` overlay slots. A pack's skills fill whichever slot the user hasn't set; if both are set, the pack's skills aren't loaded and a startup notice says so. A dedicated pack slot would need a change to `aivyx-skills` (left for later).

## Global Constraints

- The user's own files are never edited by `pack use`/`off`: not `config.toml`, not any `AGENTS.md`, not anything in the project.
- MCP servers start commands, so they're enabled only after the user agrees, per server, at `pack use`, with the full command line shown. Approval is stored with the exact command and args; if they differ at startup, the server isn't started and a notice says to run `pack use` again. Without a terminal, no server is approved.
- Pack instructions are labelled as the pack's and scanned for injection markers like the other `AGENTS.md` sources. Precedence: project `AGENTS.md` > pack > user `AGENTS.md`.
- Trust: `[pack] trusted_publishers` in `config.toml` (base64 Ed25519 keys, validated as 32 bytes) ∪ `aivyx_pack::AIVYX_PUBLISHER_KEYS`.
- `min_version` is checked against `env!("CARGO_PKG_VERSION")` with `aivyx_pack::daemon_version_ok`.
- Install keeps one version per pack: an install replaces older versions of the same pack. It writes to a temp dir, checks, then renames. A failure leaves nothing behind.
- Tests fail if a CLI flag has no `### \`--flag\`` section in `docs/manual/reference/01-command-line.md`; subcommands get `### \`pack …\`` sections and a matching drift test.
- Verification: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.
- Commits: `git commit -s`, ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: Install, check, inspect, list, remove

**Files:**
- Modify: `Cargo.toml` (workspace dep `aivyx-pack = { git = "https://github.com/Aivyx-Agent/aivyx-pack", rev = "6240c4fd142c3aec89b520fd8c5d47763c4131bf" }`, with the "pinned by SHA, must stay public" comment), `crates/aivyx/Cargo.toml` (`aivyx-pack = { workspace = true }`)
- Modify: `crates/aivyx-config/src/lib.rs` (`PackSettings { trusted_publishers: Vec<String> }`, `Settings.pack`, validation in `load`, tests); `scripts/gen-config-reference.py` meaning; regenerate `docs/manual/reference/03-configuration.md`
- Create: `crates/aivyx/src/packs.rs`
- Modify: `crates/aivyx/src/main.rs` (`#[command(subcommand)] command: Option<Command>`; `Command::Pack { #[command(subcommand)] action: PackAction }`; dispatch before the setup/first-run logic)

**Interfaces:**

```rust
#[derive(clap::Subcommand, Debug)]
enum PackAction {
    /// Install a pack's aivyx-coder part
    Install { file: PathBuf },
    /// Use an installed pack in this project (or every project)
    Use { name: String, #[arg(long)] global: bool },
    /// Stop using a pack in this project (or the global one)
    Off { #[arg(long)] global: bool },
    /// Installed packs and where they're in use
    List,
    /// Delete an installed pack
    Remove { name: String },
    /// Check a pack folder's aivyx-coder part (for pack authors)
    Check { dir: PathBuf },
    /// Verify a pack file and show what it contains
    Inspect { file: PathBuf, #[arg(long)] allow_untrusted: bool },
}

// packs.rs
pub struct PacksDir(PathBuf);                       // ~/.config/aivyx-coder/packs
impl PacksDir {
    pub fn user() -> anyhow::Result<Self>;          // beside config.toml
    pub fn at(root: PathBuf) -> Self;               // tests
    pub fn installed(&self) -> Vec<Installed>;      // name, version, dir
    pub fn find(&self, name: &str) -> Option<Installed>;
}
pub struct Installed { pub name: String, pub version: String, pub dir: PathBuf, pub manifest: aivyx_pack::ConfigPackManifest }
pub struct CoderPartSummary { pub skills: Vec<String>, pub roster_members: usize, pub mcp_servers: Vec<aivyx_config::McpServerConfig> }
pub fn check_coder_part(dir: &Path) -> Result<CoderPartSummary, Vec<String>>;
pub fn install(packs: &PacksDir, file: &Path, trusted: &[String]) -> Result<Installed, String>;
pub fn remove(packs: &PacksDir, name: &str) -> Result<(), String>;     // also drops it from active.toml
```

`check_coder_part` (collects every problem):
1. The manifest is format 2 with a `coder` part; otherwise "this is a tool pack…" or "this pack has no aivyx-coder part — it's for aivyx-pa".
2. `agents_file` exists and isn't empty.
3. Skills: each `<name>/SKILL.md` loads via `SkillLoader::new().with_project_dir(dir).get(name)` with `source == SkillSource::Project`; an empty skills folder is a problem.
4. Roster (if present) parses as `aivyx_team::TeamConfig` and passes `validate` against the built-in tool names (`aivyx_tools` registry names, from the same list `resolve_team_config` uses).
5. MCP (if present) parses as `{ servers: Vec<McpServerConfig> }` (`[[servers]]`), with names unique and non-empty commands.

`install`:
- read + verify with the trusted keys, then `read_any_manifest` (must be a config pack with a `coder` part);
- `daemon_version_ok(min_version, CARGO_PKG_VERSION)`;
- unpack to `packs/<name>/.tmp-…`, `check_coder_part`, remove `packs/<name>/*` other versions, rename to `packs/<name>/<version>`.

`list` prints each installed pack with its version and where it's in use (`this project`, `all projects`, or other project paths). `inspect` prints `aivyx_pack::describe::describe` plus the check result.

- [ ] **Step 1: Failing tests** (in `packs.rs`; helper `good_pack(tag)` writes manifest (`products = ["coder"]`, `[coder] min_version = "0.4.0"`, `agents_file = "coder/AGENTS.md"`, `skills = "coder/skills"`, `mcp = "coder/mcp.toml"`), `coder/AGENTS.md`, `coder/skills/example/SKILL.md`, and `coder/mcp.toml` with one `[[servers]] name = "files" command = "mcp-files" args = ["--root", "."]`; `signed(dir)` builds and signs a bundle with a fresh key)
  - `a_good_coder_part_passes` (1 skill, 1 MCP server)
  - `every_problem_is_reported_at_once` (empty AGENTS.md + bad skill + duplicate server → 3 problems)
  - `a_pa_only_pack_is_explained`
  - `install_unpacks_and_replaces_older_versions` (install 0.1.0, then 0.2.0 → only `packs/<name>/0.2.0` exists)
  - `an_untrusted_pack_is_refused_and_nothing_is_left`
  - `a_pack_needing_a_newer_aivyx_coder_is_refused`
  - `remove_deletes_the_pack`
  - config: `pack_trusted_publishers_must_be_32_byte_keys`
  - main: the `Pack` subcommands parse (`Cli::try_parse_from(["aivyx-coder", "pack", "use", "bm", "--global"])`).
- [ ] **Step 2: Run** — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p aivyx -p aivyx-config` → pass.
- [ ] **Step 5: Commit** — `feat: install, check, inspect and remove config packs`.

---

### Task 2: Switching a pack on and off; MCP consent

**Files:**
- Modify: `crates/aivyx/src/packs.rs` (`ActiveState` load/save; `use_pack`, `off`)

**Interfaces:**

```rust
/// ~/.config/aivyx-coder/packs/active.toml
#[derive(Serialize, Deserialize, Default)]
pub struct ActiveState { #[serde(default, rename = "use")] pub uses: Vec<Use> }
#[derive(Serialize, Deserialize, Clone)]
pub struct Use {
    /// A canonical project path, or "*" for every project.
    pub scope: String,
    pub pack: String,
    /// MCP servers the user approved, with the exact command they approved.
    #[serde(default)] pub mcp: Vec<ApprovedServer>,
}
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct ApprovedServer { pub name: String, pub command: String, pub args: Vec<String> }

impl ActiveState {
    pub fn load(packs: &PacksDir) -> Self;                 // missing/unreadable → default (with a warning log)
    pub fn save(&self, packs: &PacksDir) -> std::io::Result<()>;   // 0600
    /// The use for `cwd`: an exact project match wins over "*".
    pub fn for_project(&self, cwd: &Path) -> Option<&Use>;
}
/// `ask(server) -> bool` decides each MCP server (the CLI prompts; tests stub it).
pub fn use_pack(packs: &PacksDir, name: &str, scope: String, ask: &mut dyn FnMut(&McpServerConfig) -> bool) -> Result<Use, String>;
pub fn off(packs: &PacksDir, scope: &str) -> Result<bool, String>;   // false = nothing was on
```

The CLI's `ask`, when stdin is a terminal, prints `Pack <name> wants to run an MCP server:\n  <name>: <command> <args…>\nAllow it? [y/N] `; without a terminal it returns false and prints "MCP servers left off (no terminal to ask)". Scope: `--global` → `"*"`, else `std::fs::canonicalize(cwd)`.

- [ ] **Step 1: Failing tests**
  - `use_records_the_project_and_approved_servers` (ask → true; `active.toml` round-trips; `for_project(cwd)` finds it)
  - `a_declined_server_is_not_recorded`
  - `a_project_use_beats_the_global_one`
  - `off_removes_only_that_scope`
  - `using_a_pack_that_isnt_installed_is_an_error`
- [ ] **Step 2–4:** fail → implement → pass.
- [ ] **Step 5: Commit** — `feat: switch a config pack on per project, with MCP consent`.

---

### Task 3: The pack layer at startup

**Files:**
- Modify: `crates/aivyx/src/packs.rs` (`PackLayer`, `resolve_layer`, `apply_to_settings`)
- Modify: `crates/aivyx-core/src/agent/types.rs`, `crates/aivyx-core/src/agent/mod.rs` (`AgentsFileConfig.pack: Option<(String, PathBuf)>`, `Agent::set_pack_instructions(name, path)`, `refresh_agents_files` reads it, order and precedence note), `crates/aivyx-core/src/agent/tests.rs`
- Modify: `crates/aivyx/src/agent_builder.rs` (`build_agent(cli, settings, prompter, pack: Option<&PackLayer>)`: after `set_agents_file`, `set_pack_instructions`; startup notices through the same path as `cloud_backend_notice`)
- Modify: `crates/aivyx/src/main.rs` (TUI, ACP and MCP-server paths: resolve the layer for the cwd, apply it to `settings`, pass it to `build_agent`)

**Interfaces:**

```rust
pub struct PackLayer {
    pub name: String,
    pub version: String,
    pub agents_file: PathBuf,
    pub skills_dir: Option<PathBuf>,
    pub roster: Option<PathBuf>,
    pub mcp_servers: Vec<McpServerConfig>,    // approved and unchanged only
    pub notices: Vec<String>,                 // shown at startup
}
pub fn resolve_layer(packs: &PacksDir, cwd: &Path) -> Option<PackLayer>;
/// Roster only if `[team] roster_path` is unset; skills into the unset slot
/// (project first, then user), else a notice; approved MCP servers appended
/// unless the user already has a server with that name (notice).
pub fn apply_to_settings(layer: &mut PackLayer, settings: &mut Settings);
```

Notices: `Using pack <name> v<version> here.`; `Pack <name>'s MCP server <s> changed since you approved it — run 'aivyx-coder pack use <name>' again.`; `Pack <name>'s skills aren't loaded: [skills] project_dir and user_dir are both set.`; `Pack <name> is in use here but isn't installed any more — run 'aivyx-coder pack off'.`

Agent: `refresh_agents_files` adds `Pack instructions (<name>):\n<content>` (injection-scanned with label `pack <name> AGENTS.md`, budget notice like the others). The order of sections is user, pack, project. With more than one section, the note reads: "(Project instructions take precedence over pack instructions, and both over user preferences, if they conflict.)".

- [ ] **Step 1: Failing tests**
  - packs: `resolve_layer_returns_the_projects_pack`, `a_changed_mcp_command_is_not_started`, `apply_fills_only_unset_settings` (user roster kept; skills to project slot; both slots set → notice; MCP appended; name clash → notice), `a_missing_installed_pack_gives_a_notice`.
  - core: `pack_instructions_are_included_between_user_and_project` (a temp cwd with `AGENTS.md`, a pack file and a user file → the three sections in order with the precedence note), `pack_instructions_are_injection_scanned`.
- [ ] **Step 2–4:** fail → implement → pass (`cargo test --workspace`).
- [ ] **Step 5: Commit** — `feat: a config pack in use adds its instructions, skills, roster and approved MCP servers`.

---

### Task 4: Docs

**Files:**
- Modify: `docs/manual/reference/01-command-line.md` (a `## Subcommands` section with `### \`pack install\``, `### \`pack use\``, `### \`pack off\``, `### \`pack list\``, `### \`pack remove\``, `### \`pack check\``, `### \`pack inspect\``) + a drift test in `main.rs` `every_subcommand_has_a_command_line_reference_heading` (iterates `Cli::command().get_subcommands()` and their subcommands)
- Create: `docs/manual/guide/16-packs.md` (what a pack is, install → use → off, what a pack can and can't change, MCP consent, trust keys); register it wherever guide chapters are listed (`docs/manual/README.md`)
- Modify: `docs/manual/reference/06-files-and-paths.md` (`packs/` and `packs/active.toml`), `README.md` (one line under features), `CHANGELOG.md` (Unreleased → Added), `CLAUDE.md` (a sentence in the architecture/config notes about `packs.rs` and the pack layer)

- [ ] **Step 1:** Write the docs and the drift test.
- [ ] **Step 2: Run** — `cargo test --workspace`; `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] **Step 3: Commit** — `docs: config packs`.
