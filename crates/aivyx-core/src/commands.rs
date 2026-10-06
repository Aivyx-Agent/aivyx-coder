//! Shared slash-command metadata and parsing — the single source of
//! truth for `/help`'s listing, the TUI's autocomplete hint, and the
//! word-boundary check every command's own parser uses. See
//! `docs/superpowers/specs/2026-08-01-slash-commands-design.md`.

/// Which dispatch path a command takes — see
/// `docs/superpowers/specs/2026-08-01-slash-commands-design.md`'s
/// "Three dispatch tiers" decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTier {
    /// Goes through `Agent::run_turn` as today — needs the model.
    AgentTurn,
    /// Touches real `Agent` state directly without reaching the model or
    /// the history — either through a dedicated `Agent` method (e.g.
    /// `/clear`) or intercepted at the top of `run_turn` (`/models`,
    /// `/model`).
    AgentState,
    /// Needs nothing from `Agent` at all — handled entirely by the
    /// frontend.
    FrontendOnly,
}

pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: CommandTier,
    /// A short hint for the argument(s) this command takes, shown by a
    /// frontend that offers argument hints (today: `aivyx-acp`'s
    /// `AvailableCommand::input`) — `None` for a command that takes no
    /// arguments at all. Purely descriptive: nothing here parses or
    /// enforces this shape, it just tells a human (or an editor's command
    /// picker) what to type after the command name.
    pub args_hint: Option<&'static str>,
}

/// Every known slash command, in the order `/help` lists them. Adding a
/// command means adding an entry here plus whichever dispatch-tier code
/// path it needs — this table itself has no dynamic registration.
pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "/council",
        description: "Convene the configured council on a subject (or the last assistant message if bare)",
        tier: CommandTier::AgentTurn,
        args_hint: Some("subject (optional)"),
    },
    CommandInfo {
        name: "/wiki",
        description: "Regenerate stale wiki pages, or one named page",
        tier: CommandTier::AgentTurn,
        args_hint: Some("page (optional)"),
    },
    CommandInfo {
        name: "/architect",
        description: "Have the configured architect model produce a plan for a task, then execute it",
        tier: CommandTier::AgentTurn,
        args_hint: Some("task"),
    },
    CommandInfo {
        name: "/models",
        description: "List routing candidates; `/models refresh` re-runs discovery, `/models why` explains the last choice",
        tier: CommandTier::AgentState,
        args_hint: Some("refresh | why (optional)"),
    },
    CommandInfo {
        name: "/model",
        description: "Pin this conversation to a model (`/model <id>`), or `/model auto` to let routing choose",
        tier: CommandTier::AgentState,
        args_hint: Some("model id, or auto"),
    },
    CommandInfo {
        name: "/clear",
        description: "Start a new conversation (the old one stays in /sessions)",
        tier: CommandTier::AgentState,
        args_hint: None,
    },
    CommandInfo {
        name: "/sessions",
        description: "List this project's saved conversations",
        tier: CommandTier::AgentState,
        args_hint: None,
    },
    CommandInfo {
        name: "/resume",
        description: "Switch to saved conversation N (/resume N)",
        tier: CommandTier::AgentState,
        args_hint: Some("N"),
    },
    CommandInfo {
        name: "/undo",
        description: "Take back the assistant's last turn (asks first; /redo puts it back)",
        tier: CommandTier::AgentState,
        args_hint: None,
    },
    CommandInfo {
        name: "/redo",
        description: "Put back what the last /undo removed",
        tier: CommandTier::AgentState,
        args_hint: None,
    },
    CommandInfo {
        name: "/checkpoints",
        description: "List the turns /undo can take back",
        tier: CommandTier::AgentState,
        args_hint: None,
    },
    CommandInfo {
        name: "/diff",
        description: "Show uncommitted changes (`/diff turn`: just the last turn's)",
        tier: CommandTier::AgentState,
        args_hint: Some("turn (optional)"),
    },
    CommandInfo {
        name: "/commit",
        description: "Commit with a drafted message you approve (`/commit -m \"…\"` to write your own)",
        tier: CommandTier::AgentState,
        args_hint: Some("-m \"message\" (optional)"),
    },
    CommandInfo {
        name: "/test",
        description: "Run the project's tests (the command shown at startup)",
        tier: CommandTier::AgentState,
        args_hint: None,
    },
    CommandInfo {
        name: "/help",
        description: "List available commands",
        tier: CommandTier::FrontendOnly,
        args_hint: None,
    },
    CommandInfo {
        name: "/quit",
        description: "Exit aivyx-coder",
        tier: CommandTier::FrontendOnly,
        args_hint: None,
    },
];

/// Shared boundary-aware parser: `name` matches only as a whole word —
/// `/foobar` does not match `/foo`. Replaces the three near-identical
/// copies that used to live in council.rs/wiki.rs/architect.rs. Returns
/// the trimmed remainder after `name` (empty for the bare form).
pub fn parse_slash_command<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let trimmed = input.trim();
    let rest = trimmed.strip_prefix(name)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_slash_command_reference_heading() {
        let manual = include_str!("../../../docs/manual/reference/02-slash-commands.md");
        let missing: Vec<&str> = COMMANDS
            .iter()
            .map(|c| c.name)
            .filter(|name| !manual.contains(&format!("### `{name}")))
            .collect();
        assert!(
            missing.is_empty(),
            "docs/manual/reference/02-slash-commands.md has no heading for: {missing:?}"
        );
    }

    #[test]
    fn parse_slash_command_recognizes_bare_and_argument_forms() {
        assert_eq!(parse_slash_command("/foo", "/foo"), Some(""));
        assert_eq!(parse_slash_command("  /foo  ", "/foo"), Some(""));
        assert_eq!(
            parse_slash_command("/foo some args here", "/foo"),
            Some("some args here")
        );
    }

    #[test]
    fn parse_slash_command_rejects_lookalikes_and_normal_messages() {
        assert_eq!(parse_slash_command("/foobar", "/foo"), None);
        assert_eq!(parse_slash_command("run /foo for me", "/foo"), None);
        assert_eq!(parse_slash_command("foo", "/foo"), None);
    }

    #[test]
    fn commands_table_has_no_duplicate_names() {
        let mut names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            COMMANDS.len(),
            "COMMANDS must not list the same name twice"
        );
    }
}
