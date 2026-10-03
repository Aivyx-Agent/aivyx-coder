# Audit 3 Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the seven findings from the 2026-10-04 live audit of aivyx-coder v0.4.0.

**Source:** live TUI, ACP and `--auto` runs of the released binary against Lemonade (Qwen3.5-9B) on a small Python
project with a failing test, in an isolated HOME.

## Global Constraints

- Repo `/home/julian/Projects/Rust/aivyx-coder`, branch `fix/audit-3`. Read `CLAUDE.md` and README's "Security model"
  first.
- TDD: show a failing test (RED) before the fix (GREEN) for every behaviour change.
- Commits: `git commit -s`, message ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Verify:
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `CARGO_TARGET_DIR=target/rust199 ~/.cargo/bin/cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`
  - Never build into `/tmp`.
- Comments describe present behaviour and its reason. Never cite reviews, audits or "fix rounds".
- Exact user-facing strings are given below. Use them verbatim.

---

### Task 1: The agent core (aivyx-core)

**Files:** `crates/aivyx-core/src/agent/mod.rs`, `agent/undo_commands.rs`, `agent/change_commands.rs`,
`agent/test_command.rs`, `crates/aivyx-core/src/undo.rs`, tests in `agent/tests.rs` and `undo.rs`.

1. **A model reply that is only reasoning.** In the streaming loop (`run_turn_inner`, around the
   `StreamEvent::ReasoningDelta` arm and the `tool_calls.is_empty()` branch):
   - Accumulate the reasoning text of the current response in a local `String`. Cap it at the same
     `MAX_ASSISTANT_TEXT_BYTES`, and stop appending past the cap without failing.
   - **Reasoning only.** The response has empty `assistant_text`, no tool calls and no malformed blocks, but
     non-empty reasoning. Then push an assistant `Message` whose content is the trimmed reasoning as a
     `ContentBlock::Text`, so the history ends with an answer rather than a tool result. Then emit
     `AgentEvent::Info("(The model answered only in its thinking, shown above.)")`.
   - **Nothing at all.** If reasoning is also empty, emit
     `AgentEvent::Info("The model ended its turn without replying.")` and push nothing.
   - **Verification.** In both cases, enforced verification keeps working exactly as today.
   - **Tests:**
     - a mock stream with only `ReasoningDelta("Fixed it.")` and no text: the history's last message is an
       assistant `Text("Fixed it.")`, and that Info is emitted;
     - a stream with no events at all: the "without replying" Info, and no assistant message.
2. **The model knows the test command.** In `system_prompt_text`, when `self.tests` is `Some(t)`, plan mode is
   off and `self.verification` is `None` (verification has its own prompt), append:
   `"This project's tests run with `{t.display()}`. After changing code, run them (with run_shell) before telling the user it works."`
   Test: the assembled system prompt contains that sentence when tests are set, and omits it in plan mode.
3. **The last `/test` result, and the `/commit` warning.**
   - Store `last_test_passed: Option<bool>` on the agent:
     - `run_tests` sets it to `Some(true)` or `Some(false)`, based on pass versus failed or timed out;
     - cancelled or couldn't-start leave it unchanged;
     - `clear_conversation` and `switch_to` reset it to `None`.
   - In `/commit`'s confirmation preview (the text shown in the commit modal), when it is `Some(false)`, add the
     line `"⚠ The last /test failed."` as the first line of the preview body.
   - Test: `/test` fails, then `/commit`. The scripted prompter sees a preview containing that line.
4. **Python bytecode from test runs.**
   - `run_tests` sets the environment variable `PYTHONDONTWRITEBYTECODE=1` on the command it spawns.
   - Test: a `/test` script `sh -c 'echo "$PYTHONDONTWRITEBYTECODE"'` prints `1`.
5. **Normal results use `info`, not `notify`.** `notify` renders as a red `!` error. Switch these to `self.info(…)`:
   - in undo_commands.rs:
     - `Undone: …`, `Redone: …`, `Undo cancelled.`, `Redo cancelled.`
     - `Nothing to undo there — …`, `Nothing to redo — …`, `Nothing to redo.`, `NOTHING_TO_UNDO`
     - the `/checkpoints` listing (`checkpoints_listing`)
   - in change_commands.rs: the final commit notice at ~539 when it reports success (`Committed …`), and the
     `/commit` "Commit cancelled — nothing was committed." path if it uses `notify`.

   Real failures ("Couldn't …", "isn't available here", "Not a git repository", `NO_CHECKPOINTS`, `TOO_OLD`, usage
   hints) stay on `notify`. Update the tests that collect these messages: they move from `notices()` to `infos()`.
