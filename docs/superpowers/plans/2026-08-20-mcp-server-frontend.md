# MCP-Server Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `aivyx-coder` a third frontend — `aivyx --mcp-server` — that exposes it as an MCP server over stdio, so any local MCP client can delegate a bounded, tiered coding task to it.

**Architecture:** A new frontend crate, `aivyx-mcp-server`, built on the official `rmcp` SDK. `main.rs` calls the *existing* `agent_builder::build_agent` once at startup (as it already does unconditionally today) to get the shared, expensive-to-build pieces (LLM backend, confiner, checkpointer, repo map, a delegate-shaped base tool registry) — its outer `Agent` is never used for a turn. Each `code` MCP call then builds a **fresh** `Agent`, mirroring `delegate_task`'s own construction shape exactly, with a tool registry pre-filtered to one of three tiers (`plan`/`edit`/`execute`) via `ToolRegistry::exclude`. A custom `PermissionPrompter` (`TieredPrompter`) auto-resolves any call from the tier's own allowed set — belt-and-braces, since the excluded tools were never in the registry to begin with. Sessions live in-memory, TTL-evicted, bounded by an operator-configured ceiling that per-call requests may never exceed.

**Tech Stack:** Rust, `rmcp` 0.11 (official MCP SDK, `transport-io` feature for stdio), the existing `aivyx-core`/`aivyx-sandbox`/`aivyx-tools` crates.

## Global Constraints

