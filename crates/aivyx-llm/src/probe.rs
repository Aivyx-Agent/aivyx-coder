//! Best-effort startup probe of the server's *actually served* context
//! window, so a mismatch with `backend.context_tokens` becomes a loud
//! warning instead of silent mid-response truncation.
//!
//! Born from a live diagnosis (see ROADMAP.md Phase 2/10): Ollama serves a
//! 4096-token default regardless of configuration unless the model or
//! service says otherwise, the `/v1` API cannot change it per-call, and a
//! reasoning model's thinking phase burns through the remainder invisibly.
//!
//! Three provider-specific endpoints are tried (llama-server, Ollama,
//! Lemonade Server); a server exposing none is simply `Unknown` — this is
//! advisory, never a gate.

use std::time::Duration;

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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

    // Lemonade Server: GET <base>/health lists each loaded model with the
    // context size it was launched with. Only a loaded model is listed.
    if let Ok(response) = client
        .get(format!("{}/health", base_url.trim_end_matches('/')))
        .send()
        .await
        && response.status().is_success()
        && let Ok(json) = response.json::<serde_json::Value>().await
        && let Some(n_ctx) = parse_lemonade_health(&json, model)
    {
        return ServedContext::Known(n_ctx);
    }

    ServedContext::Unknown
}

/// How long `verify_model_responds` waits: long enough for a server that
/// loads the model on first use (Lemonade, Ollama) to load it.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(180);

/// Asks `model` for a one-token reply at `base_url` (the backend's `.../v1`
/// URL). Also loads the model on servers that load on first use, so a
/// following `probe_served_context` can see it.
pub async fn verify_model_responds(base_url: &str, model: &str) -> Result<(), String> {
    let base = base_url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(VERIFY_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(format!("{base}/chat/completions"))
        .json(&serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "Reply with OK." }],
            "max_tokens": 1,
            "stream": false,
        }))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                format!("no reply from {base} within {}s", VERIFY_TIMEOUT.as_secs())
            } else {
                format!("couldn't reach {base} -- is the server running?")
            }
        })?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    let body: String = body.chars().take(200).collect();
    Err(format!("the server answered {status}: {}", body.trim()))
}

/// The context size Lemonade Server launched `model` with, from its
/// `/health` body: `recipe_options.ctx_size`, else the `--ctx-size` in its
/// launch command. `None` when `model` isn't loaded.
fn parse_lemonade_health(json: &serde_json::Value, model: &str) -> Option<u32> {
    let entry = json
        .get("all_models_loaded")?
        .as_array()?
        .iter()
        .find(|m| m.get("model_name").and_then(|n| n.as_str()) == Some(model))?;
    let from_options = entry
        .get("recipe_options")
        .and_then(|o| o.get("ctx_size"))
        .and_then(|n| n.as_u64());
    let from_command = || {
        let args = entry.get("launch_command")?.as_array()?;
        let flag = args.iter().position(|a| a.as_str() == Some("--ctx-size"))?;
        args.get(flag + 1)?.as_str()?.parse::<u64>().ok()
    };
    from_options.or_else(from_command).map(|n| n as u32)
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

    /// Trimmed from a real Lemonade Server 11.9 `/api/v1/health` with one
    /// model loaded.
    fn lemonade_health() -> serde_json::Value {
        serde_json::json!({
            "all_models_loaded": [{
                "backend_url": "http://127.0.0.1:8001/v1",
                "launch_command": ["llama-server", "-m", "m.gguf", "--ctx-size", "217162", "--port", "8001"],
                "loaded": true,
                "max_context_window": 262144,
                "model_name": "Qwen3-4B-Instruct-2507-GGUF",
                "recipe": "llamacpp",
                "recipe_options": { "ctx_size": 217162, "llamacpp_args": "--temp 0.6" },
                "type": "llm"
            }],
            "model_loaded": null,
            "status": "ok",
            "version": "11.9.0"
        })
    }

    #[test]
    fn parses_the_served_window_from_lemonade_health() {
        let json = lemonade_health();
        assert_eq!(
            parse_lemonade_health(&json, "Qwen3-4B-Instruct-2507-GGUF"),
            Some(217162)
        );
        // Another model, or none loaded: no claim.
        assert_eq!(parse_lemonade_health(&json, "Qwen3.5-9B-GGUF"), None);
        let empty = serde_json::json!({ "all_models_loaded": [], "status": "ok" });
        assert_eq!(parse_lemonade_health(&empty, "Qwen3-4B-Instruct-2507-GGUF"), None);
    }

    #[test]
    fn lemonade_health_falls_back_to_the_launch_command() {
        let mut json = lemonade_health();
        json["all_models_loaded"][0]
            .as_object_mut()
            .unwrap()
            .remove("recipe_options");
        assert_eq!(
            parse_lemonade_health(&json, "Qwen3-4B-Instruct-2507-GGUF"),
            Some(217162)
        );
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
