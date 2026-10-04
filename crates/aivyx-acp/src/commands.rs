//! ACP-specific slash-command exposure: which of
//! `aivyx_core::commands::COMMANDS` this frontend advertises to the
//! editor's own command picker via `AvailableCommandsUpdate`, and the two
//! (`/help`, `/clear`) it now intercepts before `session/prompt` ever
//! hands the text to `Agent::run_turn`. Pure logic only, no I/O — see
//! `session.rs` for where `advertised_commands_update`/`acp_local_command`
//! actually get called against a real connection/session.

use agent_client_protocol::schema::v1::{
    AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate, ContentBlock, ContentChunk,
    SessionUpdate, TextContent, UnstructuredCommandInput,
};
use aivyx_core::commands::{COMMANDS, CommandInfo, CommandTier, parse_slash_command};

/// The exact reply ACP gives for `/clear` — identical wording to the TUI's
/// own `/clear` handler (`aivyx-tui/src/app.rs`), kept as a separate copy
/// since the two frontends don't share a text-formatting crate (same
/// precedent as `translate.rs`'s `strip_control_chars_for_display`).
pub(crate) const CLEAR_REPLY: &str = "New conversation — the previous one is in /sessions.";

/// Whether `cmd` is one this frontend exposes to the editor's command
/// picker: every `AgentTurn`- and `AgentState`-tier command except
/// `/resume`, plus `/help`. Going by tier means a new `FrontendOnly`
/// command (a TUI key-driven affordance like `/quit`) stays unadvertised
/// unless this frontend learns to handle it. `AgentTurn` commands
/// (`/council`, `/wiki`, `/architect`) reach the model as plain prompt
/// text through `run_turn`, and `AgentState` commands are intercepted at
/// the top of `run_turn` the same way; the editor's command picker is how
/// a user discovers them at all. `/resume` is excluded because ACP has no
/// in-process session-switch redraw path (see `translate.rs`'s note on
/// `AgentEvent::SessionSwitched`). `/help` is `FrontendOnly` but handled
/// locally here (see `acp_local_command` below), as is `/clear`.
fn is_advertised(cmd: &CommandInfo) -> bool {
    match cmd.tier {
        CommandTier::AgentTurn | CommandTier::AgentState => cmd.name != "/resume",
        CommandTier::FrontendOnly => cmd.name == "/help",
    }
}

/// The commands `available_commands_update` advertises, as plain
/// `AvailableCommand`s — split out from that function so it's directly
/// testable without constructing a `SessionUpdate`. `name` is the command
/// without its leading `/` (ACP's own convention — see the schema crate's
/// `AvailableCommand::name` doc, "e.g. `create_plan`"); `description` is
/// `CommandInfo.description` verbatim. `input` is set to the schema's
/// unstructured-input variant, carrying `CommandInfo.args_hint`, for any
/// command that has one — `None` (no `input` at all) for a command that
/// takes no arguments, so the editor's command picker shows no hint
/// placeholder for e.g. `/undo`.
pub(crate) fn advertised_commands() -> Vec<AvailableCommand> {
    COMMANDS
        .iter()
        .filter(|cmd| is_advertised(cmd))
        .map(|cmd| {
            let command =
                AvailableCommand::new(cmd.name.trim_start_matches('/'), cmd.description);
            match cmd.args_hint {
                Some(hint) => command.input(AvailableCommandInput::Unstructured(
                    UnstructuredCommandInput::new(hint),
                )),
                None => command,
            }
        })
        .collect()
}

/// The `SessionUpdate` `session.rs` sends once, right after `session/new`
/// succeeds, so the editor's command picker knows what this agent
/// supports from the very first turn.
pub(crate) fn available_commands_update() -> SessionUpdate {
    SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(advertised_commands()))
}

