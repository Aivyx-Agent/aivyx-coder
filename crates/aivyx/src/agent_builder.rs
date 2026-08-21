//! Shared, frontend-agnostic construction of an `Agent` from `Settings` +
//! `Cli` — the TUI (`main.rs`) and the ACP server (`aivyx-acp`) both call
//! this, differing only in which `PermissionPrompter` and event consumer
//! they plug in downstream. See `docs/superpowers/specs/
//! 2026-07-20-acp-editor-integration-design.md`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aivyx_config::Settings;
// Identical to main.rs's own top-of-file import list (main.rs:5-15) —
// copy it verbatim rather than retyping, so nothing is silently dropped.
// `DelegateTaskTool`/`DelegateTaskConfig` are deliberately absent here,
// matching main.rs: the pasted body (Step 2) references them
// fully-qualified as `aivyx_core::DelegateTaskTool::new(aivyx_core::DelegateTaskConfig { .. })`
// (main.rs:439-455), so no import is needed and no edit to that call
// site is needed either.
use aivyx_core::{Agent, AgentConfig, Architect, ArchitectSeat, Council, CouncilSeat, EditFormat, session};
use aivyx_llm::{LlmBackend, OpenAiCompatBackend};
use aivyx_sandbox::{
    AutonomousMode, ConfirmationGate, InjectionTaint, PermissionGate, PermissionPrompter, PlanMode,
};
use aivyx_tools::{
    CommandSpec, DeleteFileTool, EditFileTool, FindReferencesTool, GetMcpPromptTool,
    GitBranchTool, GitCheckpointer, GitCommitTool, GitPrTool, GitPushTool, GitReadTool, GlobTool,
    GoToDefinitionTool, GrepTool, ListMcpPromptsTool, ListMcpResourcesTool, LspClient, McpClient,
    McpToolAdapter, MemoryForgetTool, MemoryReadTool, MemoryWriteTool, MoveFileTool, PatchFileTool,
    ReadFileTool, ReadMcpResourceTool, RememberPreferenceTool, ReplResizeTarget, ReplSendTool,
    ReplStartTool, ReplStopTool, RunCommandTool, RunShellTool, SetTasksTool, ToolExecutor,
    ToolRegistry, WebFetchTool, WebSearchTool, WriteFileTool, new_shared_repl_session,
};
use tokio::sync::mpsc;

use crate::{
    Cli, COUNCIL_IDLE_TIMEOUT, DEFAULT_COMMAND_TIMEOUT_SECS, base_url_looks_local,
    build_system_prompt,
};

