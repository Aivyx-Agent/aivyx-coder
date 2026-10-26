//! Facts aivyx-coder checks itself when an autonomous run stops, instead of
//! repeating the model's claims: the last verification, final checks, and
//! the files the run changed. They go into the stop notice and, with
//! `--report`, a JSON file. In the build tests (2026-10-22/23) final
//! answers said fields were `null` that weren't, and that clippy was clean
//! after three warnings.

use serde::Serialize;
use std::borrow::Cow;

/// One `[verification] final_checks` command, run by aivyx-coder when the
/// goal was reached.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FinalCheck {
    pub name: String,
    pub passed: bool,
    pub warnings: usize,
}

/// A file that differs between the run's start and its end.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChangedFile {
    pub path: String,
    /// `added`, `modified` or `removed`.
    pub status: &'static str,
    /// Lines added and removed; `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
    /// Matches `[permissions] read_only_paths`: changed by a command,
    /// since the gate refuses the file tools.
    pub read_only: bool,
}

/// What the stop notice and the report say beyond the stop reason.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunFacts {
    /// The `[verification] command` and whether its last run passed
    /// (`None` when it never ran).
    pub verification: Option<(String, Option<bool>)>,
    pub final_checks: Vec<FinalCheck>,
    /// `None` when there was no snapshot to compare (not a git repository).
    pub changed: Option<Vec<ChangedFile>>,
}

/// Token use over a run's main-loop model requests, as the server reported
/// it. Sub-agents and specialists aren't counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub requests: u32,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// The largest single prompt: how full the context got.
    pub peak_prompt_tokens: u32,
}

impl UsageTotals {
    pub fn record(&mut self, prompt_tokens: u32, completion_tokens: u32) {
        self.requests += 1;
        self.prompt_tokens += u64::from(prompt_tokens);
        self.completion_tokens += u64::from(completion_tokens);
        self.peak_prompt_tokens = self.peak_prompt_tokens.max(prompt_tokens);
    }
}

/// The `--report` format's version: bumped when a field changes meaning
/// or goes away, not when one is added.
pub const REPORT_VERSION: u32 = 1;

/// The `--report` file's contents.
#[derive(Debug, Clone)]
pub struct RunReport<'a> {
    pub report_version: u32,
    pub coder_version: &'a str,
    pub goal: &'a str,
    /// `goal_achieved`, `budget_exhausted`, `cancelled`, `failed` or
    /// `injection_detected`.
    pub outcome: &'a str,
    pub notice: &'a str,
    pub iterations: u32,
    pub duration_secs: u64,
    pub last_verification_passed: Option<bool>,
    pub final_checks: &'a [FinalCheck],
    pub changed_files: &'a [ChangedFile],
    /// `[backend]` model and address, and `reasoning_effort` if set.
    pub model: &'a str,
    pub base_url: &'a str,
    pub reasoning_effort: Option<&'a str>,
    pub usage: UsageTotals,
    /// Tool-call checkpoints during the run, and how many failed.
    pub checkpoints: u32,
    pub checkpoint_failures: u32,
}

/// The JSON shape: `RunReport` flattened, with the read-only files that
/// changed listed on their own.
#[derive(Serialize)]
struct ReportJson<'a> {
    report_version: u32,
    coder_version: &'a str,
    goal: &'a str,
    outcome: &'a str,
    notice: &'a str,
    iterations: u32,
    duration_secs: u64,
    last_verification_passed: Option<bool>,
    final_checks: &'a [FinalCheck],
    changed_files: &'a [ChangedFile],
    model: &'a str,
    base_url: Cow<'a, str>,
    reasoning_effort: Option<&'a str>,
    model_requests: u32,
    prompt_tokens: u64,
    completion_tokens: u64,
    peak_context_tokens: u32,
    checkpoints: u32,
    checkpoint_failures: u32,
    /// Files in `read_only_paths` that changed anyway (a command can change
    /// what the file tools may not).
    read_only_touched: Vec<&'a str>,
}

/// How many changed files the notice names before "and N more".
const NOTICE_FILE_CAP: usize = 5;

