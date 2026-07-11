//! Council mode (Phase 11a, see ROADMAP.md): several local models
//! independently answer a hard question, anonymously rank each other's
//! answers, and a chairman synthesizes a recommendation. Members receive no
//! tools, so a council adds zero permission surface. The full deliberation
//! is only ever rendered to the TUI; nothing but the chairman's synthesis
//! may enter the agent's history, and a failed council contributes nothing.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::Arc;

use aivyx_llm::{ChatRequest, LlmBackend, StreamEvent};
use aivyx_types::{ContentBlock, Message, Role};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentEvent;

/// Caps one collected response — far above any sane answer, but a
/// misbehaving backend that streams forever must not accumulate unbounded.
const MAX_COUNCIL_TEXT_BYTES: usize = 512 * 1024;

/// A council answer must be at least this long after `<think>` stripping to
/// count toward quorum — an empty or near-empty answer (e.g. a reasoning
/// model that spent its whole response inside an unclosed think block) is a
/// member failure, not a contribution.
const MIN_ANSWER_CHARS: usize = 20;

const ADVISOR_PROMPT: &str = "You are one advisor on a council of several independent AI models, \
     consulted on a difficult software or design question. Answer on your \
     own judgment: take a clear position, give concrete reasoning, and keep \
     it under roughly 400 words. If the question genuinely depends on \
     unstated factors, say which way you lean and why.";

const RANKER_PROMPT: &str = "You are one advisor on a council of AI models. Several advisors (you \
     among them, all anonymized) answered the question below. Rank ALL \
     answers from best to worst, one line each, in the form:\n\
     1. Advisor X — one-line justification\n\
     Judge only correctness, concreteness, and usefulness to the person \
     deciding. Do not reward length or confident tone.";

const CHAIRMAN_PROMPT: &str = "You chair a council of AI models. Below are the question, each \
     advisor's anonymized answer, and the advisors' rankings of one \
     another. Synthesize ONE final recommendation for the user. Be \
     decisive: state the recommendation first, then the key reasons, then \
     any genuine disagreement among advisors the user should know about. \
     Keep it under roughly 500 words.";

/// One seat: a display name for the transcript plus the backend that
/// answers for it. Any OpenAI-compatible endpoint qualifies, so one council
/// can mix Ollama-swapped models with a resident llama-server.
pub struct CouncilSeat {
    pub model: String,
    pub backend: Arc<dyn LlmBackend>,
}

pub struct Council {
    /// At least two — enforced by config (`CouncilSettings::configured`)
    /// and re-checked by quorum at runtime.
    pub members: Vec<CouncilSeat>,
    pub chairman: CouncilSeat,
    /// Token budget for the conversation-tail digest members see alongside
    /// the question; converted to chars with the agent's calibrated ratio.
    pub tail_budget_tokens: u32,
}

/// Recognizes `/council` / `/council <question>` (and nothing else — a
/// message merely starting with those letters is a normal turn). Returns
/// the question, empty for the bare form.
pub fn parse_command(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix("/council")?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// The most recent assistant text in history — the subject of a bare
/// `/council` ("review what you just proposed").
pub fn last_assistant_text(history: &[Message]) -> Option<String> {
    history.iter().rev().find_map(|message| {
        if message.role != Role::Assistant {
            return None;
        }
        let text: String = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    })
}

/// Renders the newest slice of the conversation that fits `budget_chars`
/// as plain "User:"/"Assistant:" lines (tool traffic reduced to one-line
/// markers — members can't act on it and full results would drown the
/// budget). `None` when there's no history worth digesting.
pub fn tail_digest(history: &[Message], budget_chars: usize) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;

    for message in history.iter().rev() {
        let rendered = render_for_digest(message)?;
        if rendered.is_empty() {
            continue;
        }
        used += rendered.chars().count();
        if used > budget_chars && !parts.is_empty() {
            break;
        }
        parts.push(rendered);
        if used > budget_chars {
            break;
        }
    }

    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    Some(format!(
        "Recent conversation (for context):\n{}",
        parts.join("\n")
    ))
}