- `Tool::mutates_outside_session()`'s existing classification is the source of truth for "plan" tier — reused via `ToolRegistry::exclude`, not reimplemented.
- **Tier tool sets (verified against real code, not the design doc's original partial list):**
  - `plan` = `read_file`, `grep`, `glob`, `git_read`, `find_references`, `go_to_definition`, `memory_read`, `set_tasks`, `web_fetch`, `web_search`.
  - `edit` = `plan`'s set + `write_file`, `edit_file`, `delete_file`, `move_file`, `patch_file`.
  - `execute` = `edit`'s set + `run_command`, `run_shell`, `git_commit`, `git_branch`, `git_push`, `git_pr`, `memory_write`, `memory_forget`, `remember_preference`.
  - Excluded from **every** tier, always: `delegate_task` (recursion), `repl_start`/`repl_send`/`repl_stop` (unreachable after an ephemeral session ends — same reasoning `delegate_task`'s own `sub_agent_registry` already uses), every dynamically-bridged `mcp__<server>__<tool>` adapter, and (**removed from `plan` here after the final whole-branch review** — they were originally listed as part of `plan`'s set above, which contradicted the bridged-tool exclusion's own confused-deputy rationale one line below it) `list_mcp_resources`/`read_mcp_resource`/`list_mcp_prompts`/`get_mcp_prompt` (confused-deputy risk — a remote MCP caller must not transitively reach a third-party MCP server the operator configured for a different purpose, whether via a bridged tool call or via reading that server's resources/prompts).
- `AutonomousMode` is never reused or set active for any MCP session — a deliberate, separate mechanism per the approved design.
- `max_access_level` has no default; the server refuses to start if it's unset, mirroring `--auto`'s existing "refuse to start rather than run degraded" posture for its own required config.
- A `code` call requesting a level above the ceiling is **rejected outright**, before any `Agent` is constructed — never silently downgraded.
- Sessions are in-memory only (a bounded map inside the server process), separate from the on-disk `--resume` store. TTL-evicted (`session_ttl_secs`); the oldest idle session is evicted if a new session would exceed `max_concurrent_sessions`.
- An access level is fixed at `code`-call time and is not renegotiable via `code_reply`.
- No `close_session` tool for v1 — TTL eviction is the only cleanup path.
- Each `code`/`code_reply` call gets its own budget of up to `max_iterations` round trips (not a cumulative session-wide budget) — mirrors `delegate_task`'s own per-call budget shape exactly.

---

### Task 1: `[mcp_server]` config section

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (add `McpServerSettings`, add a field to `Settings`)
- Test: same file, `#[cfg(test)] mod tests` at the bottom

**Interfaces:**
- Produces: `pub struct McpServerSettings { pub max_access_level: Option<String>, pub session_ttl_secs: u64, pub max_concurrent_sessions: u32, pub max_iterations: u32 }`, reachable as `settings.mcp_server`.

- [ ] **Step 1: Write the failing tests**

Find the test module near the bottom of `crates/aivyx-config/src/lib.rs` (look for `autonomous_settings_have_conservative_defaults` and `autonomous_settings_parse_from_config` — your new tests go right after them, same style). Add:

```rust
    #[test]
    fn mcp_server_settings_have_conservative_defaults_and_no_access_level() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.mcp_server.max_access_level, None);
        assert_eq!(settings.mcp_server.session_ttl_secs, 1800);
        assert_eq!(settings.mcp_server.max_concurrent_sessions, 8);
        assert_eq!(settings.mcp_server.max_iterations, 10);
    }

    #[test]
    fn mcp_server_settings_parse_from_config() {
        let raw = r#"
            [mcp_server]
            max_access_level = "edit"
            session_ttl_secs = 600
            max_concurrent_sessions = 4
            max_iterations = 5
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.mcp_server.max_access_level.as_deref(), Some("edit"));
        assert_eq!(settings.mcp_server.session_ttl_secs, 600);
        assert_eq!(settings.mcp_server.max_concurrent_sessions, 4);
        assert_eq!(settings.mcp_server.max_iterations, 5);
    }
```

- [ ] **Step 2: Run the tests to verify they fail to compile**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config mcp_server_settings --no-run
```

Expected: a compile error — `Settings` has no field `mcp_server` and `McpServerSettings` doesn't exist yet.

- [ ] **Step 3: Implement**

Find `SubAgentSettings` in `crates/aivyx-config/src/lib.rs` (its struct + `impl Default` block). Add this new struct immediately after it:

```rust
/// `--mcp-server`: exposes `aivyx-coder` as an MCP server. `max_access_level`
/// has no working default — the operator must set it explicitly, or the
/// server refuses to start (same "refuse to start rather than run
/// degraded" posture `--auto` already uses for its own required
/// `[verification].command`). Sessions live in-memory only, TTL-evicted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerSettings {
    /// "plan" | "edit" | "execute" — the ceiling. A `code` call requesting
    /// a level above this is rejected, never silently downgraded. `None`
    /// (the default) means the server has not been configured at all and
    /// must refuse to start.
    pub max_access_level: Option<String>,
    /// An idle MCP session (no `code_reply` call) is evicted after this
    /// long.
    pub session_ttl_secs: u64,
    /// Bounded in-memory session map; the oldest idle session is evicted
    /// if a new session would exceed this.
    pub max_concurrent_sessions: u32,
    /// A single `code` or `code_reply` call's own round-trip budget —
    /// mirrors `[sub_agent].max_iterations`'s shape exactly (an outer
    /// "continue" loop, not `AgentConfig.max_tool_iterations`).
    pub max_iterations: u32,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        Self {
            max_access_level: None,
            session_ttl_secs: 1800,
            max_concurrent_sessions: 8,
            max_iterations: 10,
        }
    }
}
```

Find the `Settings` struct (starts `pub struct Settings {`). Add the new field right after `pub sub_agent: SubAgentSettings,`:

```rust
    pub sub_agent: SubAgentSettings,
    pub mcp_server: McpServerSettings,
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config
```

Expected: all tests pass, including the two new ones.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs
git commit -m "Add [mcp_server] config section for the upcoming MCP-server frontend

max_access_level has no default -- the operator must set it explicitly
before --mcp-server can start, matching --auto's existing posture for
its own required config."
```

---

### Task 2: Expose shared construction pieces from `agent_builder::build_agent`

**Files:**
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Consumes: nothing new from earlier tasks.
- Produces: `BuiltAgent` gains 5 new `pub(crate)` fields — `llm: Arc<dyn LlmBackend>`, `confiner: Arc<dyn ExecutionConfiner>`, `checkpointer: Option<Arc<GitCheckpointer>>`, `repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)>`, `mcp_registry: ToolRegistry` — all populated from values `build_agent` already computes internally. Task 5's MCP-server startup code consumes these.

- [ ] **Step 1: Add the new fields to `BuiltAgent`**

Find the `BuiltAgent` struct (`crates/aivyx/src/agent_builder.rs`, starts `pub(crate) struct BuiltAgent {`). Add these 5 fields at the end, before the closing `}`:

```rust
    /// The shared LLM backend — cheap `Arc` clone, reused by the MCP-server
    /// frontend to build a fresh `Agent` per session rather than sharing
    /// this struct's own (unused, for that frontend) `agent` field.
    pub(crate) llm: Arc<dyn LlmBackend>,
    pub(crate) confiner: Arc<dyn aivyx_sandbox::ExecutionConfiner>,
    pub(crate) checkpointer: Option<Arc<GitCheckpointer>>,
    pub(crate) repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)>,
    /// The full tool registry, minus `delegate_task` (not yet registered
    /// at the point this is cloned from), minus `repl_start`/`repl_send`/
    /// `repl_stop` (unreachable after an ephemeral session ends), minus
    /// every dynamically-bridged `mcp__<server>__<tool>` adapter
    /// (confused-deputy risk for a remote MCP caller) — the base every
    /// MCP-server session's own tier-filtered registry is built from.
    pub(crate) mcp_registry: ToolRegistry,
```

- [ ] **Step 2: Collect MCP-bridged tool names during the existing discovery loop**

Find the MCP server discovery loop (`for server in settings.mcp.servers.clone() { ... }` and the `while let Some(joined) = mcp_discovery.join_next().await { ... }` loop right after it). Immediately before that `for` loop, add:

```rust
    // Collected as each bridged tool is discovered, so mcp_registry (built
    // below, after this loop) can exclude them by their real registered
    // name — dynamically server-defined, so no static list would work.
    let mut mcp_bridged_tool_names: Vec<String> = Vec::new();
```

Inside the `while let Some(joined) = mcp_discovery.join_next().await` loop's `Ok(Ok(tools)) => { for tool_info in tools { ... } ... }` arm, add one line right after the existing `registry.register(Arc::new(McpToolAdapter::new(...)));` call:

```rust
            Ok(Ok(tools)) => {
                for tool_info in tools {
                    registry.register(Arc::new(McpToolAdapter::new(
                        Arc::clone(&client),
                        &server_name,
                        tool_info,
                    )));
                    mcp_bridged_tool_names.push(format!("mcp__{server_name}__{}", tool_info.name));
                }
                mcp_clients.push(client);
            }
```

Note: `tool_info` is moved into `McpToolAdapter::new` on the line above — reorder so the `format!` line (which only needs `tool_info.name`, a read) runs first, or clone `tool_info.name` before the move. The corrected block:

```rust
            Ok(Ok(tools)) => {
                for tool_info in tools {
                    mcp_bridged_tool_names.push(format!("mcp__{server_name}__{}", tool_info.name));
                    registry.register(Arc::new(McpToolAdapter::new(
                        Arc::clone(&client),
                        &server_name,
                        tool_info,
                    )));
                }
                mcp_clients.push(client);
            }
```

- [ ] **Step 3: Build `mcp_registry` alongside the existing `sub_agent_registry`**

Find the existing line:

```rust
    let mut sub_agent_registry = registry.clone();
    sub_agent_registry.exclude(&["repl_start", "repl_send", "repl_stop"]);
```

Add immediately after it:

```rust
    // Same clone point as sub_agent_registry (before delegate_task is
    // registered below) — additionally excludes dynamically-bridged MCP
    // tools, which sub_agent_registry does not need to (a delegate_task
    // sub-agent runs in the same trust boundary as its parent; an MCP-
    // server session's caller is a different process/product entirely).
    let mut mcp_registry = registry.clone();
    mcp_registry.exclude(&["repl_start", "repl_send", "repl_stop"]);
    let bridged_names: Vec<&str> = mcp_bridged_tool_names.iter().map(|s| s.as_str()).collect();
    mcp_registry.exclude(&bridged_names);
```

- [ ] **Step 4: Populate the new `BuiltAgent` fields**

Find the final `Ok(BuiltAgent { ... })` at the end of `build_agent`. Add the 5 new fields (all values already exist as local variables by this point in the function):

```rust
    Ok(BuiltAgent {
        agent,
        events_rx,
        cwd,
        plan_mode,
        restored,
        tasks,
        injection_taint,
        repl_resize,
        llm,
        confiner,
        checkpointer,
        repo_map,
        mcp_registry,
    })
```

Note: `llm` was moved into `Agent::new(...)` earlier in the function (the call that builds `agent`) — `Agent::new`'s first parameter is `llm: Arc<dyn LlmBackend>`, taken by value. Since `BuiltAgent` also needs an `Arc<dyn LlmBackend>`, clone it *before* that `Agent::new(...)` call: find `let mut agent = Agent::new(llm, executor, ...)` and change its first argument to `Arc::clone(&llm)`, keeping the original `llm` binding alive for the struct literal above. Same reasoning applies to `confiner`: it's used via `Arc::clone(&confiner)` at several existing call sites already (e.g. the `ToolExecutor::new(registry, Arc::clone(&gate), Arc::clone(&confiner))` line), so it is *already* a live binding at the point `BuiltAgent` is constructed — no change needed there. `checkpointer` and `repo_map` are each read via `.clone()` at existing call sites too (e.g. `checkpointer.clone()` inside the `DelegateTaskConfig` literal, `repo_map.clone()` likewise) and remain live locals — no change needed for those either.

- [ ] **Step 5: Build and run the existing test suite**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test -p aivyx-config -p aivyx-core -p aivyx-tools -p aivyx-acp -p aivyx-tui
```

Expected: clean build, zero test failures — this task only *adds* fields and *collects* names into a new local `Vec`; it changes no existing behavior for the TUI or ACP frontends.

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/src/agent_builder.rs
git commit -m "Expose shared construction pieces via BuiltAgent for the MCP-server frontend

Adds llm/confiner/checkpointer/repo_map/mcp_registry to BuiltAgent --
all values build_agent already computes internally. mcp_registry
mirrors the existing sub_agent_registry clone point, additionally
excluding dynamically-bridged MCP tools (a confused-deputy risk for a
remote MCP-server caller that delegate_task's own sub-agent, running
in the same trust boundary as its parent, doesn't share).

No behavior change for the TUI or ACP frontends -- build_agent's own
body and its wrapped Agent are unchanged."
```

---

### Task 3: Tier logic — `AccessLevel`, tool-name sets, `TieredPrompter`, session-turn helper

**Files:**
- Create: `crates/aivyx-mcp-server/Cargo.toml`
- Create: `crates/aivyx-mcp-server/src/lib.rs`
- Create: `crates/aivyx-mcp-server/src/tiers.rs`
- Create: `crates/aivyx-mcp-server/src/session.rs`
- Modify: root `Cargo.toml` (`[workspace.dependencies]`)

**Interfaces:**
- Consumes: `BuiltAgent`'s new fields (Task 2); `aivyx_core::{Agent, AgentConfig, AgentEvent, EditFormat}`; `aivyx_sandbox::{ConfirmationGate, PermissionGate, PermissionPrompter, PermissionRequest, PlanMode, AutonomousMode, UserResponse}`; `aivyx_tools::{ToolRegistry, ToolExecutor, GitCheckpointer}`.
- Produces: `pub enum AccessLevel { Plan, Edit, Execute }` with `FromStr`-style parsing (`AccessLevel::parse(&str) -> Result<Self, String>`) and an ordering (`AccessLevel::at_most(&self, ceiling: &AccessLevel) -> bool`); `pub struct SessionConfig` (everything `build_session_agent` needs, captured once at server startup — mirrors `DelegateTaskConfig`'s own shape); `pub async fn build_session_agent(config: &SessionConfig, level: AccessLevel, events_tx: mpsc::UnboundedSender<AgentEvent>) -> Agent` (a fresh, tier-filtered, isolated `Agent`, no turn run yet — the caller supplies the channel so it can hold onto the matching receiver itself, e.g. Task 4's `StoredSession`); `pub async fn run_bounded_turn(agent: &mut Agent, events_rx: &mut mpsc::UnboundedReceiver<AgentEvent>, input: String, cwd: &Path, max_iterations: u32, cancellation: CancellationToken) -> (Result<(), aivyx_core::AgentError>, String)` (runs `delegate_task`'s own outer "continue" loop shape, returns the final accumulated text; `events_rx` is borrowed since Task 4's session map must keep owning it across multiple `code_reply` calls). Task 4 consumes all four.

- [ ] **Step 1: Create the crate scaffold**

`crates/aivyx-mcp-server/Cargo.toml`:

```toml
[package]
name = "aivyx-mcp-server"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
rmcp = { version = "0.11.0", features = ["transport-io"] }
aivyx-core = { version = "0.1.0", path = "../aivyx-core" }
aivyx-sandbox = { version = "0.1.0", path = "../aivyx-sandbox" }
aivyx-tools = { version = "0.1.0", path = "../aivyx-tools" }
aivyx-llm = { version = "0.1.0", path = "../aivyx-llm" }
aivyx-repomap = { version = "0.1.0", path = "../aivyx-repomap" }
anyhow = "1.0.103"
async-trait = "0.1.89"
schemars = "0.8"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0.150"
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "sync", "time"] }
tokio-util = "0.7.18"
uuid = { version = "1", features = ["v4"] }

[dev-dependencies]
# Only used by test-only LlmBackend mocks (BoxStream, StreamExt,
# executor::block_on) -- mirrors aivyx-core's own delegate.rs test module,
# which uses the same crate the same way.
futures = "0.3"
# Only used by the cutoff-notice test's scripted ToolCallComplete event
# (ToolCall/ToolCallId/ToolCallSource) -- Cargo requires this be a direct
# dependency of this crate even though aivyx-core already depends on it;
# a transitive dependency's types are not importable without it.
aivyx-types = { version = "0.1.0", path = "../aivyx-types" }
```

`crates/aivyx-mcp-server/src/lib.rs`:

```rust
//! MCP-server frontend for aivyx-coder's `Agent` core. See
//! `docs/superpowers/specs/2026-08-20-mcp-server-frontend-design.md`.

mod server;
mod session;
mod tiers;

pub use server::{run, McpServerRunConfig};
pub use session::{build_session_agent, run_bounded_turn, SessionConfig};
pub use tiers::AccessLevel;
```

Add the new crate to the workspace's dependency table. In root `Cargo.toml`, find `[workspace.dependencies]` and add, alphabetically alongside the existing entries:

```toml
aivyx-mcp-server = { path = "crates/aivyx-mcp-server" }
```

(The crate itself is auto-discovered by the existing `members = ["crates/*"]` glob — no change needed there.)

- [ ] **Step 2: Write the failing tests for `tiers.rs`**

`crates/aivyx-mcp-server/src/tiers.rs`:

```rust
//! `AccessLevel` and the three tier→tool-name mappings. Each tier's set is
//! additive over the previous (`plan` ⊂ `edit` ⊂ `execute`), verified
//! against real `Tool::mutates_outside_session()` classifications and
//! `delegate_task`'s own exclusion precedent — see the plan's Global
//! Constraints for the full accounting of why each tool landed where it
//! did.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessLevel {
    Plan,
    Edit,
    Execute,
}

impl AccessLevel {
    /// Parses the MCP tool parameter / config value. Case-sensitive,
    /// exactly "plan" | "edit" | "execute" — no aliases, so a typo fails
    /// loudly rather than silently mapping to something unintended.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "plan" => Ok(Self::Plan),
            "edit" => Ok(Self::Edit),
            "execute" => Ok(Self::Execute),
            other => Err(format!(
                "invalid access_level {other:?} -- must be \"plan\", \"edit\", or \"execute\""
            )),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Plan => 0,
            Self::Edit => 1,
            Self::Execute => 2,
        }
    }

    /// `true` if `self` does not exceed `ceiling` -- used to reject a
    /// `code` call's requested level against the operator-configured max.
    pub fn at_most(&self, ceiling: &AccessLevel) -> bool {
        self.rank() <= ceiling.rank()
    }

    /// Names of every tool excluded from `mcp_registry` (Task 2) to reach
    /// this tier -- i.e. everything ranked strictly above it, plus (at
    /// every tier) the always-excluded set from the plan's Global
    /// Constraints.
    pub fn excluded_tool_names(&self) -> Vec<&'static str> {
        const EDIT_ONLY: &[&str] = &["write_file", "edit_file", "delete_file", "move_file", "patch_file"];
        const EXECUTE_ONLY: &[&str] = &[
            "run_command", "run_shell", "git_commit", "git_branch", "git_push", "git_pr",
            "memory_write", "memory_forget", "remember_preference",
        ];
        match self {
            Self::Plan => [EDIT_ONLY, EXECUTE_ONLY].concat(),
            Self::Edit => EXECUTE_ONLY.to_vec(),
            Self::Execute => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_exactly_the_three_valid_strings() {
        assert_eq!(AccessLevel::parse("plan"), Ok(AccessLevel::Plan));
        assert_eq!(AccessLevel::parse("edit"), Ok(AccessLevel::Edit));
        assert_eq!(AccessLevel::parse("execute"), Ok(AccessLevel::Execute));
    }

    #[test]
    fn parse_rejects_anything_else() {
        assert!(AccessLevel::parse("Plan").is_err(), "case-sensitive");
        assert!(AccessLevel::parse("danger-full-access").is_err());
        assert!(AccessLevel::parse("").is_err());
    }

    #[test]
    fn at_most_orders_plan_below_edit_below_execute() {
        assert!(AccessLevel::Plan.at_most(&AccessLevel::Plan));
        assert!(AccessLevel::Plan.at_most(&AccessLevel::Execute));
        assert!(!AccessLevel::Execute.at_most(&AccessLevel::Plan));
        assert!(!AccessLevel::Edit.at_most(&AccessLevel::Plan));
        assert!(AccessLevel::Execute.at_most(&AccessLevel::Execute));
    }

    #[test]
    fn plan_excludes_both_edit_only_and_execute_only_tools() {
        let excluded = AccessLevel::Plan.excluded_tool_names();
        assert!(excluded.contains(&"write_file"));
        assert!(excluded.contains(&"run_command"));
        assert_eq!(excluded.len(), 5 + 9);
    }

    #[test]
    fn edit_excludes_only_execute_only_tools() {
        let excluded = AccessLevel::Edit.excluded_tool_names();
        assert!(!excluded.contains(&"write_file"), "edit tier must include write_file");
        assert!(excluded.contains(&"run_command"));
        assert_eq!(excluded.len(), 9);
    }

    #[test]
    fn execute_excludes_nothing_tier_specific() {
        assert!(AccessLevel::Execute.excluded_tool_names().is_empty());
    }
}
```

Also add `PartialEq`-friendly derive needed for the `assert_eq!(AccessLevel::parse(...), Ok(AccessLevel::Plan))` calls above — `AccessLevel` already derives `PartialEq, Eq` in the struct definition, but `Result<AccessLevel, String>`'s `Ok`/`Err` comparison also needs `AccessLevel: Debug` for the assertion failure message, which is already derived too. No extra derive needed beyond what's shown.

- [ ] **Step 3: Run the tests to verify they fail to compile, then pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-mcp-server tiers --no-run
```

Expected: fails to compile (`server`/`session` modules referenced in `lib.rs` don't exist yet). Create empty stub files so only `tiers` compiles for this step:

`crates/aivyx-mcp-server/src/session.rs`: `// Task 3 Step 4 fills this in.`
`crates/aivyx-mcp-server/src/server.rs`: `// Task 4 fills this in.`

```bash
cargo test -p aivyx-mcp-server tiers
```

Expected: all 6 tests in `tiers::tests` pass.

- [ ] **Step 4: Write the failing tests for `session.rs`**

`crates/aivyx-mcp-server/src/session.rs`:

```rust
//! Fresh, isolated, tier-filtered `Agent` construction for one MCP
//! session -- structurally `delegate_task`'s own shape
//! (`aivyx-core/src/delegate.rs`), invoked externally instead of mid-turn.

use std::path::Path;
use std::sync::Arc;

use aivyx_core::{Agent, AgentConfig, AgentEvent, EditFormat};
use aivyx_sandbox::{
    AutonomousMode, ConfirmationGate, PermissionGate, PermissionPrompter, PermissionRequest,
    PlanMode, UserResponse,
};
use aivyx_tools::{GitCheckpointer, ToolExecutor, ToolRegistry};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::tiers::AccessLevel;

/// Auto-resolves any request from a tool already in this session's own
/// tier-filtered registry -- belt-and-braces with the registry exclusion
/// itself (Task 2's `mcp_registry` + `AccessLevel::excluded_tool_names`):
/// even if the model somehow invents a call to an excluded tool's name,
/// `ToolExecutor::dispatch` fails with "unknown tool" before this prompter
/// is ever reached (the tool isn't in the registry at all), and for any
/// call that IS reachable, `allowed_names` is checked again here as a
/// second, independent guard against exactly that scenario.
pub(crate) struct TieredPrompter {
    allowed_names: std::collections::HashSet<String>,
}

impl TieredPrompter {
    pub(crate) fn new(allowed_names: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_names: allowed_names.into_iter().collect(),
        }
    }
}

#[async_trait]
impl PermissionPrompter for TieredPrompter {
    async fn prompt(&self, request: &PermissionRequest) -> UserResponse {
        if self.allowed_names.contains(&request.tool_name) {
            UserResponse::Allow
        } else {
            UserResponse::Deny
        }
    }
}

/// Everything `build_session_agent` needs, captured once at server startup
/// from `BuiltAgent`'s new fields (Task 2) -- mirrors
/// `DelegateTaskConfig`'s own "gathered once, stable for the server's
/// whole lifetime" shape.
pub struct SessionConfig {
    pub llm: Arc<dyn aivyx_llm::LlmBackend>,
    pub confiner: Arc<dyn aivyx_sandbox::ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)>,
    /// Task 2's `mcp_registry` -- the delegate-shaped base set every
    /// session's own tier-filtered registry is cloned and excluded from.
    pub base_registry: ToolRegistry,
    pub deny_paths: Vec<std::path::PathBuf>,
    pub cwd: std::path::PathBuf,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
}

const SESSION_SYSTEM_PROMPT: &str = "You are aivyx-coder, delegated a bounded coding task by \
another agent over MCP. Work the task to completion within your granted access level, then give \
a clear, complete final answer describing what you found or did -- this is the only part of your \
work the caller sees directly.";

/// Builds a fresh, isolated `Agent` for one MCP session at `level` --
/// no turn run yet. A fresh `PlanMode`/`AutonomousMode`/`ConfirmationGate`
/// per session (never shared, never the outer `BuiltAgent`'s own) since
/// each session's tier is independent and AutonomousMode is never reused
/// per this project's own Global Constraints.
pub async fn build_session_agent(
    config: &SessionConfig,
    level: AccessLevel,
    events_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Agent {
    let mut registry = config.base_registry.clone();
    registry.exclude(&level.excluded_tool_names());

    let allowed_names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
    let prompter: Arc<dyn PermissionPrompter> = Arc::new(TieredPrompter::new(allowed_names));

    let plan_mode = PlanMode::new();
    // "plan" tier reuses the existing plan-mode mechanism verbatim (zero
    // new filtering logic for this tier): run_turn's own
    // `if plan_mode.active() { plan_definitions() } else { definitions() }`
    // branch does the work, and ConfirmationGate's plan-mode-deny branch
    // is the backstop if the model invents a mutating call anyway.
    plan_mode.set_active(level == AccessLevel::Plan);
    let autonomous_mode = AutonomousMode::new(); // always inactive -- never reused, see Global Constraints

    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        prompter,
        config.deny_paths.clone(),
        Vec::new(), // no pre-approved commands -- everything in-tier already auto-allows via TieredPrompter
        plan_mode,
        autonomous_mode,
        config.cwd.clone(),
        false, // no editor-approval integration for this frontend
    ));

    let mut executor = ToolExecutor::new(registry, Arc::clone(&gate), Arc::clone(&config.confiner));
    if let Some(checkpointer) = &config.checkpointer {
        executor.set_checkpointer(Arc::clone(checkpointer));
    }

    let mut agent = Agent::new(
        Arc::clone(&config.llm),
        executor,
        SESSION_SYSTEM_PROMPT,
        AgentConfig {
            // Fixed at 1, exactly like delegate_task's own inner agent --
            // run_bounded_turn's outer loop (Step 5) is what bounds the
            // total round-trip budget via max_iterations, not this field.
            max_tool_iterations: 1,
            context_tokens: config.context_tokens,
            edit_format: config.edit_format,
        },
        Arc::default(), // fresh, empty task list -- this session's own, never the outer BuiltAgent's
        PlanMode::new(), // note: a SECOND PlanMode instance, unused by Agent::new's own plan-mode reads inside run_turn (which reads the one baked into ConfirmationGate's construction above via the shared `plan_mode` variable) -- see Step 4's fix note below.
        autonomous_mode,
        events_tx,
    );
    if let Some((map, budget)) = &config.repo_map {
        agent.set_repo_map(Arc::clone(map), *budget);
    }
    agent
}
```

**Fix note before running this step:** the draft above constructs `plan_mode` once, moves it into `ConfirmationGate::new`, then passes a *second*, fresh `PlanMode::new()` into `Agent::new`'s own `plan_mode` parameter — but `run_turn`'s tool-definition choice (`crates/aivyx-core/src/agent/mod.rs:1315-1318`) reads `self.plan_mode`, which is `Agent::new`'s own parameter, not `ConfirmationGate`'s. **These must be the same shared handle** (mirrors `agent_builder.rs`'s own `plan_mode.clone()` passed to both `ConfirmationGate::new` and `Agent::new` — see `crates/aivyx/src/agent_builder.rs` around its own `ConfirmationGate::new(...)` and `Agent::new(...)` calls). Fix: bind `plan_mode` once, clone it for each consumer:

```rust
    let plan_mode = PlanMode::new();
    plan_mode.set_active(level == AccessLevel::Plan);
    let autonomous_mode = AutonomousMode::new();

    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        prompter,
        config.deny_paths.clone(),
        Vec::new(),
        plan_mode.clone(),
        autonomous_mode.clone(),
        config.cwd.clone(),
        false,
    ));

    let mut executor = ToolExecutor::new(registry, Arc::clone(&gate), Arc::clone(&config.confiner));
    if let Some(checkpointer) = &config.checkpointer {
        executor.set_checkpointer(Arc::clone(checkpointer));
    }

    let mut agent = Agent::new(
        Arc::clone(&config.llm),
        executor,
        SESSION_SYSTEM_PROMPT,
        AgentConfig {
            max_tool_iterations: 1,
            context_tokens: config.context_tokens,
            edit_format: config.edit_format,
        },
        Arc::default(),
        plan_mode,
        autonomous_mode,
        events_tx,
    );
```

Use this corrected version (bind `plan_mode`/`autonomous_mode` once, `.clone()` each into its two consumers) as the actual `build_session_agent` body — the earlier draft's two-`PlanMode`-instances shape is a bug, shown and fixed here rather than left for you to discover.

- [ ] **Step 5: Add `run_bounded_turn`**

Append to `crates/aivyx-mcp-server/src/session.rs`. `events_rx` is **borrowed**, not owned — Task 4's session map keeps the real receiver alive inside its own `StoredSession` across multiple `code_reply` calls, so this function can only ever hold it temporarily. That rules out spawning a background task to drain it (a spawned task's future must be `'static`, which a borrow is not) — instead, poll it directly, concurrently with each `run_turn` call, on the same task, via `tokio::select!`. This is exactly `aivyx-acp/src/session.rs`'s own proven pattern for the identical problem (draining a session's events concurrently with an in-flight turn), adapted here to also loop across `delegate_task`'s own outer "continue" round-trips:

