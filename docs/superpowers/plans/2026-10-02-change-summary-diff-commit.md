# Change summary, /diff, /commit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**

- After every turn that changed files, show a one-line summary of what changed.
- `/diff` reviews uncommitted changes, or the last turn's changes, in a scrollable full-screen view.
- `/commit` commits with a message the model drafts and the user approves, edits, or cancels.

**Architecture:**

- **Pure helpers** live in a new `crates/aivyx-core/src/changes.rs`: numstat parsing, the summary line, diff
  trimming and truncation, and `-m` argument parsing.
- **The commands** live in a new `crates/aivyx-core/src/agent/change_commands.rs`, following the pattern of
  `agent/undo_commands.rs`. They are intercepted in `Agent::run_turn`, ask through the existing
  `command_prompter`, and get git state from the verified checkpoint snapshots that part 1 added
  (`ToolExecutor::checkpoint_now`).
- **The commit** runs through a confined git command shared with the `git_commit` tool.
- **New events:**
  - `AgentEvent::Info(String)` for neutral notices (the summary line and command results), rendered dim
    rather than as red errors;
  - `AgentEvent::ShowDiff { title, text }`, which the TUI opens as a full-screen pager.

**Tech Stack:** Rust 2024, tokio, ratatui (TUI), git plumbing through `aivyx_tools::run_git`.

**Spec:** `docs/superpowers/specs/2026-10-02-change-summary-diff-commit-design.md` (part 2 of 4). It depends on
part 1 (`/undo`, merged at 7645c77): `UndoLedger.marks` hold `before_oid` and `after_oid` per changing turn.

## Global Constraints

- **Branch and commits.** Work on branch `feat/diff-commit` from `main`. Commit with `git commit -s`; the
  message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Checks before each commit.** `cargo clippy --workspace --all-targets -- -D warnings` (Rust 1.98) is clean
  and the touched crate's tests pass. The last task also runs
  `CARGO_TARGET_DIR=/tmp/claude-1000/-home-julian-Projects-Rust/5d3d9165-cd90-484b-bf84-962d37590e4e/scratchpad/c199 ~/.cargo/bin/cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace`.
- **Copy (exact strings):**
  - Summary: `Changed: <entries joined by " · ">`. An entry is `<path> (+A −D)`, `<path> (+A, new)`,
    `<path> (removed)` or `<path> (binary)`. At most 5 entries, then ` · and N more`.
  - Diff titles: `Uncommitted changes`; `Changes from the last turn ("<preview>")`.
  - Diff truncation footer: `… diff truncated (N more lines) — use git diff for the rest`, after 5,000 lines.
  - "Nothing to commit."
  - "No uncommitted changes."
  - "No changes from the last turn to show."
  - "Not a git repository."
  - "The commit hook rejected it:" followed by the hook's output on the next lines.
  - `Couldn't draft a message (<reason>) — commit with /commit -m "…".`
  - `Committed <short-hash>: <subject>`
  - `Commit cancelled — nothing was committed.`
  - Draft instruction: `Write a git commit message for this diff: a subject line of at most 72 characters in
    the imperative mood, then optionally a blank line and a short body. Reply with the message only.`
- **Empty tree id:** `4b825dc642cb6eb9a060e54bf8d69288fbee4904`.
- **Prompt budget:** diffs sent to the model are trimmed to `context_tokens * 4 / 3` characters, about a third
  of the context in characters at 4 chars/token. File names are always kept.