/// One digest line per message. Never fails — the `Option` is only so the
/// `?` above reads cleanly; `None` is never actually produced.
fn render_for_digest(message: &Message) -> Option<String> {
    let prefix = match message.role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::System => return Some(String::new()),
        Role::Tool => {
            return Some("[tool result omitted]".to_string());
        }
    };
    let mut lines: Vec<String> = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text(text) if !text.trim().is_empty() => {
                lines.push(format!("{prefix}: {}", text.trim()));
            }
            ContentBlock::ToolCall(call) => {
                lines.push(format!("[{prefix} ran tool: {}]", call.name));
            }
            _ => {}
        }
    }
    Some(lines.join("\n"))
}

/// Removes `<think>…</think>` spans (and an unclosed trailing `<think>…`)
/// that some serving paths leak into content verbatim — the Phase 10 lesson
/// that reasoning markup shows up where you least expect it, applied
/// defensively here since council answers skip all tool-call parsing.
fn strip_think(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Convenes the full council. Everything observable streams out as
/// `CouncilNote` events; the return value is the single message allowed
/// into the agent's history (`None` on cancellation, quorum failure, or a
/// failed chairman — a council that didn't complete leaves no residue).
pub async fn convene(
    council: &Council,
    subject: &str,
    digest: Option<String>,
    events: &UnboundedSender<AgentEvent>,
    cancellation: &CancellationToken,
) -> Option<Message> {
    let note = |text: String| {
        let _ = events.send(AgentEvent::CouncilNote(text));
    };

    note(format!(
        "council convened — {} members, chairman {} — stage 1/3: independent answers",
        council.members.len(),
        council.chairman.model
    ));

    // Stage 1: answers, sequentially (one GPU; Ollama swaps per request).
    let mut answers: Vec<(usize, String)> = Vec::new();
    for (index, seat) in council.members.iter().enumerate() {
        if cancellation.is_cancelled() {
            note("council cancelled".to_string());
            return None;
        }
        note(format!(
            "{} is answering… (cold model loads can take a while)",
            seat.model
        ));
        let mut user_prompt = String::new();
        if let Some(digest) = &digest {
            user_prompt.push_str(digest);
            user_prompt.push_str("\n\n");
        }
        user_prompt.push_str("The question before the council:\n");
        user_prompt.push_str(subject);

        let request = ChatRequest::new(vec![
            Message::text(Role::System, ADVISOR_PROMPT),
            Message::text(Role::User, user_prompt),
        ]);
        match collect_text(seat.backend.as_ref(), request, cancellation).await {
            Ok(text) => {
                let text = strip_think(&text);
                if text.chars().count() < MIN_ANSWER_CHARS {
                    note(format!(
                        "{} produced no usable answer — continuing without it",
                        seat.model
                    ));
                } else {
                    note(format!("answer from {}:\n{text}", seat.model));
                    answers.push((index, text));
                }
            }
            Err(CollectError::Cancelled) => {
                note("council cancelled".to_string());
                return None;
            }
            Err(CollectError::Backend(err)) => {
                note(format!(
                    "{} failed ({err}) — continuing without it",
                    seat.model
                ));
            }
        }
    }

    if answers.len() < 2 {
        note(format!(
            "council aborted: only {} usable answer(s) collected (quorum is 2). \
             Nothing was added to the conversation.",
            answers.len()
        ));
        return None;
    }

    // Anonymize: shuffle so neither rankers nor the chairman can infer
    // authorship from member ordering. `RandomState` gives a fresh secret
    // ordering per council without pulling in a rand dependency.
    let state = RandomState::new();
    let mut order: Vec<usize> = (0..answers.len()).collect();
    order.sort_by_key(|i| state.hash_one(answers[*i].0));
    let labeled: Vec<(String, usize, &str)> = order
        .iter()
        .enumerate()
        .map(|(position, &i)| {
            let (member_index, answer) = &answers[i];
            (advisor_label(position), *member_index, answer.as_str())
        })
        .collect();

    let mut answers_block = String::new();
    for (label, _, answer) in &labeled {
        answers_block.push_str(&format!("Advisor {label}:\n{answer}\n\n"));
    }

    // Stage 2: each contributing member ranks all anonymized answers.
    // Failures here only cost information — the chairman can still
    // synthesize from the answers alone.
    note("stage 2/3: cross-ranking (answers anonymized)".to_string());
    let mut rankings: Vec<String> = Vec::new();
    for (member_index, _) in &answers {
        if cancellation.is_cancelled() {
            note("council cancelled".to_string());
            return None;
        }
        let seat = &council.members[*member_index];
        let request = ChatRequest::new(vec![
            Message::text(Role::System, RANKER_PROMPT),
            Message::text(
                Role::User,
                format!("The question:\n{subject}\n\nThe answers:\n\n{answers_block}"),
            ),
        ]);
        match collect_text(seat.backend.as_ref(), request, cancellation).await {
            Ok(text) => {
                let text = strip_think(&text);
                note(format!("ranking from {}:\n{text}", seat.model));
                rankings.push(text);
            }
            Err(CollectError::Cancelled) => {
                note("council cancelled".to_string());
                return None;
            }
            Err(CollectError::Backend(err)) => {
                note(format!("{} failed to rank ({err}) — skipping", seat.model));
            }
        }
    }

    // Stage 3: chairman synthesis — the only stage whose failure is fatal,
    // because only its output may enter history.
    note(format!(
        "stage 3/3: chairman {} synthesizing…",
        council.chairman.model
    ));
    let rankings_block = if rankings.is_empty() {
        "(no rankings were collected — synthesize from the answers alone)".to_string()
    } else {
        rankings
            .iter()
            .enumerate()
            .map(|(i, r)| format!("Ranking {}:\n{r}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let request = ChatRequest::new(vec![
        Message::text(Role::System, CHAIRMAN_PROMPT),
        Message::text(
            Role::User,
            format!(
                "The question:\n{subject}\n\nThe answers:\n\n{answers_block}\
                 The advisors' rankings of one another:\n\n{rankings_block}"
            ),
        ),
    ]);
    let synthesis =
        match collect_text(council.chairman.backend.as_ref(), request, cancellation).await {
            Ok(text) => {
                let text = strip_think(&text);
                if text.chars().count() < MIN_ANSWER_CHARS {
                    note(
                        "chairman produced no usable synthesis — the deliberation above \
                         stands, but nothing was added to the conversation"
                            .to_string(),
                    );
                    return None;
                }
                text
            }
            Err(CollectError::Cancelled) => {
                note("council cancelled".to_string());
                return None;
            }
            Err(CollectError::Backend(err)) => {
                note(format!(
                    "chairman {} failed ({err}) — the deliberation above stands, but \
                     nothing was added to the conversation",
                    council.chairman.model
                ));
                return None;
            }
        };

    let reveal: String = labeled
        .iter()
        .map(|(label, member_index, _)| {
            format!("Advisor {label} = {}", council.members[*member_index].model)
        })
        .collect::<Vec<_>>()
        .join(", ");
    note(format!(
        "council synthesis (chairman {}):\n{synthesis}\n\nidentities: {reveal}",
        council.chairman.model
    ));

    // User-role with an explicit marker: small local models attend to user
    // messages far more reliably than to injected system addenda, and the
    // marker keeps it from reading as something the human typed.
    Some(Message::text(
        Role::User,
        format!(
            "[Council synthesis — {} local models were convened on the question below; \
             this is the chairman's recommendation, not a message typed by the user.]\n\n\
             Question: {subject}\n\nRecommendation ({}, chairman):\n{synthesis}\n\n\
             (Advisors: {reveal})",
            answers.len(),
            council.chairman.model
        ),
    ))
}

fn advisor_label(position: usize) -> String {
    if position < 26 {
        char::from(b'A' + position as u8).to_string()
    } else {
        format!("{}", position + 1)
    }
}

enum CollectError {
    Cancelled,
    Backend(String),
}

/// Streams one no-tools chat call to completion and returns the
/// accumulated text. Tool-call and usage events are ignored — members are
/// never offered tools, and there's no context indicator to feed here.
async fn collect_text(
    backend: &dyn LlmBackend,
    request: ChatRequest,
    cancellation: &CancellationToken,
) -> Result<String, CollectError> {
    let mut stream = backend
        .stream_chat(request)
        .await
        .map_err(|err| CollectError::Backend(err.to_string()))?;

    let mut text = String::new();
    loop {
        let next_event = tokio::select! {
            _ = cancellation.cancelled() => return Err(CollectError::Cancelled),
            event = stream.next() => event,
        };
        let Some(event) = next_event else { break };
        match event {
            Ok(StreamEvent::TextDelta(delta)) => {
                text.push_str(&delta);
                if text.len() > MAX_COUNCIL_TEXT_BYTES {
                    return Err(CollectError::Backend(
                        "response exceeded the council's size cap".to_string(),
                    ));
                }
            }
            Ok(_) => {}
            Err(err) => return Err(CollectError::Backend(err.to_string())),
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_recognizes_bare_and_question_forms() {
        assert_eq!(parse_command("/council"), Some(""));
        assert_eq!(parse_command("  /council  "), Some(""));
        assert_eq!(
            parse_command("/council tabs or spaces?"),
            Some("tabs or spaces?")
        );
    }

    #[test]
    fn parse_command_rejects_lookalikes_and_normal_messages() {
        assert_eq!(parse_command("/councilfoo"), None);
        assert_eq!(parse_command("run /council for me"), None);
        assert_eq!(parse_command("council"), None);
    }

    #[test]
    fn strip_think_removes_closed_and_unclosed_spans() {
        assert_eq!(
            strip_think("<think>hmm</think>the answer"),
            "the answer"
        );
        assert_eq!(
            strip_think("first<think>a</think>mid<think>b</think>last"),
            "firstmidlast"
        );
        // An unclosed block swallows the rest — an all-thinking response
        // must come out empty so it fails the MIN_ANSWER_CHARS check.
        assert_eq!(strip_think("prefix<think>never closed"), "prefix");
        assert_eq!(strip_think("no markup at all"), "no markup at all");
    }

    #[test]
    fn last_assistant_text_finds_newest_nonempty_text() {
        let history = vec![
            Message::text(Role::Assistant, "old plan"),
            Message::text(Role::User, "hm"),
            Message::text(Role::Assistant, "new plan"),
            Message::text(Role::User, "unanswered"),
        ];
        assert_eq!(last_assistant_text(&history), Some("new plan".to_string()));
        assert_eq!(last_assistant_text(&[]), None);
    }

    #[test]
    fn tail_digest_keeps_the_newest_messages_within_budget() {
        let history = vec![
            Message::text(Role::User, "oldest message that should fall out"),
            Message::text(Role::Assistant, "middle"),
            Message::text(Role::User, "newest"),
        ];
        let digest = tail_digest(&history, 40).unwrap();
        assert!(digest.contains("User: newest"));
        assert!(!digest.contains("oldest"));
        // Newest-last ordering (chronological) after the reverse walk.
        let mid = digest.find("Assistant: middle").unwrap();
        let newest = digest.find("User: newest").unwrap();
        assert!(mid < newest);
    }

    #[test]
    fn tail_digest_always_includes_at_least_one_message() {
        let history = vec![Message::text(
            Role::User,
            "far longer than the tiny budget below",
        )];
        assert!(tail_digest(&history, 5).is_some());
        assert!(tail_digest(&[], 1000).is_none());
    }

    #[test]
    fn advisor_labels_are_letters_then_numbers() {
        assert_eq!(advisor_label(0), "A");
        assert_eq!(advisor_label(25), "Z");
        assert_eq!(advisor_label(26), "27");
    }
}