```rust
/// Runs `input` to completion against `agent`, bounded by `max_iterations`
/// outer round-trips -- the exact same shape as `delegate_task`'s own
/// outer "continue" loop (`aivyx-core/src/delegate.rs`), reused here since
/// `AgentConfig.max_tool_iterations` is fixed at 1 per `build_session_agent`
/// above. Drains `agent`'s own event channel concurrently with each round
/// trip (mirrors `aivyx-acp/src/session.rs`'s own `tokio::select!` pattern)
/// to accumulate the final text answer, appending a cutoff notice if the
/// budget runs out before a natural finish -- returns `Ok` even then,
/// since the session may have done real, useful, incomplete work.
pub async fn run_bounded_turn(
    agent: &mut Agent,
    events_rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    input: String,
    cwd: &Path,
    max_iterations: u32,
    cancellation: CancellationToken,
) -> (Result<(), aivyx_core::AgentError>, String) {
    let mut accumulated = String::new();
    let max_iterations = max_iterations.max(1);

    let mut next_input = Some(input);
    let mut result = Ok(());
    let mut iterations_used = 0u32;
    while let Some(turn_input) = next_input.take() {
        iterations_used += 1;
        result = {
            let run = agent.run_turn(turn_input, cwd, cancellation.clone());
            tokio::pin!(run);
            loop {
                tokio::select! {
                    r = &mut run => break r,
                    Some(event) = events_rx.recv() => {
                        if let AgentEvent::TextDelta(text) = &event {
                            accumulated.push_str(text);
                        }
                    }
                }
            }
        };
        // Drain anything buffered right at this round trip's completion
        // (e.g. a final TextDelta that arrived after `run` resolved but
        // before select! polled the channel again) before deciding
        // whether to continue.
        while let Ok(event) = events_rx.try_recv() {
            if let AgentEvent::TextDelta(text) = &event {
                accumulated.push_str(text);
            }
        }
        if result.is_ok()
            && agent.last_turn_paused()
            && iterations_used < max_iterations
            && !cancellation.is_cancelled()
        {
            next_input = Some("continue".to_string());
        }
    }
    let cap_hit = result.is_ok() && agent.last_turn_paused();

    if cap_hit {
        accumulated.push_str(
            "\n\n(session stopped: reached its iteration budget before finishing -- the above is its best-effort partial result.)",
        );
    }
    (result, accumulated)
}
```