- **Pager keys:** ↑/↓ by one line, PgUp/PgDn by one screen, Home/End, Esc or q to close.
- **Commit modal keys:** y commit · e edit · n/Esc cancel. No "always allow" for commit.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/aivyx-core/src/changes.rs` (new) | Pure: `FileChange`, `parse_numstat`, `summary_line`, `trim_diff_for_prompt`, `truncate_lines`, `parse_commit_message_arg`, `COMMIT_DRAFT_PROMPT`, `EMPTY_TREE` |
| `crates/aivyx-core/src/agent/types.rs` | `AgentEvent::Info(String)`, `AgentEvent::ShowDiff { title: String, text: String }` |
| `crates/aivyx-core/src/agent/mod.rs` | `info()` helper; emit the summary after a recorded mark; intercept `/diff` and `/commit` |
| `crates/aivyx-core/src/agent/change_commands.rs` (new) | `/diff`, `/diff turn`, `/commit`, `/commit -m` |
| `crates/aivyx-tools/src/lib.rs` + `tools/git_commit.rs` | `pub fn confined_git(args, cwd, confiner) -> tokio::process::Command`, shared by the `git_commit` tool and `/commit`; `ToolExecutor::confiner()` accessor |
| `crates/aivyx-acp/src/translate.rs` | map `Info` / `ShowDiff` to agent message chunks |
| `crates/aivyx-tui/src/app.rs` | `ChatLine::Info` (dim); `DiffView` pager (state, keys, render); commit modal `e` key and footer; pre-fill the input |
| `crates/aivyx-core/src/commands.rs` | `/diff`, `/commit` entries |
| `README.md`, `CLAUDE.md` | docs |

---

### Task 1: Pure change helpers

**Files:**
- Create: `crates/aivyx-core/src/changes.rs`
- Modify: `crates/aivyx-core/src/lib.rs` (`pub mod changes;`, alphabetical)

**Interfaces:**
- Produces (exact):
  ```rust
  pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
  pub const COMMIT_DRAFT_PROMPT: &str = "Write a git commit message for this diff: a subject line of at most 72 characters in the imperative mood, then optionally a blank line and a short body. Reply with the message only.";
  pub const DIFF_LINE_CAP: usize = 5_000;
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ChangeStatus { Modified, Added, Removed }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct FileChange { pub path: String, pub added: Option<u32>, pub removed: Option<u32>, pub status: ChangeStatus }
  pub fn parse_numstat(numstat: &str, name_status: &str) -> Vec<FileChange>;
  pub fn summary_line(changes: &[FileChange]) -> Option<String>;
  pub fn trim_diff_for_prompt(diff: &str, file_names: &[String], budget_chars: usize) -> String;
  pub fn truncate_lines(text: &str, max_lines: usize) -> String;
  pub fn parse_commit_message_arg(rest: &str) -> Option<String>;
  ```

**`parse_numstat` semantics.**

- **Inputs:** `numstat` is `git diff --numstat <a> <b>` (`A\tD\tpath`, or `-\t-\tpath` for binary).
  `name_status` is `git diff --name-status <a> <b>`, used only to find `A`/`D` statuses by path.
- **Status per path:** `A` → `Added`, `D` → `Removed`, anything else → `Modified`.
- **Counts:** binary gives `added`/`removed` = `None`.
- **Renames:** ignore numstat's `{old => new}` rename notation. Both git calls in this plan pass
  `--no-renames`, so it never appears.
- **Order:** the same as `numstat`.

**`summary_line` formatting:**

- **Entries:**
  - `Added` → `path (+A, new)`;
  - `Removed` → `path (removed)`;
  - binary (`added == None`) → `path (binary)`;
  - otherwise → `path (+A −D)`, where the minus is U+2212.
- **Cap:** at most 5 entries, then `and N more`.
- **Result:** joined with ` · ` and prefixed `Changed: `. Empty input → `None`.

- [ ] **Step 1: Write the failing tests.** Create the file with only this test module (plus `use super::*;`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_and_name_status_combine() {
        let numstat = "3\t1\tstats.py\n12\t0\ttest_stats.py\n0\t4\told.txt\n-\t-\tlogo.png\n";
        let name_status = "M\tstats.py\nA\ttest_stats.py\nD\told.txt\nM\tlogo.png\n";
        assert_eq!(
            parse_numstat(numstat, name_status),
            vec![
                FileChange { path: "stats.py".into(), added: Some(3), removed: Some(1), status: ChangeStatus::Modified },
                FileChange { path: "test_stats.py".into(), added: Some(12), removed: Some(0), status: ChangeStatus::Added },
                FileChange { path: "old.txt".into(), added: Some(0), removed: Some(4), status: ChangeStatus::Removed },
                FileChange { path: "logo.png".into(), added: None, removed: None, status: ChangeStatus::Modified },
            ]
        );
        assert!(parse_numstat("", "").is_empty());
    }

    #[test]
    fn summary_line_formats_and_caps() {
        let c = |p: &str, a: Option<u32>, r: Option<u32>, s: ChangeStatus| FileChange { path: p.into(), added: a, removed: r, status: s };
        assert_eq!(summary_line(&[]), None);
        assert_eq!(
            summary_line(&[
                c("stats.py", Some(3), Some(1), ChangeStatus::Modified),
                c("test_stats.py", Some(12), Some(0), ChangeStatus::Added),
                c("old.txt", Some(0), Some(4), ChangeStatus::Removed),
                c("logo.png", None, None, ChangeStatus::Modified),
            ]).unwrap(),
            "Changed: stats.py (+3 −1) · test_stats.py (+12, new) · old.txt (removed) · logo.png (binary)"
        );
        let many: Vec<FileChange> = (0..8).map(|i| c(&format!("f{i}"), Some(1), Some(0), ChangeStatus::Modified)).collect();
        assert_eq!(
            summary_line(&many).unwrap(),
            "Changed: f0 (+1 −0) · f1 (+1 −0) · f2 (+1 −0) · f3 (+1 −0) · f4 (+1 −0) · and 3 more"
        );
    }

    #[test]
    fn trim_keeps_file_names_and_fits_the_budget() {
        let diff = format!("diff --git a/x b/x\n{}", "+line\n".repeat(1000));
        let trimmed = trim_diff_for_prompt(&diff, &["x".into(), "y".into()], 200);
        assert!(trimmed.len() <= 200 + 200, "{}", trimmed.len()); // budget plus the files header
        assert!(trimmed.starts_with("Files: x, y\n\n"), "{trimmed}");
        assert!(trimmed.ends_with("… (diff trimmed)"), "{trimmed}");
        let small = "diff --git a/x b/x\n+one\n";
        assert_eq!(trim_diff_for_prompt(small, &["x".into()], 1000), format!("Files: x\n\n{small}"));
    }

    #[test]
    fn truncate_lines_adds_the_footer() {
        let text = (0..10).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(truncate_lines(&text, 20), text);
        assert_eq!(
            truncate_lines(&text, 3),
            "l0\nl1\nl2\n… diff truncated (7 more lines) — use git diff for the rest"
        );
    }

    #[test]
    fn commit_message_arg_parsing() {
        assert_eq!(parse_commit_message_arg(""), None);
        assert_eq!(parse_commit_message_arg("--amend"), None);
        assert_eq!(parse_commit_message_arg("-m \"Fix the average\""), Some("Fix the average".into()));
        assert_eq!(parse_commit_message_arg("-m Fix it"), Some("Fix it".into()));
        assert_eq!(parse_commit_message_arg("-m \"Say \\\"hi\\\"\""), Some("Say \"hi\"".into()));
        assert_eq!(parse_commit_message_arg("-m \"Subject\n\nBody\""), Some("Subject\n\nBody".into()));
        assert_eq!(parse_commit_message_arg("-m \"\""), None, "an empty message is no message");
    }
}
```

