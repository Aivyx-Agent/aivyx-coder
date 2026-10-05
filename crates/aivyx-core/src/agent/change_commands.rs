//! `/diff` and `/commit`: show and commit the working tree's changes.
//! Like the undo commands they act on files directly — they never reach
//! the model as a turn of their own and are intercepted in `run_turn`
//! before the turn-start snapshot.
//!
//! `/diff` diffs a verified snapshot of the whole worktree (untracked,
//! non-ignored files included) against `HEAD` — or the empty tree in a
//! repository with no commits — and `/diff turn` against the last turn's
//! starting snapshot. Untracked generated files (`[git] ignore`) are left
//! out of both.
//!
//! `/commit` works on the whole repository from its top level, wherever
//! in it the session started: with nothing staged it stages every tracked
//! change and every untracked file but generated ones (`[git] ignore`),
//! drafts a message with the model (unless `-m` gave one),
//! asks through the command prompter, and commits under the executor's
//! confiner — so hooks run confined like any tool. Whatever it staged
//! itself is unstaged again if the commit doesn't happen; a set the user
//! staged by hand is never touched. Deny-listed files are never staged by
//! `/commit`, and the content of one the user staged by hand is never sent
//! to the model (its name is, marked "(contents withheld)"). A staged file
//! git can't diff as text is marked "(binary)", or "(binary, new)" if it's
//! new to the repository.

use std::path::{Path, PathBuf};

use aivyx_llm::{ChatRequest, RouteHint};
use aivyx_sandbox::{
    ActionKind, PermissionRequest, PermissionTarget, UserResponse, path_is_denied,
};
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
const WITHHELD: &str = " (contents withheld)";
/// Matches the change summary's own `(binary)` marking style
/// (`changes::summary_line`) but with extra leading space, so it reads as
/// a distinct annotation in the file listing rather than running into the
/// filename.
const BINARY: &str = "   (binary)";
const BINARY_NEW: &str = "   (binary, new)";
/// How long `git commit` (hooks included) may run — the git_commit tool's
/// own limit.
const COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// The hooks whose failure stops a `git commit`.
const COMMIT_HOOKS: [&str; 3] = ["pre-commit", "prepare-commit-msg", "commit-msg"];
/// Paths per git call that takes a path list (unstaging, staging untracked
/// files, `/diff` without generated files), so a huge change set never
/// overflows the argument list.
const PATH_CHUNK: usize = 500;

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

/// Shown before a commit made while the last `/test` run failed.
const LAST_TEST_FAILED: &str = "⚠ The last /test failed.";

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

/// `(binary)`/`(binary, new)` suffixes, keyed by path, for every staged
/// file whose diff is binary — `git diff --numstat`'s `-\t-` convention,
/// the same one `changes::parse_numstat` already uses for the per-turn
/// change summary's own `(binary)` marking. A binary file already tracked
/// before this change is `(binary)`; one new to the repository (never
/// committed before) is `(binary, new)`.
async fn staged_binary_suffixes(
    root: &Path,
) -> Result<std::collections::HashMap<String, &'static str>, String> {
    #[cfg(test)]
    if FAIL_BINARY_SUFFIXES.with(std::cell::Cell::get) {
        return Err("injected failure".to_string());
    }
    let numstat = git(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "--numstat",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
        ],
    )
    .await?;
    let name_status = git(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--cached",
            "--name-status",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
        ],
    )
    .await?;
    Ok(crate::changes::parse_numstat(&numstat, &name_status)
        .into_iter()
        .filter(|c| c.added.is_none() || c.removed.is_none())
        .map(|c| {
            let suffix = if c.status == crate::changes::ChangeStatus::Added {
                BINARY_NEW
            } else {
                BINARY
            };
            (c.path, suffix)
        })
        .collect())
}

#[cfg(test)]
thread_local! {
    /// Makes `staged_binary_suffixes` fail, for tests of `/commit`'s
    /// fallback (a `#[tokio::test]` runs on one thread).
    pub(super) static FAIL_BINARY_SUFFIXES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

async fn has_head(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "-q", "HEAD^{commit}"])
        .await
        .is_ok()
}

/// Unstages everything — the exact undo of `/commit`'s own staging over
/// an index that had nothing staged, for when what it staged can't be listed.
async fn restore_all(root: &Path) -> Result<(), String> {
    if has_head(root).await {
        git(root, &["reset", "-q"]).await.map(drop)
    } else {
        git(root, &["read-tree", "--empty"]).await.map(drop)
    }
}

