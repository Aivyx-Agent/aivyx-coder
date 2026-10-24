//! Which tests a test run reported as failing, read from its output line
//! by line — enough to tell whether a round of edits fixed failing tests
//! without breaking others (see `Agent::edits_made_progress`).
//!
//! Recognises cargo's libtest, pytest, `go test`, Vitest and Jest output.
//! Anything it can't account for — another runner, a build that didn't compile, a test
//! binary that crashed — gives `None`, never a guess: a missed failure
//! would make a broken change look like progress.

use std::collections::BTreeSet;

/// The failing tests in one run, plus what's needed to check that two runs
/// are comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestRun {
    /// Failing test names, as the runner printed them.
    pub(crate) failing: BTreeSet<String>,
    /// How many per-binary / per-package summaries the run printed. Fewer
    /// than another run means part of the suite didn't report.
    pub(crate) summaries: usize,
    /// Tests run (passed + failed), when the runner reports counts.
    pub(crate) total: Option<u64>,
}

/// What a round of edits did to the failing set, when it can be said with
/// confidence: it fixed at least one failing test and broke none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Progress {
    /// Failing before the edits, passing after.
    pub(crate) fixed: Vec<String>,
    /// Failing both before and after.
    pub(crate) still_failing: Vec<String>,
}

/// `Some` only when `after` is a strict improvement on `before`: every test
/// failing after was failing before, at least one failing test now passes,
/// and no part of the suite went missing (same or more summaries, same or
/// more tests run — deleting a failing test isn't fixing it).
pub(crate) fn progress(before: &TestRun, after: &TestRun) -> Option<Progress> {
    if !after.failing.is_subset(&before.failing) || after.summaries < before.summaries {
        return None;
    }
    if let (Some(before_total), Some(after_total)) = (before.total, after.total)
        && after_total < before_total
    {
        return None;
    }
    let fixed: Vec<String> = before.failing.difference(&after.failing).cloned().collect();
    if fixed.is_empty() {
        return None;
    }
    Some(Progress {
        fixed,
        still_failing: after.failing.iter().cloned().collect(),
    })
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Section {
    #[default]
    Other,
    /// Right after a libtest `failures:` line: either the per-test output
    /// (`---- name stdout ----` blocks) or the indented list of names.
    AfterFailures,
    /// Inside libtest's indented list of failing names.
    FailureList,
}

/// Fed one line at a time (the run's stdout and stderr, interleaved), so a
/// long run never has to be held in memory.
#[derive(Debug, Default)]
pub(crate) struct FailureParser {
    failing: BTreeSet<String>,
    libtest_summaries: usize,
    go_summaries: usize,
    pytest_summaries: usize,
    /// Vitest's `Test Files` lines and Jest's `Test Suites:` lines.
    js_summaries: usize,
    total: u64,
    counted: bool,
    /// Something the parser can't account for: a compile error, a crashed
    /// test binary, a Go build failure.
    unaccountable: bool,
    section: Section,
}

impl FailureParser {
    pub(crate) fn feed(&mut self, line: &str) {
        let line = line.trim_end();
        match self.section {
            Section::AfterFailures if line.is_empty() => return,
            Section::AfterFailures | Section::FailureList => {
                if let Some(name) = indented_name(line) {
                    self.section = Section::FailureList;
                    self.failing.insert(name.to_string());
                    return;
                }
                self.section = Section::Other;
            }
            Section::Other => {}
        }

        if self.feed_js(line.trim_start()) {
            return;
        }

        // libtest
        if line == "failures:" {
            self.section = Section::AfterFailures;
        } else if let Some(name) =
            line.strip_prefix("---- ").and_then(|rest| rest.strip_suffix(" stdout ----"))
        {
            self.failing.insert(name.to_string());
        } else if let Some(name) =
            line.strip_prefix("test ").and_then(|rest| rest.strip_suffix(" ... FAILED"))
        {
            self.failing.insert(name.to_string());
        } else if let Some(rest) = line.strip_prefix("test result: ") {
            self.libtest_summaries += 1;
            if let (Some(passed), Some(failed)) = (count(rest, " passed"), count(rest, " failed")) {
                self.total += passed + failed;
                self.counted = true;
            }
        } else if line.starts_with("error: could not compile")
            || line.starts_with("error[E")
            // A test binary killed by a signal never prints its summary.
            || (line.contains("process didn't exit successfully") && line.contains("(signal:"))
        {
            self.unaccountable = true;
        }
        // go test
        else if let Some(rest) = line.trim_start().strip_prefix("--- FAIL: ") {
            if let Some(name) = rest.split_whitespace().next() {
                self.failing.insert(name.to_string());
            }
        } else if line.starts_with("ok  \t") || line.starts_with("FAIL\t") {
            self.go_summaries += 1;
            if line.contains("[build failed]") || line.contains("[setup failed]") {
                self.unaccountable = true;
            }
        }
        // pytest
        else if let Some(rest) =
            line.strip_prefix("FAILED ").or_else(|| line.strip_prefix("ERROR "))
        {
            if let Some(name) = rest.split(" - ").next().map(str::trim)
                && !name.is_empty()
            {
                self.failing.insert(name.to_string());
            }
        } else if is_pytest_summary(line) {
            self.pytest_summaries += 1;
            let body = line.trim_matches('=').trim();
            let passed = pytest_count(body, "passed").unwrap_or(0);
            let failed = pytest_count(body, "failed").unwrap_or(0);
            let errors = pytest_count(body, "error").unwrap_or(0)
                + pytest_count(body, "errors").unwrap_or(0);
            self.total += passed + failed + errors;
            self.counted = true;
        }
    }

    /// Vitest and Jest lines; `true` when `line` was one of theirs.
    fn feed_js(&mut self, line: &str) -> bool {
        // Vitest: `FAIL  file > suite > test`, or `FAIL  file [ file ]` for a
        // test file that failed to load. Jest's `FAIL  file` (no `>`) only
        // names the file; its tests come on `●` lines.
        if let Some(rest) = line.strip_prefix("FAIL  ") {
            if let Some((file, _)) = rest.split_once(" [ ").filter(|_| rest.ends_with(" ]")) {
                self.failing.insert(file.to_string());
            } else if rest.contains(" > ") {
                self.failing.insert(rest.to_string());
            }
            return true;
        }
        if line.starts_with("Test Files  ") || line.starts_with("Test Suites:") {
            self.js_summaries += 1;
            return true;
        }
        // Vitest: `Tests  3 failed | 118 passed (121)`, or `Tests  no tests`.
        if let Some(rest) = line.strip_prefix("Tests  ") {
            let total = rest
                .rsplit_once('(')
                .and_then(|(_, n)| n.strip_suffix(')'))
                .and_then(|n| n.parse().ok());
            if let Some(total) = total.or_else(|| rest.contains("no tests").then_some(0)) {
                self.total += total;
                self.counted = true;
            }
            return true;
        }
        // Jest: `Tests:       2 failed, 3 passed, 5 total`.
        if let Some(rest) = line.strip_prefix("Tests:") {
            if let Some(total) = rest
                .split(',')
                .find_map(|part| part.trim().strip_suffix(" total"))
                .and_then(|n| n.trim().parse::<u64>().ok())
            {
                self.total += total;
                self.counted = true;
            }
            return true;
        }
        if let Some(name) = line.strip_prefix("● ") {
            if name.trim() == "Test suite failed to run" {
                self.unaccountable = true;
            } else {
                self.failing.insert(name.trim().to_string());
            }
            return true;
        }
        if line.contains("Unhandled Errors") && line.contains('⎯') {
            self.unaccountable = true;
            return true;
        }
        false
    }

    /// The run's failing tests, or `None` when no recognised runner
    /// reported, or something happened the parser can't account for.
    pub(crate) fn finish(self) -> Option<TestRun> {
        let summaries = self.libtest_summaries
            + self.go_summaries
            + self.pytest_summaries
            + self.js_summaries;
        if self.unaccountable || summaries == 0 {
            return None;
        }
        Some(TestRun {
            failing: self.failing,
            summaries,
            total: self.counted.then_some(self.total),
        })
    }
}

/// A libtest failure-list entry: four spaces, then one path-like token.
fn indented_name(line: &str) -> Option<&str> {
    let name = line.strip_prefix("    ")?;
    (!name.is_empty() && !name.starts_with(' ') && !name.contains(char::is_whitespace))
        .then_some(name)
}

/// `N label` from a libtest summary such as `494 passed; 6 failed; ...`.
fn count(summary: &str, label: &str) -> Option<u64> {
    summary
        .split(';')
        .find_map(|part| part.trim().strip_suffix(label))
        .and_then(|n| n.rsplit(' ').next())
        .and_then(|n| n.parse().ok())
}

/// pytest's closing line, e.g. `==== 2 failed, 40 passed in 1.23s ====`.
fn is_pytest_summary(line: &str) -> bool {
    line.starts_with('=')
        && line.ends_with('=')
        && line.contains(" in ")
        && ["passed", "failed", "error"].iter().any(|word| line.contains(word))
}

/// `N word` from a pytest summary body such as `2 failed, 40 passed in 1.2s`.
fn pytest_count(body: &str, word: &str) -> Option<u64> {
    body.split([',', ' '])
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[1] == word)
        .and_then(|pair| pair[0].parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(output: &str) -> Option<TestRun> {
        let mut parser = FailureParser::default();
        output.lines().for_each(|line| parser.feed(line));
        parser.finish()
    }

    fn names(run: &TestRun) -> Vec<&str> {
        run.failing.iter().map(String::as_str).collect()
    }

    /// `vitest run` with failures, escape codes stripped (as aivyx-coder now
    /// passes it on), from the ckl build test.
    const VITEST_BEFORE: &str = "\
 RUN  v5.0.5 /k/runs/m/app

 ✓ server/format.test.ts (18 tests) 5ms
 ❯ src/Quiz.test.tsx (7 tests | 1 failed) 128ms
   ❯ Quiz (7)
     × shows the first due card with aircraft, title and challenge 67ms
     ✓ a typed answer shows the result and the next due date 14ms
 ❯ server/quiz.test.ts (27 tests | 2 failed) 3684ms
   ❯ PUT matching cards by challenge (3)
     × the k-th item with a challenge matches the k-th existing card 182ms

 Test Files  2 failed | 6 passed (8)
      Tests  3 failed | 118 passed (121)
   Start at  00:53:12
   Duration  3.84s (tests 86%, environment 10%, import 3%, transform 1%)

⎯⎯⎯⎯⎯⎯⎯ Failed Tests 3 ⎯⎯⎯⎯⎯⎯⎯

 FAIL  server/quiz.test.ts > PUT matching cards by challenge > the k-th item with a challenge \
matches the k-th existing card
AssertionError: expected { id: 3, checklist_id: 1, …(10) } to match object \
{ id: 1, position: 1, …(4) }
(6 matching properties omitted from actual)

 FAIL  server/quiz.test.ts > POST /api/quiz/cards/:id/review > second success: interval 6
AssertionError: expected 13 to be 14 // Object.is equality

 FAIL  src/Quiz.test.tsx > Quiz > shows the first due card with aircraft, title and challenge
Error: Invalid Chai property: toHaveTextContent
";

    const VITEST_AFTER: &str = "\
 Test Files  1 failed | 7 passed (8)
      Tests  1 failed | 120 passed (121)

 FAIL  server/quiz.test.ts > POST /api/quiz/cards/:id/review > second success: interval 6
AssertionError: expected 13 to be 14 // Object.is equality
";

    #[test]
    fn vitest_failures_and_totals() {
        let run = parse(VITEST_BEFORE).unwrap();
        assert_eq!(
            names(&run),
            [
                "server/quiz.test.ts > POST /api/quiz/cards/:id/review > second success: \
                 interval 6",
                "server/quiz.test.ts > PUT matching cards by challenge > the k-th item with a \
                 challenge matches the k-th existing card",
                "src/Quiz.test.tsx > Quiz > shows the first due card with aircraft, title and \
                 challenge",
            ]
        );
        assert_eq!((run.summaries, run.total), (1, Some(121)));
        let after = parse(VITEST_AFTER).unwrap();
        let progress = progress(&run, &after).unwrap();
        assert_eq!(progress.fixed.len(), 2);
        assert_eq!(progress.still_failing.len(), 1);
    }

    #[test]
    fn vitest_passing_runs_and_files_that_fail_to_load() {
        let run = parse(" Test Files  6 passed (6)\n      Tests  87 passed (87)\n").unwrap();
        assert!(run.failing.is_empty());
        assert_eq!(run.total, Some(87));
        let broken = " FAIL  src/Quiz.test.tsx [ src/Quiz.test.tsx ]\nError: Failed to load url\n\
                      \n Test Files  1 failed | 5 passed (6)\n      Tests  80 passed (80)\n";
        assert_eq!(names(&parse(broken).unwrap()), ["src/Quiz.test.tsx"]);
        let unhandled = " Test Files  6 passed (6)\n      Tests  87 passed (87)\n\
                         ⎯⎯⎯⎯ Unhandled Errors ⎯⎯⎯⎯\nError: boom\n";
        assert_eq!(parse(unhandled), None, "an unhandled error isn't accounted for");
    }

    const JEST: &str = "\
 PASS  src/a.test.js
 FAIL  src/sum.test.js
  ● math › adds

    expect(received).toBe(expected) // Object.is equality

    Expected: 4
    Received: 5

      at Object.<anonymous> (src/sum.test.js:3:17)

  ● math › subtracts

    boom

Test Suites: 1 failed, 1 passed, 2 total
Tests:       2 failed, 3 passed, 5 total
Snapshots:   0 total
Time:        0.5 s
";

    #[test]
    fn jest_failures_and_totals() {
        let run = parse(JEST).unwrap();
        assert_eq!(names(&run), ["math › adds", "math › subtracts"]);
        assert_eq!((run.summaries, run.total), (1, Some(5)));
        let crashed = " FAIL  src/x.test.js\n  ● Test suite failed to run\n\n    SyntaxError\n\n\
                       Test Suites: 1 failed, 1 total\nTests:       0 total\n";
        assert_eq!(parse(crashed), None, "a suite that didn't run isn't accounted for");
    }

    /// `cargo test -q` with two binaries, as just's suite prints it.
    const CARGO_BEFORE: &str = "\
running 3 tests
..F
failures:

---- lexer::tests::escapes stdout ----
thread 'lexer::tests::escapes' panicked at src/lexer.rs:10:5:
    left: 1
error: something the test printed

failures:
    lexer::tests::escapes

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `--lib`

running 2 tests
FF
failures:

---- functions::trim_end_match stdout ----
boom

---- search::not_found stdout ----
denied

failures:
    functions::trim_end_match
    search::not_found

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";

    const CARGO_AFTER: &str = "\
running 3 tests
...
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

running 2 tests
.F
failures:

---- search::not_found stdout ----
denied

failures:
    search::not_found

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
";

    #[test]
    fn reads_failing_names_from_cargo_test_output() {
        let before = parse(CARGO_BEFORE).unwrap();
        assert_eq!(
            names(&before),
            ["functions::trim_end_match", "lexer::tests::escapes", "search::not_found"]
        );
        assert_eq!(before.summaries, 2);
        assert_eq!(before.total, Some(5));
    }

    #[test]
    fn an_indented_line_in_a_tests_own_output_is_not_a_name() {
        // `    left: 1` sits in the per-test output, not the name list.
        let run = parse(CARGO_BEFORE).unwrap();
        assert!(!run.failing.iter().any(|name| name.contains("left")));
    }

    #[test]
    fn verbose_cargo_output_names_failures_too() {
        let run = parse(
            "test a::b ... ok\ntest a::c ... FAILED\n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
        )
        .unwrap();
        assert_eq!(names(&run), ["a::c"]);
    }

    #[test]
    fn fixing_some_failures_and_breaking_none_is_progress() {
        // Live finding (2026-10-21, just bug hunt): the model fixed all
        // three planted bugs; only a test that fails inside the sandbox
        // was left, and the whole round was discarded.
        let before = parse(CARGO_BEFORE).unwrap();
        let after = parse(CARGO_AFTER).unwrap();
        let progress = progress(&before, &after).unwrap();
        assert_eq!(progress.fixed, ["functions::trim_end_match", "lexer::tests::escapes"]);
        assert_eq!(progress.still_failing, ["search::not_found"]);
    }

    #[test]
    fn a_new_failure_is_not_progress() {
        let before = parse(CARGO_BEFORE).unwrap();
        let after = parse(&CARGO_AFTER.replace(
            "    search::not_found\n",
            "    search::not_found\n    parser::fine_before\n",
        ))
        .unwrap();
        assert!(progress(&before, &after).is_none());
    }

    #[test]
    fn nothing_fixed_is_not_progress() {
        let before = parse(CARGO_BEFORE).unwrap();
        assert!(progress(&before, &before.clone()).is_none());
    }

    #[test]
    fn a_missing_test_binary_or_deleted_tests_are_not_progress() {
        let before = parse(CARGO_BEFORE).unwrap();
        // Only the second binary reported.
        let after = parse(CARGO_AFTER.split_once("\nrunning 2 tests").unwrap().0).unwrap();
        assert!(progress(&before, &after).is_none());
        // Same summaries, but a failing test was deleted rather than fixed.
        let mut fewer = parse(CARGO_AFTER).unwrap();
        fewer.total = Some(3);
        assert!(progress(&before, &fewer).is_none());
    }

    #[test]
    fn a_compile_error_or_crash_is_unreadable() {
        let compile_error =
            "error[E0425]: cannot find value `x`\nerror: could not compile `just`\n";
        assert!(parse(compile_error).is_none());
        let crashed = format!(
            "{CARGO_AFTER}error: test failed, to rerun pass `--test integration`\n\n\
             Caused by:\n  process didn't exit successfully: `target/debug/deps/x` \
             (signal: 11, SIGSEGV)\n"
        );
        assert!(parse(&crashed).is_none());
        // A test's own captured output saying `error:` is fine.
        assert!(parse(CARGO_BEFORE).is_some());
    }

    #[test]
    fn output_from_an_unknown_runner_is_unreadable() {
        assert!(parse("lint: 3 problems\n").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn reads_pytest_output() {
        let run = parse(
            "tests/test_a.py ..F\n\
             =========================== short test summary info ===========================\n\
             FAILED tests/test_a.py::test_scale - AssertionError: assert 2 == 3\n\
             ERROR tests/test_b.py::test_io\n\
             ==================== 1 failed, 2 passed, 1 error in 0.12s =====================\n",
        )
        .unwrap();
        assert_eq!(names(&run), ["tests/test_a.py::test_scale", "tests/test_b.py::test_io"]);
        assert_eq!(run.total, Some(4));
    }

    #[test]
    fn reads_go_test_output() {
        let run = parse(
            "--- FAIL: TestScale (0.00s)\n    scale_test.go:9: got 2\nFAIL\n\
             FAIL\texample.com/recipe\t0.01s\n\
             ok  \texample.com/recipe/units\t0.01s\n",
        )
        .unwrap();
        assert_eq!(names(&run), ["TestScale"]);
        assert_eq!(run.summaries, 2);
        assert!(parse("FAIL\texample.com/recipe [build failed]\n").is_none());
    }
}
