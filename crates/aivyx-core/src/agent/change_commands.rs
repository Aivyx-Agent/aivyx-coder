//! `/diff` and `/commit`: show and commit the working tree's changes.
//! Like the undo commands they act on files directly — they never reach
//! the model as a turn of their own and are intercepted in `run_turn`
//! before the turn-start snapshot.
//!
//! `/diff` diffs a verified snapshot of the whole worktree (untracked,
//! non-ignored files included) against `HEAD` — or the empty tree in a
//! repository with no commits — and `/diff turn` against the last turn's
//! starting snapshot.
//!
//! `/commit` works on the whole repository from its top level, wherever
//! in it the session started: with nothing staged it stages everything
//! (`git add -A`), drafts a message with the model (unless `-m` gave one),
//! asks through the command prompter, and commits under the executor's
//! confiner — so hooks run confined like any tool. Whatever it staged
//! itself is unstaged again if the commit doesn't happen; a set the user
//! staged by hand is never touched.

use std::path::{Path, PathBuf};

use aivyx_llm::{ChatRequest, RouteHint};
use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, UserResponse};
use aivyx_types::{Message, Role};
use tokio_util::sync::CancellationToken;

use super::{Agent, AgentEvent, resolve_oid};
use crate::changes::{
    COMMIT_DRAFT_PROMPT, DIFF_LINE_CAP, EMPTY_TREE, parse_commit_message_arg, trim_diff_for_prompt,
    truncate_lines,
};
use crate::council::{CollectError, collect_text, strip_think};

const NOT_A_REPO: &str = "Not a git repository.";
const NO_UNCOMMITTED: &str = "No uncommitted changes.";
const NO_TURN_CHANGES: &str = "No changes from the last turn to show.";
const TOO_OLD: &str = "That turn is too old to show (only the newest 50 checkpoints are kept).";
const DIFF_USAGE: &str = "Use /diff, or /diff turn for just the last turn's changes.";
const COMMIT_USAGE: &str = "Use /commit, or /commit -m \"message\".";
const NOTHING_TO_COMMIT: &str = "Nothing to commit.";
const COMMIT_CANCELLED: &str = "Commit cancelled — nothing was committed.";
/// Paths per `git reset`/`git rm --cached` call when unstaging, so a huge
/// change set never overflows the argument list.
const UNSTAGE_CHUNK: usize = 500;

/// Which change command a message is, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChangeCommand {
    /// `/diff` (`turn: false`) or `/diff turn`.
    Diff { turn: bool },
    /// `/commit` (draft a message) or `/commit -m "…"`.
    Commit { message: Option<String> },
    /// `/diff …` or `/commit …` with arguments neither understands: a
    /// usage hint, so the message never reaches the model.
    Usage(&'static str),
}

/// `/diff`, `/diff turn`, `/commit`, `/commit -m …`; any other argument
/// to `/diff` or `/commit` is a [`ChangeCommand::Usage`] hint.
pub(super) fn parse(user_input: &str) -> Option<ChangeCommand> {
    use crate::commands::parse_slash_command as cmd;
    if let Some(rest) = cmd(user_input, "/diff") {
        match rest {
            "" => Some(ChangeCommand::Diff { turn: false }),
            "turn" => Some(ChangeCommand::Diff { turn: true }),
            _ => Some(ChangeCommand::Usage(DIFF_USAGE)),
        }
    } else if let Some(rest) = cmd(user_input, "/commit") {
        if rest.is_empty() {
            Some(ChangeCommand::Commit { message: None })
        } else {
            Some(match parse_commit_message_arg(rest) {
                Some(m) => ChangeCommand::Commit { message: Some(m) },
                None => ChangeCommand::Usage(COMMIT_USAGE),
            })
        }
    } else {
        None
    }
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    aivyx_tools::run_git(cwd, args, &[]).await
}

/// The staged paths, unquoted (`-z`, `core.quotePath=false`).
async fn staged_names(root: &Path) -> Result<Vec<String>, String> {
    let out = git(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "-z",
        ],
    )
    .await?;
    Ok(out
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

async fn has_head(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "-q", "HEAD^{commit}"])
        .await
        .is_ok()
}

