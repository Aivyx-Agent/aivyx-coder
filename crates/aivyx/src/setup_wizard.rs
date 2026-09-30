//! `aivyx-coder --setup`: an interactive first-run wizard that picks a
//! backend, picks (or verifies) a model, and writes `config.toml`.
//! Re-runnable: when a config already exists it asks before replacing it,
//! and keeps the old one as `config.toml.bak`.
//!
//! Split into a pure decision layer (`backend_settings_from_answers`,
//! `default_base_url` -- both tested) and a thin interactive-I/O layer
//! (`run`, not unit-tested -- real stdin/stdout via `dialoguer`) that
//! collects a `WizardAnswers` and calls Task 1's model-listing helpers
//! plus the existing `probe_served_context` before writing.

use aivyx_config::{BackendKind, BackendSettings, Settings};
use aivyx_llm::context_warning;
use aivyx_llm::list_models::{list_ollama_models, list_openai_compatible_models};
use aivyx_llm::probe::{ServedContext, probe_served_context, verify_model_responds};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendChoice {
    Ollama,
    Lemonade,
    GenericOpenAiCompatible,
}

#[derive(Debug, Clone)]
pub(crate) struct WizardAnswers {
    // Captured from the prompt but not read by `backend_settings_from_answers`
    // -- the wizard always writes `BackendKind::Generic` regardless of this
    // choice (out of scope for this plan to branch on, see the design spec's
    // Decision 2). Kept on `WizardAnswers` anyway since it's the natural home
    // for this answer and a future plan may want to read it back. Only
    // surfaces as a `dead_code` warning once `run()` itself became reachable
    // from `main()` (Task 4) -- rustc's dead-code analysis doesn't descend
    // into an unreachable function's own field usage, so this was invisible
    // before that wiring landed.
    #[allow(dead_code)]
    pub(crate) backend_choice: BackendChoice,
    pub(crate) base_url: String,
    pub(crate) model: String,
}

pub(crate) fn default_base_url(choice: BackendChoice) -> &'static str {
    match choice {
        BackendChoice::Ollama => "http://localhost:11434/v1",
        // Lemonade Server's OpenAI-compatible API lives under /api.
        BackendChoice::Lemonade => "http://127.0.0.1:13305/api/v1",
        BackendChoice::GenericOpenAiCompatible => "http://localhost:8080/v1",
    }
}

/// Where a re-run of `--setup` keeps the config it replaces.
pub(crate) fn backup_path(config_path: &std::path::Path) -> std::path::PathBuf {
    let mut name = config_path.as_os_str().to_os_string();
    name.push(".bak");
    std::path::PathBuf::from(name)
}

/// Pure decision layer: maps the wizard's answers plus the probed served
/// context window (`ServedContext::Known(n)` -> `context_tokens = n`, any
/// other variant -> the `BackendSettings` default) to the `BackendSettings`
/// that gets written. Takes the already-probed value rather than probing
/// itself, so it stays I/O-free and unit-testable.
pub(crate) fn backend_settings_from_answers(
    answers: &WizardAnswers,
    served: &ServedContext,
) -> BackendSettings {
    let mut settings = BackendSettings {
        base_url: answers.base_url.clone(),
        model: answers.model.clone(),
        kind: BackendKind::Generic,
        ..Default::default()
    };
    if let ServedContext::Known(n) = served {
        settings.context_tokens = *n;
    }
    settings
}

/// What the wizard prints after verifying the model and probing its served
/// context window. Pure, so the wording is tested; `default_context` is what
/// gets written when the window is unknown.
pub(crate) fn verification_lines(
    verified: &Result<(), String>,
    served: &ServedContext,
    default_context: u32,
) -> Vec<String> {
    let mut lines = vec![match verified {
        Ok(()) => "  ok: the model answered".to_string(),
        Err(why) => format!(
            "  warning: the model didn't answer ({why}) -- writing the config anyway; \
             fix the server, then re-run `aivyx-coder --setup`"
        ),
    }];
    match served {
        ServedContext::Known(n) => lines.push(format!("  served context window: {n} tokens")),
        ServedContext::Unknown => lines.push(format!(
            "  couldn't read the served context window; using {default_context} tokens -- \
             if your server serves more, raise [backend] context_tokens in config.toml"
        )),
        // context_warning() always has something to say for this variant --
        // printed after the settings are built, so nothing extra here.
        ServedContext::OllamaDefaultUnknown => {}
    }
    lines
}

