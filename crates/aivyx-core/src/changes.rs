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
    // One pass over name-status, so a turn touching thousands of files
    // doesn't rescan it per numstat line.
    let statuses: std::collections::HashMap<&str, ChangeStatus> = name_status
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(s, p)| {
            let status = match s.chars().next() {
                Some('A') => ChangeStatus::Added,
                Some('D') => ChangeStatus::Removed,
                _ => ChangeStatus::Modified,
            };
            (p, status)
        })
        .collect();
    let status_of = |path: &str| statuses.get(path).cloned().unwrap_or(ChangeStatus::Modified);
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