/// The warning count in a command result's status line
/// (`exit status: 0 (success), 3 warnings`); 0 when it has none.
pub fn warnings_in(output: &str) -> usize {
    let status = output.lines().next().unwrap_or_default();
    status
        .split_once("(success), ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// `url` without any `user:password@` in its authority (the part between
/// `://` and the first `/`, `?` or `#`). Anything else comes back as is.
fn without_userinfo(url: &str) -> Cow<'_, str> {
    let Some(scheme_end) = url.find("://").map(|i| i + 3) else {
        return Cow::Borrowed(url);
    };
    let rest = &url[scheme_end..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => Cow::Owned(format!("{}{}", &url[..scheme_end], &rest[at + 1..])),
        None => Cow::Borrowed(url),
    }
}

impl RunFacts {
    /// The stop notice's lines after its first, in order: verification,
    /// final checks, changed files, read-only files changed.
    pub fn notice_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some((name, passed)) = &self.verification {
            let verdict = match passed {
                Some(true) => "passed",
                Some(false) => "failed",
                None => "never ran",
            };
            lines.push(format!("verification: `{name}` {verdict}"));
        }
        for check in &self.final_checks {
            let verdict = match (check.passed, check.warnings) {
                (false, _) => "failed".to_string(),
                (true, 0) => "passed".to_string(),
                (true, 1) => "passed, 1 warning".to_string(),
                (true, n) => format!("passed, {n} warnings"),
            };
            lines.push(format!("final check `{}`: {verdict}", check.name));
        }
        let Some(changed) = &self.changed else {
            return lines;
        };
        if changed.is_empty() {
            lines.push("changed: no files".to_string());
            return lines;
        }
        let added: u32 = changed.iter().filter_map(|f| f.added).sum();
        let removed: u32 = changed.iter().filter_map(|f| f.removed).sum();
        let mut names: Vec<&str> =
            changed.iter().take(NOTICE_FILE_CAP).map(|f| f.path.as_str()).collect();
        let more = changed.len().saturating_sub(NOTICE_FILE_CAP);
        let more = if more > 0 { format!(" and {more} more") } else { String::new() };
        let count = match changed.len() {
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        };
        lines.push(format!("changed: {count} (+{added} −{removed}) — {}{more}", names.join(", ")));
        names = changed.iter().filter(|f| f.read_only).map(|f| f.path.as_str()).collect();
        if !names.is_empty() {
            lines.push(format!("read-only files changed: {}", names.join(", ")));
        }
        lines
    }
}