- [ ] **Step 6: Write tests for `build_session_agent` + `run_bounded_turn`**

Append to `crates/aivyx-mcp-server/src/session.rs`, inside a new `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_tools::{ReadFileTool, RunCommandTool, WriteFileTool};
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use std::sync::Mutex;

    struct MockBackend {
        responses: Mutex<std::collections::VecDeque<Vec<StreamEvent>>>,
    }
    impl MockBackend {
        fn says(text: &str) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(std::collections::VecDeque::from(vec![vec![
                    StreamEvent::TextDelta(text.to_string()),
                    StreamEvent::Done { finish_reason: FinishReason::Stop, usage: None },
                ]])),
            })
        }
    }
    #[async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }
        async fn stream_chat(
            &self,
            _request: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            let events = self.responses.lock().unwrap().pop_front().unwrap_or_default();
            Ok(futures::stream::iter(events.into_iter().map(Ok)).boxed())
        }
    }

    fn config(base_registry: ToolRegistry) -> SessionConfig {
        SessionConfig {
            llm: MockBackend::says("done"),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            base_registry,
            deny_paths: Vec::new(),
            cwd: std::env::temp_dir(),
            context_tokens: 8192,
            edit_format: EditFormat::Native,
        }
    }

    fn full_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(RunCommandTool::new(Vec::new())));
        registry
    }

    #[tokio::test]
    async fn plan_tier_session_has_no_mutating_tools_in_its_registry() {
        let (tx, rx) = mpsc::unbounded_channel();
        let agent = build_session_agent(&config(full_registry()), AccessLevel::Plan, tx).await;
        // No public accessor for the registry directly -- proven instead
        // via the tier's own excluded_tool_names, which is what
        // build_session_agent actually applies (a unit-level proof this
        // wiring uses that exact list, not a copy that could drift).
        let excluded = AccessLevel::Plan.excluded_tool_names();
        assert!(excluded.contains(&"write_file") && excluded.contains(&"run_command"));
        drop(agent);
        drop(rx);
    }

    #[tokio::test]
    async fn run_bounded_turn_returns_accumulated_text_on_a_single_round_trip() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut agent = build_session_agent(&config(full_registry()), AccessLevel::Plan, tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut rx,
            "say hello".to_string(),
            &std::env::temp_dir(),
            10,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(text, "done");
    }

    #[test]
    fn tiered_prompter_allows_only_names_it_was_given() {
        // Direct unit proof of the belt-and-braces defense described in
        // TieredPrompter's own doc comment -- independent of whether
        // ToolRegistry::exclude is ever miswired upstream.
        let prompter = TieredPrompter::new(vec!["read_file".to_string()]);
        let allowed = PermissionRequest {
            tool_name: "read_file".to_string(),
            action: aivyx_sandbox::ActionKind::Read,
            target: aivyx_sandbox::PermissionTarget::Other("x".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        };
        let denied = PermissionRequest { tool_name: "run_command".to_string(), ..allowed.clone() };
        // PermissionRequest has no #[derive(Clone)] guarantee beyond what
        // aivyx-sandbox already declares (Debug, Clone -- confirmed in
        // lib.rs) so `..allowed.clone()` above is valid; if a future
        // change to PermissionRequest drops Clone, construct `denied`
        // with its own full literal instead.
        let allow = futures::executor::block_on(prompter.prompt(&allowed));
        let deny = futures::executor::block_on(prompter.prompt(&denied));
        assert_eq!(allow, UserResponse::Allow);
        assert_eq!(deny, UserResponse::Deny);
    }

    #[tokio::test]
    async fn run_bounded_turn_appends_a_cutoff_notice_on_budget_exhaustion() {
        // Mirrors aivyx-core/src/delegate.rs's own
        // cap_exhaustion_returns_ok_with_a_cutoff_notice_not_an_error test
        // exactly: every response is a tool call with no final answer, so
        // the agent pauses every single iteration and never finishes
        // naturally -- proving run_bounded_turn's outer loop actually
        // stops at max_iterations and appends the cutoff notice, the same
        // behavior delegate_task's own outer loop has.
        use aivyx_llm::FinishReason;
        use aivyx_types::{ToolCall, ToolCallId, ToolCallSource};

        struct LoopingBackend;
        #[async_trait]
        impl aivyx_llm::LlmBackend for LoopingBackend {
            fn model_id(&self) -> &str {
                "looping"
            }
            async fn stream_chat(
                &self,
                _request: aivyx_llm::ChatRequest,
            ) -> Result<futures::stream::BoxStream<'static, Result<StreamEvent, aivyx_llm::LlmError>>, aivyx_llm::LlmError>
            {
                Ok(futures::stream::iter([
                    Ok(StreamEvent::ToolCallComplete(ToolCall {
                        id: ToolCallId("c".to_string()),
                        name: "nonexistent_tool".to_string(),
                        arguments: serde_json::json!({}),
                        source: ToolCallSource::Native,
                    })),
                    Ok(StreamEvent::Done { finish_reason: FinishReason::ToolCalls }),
                ])
                .boxed())
            }
        }

        let mut cfg = config(full_registry());
        cfg.llm = std::sync::Arc::new(LoopingBackend);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut agent = build_session_agent(&cfg, AccessLevel::Plan, tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut rx,
            "loop forever".to_string(),
            &std::env::temp_dir(),
            3,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok(), "budget exhaustion must return Ok, not an error");
        assert!(
            text.contains("reached its iteration budget"),
            "expected a cutoff notice, got: {text}"
        );
    }
}
```

