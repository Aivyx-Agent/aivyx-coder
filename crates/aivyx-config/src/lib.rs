use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine a config directory for this platform")]
    NoConfigDir,
    #[error("failed to read config file at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write default config file at {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file at {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to serialize default config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: BackendSettings,
    pub permissions: PermissionSettings,
    pub sandbox: SandboxSettings,
    pub git: GitSettings,
    pub repo_map: RepoMapSettings,
    pub council: CouncilSettings,
    pub architect: ArchitectSettings,
    pub verification: VerificationSettings,
    pub autonomous: AutonomousSettings,
    pub sub_agent: SubAgentSettings,
    pub mcp_server: McpServerSettings,
    pub lsp: LspSettings,
    pub agents_file: AgentsFileSettings,
    pub editor_context: EditorContextSettings,
    pub editor_approval: EditorApprovalSettings,
    pub web: WebSettings,
    pub mcp: McpSettings,
    pub persona: PersonaSettings,
    pub repl: ReplSettings,
}

/// Enforced verification (ROADMAP.md Phase 12 Part B): after file edits,
/// before a turn is allowed to end, auto-run this named
/// `[[permissions.allowed_commands]]` entry (reusing that trust tier, not a
/// new one) and let the model react to the result. `command` absent (the
/// default) disables the feature entirely — setting it alone is the
/// opt-in, deliberately no separate enable flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerificationSettings {
    pub command: Option<String>,
    /// How many (edit, re-verify) cycles may fail before the agent gives up
    /// on this round of edits and lets the turn end anyway, with a loud
    /// notice rather than silence. Clamped to a minimum of 1 by the agent.
    pub max_auto_verify_retries: u32,
    /// Another `[[permissions.allowed_commands]]` entry name, whose `args`
    /// may contain the literal token `"{touched_paths}"` — substituted at
    /// runtime with the files touched since edits became unverified, one
    /// argv entry per path. `None` (the default) means every retry always
    /// runs the full `command`, exactly as before this field existed.
    pub scoped_command: Option<String>,
}

impl Default for VerificationSettings {
    fn default() -> Self {
        Self {
            command: None,
            max_auto_verify_retries: 3,
            scoped_command: None,
        }
    }
}

/// Autonomous mode (`--auto "<goal>"`, ROADMAP.md Phase 11c): hard stops
/// for the unattended loop, independent of and in addition to
/// `permissions.max_tool_iterations_per_turn` (which bounds a single
/// turn's round-trips, not the whole autonomous session). Conservative
/// defaults so a bare `--auto` without further tuning cannot run
/// indefinitely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomousSettings {
    /// Total "continue" round-trips for the whole autonomous run.
    pub max_iterations: u32,
    /// Wall-clock ceiling, in seconds, for the whole autonomous run.
    pub max_duration_secs: u64,
}

impl Default for AutonomousSettings {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_duration_secs: 3600,
        }
    }
}

/// REPL/interactive-process support (`repl_start`/`repl_send`/
/// `repl_stop`): timing knobs for deciding when a call has "enough"
/// output to return, and for auto-killing a forgotten session. All
/// optional with usable defaults — zero-config works out of the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplSettings {
    /// How long output must be silent (no new bytes) before `repl_send`
    /// returns, in milliseconds.
    pub quiet_window_ms: u64,
    /// Hard per-call backstop, in seconds, in case output never goes
    /// quiet (e.g. a build tool spewing output continuously).
    pub max_wait_secs: u64,
    /// Auto-kill a session with no `repl_send` activity for this long, in
    /// seconds — a safety net against a forgotten session lingering
    /// indefinitely.
    pub idle_timeout_secs: u64,
}

impl Default for ReplSettings {
    fn default() -> Self {
        Self {
            quiet_window_ms: 300,
            max_wait_secs: 10,
            idle_timeout_secs: 600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubAgentSettings {
    /// A delegated task's own tool-call budget — deliberately separate
    /// from, and smaller than, `[permissions] max_tool_iterations_per_turn`:
    /// a delegated task is meant to be a scoped, bounded piece of work, not
    /// a full session.
    pub max_iterations: u32,
}

impl Default for SubAgentSettings {
    fn default() -> Self {
        Self { max_iterations: 10 }
    }
}

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

/// LSP integration (`go_to_definition`/`find_references`, ROADMAP.md
/// Phase 9): bounds each JSON-RPC request to the lazily-spawned
/// `rust-analyzer` subprocess. Generous default — cold indexing on the
/// first call in a large workspace can take tens of seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LspSettings {
    pub timeout_secs: u64,
}

impl Default for LspSettings {
    fn default() -> Self {
        Self { timeout_secs: 60 }
    }
}

/// Project-level (and user-global) instructions (`AGENTS.md`, ROADMAP.md
/// Phase 9): auto-loaded into every turn's system prompt, refreshed live so
/// an edit mid-session applies on the next turn without a restart. Off
/// entirely disables both the project (`<cwd>/AGENTS.md`) and user-global
/// (`<config_dir>/AGENTS.md`) files — there is no separate flag per file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsFileSettings {
    pub enabled: bool,
    /// Applied per file independently, not as a combined pool. A file over
    /// budget is still included in full — this is hand-written prose with
    /// no natural truncation point, unlike the repo map — but triggers a
    /// notice so the user knows to trim it or raise this value.
    pub budget_tokens: u32,
}

impl Default for AgentsFileSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_tokens: 1024,
        }
    }
}

/// Live editor context (open file, cursor, selection) — see
/// `aivyx_core::editor_context` and the "Editor context" README section
/// for the JSON file contract. No budget concept, unlike `repo_map` and
/// `agents_file` (a one-line status has no natural truncation point to
/// budget against) — just an enable flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorContextSettings {
    pub enabled: bool,
}

