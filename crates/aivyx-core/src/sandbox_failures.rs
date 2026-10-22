//! Tests the user knows fail only inside the sandbox (`[verification]
//! sandbox_failures`): a test that lists `/tmp` or walks up to `/`, say.
//! Coder can't tell such a failure from a real one on its own — a test can
//! swallow the permission error it hit (live finding, 2026-10-21: just's
//! `search::tests::not_found` prints only "No justfile found error was
//! expected", and a 9B model spent a whole `--auto` run "fixing" the
//! search code around it). With the list, the model is told up front and
//! on every run where they fail that these failures are not its task, and
//! verification passes when they are the only failures.
//!
//! Failures are read with `FailureParser`, so the list works for libtest,
//! pytest and `go test`. Whenever the output can't be read with confidence
//! (another runner, a compile error, a crash, output cut short) nothing is
//! excused.

use std::sync::Arc;

use crate::failing_tests::FailureParser;

/// How many names a note lists before summarising the rest.
const NOTE_NAMES: usize = 5;

/// How many patterns the system prompt lists before summarising the rest.
const PROMPT_PATTERNS: usize = 20;

/// The marker `aivyx_tools::run` puts at the start of a stream it cut down
/// to its tail: failures before the cut are unknown.
const TRUNCATION_MARKER: &str = " bytes truncated ...]";

/// The user's list. An entry matches a test name exactly, or, ending in
/// `*`, every name starting with what comes before it
/// (`global::*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxFailures(Arc<[String]>);

/// What a failed test run's output says about the known sandbox failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Assessment {
    /// Failing tests on the list.
    pub(crate) known: Vec<String>,
    /// Failing tests not on it.
    pub(crate) other: Vec<String>,
    /// Whether the whole output was seen. Cut short, there may be other
    /// failures the parser never saw.
    pub(crate) complete: bool,
}

impl Assessment {
    /// Every failure is a known sandbox failure, and that's certain.
    pub(crate) fn only_known(&self) -> bool {
        self.complete && self.other.is_empty()
    }

    /// The note appended to the run's output for the model.
    /// `verification` adds what the outcome means for verification.
    pub(crate) fn note(&self, verification: bool) -> String {
        let mut note = String::from("\n--- note from aivyx-coder -- KNOWN SANDBOX FAILURES. ");
        if self.only_known() {
            note.push_str(&format!(
                "Every test that failed here is on the user's list of tests that fail only \
                 inside the sandbox ([verification] sandbox_failures): {}. They are not bugs and \
                 not part of your task: do not change code or tests for them.",
                list_names(&self.known)
            ));
            if verification {
                note.push_str(" Verification counts this run as passed.");
            }
            return note;
        }
        note.push_str(&format!(
            "These failing tests are on the user's list of tests that fail only inside the \
             sandbox ([verification] sandbox_failures): {}. Leave them failing: do not change \
             code or tests for them.",
            list_names(&self.known)
        ));
        if !self.other.is_empty() {
            note.push_str(&format!(" The other failures are real: {}.", list_names(&self.other)));
        }
        if !self.complete {
            note.push_str(" The output was cut short, so there may be other failures too.");
        }
        note
    }
}

impl SandboxFailures {
    /// The list from config, plus the entries it can't use: blank ones,
    /// and a bare `*`, which would excuse every failure.
    pub fn new(patterns: &[String]) -> (Self, Vec<String>) {
        let (usable, rejected): (Vec<String>, Vec<String>) = patterns
            .iter()
            .map(|pattern| pattern.trim().to_string())
            .partition(|pattern| !pattern.trim_end_matches('*').is_empty());
        (Self(usable.into()), rejected)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `test` is on the list.
    pub fn matches(&self, test: &str) -> bool {
        self.0.iter().any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => test.starts_with(prefix),
            None => test == pattern,
        })
    }

    /// For a failed test run's output: which failures are on the list.
    /// `None` when the list is empty, the output can't be read with
    /// confidence, or none of the failures is on the list.
    pub(crate) fn assess(&self, output: &str) -> Option<Assessment> {
        if self.is_empty() {
            return None;
        }
        let mut parser = FailureParser::default();
        output.lines().for_each(|line| parser.feed(line));
        self.assess_parsed(parser, !output.contains(TRUNCATION_MARKER))
    }

    /// `assess` for output already fed to `parser` line by line (`/test`
    /// streams it). `complete`: every line of the output was fed.
    pub(crate) fn assess_parsed(
        &self,
        parser: FailureParser,
        complete: bool,
    ) -> Option<Assessment> {
        if self.is_empty() {
            return None;
        }
        let run = parser.finish()?;
        let (known, other): (Vec<String>, Vec<String>) =
            run.failing.into_iter().partition(|test| self.matches(test));
        if known.is_empty() {
            return None;
        }
        Some(Assessment { known, other, complete })
    }

    /// The system prompt's line naming the list, `None` when it's empty.
    pub(crate) fn prompt(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        Some(format!(
            "Tests the user knows fail only inside the sandbox, not because of a bug \
             ([verification] sandbox_failures): {}. Leave them failing and never change code or \
             tests for them; verification doesn't count them.",
            list_with_limit(&self.0, PROMPT_PATTERNS)
        ))
    }
}

