//! Lists models a locally-running backend actually has available, so
//! `aivyx-coder --setup` can offer a real pick-list instead of asking
//! the operator to type a model name blind. Split into a network call
//! plus a pure parse function each, matching `probe.rs`'s own
//! established convention (only the parse functions are unit-tested).

use std::time::Duration;

const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum ListModelsError {
    #[error("could not reach the server: {0}")]
    Transport(String),
    #[error("server did not return a recognizable model list")]
    Unsupported,
}

/// Lists models an Ollama server has already pulled, via its native
/// `/api/tags` endpoint (not the OpenAI-compat `/v1/models`, which
/// Ollama also exposes but with less reliable coverage of locally-pulled
/// models across versions).
pub async fn list_ollama_models(base_url: &str) -> Result<Vec<String>, ListModelsError> {
    let origin = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let client = reqwest::Client::builder()
        .timeout(LIST_MODELS_TIMEOUT)
        .build()
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    let response = client
        .get(format!("{origin}/api/tags"))
        .send()
        .await
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ListModelsError::Unsupported);
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| ListModelsError::Unsupported)?;
    let models = parse_ollama_tags(&json);
    if models.is_empty() {
        return Err(ListModelsError::Unsupported);
    }
    Ok(models)
}

fn parse_ollama_tags(json: &serde_json::Value) -> Vec<String> {
    json.get("models")
        .and_then(|v| v.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Lists models a generic OpenAI-compatible server (llama-server, vLLM,
/// ...) reports via the standard `GET /v1/models` route. Not every
/// server implements this — a non-success status or unparseable body is
/// `Unsupported`, not a hard error, so the wizard can fall back to
/// manual entry.
pub async fn list_openai_compatible_models(
    base_url: &str,
) -> Result<Vec<String>, ListModelsError> {
    let origin = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let client = reqwest::Client::builder()
        .timeout(LIST_MODELS_TIMEOUT)
        .build()
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    let response = client
        .get(format!("{origin}/v1/models"))
        .send()
        .await
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ListModelsError::Unsupported);
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| ListModelsError::Unsupported)?;
    let models = parse_openai_models(&json);
    if models.is_empty() {
        return Err(ListModelsError::Unsupported);
    }
    Ok(models)
}

fn parse_openai_models(json: &serde_json::Value) -> Vec<String> {
    json.get("data")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("id").and_then(|id| id.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ollama_tags_response() {
        let json = serde_json::json!({
            "models": [
                {"name": "qwen3.5:9b", "size": 123},
                {"name": "llama3.2:3b", "size": 456}
            ]
        });
        assert_eq!(
            parse_ollama_tags(&json),
            vec!["qwen3.5:9b".to_string(), "llama3.2:3b".to_string()]
        );
    }

    #[test]
    fn parses_ollama_tags_response_with_no_models_field() {
        assert_eq!(parse_ollama_tags(&serde_json::json!({})), Vec::<String>::new());
    }

    #[test]
    fn parses_openai_models_response() {
        let json = serde_json::json!({
            "data": [
                {"id": "gpt-4o", "object": "model"},
                {"id": "gpt-4o-mini", "object": "model"}
            ]
        });
        assert_eq!(
            parse_openai_models(&json),
            vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]
        );
    }

    #[test]
    fn parses_openai_models_response_with_no_data_field() {
        assert_eq!(parse_openai_models(&serde_json::json!({})), Vec::<String>::new());
    }

    #[test]
    fn parses_openai_models_response_skipping_entries_with_no_id() {
        let json = serde_json::json!({
            "data": [
                {"id": "gpt-4o"},
                {"object": "model"}
            ]
        });
        assert_eq!(parse_openai_models(&json), vec!["gpt-4o".to_string()]);
    }
}
