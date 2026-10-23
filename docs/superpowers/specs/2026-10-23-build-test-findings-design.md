# Build-test findings: fixed inputs, warnings, and facts at the end of a run

Date: 2026-10-23. Status: approved.

## Why

Three from-scratch build tests found failures that coder can address: fltplan
(2026-10-22, Qwen3.5-9B and Qwen3.8-27B), its self-audit, and vaops (2026-10-23,
the 27B in five staged milestones). The same three things went wrong more
than once:

1. **The model changed fixed inputs to make its own wrong tests pass.** The 9B
   rewrote a test's expected ground speed, which came from the brief, to match
   a sign error in its code. The 27B decided "tests are the spec here",
   replaced an airport in `data/airports.csv` with an invented one, and raised
   an aircraft's range. Nothing stops this today. `deny_paths` blocks reads
   too, and the system prompt says nothing about specs or tests.
2. **The final answer claimed things that weren't true.** It said missing
   fields were `null` (twice) and that clippy was clean when clippy had
   printed three warnings. The goal-achieved notice repeats only the
   iteration count and whether verification passed.
3. **A clippy run with warnings reads as plain success.** `run_command`
   results say `exit status: 0 (success)` whether or not there were warnings.

The display backlog and checkpoint bloat from the same tests are already
fixed on `main` and ship in the same release.

## 1. Fixed inputs

**Config.** `[permissions] read_only_paths: Vec<String>`, empty by default.
Each entry is resolved like this:

- `~` is expanded;
- a relative entry with no glob character is joined to the project root
  (`BRIEF.md` is `<root>/BRIEF.md`, and `data` covers everything under
  `<root>/data`);
- an entry with a glob character (`*`, `?`, `[`) is a file-name pattern, the
  same as a bare `deny_paths` pattern.

**Gate.** `ConfirmationGate::with_read_only_paths(Vec<PathBuf>)`. A `Write`,
`Delete` or `Move` whose target, or either end of a move, matches is denied
in every mode, at the same tier as `deny_paths`. Reads are unaffected. The
denial tells the model why and what to do: the file is a fixed input of the
task; if a test disagrees with it, fix the test or the code. The main gate
and every specialist's scoped gate get the list; `delegate_task` sub-agents
already share the main gate.

**Limit.** Commands are not covered: an approved `run_command` or `run_shell`
can still write these files, because Landlock grants write on the whole
project. The docs say so. A Landlock-level carve-out belongs in
`aivyx-confine` and is later work.

**Prompt.** A rule shared by the main system prompt (interactive and
autonomous), the `delegate_task` sub-agent prompt, and specialist prompts:

> The task's spec (a brief, requirements, a README you were pointed to) and
> the data it gives you are fixed inputs: don't edit them unless the user
> asks you to. When a test disagrees with them, the test or the code is
> wrong. Fix that, and never change an expected value that came from the
> spec to match what your code returns.

## 2. Warnings in command results

When a command exits 0 and its output has compiler or linter warnings, the
status line becomes `exit status: 0 (success), N warnings`. The `(success)`
text is kept, so every existing success check still holds. A warning is a
line that starts with `warning:` (rustc, cargo, clippy) or contains
`: warning:` (gcc, clang, `file:line: warning:`). Cargo's own summary lines
(`warning: \`x\` (lib) generated 3 warnings`, and `... (run \`cargo clippy
--fix ...\`)` variants) are not counted. A failed command's line is unchanged.

## 3. Facts at the end of an autonomous run

**Final checks.** `[verification] final_checks: Vec<String>` holds names of
`[[permissions.allowed_commands]]` entries, empty by default. When an
autonomous run reaches its goal, coder runs each one itself, confined like
any command, and shows it in the transcript as an aivyx-coder call (not the
model's). It is not added to the conversation. Each result records whether
it passed and how many warnings it printed. An unknown name is a startup
error, like `[verification] command`.

**Changed files.** The driver snapshots the worktree when the run starts, the
way `/undo` does. When the run stops, for any reason, coder diffs that
snapshot against the worktree, leaving out generated files as the per-turn
change summary does. Files matching `read_only_paths` are marked.

**Notice.** The stop notice gains fact lines after its first line, for
example:

```
autonomous run stopped: goal achieved after 5 iteration(s)
  verification: `test` passed
  final check `clippy`: passed, 3 warnings
  changed: 14 files (+2310 −12) — src/main.rs, src/db.rs, … and 9 more
  read-only files changed: data/airports.csv
```

There is no fact line without a snapshot (not a git repository) or without
final checks.

**Report.** `--report <path>` (with `--auto` only) writes the facts as JSON
when the run stops: `goal`, `outcome` (`goal_achieved`, `budget_exhausted`,
`cancelled`, `failed`, `injection_detected`), `notice`, `iterations`,
`duration_secs`, `last_verification_passed`, `final_checks` (`name`,
`passed`, `warnings`), and `changed_files` (`path`, `status`, `added`,
`removed`, `read_only`). The file is written with mode 0600. A write failure
is a transcript notice, not a crash.

## Testing

- Gate: read-only paths deny write, delete and move (either end); allow
  reads; apply in plan, interactive and autonomous modes; glob and prefix
  forms both match.
- Config: resolution of relative, glob and `~` entries.
- Specialist scoped gate: honours the list.
- Prompts: the main, sub-agent and specialist prompts contain the rule.
- `format_output`: warning counts for rustc, clippy and gcc output; cargo's
  summary lines are not counted; no suffix on failure or with no warnings;
  `command_reported_success` still holds.
- Facts: changed files between two real snapshots, with generated files left
  out and read-only files marked; notice lines; final check outcome parsing;
  report JSON shape and 0600 mode.
- Live: rerun vaops milestone 1 with `read_only_paths = ["BRIEF.md", "data"]`
  and `final_checks = ["clippy"]`.

## Out of scope

- Landlock enforcement of read-only paths (`aivyx-confine`).
- An end-of-run review turn against the spec (a separate, measured
  experiment).
- Path sanity for container paths missing their leading `/`.
