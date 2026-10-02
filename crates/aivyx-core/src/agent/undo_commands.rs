//! `/undo`, `/redo` and `/checkpoints`: the user-facing side of the
//! [`crate::undo::UndoLedger`] the turn loop fills in. Every restore asks
//! first through the command prompter (the same modal tool approvals use,
//! but never cached and never subject to plan mode), snapshots the current
//! state so it can be put back, and tells the model in the user's next
//! message.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, UserResponse};
use tokio_util::sync::CancellationToken;

use super::{Agent, resolve_oid};
use crate::undo::{
    ChangeKind, PreviewEntry, RedoMark, checkpoints_listing, parse_name_status, preview_text,
    preview_title_undo,
};

/// Shown when there is no checkpointer: `[git] checkpoints = false`, or
/// the folder isn't a git repository.
const NO_CHECKPOINTS: &str =
    "No checkpoints here — checkpoints are off or this folder isn't a git repository.";
const NOTHING_TO_UNDO: &str = "Nothing to undo — no changes made in this session.";
const TOO_OLD: &str = "That turn is too old to undo (only the newest 50 checkpoints are kept).";

/// Which undo command a message is, if any.
pub(super) enum UndoCommand {
    Undo,
    Redo,
    Checkpoints,
}

pub(super) fn parse(user_input: &str) -> Option<UndoCommand> {
    use crate::commands::parse_slash_command as cmd;
    if cmd(user_input, "/undo").is_some() {
        Some(UndoCommand::Undo)
    } else if cmd(user_input, "/redo").is_some() {
        Some(UndoCommand::Redo)
    } else if cmd(user_input, "/checkpoints").is_some() {
        Some(UndoCommand::Checkpoints)
    } else {
        None
    }
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    aivyx_tools::run_git(cwd, args, &[]).await
}

async fn oid_exists(cwd: &Path, oid: &str) -> bool {
    git(cwd, &["cat-file", "-e", &format!("{oid}^{{commit}}")])
        .await
        .is_ok()
}

/// The local UTC offset, in seconds, for `/checkpoints` times.
fn local_offset_secs() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

impl Agent {
    pub(super) async fn run_undo_command(&mut self, command: UndoCommand) {
        match command {
            UndoCommand::Undo => self.undo_last_turn().await,
            UndoCommand::Redo => self.redo_last_undo().await,
            UndoCommand::Checkpoints => self.list_checkpoints().await,
        }
    }

    /// The repository root `/undo` works in, after the shared refusals.
    fn undo_cwd(&self, command: &str) -> Option<PathBuf> {
        if self.command_prompter.is_none() {
            self.notify(format!("{command} isn't available here."));
            return None;
        }
        match self.executor.checkpoint_cwd() {
            Some(cwd) => Some(cwd.to_path_buf()),
            None => {
                self.notify(NO_CHECKPOINTS);
                None
            }
        }
    }

    /// Snapshot the worktree now (the point `/redo` returns to); its oid.
    async fn snapshot_now(&self, cwd: &Path) -> Option<String> {
        let r = self
            .executor
            .checkpoint_now("undo", &CancellationToken::new())
            .await?;
        resolve_oid(cwd, &r).await
    }

    /// What restoring `target` changes, relative to `current`.
    async fn restore_effects(
        cwd: &Path,
        current: &str,
        target: &str,
    ) -> Result<Vec<(String, ChangeKind)>, String> {
        git(cwd, &["diff", "--name-status", current, target])
            .await
            .map(|out| parse_name_status(&out))
    }

    async fn confirm(&self, tool_name: &str, preview: String) -> bool {
        let Some(prompter) = &self.command_prompter else {
            return false;
        };
        let request = PermissionRequest {
            tool_name: tool_name.to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Other(format!("{tool_name} last turn")),
            arguments_preview: serde_json::json!({}),
            preview: Some(preview),
            diff: None,
        };
        // Allow and AllowAlways both mean yes for this one decision; nothing
        // is ever cached for /undo.
        !matches!(prompter.prompt(&request).await, UserResponse::Deny)
    }