/// Mirrors the TUI's own `help_text` (`aivyx-tui/src/app.rs`) minus its
/// "Keys:" section — that section documents terminal key bindings
/// (`Ctrl+P`, the approval prompt's `y`/`a`/`n`, …) that have no meaning in
/// an editor, which drives its own UI instead. Lists exactly the commands
/// `advertised_commands` advertises, so this text can never claim a
/// command ACP doesn't actually support. Rendered as a markdown list,
/// after a leading blank line (as `translate.rs` gives `Info` chunks), so
/// an editor that renders agent messages as markdown shows one command per
/// line instead of folding the listing into a single paragraph.
pub(crate) fn acp_help_text() -> String {
    let mut text = "\n\nAvailable commands:\n".to_string();
    for cmd in COMMANDS.iter().filter(|cmd| is_advertised(cmd)) {
        match cmd.args_hint {
            Some(hint) => {
                text.push_str(&format!("\n- `{} <{hint}>` — {}", cmd.name, cmd.description))
            }
            None => text.push_str(&format!("\n- `{}` — {}", cmd.name, cmd.description)),
        }
    }
    text
}

/// The two slash commands this frontend intercepts before `Agent::run_turn`
/// ever sees the prompt text — see `session.rs`'s `PromptRequest` handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalCommand {
    Help,
    Clear,
}

/// Recognizes a bare `/help` or `/clear` in `text`, word-boundary aware via
/// the same `parse_slash_command` every other command's own parser uses
/// (so `/helpful`/`/clearly` don't misfire, and `/clear now` still counts
/// as `/clear` — the TUI's own `/clear` handler ignores trailing args the
/// same way). Anything else — including every other known command, which
/// `run_turn` already handles on its own — falls through to `None`.
pub(crate) fn acp_local_command(text: &str) -> Option<LocalCommand> {
    if parse_slash_command(text, "/help").is_some() {
        Some(LocalCommand::Help)
    } else if parse_slash_command(text, "/clear").is_some() {
        Some(LocalCommand::Clear)
    } else {
        None
    }
}

/// A single-chunk `AgentMessageChunk` carrying `text` — the shape both
/// `/help`'s listing and `/clear`'s confirmation reply take. Duplicated
/// rather than reused from `translate.rs`'s own private `text_chunk`
/// (module-private there, and this project's established precedent is a
/// small duplicate over a cross-module `pub(crate)` for a helper this
/// size — see `translate.rs`'s own `strip_control_chars_for_display` doc
/// comment).
fn agent_message(text: String) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
        text,
    ))))
}

/// The reply for a locally-handled `/help` — never reaches `Agent::run_turn`
/// or the model.
pub(crate) fn help_update() -> SessionUpdate {
    agent_message(acp_help_text())
}

