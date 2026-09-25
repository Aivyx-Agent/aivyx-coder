//! `/models` (list, `refresh`, `why`) and `/model <id>|auto` — the model
//! router's operator surface. Handled inside `Agent::run_turn` so the TUI
//! and ACP frontends both get them, and never reach the model.

use aivyx_llm::RoutedBackend;
use aivyx_route::{Availability, ModelKey, ModelProfile, find};

use crate::commands::parse_slash_command;

/// Shown when this agent has no router: routing is off, or this is not the
/// interactive session (sub-agents and MCP sessions never get one).
const ROUTING_OFF: &str = "Model routing commands are not available here — they need [routing] enabled = true and the interactive session.";

/// `None` when `input` is not a routing command.
pub async fn run(router: Option<&RoutedBackend>, session: &str, input: &str) -> Option<String> {
    if let Some(arg) = parse_slash_command(input, "/models") {
        let Some(router) = router else {
            return Some(ROUTING_OFF.to_string());
        };
        return Some(match arg {
            "refresh" => match router.refresh().await {
                Ok(n) => format!("Refreshed: {n} candidate models."),
                Err(e) => format!("Could not refresh: {e}."),
            },
            "why" => match router.last_decision(session) {
                Some(r) => format!("`{}` ({}): {}", r.model, r.task.name(), r.reason),
                None => "No routed call yet in this conversation.".to_string(),
            },
            "" => format_models(
                &router.profiles(),
                router.current(session).as_ref(),
                router.pinned(session).as_ref(),
            ),
            other => format!(
                "Unknown /models argument `{other}` — try /models, /models refresh or /models why."
            ),
        });
    }
    let arg = parse_slash_command(input, "/model")?;
    let Some(router) = router else {
        return Some(ROUTING_OFF.to_string());
    };
    Some(match arg {
        "" => match router.pinned(session) {
            Some(k) => format!("Pinned to `{k}`. Use /model auto to let routing choose again."),
            None => {
                "Not pinned; routing chooses this conversation's model. Use /model <id> to pin."
                    .to_string()
            }
        },
        "auto" => {
            router.unpin(session);
            "Pin cleared; routing chooses this conversation's model again on the next call."
                .to_string()
        }
        arg => match resolve(&router.profiles(), arg) {
            Ok(k) => {
                router.pin(session, k.clone());
                format!("Pinned this conversation to `{k}`.")
            }
            Err(e) => e,
        },
    })
}

/// `id@endpoint`, or a bare `id` served by exactly one endpoint.
pub fn resolve(profiles: &[ModelProfile], arg: &str) -> Result<ModelKey, String> {
    if let Some((id, endpoint)) = arg.rsplit_once('@') {
        let k = ModelKey {
            endpoint: aivyx_route::EndpointRef::new(endpoint),
            id: id.to_string(),
        };
        return find(profiles, &k)
            .map(ModelProfile::key)
            .ok_or_else(|| format!("No model `{arg}` — see /models."));
    }
    let matches: Vec<ModelKey> = profiles
        .iter()
        .filter(|p| p.id == arg)
        .map(ModelProfile::key)
        .collect();
    match matches.as_slice() {
        [] => Err(format!("No model `{arg}` — see /models.")),
        [one] => Ok(one.clone()),
        many => {
            let names: Vec<String> = many.iter().map(ToString::to_string).collect();
            Err(format!(
                "`{arg}` is served by several endpoints ({}) — use id@endpoint.",
                names.join(", ")
            ))
        }
    }
}