    async fn undo_last_turn(&mut self) {
        let Some(cwd) = self.undo_cwd("/undo") else {
            return;
        };
        let Some(mark) = self.undo.marks.last().cloned() else {
            self.notify(NOTHING_TO_UNDO);
            return;
        };
        if !oid_exists(&cwd, &mark.before_oid).await {
            self.notify(TOO_OLD);
            return;
        }
        let Some(redo_oid) = self.snapshot_now(&cwd).await else {
            self.notify("Couldn't undo: could not snapshot the current state");
            return;
        };
        let changes = match Self::restore_effects(&cwd, &redo_oid, &mark.before_oid).await {
            Ok(changes) => changes,
            Err(e) => {
                self.notify(format!("Couldn't undo: {e}"));
                return;
            }
        };
        if changes.is_empty() {
            // The worktree is already where the undo would take it: drop
            // the mark, but there is nothing to redo and nothing to tell
            // the model.
            self.undo.pop_mark();
            self.notify("Nothing to undo there — that turn's changes are already gone.");
            self.persist_if_owned();
            return;
        }
        let changed_after: HashSet<String> = match &mark.after_oid {
            Some(after) => git(&cwd, &["diff", "--name-only", after, &redo_oid])
                .await
                .map(|out| out.lines().map(str::to_string).collect())
                .unwrap_or_default(),
            None => HashSet::new(),
        };
        let paths: Vec<String> = changes.iter().map(|(p, _)| p.clone()).collect();
        let entries: Vec<PreviewEntry> = changes
            .into_iter()
            .map(|(path, kind)| PreviewEntry {
                changed_after: changed_after.contains(&path),
                path,
                kind,
            })
            .collect();
        let preview = preview_text(&preview_title_undo(&mark.user_text_preview), &entries);
        if !self.confirm("undo", preview).await {
            self.notify("Undo cancelled.");
            return;
        }
        if let Err(e) = self
            .executor
            .restore_to_checkpoint(&mark.before_oid, &CancellationToken::new())
            .await
        {
            self.notify(format!("Couldn't undo: {e}"));
            return;
        }
        self.undo.pop_mark();
        self.undo.push_redo(RedoMark { mark, redo_oid });
        let list = paths.join(", ");
        self.pending_notes.push(format!(
            "The user undid your changes from the last turn: {list}."
        ));
        self.notify(format!("Undone: {list}"));
        self.persist_if_owned();
    }

    async fn redo_last_undo(&mut self) {
        let Some(cwd) = self.undo_cwd("/redo") else {
            return;
        };
        let Some(redo) = self.undo.redo.last().cloned() else {
            self.notify("Nothing to redo.");
            return;
        };
        if !oid_exists(&cwd, &redo.redo_oid).await {
            self.notify(TOO_OLD);
            return;
        }
        let Some(current) = self.snapshot_now(&cwd).await else {
            self.notify("Couldn't redo: could not snapshot the current state");
            return;
        };
        let changes = match Self::restore_effects(&cwd, &current, &redo.redo_oid).await {
            Ok(changes) => changes,
            Err(e) => {
                self.notify(format!("Couldn't redo: {e}"));
                return;
            }
        };
        if changes.is_empty() {
            // The turn's changes exist again (the user put them back), so
            // the turn is undoable again — return it to the undo list.
            if let Some(redo) = self.undo.pop_redo() {
                self.undo.push_mark_back(redo.mark);
            }
            self.notify("Nothing to redo — those changes are already back.");
            self.persist_if_owned();
            return;
        }
        // Paths edited since the undo (which left the worktree at the
        // turn's `before_oid`): the redo would overwrite those edits.
        let changed_after: HashSet<String> =
            git(&cwd, &["diff", "--name-only", &redo.mark.before_oid, &current])
                .await
                .map(|out| out.lines().map(str::to_string).collect())
                .unwrap_or_default();
        let paths: Vec<String> = changes.iter().map(|(p, _)| p.clone()).collect();
        let entries: Vec<PreviewEntry> = changes
            .into_iter()
            .map(|(path, kind)| PreviewEntry {
                changed_after: changed_after.contains(&path),
                path,
                kind,
            })
            .collect();
        let preview = preview_text("Redo the last undo?", &entries);
        if !self.confirm("redo", preview).await {
            self.notify("Redo cancelled.");
            return;
        }
        if let Err(e) = self
            .executor
            .restore_to_checkpoint(&redo.redo_oid, &CancellationToken::new())
            .await
        {
            self.notify(format!("Couldn't redo: {e}"));
            return;
        }
        let redo = self.undo.pop_redo().expect("checked above");
        self.undo.push_mark_back(redo.mark);
        let list = paths.join(", ");
        self.pending_notes
            .push(format!("The user restored your changes: {list}."));
        self.notify(format!("Redone: {list}"));
        self.persist_if_owned();
    }

    async fn list_checkpoints(&mut self) {
        let Some(cwd) = self.executor.checkpoint_cwd().map(Path::to_path_buf) else {
            self.notify(NO_CHECKPOINTS);
            return;
        };
        let mut missing = HashSet::new();
        for mark in &self.undo.marks {
            if !oid_exists(&cwd, &mark.before_oid).await {
                missing.insert(mark.before_oid.clone());
            }
        }
        self.notify(checkpoints_listing(
            &self.undo.marks,
            local_offset_secs(),
            |m| missing.contains(&m.before_oid),
        ));
    }
}
