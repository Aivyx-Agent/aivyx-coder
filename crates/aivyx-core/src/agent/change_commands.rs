//! `/diff` and `/commit`: show and commit the working tree's changes.
//! Like the undo commands they act on files directly — they never reach
//! the model as a turn of their own and are intercepted in `run_turn`
//! before the turn-start snapshot.
//!
//! `/diff` diffs a verified snapshot of the whole worktree (untracked,
//! non-ignored files included) against `HEAD` — or the empty tree in a
//! repository with no commits — and `/diff turn` against the last turn's
//! starting snapshot.

use std::path::Path;

use tokio_util::sync::CancellationToken;

use super::{Agent, AgentEvent, resolve_oid};
use crate::changes::{DIFF_LINE_CAP, EMPTY_TREE, parse_commit_message_arg, truncate_lines};

const NOT_A_REPO: &str = "Not a git repository.";
const NO_UNCOMMITTED: &str = "No uncommitted changes.";
const NO_TURN_CHANGES: &str = "No changes from the last turn to show.";

/// Which change command a message is, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChangeCommand {
    /// `/diff` (`turn: false`) or `/diff turn`.
    Diff { turn: bool },
    /// `/commit` (draft a message) or `/commit -m "…"`.
    Commit { message: Option<String> },
}

/// `/diff`, `/diff turn`, `/commit`, `/commit -m …`. Any other argument
/// isn't one of these commands, so the message is left alone.
pub(super) fn parse(user_input: &str) -> Option<ChangeCommand> {
    use crate::commands::parse_slash_command as cmd;
    if let Some(rest) = cmd(user_input, "/diff") {
        match rest {
            "" => Some(ChangeCommand::Diff { turn: false }),
            "turn" => Some(ChangeCommand::Diff { turn: true }),
            _ => None,
        }
    } else if let Some(rest) = cmd(user_input, "/commit") {
        if rest.is_empty() {
            Some(ChangeCommand::Commit { message: None })
        } else {
            parse_commit_message_arg(rest).map(|m| ChangeCommand::Commit { message: Some(m) })
        }
    } else {
        None
    }
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    aivyx_tools::run_git(cwd, args, &[]).await
}

impl Agent {
    pub(super) async fn run_change_command(&mut self, command: ChangeCommand) {
        match command {
            ChangeCommand::Diff { turn } => self.show_diff(turn).await,
            // Task 5 replaces this with the real commit flow.
            ChangeCommand::Commit { .. } => self.notify("/commit isn't available yet."),
        }
    }

    async fn show_diff(&mut self, turn: bool) {
        let Some(cwd) = self.executor.checkpoint_cwd().map(Path::to_path_buf) else {
            self.notify(NOT_A_REPO);
            return;
        };
        let (base, title, empty) = if turn {
            let Some(mark) = self.undo.marks.last() else {
                self.info(NO_TURN_CHANGES);
                return;
            };
            (
                mark.before_oid.clone(),
                format!(
                    "Changes from the last turn (\"{}\")",
                    mark.user_text_preview
                ),
                NO_TURN_CHANGES,
            )
        } else {
            let base = match git(&cwd, &["rev-parse", "--verify", "HEAD"]).await {
                Ok(head) if !head.trim().is_empty() => head.trim().to_string(),
                _ => EMPTY_TREE.to_string(),
            };
            (base, "Uncommitted changes".to_string(), NO_UNCOMMITTED)
        };
        let now = match self
            .executor
            .checkpoint_now("diff", &CancellationToken::new())
            .await
        {
            Some(r) => resolve_oid(&cwd, &r).await,
            None => None,
        };
        let Some(now) = now else {
            self.notify("Couldn't read the changes: could not snapshot the current state");
            return;
        };
        let diff = match git(
            &cwd,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--no-renames",
                &base,
                &now,
            ],
        )
        .await
        {
            Ok(diff) => diff,
            Err(e) => {
                self.notify(format!("Couldn't read the changes: {e}"));
                return;
            }
        };
        if diff.trim().is_empty() {
            self.info(empty);
            return;
        }
        self.emit(AgentEvent::ShowDiff {
            title,
            text: truncate_lines(&diff, DIFF_LINE_CAP),
        });
    }
}