impl Default for EditorContextSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Gates whether `ConfirmationGate` will race a pending permission
/// decision against a possible editor-side answer (see
/// `docs/superpowers/specs/2026-07-19-editor-approval-integration-design.md`).
/// Unlike `EditorContextSettings`, there is no `deny_paths` concept here —
/// `deny_paths` is already enforced upstream of `ConfirmationGate` ever
/// reaching the interactive-prompt tier for a denied target at all.
/// Defaults to `true`, same as `editor_context`: the feature is inert
/// without an active external process writing a response file, so
/// `enabled` alone grants no new capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorApprovalSettings {
    pub enabled: bool,
}

impl Default for EditorApprovalSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Gates whether the agent can propose edits to its own global
/// `AGENTS.md` via the `remember_preference` tool (see
/// docs/superpowers/specs/2026-07-21-agent-learned-preferences-design.md).
/// Defaults to `true` — same reasoning as `EditorApprovalSettings`: every
/// use is still individually gated by `ConfirmationGate`, so the
/// capability alone grants nothing without the model choosing to use it
/// and the user approving that specific call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonaSettings {
    pub enabled: bool,
}

impl Default for PersonaSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// The agent's first network-reaching tools (`web_fetch`/`web_search`,
/// ROADMAP.md Phase 9): "local-only" describes where LLM inference
/// happens, not whether the agent can reach the network — see the
/// 2026-07-15 web-tools design doc for the full reasoning. Both tools are
/// registered only when `enabled`; `web_search` additionally needs
/// `search_base_url` set (a SearXNG instance) to actually function, and
/// explains itself if called before that.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSettings {
    pub enabled: bool,
    pub search_base_url: Option<String>,
    /// Applied by `web_search`; results beyond this are dropped, not an
    /// error.
    pub max_search_results: u32,
    /// Applied to both tools' HTTP client.
    pub fetch_timeout_secs: u64,
    /// When `false` (default), `web_fetch` refuses to connect to a
    /// resolved loopback/private/link-local address. See
    /// `aivyx_tools::web::is_private_or_local`.
    pub allow_private_targets: bool,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            search_base_url: None,
            max_search_results: 10,
            fetch_timeout_secs: 30,
            allow_private_targets: false,
        }
    }
}

/// One configured MCP (Model Context Protocol) server: spawned as a child
/// process, speaking JSON-RPC 2.0 over its stdin/stdout (stdio transport
/// only — see `docs/superpowers/specs/2026-07-16-mcp-support-design.md`'s
/// non-goals for remote/HTTP transport).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServerConfig {
    /// Used verbatim in the registered `mcp__<name>__<tool>` tool name and
    /// in the resources/prompts meta-tools' `server` filter argument.
    pub name: String,
    /// The executable to spawn.
    pub command: String,
    pub args: Vec<String>,
    /// Additional environment variables for the spawned process (e.g. API
    /// keys the server itself needs), merged over the agent's own
    /// environment.
    pub env: HashMap<String, String>,
    /// Bounds this server's spawn + `initialize` handshake + discovery
    /// (`tools/list`/`resources/list`/`prompts/list`) sequence at startup.
    /// A server that doesn't finish within this budget is skipped for the
    /// session with a warning, rather than blocking startup indefinitely.
    pub timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            timeout_secs: 30,
        }
    }
}

/// Wraps `servers` purely so `config.toml` can use `[[mcp.servers]]` array-
/// of-tables syntax rather than a flat `[[mcp_servers]]` at the top level.
/// No top-level `[mcp] enabled` flag: an empty `servers` list is already a
/// complete no-op (no servers to connect to, nothing to register), unlike
/// `[web] enabled`, which had to gate two tools that are otherwise always
/// statically present regardless of configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    pub servers: Vec<McpServerConfig>,
}

/// Council mode (`/council <question>`): the configured members each answer
/// independently, anonymously rank each other's answers, and the chairman
/// synthesizes a recommendation. Members never receive tools, so the
/// feature adds no permission surface. Off until configured: fewer than two
/// members or no chairman means `/council` explains how to enable itself
/// instead of running.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CouncilSettings {
    pub members: Vec<CouncilMember>,
    /// Deliberately no "first member chairs" fallback — which model gets
    /// the last word should always be an explicit choice.
    pub chairman: Option<CouncilMember>,
    /// Token budget for the conversation-tail digest each member sees
    /// alongside the question.
    pub tail_budget_tokens: u32,
}

impl Default for CouncilSettings {
    fn default() -> Self {
        Self {
            members: Vec::new(),
            chairman: None,
            // Enough recent conversation for members to see what's being
            // decided without re-serving the whole window to every model.
            tail_budget_tokens: 3072,
        }
    }
}

impl CouncilSettings {
    /// The council only convenes fully configured — a lone member has
    /// nobody to be ranked against, and without a chairman nothing may
    /// enter the agent's history (per the Phase 11a persistence decision).
    pub fn configured(&self) -> bool {
        self.members.len() >= 2 && self.chairman.is_some()
    }
}

/// One council seat: any OpenAI-compatible local endpoint, so a council can
/// mix Ollama-swapped models with a resident llama-server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouncilMember {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Architect/editor model-pairing (`/architect <task>`, ROADMAP.md Phase 9):
/// a single, separately configured model produces a prose implementation
/// plan, which is then handed directly to the primary/editor model's own
/// turn loop. Off until configured: an empty `base_url` or `model` means
/// `/architect` explains how to enable itself instead of running — no
/// separate enable flag, matching `VerificationSettings`'s convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchitectSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Token budget for the conversation-tail digest the architect sees
    /// alongside the task — separate from `council.tail_budget_tokens`
    /// since the two features are configured and toggled independently.
    pub tail_budget_tokens: u32,
}