impl RunReport<'_> {
    /// Pretty-printed JSON, as written to the `--report` file.
    pub fn to_json(&self) -> String {
        let json = ReportJson {
            report_version: self.report_version,
            coder_version: self.coder_version,
            goal: self.goal,
            outcome: self.outcome,
            notice: self.notice,
            iterations: self.iterations,
            duration_secs: self.duration_secs,
            last_verification_passed: self.last_verification_passed,
            final_checks: self.final_checks,
            changed_files: self.changed_files,
            model: self.model,
            base_url: without_userinfo(self.base_url),
            reasoning_effort: self.reasoning_effort,
            model_requests: self.usage.requests,
            prompt_tokens: self.usage.prompt_tokens,
            completion_tokens: self.usage.completion_tokens,
            peak_context_tokens: self.usage.peak_prompt_tokens,
            checkpoints: self.checkpoints,
            checkpoint_failures: self.checkpoint_failures,
            read_only_touched: self
                .changed_files
                .iter()
                .filter(|f| f.read_only)
                .map(|f| f.path.as_str())
                .collect(),
        };
        serde_json::to_string_pretty(&json).expect("a RunReport always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, status: &'static str, added: u32, removed: u32, read_only: bool) -> ChangedFile {
        ChangedFile { path: path.into(), status, added: Some(added), removed: Some(removed), read_only }
    }

    #[test]
    fn warnings_are_read_from_a_command_result() {
        assert_eq!(warnings_in("exit status: 0 (success), 3 warnings\n--- stdout ---\n"), 3);
        assert_eq!(warnings_in("exit status: 0 (success), 1 warning\n--- stdout ---\n"), 1);
        assert_eq!(warnings_in("exit status: 0 (success)\n--- stdout ---\n3 warnings"), 0);
        assert_eq!(warnings_in("exit status: 101 (failed)\n"), 0);
    }

    #[test]
    fn the_notice_lists_verification_checks_and_changes() {
        let facts = RunFacts {
            verification: Some(("test".into(), Some(true))),
            final_checks: vec![
                FinalCheck { name: "clippy".into(), passed: true, warnings: 3 },
                FinalCheck { name: "fmt".into(), passed: false, warnings: 0 },
            ],
            changed: Some(vec![
                file("src/main.rs", "modified", 10, 2, false),
                file("data/airports.csv", "modified", 1, 1, true),
                file("src/db.rs", "added", 300, 0, false),
            ]),
        };
        assert_eq!(
            facts.notice_lines(),
            [
                "verification: `test` passed",
                "final check `clippy`: passed, 3 warnings",
                "final check `fmt`: failed",
                "changed: 3 files (+311 −3) — src/main.rs, data/airports.csv, src/db.rs",
                "read-only files changed: data/airports.csv",
            ]
        );
    }

    #[test]
    fn the_notice_caps_the_file_list_and_says_what_it_does_not_know() {
        let many: Vec<ChangedFile> =
            (0..8).map(|i| file(&format!("f{i}.rs"), "modified", 1, 0, false)).collect();
        let facts = RunFacts {
            verification: Some(("test".into(), None)),
            final_checks: vec![],
            changed: Some(many),
        };
        assert_eq!(
            facts.notice_lines(),
            [
                "verification: `test` never ran",
                "changed: 8 files (+8 −0) — f0.rs, f1.rs, f2.rs, f3.rs, f4.rs and 3 more",
            ]
        );
        let nothing = RunFacts { changed: Some(vec![]), ..RunFacts::default() };
        assert_eq!(nothing.notice_lines(), ["changed: no files"]);
        assert!(RunFacts::default().notice_lines().is_empty());
    }

    #[test]
    fn the_report_serializes_every_field() {
        let checks = [FinalCheck { name: "clippy".into(), passed: true, warnings: 3 }];
        let files = [
            file("src/main.rs", "modified", 10, 2, false),
            file("BRIEF.md", "modified", 1, 1, true),
        ];
        let report = RunReport {
            report_version: REPORT_VERSION,
            coder_version: "0.6.3",
            goal: "Build it.",
            outcome: "goal_achieved",
            notice: "autonomous run stopped: goal achieved after 5 iteration(s)",
            iterations: 5,
            duration_secs: 1200,
            last_verification_passed: Some(true),
            final_checks: &checks,
            changed_files: &files,
            model: "Qwen3.8-27B",
            base_url: "http://llama:8080/v1",
            reasoning_effort: Some("medium"),
            usage: UsageTotals {
                requests: 9,
                prompt_tokens: 90_000,
                completion_tokens: 12_000,
                peak_prompt_tokens: 41_000,
            },
            checkpoints: 30,
            checkpoint_failures: 1,
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["report_version"], 1);
        let with_userinfo = RunReport { base_url: "http://user:secret@llama:8080/v1", ..report };
        let json_userinfo: serde_json::Value =
            serde_json::from_str(&with_userinfo.to_json()).unwrap();
        assert_eq!(json_userinfo["base_url"], "http://llama:8080/v1");
        assert_eq!(json["coder_version"], "0.6.3");
        assert_eq!(json["outcome"], "goal_achieved");
        assert_eq!(json["iterations"], 5);
        assert_eq!(json["final_checks"][0]["warnings"], 3);
        assert_eq!(json["changed_files"][0]["status"], "modified");
        assert_eq!(json["changed_files"][0]["read_only"], false);
        assert_eq!(json["last_verification_passed"], true);
        assert_eq!(json["model"], "Qwen3.8-27B");
        assert_eq!(json["base_url"], "http://llama:8080/v1");
        assert_eq!(json["reasoning_effort"], "medium");
        assert_eq!(json["model_requests"], 9);
        assert_eq!(json["prompt_tokens"], 90_000);
        assert_eq!(json["completion_tokens"], 12_000);
        assert_eq!(json["peak_context_tokens"], 41_000);
        assert_eq!(json["checkpoints"], 30);
        assert_eq!(json["checkpoint_failures"], 1);
        assert_eq!(json["read_only_touched"], serde_json::json!(["BRIEF.md"]));
    }

    #[test]
    fn userinfo_is_stripped_from_a_base_url() {
        assert_eq!(without_userinfo("http://user:pass@llama:8080/v1"), "http://llama:8080/v1");
        assert_eq!(
            without_userinfo("https://token@api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(without_userinfo("http://llama:8080/v1"), "http://llama:8080/v1");
        assert_eq!(without_userinfo("http://llama:8080/v1/path@x"), "http://llama:8080/v1/path@x");
        assert_eq!(without_userinfo(""), "");
    }

    #[test]
    fn usage_totals_add_up_and_keep_the_peak() {
        let mut usage = UsageTotals::default();
        usage.record(100, 20);
        usage.record(300, 5);
        usage.record(200, 10);
        assert_eq!(
            usage,
            UsageTotals { requests: 3, prompt_tokens: 600, completion_tokens: 35, peak_prompt_tokens: 300 }
        );
    }
}