/// The reply for a locally-handled `/clear`, sent after the caller has
/// already called `Agent::clear_conversation()` — never reaches
/// `Agent::run_turn` or the model.
pub(crate) fn clear_update() -> SessionUpdate {
    agent_message(CLEAR_REPLY.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised_names() -> Vec<String> {
        advertised_commands().into_iter().map(|c| c.name).collect()
    }

    #[test]
    fn advertised_commands_includes_every_agent_state_command_plus_help() {
        let names = advertised_names();
        for expected in ["undo", "diff", "test", "sessions", "help", "clear"] {
            assert!(
                names.iter().any(|n| n == expected),
                "expected {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn advertised_commands_excludes_resume_and_quit() {
        let names = advertised_names();
        assert!(!names.contains(&"resume".to_string()), "{names:?}");
        assert!(!names.contains(&"quit".to_string()), "{names:?}");
    }

    #[test]
    fn advertised_commands_includes_agent_turn_commands() {
        let names = advertised_names();
        for expected in ["council", "wiki", "architect"] {
            assert!(
                names.iter().any(|n| n == expected),
                "expected {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn advertised_commands_strip_the_leading_slash() {
        for cmd in advertised_commands() {
            assert!(!cmd.name.starts_with('/'), "{}", cmd.name);
        }
    }

    #[test]
    fn advertised_commands_carry_the_real_description() {
        let clear = COMMANDS.iter().find(|c| c.name == "/clear").unwrap();
        let advertised = advertised_commands()
            .into_iter()
            .find(|c| c.name == "clear")
            .unwrap();
        assert_eq!(advertised.description, clear.description);
    }

    #[test]
    fn acp_local_command_recognizes_help_and_clear() {
        assert_eq!(acp_local_command("/help"), Some(LocalCommand::Help));
        assert_eq!(acp_local_command("  /help  "), Some(LocalCommand::Help));
        assert_eq!(acp_local_command("/clear"), Some(LocalCommand::Clear));
        assert_eq!(acp_local_command("/clear now"), Some(LocalCommand::Clear));
    }

    #[test]
    fn acp_local_command_rejects_lookalikes_and_other_commands() {
        assert_eq!(acp_local_command("/helpful"), None);
        assert_eq!(acp_local_command("/clearly"), None);
        assert_eq!(acp_local_command("/undo"), None);
        assert_eq!(acp_local_command("fix the bug"), None);
    }

    #[test]
    fn acp_help_text_lists_the_advertised_commands_only() {
        let text = acp_help_text();
        assert!(text.contains("Available commands:"));
        for cmd in COMMANDS.iter().filter(|c| is_advertised(c)) {
            assert!(
                text.contains(cmd.name) && text.contains(cmd.description),
                "missing {}: {text}",
                cmd.name
            );
        }
        // Excluded commands must not leak in regardless.
        assert!(!text.contains("/resume"));
        assert!(!text.contains("/quit"));
        // No TUI-only Keys section.
        assert!(!text.contains("Keys:"));
    }

    #[test]
    fn is_advertised_goes_by_tier_not_a_deny_list() {
        use aivyx_core::commands::CommandTier;
        let info = |name, tier| CommandInfo {
            name,
            description: "d",
            tier,
            args_hint: None,
        };
        assert!(is_advertised(&info("/future-turn", CommandTier::AgentTurn)));
        assert!(is_advertised(&info(
            "/future-state",
            CommandTier::AgentState
        )));
        // A frontend-only command is the TUI's own business unless it's
        // one this frontend handles itself.
        assert!(!is_advertised(&info(
            "/future-key",
            CommandTier::FrontendOnly
        )));
        assert!(is_advertised(&info("/help", CommandTier::FrontendOnly)));
        assert!(!is_advertised(&info("/resume", CommandTier::AgentState)));
    }

    #[test]
    fn acp_help_text_is_a_markdown_list_after_a_blank_line() {
        let text = acp_help_text();
        assert!(text.starts_with("\n\nAvailable commands:\n\n"), "{text:?}");
        let undo = COMMANDS.iter().find(|c| c.name == "/undo").unwrap();
        assert!(
            text.lines()
                .any(|l| l == format!("- `/undo` — {}", undo.description)),
            "{text}"
        );
        for line in text.lines().skip_while(|l| !l.starts_with("- ")) {
            assert!(line.starts_with("- `/"), "{line:?} in {text}");
        }
    }

    #[test]
    fn help_update_carries_the_help_text() {
        let SessionUpdate::AgentMessageChunk(chunk) = help_update() else {
            panic!("expected an AgentMessageChunk");
        };
        let ContentBlock::Text(text) = chunk.content else {
            panic!("expected a text content block");
        };
        assert_eq!(text.text, acp_help_text());
    }

    #[test]
    fn advertised_model_command_carries_its_configured_args_hint() {
        let model = advertised_commands()
            .into_iter()
            .find(|c| c.name == "model")
            .expect("/model must be advertised");
        let Some(AvailableCommandInput::Unstructured(input)) = model.input else {
            panic!("expected /model to carry an Unstructured input hint, got {:?}", model.input);
        };
        assert_eq!(input.hint, "model id, or auto");
    }

    #[test]
    fn advertised_undo_command_carries_no_args_hint() {
        let undo = advertised_commands()
            .into_iter()
            .find(|c| c.name == "undo")
            .expect("/undo must be advertised");
        assert_eq!(undo.input, None);
    }

    #[test]
    fn acp_help_text_shows_the_args_hint_for_commands_that_have_one() {
        let text = acp_help_text();
        assert!(
            text.contains("`/model <model id, or auto>` — "),
            "{text}"
        );
        // /undo has no args_hint -- its line must stay exactly as before.
        assert!(text.contains("- `/undo` — "), "{text}");
    }

    #[test]
    fn clear_update_carries_the_exact_reply_string() {
        let SessionUpdate::AgentMessageChunk(chunk) = clear_update() else {
            panic!("expected an AgentMessageChunk");
        };
        let ContentBlock::Text(text) = chunk.content else {
            panic!("expected a text content block");
        };
        assert_eq!(text.text, CLEAR_REPLY);
    }
}
