//! Best-effort startup probe of the server's *actually served* context
//! window, so a mismatch with `backend.context_tokens` becomes a loud
//! warning instead of silent mid-response truncation.
//!
//! Born from a live diagnosis (see ROADMAP.md Phase 2/10): Ollama serves a
//! 4096-token default regardless of configuration unless the model or
//! service says otherwise, the `/v1` API cannot change it per-call, and a
//! reasoning model's thinking phase burns through the remainder invisibly.
//!
//! Two provider-specific endpoints are tried; a server exposing neither is
//! simply `Unknown` — this is advisory, never a gate.

use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServedContext {
    /// The server reports serving this many tokens of context.
    Known(u32),
    /// An Ollama server whose model sets no `num_ctx`: the served window is
    /// the server's default (typically 4096, or `OLLAMA_CONTEXT_LENGTH`),
    /// which the API does not expose.
    OllamaDefaultUnknown,
    /// Nothing recognizable answered; no claim either way.
    Unknown,
}

/// Queries the server behind `base_url` (the same `.../v1` URL the backend
/// uses) for its served context window.
pub async fn probe_served_context(base_url: &str, model: &str) -> ServedContext {
    let origin = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(_) => return ServedContext::Unknown,
    };

    // llama-server: GET /props carries default_generation_settings.n_ctx.
    if let Ok(response) = client.get(format!("{origin}/props")).send().await
        && response.status().is_success()
        && let Ok(json) = response.json::<serde_json::Value>().await
        && let Some(n_ctx) = parse_llama_props(&json)
    {
        return ServedContext::Known(n_ctx);
    }

    // Ollama: POST /api/show returns the model's parameters (num_ctx only
    // if the Modelfile sets one).
    if let Ok(response) = client
        .post(format!("{origin}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        && response.status().is_success()
        && let Ok(json) = response.json::<serde_json::Value>().await
    {
        return match parse_ollama_show(&json) {
            Some(num_ctx) => ServedContext::Known(num_ctx),
            None => ServedContext::OllamaDefaultUnknown,
        };
    }

    ServedContext::Unknown
}

fn parse_llama_props(json: &serde_json::Value) -> Option<u32> {
    json.get("default_generation_settings")?
        .get("n_ctx")?
        .as_u64()
        .map(|n| n as u32)
}

/// `total_slots` + `build_info` from a real llama-server `/props`
/// response -- the same JSON body `probe_served_context` already fetches
/// for context-window detection, so a caller wanting both should parse
/// the one response with both this function and `parse_llama_props`
/// rather than fetching `/props` twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaSlotsInfo {
    pub total_slots: u32,
    pub build_info: String,
}

pub fn parse_llama_slots_info(json: &serde_json::Value) -> Option<LlamaSlotsInfo> {
    let total_slots = json.get("total_slots")?.as_u64()? as u32;
    let build_info = json.get("build_info")?.as_str()?.to_string();
    Some(LlamaSlotsInfo { total_slots, build_info })
}

/// The `parameters` field is the Modelfile parameter block as plain text,
/// one `key value` pair per line.
fn parse_ollama_show(json: &serde_json::Value) -> Option<u32> {
    let parameters = json.get("parameters")?.as_str()?;
    for line in parameters.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("num_ctx") {
            return parts.next()?.parse().ok();
        }
    }
    None
}

/// The advisory to surface when the served window is smaller than what the
/// agent budgets against, or unknowable. `None` means all is well (or
/// nothing useful can be said).
pub fn context_warning(configured: u32, served: &ServedContext) -> Option<String> {
    match served {
        ServedContext::Known(n) if *n < configured => Some(format!(
            "the server reports serving a {n}-token context window, but \
             backend.context_tokens is {configured} — responses will truncate before the \
             agent expects. Lower context_tokens or serve the model with a larger window."
        )),
        ServedContext::Known(_) => None,
        ServedContext::OllamaDefaultUnknown => Some(format!(
            "this Ollama model sets no num_ctx, so Ollama serves its own default window \
             (typically 4096) regardless of backend.context_tokens ({configured}) — and /v1 \
             cannot change it. If responses truncate mid-thought, create a derived model: \
             printf 'FROM <model>\\nPARAMETER num_ctx {configured}\\n' | ollama create <model>-ctx -f -"
        )),
        ServedContext::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_llama_server_props() {
        let json = serde_json::json!({
            "default_generation_settings": { "n_ctx": 16384, "seed": -1 },
            "model_path": "/x/y.gguf"
        });
        assert_eq!(parse_llama_props(&json), Some(16384));
        assert_eq!(parse_llama_props(&serde_json::json!({})), None);
    }

    #[test]
    fn parses_ollama_show_with_and_without_num_ctx() {
        let with = serde_json::json!({
            "parameters": "temperature 1\nnum_ctx 8192\ntop_k 20"
        });
        assert_eq!(parse_ollama_show(&with), Some(8192));

        let without = serde_json::json!({ "parameters": "temperature 1\ntop_k 20" });
        assert_eq!(parse_ollama_show(&without), None);
        assert_eq!(parse_ollama_show(&serde_json::json!({})), None);
    }

    #[test]
    fn warning_fires_only_when_it_should() {
        assert!(context_warning(8192, &ServedContext::Known(4096)).is_some());
        assert!(context_warning(8192, &ServedContext::Known(16384)).is_none());
        assert!(context_warning(8192, &ServedContext::Known(8192)).is_none());
        let ollama = context_warning(8192, &ServedContext::OllamaDefaultUnknown);
        assert!(ollama.is_some_and(|w| w.contains("num_ctx")));
        assert!(context_warning(8192, &ServedContext::Unknown).is_none());
    }

    #[test]
    fn parses_llama_slots_info_from_real_props_shape() {
        // Real /props response shape, confirmed against a live llama-server
        // on the GPU test rig (2026-08-21) -- trimmed to the fields this
        // parser reads plus enough surrounding structure to be realistic.
        let json = serde_json::json!({
            "default_generation_settings": {"params": {}},
            "total_slots": 4,
            "model_path": "/home/julian/models/Qwen3.5-9B-Q4_K_M.gguf",
            "build_info": "b10107-3121043"
        });
        let info = parse_llama_slots_info(&json).expect("must parse a real llama-server /props body");
        assert_eq!(info.total_slots, 4);
        assert_eq!(info.build_info, "b10107-3121043");
    }

    #[test]
    fn parse_llama_slots_info_returns_none_when_fields_are_absent() {
        let json = serde_json::json!({"some_other_server": true});
        assert!(parse_llama_slots_info(&json).is_none());
    }
}