/// Whether an executable hook that can stop a commit exists (honouring
/// `core.hooksPath`).
async fn has_commit_hook(root: &Path) -> bool {
    for name in COMMIT_HOOKS {
        // Plain git, not `run_git`: its `core.hooksPath=/dev/null` override
        // would hide the very hooks this looks for. `rev-parse` runs nothing.
        let Ok(output) = tokio::process::Command::new("git")
            .args(["rev-parse", "--git-path", &format!("hooks/{name}")])
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await
        else {
            continue;
        };
        let path = String::from_utf8_lossy(&output.stdout).into_owned();
        let Ok(meta) = std::fs::metadata(root.join(path.trim())) else {
            continue;
        };
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt as _;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = meta.is_file();
        if executable {
            return true;
        }
    }
    false
}

/// Unstages exactly `paths` — what `/commit` staged itself — back to
/// `HEAD`, or out of the index entirely when there is no `HEAD` yet.
/// A no-op for an empty list, so a hand-staged set is never touched.
async fn restore_staging(root: &Path, paths: &[String]) -> Result<(), String> {
    let head = has_head(root).await;
    for chunk in paths.chunks(PATH_CHUNK) {
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
    pub(super) async fn run_change_command(
        &mut self,
        command: ChangeCommand,
        cancellation: CancellationToken,
    ) {
        match command {
            ChangeCommand::Diff { turn } => self.show_diff(turn).await,
            ChangeCommand::Commit { message } => self.commit(message, &cancellation).await,
            ChangeCommand::Usage(hint) => self.notify(hint),
        }
    }

    async fn show_diff(&mut self, turn: bool) {
        let Some(cwd) = self.executor.checkpoint_cwd().map(Path::to_path_buf) else {
            self.notify(NOT_A_REPO);
            return;
        };
        // Mirrors `commit`'s identical check -- `/diff` emits
        // `AgentEvent::ShowDiff`, meant for a human looking at a pager
        // (the TUI/ACP frontends), never for a delegated sub-agent or an
        // MCP session, whatever its executor is given. Unlike `/commit`,
        // nothing here actually needs a prompt/response -- this is a
        // visibility gate, not a confirmation -- but the same "only where
        // there's a user at the keyboard" rule applies.
        if self.command_prompter.is_none() {
            self.notify("/diff isn't available here.");
            return;
        }
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
        let diff = match self.visible_diff(&cwd, &base, &now).await {
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

    /// What `git add -A` would stage, minus deny-listed files and
    /// untracked generated files: tracked changes (`git add -u`), then
    /// the untracked files `git ls-files --others --exclude-standard`
    /// lists, generated ones dropped — both under the same deny-aware
    /// pathspecs.
    async fn stage_all_but_generated(&self, root: &Path, deny: &[PathBuf]) -> Result<(), String> {
        let specs = aivyx_tools::deny_aware_pathspecs(root, deny);
        let mut args = vec!["add", "-u", "--"];
        args.extend(specs.iter().map(String::as_str));
        if let Err(e) = git(root, &args).await {
            // With nothing tracked at all (a repository with no commits)
            // `.` matches no tracked file and `add -u` refuses it; there
            // is nothing for it to stage then anyway.
            let tracked = git(root, &["ls-files", "-z"]).await?;
            if !tracked.is_empty() {
                return Err(e);
            }
        }

        let mut args = vec![
            "-c",
            "core.quotePath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
        ];
        args.extend(specs.iter().map(String::as_str));
        let untracked = git(root, &args).await?;
        let generated = self.generated_files(root);
        let to_add: Vec<&str> = untracked
            .split('\0')
            .filter(|name| !name.is_empty())
            .filter(|name| !generated.as_ref().is_some_and(|g| g.is_generated(name)))
            .collect();
        for chunk in to_add.chunks(PATH_CHUNK) {
            let mut args = vec!["--literal-pathspecs", "add", "--"];
            args.extend(chunk.iter().copied());
            git(root, &args).await?;
        }
        Ok(())
    }

    /// `git diff base now`, without the sections of untracked generated
    /// files. When one is hidden, the visible paths are diffed by name,
    /// in chunks so a large change set never overflows the argument list.
    async fn visible_diff(&self, cwd: &Path, base: &str, now: &str) -> Result<String, String> {
        const DIFF: [&str; 6] = [
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
        ];
        let whole = || async {
            let mut args = DIFF.to_vec();
            args.extend([base, now]);
            git(cwd, &args).await
        };
        let Some(filter) = self.generated_filter(cwd).await else {
            return whole().await;
        };
        let mut args = DIFF.to_vec();
        args.extend(["--name-only", "-z", base, now]);
        let names = git(cwd, &args).await?;
        let (hidden, visible): (Vec<&str>, Vec<&str>) = names
            .split('\0')
            .filter(|name| !name.is_empty())
            .partition(|name| filter.hides(name));
        if hidden.is_empty() {
            return whole().await;
        }
        let mut diff = String::new();
        for chunk in visible.chunks(PATH_CHUNK) {
            // `top`: the names are relative to the repository root, the
            // diff runs wherever in it the session started.
            let specs: Vec<String> = chunk.iter().map(|p| format!(":(top,literal){p}")).collect();
            let mut args = DIFF.to_vec();
            args.extend([base, now, "--"]);
            args.extend(specs.iter().map(String::as_str));
            diff.push_str(&git(cwd, &args).await?);
        }
        Ok(diff)
    }

    async fn commit(&mut self, message: Option<String>, cancellation: &CancellationToken) {
        let Some(cwd) = self.executor.checkpoint_cwd().map(Path::to_path_buf) else {
            self.notify(NOT_A_REPO);
            return;
        };
        // Both forms need a human at the keyboard: `/commit -m` skips the
        // modal because the user typed the message, which only holds where
        // there *is* a user (TUI, ACP) — never for a delegated sub-agent or
        // an MCP session, whatever its executor is given.
        if self.command_prompter.is_none() {
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

        if let Some(refusal) = self.sandbox_refusal(&cwd, &root).await {
            self.notify(refusal);
            return;
        }
        let deny = self.executor.checkpoint_deny_paths().to_vec();
        let denied = |name: &str| path_is_denied(&root.join(name), &deny);

        // Stage everything but deny-listed files only when nothing is
        // staged yet; then every staged path is ours to unstage again.
        let staged_by_us = if git(&root, &["diff", "--cached", "--quiet"]).await.is_err() {
            Vec::new()
        } else {
            let staged = match self.stage_all_but_generated(&root, &deny).await {
                Ok(()) => staged_names(&root).await,
                Err(e) => Err(e),
            };
            let staged = match staged {
                Ok(names) => names,
                Err(e) => {
                    if let Err(e) = restore_all(&root).await {
                        self.notify(format!("Couldn't unstage the changes: {e}"));
                    }
                    self.notify(format!("Couldn't commit: {e}"));
                    return;
                }
            };
            // Backstop for a deny entry the pathspecs can't express.
            let (slipped, ours): (Vec<String>, Vec<String>) =
                staged.into_iter().partition(|name| denied(name));
            if let Err(e) = restore_staging(&root, &slipped).await {
                let all: Vec<String> = slipped.into_iter().chain(ours).collect();
                self.abandon_commit(&root, &all, format!("Couldn't commit: {e}"))
                    .await;
                return;
            }
            ours
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
            Some(message) => {
                // The same warning the confirm dialog shows; with `-m`
                // there is no dialog, so it goes into the transcript
                // instead. A warning, not a block.
                if self.last_test_passed == Some(false) {
                    self.info(LAST_TEST_FAILED);
                }
                message
            }
            None => {
                // A deny-listed file the user staged by hand is committed,
                // but only its name ever reaches the model or the prompt.
                let withheld: Vec<&String> = files.iter().filter(|f| denied(f)).collect();
                // The `(binary)` labels only annotate the preview; if
                // they can't be worked out, the commit goes ahead without
                // them.
                let binary = staged_binary_suffixes(&root).await.unwrap_or_default();
                let labels: Vec<String> = files
                    .iter()
                    .map(|f| {
                        let mut label = f.clone();
                        if denied(f) {
                            label.push_str(WITHHELD);
                        }
                        if let Some(suffix) = binary.get(f) {
                            label.push_str(suffix);
                        }
                        label
                    })
                    .collect();
                let excludes: Vec<String> = withheld
                    .iter()
                    .map(|f| format!(":(exclude,literal){f}"))
                    .collect();
                let mut args = vec![
                    "-c",
                    "core.quotePath=false",
                    "diff",
                    "--cached",
                    "--no-renames",
                    "--no-ext-diff",
                    "--no-textconv",
                ];
                if !excludes.is_empty() {
                    args.extend(["--", "."]);
                    args.extend(excludes.iter().map(String::as_str));
                }
                let diff = match git(&root, &args).await {
                    Ok(diff) => diff,
                    Err(e) => {
                        self.abandon_commit(&root, &staged_by_us, format!("Couldn't commit: {e}"))
                            .await;
                        return;
                    }
                };
                let draft = match self
                    .draft_commit_message(&diff, &labels, cancellation)
                    .await
                {
                    Ok(draft) => draft,
                    Err(_) if cancellation.is_cancelled() => {
                        if let Err(e) = restore_staging(&root, &staged_by_us).await {
                            self.notify(format!("Couldn't unstage the changes: {e}"));
                        }
                        self.info(COMMIT_CANCELLED);
                        return;
                    }
                    Err(reason) => {
                        let notice = format!(
                            "Couldn't draft a message ({reason}) — commit with /commit -m \"…\"."
                        );
                        self.abandon_commit(&root, &staged_by_us, notice).await;
                        return;
                    }
                };
                if cancellation.is_cancelled() || !self.confirm_commit(&draft, &labels).await {
                    if let Err(e) = restore_staging(&root, &staged_by_us).await {
                        self.notify(format!("Couldn't unstage the changes: {e}"));
                    }
                    self.info(COMMIT_CANCELLED);
                    return;
                }
                draft
            }
        };

        if cancellation.is_cancelled() {
            self.abandon_commit(&root, &staged_by_us, COMMIT_CANCELLED.to_string())
                .await;
            return;
        }
        let confiner = self.executor.confiner();
        let args = [
            "commit".to_string(),
            "-q".into(),
            "-m".into(),
            message.clone(),
        ];
        let mut command = aivyx_tools::confined_git(&args, &root, confiner.as_ref());
        command.kill_on_drop(true);
        // Bounded and cancellable like the git_commit tool: a hook that hangs
        // must not wedge the session. Dropping the future kills git's whole
        // process group, and so does git exiting, so nothing a hook left
        // running outlives the commit.
        let output = tokio::select! {
            output = tokio::time::timeout(COMMIT_TIMEOUT, aivyx_tools::output_in_group(command)) => match output {
                Ok(output) => output,
                Err(_) => {
                    self.abandon_commit(
                        &root,
                        &staged_by_us,
                        format!("Couldn't commit: git took longer than {}s", COMMIT_TIMEOUT.as_secs()),
                    )
                    .await;
                    return;
                }
            },
            _ = cancellation.cancelled() => {
                self.abandon_commit(&root, &staged_by_us, COMMIT_CANCELLED.to_string())
                    .await;
                return;
            }
        };
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
            let said = format!("{stderr}{stdout}");
            let said = said.trim();
            let notice = if said.is_empty() {
                format!("Couldn't commit: {}", output.status)
            } else if has_commit_hook(&root).await {
                format!("The commit hook rejected it:\n{said}")
            } else {
                format!("git refused the commit:\n{said}")
            };
            self.abandon_commit(&root, &staged_by_us, notice).await;
            return;
        }
        let hash = git(&root, &["rev-parse", "--short", "HEAD"])
            .await
            .map(|h| h.trim().to_string())
            .unwrap_or_default();
        let subject = message.lines().next().unwrap_or_default().trim();
        if hash.is_empty() {
            self.info(format!("Committed: {subject}"));
        } else {
            self.info(format!("Committed {hash}: {subject}"));
        }
    }

    /// Why `/commit` can't work here, when confinement is on and the git
    /// directory (or a worktree's common directory) lies outside the
    /// directory confined processes may write under — checked before
    /// anything is staged.
    async fn sandbox_refusal(&self, cwd: &Path, root: &Path) -> Option<String> {
        let sandbox = self.write_sandbox.as_ref()?;
        let sandbox = sandbox.canonicalize().unwrap_or_else(|_| sandbox.clone());
        let queries: [&[&str]; 2] = [
            &["rev-parse", "--absolute-git-dir"],
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ];
        for args in queries {
            let Ok(out) = git(cwd, args).await else {
                continue;
            };
            let dir = PathBuf::from(out.trim());
            let dir = dir.canonicalize().unwrap_or(dir);
            if !dir.starts_with(&sandbox) {
                return Some(format!(
                    "/commit needs write access to {}, which is outside this session's sandbox \
                     — start aivyx-coder at {}, or commit with git directly.",
                    dir.display(),
                    root.display()
                ));
            }
        }
        None
    }

    /// Unstages what `/commit` staged, then reports why it stopped.
    async fn abandon_commit(&self, root: &Path, staged_by_us: &[String], notice: String) {
        if let Err(e) = restore_staging(root, staged_by_us).await {
            self.notify(format!("Couldn't unstage the changes: {e}"));
        }
        self.notify(notice);
    }

    /// One no-tools request for a commit message; the reason on failure.
    async fn draft_commit_message(
        &self,
        diff: &str,
        files: &[String],
        cancellation: &CancellationToken,
    ) -> Result<String, String> {
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
        match collect_text(self.llm.as_ref(), request, cancellation).await {
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
        let mut preview = format!("{draft}\n\nFiles:\n{}", listing.join("\n"));
        if self.last_test_passed == Some(false) {
            // A real warning, not a block -- committing on top of a known
            // failure is the user's call, but they should see it before
            // approving.
            preview = format!("{LAST_TEST_FAILED}\n{preview}");
        }
        let request = PermissionRequest {
            tool_name: "commit".to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Other("commit".to_string()),
            arguments_preview: serde_json::json!({ "message": draft }),
            preview: Some(preview),
            diff: None,
        };
        !matches!(prompter.prompt(&request).await, UserResponse::Deny)
    }
}