6. **`/checkpoints` when a redo is waiting.** With no marks but `redo` non-empty, the listing is
   `"Nothing to undo — /redo puts back what the last /undo removed."`, not the "no changes made in this session"
   text. Test it.

### Task 2: Verification runs and the commit dialog

**Files:** `crates/aivyx-tools/src/process.rs` (`CommandSpec`), `tools/run_command.rs`,
`crates/aivyx-core/src/agent/change_commands.rs`, `crates/aivyx/src/agent_builder.rs`, and every `CommandSpec { … }`
construction site the compiler reports.

1. **Environment on `CommandSpec`.**
   - Add `pub env: Vec<(String, String)>` to `CommandSpec`. Configured entries use an empty `env`.
   - `run_command`'s `execute`, and the scoped-verification runner if it builds its own command, apply it with
     `.envs(...)`.
   - The synthetic `detected-tests` entry (`auto_verification` in `agent_builder.rs`) gets
     `env: vec![("PYTHONDONTWRITEBYTECODE".into(), "1".into())]`.
   - Test: a `run_command` with a spec env `FOO=bar` and a program `sh -c 'echo $FOO'` outputs `bar`.
   - Test: `auto_verification` gives the synthetic entry that env.
2. **Binary files in the `/commit` dialog.** In the list of files to commit shown in `/commit`'s preview, mark binary
   files with `   (binary)`, using `git diff --numstat`'s `-\t-` convention, which the change summary already uses for
   `(binary)`. Mark files that are untracked (new) and binary with `   (binary, new)`. Test with a repo that has an
   untracked `x.pyc` holding NUL bytes.

### Task 3: TUI and CLI text

**Files:** `crates/aivyx-tui/src/app.rs`, `crates/aivyx/src/main.rs`.

1. **`--auto` mode.**
   - `run()` knows `autonomous.is_some()`. Store `autonomous: bool` on `App`.
   - When it's true, the welcome hint (`first_message_hint`) is replaced by:
     - `"Autonomous run — edits and pre-approved commands are approved automatically."`
     - `"Ctrl+C stops the run."`
     - the tests line, unchanged.
   - The status line's idle text `"ready — Enter to send, Ctrl+C to quit"` becomes `"autonomous run — Ctrl+C to stop"`
     while autonomous.
   - Tests for both.
2. **The resumed-session line** in `App::new`, `resumed previous session (N messages restored)`, becomes a
   `ChatLine::Info` (dim), not a `Notice`. Update its test.
3. **The welcome example** no longer names a file:
   `"  \"the tests fail — find out why and fix it\""`.
4. **`--mcp-server` help text** in main.rs. The doc comment was garbled by an edit; it must end like this:
   `"Requires [mcp_server].max_access_level to be configured in config.toml first -- refuses to start otherwise. Mutually exclusive with --acp/--plan/--auto/--resume: this frontend has no human to show a modal to, no editor session to embed in, and no unattended-goal concept of its own (each MCP call is its own bounded, isolated session)"`
   Remove the "matching --auto's own posture … test command …" fragment. Check `--help` output in a test if one
   exists for help text; otherwise just verify by running `cargo run -p aivyx -- --help`.

### Task 4: ACP slash commands

**Files:** `crates/aivyx-acp/src/session.rs`, `translate.rs` (or a new small `commands.rs`), and tests.

1. **Advertise the commands.**
   - After `session/new` succeeds, send a `SessionUpdate::AvailableCommandsUpdate` listing every
     `aivyx_core::commands::COMMANDS` entry an ACP user can use:
     - the `CommandTier::AgentState` entries except `/resume` (ACP refuses it);
     - plus `/help` and `/clear`, which ACP now handles (below).
     - `/quit` is excluded.
   - Each `AvailableCommand` has name = the command without its leading `/`, and description = `CommandInfo.description`.
   - Look at how the schema crate (`agent-client-protocol-schema` 1.5, `v1::AvailableCommand`, builder methods)
     constructs it, and how `session.rs` sends other updates.
   - Test: the pure function that builds the list includes `undo`, `diff`, `test`, `sessions`, `help` and `clear`,
     and excludes `resume` and `quit`.
2. **Handle `/help` and `/clear` in ACP** before `run_turn`, where `session/prompt` gets the text:
   - `/help` sends back an agent message with the same help text the TUI shows, minus the TUI-only "Keys:"
     section. Build it from `COMMANDS` the same way, filtered to the advertised set. The model is never called.
   - `/clear` calls `agent.clear_conversation()` and replies `"New conversation — the previous one is in /sessions."`.
   - Both end the prompt with `StopReason::EndTurn`.
   - Tests at the session level if a harness exists (look at session.rs's tests). Otherwise test the pure
     dispatcher (`fn acp_local_command(text) -> Option<LocalCommand>`) plus a unit test of the help text.