- [ ] **Step 2: Verify RED.** Run `cargo test -p aivyx-core --lib changes::`. Expected: compile errors.

- [ ] **Step 3: Implement** above the tests:

```rust
//! Pure helpers for the change summary, `/diff` and `/commit`.

pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
pub const COMMIT_DRAFT_PROMPT: &str = "Write a git commit message for this diff: a subject line of at most 72 characters in the imperative mood, then optionally a blank line and a short body. Reply with the message only.";
pub const DIFF_LINE_CAP: usize = 5_000;
const SUMMARY_CAP: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeStatus {
    Modified,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    pub status: ChangeStatus,
}

pub fn parse_numstat(numstat: &str, name_status: &str) -> Vec<FileChange> {
    let status_of = |path: &str| {
        name_status
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .find(|(_, p)| *p == path)
            .map(|(s, _)| match s.chars().next() {
                Some('A') => ChangeStatus::Added,
                Some('D') => ChangeStatus::Removed,
                _ => ChangeStatus::Modified,
            })
            .unwrap_or(ChangeStatus::Modified)
    };
    numstat
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (a, r, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(FileChange {
                path: path.to_string(),
                added: a.parse().ok(),
                removed: r.parse().ok(),
                status: status_of(path),
            })
        })
        .collect()
}

pub fn summary_line(changes: &[FileChange]) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = changes
        .iter()
        .take(SUMMARY_CAP)
        .map(|c| match (&c.status, c.added, c.removed) {
            (ChangeStatus::Removed, _, _) => format!("{} (removed)", c.path),
            (_, None, _) | (_, _, None) => format!("{} (binary)", c.path),
            (ChangeStatus::Added, Some(a), _) => format!("{} (+{a}, new)", c.path),
            (ChangeStatus::Modified, Some(a), Some(r)) => format!("{} (+{a} −{r})", c.path),
        })
        .collect();
    if changes.len() > SUMMARY_CAP {
        parts.push(format!("and {} more", changes.len() - SUMMARY_CAP));
    }
    Some(format!("Changed: {}", parts.join(" · ")))
}

pub fn trim_diff_for_prompt(diff: &str, file_names: &[String], budget_chars: usize) -> String {
    let header = format!("Files: {}\n\n", file_names.join(", "));
    if diff.len() <= budget_chars {
        return format!("{header}{diff}");
    }
    let mut cut = budget_chars;
    while !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{header}{}\n… (diff trimmed)", &diff[..cut])
}

pub fn truncate_lines(text: &str, max_lines: usize) -> String {
    let total = text.lines().count();
    if total <= max_lines {
        return text.to_string();
    }
    let kept: Vec<&str> = text.lines().take(max_lines).collect();
    format!(
        "{}\n… diff truncated ({} more lines) — use git diff for the rest",
        kept.join("\n"),
        total - max_lines
    )
}

/// `-m "message"` / `-m message` → the message. `None` when the argument
/// isn't `-m …` or the message is empty.
pub fn parse_commit_message_arg(rest: &str) -> Option<String> {
    let rest = rest.trim().strip_prefix("-m")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    let message = match rest.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(quoted) => quoted.replace("\\\"", "\""),
        None => rest.to_string(),
    };
    (!message.trim().is_empty()).then_some(message)
}
```

  An added *binary* file reports `-\t-`, so it shows `(binary)` — acceptable.