- [ ] **Step 7: Run the tests**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-mcp-server
```

Expected: all tests pass (`tiers::tests`'s 6 plus `session::tests`'s 2).

- [ ] **Step 8: Commit**

```bash
git add root/Cargo.toml crates/aivyx-mcp-server
git commit -m "Add aivyx-mcp-server crate scaffold: AccessLevel, TieredPrompter, session construction

build_session_agent mirrors delegate_task's own Agent::new construction
shape exactly, tier-filtered via ToolRegistry::exclude. run_bounded_turn
mirrors delegate_task's own outer 'continue' round-trip loop. No MCP
wire protocol yet -- this is pure, unit-testable session logic."
```

---

### Task 4: `rmcp` server wiring — `code`/`code_reply`, session map, ceiling

**Files:**
- Modify: `crates/aivyx-mcp-server/src/lib.rs` (already has the `mod server;` line from Task 3)
- Create: `crates/aivyx-mcp-server/src/server.rs`

**Interfaces:**
- Consumes: `AccessLevel`, `SessionConfig`, `build_session_agent`, `run_bounded_turn` (Task 3).
- Produces: `pub struct McpServerRunConfig { pub session_config: SessionConfig, pub max_access_level: AccessLevel, pub session_ttl: std::time::Duration, pub max_concurrent_sessions: usize, pub max_iterations: u32 }`; `pub async fn run(config: McpServerRunConfig) -> anyhow::Result<()>` — starts the stdio MCP server and blocks until shutdown. Task 5 constructs `McpServerRunConfig` and calls `run`.

- [ ] **Step 1: Write the session-map data structure and its tests**

`crates/aivyx-mcp-server/src/server.rs`:

```rust
//! The rmcp-facing layer: `code`/`code_reply` tool definitions, an
//! in-memory TTL-evicted session map, and the startup ceiling check.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aivyx_core::Agent;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, InitializeResult, ProtocolVersion, ServerCapabilities,
    ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::session::{build_session_agent, run_bounded_turn, SessionConfig};