/// Everything a frontend needs to start driving a fully-configured
/// `Agent` — the exact set of values `main.rs`'s TUI path used to build
/// inline before handing off to `aivyx_tui::run`.
pub(crate) struct BuiltAgent {
    pub(crate) agent: Agent,
    pub(crate) events_rx: mpsc::UnboundedReceiver<aivyx_core::AgentEvent>,
    pub(crate) cwd: PathBuf,
    pub(crate) plan_mode: PlanMode,
    pub(crate) restored: Option<session::SessionState>,
    pub(crate) tasks: Arc<std::sync::Mutex<Vec<session::Task>>>,
    pub(crate) injection_taint: InjectionTaint,
    /// Lets a frontend forward a live terminal resize down to whatever
    /// `repl_start` session is currently running. Only the TUI frontend
    /// uses this (see `main.rs`) — the ACP frontend has no real terminal
    /// to forward a resize *from*, so it simply never reads this field.
    pub(crate) repl_resize: Arc<dyn aivyx_sandbox::ResizeTarget>,
    /// The shared LLM backend — cheap `Arc` clone, reused by the MCP-server
    /// frontend to build a fresh `Agent` per session rather than sharing
    /// this struct's own (unused, for that frontend) `agent` field.
    pub(crate) llm: Arc<dyn LlmBackend>,
    pub(crate) confiner: Arc<dyn aivyx_sandbox::ExecutionConfiner>,
    pub(crate) checkpointer: Option<Arc<GitCheckpointer>>,
    pub(crate) repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)>,
    /// The shared KV-cache pool + store, when `[backend] kind =
    /// "llama_server"` and the `/props` probe (Step 1 above) succeeded --
    /// reused by the MCP-server frontend so every session's `Agent`
    /// shares the *same* pool/store instances instead of each opening its
    /// own (see `SessionConfig::kv_cache_handles` in `aivyx-mcp-server`).
    /// The `String` is `build_info`, the served binary's build hash.
    pub(crate) kv_cache_handles: Option<(
        Arc<aivyx_llm::KvSlotPool>,
        Arc<aivyx_kvcache::LlamaServerSlotStore>,
        String,
    )>,
    /// The full tool registry, minus `delegate_task` (not yet registered
    /// at the point this is cloned from), minus `repl_start`/`repl_send`/
    /// `repl_stop` (unreachable after an ephemeral session ends), minus
    /// every dynamically-bridged `mcp__<server>__<tool>` adapter and minus
    /// the four mcp_meta tools (`list_mcp_resources`/`read_mcp_resource`/
    /// `list_mcp_prompts`/`get_mcp_prompt`) — all excluded for the same
    /// confused-deputy reason: a remote MCP caller must not transitively
    /// reach a third-party MCP server the operator configured for a
    /// different purpose, whether via a bridged tool call or via reading
    /// that server's resources/prompts — the base every MCP-server
    /// session's own tier-filtered registry is built from.
    pub(crate) mcp_registry: ToolRegistry,
    /// The resolved `deny_paths` this function computed and used
    /// throughout its own body (sandbox confiner, checkpointer, repo map,
    /// tool constructors) — exposed so a frontend never has to recompute
    /// `settings.permissions.resolved_deny_paths()` a second time
    /// independently, which could silently diverge if this function's own
    /// local `deny_paths` is ever augmented further.
    pub(crate) deny_paths: Vec<PathBuf>,
}