- [ ] **Step 4: Verify GREEN.** Run `cargo test -p aivyx-core --lib changes::` (5 pass) and
  `cargo clippy -p aivyx-core --all-targets -- -D warnings`.

- [ ] **Step 5: Commit.**
  `git commit -s -m "core: pure helpers for the change summary, /diff and /commit" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"`
  The message must END with the trailer; use a heredoc as earlier tasks did.

---

### Task 2: `AgentEvent::Info`, the change summary, and the confined git helper

**Files:**
- Modify:
  - `crates/aivyx-core/src/agent/types.rs`: add `Info(String)` and `ShowDiff { title: String, text: String }`
    to `AgentEvent`, with doc comments.
  - `crates/aivyx-core/src/agent/mod.rs`: `pub(crate) fn info(&self, text)`, which emits `AgentEvent::Info`;
    the summary after `self.undo.record(…)` in `run_turn`.
  - `crates/aivyx-acp/src/translate.rs`: `Info(text)` → `AgentMessageChunk`, like `Error`; `ShowDiff` → a
    chunk with `"{title}\n\n```diff\n{text}\n```"`.
  - `crates/aivyx-tui/src/app.rs`: `ChatLine::Info(String)`, rendered `prefixed_lines(text, "  ",
    Style::default().fg(Color::DarkGray))`. `handle_agent_event` maps `Info` → `ChatLine::Info` without
    touching `streaming_active`. Add a `ShowDiff` arm that Task 4 fills in; for now push
    `ChatLine::Info(title)`. Update the second exhaustive `match event` (~line 1078) too.
  - `crates/aivyx-tools/src/tools/git_commit.rs`: make the command builder shareable.
  - `crates/aivyx-tools/src/lib.rs`: re-export it; add `ToolExecutor::confiner(&self) -> Arc<dyn ExecutionConfiner>`.
