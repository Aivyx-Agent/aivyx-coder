# Tests found per project, /test — Design

**Date:** 2026-10-02
**Status:** Approved in brainstorming; awaiting implementation plan
**Repo:** aivyx-coder (`crates/aivyx-core`, `crates/aivyx-tui`, `crates/aivyx`)
**Part 3 of 4.** Independent of parts 1 and 2.

## Why

Verification (`[verification].command`) gives small models a deterministic keep-or-discard signal, and
`--auto` refuses to start without it. But nobody sets it: the config is global
(`~/.config/aivyx-coder/config.toml`) while test commands are per project. Running the tests by hand from
inside the app also has no shortcut.

## What the user gets

- **Detection.** Each time aivyx-coder starts, it inspects the project's top-level folder and detects its test
  command.
  - **Welcome text and `/help`:** "Tests: `cargo test` (detected from Cargo.toml) — run them with /test".
  - **Nothing found:** "No test command found — set [verification] command in config.toml".
  - **Configured command present:** "Tests: `<command>` (from config.toml)".
- **`/test`** runs the effective test command, as described below.
- **`--auto`** uses the detected command when `[verification].command` isn't set, and says so up front:
  "Verifying with `cargo test` (detected from Cargo.toml)". When nothing is configured or detected it refuses:
  "--auto needs a test command: set [verification] command in config.toml, or run it in a project with a
  recognised test setup."
- **Interactive sessions don't change.** Automatic after-edit verification still only happens when
  `[verification].command` is configured. Detection never turns it on by itself.

## Detection rules

Detection only looks at files directly in the project folder. The first match wins:

| # | Marker | Command | Reason text |
|---|---|---|---|
| 1 | `Cargo.toml` | `cargo test` | detected from Cargo.toml |
| 2 | `go.mod` | `go test ./...` | detected from go.mod |
| 3 | `package.json` with `scripts.test` that is not npm's placeholder (contains "no test specified") | `pnpm test` if `pnpm-lock.yaml`, `yarn test` if `yarn.lock`, else `npm test` | detected from package.json |
| 4 | `pytest.ini`, or `pyproject.toml` containing `[tool.pytest`, or `conftest.py` | `python3 -m pytest` | detected from <file> |
| 5 | any `test_*.py` or `*_test.py` file, or a `tests/` folder containing `.py` files | `python3 -m unittest` | detected from Python test files |
| 6 | `Makefile` with a line starting `test:` | `make test` | detected from Makefile |

A configured `[verification].command` always takes precedence over detection.

## /test

- **How it runs:**
  - through `sh -c <command>` in the project folder;
  - confined by the same `ExecutionConfiner` as `run_command`;
  - no approval prompt, because the user typed `/test` explicitly.
- **Stopping it:** Ctrl-C cancels; it times out after 10 minutes.
- **Display:** output streams into the transcript as a dimmed block, keeping the last 200 lines visible.
- **Result line:** `Tests passed (12.3 s)`, `Tests failed (exit 1, 12.3 s)`, `Tests cancelled` or
  `Tests timed out after 10 min`.
- **What the model sees:** the command, the result line and the last 80 lines of output are added as a note
  in the history, so "fix the failing test" works as the next message. The output is scanned with the same
  prompt-injection tripwire tool results use; follow the existing convention for tool output entering
  history.
- **Edge cases:**
  - No command: "No test command — set [verification] command in config.toml."
  - Busy agent: "Wait for the reply to finish (or press Ctrl+C) first."

## Design

- **Detection module.** New `crates/aivyx-core/src/test_detect.rs` provides
  `detect(dir: &Path) -> Option<DetectedTests { command: String, reason: String }>`. It is pure apart from
  reading at most four small files (`package.json`, `pyproject.toml`, `Makefile`, the directory listing),
  each capped at 256 KiB.
- **Effective command.** `EffectiveTests { command, source: Config | Detected(reason) }` is resolved once at
  startup in `crates/aivyx/src/main.rs` / `agent_builder.rs` and passed to the agent and the TUI.
- **Wiring.** `/test` is intercepted in `Agent::run_turn` like the other commands. It runs through the
  executor's confiner and emits streaming `AgentEvent::Notice` lines. Use a new `AgentEvent::TestOutput` only
  if notices can't stream.
- **`--auto`.** The startup check that requires `[verification].command` now accepts the effective command,
  and the autonomous verification uses it.

## Testing

- **Detection:** table-driven tests on temporary folders covering every row, npm's placeholder script, the
  lockfile choices, a Makefile without a `test:` target, `[tool.pytest` versus a plain `pyproject`, nothing
  found, and precedence order when several markers exist.
- **`/test`:** scripts that pass, fail (exit 1), and sleep (cancel and timeout with a short test timeout).
  Check the result line, that the history note is added, and that the busy refusal works.
- **`--auto` resolution:** config wins, detected is used when config is unset, and refusal happens with
  neither.

## Out of scope

- Running only the tests related to the changed files (`scoped_command` already exists for configured setups).
- Detecting tests in nested packages or monorepos.
- Persisting the detected command.