/// Unstages exactly `paths` — what `/commit` staged itself — back to
/// `HEAD`, or out of the index entirely when there is no `HEAD` yet.
/// A no-op for an empty list, so a hand-staged set is never touched.
async fn restore_staging(root: &Path, paths: &[String]) -> Result<(), String> {
    let head = has_head(root).await;
    for chunk in paths.chunks(UNSTAGE_CHUNK) {
        let mut args = vec!["--literal-pathspecs"];
        if head {
            args.extend(["reset", "-q", "--"]);
        } else {
            args.extend(["rm", "--cached", "-q", "-r", "--"]);
        }
        args.extend(chunk.iter().map(String::as_str));
        git(root, &args).await?;
    }
    Ok(())
}

impl Agent {
    pub(super) async fn run_change_command(&mut self, command: ChangeCommand) {
        match command {
            ChangeCommand::Diff { turn } => self.show_diff(turn).await,
            ChangeCommand::Commit { message } => self.commit(message).await,
            ChangeCommand::Usage(hint) => self.notify(hint),
        }
    }

    async fn show_diff(&mut self, turn: bool) {
        let Some(cwd) = self.executor.checkpoint_cwd().map(Path::to_path_buf) else {
            self.notify(NOT_A_REPO);
            return;
        };
        // The base, the "now" snapshot, the title and the nothing-to-show
        // line. `/diff turn` compares two checkpoints (the mark's was taken
        // by the checkpointer, so "now" must be one too); `/diff` compares
        // HEAD with the tree a commit would get, so tracked-but-ignored and
        // tracked-but-denied files keep their HEAD version rather than
        // showing up as deleted.
        let (base, now, title, empty) = if turn {
            let Some(mark) = self.undo.marks.last().cloned() else {
                self.info(NO_TURN_CHANGES);
                return;
            };
            if git(
                &cwd,
                &["cat-file", "-e", &format!("{}^{{commit}}", mark.before_oid)],
            )
            .await
            .is_err()
            {
                self.notify(TOO_OLD);
                return;
            }
            let now = match self
                .executor
                .checkpoint_now("diff", &CancellationToken::new())
                .await
            {
                Some(r) => resolve_oid(&cwd, &r).await,
                None => None,
            };
            (
                mark.before_oid,
                now,
                format!(
                    "Changes from the last turn (\"{}\")",
                    mark.user_text_preview
                ),
                NO_TURN_CHANGES,
            )
        } else {
            let head = match git(&cwd, &["rev-parse", "--verify", "HEAD^{commit}"]).await {
                Ok(head) if !head.trim().is_empty() => Some(head.trim().to_string()),
                _ => None,
            };
            let now = self.executor.worktree_tree_over(head.as_deref()).await;
            (
                head.unwrap_or_else(|| EMPTY_TREE.to_string()),
                now,
                "Uncommitted changes".to_string(),
                NO_UNCOMMITTED,
            )
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

    async fn commit(&mut self, message: Option<String>) {
        let Some(cwd) = self.executor.checkpoint_cwd().map(Path::to_path_buf) else {
            self.notify(NOT_A_REPO);
            return;
        };
        if message.is_none() && self.command_prompter.is_none() {
            self.notify("/commit isn't available here.");
            return;
        }
        let root = match git(&cwd, &["rev-parse", "--show-toplevel"]).await {
            Ok(out) if !out.trim().is_empty() => PathBuf::from(out.trim()),
            _ => {
                self.notify(NOT_A_REPO);
                return;
            }
        };

        // Stage everything only when nothing is staged yet; then every
        // staged path is ours to unstage again.
        let staged_by_us = if git(&root, &["diff", "--cached", "--quiet"]).await.is_err() {
            Vec::new()
        } else {
            if let Err(e) = git(&root, &["add", "-A"]).await {
                self.notify(format!("Couldn't commit: {e}"));
                return;
            }
            match staged_names(&root).await {
                Ok(names) => names,
                Err(e) => {
                    self.notify(format!("Couldn't commit: {e}"));
                    return;
                }
            }
        };
        let files = match staged_names(&root).await {
            Ok(files) => files,
            Err(e) => {
                self.abandon_commit(&root, &staged_by_us, format!("Couldn't commit: {e}"))
                    .await;
                return;
            }
        };
        if files.is_empty() {
            self.info(NOTHING_TO_COMMIT);
            return;
        }

        let message = match message {
            Some(message) => message,
            None => {
                let diff = match git(
                    &root,
                    &[
                        "-c",
                        "core.quotePath=false",
                        "diff",
                        "--cached",
                        "--no-renames",
                    ],
                )
                .await
                {
                    Ok(diff) => diff,
                    Err(e) => {
                        self.abandon_commit(&root, &staged_by_us, format!("Couldn't commit: {e}"))
                            .await;
                        return;
                    }
                };
                let draft = match self.draft_commit_message(&diff, &files).await {
                    Ok(draft) => draft,
                    Err(reason) => {
                        let notice = format!(
                            "Couldn't draft a message ({reason}) — commit with /commit -m \"…\"."
                        );
                        self.abandon_commit(&root, &staged_by_us, notice).await;
                        return;
                    }
                };
                if !self.confirm_commit(&draft, &files).await {
                    if let Err(e) = restore_staging(&root, &staged_by_us).await {
                        self.notify(format!("Couldn't unstage the changes: {e}"));
                    }
                    self.info(COMMIT_CANCELLED);
                    return;
                }
                draft
            }
        };

        let confiner = self.executor.confiner();
        let args = [
            "commit".to_string(),
            "-q".into(),
            "-m".into(),
            message.clone(),
        ];
        let output = aivyx_tools::confined_git(&args, &root, confiner.as_ref())
            .kill_on_drop(true)
            .output()
            .await;
        let output = match output {
            Ok(output) => output,
            Err(e) => {
                self.abandon_commit(&root, &staged_by_us, format!("Couldn't commit: {e}"))
                    .await;
                return;
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let notice = if stderr.trim().is_empty() {
                format!("Couldn't commit: {}", output.status)
            } else {
                let said = format!("{stderr}{stdout}");
                format!("The commit hook rejected it:\n{}", said.trim())
            };
            self.abandon_commit(&root, &staged_by_us, notice).await;
            return;
        }
        let hash = git(&root, &["rev-parse", "--short", "HEAD"])
            .await
            .map(|h| h.trim().to_string())
            .unwrap_or_default();
        let subject = message.lines().next().unwrap_or_default().trim();
        self.info(format!("Committed {hash}: {subject}"));
    }

    /// Unstages what `/commit` staged, then reports why it stopped.
    async fn abandon_commit(&self, root: &Path, staged_by_us: &[String], notice: String) {
        if let Err(e) = restore_staging(root, staged_by_us).await {
            self.notify(format!("Couldn't unstage the changes: {e}"));
        }
        self.notify(notice);
    }

    /// One no-tools request for a commit message; the reason on failure.
    async fn draft_commit_message(&self, diff: &str, files: &[String]) -> Result<String, String> {
        let budget = (self.context_limit as usize) * 4 / 3;
        let user = trim_diff_for_prompt(diff, files, budget);
        let chars = COMMIT_DRAFT_PROMPT.chars().count() + user.chars().count();
        let mut request = ChatRequest::new(vec![
            Message::text(Role::System, COMMIT_DRAFT_PROMPT),
            Message::text(Role::User, user),
        ]);
        request.route = Some(RouteHint {
            task: aivyx_route::TaskKind::Summarize,
            session: None,
            estimated_prompt_tokens: (chars / 4) as u32,
        });
        match collect_text(self.llm.as_ref(), request, &CancellationToken::new()).await {
            Ok(text) => {
                let draft = strip_think(&text).trim().to_string();
                if draft.is_empty() {
                    Err("the model's reply was empty".to_string())
                } else {
                    Ok(draft)
                }
            }
            Err(CollectError::Cancelled) => Err("cancelled".to_string()),
            Err(CollectError::Backend(e)) => Err(e),
        }
    }

    /// Asks before committing `draft`; any answer but Deny means yes.
    async fn confirm_commit(&self, draft: &str, files: &[String]) -> bool {
        let Some(prompter) = &self.command_prompter else {
            return false;
        };
        let listing: Vec<String> = files.iter().map(|f| format!("  {f}")).collect();
        let request = PermissionRequest {
            tool_name: "commit".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Other("commit".to_string()),
            arguments_preview: serde_json::json!({ "message": draft }),
            preview: Some(format!("{draft}\n\nFiles:\n{}", listing.join("\n"))),
            diff: None,
        };
        !matches!(prompter.prompt(&request).await, UserResponse::Deny)
    }
}