impl Default for ArchitectSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key: None,
            tail_budget_tokens: 3072,
        }
    }
}

impl ArchitectSettings {
    pub fn configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoMapSettings {
    /// Append a token-budgeted map of the repository's top-ranked files and
    /// symbols to the system prompt each turn (Rust, Python,
    /// JavaScript/JSX, and TypeScript/TSX today; other languages degrade
    /// gracefully to no map). Costs prompt tokens every request but gives
    /// the model repository orientation it won't ask for on its own.
    pub enabled: bool,
    /// Rough token budget the rendered map may consume. Counted against
    /// `backend.context_tokens` by the compaction estimator.
    pub budget_tokens: u32,
}

impl Default for RepoMapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_tokens: 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitSettings {
    /// Snapshot the worktree to `refs/aivyx/checkpoints/*` before every
    /// mutating tool call, so any agent change (including arbitrary
    /// `run_shell` effects) can be rewound with plain git commands. Never
    /// touches HEAD, the index, or the worktree; silently disabled when the
    /// working directory isn't a git repository.
    pub checkpoints: bool,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self { checkpoints: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxSettings {
    /// Additional filesystem paths process-executing tools may read from,
    /// beyond the built-in default (the working directory plus common
    /// system/toolchain paths). Empty by default — an escape hatch for
    /// ecosystems the built-in list doesn't cover (e.g. a Python venv or
    /// node_modules cache living outside the project directory), not a
    /// fully general reconfigurable policy.
    pub extra_read_paths: Vec<String>,
    /// Whether process-execution tools must refuse to run at all if real
    /// Landlock confinement can't actually be established (kernel support
    /// missing/disabled, or the kernel only partially enforces the
    /// requested ruleset) — fail closed rather than silently running
    /// unconfined. Defaults to `true`: a coding agent whose core value
    /// proposition includes real sandboxing should not silently degrade to
    /// no sandboxing at all without the user explicitly opting into that.
    pub require_enforcement: bool,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            extra_read_paths: Vec::new(),
            require_enforcement: true,
        }
    }
}

impl SandboxSettings {
    /// Expands a leading `~` in each `extra_read_paths` entry — see
    /// `PermissionSettings::resolved_deny_paths` for the same convention.
    pub fn resolved_extra_read_paths(&self) -> Vec<PathBuf> {
        resolve_tilde_paths(&self.extra_read_paths)
    }
}

impl BackendSettings {
    /// The kvcache store directory this run actually uses: the
    /// configured override (tilde-expanded, same convention as
    /// `PermissionSettings::resolved_deny_paths`), or the historical
    /// `ProjectDirs`-derived default when unset. Single source of truth
    /// reused by both the real kvcache construction in `agent_builder.rs`
    /// and `Settings::effective_deny_paths` below -- the two can never
    /// silently diverge.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Parsed and validated now; only exercised once the text-fallback
    /// parser lands (see project plan, milestone M4).
    pub tool_calling_mode: ToolCallingMode,
    /// The model's context window, in tokens — used to show a live budget
    /// indicator and to trigger context compaction before the window
    /// overflows. Conservative default; **set this to your model's actual
    /// window** (qwen3.5/qwen3.6 support far more than the default). Not
    /// auto-detected: the OpenAI-compatible `/v1` surface doesn't expose it
    /// reliably across Ollama/vLLM/llama.cpp.
    pub context_tokens: u32,
    /// How edit content travels: "native" sends edits as edit_file /
    /// write_file tool-call arguments; "prompted" teaches the model to
    /// write SEARCH/REPLACE blocks as plain text instead (parsed by aivyx
    /// and applied through the same permission gate). Multiline code
    /// survives plain text better than JSON string escaping on some
    /// models — see ROADMAP.md Phase 2 for the A/B evidence behind the
    /// default.
    pub edit_format: EditFormat,
    /// Which backend server this config talks to — see `BackendKind`'s
    /// own doc comment. Default `Generic`: no behavior change for
    /// existing configs.
    pub kind: BackendKind,
    /// Maximum bytes the kvcache store (docs/README's KV-cache persistence
    /// section) will hold on disk before evicting the least-recently-used
    /// entry. Only meaningful when `kind = "llama_server"`. Default 10 GiB.
    pub kvcache_max_bytes: u64,
    /// Overrides where the kvcache store directory lives. `None`
    /// (default) preserves the historical per-app `ProjectDirs`-derived
    /// path. Set this to the *same* directory as `aivyx`'s own
    /// `kvcache_store_path` (and point both configs' backends at the
    /// same `llama-server`) to share prefill work across the two
    /// processes — see `docs/MCP_RECIPES.md`'s `aivyx-coder` recipe in
    /// the `aivyx` repo for the full pairing guidance. Supports a
    /// leading `~`, same convention as `deny_paths`.
    pub kvcache_store_path: Option<String>,
}

/// Phase kvcache-adoption — which local-LLM backend server this config
/// talks to. `Generic` (the default) is today's fully backend-agnostic
/// behavior; `LlamaServer` opts a config into llama-server-specific
/// features (currently: KV-cache persistence via `aivyx-kvcache`, gated
/// on this exact variant so no other backend is ever affected).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Generic,
    LlamaServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditFormat {
    Native,
    Prompted,
}

impl Default for BackendSettings {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
            api_key: None,
            tool_calling_mode: ToolCallingMode::Native,
            context_tokens: 8192,
            // Evidence-based default: the Phase 2 A/B (qwen3.5:9b, see
            // ROADMAP.md) measured native 7/9 vs prompted 6/9 with zero
            // payload-format failures in either — native is simpler and
            // ~45% faster, prompted stays available for models that
            // genuinely mangle tool-call JSON.
            edit_format: EditFormat::Native,
            kind: BackendKind::Generic,
            kvcache_max_bytes: 10 * 1024 * 1024 * 1024,
            kvcache_store_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallingMode {
    Native,
    TextFallback,
    NativeWithFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionSettings {
    pub mode: PermissionMode,
    /// Hard-blocked regardless of prompt/allow-list state, enforced by
    /// `ConfirmationGate` before any prompt or Always-Allow cache lookup.
    /// Entries may use a leading `~` for the home directory — see
    /// `resolved_deny_paths`.
    pub deny_paths: Vec<String>,
    pub max_tool_iterations_per_turn: u32,
    /// Fixed set of commands the `run_command` tool may execute — the model
    /// selects one by `name`, it never supplies a program or arbitrary args.
    /// Empty by default: nothing is runnable until a project explicitly
    /// opts in.
    pub allowed_commands: Vec<AllowedCommand>,
}

impl Default for PermissionSettings {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Confirm,
            // `read_file` has no OS-level backstop at all — Landlock only
            // confines spawned child processes, never this crate's own
            // in-process file reads — so this list is the *sole*
            // protection against the model reading plaintext credentials
            // via a normal, auto-allowed `ActionKind::Read` call. Not
            // exhaustive (impossible to be), but covers the common,
            // high-value cases beyond SSH/AWS: GPG, generic netrc-style
            // creds, container/cluster/cloud-CLI auth, and package-registry
            // tokens (including this project's own toolchain's). The
            // basename-glob entries below (no leading `~`) match by file
            // name anywhere rather than one fixed location, covering
            // project-local secrets like `.env` that recur across
            // arbitrary project directories.
            deny_paths: vec![
                "~/.ssh".to_string(),
                "~/.aws".to_string(),
                "~/.config/aivyx-coder".to_string(),
                // Protects the whole control-plane state directory, not
                // just its `memory/` subdirectory (matching the same
                // whole-directory reasoning as `~/.config/aivyx-coder`
                // above) — without this, a generic write_file/edit_file
                // could plant a crafted memory topic file directly under
                // `memory/`, bypassing the ActionKind::PersistentMemory
                // gate entirely; a later memory_read (auto-allowed) would
                // then return the planted content.
                "~/.local/state/aivyx-coder".to_string(),
                "~/.gnupg".to_string(),
                "~/.netrc".to_string(),
                "~/.docker/config.json".to_string(),
                "~/.kube/config".to_string(),
                "~/.npmrc".to_string(),
                "~/.pypirc".to_string(),
                "~/.config/gcloud".to_string(),
                "~/.azure".to_string(),
                "~/.cargo/credentials.toml".to_string(),
                "~/.config/gh".to_string(),
                ".env".to_string(),
                ".env.*".to_string(),
                "id_rsa".to_string(),
                "id_ed25519".to_string(),
                "*.pem".to_string(),
                "*.key".to_string(),
            ],
            max_tool_iterations_per_turn: 25,
            allowed_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowedCommand {
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Overrides the tool's default timeout for this command when set.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl PermissionSettings {
    /// Expands a leading `~` (home directory) in each path-like
    /// `deny_paths` entry into an absolute `PathBuf`. A *bare* entry —
    /// one with no `/` and not starting with `~` (e.g. `.env`, `*.pem`)
    /// — is left exactly as configured instead: `aivyx_sandbox::path_is_denied`
    /// treats a single-component entry as a basename-glob pattern matched
    /// against a path's file name, not a real filesystem location to
    /// resolve. Running it through tilde-expansion and
    /// symlink-canonicalization here would tie a "matches anywhere"
    /// pattern to whatever the process's actual launch directory happens
    /// to contain instead. Path-like entries that can't be expanded (no
    /// home directory found) are skipped rather than left unresolved and
    /// silently wrong. A bare entry that isn't valid glob syntax (e.g. a
    /// typo like `a[`) is warned about via `tracing::warn!` but still
    /// passed through unchanged — the warning is purely additive
    /// visibility, not a behavior change.
    pub fn resolved_deny_paths(&self) -> Vec<PathBuf> {
        let (bare, path_like): (Vec<String>, Vec<String>) = self
            .deny_paths
            .iter()
            .cloned()
            .partition(|raw| !raw.starts_with('~') && !raw.contains('/'));
        for pattern in &bare {
            if let Err(err) = globset::Glob::new(pattern) {
                tracing::warn!(
                    entry = %pattern,
                    error = %err,
                    "malformed basename-glob deny_paths entry; it will never match anything until fixed"
                );
            }
        }
        let mut resolved = resolve_tilde_paths(&path_like);
        resolved.extend(bare.into_iter().map(PathBuf::from));
        resolved
    }
}

impl Settings {
    /// The complete, resolved deny_paths list this run actually uses:
    /// `permissions.resolved_deny_paths()` plus the effective kvcache
    /// store path (default or overridden), which must always be
    /// protected regardless of whether kvcache is enabled for this
    /// particular run -- a previous run may have left slot files behind
    /// under a path this run's `backend.kind` no longer even selects.
    pub fn effective_deny_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.permissions.resolved_deny_paths();
        paths.push(self.backend.resolved_kvcache_store_path());
        paths
    }
}

/// Shared by `PermissionSettings::resolved_deny_paths` and
/// `SandboxSettings::resolved_extra_read_paths`: expands a leading `~` into
/// an absolute path, skipping entries that can't be expanded (no home
/// directory found) rather than leaving them unresolved and silently wrong.
/// Every result is also symlink-canonicalized (see `resolve_symlinks`) —
/// without this, a symlinked entry (e.g. `~/.ssh` symlinked via a dotfile
/// manager) would never match the fully-resolved paths every tool actually
/// checks against, making the entry a silent no-op.
fn resolve_tilde_paths(raw_paths: &[String]) -> Vec<PathBuf> {
    let home_dir = directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());

    raw_paths
        .iter()
        .filter_map(|raw| {
            let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                home_dir.as_ref().map(|home| home.join(rest))
            } else if raw == "~" {
                home_dir.clone()
            } else if raw.starts_with('~') {
                // `~username`-style expansion (another user's home
                // directory) isn't supported. Failing loudly (skip + warn)
                // is safer than silently keeping it as a literal string —
                // no real filesystem path is relative and tilde-prefixed,
                // so it would otherwise be a permanently no-op entry with
                // no indication anything was wrong.
                tracing::warn!(
                    entry = %raw,
                    "unsupported ~username path syntax in config; skipping this entry"
                );
                None
            } else {
                Some(PathBuf::from(raw))
            };
            expanded.map(|path| resolve_symlinks(&path))
        })
        .collect()
}

/// Canonicalizes as much of `path` as exists, then re-appends whatever
/// doesn't. Mirrors `aivyx-tools`'s `path_resolve::resolve_symlinks`
/// exactly — duplicated here rather than having this lower-level config
/// crate depend on the tools crate for one small helper. Keep the two in
/// sync if either changes.
fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut current = path;

    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut result = canonical;
            for component in tail.into_iter().rev() {
                result.push(component);
            }
            return result;
        }

        match (current.file_name(), current.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Confirm,
}