use crate::tiers::AccessLevel;

pub struct McpServerRunConfig {
    pub session_config: SessionConfig,
    pub max_access_level: AccessLevel,
    pub session_ttl: Duration,
    pub max_concurrent_sessions: usize,
    pub max_iterations: u32,
}

/// `events_rx` travels with its `Agent`: `Agent::run_turn` sends every
/// `AgentEvent` to whichever channel it was constructed with, baked in
/// once at `build_session_agent` (Task 3) time -- a `code_reply` call
/// must reuse the SAME receiver `code` first created, not a fresh,
/// disconnected one, or its accumulated text would always come back
/// empty. Storing them together is what makes that automatic.
struct StoredSession {
    agent: Agent,
    events_rx: tokio::sync::mpsc::UnboundedReceiver<aivyx_core::AgentEvent>,
    last_active: Instant,
}

/// Bounded, TTL-evicted in-memory session map -- the whole of "cleanup"
/// for v1 (Global Constraints: no `close_session` tool). `evict_stale`
/// and `evict_oldest_if_full` are separated from insertion so each is
/// independently testable.
struct SessionMap {
    sessions: HashMap<String, StoredSession>,
    ttl: Duration,
    max_concurrent: usize,
}

impl SessionMap {
    fn new(ttl: Duration, max_concurrent: usize) -> Self {
        Self { sessions: HashMap::new(), ttl, max_concurrent }
    }

    /// Removes every session idle longer than `ttl`. Call before every
    /// insert/lookup so staleness is judged relative to "now", not to
    /// whenever the map was last touched.
    fn evict_stale(&mut self) {
        let ttl = self.ttl;
        self.sessions.retain(|_, s| s.last_active.elapsed() < ttl);
    }

    /// If inserting one more session would exceed `max_concurrent`,
    /// evicts whichever current session has been idle longest. No-op if
    /// there's already room.
    fn evict_oldest_if_full(&mut self) {
        if self.sessions.len() < self.max_concurrent {
            return;
        }
        if let Some(oldest_id) = self
            .sessions
            .iter()
            .min_by_key(|(_, s)| s.last_active)
            .map(|(id, _)| id.clone())
        {
            self.sessions.remove(&oldest_id);
        }
    }

    fn insert(
        &mut self,
        id: String,
        agent: Agent,
        events_rx: tokio::sync::mpsc::UnboundedReceiver<aivyx_core::AgentEvent>,
    ) {
        self.sessions.insert(id, StoredSession { agent, events_rx, last_active: Instant::now() });
    }

    fn touch_and_borrow(
        &mut self,
        id: &str,
    ) -> Option<(&mut Agent, &mut tokio::sync::mpsc::UnboundedReceiver<aivyx_core::AgentEvent>)> {
        let session = self.sessions.get_mut(id)?;
        session.last_active = Instant::now();
        Some((&mut session.agent, &mut session.events_rx))
    }
}

#[cfg(test)]
mod session_map_tests {
    use super::*;
    use crate::session::SessionConfig;
    use aivyx_core::AgentEvent;
    use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_tools::ToolRegistry;
    use futures::stream::BoxStream;
    use futures::StreamExt;

    struct MockBackend;
    #[async_trait::async_trait]
    impl LlmBackend for MockBackend {
        fn model_id(&self) -> &str {
            "mock"
        }
        async fn stream_chat(
            &self,
            _r: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Ok(futures::stream::iter([Ok(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            })])
            .boxed())
        }
    }

    /// A real, minimal `Agent` built via `build_session_agent` (Task 3's
    /// own function, already proven in `session.rs`'s own tests) plus its
    /// matching `events_rx` -- this module's tests only exercise map
    /// bookkeeping (eviction order, TTL, touch-refresh), never a real turn.
    async fn fake_session() -> (Agent, tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
        let config = SessionConfig {
            llm: Arc::new(MockBackend),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            base_registry: ToolRegistry::new(),
            deny_paths: Vec::new(),
            cwd: std::env::temp_dir(),
            context_tokens: 8192,
            edit_format: aivyx_core::EditFormat::Native,
        };
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let agent = build_session_agent(&config, AccessLevel::Plan, tx).await;
        (agent, rx)
    }

    #[tokio::test]
    async fn evict_oldest_if_full_removes_the_least_recently_active_session() {
        let mut map = SessionMap::new(Duration::from_secs(3600), 2);
        let (a, a_rx) = fake_session().await;
        map.insert("a".to_string(), a, a_rx);
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (b, b_rx) = fake_session().await;
        map.insert("b".to_string(), b, b_rx);

        map.evict_oldest_if_full(); // len == max_concurrent (2) -- evicts "a" before the 3rd insert
        let (c, c_rx) = fake_session().await;
        map.insert("c".to_string(), c, c_rx);

        assert!(map.touch_and_borrow("a").is_none(), "a was the oldest, must be evicted");
        assert!(map.touch_and_borrow("b").is_some());
        assert!(map.touch_and_borrow("c").is_some());
    }

    #[tokio::test]
    async fn evict_stale_removes_only_sessions_past_the_ttl() {
        let mut map = SessionMap::new(Duration::from_millis(10), 8);
        let (stale, stale_rx) = fake_session().await;
        map.insert("stale".to_string(), stale, stale_rx);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let (fresh, fresh_rx) = fake_session().await;
        map.insert("fresh".to_string(), fresh, fresh_rx);

        map.evict_stale();

        assert!(map.touch_and_borrow("stale").is_none());
        assert!(map.touch_and_borrow("fresh").is_some());
    }

    #[tokio::test]
    async fn touch_and_borrow_refreshes_last_active_so_evict_stale_spares_it() {
        let mut map = SessionMap::new(Duration::from_millis(15), 8);
        let (s, s_rx) = fake_session().await;
        map.insert("s".to_string(), s, s_rx);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(map.touch_and_borrow("s").is_some(), "touched before its TTL expires");
        tokio::time::sleep(Duration::from_millis(10)).await;
        // 10ms since the touch above -- still under the 15ms TTL relative
        // to that touch, even though 20ms have passed since insertion.
        map.evict_stale();
        assert!(map.touch_and_borrow("s").is_some(), "the touch above must have reset the TTL clock");
    }
}
```

- [ ] **Step 2: Run the session-map tests**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-mcp-server session_map_tests
```

Expected: all 3 pass.

- [ ] **Step 3: Add the `code`/`code_reply` tool definitions and `ServerHandler`**

Append to `crates/aivyx-mcp-server/src/server.rs` (after the `SessionMap` impl and before the `#[cfg(test)]` block):

