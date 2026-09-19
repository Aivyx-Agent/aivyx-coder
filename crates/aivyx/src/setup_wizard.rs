//! `aivyx-coder --setup`: an interactive first-run wizard that picks a
//! backend, picks (or verifies) a model, and writes `config.toml`.
//! First-run only -- refuses immediately if a config already exists,
//! before any prompt.
//!
//! Split into a pure decision layer (`backend_settings_from_answers`,
//! `default_base_url` -- both tested) and a thin interactive-I/O layer
//! (`run`, not unit-tested -- real stdin/stdout via `dialoguer`) that
//! collects a `WizardAnswers` and calls Task 1's model-listing helpers
//! plus the existing `probe_served_context` before writing.

use aivyx_config::{BackendKind, BackendSettings, Settings};
use aivyx_llm::context_warning;
use aivyx_llm::list_models::{list_ollama_models, list_openai_compatible_models};
use aivyx_llm::probe::{ServedContext, probe_served_context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendChoice {
    Ollama,
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
        BackendChoice::GenericOpenAiCompatible => "http://localhost:8080/v1",
    }
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

/// The wizard's real entry point -- checks for an existing config first,
/// then runs the interactive flow, verifies the choice, and writes.
pub async fn run() -> anyhow::Result<()> {
    let config_path = Settings::config_path()?;
    if config_path.exists() {
        println!(
            "config.toml already exists at {}. Edit it directly, or delete it and re-run --setup.",
            config_path.display()
        );
        return Ok(());
    }

    let backend_choice = prompt_backend_choice()?;
    let base_url = prompt_base_url(backend_choice)?;
    let model = prompt_model(backend_choice, &base_url).await?;

    println!("Verifying {model} at {base_url} ...");
    let served = probe_served_context(&base_url, &model).await;
    match &served {
        ServedContext::Known(n) => println!("  served context window: {n} tokens"),
        ServedContext::Unknown => println!(
            "  warning: could not verify the server responded at all -- writing config anyway"
        ),
        // context_warning() always has something to say for this variant --
        // printed unconditionally below, so nothing extra here.
        ServedContext::OllamaDefaultUnknown => {}
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
    } else {
        println!("Setup complete. Run `aivyx-coder` to start.");
    }

    Ok(())
}

fn prompt_backend_choice() -> anyhow::Result<BackendChoice> {
    let choices = [
        "Ollama (recommended, zero setup)",
        "A running OpenAI-compatible server (llama-server, vLLM, ...)",
    ];
    let selection = dialoguer::Select::new()
        .with_prompt("Which backend are you using?")
        .items(&choices)
        .default(0)
        .interact()?;
    Ok(if selection == 0 {
        BackendChoice::Ollama
    } else {
        BackendChoice::GenericOpenAiCompatible
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
        BackendChoice::GenericOpenAiCompatible => list_openai_compatible_models(base_url).await,
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