fn list_names(names: &[String]) -> String {
    list_with_limit(names, NOTE_NAMES)
}

fn list_with_limit(names: &[String], limit: usize) -> String {
    let shown = names.iter().take(limit).map(String::as_str).collect::<Vec<_>>();
    match names.len().saturating_sub(limit) {
        0 => shown.join(", "),
        more => format!("{} and {more} more", shown.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(patterns: &[&str]) -> SandboxFailures {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        let (list, rejected) = SandboxFailures::new(&patterns);
        assert!(rejected.is_empty(), "{rejected:?}");
        list
    }

    /// just's suite under the sandbox: one real failure, one that only
    /// the sandbox causes.
    const TWO_FAILURES: &str = "\
running 3 tests
.FF
failures:

---- functions::trim_end_match stdout ----
boom

---- search::tests::not_found stdout ----
No justfile found error was expected

failures:
    functions::trim_end_match
    search::tests::not_found

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";

    const ONLY_SANDBOX: &str = "\
running 2 tests
.F
failures:

---- search::tests::not_found stdout ----
No justfile found error was expected

failures:
    search::tests::not_found

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";

    #[test]
    fn an_entry_matches_exactly_or_as_a_prefix_with_a_star() {
        let list = list(&["search::tests::not_found", "global::*"]);
        assert!(list.matches("search::tests::not_found"));
        assert!(!list.matches("search::tests::not_found_either"));
        assert!(!list.matches("search::tests"));
        assert!(list.matches("global::linux"));
        assert!(!list.matches("init::global"));
    }

    #[test]
    fn blank_entries_and_a_bare_star_are_rejected() {
        let patterns: Vec<String> =
            ["", "  ", "*", "**", " ok "].iter().map(|p| p.to_string()).collect();
        let (list, rejected) = SandboxFailures::new(&patterns);
        assert_eq!(rejected.len(), 4, "{rejected:?}");
        assert!(list.matches("ok"));
        assert!(!list.matches("anything"));
    }

    #[test]
    fn only_known_failures_are_excused() {
        let list = list(&["search::tests::not_found"]);
        let only = list.assess(ONLY_SANDBOX).expect("a known failure");
        assert!(only.only_known(), "{only:?}");
        let note = only.note(true);
        assert!(note.contains("Every test that failed here"), "{note}");
        assert!(note.contains("search::tests::not_found"), "{note}");
        assert!(note.ends_with("Verification counts this run as passed."), "{note}");

        let mixed = list.assess(TWO_FAILURES).expect("a known failure");
        assert!(!mixed.only_known(), "{mixed:?}");
        assert_eq!(mixed.other, ["functions::trim_end_match"]);
        let note = mixed.note(true);
        assert!(note.contains("The other failures are real: functions::trim_end_match"), "{note}");
        assert!(!note.contains("counts this run as passed"), "{note}");
    }

    #[test]
    fn nothing_is_excused_when_no_failure_is_on_the_list() {
        assert_eq!(list(&["global::*"]).assess(TWO_FAILURES), None);
        assert_eq!(SandboxFailures::default().assess(ONLY_SANDBOX), None);
    }

    #[test]
    fn nothing_is_excused_from_output_that_cant_be_read_with_confidence() {
        let list = list(&["search::tests::not_found"]);
        // A compile error.
        let broken = format!("{ONLY_SANDBOX}\nerror[E0425]: cannot find value `x`\n");
        assert_eq!(list.assess(&broken), None);
        // No summary at all.
        assert_eq!(list.assess("search::tests::not_found failed\n"), None);
        // Cut short: earlier failures may be missing.
        let cut = format!("[... 9000 bytes truncated ...]\n{ONLY_SANDBOX}");
        let assessment = list.assess(&cut).expect("a known failure");
        assert!(!assessment.only_known(), "{assessment:?}");
        assert!(assessment.note(true).contains("cut short"));
    }

    #[test]
    fn the_prompt_names_the_list() {
        assert_eq!(SandboxFailures::default().prompt(), None);
        let prompt = list(&["search::tests::not_found", "global::*"]).prompt().unwrap();
        assert!(prompt.contains("search::tests::not_found, global::*"), "{prompt}");
    }
}
