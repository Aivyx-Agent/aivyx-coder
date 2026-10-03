//! `/sessions` and `/resume N`: list this project's saved conversations
//! and switch between them in place. The pure formatting lives in
//! [`crate::session_list`]; this module is just the command-dispatch glue
//! plus `Agent::switch_to` (defined in `agent/mod.rs`, since it reaches
//! into state this module has no other reason to know about).

use super::{Agent, now_unix};
use crate::session_list::sessions_listing;

const NOT_AVAILABLE: &str = "/resume isn't available here.";
const NO_STORE: &str = "Sessions aren't saved here.";
const USAGE: &str = "Use /resume N — /sessions lists them.";

/// Which sessions command a message is, if any. `Resume` carries the raw
/// text after `/resume` (already trimmed by `parse_slash_command`), still
/// to be validated as a positive, in-range index.
pub(super) enum SessionCommand<'a> {
    List,
    Resume(&'a str),
}

pub(super) fn parse(user_input: &str) -> Option<SessionCommand<'_>> {
    use crate::commands::parse_slash_command as cmd;
    if cmd(user_input, "/sessions").is_some() {
        Some(SessionCommand::List)
    } else {
        cmd(user_input, "/resume").map(SessionCommand::Resume)
    }
}

/// The local UTC offset, in seconds -- matches `undo_commands`'s own
/// identically-named, independently-kept helper (see that module's own
/// precedent for why it isn't shared).
fn local_offset_secs() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

impl Agent {
    pub(super) async fn run_session_command(&mut self, command: SessionCommand<'_>) {
        match command {
            SessionCommand::List => self.list_sessions(),
            SessionCommand::Resume(arg) => self.resume_session(arg).await,
        }
    }

    fn list_sessions(&self) {
        let Some(store) = self.session_store() else {
            self.info(NO_STORE);
            return;
        };
        self.info(sessions_listing(
            &store.list(),
            self.current_session_id(),
            now_unix(),
            local_offset_secs(),
        ));
    }

    async fn resume_session(&mut self, arg: &str) {
        if !self.session_switching {
            self.info(NOT_AVAILABLE);
            return;
        }
        let Some(store) = self.session_store().cloned() else {
            self.info(NOT_AVAILABLE);
            return;
        };
        let Ok(n) = arg.trim().parse::<usize>() else {
            self.info(USAGE);
            return;
        };
        let list = store.list();
        if n == 0 || n > list.len() {
            self.info(format!(
                "There are only {} saved conversations for this project (see /sessions).",
                list.len()
            ));
            return;
        }
        let meta = list[n - 1].clone();
        if self.switch_to(&meta.id).await {
            let turn_word = if meta.turns == 1 { "turn" } else { "turns" };
            self.info(format!("Resumed conversation {n} ({} {turn_word})", meta.turns));
        }
    }
}