- Test: `crates/aivyx-core/src/agent/tests.rs`, `crates/aivyx-tui/src/app.rs` tests,
  `crates/aivyx-acp/src/translate.rs` tests.

**Interfaces:**
- Produces:
  - `AgentEvent::Info(String)`, `AgentEvent::ShowDiff { title: String, text: String }`;
  - `pub fn confined_git(args: &[String], cwd: &Path, confiner: &dyn ExecutionConfiner) -> tokio::process::Command`
    (in `aivyx_tools`). It adds `-c core.fsmonitor=false`, sets cwd, null stdin, piped stdout and stderr, and
    confines;
  - `ToolExecutor::confiner()`.

**Change summary.** Right after `self.undo.record(TurnMark { … })` in `run_turn`, compute the summary from the
mark's `before_oid` → `after_oid`:

```rust
let numstat = aivyx_tools::run_git(&cwd, &["diff", "--no-renames", "--numstat", &before_oid, &after_oid], &[]).await;
let names = aivyx_tools::run_git(&cwd, &["diff", "--no-renames", "--name-status", &before_oid, &after_oid], &[]).await;
if let (Ok(n), Ok(s)) = (numstat, names)
    && let Some(line) = crate::changes::summary_line(&crate::changes::parse_numstat(&n, &s))
{
    self.info(line);
}
```

Restructure the `record` block so the oids are still available. Emit it before `persist` and before
`run_turn` returns; `TurnComplete` was already emitted inside the inner turn, so the summary arrives after
the reply, which is the intended place.

- [ ] **Step 1: Write the failing tests.**
  - **Agent.** Reusing `undo_agent_with_events` from the undo tests, after a turn that writes `a.txt` ("one\n")
    and edits `tracked.txt`, `AgentEvent::Info` contains exactly
    `"Changed: a.txt (+1, new) · tracked.txt (+1 −1)"`. The order is git's numstat order, alphabetical. If
    your fixture differs, assert on the contained entries. A read-only turn emits no `Info`.
  - **TUI.** `handle_agent_event(AgentEvent::Info("x".into()))` pushes `ChatLine::Info("x")` and leaves
    `streaming_active` unchanged. `chat_line_to_lines(&ChatLine::Info(...))` has no `"!"` prefix.
  - **ACP.** `translate_event` of `Info` and `ShowDiff` yields an `AgentMessageChunk`.
  - **Tools.** `confined_git(&["status".into()], cwd, &NoopConfiner)` built `Command`'s args start with
    `["-c", "core.fsmonitor=false", "status"]`. Use `as_std().get_args()`. The existing `git_commit` tool tests
    must still pass.
- [ ] **Step 2: Verify RED.**
- [ ] **Step 3: Implement.** In `git_commit.rs`, replace the private `git_command(args, ctx)` body with a call
  to the new public `confined_git(args, &ctx.cwd, ctx.confiner.as_ref())`, so the tool and `/commit` share one
  builder.
- [ ] **Step 4: Verify GREEN.** Run `cargo test -p aivyx-core -p aivyx-tui -p aivyx-acp -p aivyx-tools` and
  workspace clippy.
- [ ] **Step 5: Commit.** "core: show what each turn changed; Info events; shared confined git"

---

### Task 3: `/diff` and `/diff turn`

**Files:**
- Create: `crates/aivyx-core/src/agent/change_commands.rs` (`mod change_commands;` in `agent/mod.rs`)
- Modify:
  - `agent/mod.rs`: intercept next to the undo commands, before `turn_before`;
  - `crates/aivyx-core/src/commands.rs`: entries.
- Test: `agent/tests.rs`

**Interfaces:**
- Consumes:
  - `ToolExecutor::checkpoint_now`, `checkpoint_cwd`;
  - `resolve_oid`;
  - `crate::changes::{EMPTY_TREE, truncate_lines, DIFF_LINE_CAP}`;
  - `crate::undo::text_preview` (already applied in marks);
  - `AgentEvent::ShowDiff`.