/// Builds `Agent` + every collaborator it needs, identically regardless
/// of which frontend is asking — only `prompter` differs between the TUI
/// (`TuiPrompter`) and ACP (`AcpPrompter`) call sites.
pub(crate) async fn build_agent(
    cli: &Cli,
    settings: &Settings,
    prompter: Arc<dyn PermissionPrompter>,
) -> anyhow::Result<BuiltAgent> {
    tracing::info!(
        base_url = %settings.backend.base_url,
        model = %settings.backend.model,
        "starting aivyx"
    );

    if settings.backend.api_key.is_some() && !base_url_looks_local(&settings.backend.base_url) {
        tracing::warn!(
            base_url = %settings.backend.base_url,
            "backend.api_key is set but base_url doesn't look like a local endpoint — \
             credentials are being sent to a non-local host, which contradicts this \
             project's local-only premise"
        );
    }

    let llm: Arc<dyn LlmBackend> = Arc::new(OpenAiCompatBackend::new(
        settings.backend.base_url.clone(),
        settings.backend.model.clone(),
        settings.backend.api_key.clone(),
    ));

    let deny_paths = settings.permissions.resolved_deny_paths();
    // Canonicalized once and reused everywhere `cwd` is needed (sandbox
    // confiner, checkpointer, repo map, session path, and — critically —
    // `ConfirmationGate`'s cwd-boundary check below): `current_dir()` does
    // not resolve symlinks, and the gate's `starts_with` comparison is
    // against symlink-canonicalized tool target paths, so an uncanonicalized
    // cwd could cause it to spuriously deny legitimate in-worktree edits
    // when the process is launched from a path involving a symlink.
    let cwd = std::env::current_dir()?.canonicalize()?;

    let command_specs: Vec<CommandSpec> = settings
        .permissions
        .allowed_commands
        .iter()
        .map(|c| CommandSpec {
            name: c.name.clone(),
            program: c.program.clone(),
            args: c.args.clone(),
            timeout: Duration::from_secs(c.timeout_secs.unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)),
        })
        .collect();

    // One handle shared between the `set_tasks` tool (the model-facing
    // mutator) and the agent (which renders and persists the list).
    let tasks: Arc<std::sync::Mutex<Vec<session::Task>>> = Arc::default();

    // Each configured command is pre-approved in two forms: the direct
    // `(program, args)` invocation `run_command` uses, and the `sh -c
    // "<program> <args>"` form `run_shell` always wraps commands in — the
    // two tools have genuinely different invocation shapes (direct exec vs.
    // shell-interpreted), so a single natural config entry (e.g. `program =
    // "cargo", args = ["test"]`) needs both to be recognized by either tool
    // without requiring the user to write it out twice in different shapes.
    //
    // Every arg is shell-escaped before joining — a naive `args.join(" ")`
    // would let an arg containing a shell metacharacter (e.g. `program =
    // "grep", args = ["-rn", "TODO|FIXME", "."]`, a harmless regex under
    // direct execve) turn into live, unconfirmed shell syntax the moment
    // the reconstructed `sh -c` form is looked up in the Always-Allow cache.
    let mut pre_approved_commands: Vec<(String, Vec<String>)> = Vec::new();
    for spec in &command_specs {
        pre_approved_commands.push((spec.program.clone(), spec.args.clone()));
        let mut shell_form = shell_escape::escape(spec.program.as_str().into()).into_owned();
        for arg in &spec.args {
            shell_form.push(' ');
            shell_form.push_str(&shell_escape::escape(arg.as_str().into()));
        }
        pre_approved_commands.push(("sh".to_string(), vec!["-c".to_string(), shell_form]));
    }

    // One shared flag, three consumers: the gate enforces it, the agent
    // filters tools + annotates the system prompt by it, the TUI toggles it.
    let plan_mode = PlanMode::new();
    plan_mode.set_active(cli.plan);

    // --auto and --plan are contradictory (unattended action-taking vs.
    // enforced read-only); --auto and --resume are unsupported together in
    // this version (autonomous session state — unverified edits, retry
    // counts, the pre-experiment checkpoint ref — isn't part of
    // SessionState yet; see the Phase 11c design doc's non-goals).
    if cli.auto.is_some() && cli.plan {
        anyhow::bail!("--auto and --plan cannot be used together");
    }
    if cli.auto.is_some() && cli.resume {
        anyhow::bail!("--auto and --resume cannot be used together (not supported yet)");
    }
    if let Some(goal) = cli.auto.as_deref()
        && goal.trim().is_empty()
    {
        anyhow::bail!("--auto requires a non-empty goal");
    }
    let autonomous_mode = AutonomousMode::new();
    autonomous_mode.set_active(cli.auto.is_some());
    // One shared handle, three consumers: the agent flags it when
    // scanning, the gate consults it to deny further autonomous-mode
    // mutations, the TUI's autonomous driver consults it to stop the
    // whole run. See docs/superpowers/specs/
    // 2026-07-22-autonomous-mode-injection-guard-design.md.
    let injection_taint = InjectionTaint::new();
    // Auto-approving edits is only defensible because deterministic
    // verification is the safety net — without it, "autonomous" would mean
    // "unchecked." Refuse to start rather than run degraded.
    if cli.auto.is_some() && settings.verification.command.is_none() {
        anyhow::bail!(
            "--auto requires [verification].command to be configured — auto-approving edits \
             with no verification check is not supported"
        );
    }

    let gate: Arc<dyn PermissionGate> = Arc::new(
        ConfirmationGate::new(
            Arc::clone(&prompter),
            deny_paths.clone(),
            pre_approved_commands,
            plan_mode.clone(),
            autonomous_mode.clone(),
            cwd.clone(),
            settings.editor_approval.enabled,
        )
        .with_injection_taint(injection_taint.clone()),
    );
    let confiner = aivyx_sandbox::default_confiner(
        &cwd,
        &settings.sandbox.resolved_extra_read_paths(),
        &deny_paths,
        settings.sandbox.require_enforcement,
    );

    let mut checkpointer: Option<Arc<GitCheckpointer>> = None;
    if settings.git.checkpoints
        && let Some(detected) = GitCheckpointer::detect(&cwd, deny_paths.clone()).await
    {
        checkpointer = Some(Arc::new(detected));
    }
    let has_checkpointer = checkpointer.is_some();
    // The discard/rewind safety net on exhausted verification (the entire
    // basis for auto-approving edits in `--auto`) depends on a checkpointer
    // being present — without one, `Agent`'s discard/rewind logic degrades
    // silently to "leave the broken edits in place and keep going," which
    // is not a safe default for an unattended run. Refuse to start rather
    // than run degraded, same as the verification-command check above.
    if cli.auto.is_some() && !has_checkpointer {
        anyhow::bail!(
            "--auto requires a git worktree with checkpoints enabled ([git] checkpoints = true, \
             the default) — the discard/rewind safety net on exhausted verification depends on it"
        );
    }

    // Constructed here (rather than left inline at the `agent.set_repo_map`
    // call site, as before) so `delegate_task`'s sub-agent can share the
    // exact same `Arc<RepoMap>` — a fresh second `RepoMap` would duplicate
    // the parse cache for no benefit, since both agents walk the same cwd.
    let repo_map: Option<(Arc<aivyx_repomap::RepoMap>, u32)> = settings.repo_map.enabled.then(|| {
        (
            Arc::new(aivyx_repomap::RepoMap::new(cwd.clone(), deny_paths.clone())),
            settings.repo_map.budget_tokens,
        )
    });

    let (events_tx, events_rx) = mpsc::unbounded_channel();

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(DeleteFileTool));
    registry.register(Arc::new(MoveFileTool::new(deny_paths.clone())));
    registry.register(Arc::new(PatchFileTool));
    registry.register(Arc::new(GrepTool::new(deny_paths.clone())));
    registry.register(Arc::new(GlobTool::new(deny_paths.clone())));
    registry.register(Arc::new(RunShellTool));
    let repl_session = new_shared_repl_session();
    let repl_quiet_window = Duration::from_millis(settings.repl.quiet_window_ms);
    let repl_max_wait = Duration::from_secs(settings.repl.max_wait_secs);
    let repl_idle_timeout = Duration::from_secs(settings.repl.idle_timeout_secs);
    registry.register(Arc::new(ReplStartTool::new(
        Arc::clone(&repl_session),
        repl_quiet_window,
        repl_max_wait,
        repl_idle_timeout,
    )));
    registry.register(Arc::new(ReplSendTool::new(
        Arc::clone(&repl_session),
        repl_quiet_window,
        repl_max_wait,
    )));
    registry.register(Arc::new(ReplStopTool::new(Arc::clone(&repl_session))));
    let repl_resize: Arc<dyn aivyx_sandbox::ResizeTarget> =
        Arc::new(ReplResizeTarget::new(repl_session));
    registry.register(Arc::new(SetTasksTool::new(Arc::clone(&tasks))));

    // Cross-session memory (memory_write/memory_read/memory_forget),
    // backed by aivyx-recall's FileRecall — one shared Arc<dyn Recall>
    // across all three tools. See docs/superpowers/specs/
    // 2026-08-09-aivyx-recall-design.md. Falls back to an in-memory-only
    // store (functional for this process, just not persisted) if the
    // state directory can't be resolved — matching session persistence's
    // own best-effort, never-block-startup posture immediately below.
    let recall: Arc<dyn aivyx_recall::Recall> = match session::memory_dir_path() {
        Some(dir) => Arc::new(aivyx_recall::FileRecall::new(dir)),
        None => {
            tracing::warn!(
                "no state directory available — cross-session memory will not persist across restarts"
            );
            Arc::new(aivyx_recall::InMemoryRecall::new())
        }
    };
    registry.register(Arc::new(MemoryReadTool::new(Arc::clone(&recall))));
    registry.register(Arc::new(MemoryWriteTool::new(Arc::clone(&recall))));
    registry.register(Arc::new(MemoryForgetTool::new(recall)));

    registry.register(Arc::new(GitReadTool::new(deny_paths.clone())));
    registry.register(Arc::new(GitCommitTool::new(deny_paths.clone())));
    registry.register(Arc::new(GitBranchTool::new()));
    registry.register(Arc::new(GitPushTool::new()));
    registry.register(Arc::new(GitPrTool::new()));

    let lsp_client = Arc::new(LspClient::new(Duration::from_secs(settings.lsp.timeout_secs)));
    registry.register(Arc::new(GoToDefinitionTool::new(Arc::clone(&lsp_client))));
    registry.register(Arc::new(FindReferencesTool::new(Arc::clone(&lsp_client))));

    // Only registered when configured — an always-erroring tool offered to
    // the model would just be confusing noise for a project that hasn't
    // opted into any commands.
    if !command_specs.is_empty() {
        registry.register(Arc::new(RunCommandTool::new(command_specs.clone())));
    }

    // Registered only when persona learning + AGENTS.md loading are both
    // enabled, and a global AGENTS.md location can even be resolved —
    // otherwise the agent could "successfully" remember something into a
    // file this session never reads back into context.
    if settings.persona.enabled
        && settings.agents_file.enabled
        && let Ok(path) = aivyx_config::Settings::agents_file_path()
    {
        registry.register(Arc::new(RememberPreferenceTool::new(path)));
    }

    // Both tools are only registered when explicitly enabled — network
    // access being off is always a deliberate choice, unlike (for
    // example) a missing rust-analyzer binary, which is an environmental
    // accident LSP's own tools handle by always registering and failing
    // clearly on first call instead.
    if settings.web.enabled {
        registry.register(Arc::new(WebFetchTool::new(
            settings.web.fetch_timeout_secs,
            settings.web.allow_private_targets,
        )));
        registry.register(Arc::new(WebSearchTool::new(
            settings.web.search_base_url.clone(),
            settings.web.max_search_results,
            settings.web.fetch_timeout_secs,
        )));
    }

    // Every configured server connects concurrently, each bounded by its
    // own `timeout_secs` — a slow or broken server can't hang startup or
    // delay every other server's tools from becoming available. A server
    // that fails or times out is skipped with a one-time warning (both a
    // log line and a surfaced AgentEvent::Error, matching how a
    // context-window mismatch is reported above) rather than aborting the
    // whole session.
    let mut mcp_discovery = tokio::task::JoinSet::new();
    // Collected as each bridged tool is discovered, so mcp_registry (built
    // below, after this loop) can exclude them by their real registered
    // name — dynamically server-defined, so no static list would work.
    let mut mcp_bridged_tool_names: Vec<String> = Vec::new();
    for server in settings.mcp.servers.clone() {
        let confiner = Arc::clone(&confiner);
        let cwd = cwd.clone();
        mcp_discovery.spawn(async move {
            let client = Arc::new(McpClient::new(
                server.name.clone(),
                server.command.clone(),
                server.args.clone(),
                server.env.clone().into_iter().collect(),
            ));
            let timeout = Duration::from_secs(server.timeout_secs);
            let outcome = tokio::time::timeout(timeout, async {
                client.ensure_started(&cwd, &confiner).await?;
                client.list_tools().await
            })
            .await;
            (server.name, client, outcome)
        });
    }

    let mut mcp_clients: Vec<Arc<McpClient>> = Vec::new();
    while let Some(joined) = mcp_discovery.join_next().await {
        // A `JoinError` here means the discovery task itself panicked —
        // treated the same as a connect/discover failure or timeout below:
        // log it and keep going. One broken server (even one whose
        // discovery code panics) must never crash the whole session.
        let (server_name, client, outcome) = match joined {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!("an MCP server discovery task panicked — skipping");
                let _ = events_tx.send(aivyx_core::AgentEvent::Error(
                    "an MCP server discovery task panicked — skipping".to_string(),
                ));
                continue;
            }
        };
        match outcome {
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
            Ok(Err(err)) => {
                tracing::warn!(
                    server = %server_name,
                    error = %err,
                    "MCP server failed to connect/discover tools — skipping for this session"
                );
                let _ = events_tx.send(aivyx_core::AgentEvent::Error(format!(
                    "MCP server \"{server_name}\" failed to connect: {err} — its tools are \
                     unavailable this session"
                )));
            }
            Err(_) => {
                tracing::warn!(
                    server = %server_name,
                    "MCP server startup timed out — skipping for this session"
                );
                let _ = events_tx.send(aivyx_core::AgentEvent::Error(format!(
                    "MCP server \"{server_name}\" timed out during startup — its tools are \
                     unavailable this session"
                )));
            }
        }
    }

    if !mcp_clients.is_empty() {
        registry.register(Arc::new(ListMcpResourcesTool::new(mcp_clients.clone())));
        registry.register(Arc::new(ReadMcpResourceTool::new(mcp_clients.clone())));
        registry.register(Arc::new(ListMcpPromptsTool::new(mcp_clients.clone())));
        registry.register(Arc::new(GetMcpPromptTool::new(mcp_clients.clone())));
    }

    let edit_format = match cli.edit_format.as_deref() {
        Some("native") => EditFormat::Native,
        Some("prompted") => EditFormat::Prompted,
        _ => match settings.backend.edit_format {
            aivyx_config::EditFormat::Native => EditFormat::Native,
            aivyx_config::EditFormat::Prompted => EditFormat::Prompted,
        },
    };

    // Snapshot every tool registered so far — this becomes a sub-agent's
    // own tool list, which must never include `delegate_task` itself
    // (recursion is structurally impossible this way, not merely
    // policy-excluded). `delegate_task` is registered onto `registry`
    // (the parent's) below, *after* this clone.
    let mut sub_agent_registry = registry.clone();
    sub_agent_registry.exclude(&["repl_start", "repl_send", "repl_stop"]);
    // Same clone point as sub_agent_registry (before delegate_task is
    // registered below) — additionally excludes dynamically-bridged MCP
    // tools, which sub_agent_registry does not need to (a delegate_task
    // sub-agent runs in the same trust boundary as its parent; an MCP-
    // server session's caller is a different process/product entirely).
    let mut mcp_registry = registry.clone();
    mcp_registry.exclude(&[
        "repl_start",
        "repl_send",
        "repl_stop",
        "list_mcp_resources",
        "read_mcp_resource",
        "list_mcp_prompts",
        "get_mcp_prompt",
    ]);
    let bridged_names: Vec<&str> = mcp_bridged_tool_names.iter().map(|s| s.as_str()).collect();
    mcp_registry.exclude(&bridged_names);
    // Verification config is threaded through so a sub-agent's own edits
    // get verified before `delegate_task` returns, exactly like the
    // parent's own edits would — mirrors the `agent.set_verification(...)`
    // call below, resolved once here so both call sites agree without
    // duplicating the allowed_commands-membership check.
    let verification = settings
        .verification
        .command
        .as_ref()
        .filter(|command| command_specs.iter().any(|spec| &spec.name == *command))
        .map(|command| (command.clone(), settings.verification.max_auto_verify_retries));
    registry.register(Arc::new(aivyx_core::DelegateTaskTool::new(
        aivyx_core::DelegateTaskConfig {
            llm: Arc::clone(&llm),
            gate: Arc::clone(&gate),
            confiner: Arc::clone(&confiner),
            checkpointer: checkpointer.clone(),
            repo_map: repo_map.clone(),
            events_tx: events_tx.clone(),
            sub_agent_registry,
            plan_mode: plan_mode.clone(),
            autonomous_mode: autonomous_mode.clone(),
            injection_taint: injection_taint.clone(),
            context_tokens: settings.backend.context_tokens,
            edit_format,
            verification: verification.clone(),
            max_iterations: settings.sub_agent.max_iterations,
        },
    )));

    let mut executor = ToolExecutor::new(registry, Arc::clone(&gate), Arc::clone(&confiner));
    if let Some(cp) = &checkpointer {
        executor.set_checkpointer(Arc::clone(cp));
    }
    let system_prompt = build_system_prompt(&executor, edit_format);

    // Best-effort probe of the *served* context window (llama-server
    // /props, Ollama /api/show) — a smaller-than-configured window means
    // silent mid-response truncation, the exact failure the Phase 2 A/B
    // spent a round diagnosing. Advisory only: the warning lands in the
    // transcript as a notice; an unreachable/unknown server stays silent.
    {
        let served =
            aivyx_llm::probe_served_context(&settings.backend.base_url, &settings.backend.model)
                .await;
        if let Some(warning) = aivyx_llm::context_warning(settings.backend.context_tokens, &served)
        {
            tracing::warn!(%warning, "context window mismatch");
            let _ = events_tx.send(aivyx_core::AgentEvent::Error(warning));
        }
    }

    // KV-cache persistence/sharing (aivyx-kvcache): only ever attempted
    // against a real llama-server backend, whose `/props` response both
    // `probe_served_context` above and `parse_llama_slots_info` here parse
    // from the same endpoint shape. Every failure mode here is fail-open --
    // logged at `warn`, `kv_cache_handles` stays `None`, the agent runs
    // exactly as if kvcache weren't configured at all.
    let kv_cache_handles = if settings.backend.kind == aivyx_config::BackendKind::LlamaServer {
        let origin = settings.backend.base_url.trim_end_matches('/').trim_end_matches("/v1");
        let props_url = format!("{origin}/props");
        match reqwest::Client::new().get(&props_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
                Ok(json) => match aivyx_llm::probe::parse_llama_slots_info(&json) {
                    Some(info) => {
                        let store_path = dirs::data_local_dir()
                            .unwrap_or_else(std::env::temp_dir)
                            .join("aivyx-coder")
                            .join("kvcache");
                        match aivyx_kvcache::LlamaServerSlotStore::open(
                            &store_path,
                            &settings.backend.base_url,
                            10 * 1024 * 1024 * 1024, // 10 GiB default budget
                        ) {
                            Ok(store) => Some((
                                Arc::new(aivyx_llm::KvSlotPool::new(info.total_slots)),
                                Arc::new(store),
                                info.build_info,
                            )),
                            Err(err) => {
                                tracing::warn!(error = %err, "kvcache: failed to open store; disabled for this run");
                                None
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            "kvcache: [backend] kind = \"llama_server\" but /props didn't look like a real \
                             llama-server response; disabled for this run"
                        );
                        None
                    }
                },
                Err(_) => None,
            },
            _ => {
                tracing::warn!("kvcache: /props probe failed; disabled for this run");
                None
            }
        }
    } else {
        None
    };

    let mut agent = Agent::new(
        Arc::clone(&llm),
        executor,
        system_prompt,
        AgentConfig {
            max_tool_iterations: settings.permissions.max_tool_iterations_per_turn,
            context_tokens: settings.backend.context_tokens,
            edit_format,
        },
        Arc::clone(&tasks),
        plan_mode.clone(),
        autonomous_mode.clone(),
        events_tx,
    );
    agent.set_injection_taint(injection_taint.clone());

    if let Some((pool, store, build_hash)) = &kv_cache_handles {
        agent.set_kv_cache(
            Arc::clone(pool),
            Arc::clone(store),
            "llama-server".to_string(),
            settings.backend.model.clone(),
            build_hash.clone(),
        );
    }

    if let Some((map, budget)) = &repo_map {
        agent.set_repo_map(Arc::clone(map), *budget);
    }

    // Absence of either file is not an error — the feature is off only
    // when the user explicitly disables it via [agents_file] enabled.
    if settings.agents_file.enabled {
        let global_path = aivyx_config::Settings::agents_file_path().ok();
        agent.set_agents_file(global_path, settings.agents_file.budget_tokens, deny_paths.clone());
    }

    // Absence of a context file is not an error — the feature is off only
    // when the user explicitly disables it via [editor_context] enabled.
    if settings.editor_context.enabled {
        agent.set_editor_context(deny_paths.clone());
    }

    // `/council` needs ≥2 members and a chairman; anything less and the
    // command explains itself instead (the agent handles the None case).
    if settings.council.configured() {
        let seat = |member: &aivyx_config::CouncilMember| CouncilSeat {
            model: member.model.clone(),
            backend: Arc::new(OpenAiCompatBackend::with_idle_timeout(
                member.base_url.clone(),
                member.model.clone(),
                member.api_key.clone(),
                COUNCIL_IDLE_TIMEOUT,
            )),
        };
        let chairman = settings
            .council
            .chairman
            .as_ref()
            .expect("configured() guarantees a chairman");
        agent.set_council(Council {
            members: settings.council.members.iter().map(seat).collect(),
            chairman: seat(chairman),
            tail_budget_tokens: settings.council.tail_budget_tokens,
        });
    }

    // `/architect` needs base_url + model; anything less and the command
    // explains itself instead (the agent handles the None case).
    if settings.architect.configured() {
        agent.set_architect(Architect {
            seat: ArchitectSeat {
                model: settings.architect.model.clone(),
                backend: Arc::new(OpenAiCompatBackend::with_idle_timeout(
                    settings.architect.base_url.clone(),
                    settings.architect.model.clone(),
                    settings.architect.api_key.clone(),
                    COUNCIL_IDLE_TIMEOUT,
                )),
            },
            tail_budget_tokens: settings.architect.tail_budget_tokens,
        });
    }

    // Enforced verification (ROADMAP.md Phase 12 Part B): configuring the
    // command alone is the opt-in, no separate enable flag. The name must
    // match an `allowed_commands` entry (the same trust tier `run_command`
    // itself uses) — warn loudly rather than silently doing nothing if it
    // doesn't, since a typo here would otherwise look like the feature is
    // enabled but never actually verify anything.
    if let Some((command, max_retries)) = &verification {
        agent.set_verification(command.clone(), *max_retries);

        // Scoped verification (docs/superpowers/specs/
        // 2026-07-28-verification-test-selection-design.md): only
        // resolved if the base command above was itself valid — scoping
        // an already-disabled verification setup makes no sense. Same
        // "warn, don't silently do nothing" posture as the base command's
        // own validation, but scoping alone being misconfigured must not
        // disable verification entirely — it falls back to always
        // running the full command, exactly as if scoped_command had
        // never been set.
        if let Some(scoped_name) = &settings.verification.scoped_command {
            match command_specs.iter().find(|spec| &spec.name == scoped_name) {
                Some(spec) => {
                    agent.set_scoped_verification(spec.clone(), Arc::clone(&confiner));
                }
                None => {
                    tracing::warn!(
                        scoped_command = %scoped_name,
                        "verification.scoped_command does not match any \
                         [[permissions.allowed_commands]] entry name — scoped verification is \
                         unavailable this session (falling back to always running the full \
                         command)"
                    );
                }
            }
        }
    } else if let Some(command) = &settings.verification.command {
        tracing::warn!(
            command = %command,
            "verification.command does not match any [[permissions.allowed_commands]] \
             entry name — enforced verification is disabled until this is fixed"
        );
    }

    // Persistence is always on (it's what makes `--resume` possible after a
    // crash or an interrupted slow-model turn); only *restoring* is opt-in.
    let restored = match session::session_file_path(&cwd) {
        Some(path) => {
            let restored = if cli.resume {
                let state = session::load(&path);
                if state.is_none() {
                    tracing::info!(path = %path.display(), "--resume: no resumable session found, starting fresh");
                }
                state
            } else {
                None
            };
            if let Some(state) = &restored {
                agent.restore(state.clone());
            }
            agent.set_session_path(path);
            restored
        }
        None => {
            tracing::warn!(
                "no state directory available — session persistence and --resume are disabled"
            );
            None
        }
    };

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
        kv_cache_handles,
        mcp_registry,
        deny_paths: deny_paths.clone(),
    })
}
