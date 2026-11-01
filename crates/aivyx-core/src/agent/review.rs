//! The agent's side of `[autonomous] final_review`: the worktree snapshots
//! the report's review changes are measured between, and the fresh context
//! a review turn can start from (`final_review_fresh_context`).

use std::path::Path;

use aivyx_types::{ToolCall, ToolCallId, ToolCallSource, ToolOutput};
use tokio_util::sync::CancellationToken;

use super::Agent;

/// At most this many files named in the goal go into a fresh review context.
const REVIEW_FILES: usize = 4;
/// Each of those files is cut at this many bytes.
const REVIEW_FILE_CAP: usize = 64 * 1024;
/// The diff since the run started is cut at this many bytes.
const REVIEW_DIFF_CAP: usize = 64 * 1024;
/// The last verification output keeps at most its last this many bytes
/// (a test runner's summary comes last).
const REVIEW_VERIFICATION_CAP: usize = 16 * 1024;
/// `read_file` calls per named file, at most (each returns up to
/// `aivyx_tools::MAX_READ_CHARS` characters).
const READS_PER_FILE: usize = 16;

/// What a review turn started from a fresh context sees.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReviewContext {
    pub goal: String,
    /// `(path as named in the goal, contents)`, read through `read_file`.
    pub files: Vec<(String, String)>,
    /// The diff since the run started; `None` without a starting snapshot
    /// (not a git repository).
    pub diff: Option<String>,
    /// The last full `[verification] command` run's output, if one ran.
    pub last_verification: Option<String>,
}

/// The files `goal` names, in order, each once: words with a `.` or `/` in
/// them (quotes, brackets and sentence punctuation stripped) that are files
/// under `cwd`. URLs are left out. At most [`REVIEW_FILES`].
pub(crate) fn files_named_in(goal: &str, cwd: &Path) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in goal.split_whitespace() {
        let word = word
            .trim_matches(|c: char| "`'\"()[]{}<>,;:!?*".contains(c))
            .trim_end_matches('.');
        if word.is_empty()
            || word.contains("://")
            || !(word.contains('.') || word.contains('/'))
            || found.iter().any(|f| f == word)
        {
            continue;
        }
        if aivyx_tools::resolve(cwd, word).is_file() {
            found.push(word.to_string());
            if found.len() == REVIEW_FILES {
                break;
            }
        }
    }
    found
}

/// `text` cut to at most `max` bytes at a char boundary, keeping the start.
fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… {} bytes cut", &text[..cut], text.len() - cut)
}

/// `text` cut to at most `max` bytes at a char boundary, keeping the end.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("… {start} bytes cut\n{}", &text[start..])
}

/// Room left for each cut-marker ("… N bytes cut") and the line break
/// around it, so a part cut to fit still fits with its marker.
const MARKER_ROOM: usize = 32;

/// `context` cut so the whole review message (`render_review` with
/// `request`) stays within `budget` bytes. Parts are filled in priority
/// order (the goal, the files it names in order — the spec the review checks
/// against — the last verification output, then the diff), so the
/// lowest-priority parts are cut first, each with a visible "… N bytes cut"
/// marker.
fn fit_review_context(context: ReviewContext, request: &str, budget: usize) -> ReviewContext {
    // The message with every part empty is the fixed cost.
    let skeleton = ReviewContext {
        goal: String::new(),
        files: context.files.iter().map(|(path, _)| (path.clone(), String::new())).collect(),
        diff: context.diff.as_ref().map(|_| String::new()),
        last_verification: context.last_verification.as_ref().map(|_| String::new()),
    };
    let parts = 3 + context.files.len();
    let fixed = render_review(&skeleton, request).len() + parts * MARKER_ROOM;
    let mut room = budget.saturating_sub(fixed);
    let mut take = |text: &str, keep_end: bool| -> String {
        let cut = if keep_end { tail(text, room) } else { head(text, room) };
        room = room.saturating_sub(text.len().min(room));
        cut
    };
    let goal = take(&context.goal, false);
    let files = context
        .files
        .iter()
        .map(|(path, text)| (path.clone(), take(text, false)))
        .collect();
    let last_verification = context.last_verification.as_deref().map(|v| take(v, true));
    let diff = context.diff.as_deref().map(|d| take(d, false));
    ReviewContext { goal, files, diff, last_verification }
}

