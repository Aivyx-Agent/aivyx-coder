//! Before `--auto` discards a round of edits whose verification still
//! fails: did the round make things better? Live finding (2026-10-21, just
//! bug hunt in Docker): the model fixed all three planted bugs and rightly
//! left alone a test that fails only inside the sandbox — and the whole
//! round was discarded, because the remaining failure looked like the
//! edits' fault. A failure that predates the edits says nothing about them.
//!
//! The test command is run twice, streamed and uncapped (`run_command`
//! keeps only the tail of its output, which can drop part of the failure
//! list): on the edited tree, then on the pre-experiment checkpoint, after
//! which the edits are put back. The round is kept only when it fixed at
//! least one failing test and broke none (`failing_tests::progress`).

use std::path::Path;
use std::process::Stdio;

use tokio::io::BufReader;
use tokio_util::sync::CancellationToken;

use super::Agent;
use super::test_command::{CappedLines, MAX_LINE_BYTES};
use crate::failing_tests::{FailureParser, Progress, TestRun, progress};

impl Agent {
    /// Whether this round of edits fixed failing tests without breaking
    /// any, comparing the worktree as it stands with `pre_experiment_ref`.
    /// Leaves the worktree as it found it either way. `None` whenever that
    /// can't be said with confidence — no test command, unrecognised
    /// output, a run that timed out or was cancelled, a checkpoint that
    /// couldn't be taken — so the caller discards, as before.
    pub(super) async fn edits_made_progress(
        &mut self,
        pre_experiment_ref: &str,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Option<Progress> {
        let tests = self.tests.clone()?;
        self.info(format!(
            "Checking whether this round of edits fixed anything before discarding it: running \
             `{}` with and without them.",
            tests.display()
        ));
        let after = self.failing_tests(cwd, cancellation).await?;
        if after.failing.is_empty() {
            // The suite passes now: the verification failure is something
            // else (a lint, a build step), which this can't judge.
            return None;
        }
        let edited_ref = self.executor.checkpoint_now("progress-check", cancellation).await?;
        if self
            .executor
            .restore_to_checkpoint(pre_experiment_ref, cancellation)
            .await
            .is_err()
        {
            self.restore_edits(&edited_ref).await;
            return None;
        }
        let before = self.failing_tests(cwd, cancellation).await;
        if !self.restore_edits(&edited_ref).await {
            return None;
        }
        progress(&before?, &after)
    }

    /// Puts the edited worktree back. A fresh token: a cancelled turn must
    /// not leave the worktree at the pre-experiment state by accident.
    async fn restore_edits(&mut self, edited_ref: &str) -> bool {
        match self
            .executor
            .restore_to_checkpoint(edited_ref, &CancellationToken::new())
            .await
        {
            Ok(()) => true,
            Err(err) => {
                self.info(format!(
                    "Couldn't restore the edits after checking them ({err}); the worktree is at \
                     the pre-experiment checkpoint, and the edits are in {edited_ref}."
                ));
                false
            }
        }
    }

    /// Runs the test command confined, as `/test` does, feeding every line
    /// to a `FailureParser`. `None` if it couldn't start, timed out or was
    /// cancelled, or its output wasn't recognised.
    async fn failing_tests(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Option<TestRun> {
        let tests = self.tests.as_ref()?;
        let mut command = tokio::process::Command::new(&tests.program);
        command
            .args(&tests.args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut command = self.executor.confiner().confine(command);
        command.process_group(0);
        aivyx_tools::die_with_parent(&mut command);
        let mut child = command.spawn().ok()?;
        // Killed on every way out of this function (see `ProcessGroup`).
        let mut group = aivyx_tools::ProcessGroup::of(&child);
        let mut stdout =
            CappedLines::new(BufReader::new(child.stdout.take()?), MAX_LINE_BYTES);
        let mut stderr =
            CappedLines::new(BufReader::new(child.stderr.take()?), MAX_LINE_BYTES);
        let (mut out_open, mut err_open) = (true, true);
        let mut exited = None;
        let mut parser = FailureParser::default();
        let deadline = tokio::time::sleep(self.test_timeout);
        tokio::pin!(deadline);
        // When the command itself exits, the rest of its group is killed at
        // once: a background job left holding the pipes would otherwise keep
        // this reading until the deadline.
        while out_open || err_open {
            let line = tokio::select! {
                line = stdout.next_line(), if out_open => {
                    out_open = line.is_some();
                    line
                }
                line = stderr.next_line(), if err_open => {
                    err_open = line.is_some();
                    line
                }
                status = child.wait(), if exited.is_none() => {
                    exited = Some(status.is_ok());
                    group.kill();
                    None
                }
                _ = &mut deadline => return None,
                _ = cancellation.cancelled() => return None,
            };
            if let Some(line) = line {
                parser.feed(&line);
            }
        }
        let finished = match exited {
            Some(waited) => waited,
            None => tokio::select! {
                status = child.wait() => status.is_ok(),
                _ = &mut deadline => false,
                _ = cancellation.cancelled() => false,
            },
        };
        group.kill();
        if !finished {
            return None;
        }
        parser.finish()
    }
}

/// How many test names a notice lists before summarising the rest.
const NOTICE_NAMES: usize = 5;

/// The end-of-turn notice when `--auto` keeps a round of edits whose
/// verification still fails. `failing` is the usual "verification (`x`)
/// still failing after N attempt(s)" lead.
pub(super) fn kept_edits_notice(failing: &str, progress: &Progress) -> String {
    format!(
        "{failing} — but this round of edits fixed {} failing test(s) ({}) and broke none, so it \
         was kept. Still failing, as before these edits: {}.",
        progress.fixed.len(),
        list_names(&progress.fixed),
        list_names(&progress.still_failing),
    )
}

fn list_names(names: &[String]) -> String {
    let shown = names.iter().take(NOTICE_NAMES).map(String::as_str).collect::<Vec<_>>();
    match names.len().saturating_sub(NOTICE_NAMES) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}