/// Whether a plain launch should run the wizard before starting: no config
/// yet and someone at a terminal to answer it. `--mcp-server` and `--auto`
/// have nobody to ask, so they keep writing the defaults.
pub(crate) fn first_run_needs_setup(
    config_exists: bool,
    interactive: bool,
    mcp_server: bool,
    auto: bool,
) -> bool {
    !config_exists && interactive && !mcp_server && !auto
}

/// What happens after the wizard writes the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterSetup {
    /// `--setup`: exit, telling the user how to start.
    Exit,
    /// A first plain launch: the caller starts the agent next.
    StartAgent,
}

/// The wizard's real entry point -- checks for an existing config first,
/// then runs the interactive flow, verifies the choice, and writes.
pub async fn run() -> anyhow::Result<()> {
    run_then(AfterSetup::Exit).await
}

pub(crate) async fn run_then(after: AfterSetup) -> anyhow::Result<()> {
    let config_path = Settings::config_path()?;
    if config_path.exists() {
        let backup = backup_path(&config_path);
        let replace = dialoguer::Confirm::new()
            .with_prompt(format!(
                "A config already exists at {}. Replace it? (the current one is kept as {}, replacing any earlier backup)",
                config_path.display(),
                backup.display()
            ))
            .default(false)
            .interact()?;
        if !replace {
            println!(
                "Kept the existing config. Edit it directly, or re-run --setup to replace it."
            );
            return Ok(());
        }
        std::fs::rename(&config_path, &backup)?;
        println!("Moved the old config to {}.", backup.display());
    }

    let backend_choice = prompt_backend_choice().await?;
    let base_url = prompt_base_url(backend_choice)?;
    let model = prompt_model(backend_choice, &base_url).await?;

    println!("Verifying {model} at {base_url} (the first reply can take a while if it has to load) ...");
    // Verify first: on servers that load a model on first use (Lemonade),
    // that load is what lets the probe below see its context window.
    let verified = verify_model_responds(&base_url, &model).await;
    let served = probe_served_context(&base_url, &model).await;
    let default_context = BackendSettings::default().context_tokens;
    for line in verification_lines(&verified, &served, default_context) {
        println!("{line}");
    }
    let answers = WizardAnswers {
        backend_choice,
        base_url,
        model,
    };
    let backend = backend_settings_from_answers(&answers, &served);
    // Compare against what backend_settings_from_answers is actually about
    // to write -- for ServedContext::Known(n) that's n itself (the measured
    // window), so there is no truncation risk and no warning; for
    // OllamaDefaultUnknown it's still the untouched default, so the warning
    // about Ollama's hidden served window still fires as before.
    if let Some(warning) = context_warning(backend.context_tokens, &served) {
        println!("  warning: {warning}");
    }

    let settings = Settings {
        backend,
        ..Default::default()
    };
    let written = settings.write_if_absent()?;
    println!("Wrote {}", written.display());

    if std::env::var("AIVYX_CODER_ACP_TERMINAL_AUTH").is_ok() {
        println!("Setup complete -- reconnecting...");
    } else if after == AfterSetup::StartAgent {
        println!("Setup complete -- starting aivyx-coder ...");
    } else {
        println!("Setup complete. Run `aivyx-coder` to start.");
    }

    Ok(())
}

/// The backend menu's labels and preselected index, given which of Ollama,
/// Lemonade and a generic server (in menu order) answered at their default
/// address. The first one running is preselected; with none, Ollama is.
pub(crate) fn backend_menu(running: [bool; 3]) -> ([String; 3], usize) {
    let base = [
        "Ollama",
        "Lemonade Server",
        "A running OpenAI-compatible server (llama-server, vLLM, Jan, ...)",
    ];
    let default = running.iter().position(|r| *r);
    let labels = std::array::from_fn(|i| match (default, running[i]) {
        (None, _) if i == 0 => "Ollama (recommended, zero setup)".to_string(),
        (_, true) => format!("{} -- running now", base[i]),
        _ => base[i].to_string(),
    });
    (labels, default.unwrap_or(0))
}