/// Lockfiles: generated, often large, and nothing to review line by line.
/// A fresh-context review lists them in the diff summary, without a body.
const LOCKFILES: [&str; 7] = [
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Gemfile.lock",
    "go.sum",
];

fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    LOCKFILES.contains(&name)
}

/// A `--stat`-style summary of every changed file (lockfiles marked), then
/// `diff` without the sections of lockfiles. The summary comes first so a
/// diff cut to fit still names every file.
fn diff_with_summary(changes: &[crate::changes::FileChange], diff: &str) -> String {
    use crate::changes::ChangeStatus;
    let mut text = match changes.len() {
        1 => "1 file changed:\n".to_string(),
        n => format!("{n} files changed:\n"),
    };
    for change in changes {
        let counts = match (&change.status, change.added, change.removed) {
            (ChangeStatus::Removed, _, _) => "removed".to_string(),
            (_, Some(a), Some(r)) => format!("+{a} −{r}"),
            _ => "binary".to_string(),
        };
        let new = if change.status == ChangeStatus::Added { ", new" } else { "" };
        let lock = if is_lockfile(&change.path) { " (lockfile: diff left out)" } else { "" };
        text.push_str(&format!("  {} | {counts}{new}{lock}\n", change.path));
    }
    text.push('\n');
    let mut keep = true;
    for line in diff.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).trim_end();
            keep = !is_lockfile(path);
        }
        if keep {
            text.push_str(line);
        }
    }
    text
}

/// The fresh-context review turn's user message: what the review needs,
/// then `request` (the review message itself).
fn render_review(context: &ReviewContext, request: &str) -> String {
    let mut text = String::from(
        "The conversation so far has been set aside so you can review the work with fresh \
         eyes. Here is what you need; read any other file you want.\n\n## The goal\n\n",
    );
    text.push_str(&context.goal);
    for (path, contents) in &context.files {
        text.push_str(&format!(
            "\n\n## {path} (named in the goal)\n\n----- begin {path} -----\n{contents}"
        ));
        if !contents.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("----- end {path} -----"));
    }
    text.push_str("\n\n## Changes since the run started\n\n");
    match context.diff.as_deref() {
        None => text.push_str("(not available: no snapshot from the run's start)"),
        Some(diff) if diff.trim().is_empty() => text.push_str("(no changes)"),
        Some(diff) => {
            text.push_str("----- begin diff -----\n");
            text.push_str(diff);
            if !diff.ends_with('\n') {
                text.push('\n');
            }
            text.push_str("----- end diff -----");
        }
    }
    if let Some(output) = &context.last_verification {
        text.push_str("\n\n## The last verification run\n\n----- begin output -----\n");
        text.push_str(output);
        if !output.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("----- end output -----");
    }
    text.push_str("\n\n");
    text.push_str(request);
    text
}

/// A `read_file` result split into its text and, when the file goes on,
/// the `offset` its closing note says to read from next.
fn split_read_note(output: &str) -> (&str, Option<usize>) {
    let at = output
        .rfind("\n[read_file: ")
        .or_else(|| output.starts_with("[read_file: ").then_some(0));
    let Some(at) = at else {
        return (output, None);
    };
    let note = &output[at..];
    if !note.trim_end().ends_with(']') {
        return (output, None);
    }
    let next = note
        .split_once("pass offset=")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok());
    (&output[..at], next)
}

impl Agent {
    /// Snapshots the worktree now and returns the commit oid, or `None`
    /// without a checkpointer (not a git repository) or when git fails.
    pub(crate) async fn snapshot_worktree(&self, label: &str) -> Option<String> {
        self.undo_snapshot(label).await.map(|(_, oid)| oid)
    }

