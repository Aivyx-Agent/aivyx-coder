use std::sync::Arc;

use aivyx_config::Settings;
use aivyx_core::Agent;
use aivyx_llm::{LlmBackend, OpenAiCompatBackend};
use aivyx_sandbox::{ConfirmationGate, ExecutionConfiner, NoopConfiner, PermissionGate};
use aivyx_tools::{EditFileTool, ReadFileTool, ToolExecutor, ToolRegistry, WriteFileTool};
use clap::Parser;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

const SYSTEM_PROMPT: &str = "You are aivyx, a local-only coding agent running in a terminal UI. \
You have three tools: read_file, write_file (creates or fully overwrites a file), and edit_file \
(exact-string search/replace — old_string must match exactly once in the file, or set replace_all). \
Mutating actions (write_file, edit_file) require the user to approve a confirmation prompt before \
they take effect, so explain what you're about to do before calling them.";

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

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));

    let (prompter, permission_rx) = aivyx_tui::permission_channel();
    let gate: Arc<dyn PermissionGate> = Arc::new(ConfirmationGate::new(
        Arc::new(prompter),
        settings.permissions.resolved_deny_paths(),
    ));
    let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
    let executor = ToolExecutor::new(registry, gate, confiner);

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let agent = Agent::new(llm, executor, SYSTEM_PROMPT, events_tx);

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