impl Settings {
    pub fn config_path() -> Result<PathBuf, ConfigError> {
        let dirs = ProjectDirs::from("", "", "aivyx-coder").ok_or(ConfigError::NoConfigDir)?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Resolves the user-global `AGENTS.md` location: the same config
    /// directory `config.toml` lives in.
    pub fn agents_file_path() -> Result<PathBuf, ConfigError> {
        let dirs = ProjectDirs::from("", "", "aivyx-coder").ok_or(ConfigError::NoConfigDir)?;
        Ok(dirs.config_dir().join("AGENTS.md"))
    }

    /// Loads settings from the XDG config file, writing it out with
    /// defaults on first run so the user has a real file to edit rather
    /// than an invisible set of built-in defaults.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path()?;

        if !path.exists() {
            let defaults = Self::default();
            defaults.write_to(&path)?;
            return Ok(defaults);
        }

        let raw = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        toml::from_str(&raw).map_err(|source| ConfigError::Parse { path, source })
    }

    fn write_to(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let toml_string = toml::to_string_pretty(self)?;
        fs::write(path, toml_string).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;

        // The config may contain a plaintext `backend.api_key` — restrict
        // it to owner-read-write rather than leaving it at the OS/umask
        // default (commonly world-readable).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    /// CLI flags win over the config file when present.
    pub fn apply_overrides(&mut self, base_url: Option<String>, model: Option<String>) {
        if let Some(base_url) = base_url {
            self.backend.base_url = base_url;
        }
        if let Some(model) = model {
            self.backend.model = model;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_round_trip_through_toml() {
        let settings = Settings::default();
        let toml_string = toml::to_string_pretty(&settings).unwrap();
        let parsed: Settings = toml::from_str(&toml_string).unwrap();
        assert_eq!(parsed.backend.base_url, settings.backend.base_url);
        assert_eq!(parsed.backend.model, settings.backend.model);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults_for_missing_fields() {
        let partial = r#"
            [backend]
            model = "custom-model"
        "#;
        let parsed: Settings = toml::from_str(partial).unwrap();
        assert_eq!(parsed.backend.model, "custom-model");
        assert_eq!(parsed.backend.base_url, BackendSettings::default().base_url);
        assert_eq!(
            parsed.permissions.deny_paths,
            PermissionSettings::default().deny_paths
        );
    }

    #[test]
    fn default_deny_paths_includes_the_config_directory() {
        assert!(
            PermissionSettings::default()
                .deny_paths
                .contains(&"~/.config/aivyx-coder".to_string())
        );
    }

    #[test]
    fn default_deny_paths_includes_the_state_directory() {
        // Protects the whole `~/.local/state/aivyx-coder` control-plane
        // directory (parent of `memory/`), not just the memory
        // subdirectory — otherwise a generic write_file/edit_file could
        // plant a crafted memory topic file directly, bypassing the
        // ActionKind::PersistentMemory gate entirely; a later memory_read
        // (auto-allowed) would then return the planted content.
        assert!(
            PermissionSettings::default()
                .deny_paths
                .contains(&"~/.local/state/aivyx-coder".to_string())
        );
    }

    #[test]
    fn resolved_kvcache_store_path_matches_the_historical_default_when_unset() {
        let settings = Settings::default();
        let path = settings.backend.resolved_kvcache_store_path();
        assert!(
            path.to_string_lossy().contains(".local/share/aivyx-coder/kvcache"),
            "default kvcache path must be unchanged when no override is configured, got {path:?}"
        );
    }

    #[test]
    fn resolved_kvcache_store_path_uses_the_configured_override() {
        let mut settings = Settings::default();
        settings.backend.kvcache_store_path = Some("~/shared-kvcache".to_string());
        let path = settings.backend.resolved_kvcache_store_path();
        assert!(
            path.ends_with("shared-kvcache"),
            "overridden kvcache path must be used verbatim (tilde-expanded), got {path:?}"
        );
        assert!(
            !path.to_string_lossy().contains("aivyx-coder/kvcache"),
            "overridden path must replace the default, not sit alongside it, got {path:?}"
        );
    }

    #[test]
    fn default_effective_deny_paths_includes_the_kvcache_directory() {
        let settings = Settings::default();
        assert!(
            settings
                .effective_deny_paths()
                .iter()
                .any(|p| p.to_string_lossy().contains(".local/share/aivyx-coder/kvcache")),
            "kvcache store directory must be in effective deny_paths, same rationale as the \
             state directory"
        );
    }

    #[test]
    fn effective_deny_paths_tracks_an_overridden_kvcache_path_not_the_old_default() {
        let mut settings = Settings::default();
        settings.backend.kvcache_store_path = Some("~/shared-kvcache".to_string());
        let paths = settings.effective_deny_paths();
        assert!(
            paths.iter().any(|p| p.ends_with("shared-kvcache")),
            "the overridden path must be protected"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("aivyx-coder/kvcache")),
            "the stale default path must not remain protected once overridden away from it"
        );
    }

    #[test]
    fn default_deny_paths_covers_common_credential_locations() {
        // Regression test for a full-codebase audit finding: `~/.ssh`/
        // `~/.aws` covered the two most obvious cases, but `read_file`
        // has no OS-level backstop at all (Landlock only wraps spawned
        // child processes, never in-process file reads — see
        // `ConfirmationGate::check`, which auto-allows `ActionKind::Read`
        // unconditionally once past this exact list) — so this list is
        // the *only* protection for reads, and it was missing several
        // other common plaintext-credential locations.
        let deny_paths = PermissionSettings::default().deny_paths;
        for expected in [
            "~/.gnupg",
            "~/.netrc",
            "~/.docker/config.json",
            "~/.kube/config",
            "~/.npmrc",
            "~/.pypirc",
            "~/.config/gcloud",
            "~/.azure",
            "~/.cargo/credentials.toml",
            "~/.config/gh",
            // Basename-glob entries (2026-07-28 capability audit): these
            // recur across arbitrary project directories, unlike the
            // fixed `~/`-anchored entries above, so they need matching
            // by name rather than by one absolute location.
            ".env",
            ".env.*",
            "id_rsa",
            "id_ed25519",
            "*.pem",
            "*.key",
        ] {
            assert!(
                deny_paths.contains(&expected.to_string()),
                "expected default deny_paths to include {expected:?}, got {deny_paths:?}"
            );
        }
    }

    #[test]
    fn cli_overrides_win_over_config_file_values() {
        let mut settings = Settings::default();
        settings.apply_overrides(Some("http://localhost:8080/v1".to_string()), None);
        assert_eq!(settings.backend.base_url, "http://localhost:8080/v1");
        assert_eq!(settings.backend.model, BackendSettings::default().model);
    }

    #[test]
    fn tilde_prefixed_deny_paths_expand_to_the_home_directory() {
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .canonicalize()
            .expect("$HOME must exist");
        let settings = PermissionSettings {
            deny_paths: vec!["~/.ssh".to_string()],
            ..PermissionSettings::default()
        };

        // `~/.ssh` need not exist for this test — `resolve_symlinks` walks
        // up to the nearest existing ancestor (home itself) and re-appends
        // the rest, same as `path_resolve::resolve` does for tools.
        assert_eq!(settings.resolved_deny_paths(), vec![home.join(".ssh")]);
    }

    #[test]
    fn non_tilde_deny_paths_pass_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("shadow");
        std::fs::write(&target, "").unwrap();
        let settings = PermissionSettings {
            deny_paths: vec![target.to_str().unwrap().to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(
            settings.resolved_deny_paths(),
            vec![target.canonicalize().unwrap()]
        );
    }

    #[test]
    fn tilde_username_syntax_is_skipped_not_treated_as_literal() {
        let settings = PermissionSettings {
            deny_paths: vec!["~root/.ssh".to_string()],
            ..PermissionSettings::default()
        };

        assert!(settings.resolved_deny_paths().is_empty());
    }

    #[test]
    fn a_malformed_bare_glob_pattern_still_resolves_unchanged() {
        // The warning this now emits isn't asserted here (this project's
        // existing tests don't assert on tracing output either — see
        // tilde_username_syntax_is_skipped_not_treated_as_literal above,
        // which only checks behavior) — this just proves the fix is
        // purely additive visibility, not a behavior change: a malformed
        // pattern still passes through exactly as before, just now with
        // a warning logged alongside it.
        let settings = PermissionSettings {
            deny_paths: vec!["a[".to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(settings.resolved_deny_paths(), vec![PathBuf::from("a[")]);
    }

    #[test]
    fn require_enforcement_defaults_to_true() {
        assert!(SandboxSettings::default().require_enforcement);
    }

    #[test]
    fn council_absent_from_config_means_not_configured() {
        let settings: Settings = toml::from_str("").unwrap();
        assert!(!settings.council.configured());
        assert_eq!(settings.council.tail_budget_tokens, 3072);
    }

    #[test]
    fn verification_absent_from_config_means_disabled() {
        let settings: Settings = toml::from_str("").unwrap();
        assert!(settings.verification.command.is_none());
        assert_eq!(settings.verification.max_auto_verify_retries, 3);
    }

    #[test]
    fn verification_command_parses_from_config() {
        let raw = r#"
            [verification]
            command = "test"
            max_auto_verify_retries = 5
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.verification.command.as_deref(), Some("test"));
        assert_eq!(settings.verification.max_auto_verify_retries, 5);
    }

    #[test]
    fn scoped_command_defaults_to_none() {
        let settings = VerificationSettings::default();
        assert_eq!(settings.scoped_command, None);
    }

    #[test]
    fn scoped_command_deserializes_when_present() {
        let toml = r#"
            command = "test"
            scoped_command = "test_scoped"
        "#;
        let settings: VerificationSettings = toml::from_str(toml).unwrap();
        assert_eq!(settings.scoped_command, Some("test_scoped".to_string()));
    }

    #[test]
    fn scoped_command_defaults_to_none_when_absent_from_toml() {
        let toml = r#"command = "test""#;
        let settings: VerificationSettings = toml::from_str(toml).unwrap();
        assert_eq!(settings.scoped_command, None);
    }

    #[test]
    fn autonomous_settings_have_conservative_defaults() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.autonomous.max_iterations, 20);
        assert_eq!(settings.autonomous.max_duration_secs, 3600);
    }

    #[test]
    fn autonomous_settings_parse_from_config() {
        let raw = r#"
            [autonomous]
            max_iterations = 5
            max_duration_secs = 600
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.autonomous.max_iterations, 5);
        assert_eq!(settings.autonomous.max_duration_secs, 600);
    }

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

    #[test]
    fn council_block_parses_members_and_chairman() {
        let raw = r#"
            [council]
            tail_budget_tokens = 2048
            members = [
                { base_url = "http://localhost:11434/v1", model = "qwen3.5:9b" },
                { base_url = "http://localhost:8080/v1", model = "resident" },
            ]
            chairman = { base_url = "http://localhost:11434/v1", model = "qwen3.6:27b" }
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(settings.council.configured());
        assert_eq!(settings.council.members.len(), 2);
        assert_eq!(settings.council.members[1].model, "resident");
        assert_eq!(
            settings.council.chairman.as_ref().unwrap().model,
            "qwen3.6:27b"
        );
        assert_eq!(settings.council.tail_budget_tokens, 2048);
    }

    #[test]
    fn council_needs_two_members_and_a_chairman_to_convene() {
        let one_member = r#"
            [council]
            members = [{ base_url = "http://localhost:11434/v1", model = "solo" }]
            chairman = { base_url = "http://localhost:11434/v1", model = "chair" }
        "#;
        let settings: Settings = toml::from_str(one_member).unwrap();
        assert!(!settings.council.configured());

        let no_chairman = r#"
            [council]
            members = [
                { base_url = "http://localhost:11434/v1", model = "a" },
                { base_url = "http://localhost:11434/v1", model = "b" },
            ]
        "#;
        let settings: Settings = toml::from_str(no_chairman).unwrap();
        assert!(!settings.council.configured());
    }

    #[test]
    fn architect_settings_default_is_unconfigured() {
        let settings = Settings::default();
        assert!(!settings.architect.configured());
        assert_eq!(settings.architect.base_url, "");
        assert_eq!(settings.architect.model, "");
        assert_eq!(settings.architect.tail_budget_tokens, 3072);
    }

    #[test]
    fn architect_block_parses_and_reports_configured() {
        let raw = r#"
            [architect]
            base_url = "http://localhost:11434/v1"
            model = "qwen3.6:27b"
            tail_budget_tokens = 2048
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(settings.architect.configured());
        assert_eq!(settings.architect.base_url, "http://localhost:11434/v1");
        assert_eq!(settings.architect.model, "qwen3.6:27b");
        assert_eq!(settings.architect.tail_budget_tokens, 2048);
    }

    #[test]
    fn architect_needs_both_base_url_and_model_to_be_configured() {
        let only_model = r#"
            [architect]
            model = "qwen3.6:27b"
        "#;
        let settings: Settings = toml::from_str(only_model).unwrap();
        assert!(!settings.architect.configured());

        let only_base_url = r#"
            [architect]
            base_url = "http://localhost:11434/v1"
        "#;
        let settings: Settings = toml::from_str(only_base_url).unwrap();
        assert!(!settings.architect.configured());
    }

    #[test]
    fn lsp_settings_default_timeout_is_sixty_seconds() {
        let settings = Settings::default();
        assert_eq!(settings.lsp.timeout_secs, 60);
    }

    #[test]
    fn lsp_block_parses_a_custom_timeout() {
        let raw = r#"
            [lsp]
            timeout_secs = 120
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.lsp.timeout_secs, 120);
    }

    #[test]
    fn agents_file_settings_default_is_enabled_with_a_1024_token_budget() {
        let settings = Settings::default();
        assert!(settings.agents_file.enabled);
        assert_eq!(settings.agents_file.budget_tokens, 1024);
    }

    #[test]
    fn agents_file_block_parses_custom_values() {
        let raw = r#"
            [agents_file]
            enabled = false
            budget_tokens = 2048
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(!settings.agents_file.enabled);
        assert_eq!(settings.agents_file.budget_tokens, 2048);
    }

    #[test]
    fn editor_context_settings_default_is_enabled() {
        let settings = Settings::default();
        assert!(settings.editor_context.enabled);
    }

    #[test]
    fn editor_approval_settings_default_is_enabled() {
        assert!(EditorApprovalSettings::default().enabled);
    }

    #[test]
    fn editor_context_block_parses_custom_values() {
        let raw = r#"
            [editor_context]
            enabled = false
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(!settings.editor_context.enabled);
    }

    #[test]
    fn web_settings_defaults_are_network_enabled_but_search_unconfigured() {
        let settings = Settings::default();
        assert!(settings.web.enabled);
        assert_eq!(settings.web.search_base_url, None);
        assert_eq!(settings.web.max_search_results, 10);
        assert_eq!(settings.web.fetch_timeout_secs, 30);
        assert!(!settings.web.allow_private_targets);
    }

    #[test]
    fn web_block_parses_custom_values() {
        let raw = r#"
            [web]
            enabled = true
            search_base_url = "https://searx.example.org"
            max_search_results = 5
            fetch_timeout_secs = 15
            allow_private_targets = true
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(settings.web.enabled);
        assert_eq!(
            settings.web.search_base_url,
            Some("https://searx.example.org".to_string())
        );
        assert_eq!(settings.web.max_search_results, 5);
        assert_eq!(settings.web.fetch_timeout_secs, 15);
        assert!(settings.web.allow_private_targets);
    }

    #[test]
    fn web_disabled_via_config() {
        let raw = r#"
            [web]
            enabled = false
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert!(!settings.web.enabled);
    }

    #[test]
    fn sub_agent_settings_default_max_iterations_is_ten() {
        let settings = SubAgentSettings::default();
        assert_eq!(settings.max_iterations, 10);
    }

    #[test]
    fn sub_agent_settings_parses_from_toml() {
        let toml = r#"
            [sub_agent]
            max_iterations = 5
        "#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(settings.sub_agent.max_iterations, 5);
    }

    #[test]
    fn mcp_server_config_defaults() {
        let config = McpServerConfig::default();
        assert_eq!(config.name, "");
        assert_eq!(config.command, "");
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
        assert_eq!(config.timeout_secs, 30);
    }

    #[test]
    fn settings_defaults_to_no_mcp_servers() {
        let settings = Settings::default();
        assert!(settings.mcp.servers.is_empty());
    }

    #[test]
    fn mcp_servers_array_parses_from_toml() {
        let toml_str = r#"
            [[mcp.servers]]
            name = "filesystem"
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
            timeout_secs = 15

            [mcp.servers.env]
            API_KEY = "secret"
        "#;
        let settings: Settings = toml::from_str(toml_str).unwrap();
        assert_eq!(settings.mcp.servers.len(), 1);
        let server = &settings.mcp.servers[0];
        assert_eq!(server.name, "filesystem");
        assert_eq!(server.command, "npx");
        assert_eq!(
            server.args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert_eq!(server.timeout_secs, 15);
        assert_eq!(server.env.get("API_KEY"), Some(&"secret".to_string()));
    }

    #[test]
    fn config_file_is_written_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let settings = Settings::default();

        settings.write_to(&path).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn repl_settings_default_to_a_usable_zero_config_shape() {
        let settings = ReplSettings::default();
        assert_eq!(settings.quiet_window_ms, 300);
        assert_eq!(settings.max_wait_secs, 10);
        assert_eq!(settings.idle_timeout_secs, 600);
    }

    #[test]
    fn a_bare_basename_glob_entry_is_left_unresolved() {
        let settings = PermissionSettings {
            deny_paths: vec![".env".to_string(), "*.pem".to_string()],
            ..PermissionSettings::default()
        };

        let resolved = settings.resolved_deny_paths();
        assert!(resolved.contains(&PathBuf::from(".env")));
        assert!(resolved.contains(&PathBuf::from("*.pem")));
    }

    #[test]
    fn a_bare_entry_is_unaffected_by_the_process_current_directory() {
        // Regression test for the bug this fix closes: if a bare entry
        // were run through tilde/symlink resolution like a real path,
        // `canonicalize` could silently rewrite it into an absolute path
        // tied to wherever the process happened to be launched from —
        // breaking the "matches this basename anywhere" semantic. This
        // test does NOT create a colliding file or change directory to
        // demonstrate the pre-fix bug directly (that would require
        // mutating the test process's real cwd, which is fragile under
        // parallel test execution) — it instead asserts the guaranteed
        // post-fix invariant: resolution is never even attempted for a
        // bare entry, so the result is deterministic regardless of what
        // exists on disk anywhere.
        let settings = PermissionSettings {
            deny_paths: vec![".env".to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(settings.resolved_deny_paths(), vec![PathBuf::from(".env")]);
    }

    #[test]
    fn a_bare_tilde_alone_still_expands_to_the_home_directory() {
        // `"~"` contains no `/`, so it must be special-cased in the
        // bare-pattern classification — otherwise this would regress from
        // an already-supported case into a literal, wrong pattern.
        let home = directories::UserDirs::new()
            .unwrap()
            .home_dir()
            .canonicalize()
            .expect("$HOME must exist");
        let settings = PermissionSettings {
            deny_paths: vec!["~".to_string()],
            ..PermissionSettings::default()
        };

        assert_eq!(settings.resolved_deny_paths(), vec![home]);
    }

    #[test]
    fn backend_kind_defaults_to_generic_when_absent() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.backend.kind, BackendKind::Generic);
    }

    #[test]
    fn backend_kind_parses_llama_server() {
        let raw = r#"
            [backend]
            base_url = "http://127.0.0.1:8080/v1"
            model = "test-model"
            kind = "llama_server"
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.backend.kind, BackendKind::LlamaServer);
    }

    #[test]
    fn kvcache_max_bytes_defaults_to_ten_gib() {
        let settings: Settings = toml::from_str("").unwrap();
        assert_eq!(settings.backend.kvcache_max_bytes, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn kvcache_max_bytes_is_configurable() {
        let raw = r#"
            [backend]
            base_url = "http://127.0.0.1:8080/v1"
            model = "test-model"
            kvcache_max_bytes = 5000000000
        "#;
        let settings: Settings = toml::from_str(raw).unwrap();
        assert_eq!(settings.backend.kvcache_max_bytes, 5_000_000_000);
    }
}
