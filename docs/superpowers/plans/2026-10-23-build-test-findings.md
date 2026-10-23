# Build-test findings Implementation Plan

> **For agentic workers:** executed inline in the session that wrote it
> (superpowers:executing-plans). Steps use checkbox (`- [ ]`) syntax.

**Goal:** implement `docs/superpowers/specs/2026-10-23-build-test-findings-design.md`:
read-only fixed inputs, warning counts in command results, and facts
(final checks, changed files, `--report`) at the end of an autonomous run.

**Architecture:** the gate gains a read-only tier beside `deny_paths`; a
shared prompt rule lives in `aivyx-core`; `format_output` counts warnings; a
new pure module `aivyx-core::run_facts` holds the facts types, notice lines
and report JSON, fed by new `Agent` methods and used by the TUI's autonomous
driver.

**Tech stack:** Rust workspace; tests with real git fixtures
(`aivyx_checkpoint::test_support`).

## Global constraints

- Never `cargo fmt`; lines ≤ 100 characters by hand; match surrounding style.
- Commits use `-s` and the Co-Authored-By line, on `feat/build-test-findings`.
- Regenerate `docs/manual/reference/03-configuration.md` after config
  changes; a new CLI flag needs a section in `01-command-line.md`.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets` clean
  at the end.

---

### Task 1: Warning counts in command results

**Files:** `crates/aivyx-tools/src/process.rs` (`format_output`, new
`count_warnings`).

- [ ] Tests: rustc/clippy output with 3 warnings → `(success), 3 warnings`;
      gcc `a.c:1:2: warning:` counted; cargo's `generated N warnings` summary
      not counted; failure → unchanged; no warnings → unchanged; the result
      still satisfies `command_reported_success`.
- [ ] Implement, run, commit.

### Task 2: Read-only paths

**Files:** `crates/aivyx-config/src/lib.rs` (`PermissionSettings::read_only_paths`,
`resolved_read_only_paths(root)`), `crates/aivyx-sandbox/src/confirmation.rs`
(`with_read_only_paths`, denial text, check tier),
`crates/aivyx-core/src/specialist_enforcement.rs` (ingredient field),
`crates/aivyx/src/agent_builder.rs` (wiring), plus every
`SpecialistEnforcementIngredients { .. }` literal.

- [ ] Tests: config resolution (relative, glob, `~`); the gate denies
      write/delete/move-either-end, allows read, applies in every mode; the
      specialist gate honours the list.
- [ ] Implement, run, commit.

### Task 3: The fixed-inputs rule

**Files:** `crates/aivyx-core/src/lib.rs` (`pub const FIXED_INPUTS_RULE`),
`crates/aivyx/src/main.rs` (`build_system_prompt`),
`crates/aivyx-core/src/delegate.rs` (sub-agent prompt),
`crates/aivyx-core/src/delegate_to_specialist.rs` (persona + rule).

- [ ] Tests: each prompt contains the rule (update the specialist-persona
      test).
- [ ] Implement, run, commit.

### Task 4: Facts at the end of a run

**Files:** new `crates/aivyx-core/src/run_facts.rs` (`RunFacts`,
`FinalCheck`, `ChangedFile`, `notice_lines`, `report_json`);
`crates/aivyx-core/src/agent/mod.rs` (`mark_run_start`, `run_final_checks`,
`changed_since_run_start`); `crates/aivyx-config/src/lib.rs`
(`VerificationSettings::final_checks`, startup validation);
`crates/aivyx-tui/src/app.rs` (driver: snapshot at start, facts at every
stop, report file); `crates/aivyx/src/main.rs` (`--report`, `AutonomousRun`
fields).

- [ ] Tests: notice lines and report JSON (pure); changed files between real
      snapshots, generated files left out, read-only marked; final-check
      parsing (pass, warnings); report written 0600; CLI `--report` needs
      `--auto`.
- [ ] Implement, run, commit.

### Task 5: Docs and the whole suite

- [ ] Security model (read-only tier and its command limit), autonomous
      guide (final checks, report, the notice), `config-example.toml`,
      regenerated configuration reference, `01-command-line.md` (`--report`).
- [ ] `cargo test --workspace`, `cargo clippy --workspace --all-targets`;
      commit.

### Task 6: Live check

- [ ] Build `aivyx-coder:main`-style image from the branch; rerun vaops
      milestone 1 (fresh seed) with `read_only_paths = ["BRIEF.md", "data"]`
      and `final_checks = ["clippy"]` and `--report`; check the data files are
      untouched, the notice's facts, and the report.