/// Which backends answer at their default address right now (menu order).
async fn detect_running_backends() -> [bool; 3] {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(client) => client,
        Err(_) => return [false; 3],
    };
    let answers = |url: &'static str| {
        let client = client.clone();
        async move {
            client
                .get(url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
        }
    };
    let (ollama, lemonade, generic) = tokio::join!(
        answers("http://localhost:11434/api/tags"),
        answers("http://127.0.0.1:13305/api/v1/health"),
        answers("http://localhost:8080/v1/models"),
    );
    [ollama, lemonade, generic]
}

async fn prompt_backend_choice() -> anyhow::Result<BackendChoice> {
    let (choices, default) = backend_menu(detect_running_backends().await);
    let selection = dialoguer::Select::new()
        .with_prompt("Which backend are you using?")
        .items(&choices)
        .default(default)
        .interact()?;
    Ok(match selection {
        0 => BackendChoice::Ollama,
        1 => BackendChoice::Lemonade,
        _ => BackendChoice::GenericOpenAiCompatible,
    })
}

fn prompt_base_url(choice: BackendChoice) -> anyhow::Result<String> {
    let default = default_base_url(choice);
    let base_url: String = dialoguer::Input::new()
        .with_prompt("Base URL")
        .default(default.to_string())
        .interact_text()?;
    Ok(base_url)
}

