use std::sync::Arc;

use aivyx_config::Settings;
use aivyx_core::Agent;
use aivyx_llm::{LlmBackend, OpenAiCompatBackend};
use aivyx_sandbox::{AlwaysDenyGate, ExecutionConfiner, NoopConfiner, PermissionGate};
use aivyx_tools::{ToolExecutor, ToolRegistry};
use clap::Parser;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

const SYSTEM_PROMPT: &str = "You are aivyx, a local-only coding agent running in a terminal UI. \
This build has no tools available yet — answer directly and concisely.";

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

    // No concrete tools this pass, so the registry is empty and this gate
    // is never actually consulted — see AlwaysDenyGate's doc comment for
    // why "deny" is the right placeholder decision anyway.
    let registry = ToolRegistry::new();
    let gate: Arc<dyn PermissionGate> = Arc::new(AlwaysDenyGate);
    let confiner: Arc<dyn ExecutionConfiner> = Arc::new(NoopConfiner);
    let executor = ToolExecutor::new(registry, gate, confiner);

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let agent = Agent::new(llm, executor, SYSTEM_PROMPT, events_tx);

    let cwd = std::env::current_dir()?;

    aivyx_tui::run(agent, events_rx, cwd).await
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