- Produces:
  - `pub(super) enum ChangeCommand { Diff { turn: bool }, Commit { message: Option<String> } }`;
  - `pub(super) fn parse(user_input: &str) -> Option<ChangeCommand>`. `/diff turn` maps to `turn: true`;
    `/commit -m …` to `message: Some`; bare `/commit` to `None`.

**`/diff` behaviour.**

1. **Repository check.** With no `checkpoint_cwd`, report "Not a git repository." through `notify`.
2. **Snapshot.** `now = checkpoint_now("diff")` resolved to an oid; this tree includes untracked, non-ignored
   files. If that fails, "Couldn't read the changes: could not snapshot the current state".
3. **Base.**
   - `/diff` uses `HEAD` if `git rev-parse --verify HEAD` succeeds, else `EMPTY_TREE`.
   - `/diff turn` uses the last mark's `before_oid`. With no mark: "No changes from the last turn to show."
4. **Diff.** `git diff --no-renames <base> <now>`.
5. **Empty result.** "No uncommitted changes." (or "No changes from the last turn to show.") via `info`.
6. **Otherwise** emit `AgentEvent::ShowDiff { title, text: truncate_lines(&diff, DIFF_LINE_CAP) }`. The title
   is `Uncommitted changes` or `Changes from the last turn ("<preview>")`.

**Commands table:**

```rust
CommandInfo { name: "/diff", description: "Show uncommitted changes (`/diff turn`: just the last turn's)", tier: CommandTier::AgentState },
CommandInfo { name: "/commit", description: "Commit with a drafted message you approve (`/commit -m \"…\"` to write your own)", tier: CommandTier::AgentState },
```

- [ ] **Step 1: Write the failing tests** (temp git repo, fake backend, an event receiver):
  - an edited `tracked.txt` plus an untracked `new.txt` → `ShowDiff` titled `Uncommitted changes` whose text
    contains `+v2`, `new.txt`, and `new file mode`;
  - a clean repo → `Info("No uncommitted changes.")`;
  - `/diff turn` after a turn that wrote `a.txt`, plus a hand edit to `tracked.txt` *before* the turn → the
    text mentions `a.txt` and not the earlier hand edit;
  - `/diff turn` with no marks → "No changes from the last turn to show.";
  - a repo with no commits → the diff is against the empty tree and lists every file;
  - no checkpointer → `Error("Not a git repository.")`;
  - parse tests: `/diff`, `/diff turn`, `/commit`, `/commit -m "x"`, and `/differ` → `None`.
- [ ] **Step 2: Verify RED.**
- [ ] **Step 3: Implement** following `undo_commands.rs`'s structure: a module-level `git` helper,
  `impl Agent { pub(super) async fn run_change_command(&mut self, cmd) }`. Leave `Commit` as a stub that Task 5
  implements: `self.notify("…")` is fine here, but no `unimplemented!`. Wire the interception exactly like
  `undo_commands::parse`: `emit(TurnComplete)` and return.
- [ ] **Step 4: Verify GREEN**, then **Step 5: Commit** "core: /diff and /diff turn".

---

### Task 4: The TUI diff pager

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs`

**Interfaces:**
- Consumes: `AgentEvent::ShowDiff { title, text }`.
- Produces: `struct DiffView { title: String, lines: Vec<String>, offset: usize }` with
  `fn scroll(&mut self, key: DiffKey, viewport: usize)`, and `enum DiffKey { Up, Down, PageUp, PageDown, Home, End }`.

**Behaviour.**

- **Opening.** `ShowDiff` sets `app.diff_view = Some(DiffView { title, lines: text.lines()…, offset: 0 })`.
- **Keys while open.** All keys go to the pager first, before the permission modal check, and the pager is
  never opened while a modal is up. The mapping is ↑/↓ → ±1 line, PgUp/PgDn → ±(viewport − 1) lines,
  Home/End, and Esc/q to close (`diff_view = None`). Other keys are swallowed.
