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
use aivyx_llm::list_models::{list_ollama_models, list_openai_compatible_models};
use aivyx_llm::probe::{ServedContext, probe_served_context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendChoice {
    Ollama,
    GenericOpenAiCompatible,
}

#[derive(Debug, Clone)]
pub(crate) struct WizardAnswers {
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

pub(crate) fn backend_settings_from_answers(answers: &WizardAnswers) -> BackendSettings {
    BackendSettings {
        base_url: answers.base_url.clone(),
        model: answers.model.clone(),
        kind: BackendKind::Generic,
        ..Default::default()
    }
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
    match probe_served_context(&base_url, &model).await {
        ServedContext::Known(n) => println!("  served context window: {n} tokens"),
        ServedContext::OllamaDefaultUnknown => println!(
            "  warning: this model's served context window could not be determined -- \
             Ollama serves a 4096-token default unless the model or service says \
             otherwise; set context_tokens in config.toml once you know the real value"
        ),
        ServedContext::Unknown => {
            println!(
                "  warning: could not verify the server responded at all -- writing config anyway"
            )
        }
    }

    let answers = WizardAnswers {
        backend_choice,
        base_url,
        model,
    };
    let backend = backend_settings_from_answers(&answers);
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
        let settings = backend_settings_from_answers(&answers);
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
        let settings = backend_settings_from_answers(&answers);
        assert_eq!(settings.base_url, "http://localhost:8080/v1");
        assert_eq!(settings.model, "some-model");
        assert_eq!(settings.kind, aivyx_config::BackendKind::Generic);
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