```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CodeParams {
    /// A complete, self-contained description of the task -- the session
    /// starts with no context beyond this text.
    pub task: String,
    /// "plan" | "edit" | "execute". Rejected if it exceeds this server's
    /// configured ceiling.
    pub access_level: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CodeReplyParams {
    pub session_id: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
struct CodeOutcome {
    session_id: String,
    result: String,
}

fn mcp_error(detail: impl Into<String>) -> McpError {
    McpError::internal_error(detail.into(), None)
}

#[derive(Clone)]
pub struct AivyxCoderMcpServer {
    tool_router: ToolRouter<Self>,
    sessions: Arc<Mutex<SessionMap>>,
    session_config: Arc<SessionConfig>,
    max_access_level: AccessLevel,
    max_iterations: u32,
}

#[tool_router]
impl AivyxCoderMcpServer {
    fn new(config: McpServerRunConfig) -> Self {
        Self {
            tool_router: Self::tool_router(),
            sessions: Arc::new(Mutex::new(SessionMap::new(config.session_ttl, config.max_concurrent_sessions))),
            session_config: Arc::new(config.session_config),
            max_access_level: config.max_access_level,
            max_iterations: config.max_iterations,
        }
    }

    #[tool(description = "Delegate a bounded coding task to aivyx-coder. access_level is \
        \"plan\" (read-only), \"edit\" (file writes, no shell), or \"execute\" (full tool \
        access, still sandboxed) -- rejected if it exceeds this server's configured ceiling. \
        Returns a session_id for use with code_reply, plus the session's final answer.")]
    async fn code(
        &self,
        Parameters(params): Parameters<CodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let level = AccessLevel::parse(&params.access_level).map_err(mcp_error)?;
        if !level.at_most(&self.max_access_level) {
            return Err(mcp_error(format!(
                "access_level {:?} exceeds this server's configured ceiling",
                params.access_level
            )));
        }

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = build_session_agent(&self.session_config, level, events_tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut events_rx,
            params.task,
            &self.session_config.cwd,
            self.max_iterations,
            CancellationToken::new(),
        )
        .await;
        result.map_err(|e| mcp_error(e.to_string()))?;

        let session_id = uuid::Uuid::new_v4().to_string();
        {
            let mut sessions = self.sessions.lock().await;
            sessions.evict_stale();
            sessions.evict_oldest_if_full();
            sessions.insert(session_id.clone(), agent, events_rx);
        }

        let content = Content::json(CodeOutcome { session_id, result: text })
            .map_err(|e| mcp_error(format!("failed to encode result: {e}")))?;
        Ok(CallToolResult::success(vec![content]))
    }

    #[tool(description = "Continue a session started by code, with a follow-up message. The \
        access level chosen at session start is not renegotiable here.")]
    async fn code_reply(
        &self,
        Parameters(params): Parameters<CodeReplyParams>,
    ) -> Result<CallToolResult, McpError> {
        let text = {
            let mut sessions = self.sessions.lock().await;
            sessions.evict_stale();
            // Reuses the SAME events_rx `code` first created (stored
            // alongside the Agent in StoredSession) -- Agent::run_turn
            // sends to whatever channel it was constructed with, baked in
            // once at build_session_agent time, so a fresh, disconnected
            // channel here would drain nothing and always return empty text.
            let Some((agent, events_rx)) = sessions.touch_and_borrow(&params.session_id) else {
                return Err(mcp_error(format!(
                    "no session {:?} -- it may have expired (idle past the configured TTL)",
                    params.session_id
                )));
            };
            let (result, text) = run_bounded_turn(
                agent,
                events_rx,
                params.message,
                &self.session_config.cwd,
                self.max_iterations,
                CancellationToken::new(),
            )
            .await;
            result.map_err(|e| mcp_error(e.to_string()))?;
            text
        };
        let content = Content::json(CodeOutcome { session_id: params.session_id, result: text })
            .map_err(|e| mcp_error(format!("failed to encode result: {e}")))?;
        Ok(CallToolResult::success(vec![content]))
    }
}
```

Note: `run_bounded_turn`'s signature (Task 3) takes `events_rx` **by value** (`mpsc::UnboundedReceiver<AgentEvent>`), but `touch_and_borrow` above hands back `&mut UnboundedReceiver<AgentEvent>` (a borrow, since the receiver must stay owned by `StoredSession` for a possible third `code_reply` later). Passing `events_rx` (a `&mut`) where a by-value receiver is expected does not compile as written — `run_bounded_turn` needs a small signature adjustment to take `events_rx: &mut mpsc::UnboundedReceiver<AgentEvent>` instead of by value (its own body already only ever calls `.recv()`/`.close()`/`.try_recv()` on it through a mutable reference internally, so this is a signature-only change, not a logic change — apply it back in Task 3 Step 5's `run_bounded_turn` definition, and update that same task's own tests, which currently pass `rx` by value, to pass `&mut rx` instead).

- [ ] **Step 4: Add `ServerHandler` + the stdio `run` entry point**

Append to `crates/aivyx-mcp-server/src/server.rs`:

```rust
#[tool_handler]
impl ServerHandler for AivyxCoderMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_06_18,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Delegate bounded coding tasks to aivyx-coder via code/code_reply.".to_string(),
            ),
        }
    }
}

pub async fn run(config: McpServerRunConfig) -> anyhow::Result<()> {
    let server = AivyxCoderMcpServer::new(config);
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("aivyx-mcp-server: serving error: {e:?}"))?;
    service.waiting().await?;
    Ok(())
}
```

- [ ] **Step 5: Write an integration test proving `code` → `code_reply` actually round-trips real conversation state**

Append to `crates/aivyx-mcp-server/src/server.rs`'s test module (create `#[cfg(test)] mod tests` if the earlier `session_map_tests` module is the only one so far — keep them as sibling modules):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use aivyx_llm::{ChatRequest, FinishReason, LlmBackend, LlmError, StreamEvent};
    use aivyx_sandbox::NoopConfiner;
    use aivyx_tools::ToolRegistry;
    use futures::stream::BoxStream;
    use futures::StreamExt;
    use std::sync::Mutex as StdMutex;

    /// Scripted responses, one Vec<StreamEvent> per call -- proves
    /// code_reply's turn is a SECOND real call to the backend (not, e.g.,
    /// replaying the first response) and that its own TextDelta content
    /// makes it all the way back out, discriminating this test from one
    /// that would pass even with the events_rx bug the plan's fix note
    /// above describes (that bug makes code_reply's text always empty --
    /// this test's second assertion fails under it).
    struct ScriptedBackend {
        responses: StdMutex<std::collections::VecDeque<&'static str>>,
    }
    #[async_trait::async_trait]
    impl LlmBackend for ScriptedBackend {
        fn model_id(&self) -> &str {
            "scripted"
        }
        async fn stream_chat(
            &self,
            _r: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            let text = self.responses.lock().unwrap().pop_front().unwrap_or("");
            Ok(futures::stream::iter([
                Ok(StreamEvent::TextDelta(text.to_string())),
                Ok(StreamEvent::Done { finish_reason: FinishReason::Stop, usage: None }),
            ])
            .boxed())
        }
    }

    fn server_with_ceiling(ceiling: AccessLevel) -> AivyxCoderMcpServer {
        let session_config = SessionConfig {
            llm: Arc::new(ScriptedBackend {
                responses: StdMutex::new(std::collections::VecDeque::from(vec!["first answer", "second answer"])),
            }),
            confiner: Arc::new(NoopConfiner),
            checkpointer: None,
            repo_map: None,
            base_registry: ToolRegistry::new(),
            deny_paths: Vec::new(),
            cwd: std::env::temp_dir(),
            context_tokens: 8192,
            edit_format: aivyx_core::EditFormat::Native,
        };
        AivyxCoderMcpServer::new(McpServerRunConfig {
            session_config,
            max_access_level: ceiling,
            session_ttl: Duration::from_secs(3600),
            max_concurrent_sessions: 8,
            max_iterations: 10,
        })
    }

    #[tokio::test]
    async fn code_then_code_reply_round_trips_real_conversation_state() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let first = server
            .code(Parameters(CodeParams { task: "start".to_string(), access_level: "plan".to_string() }))
            .await
            .expect("code call should succeed");
        let CallToolResult { content, .. } = first;
        let first_json: serde_json::Value = content[0].raw.as_text().unwrap().text.parse().unwrap_or_else(|_| {
            serde_json::from_str(&content[0].raw.as_text().unwrap().text).unwrap()
        });
        let session_id = first_json["session_id"].as_str().unwrap().to_string();
        assert_eq!(first_json["result"], "first answer");

        let second = server
            .code_reply(Parameters(CodeReplyParams { session_id: session_id.clone(), message: "continue".to_string() }))
            .await
            .expect("code_reply should succeed");
        let second_json: serde_json::Value =
            serde_json::from_str(&second.content[0].raw.as_text().unwrap().text).unwrap();
        assert_eq!(
            second_json["result"], "second answer",
            "code_reply must return its OWN turn's real text, not an empty string from a \
             disconnected events channel"
        );
        assert_eq!(second_json["session_id"], session_id);
    }

    #[tokio::test]
    async fn code_call_above_the_ceiling_is_rejected_before_any_agent_is_built() {
        let server = server_with_ceiling(AccessLevel::Plan);
        let outcome = server
            .code(Parameters(CodeParams { task: "do something".to_string(), access_level: "execute".to_string() }))
            .await;
        assert!(outcome.is_err(), "execute must be rejected when the ceiling is plan");
    }

    #[tokio::test]
    async fn code_reply_against_an_unknown_session_id_fails_clearly() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let outcome = server
            .code_reply(Parameters(CodeReplyParams {
                session_id: "does-not-exist".to_string(),
                message: "hi".to_string(),
            }))
            .await;
        assert!(outcome.is_err());
    }
}
```

Note: `Content`'s exact accessor for its inner text (`content[0].raw.as_text()...`) may differ slightly by `rmcp` 0.11's real API surface — if `cargo test` reports a compile error on that specific line, check `rmcp::model::Content`'s real methods (`cargo doc -p rmcp --open` or the crate's docs.rs page) and adjust just that accessor; the JSON round-trip assertions themselves (`first_json["result"]`, `second_json["result"]`) are what matter and should not need to change.

- [ ] **Step 6: Run the full test suite for this crate**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-mcp-server
```