- **Offset clamp.** `offset` is clamped to `lines.len().saturating_sub(viewport)`.
- **Rendering.**
  - A full-screen `Clear` plus a bordered block titled `<title> — ↑↓ PgUp/PgDn Home/End · Esc to close`.
  - Lines start at `offset`, coloured with the existing `diff_line()` helper the approval modal uses.
  - Lines are not wrapped (`Paragraph` without wrap) so the line maths stays exact.

- [ ] **Step 1: Write the failing tests** (pure `DiffView::scroll`):
  - down and up by one, clamped at 0 and at max;
  - PgDn by viewport − 1;
  - End → `len − viewport`;
  - Home → 0;
  - a viewport larger than the diff → offset always 0.

  Plus an `App` test: `handle_agent_event(ShowDiff{…})` sets `diff_view`, and closing sets it back to `None`.
  Use the existing key-dispatch seam if there is one; otherwise test a small `fn diff_key(code) ->
  Option<DiffKeyAction>` mapping function.
- [ ] **Step 2: Verify RED.** **Step 3: Implement.** **Step 4: Verify GREEN** (`cargo test -p aivyx-tui`,
  clippy).
- [ ] **Step 5: Commit.** "tui: a scrollable diff view for /diff"

---

### Task 5: `/commit`

**Files:**
- Modify:
  - `crates/aivyx-core/src/agent/change_commands.rs` (the `Commit` arm);
  - `crates/aivyx-tui/src/app.rs`: the commit modal's `e` key and footer, and pre-filling the input.
- Test: `agent/tests.rs`, `app.rs` tests

**Interfaces:**
- Consumes:
  - `confined_git`, `ToolExecutor::confiner()`;
  - `crate::council::collect_text`, `crate::council::strip_think`;
  - `crate::changes::{trim_diff_for_prompt, COMMIT_DRAFT_PROMPT}`;
  - `command_prompter`.
- Produces: TUI `fn commit_edit_input(draft: &str) -> String`, which returns `/commit -m "<draft with \" escaped>"`.

**Behaviour.**

1. **Repository check.** With no `checkpoint_cwd`, report "Not a git repository."
2. **Staged or not.** Staged changes exist iff `git diff --cached --quiet` exits non-zero (Err from
   `run_git`).
   - If there are none, run `git add -A`. `staged_by_us` = the names from `git diff --cached --name-only`
     taken after the add; nothing was staged before, so all of them are ours.
   - If the result is still empty: "Nothing to commit." Stop.
3. **Files and diff.** `files` = `git diff --cached --name-only`; `diff` = `git diff --cached --no-renames`.
4. **The message.**
   - With `-m`, it's the provided text. Skip the model and the modal.
   - Otherwise draft it. The request is
     `ChatRequest::new(vec![Message::text(Role::System, COMMIT_DRAFT_PROMPT), Message::text(Role::User,
     trim_diff_for_prompt(&diff, &files, (self.config.context_tokens as usize) * 4 / 3))])`, with
     `route = Some(RouteHint { task: TaskKind::Summarize, session: None, estimated_prompt_tokens: … })`, sent
     through `collect_text(self.llm.as_ref(), …, &CancellationToken::new())`.
   - Then `strip_think` and trim.
   - On `Err` or an empty result: restore staging and notify
     `Couldn't draft a message (<reason>) — commit with /commit -m "…".`
5. **Asking.** The prompter request uses:
   - `tool_name: "commit"` and `ActionKind::Write`;
   - target `Other("commit")`;
   - `arguments_preview: json!({ "message": draft })`;
   - `preview: Some(format!("{draft}\n\nFiles:\n{}", files.iter().map(|f| format!("  {f}")).join("\n")))`.

   Deny: restore staging, then `info("Commit cancelled — nothing was committed.")`.
6. **Committing.** `confined_git(["commit", "-q", "-m", message], cwd, confiner)` → `.output()`.
   - On failure: restore staging, then notify `"The commit hook rejected it:\n" + stderr + stdout` (trimmed).
     Use this wording when stderr is non-empty; otherwise `Couldn't commit: <status>`.
   - On success: `git rev-parse --short HEAD` → `info(format!("Committed {hash}: {subject}"))`, where subject
     is the first line of the message.