    /// The files that differ between the snapshot `oid` and the worktree
    /// now, generated files left out. `None` when git fails.
    pub(crate) async fn changed_paths_since(&self, oid: &str) -> Option<Vec<String>> {
        let root = self.executor.checkpoint_cwd()?.to_path_buf();
        let now = self.snapshot_worktree("final review end").await?;
        let changes = self.changes_between(&root, oid, &now).await?;
        Some(changes.into_iter().map(|c| c.path).collect())
    }

    /// The changes between `mark_run_start`'s snapshot and the worktree
    /// now, generated files left out: a summary of every changed file, then
    /// the diff without lockfile bodies (`diff_with_summary`). `None`
    /// without a starting snapshot or when git fails.
    pub(crate) async fn diff_since_run_start(&self) -> Option<String> {
        let before = self.run_start_oid.as_deref()?;
        let root = self.executor.checkpoint_cwd()?.to_path_buf();
        let now = self.snapshot_worktree("final review diff").await?;
        let changes = self.changes_between(&root, before, &now).await?;
        let diff = self.visible_diff(&root, before, &now).await.ok()?;
        Some(diff_with_summary(&changes, &diff))
    }

    /// Reads `path` the way the model's own `read_file` call would — through
    /// `ToolExecutor::dispatch`, so the permission gate (and with it
    /// `deny_paths`) decides — page by page up to [`REVIEW_FILE_CAP`].
    /// Each page is scanned for prompt injection like any tool output.
    /// `None` when the read is refused or fails.
    async fn read_for_review(&mut self, path: &str, cwd: &Path) -> Option<String> {
        let mut text = String::new();
        let mut offset = 1usize;
        for _ in 0..READS_PER_FILE {
            self.synthetic_seq += 1;
            let call = ToolCall {
                id: ToolCallId(format!("final-review-read-{}", self.synthetic_seq)),
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": path, "offset": offset }),
                source: ToolCallSource::AutoVerification,
            };
            let result = self.executor.dispatch(call, cwd, CancellationToken::new()).await;
            let ToolOutput::Ok(output) = result.output else {
                break;
            };
            if let Some(finding) =
                aivyx_sandbox::scan_for_injection_markers(&output, &format!("read_file: {path}"))
            {
                self.injection_taint.flag(finding);
            }
            let (body, next) = split_read_note(&output);
            text.push_str(body);
            match next {
                Some(n) if n > offset && text.len() < REVIEW_FILE_CAP => offset = n,
                _ => break,
            }
        }
        (!text.is_empty()).then(|| head(&text, REVIEW_FILE_CAP))
    }

    /// Gathers what a fresh-context review turn starts from: the goal, the
    /// files it names (read through the gate, size-capped), the diff since
    /// the run started, and the last verification output. The files and
    /// the verification output are scanned for prompt injection like any
    /// tool output; the diff is the run's own work and is not.
    pub(crate) async fn review_context(&mut self, goal: &str, cwd: &Path) -> ReviewContext {
        let mut files = Vec::new();
        for path in files_named_in(goal, cwd) {
            if let Some(text) = self.read_for_review(&path, cwd).await {
                files.push((path, text));
            }
        }
        let diff = self.diff_since_run_start().await.map(|diff| head(&diff, REVIEW_DIFF_CAP));
        let last_verification = self
            .last_full_verification_output
            .as_deref()
            .map(|output| tail(output, REVIEW_VERIFICATION_CAP));
        if let Some(output) = &last_verification
            && let Some(finding) =
                aivyx_sandbox::scan_for_injection_markers(output, "verification output")
        {
            self.injection_taint.flag(finding);
        }
        ReviewContext { goal: goal.to_string(), files, diff, last_verification }
    }

