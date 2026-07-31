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
    /// Touches real `Agent` state directly, bypassing `run_turn` and the
    /// model entirely.
    AgentState,
    /// Needs nothing from `Agent` at all — handled entirely by the
    /// frontend.
    FrontendOnly,
}

pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: CommandTier,
}

/// Every known slash command, in the order `/help` lists them. Adding a
/// command means adding an entry here plus whichever dispatch-tier code
/// path it needs — this table itself has no dynamic registration.
pub const COMMANDS: &[CommandInfo] = &[
    CommandInfo {
        name: "/council",
        description: "Convene the configured council on a subject (or the last assistant message if bare)",
        tier: CommandTier::AgentTurn,
    },
    CommandInfo {
        name: "/wiki",
        description: "Regenerate stale wiki pages, or one named page",
        tier: CommandTier::AgentTurn,
    },
    CommandInfo {
        name: "/architect",
        description: "Have the configured architect model produce a plan for a task, then execute it",
        tier: CommandTier::AgentTurn,
    },
    CommandInfo {
        name: "/clear",
        description: "Start a fresh conversation (clears history and tasks)",
        tier: CommandTier::AgentState,
    },
    CommandInfo {
        name: "/help",
        description: "List available commands",
        tier: CommandTier::FrontendOnly,
    },
    CommandInfo {
        name: "/quit",
        description: "Exit aivyx-coder",
        tier: CommandTier::FrontendOnly,
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