/// One line per candidate: `* ` marks the conversation's current model;
/// unknown capabilities carry a `?`.
pub fn format_models(
    profiles: &[ModelProfile],
    current: Option<&ModelKey>,
    pinned: Option<&ModelKey>,
) -> String {
    let mut out = String::from("Routing candidates (* = this conversation's model):");
    for p in profiles {
        let key = p.key();
        let marker = if current == Some(&key) { "* " } else { "  " };
        let mut caps: Vec<String> = p.capabilities.iter().map(ToString::to_string).collect();
        caps.extend(p.unknown_capabilities.iter().map(|c| format!("{c}?")));
        let ctx = p
            .context_window
            .map_or_else(|| "?".to_string(), |n| n.to_string());
        let availability = match p.availability {
            Availability::Available => "available",
            Availability::Unverified => "unverified",
            Availability::Unavailable => "unavailable",
        };
        out.push_str(&format!(
            "\n{marker}{key} — {}, {}, ctx {ctx}, {availability}",
            p.tier,
            if caps.is_empty() {
                "no known capabilities".to_string()
            } else {
                caps.join(" ")
            },
        ));
        if pinned == Some(&key) {
            out.push_str(" (pinned)");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use aivyx_llm::{ChatRequest, LlmBackend, LlmError, RoutedBackend, StreamEvent};
    use aivyx_route::{Capability, EndpointRef, TaskOverrides, Tier};
    use futures::stream::BoxStream;

    fn profile(endpoint: &str, id: &str, tier: Tier) -> ModelProfile {
        let mut p = ModelProfile::new(id, EndpointRef::new(endpoint));
        p.tier = tier;
        p.capabilities.insert(Capability::Completion);
        p
    }

    fn key(endpoint: &str, id: &str) -> ModelKey {
        ModelKey {
            endpoint: EndpointRef::new(endpoint),
            id: id.into(),
        }
    }

    struct Never;

    #[async_trait::async_trait]
    impl LlmBackend for Never {
        fn model_id(&self) -> &str {
            "default"
        }
        async fn stream_chat(
            &self,
            _: ChatRequest,
        ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
            Err(LlmError::Timeout)
        }
    }

    fn router() -> RoutedBackend {
        RoutedBackend::new(
            key("backend", "default"),
            Arc::new(Never),
            vec![
                profile("backend", "default", Tier::Medium),
                profile("a-gpu", "qwen3:8b", Tier::Small),
                profile("b-gpu", "qwen3:8b", Tier::Small),
                profile("a-gpu", "coder", Tier::Large),
            ],
            TaskOverrides::default(),
            Box::new(|k: &ModelKey| Err(format!("no `{k}`"))),
        )
    }

    #[test]
    fn format_models_marks_current_and_pinned() {
        let mut coder = profile("a-gpu", "coder", Tier::Large);
        coder.capabilities.insert(Capability::Tools);
        coder.unknown_capabilities.insert(Capability::Vision);
        coder.context_window = Some(32_768);
        let text = format_models(
            &[profile("backend", "default", Tier::Medium), coder],
            Some(&key("a-gpu", "coder")),
            Some(&key("a-gpu", "coder")),
        );
        let line = text.lines().find(|l| l.contains("coder@a-gpu")).unwrap();
        assert!(line.starts_with("* "), "{line}");
        assert!(line.contains("large"), "{line}");
        assert!(line.contains("tools"), "{line}");
        assert!(line.contains("vision?"), "{line}");
        assert!(line.contains("ctx 32768"), "{line}");
        assert!(line.contains("pinned"), "{line}");
        let other = text
            .lines()
            .find(|l| l.contains("default@backend"))
            .unwrap();
        assert!(other.starts_with("  "), "{other}");
        assert!(other.contains("ctx ?"), "{other}");
    }

    #[test]
    fn resolve_accepts_bare_ids_and_disambiguates() {
        let ps = router().profiles();
        assert_eq!(resolve(&ps, "coder"), Ok(key("a-gpu", "coder")));
        assert_eq!(resolve(&ps, "qwen3:8b@b-gpu"), Ok(key("b-gpu", "qwen3:8b")));
        let err = resolve(&ps, "qwen3:8b").unwrap_err();
        assert!(
            err.contains("qwen3:8b@a-gpu") && err.contains("qwen3:8b@b-gpu"),
            "{err}"
        );
        assert!(resolve(&ps, "nope").unwrap_err().contains("/models"));
    }

    #[tokio::test]
    async fn non_commands_pass_through() {
        assert_eq!(run(Some(&router()), "s", "hello").await, None);
        assert_eq!(run(Some(&router()), "s", "/modelsx").await, None);
    }

    #[tokio::test]
    async fn routing_off_explains_itself() {
        let text = run(None, "s", "/models").await.unwrap();
        assert!(text.contains("[routing] enabled = true"), "{text}");
        // Also shown to agents without a router while routing is on
        // (sub-agents, MCP sessions), so it must not claim routing is off.
        assert!(!text.contains("routing is off"), "{text}");
        assert!(text.contains("interactive session"), "{text}");
    }

    #[tokio::test]
    async fn model_pins_and_auto_unpins() {
        let r = router();
        let text = run(Some(&r), "s", "/model coder").await.unwrap();
        assert!(text.contains("coder@a-gpu"), "{text}");
        assert_eq!(r.pinned("s"), Some(key("a-gpu", "coder")));
        let text = run(Some(&r), "s", "/model auto").await.unwrap();
        assert_eq!(
            text,
            "Pin cleared; routing chooses this conversation's model again on the next call."
        );
        assert_eq!(r.pinned("s"), None);
        let text = run(Some(&r), "s", "/model").await.unwrap();
        assert_eq!(
            text,
            "Not pinned; routing chooses this conversation's model. Use /model <id> to pin."
        );
        let text = run(Some(&r), "s", "/model qwen3:8b").await.unwrap();
        assert!(text.contains("several endpoints"), "{text}");
        assert_eq!(r.pinned("s"), None);
    }

    #[tokio::test]
    async fn models_why_and_refresh() {
        let r = router();
        let text = run(Some(&r), "s", "/models why").await.unwrap();
        assert!(text.contains("No routed call yet"), "{text}");
        let text = run(Some(&r), "s", "/models refresh").await.unwrap();
        assert!(text.contains("no discovery configured"), "{text}");
        let text = run(Some(&r), "s", "/models").await.unwrap();
        assert!(text.contains("coder@a-gpu"), "{text}");
    }
}