7. **Restoring staging:**
   - with a HEAD: `git reset -q -- <staged_by_us…>`;
   - with no HEAD: `git rm --cached -q -r -- <staged_by_us…>`.

   This is a no-op when `staged_by_us` is empty, so a hand-staged set is never touched.

**TUI.** When the pending modal's `request.tool_name == "commit"`:

- **Footer:** `[y] Commit    [e] Edit message    [n] / [Esc] Cancel`, with no always-allow. Extend
  `offer_always_allow` to return false for `"commit"` and `"undo"`/`"redo"`, which also fixes the part 1
  cosmetic issue.
- **`e` key:** read `request.arguments_preview["message"]` as a string, `resolve_permission(UserResponse::Deny)`,
  then set the input box's contents to `commit_edit_input(&draft)`. Use the same input-box API `take_input`
  uses: `new_input_box()` plus inserting text; find the tui-textarea insertion call already in the file.
- **Other modals:** `e` stays swallowed.

- [ ] **Step 1: Write the failing tests.**
  - **Agent** (temp repo with HEAD, `MockBackend` scripted to reply `"Fix the average\n\nUse len."`, scripted
    prompter):
    - nothing staged + Allow → a commit exists with that message, the worktree is clean, and `Info` is
      `Committed <hash>: Fix the average`;
    - a hand-staged `a.txt` plus an unstaged edit to `b.txt` → the commit contains only `a.txt`, and `b.txt` is
      still modified and unstaged;
    - Deny → no new commit, and the index is back to exactly what it was. Compare `git diff --cached
      --name-only` before and after, and check a previously hand-staged file stays staged;
    - `/commit -m "Hand written"` → commits without calling the backend (a backend that panics if called) and
      without prompting;
    - a `.git/hooks/pre-commit` that `exit 1`s with `echo nope >&2` → notify starts with
      `The commit hook rejected it:` and contains `nope`; no commit; staging restored. Write the hook with a
      plain `std::fs::write` in the test: this is the test fixture, not a tool call;
    - nothing to commit → "Nothing to commit.";
    - a backend that errors → the `Couldn't draft a message (` notice, and staging restored;
    - a repo with no commits → commit works, and cancel restores via `git rm --cached`.
  - **TUI:**
    - `commit_edit_input("Say \"hi\"")` == `"/commit -m \"Say \\\"hi\\\"\""`;
    - the footer for a commit request contains `[e] Edit message` and not `Always`;
    - `offer_always_allow` is false for commit, undo and redo.
- [ ] **Step 2: Verify RED.** **Step 3: Implement.** **Step 4: Verify GREEN** (workspace tests + clippy).
- [ ] **Step 5: Commit.** "feat: /commit with a drafted message you approve, edit or cancel"

---

### Task 6: Docs and final verification

- [ ] **README.** Add a section after the worktree-checkpoints and `/undo` text:

```markdown
**Reviewing and committing**: after every turn that changed files, a dim line
shows what changed (`Changed: stats.py (+3 −1) · test_stats.py (+12, new)`).
**`/diff`** opens everything uncommitted in a scrollable view (↑↓, PgUp/PgDn,
Home/End, Esc); **`/diff turn`** shows just the last turn's changes.
**`/commit`** commits what you've staged — or, if nothing is staged, every
uncommitted change (`.gitignore` respected) — with a message the model drafts
from the diff: approve it, press **e** to edit it, or cancel (your staging is
left exactly as it was). `/commit -m "message"` commits with your own message
straight away. Commits use your git identity and run your hooks.
```

- [ ] **CLAUDE.md.** Append this to the `aivyx-core` row: "`changes` + `agent/change_commands.rs` (per-turn
  change summary, `/diff`, `/commit` — drafts via a one-shot `collect_text`, commits through the shared
  `aivyx_tools::confined_git`)".
- [ ] **Full verification.** `cargo test --workspace`, clippy on Rust 1.98, and the Rust 1.99 command. All
  green.
- [ ] **Commit.** "docs: change summary, /diff and /commit"
