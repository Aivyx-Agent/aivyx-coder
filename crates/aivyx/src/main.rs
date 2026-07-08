use std::fmt::Write as _;
use std::sync::Arc;

use aivyx_config::Settings;
use aivyx_core::Agent;
use aivyx_llm::{LlmBackend, OpenAiCompatBackend};
use aivyx_sandbox::{ConfirmationGate, ExecutionConfiner, NoopConfiner, PermissionGate};
use aivyx_tools::{
    EditFileTool, GlobTool, GrepTool, ReadFileTool, ToolExecutor, ToolRegistry, WriteFileTool,
};
use clap::Parser;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

const SYSTEM_PROMPT_PREAMBLE: &str = "You are aivyx, a local-only coding agent running in a terminal UI. \
Mutating actions require the user to approve a confirmation prompt before they take effect, so explain \
what you're about to do before calling them.";

/// Builds the tool-describing part of the system prompt from the tools
/// actually registered, so it can't silently drift out of sync with what's
/// sent to the model via `ChatRequest.tools` as the tool set grows.
fn build_system_prompt(executor: &ToolExecutor) -> String {
    let definitions = executor.definitions();
    if definitions.is_empty() {
        return format!("{SYSTEM_PROMPT_PREAMBLE}\n\nYou have no tools available in this session.");
    }

    let mut prompt = format!("{SYSTEM_PROMPT_PREAMBLE}\n\nAvailable tools:");
    for def in &definitions {
        let _ = write!(prompt, "\n- {}: {}", def.name, def.description);
    }
    prompt
}

#[derive(Parser, Debug)]
#[command(
    name = "aivyx",
    about = "A TUI coding agent for local LLMs (Ollama / vLLM / llama.cpp)"
)]
struct Cli {
    /// Override config.toml's backend base_url (e.g. http://localhost:11434/v1)
    #[arg(long)]
    base_url: Option<String>,

    /// Override config.toml's backend model
    #[arg(long)]
    model: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let _tracing_guard = init_tracing()?;

    let mut settings = Settings::load()?;
    settings.apply_overrides(cli.base_url, cli.model);

    tracing::info!(
        base_url = %settings.backend.base_url,
        model = %settings.backend.model,
        "starting aivyx"
    );

    let llm: Arc<dyn LlmBackend> = Arc::new(OpenAiCompatBackend::new(
        settings.backend.base_url.clone(),
        settings.backend.model.clone(),
        settings.backend.api_key.clone(),
    ));

    let deny_paths = settings.permissions.resolved_deny_paths();

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(GrepTool::new(deny_paths.clone())));
    registry.register(Arc::new(GlobTool::new(deny_paths.clone())));

    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> =
        Arc::new(ConfirmationGate::new(Arc::new(prompter), deny_paths));
    let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
    let executor = ToolExecutor::new(registry, gate, confiner);
    let system_prompt = build_system_prompt(&executor);

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let agent = Agent::new(
        llm,
        executor,
        system_prompt,
        settings.permissions.max_tool_iterations_per_turn,
        events_tx,
    );

    let cwd = std::env::current_dir()?;

    aivyx_tui::run(agent, events_rx, cwd, permission_rx).await
}

fn init_tracing() -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let config_path = Settings::config_path()?;
    let log_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::never(&log_dir, "aivyx.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .init();

    Ok(guard)
}