async fn prompt_model(choice: BackendChoice, base_url: &str) -> anyhow::Result<String> {
    let listed = match choice {
        BackendChoice::Ollama => list_ollama_models(base_url).await,
        BackendChoice::Lemonade | BackendChoice::GenericOpenAiCompatible => {
            list_openai_compatible_models(base_url).await
        }
    };
    match listed {
        Ok(models) if !models.is_empty() => {
            let selection = dialoguer::Select::new()
                .with_prompt("Model")
                .items(&models)
                .default(0)
                .interact()?;
            Ok(models[selection].clone())
        }
        _ => {
            let model: String = dialoguer::Input::new()
                .with_prompt("Model (could not list available models -- enter one manually)")
                .interact_text()?;
            Ok(model)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_preselects_the_first_server_found_running() {
        // Nothing running: Ollama, the easiest to install, stays first choice.
        let (labels, default) = backend_menu([false, false, false]);
        assert_eq!(default, 0);
        assert_eq!(labels[0], "Ollama (recommended, zero setup)");
        // Only Lemonade running: it's preselected and marked.
        let (labels, default) = backend_menu([false, true, false]);
        assert_eq!(default, 1);
        assert_eq!(labels[1], "Lemonade Server -- running now");
        assert_eq!(labels[0], "Ollama");
        // Several running: the first one wins.
        let (_, default) = backend_menu([true, true, false]);
        assert_eq!(default, 0);
    }

    #[test]
    fn a_first_interactive_run_without_a_config_runs_setup() {
        assert!(first_run_needs_setup(false, true, false, false));
        // A config exists, or nobody is at a terminal to answer: no wizard.
        assert!(!first_run_needs_setup(true, true, false, false));
        assert!(!first_run_needs_setup(false, false, false, false));
        // Non-interactive modes keep writing defaults as before.
        assert!(!first_run_needs_setup(false, true, true, false));
        assert!(!first_run_needs_setup(false, true, false, true));
    }

    #[test]
    fn a_missing_context_size_is_not_reported_as_a_dead_server() {
        let lines = verification_lines(&Ok(()), &ServedContext::Unknown, 8192);
        assert_eq!(lines[0], "  ok: the model answered");
        assert!(lines[1].contains("couldn't read the served context window"), "{lines:?}");
        assert!(lines[1].contains("8192"), "{lines:?}");
        assert!(lines[1].contains("context_tokens"), "{lines:?}");
        assert!(!lines.concat().contains("could not verify the server responded"));
    }

    #[test]
    fn a_model_that_does_not_answer_says_so_and_how_to_retry() {
        let lines = verification_lines(
            &Err("couldn't reach http://localhost:11434/v1 -- is the server running?".into()),
            &ServedContext::Unknown,
            8192,
        );
        assert!(lines[0].starts_with("  warning: the model didn't answer"), "{lines:?}");
        assert!(lines[0].contains("is the server running?"), "{lines:?}");
        assert!(lines[0].contains("--setup"), "{lines:?}");
    }

    #[test]
    fn a_known_context_size_is_reported() {
        let lines = verification_lines(&Ok(()), &ServedContext::Known(217162), 8192);
        assert_eq!(lines[1], "  served context window: 217162 tokens");
    }

    #[test]
    fn lemonade_defaults_to_its_openai_compatible_api() {
        assert_eq!(
            default_base_url(BackendChoice::Lemonade),
            "http://127.0.0.1:13305/api/v1"
        );
    }

    #[test]
    fn re_running_setup_keeps_the_old_config_as_bak() {
        let p = std::path::Path::new("/home/u/.config/aivyx-coder/config.toml");
        assert_eq!(
            backup_path(p),
            std::path::PathBuf::from("/home/u/.config/aivyx-coder/config.toml.bak")
        );
    }

    #[test]
    fn backend_settings_from_answers_maps_ollama_choice_to_generic_kind() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::Ollama,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
        };
        let settings = backend_settings_from_answers(&answers, &ServedContext::Unknown);
        assert_eq!(settings.base_url, "http://localhost:11434/v1");
        assert_eq!(settings.model, "qwen3.5:9b");
        assert_eq!(settings.kind, aivyx_config::BackendKind::Generic);
    }

    #[test]
    fn backend_settings_from_answers_maps_generic_openai_choice_the_same_way() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::GenericOpenAiCompatible,
            base_url: "http://localhost:8080/v1".to_string(),
            model: "some-model".to_string(),
        };
        let settings = backend_settings_from_answers(&answers, &ServedContext::Unknown);
        assert_eq!(settings.base_url, "http://localhost:8080/v1");
        assert_eq!(settings.model, "some-model");
        assert_eq!(settings.kind, aivyx_config::BackendKind::Generic);
    }

    #[test]
    fn backend_settings_from_answers_writes_the_measured_context_window_when_known() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::Ollama,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
        };
        let settings = backend_settings_from_answers(&answers, &ServedContext::Known(16384));
        assert_eq!(settings.context_tokens, 16384);
    }

    #[test]
    fn backend_settings_from_answers_falls_back_to_the_default_context_window_when_not_known() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::Ollama,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
        };
        let default_context_tokens = BackendSettings::default().context_tokens;

        let unknown = backend_settings_from_answers(&answers, &ServedContext::Unknown);
        assert_eq!(unknown.context_tokens, default_context_tokens);

        let ollama_default_unknown =
            backend_settings_from_answers(&answers, &ServedContext::OllamaDefaultUnknown);
        assert_eq!(
            ollama_default_unknown.context_tokens,
            default_context_tokens
        );
    }

    #[test]
    fn context_warning_against_the_written_backend_fires_only_for_ollama_default_unknown() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::Ollama,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
        };

        // ServedContext::Known(n) -> backend_settings_from_answers writes
        // context_tokens = n exactly, so comparing against the real,
        // already-built BackendSettings (not the unrelated default) must
        // produce no warning -- there is no truncation risk.
        let known = backend_settings_from_answers(&answers, &ServedContext::Known(4096));
        assert_eq!(known.context_tokens, 4096);
        assert!(
            aivyx_llm::context_warning(known.context_tokens, &ServedContext::Known(4096)).is_none()
        );

        // ServedContext::OllamaDefaultUnknown doesn't feed a measured value
        // in, so context_tokens stays at the BackendSettings default and the
        // warning about Ollama's hidden served window must still fire.
        let ollama_default_unknown =
            backend_settings_from_answers(&answers, &ServedContext::OllamaDefaultUnknown);
        assert_eq!(
            ollama_default_unknown.context_tokens,
            BackendSettings::default().context_tokens
        );
        let warning = aivyx_llm::context_warning(
            ollama_default_unknown.context_tokens,
            &ServedContext::OllamaDefaultUnknown,
        );
        assert!(warning.is_some_and(|w| w.contains("num_ctx")));
    }

    #[test]
    fn default_base_url_for_ollama_is_the_well_known_local_port() {
        assert_eq!(
            default_base_url(BackendChoice::Ollama),
            "http://localhost:11434/v1"
        );
    }

    #[test]
    fn default_base_url_for_generic_is_llama_server_s_well_known_local_port() {
        assert_eq!(
            default_base_url(BackendChoice::GenericOpenAiCompatible),
            "http://localhost:8080/v1"
        );
    }
}
