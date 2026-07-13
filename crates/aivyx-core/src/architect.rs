//! Architect/editor model-pairing (`/architect <task>`, ROADMAP.md Phase 9):
//! a single, separately configured model ("architect") produces a prose
//! implementation plan for a stated task; the plan is then handed directly
//! to the primary/editor model's own turn loop (see
//! `Agent::run_architect_turn` in `agent.rs`), which begins calling edit
//! tools on it immediately. Unlike `/council`, there is no deliberation and
//! no ranking — exactly one seat, one planning call.

use std::sync::Arc;

use aivyx_llm::{ChatRequest, LlmBackend};
use aivyx_types::{Message, Role};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentEvent;
use crate::council::CollectError;

const ARCHITECT_PROMPT: &str = "You are a senior engineer producing a concrete implementation plan \
     for the task below, to be executed by a separate, faster model. Describe what to change and \
     why, file by file where relevant. Do not write the actual diffs or full file contents — the \
     executing model will produce those. Keep the plan concrete and actionable, not exploratory.";

/// One architect seat: a display name for the transcript plus the backend
/// that produces the plan. Mirrors `council::CouncilSeat`'s shape.
pub struct ArchitectSeat {
    pub model: String,
    pub backend: Arc<dyn LlmBackend>,
}

/// The configured architect, if any — `tail_budget_tokens` lives here
/// (rather than on `ArchitectSeat`) exactly as `council::Council` wraps its
/// seats with `tail_budget_tokens`, since a bare seat carries no notion of
/// how much conversation context it should see.
pub struct Architect {
    pub seat: ArchitectSeat,
    pub tail_budget_tokens: u32,
}

/// Recognizes `/architect` / `/architect <task>` (and nothing else — a
/// message merely starting with those letters is a normal turn). Returns
/// the task, empty for the bare form — callers decide what an empty task
/// means (see `Agent::run_architect_turn`, which treats it as a usage
/// note rather than falling back to reviewing prior conversation the way
/// `/council`'s bare form does).
pub fn parse_command(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("/architect")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Makes one no-tools planning call to the architect's backend and returns
/// the produced plan text, or `None` on any failure (backend error, an
/// empty/near-empty response after `<think>`-stripping, or cancellation) —
/// every failure path emits its own `AgentEvent::ArchitectNote` explaining
/// why, mirroring `council::convene`'s "a failed stage is a transcript
/// note, not an `AgentError`" contract.
pub(crate) async fn plan(
    seat: &ArchitectSeat,
    subject: &str,
    context: Option<String>,
    events: &UnboundedSender<AgentEvent>,
    cancellation: &CancellationToken,
) -> Option<String> {
    let note = |text: String| {
        let _ = events.send(AgentEvent::ArchitectNote(text));
    };

    // Checked before the single planning call, mirroring
    // `council::convene`'s per-stage check before each `collect_text` call:
    // without it, a pre-cancelled token races `collect_text`'s internal
    // `tokio::select!` against an already-ready stream and is only
    // sometimes observed.
    if cancellation.is_cancelled() {
        note("architect planning cancelled — nothing was executed".to_string());
        return None;
    }

    note(format!(
        "{} is planning… (cold model loads can take a while)",
        seat.model
    ));

    let mut user_prompt = String::new();
    if let Some(context) = &context {
        user_prompt.push_str(context);
        user_prompt.push_str("\n\n");
    }
    user_prompt.push_str("The task to plan:\n");
    user_prompt.push_str(subject);

    let request = ChatRequest::new(vec![
        Message::text(Role::System, ARCHITECT_PROMPT),
        Message::text(Role::User, user_prompt),
    ]);

    match crate::council::collect_text(seat.backend.as_ref(), request, cancellation).await {
        Ok(text) => {
            let text = crate::council::strip_think(&text);
            if text.chars().count() < crate::council::MIN_ANSWER_CHARS {
                note(format!(
                    "{} produced no usable plan — nothing was executed",
                    seat.model
                ));
                None
            } else {
                note(format!("plan from {}:\n{text}", seat.model));
                Some(text)
            }
        }
        Err(CollectError::Cancelled) => {
            note("architect planning cancelled — nothing was executed".to_string());
            None
        }
        Err(CollectError::Backend(err)) => {
            note(format!(
                "{} failed to produce a plan ({err}) — nothing was executed",
                seat.model
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_recognizes_bare_and_task_forms() {
        assert_eq!(parse_command("/architect"), Some(""));
        assert_eq!(parse_command("  /architect  "), Some(""));
        assert_eq!(
            parse_command("/architect refactor the auth module"),
            Some("refactor the auth module")
        );
    }

    #[test]
    fn parse_command_rejects_lookalikes_and_normal_messages() {
        assert_eq!(parse_command("/architecture"), None);
        assert_eq!(parse_command("run /architect for me"), None);
        assert_eq!(parse_command("architect"), None);
    }
}