    /// Sets the conversation aside for a fresh-context review turn and
    /// returns its whole user message: the context, then `request` (the
    /// review message). One user message, so chat templates that require
    /// alternating roles still work. The message is kept to a quarter of
    /// `[backend] context_tokens` (estimated with the agent's own
    /// characters-per-token ratio), cutting the lowest-priority parts first
    /// (`fit_review_context`).
    ///
    /// Only the live conversation history changes; the saved session still
    /// holds the conversation set aside, followed by the review's
    /// (`history_before_review`). The system prompt is built
    /// per request and is kept, and so is everything else: the task list,
    /// the permission gate and its approvals, checkpoints and the `/undo`
    /// ledger, the injection taint, plan mode, the autonomous goal,
    /// verification state and usage counters. The review needs the same
    /// rules and the same run, just without the run's long history.
    pub(crate) fn replace_history_for_review(
        &mut self,
        context: ReviewContext,
        request: &str,
    ) -> String {
        // Kept for the saved session (`snapshot_state`), so a review turn
        // doesn't overwrite the run's conversation with its own.
        let set_aside = std::mem::take(&mut self.history);
        self.history_before_review.extend(set_aside);
        let budget = (f64::from(self.context_limit / 4) * self.chars_per_token) as usize;
        render_review(&fit_review_context(context, request, budget), request)
    }

    /// The token estimate for `text`, with the ratio compaction uses.
    #[cfg(test)]
    pub(crate) fn estimate_tokens(&self, text: &str) -> u32 {
        (text.len() as f64 / self.chars_per_token).ceil() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_file_note_gives_the_next_offset() {
        let page = "a\nb\n\n[read_file: lines 1-2 of 9 — pass offset=3 to read on, or grep for the part you need]";
        assert_eq!(split_read_note(page), ("a\nb\n", Some(3)));
        assert_eq!(split_read_note("whole file\n"), ("whole file\n", None));
        let last = "x\n\n[read_file: lines 8-9 of 9]";
        assert_eq!(split_read_note(last), ("x\n", None));
        assert_eq!(split_read_note("[read_file: the file has 3 lines — nothing at line 9]"), ("", None));
    }

    #[test]
    fn caps_keep_the_right_end_on_char_boundaries() {
        assert_eq!(head("abc", 5), "abc");
        assert_eq!(head("ééé", 3), "é\n… 4 bytes cut");
        assert_eq!(tail("ééé", 3), "… 4 bytes cut\né");
        assert_eq!(tail("abc", 3), "abc");
    }

    #[test]
    fn the_lowest_priority_parts_are_cut_first() {
        let context = ReviewContext {
            goal: "GOAL".to_string(),
            files: vec![("a.md".to_string(), "F".repeat(5000))],
            diff: Some("D".repeat(5000)),
            last_verification: Some("V".repeat(500)),
        };
        let fitted = fit_review_context(context, "REQUEST", 7000);
        assert_eq!(fitted.goal, "GOAL");
        assert_eq!(fitted.files[0].1, "F".repeat(5000), "the spec comes before the rest");
        assert_eq!(fitted.last_verification.as_deref(), Some("V".repeat(500).as_str()));
        let diff = fitted.diff.as_deref().unwrap();
        assert!(diff.contains("bytes cut"), "the diff is cut first: {diff}");
        assert!(render_review(&fitted, "REQUEST").len() <= 7000);
    }

    #[test]
    fn the_diff_opens_with_a_summary_and_leaves_lockfile_bodies_out() {
        use crate::changes::{ChangeStatus, FileChange};
        let changes = [
            FileChange { path: "src/lib.rs".into(), added: Some(3), removed: Some(1), status: ChangeStatus::Modified },
            FileChange { path: "Cargo.lock".into(), added: Some(90), removed: Some(2), status: ChangeStatus::Modified },
            FileChange { path: "web/package-lock.json".into(), added: Some(5), removed: Some(0), status: ChangeStatus::Added },
        ];
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n+SRC\n\
                    diff --git a/Cargo.lock b/Cargo.lock\n+LOCKBODY\n\
                    diff --git a/web/package-lock.json b/web/package-lock.json\n+LOCKBODY\n";
        let text = diff_with_summary(&changes, diff);
        assert!(text.starts_with("3 files changed:\n"), "{text}");
        assert!(text.contains("src/lib.rs | +3 −1"), "{text}");
        assert!(text.contains("Cargo.lock | +90 −2 (lockfile: diff left out)"), "{text}");
        assert!(text.contains("web/package-lock.json"), "{text}");
        assert!(text.contains("+SRC"), "{text}");
        assert!(!text.contains("LOCKBODY"), "{text}");
    }
}