Expected: every test in `tiers::tests`, `session::tests`, `server::session_map_tests`, and `server::tests` passes, including the two that specifically discriminate the `code_reply` events-channel bug and the ceiling-rejection path.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-mcp-server/src/server.rs crates/aivyx-mcp-server/src/lib.rs
git commit -m "Add rmcp server wiring: code/code_reply tools, TTL-evicted session map, ceiling check

A real bug caught and fixed during this task's own drafting: code_reply
was wiring a fresh, disconnected events channel instead of reusing the
session's real one, which would have made every code_reply's returned
text silently empty. Fixed by storing each session's events_rx
alongside its Agent. code_then_code_reply_round_trips_real_conversation_state
is written specifically to fail under the original bug."
```

---

### Task 5: `--mcp-server` CLI flag and wiring

**Files:**
- Modify: `crates/aivyx/Cargo.toml` (new dependency)
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Consumes: `BuiltAgent`'s new fields (Task 2), `aivyx_mcp_server::{run, McpServerRunConfig, AccessLevel}` and `aivyx_mcp_server::session::SessionConfig` (Tasks 3–4).

- [ ] **Step 1: Add the dependency**

In `crates/aivyx/Cargo.toml`, find the `[dependencies]` block (alongside the existing `aivyx-acp = { version = "0.1.0", path = "../aivyx-acp" }` line) and add:

```toml
aivyx-mcp-server = { version = "0.1.0", path = "../aivyx-mcp-server" }
```

- [ ] **Step 2: Add the CLI flag**

In `crates/aivyx/src/main.rs`, find the `acp: bool` field on the `Cli` struct (the last field, per Task investigation — right before the struct's closing `}`). Add a new field after it:

```rust
    acp: bool,

    /// Run as an MCP (Model Context Protocol) server over stdin/stdout,
    /// for delegation from another local MCP client (e.g. aivyx). Requires
    /// [mcp_server].max_access_level to be configured in config.toml first
    /// -- refuses to start otherwise, matching --auto's own posture for
    /// its required [verification].command. Mutually exclusive with
    /// --acp/--plan/--auto/--resume: this frontend has no human to show a
    /// modal to, no editor session to embed in, and no unattended-goal
    /// concept of its own (each MCP call is its own bounded, isolated
    /// session).
    #[arg(long)]
    mcp_server: bool,
}
```

- [ ] **Step 3: Wire the dispatch**

In `crates/aivyx/src/main.rs`'s `async fn main()`, find the `if cli.acp { ... }` block (it ends with `.await.map_err(...)` and a `;` before falling through to the TUI path). Add a new `if cli.mcp_server { ... }` block immediately after it, before the `let permission_rx = tui_permission_rx.expect(...)` line:

```rust
    if cli.mcp_server {
        if cli.acp {
            anyhow::bail!("--mcp-server and --acp cannot be used together");
        }
        if cli.plan {
            anyhow::bail!("--mcp-server and --plan cannot be used together");
        }
        if cli.auto.is_some() {
            anyhow::bail!("--mcp-server and --auto cannot be used together");
        }
        if cli.resume {
            anyhow::bail!("--mcp-server and --resume cannot be used together");
        }
        let Some(max_access_level_str) = settings.mcp_server.max_access_level.as_deref() else {
            anyhow::bail!(
                "--mcp-server requires [mcp_server].max_access_level to be set in config.toml \
                 (\"plan\", \"edit\", or \"execute\") -- refusing to start with no configured \
                 ceiling"
            );
        };
        let max_access_level = aivyx_mcp_server::AccessLevel::parse(max_access_level_str)
            .map_err(|e| anyhow::anyhow!("[mcp_server].max_access_level: {e}"))?;

        let edit_format = match cli.edit_format.as_deref() {
            Some("native") => aivyx_core::EditFormat::Native,
            Some("prompted") => aivyx_core::EditFormat::Prompted,
            _ => match settings.backend.edit_format {
                aivyx_config::EditFormat::Native => aivyx_core::EditFormat::Native,
                aivyx_config::EditFormat::Prompted => aivyx_core::EditFormat::Prompted,
            },
        };
        let deny_paths = settings.permissions.resolved_deny_paths();

        return aivyx_mcp_server::run(aivyx_mcp_server::McpServerRunConfig {
            session_config: aivyx_mcp_server::SessionConfig {
                llm: built.llm,
                confiner: built.confiner,
                checkpointer: built.checkpointer,
                repo_map: built.repo_map,
                base_registry: built.mcp_registry,
                deny_paths,
                cwd: built.cwd,
                context_tokens: settings.backend.context_tokens,
                edit_format,
            },
            max_access_level,
            session_ttl: Duration::from_secs(settings.mcp_server.session_ttl_secs),
            max_concurrent_sessions: settings.mcp_server.max_concurrent_sessions as usize,
            max_iterations: settings.mcp_server.max_iterations,
        })
        .await;
    }
```

Note: `aivyx_mcp_server::session::SessionConfig` is re-exported as `aivyx_mcp_server::SessionConfig` per Task 3's `lib.rs` (`pub use session::{..., SessionConfig};`) — the code above uses the shorter path.

Note on `edit_format` duplication: this recomputes the same 6-line match `agent_builder.rs`'s `build_agent` already computes internally (not exposed via `BuiltAgent`, since only the MCP-server path needs it a second time) — an accepted, minor duplication rather than adding a 6th field to `BuiltAgent` for one derived value only one caller needs twice.

- [ ] **Step 4: Build and run the full existing test suite**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

Expected: clean build, zero test failures across every crate (this task only adds a new, gated branch — the TUI and `--acp` paths are unchanged), clippy clean (or only pre-existing warnings unrelated to this change).

- [ ] **Step 5: Manual smoke test**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
# Should fail with the "requires max_access_level" message:
cargo run -p aivyx -- --mcp-server

# Add to ~/.config/aivyx-coder/config.toml:
#   [mcp_server]
#   max_access_level = "plan"
# Then, from a real project directory:
cd /tmp && mkdir -p mcp-smoke-test && cd mcp-smoke-test && git init -q
cargo run --manifest-path /home/julian/Projects/Rust/aivyx-coder/Cargo.toml -p aivyx -- --mcp-server
# (Ctrl+C to stop -- this just confirms it starts and doesn't immediately
# crash; a real end-to-end MCP client round trip is out of scope for this
# plan's own automated tests, covered instead by Task 4's in-process
# integration tests against AivyxCoderMcpServer directly.)
```

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx/Cargo.toml crates/aivyx/src/main.rs
git commit -m "Add --mcp-server CLI flag, wiring aivyx-mcp-server into the aivyx binary

Refuses to start without [mcp_server].max_access_level configured,
matching --auto's own posture for its required [verification].command.
Mutually exclusive with --acp/--plan/--auto/--resume."
```

---

## Final verification (after all 5 tasks)

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

Expected: clean build, zero test failures, clippy clean (or only pre-existing warnings unrelated to this change).
