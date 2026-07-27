# Verification Test-Selection — Design

**Status:** Approved by user 2026-07-28.

## Context

The last item in the 2026-07-22 capability audit's backlog (tracked in
`ROADMAP.md`, now empty once this closes): enforced verification always
re-runs the entire configured `[verification] command`. There's no
mechanism to scope a retry to just the tests relevant to the files touched
in that batch of edits, so the auto-fix-and-retry loop
(`Agent::run_auto_verification`, `crates/aivyx-core/src/agent/mod.rs:721-776`,
invoked from the retry logic in `run_turn_inner` around lines 1339-1367)
pays the full suite's cost on every retry even for a large test suite and
a small edit.

This project currently has **zero test-framework awareness** — `[verification]
command` is a name that must match an entry in
`[[permissions.allowed_commands]]` (`AllowedCommand { name, program, args,
timeout_secs }`, reusing that trust tier rather than free-form shell text),
and `run_command`/`run_shell` are otherwise fully generic. There is no
`cargo`/`pytest`/`npm test` detection anywhere in the codebase.

Four design questions were resolved with the user via one-at-a-time
questions before this doc was written — the fourth only surfaced after
tracing the permission gate's actual caching mechanics during design, not
during the initial round of questions:

1. **Safety net**: interim retries use a fast scoped command; one full,
   unscoped run is still required before the batch is finally declared
   verified. Preserves today's completeness guarantee (nothing ships
   without a full pass) while cutting the cost of the iteration loop,
   which is exactly where the backlog description says the cost is
   wasted.
2. **Scoping mechanism**: a user-configured command template with a
   placeholder for touched paths, not built-in framework detection. This
   project's own history has repeatedly found subtle, hard-to-verify
   assumptions in exactly this kind of framework/grammar-specific
   heuristic (e.g. the repo-map phase's `tree-sitter` field-name
   mismatches); pushing the file→test mapping to the user (who already
   knows their own project's test-runner conventions) avoids inventing
   fragile heuristics the agent would need to maintain.
3. **Placeholder shape**: the template's `{touched_paths}` placeholder
   expands into multiple separate argv entries (one per touched path),
   not a single joined string — matches how most multi-path-accepting
   test runners actually work (`pytest a.py b.py`), and there's no shell
   involved at any point so there's no quoting/injection surface either
   way.
4. **Gate interaction** (found during design, not anticipated in the
   original backlog framing): a scoped command's args change on every
   retry (different touched files each time). Routing it through
   `ConfirmationGate`'s existing Always-Allow cache — keyed on the
   **exact** `(program, args)` pair, by design (`crates/aivyx-sandbox/src/confirmation.rs`'s
   `PermissionKey::Command`, and this project's own stated invariant that
   "approving X never blesses Y") — would mean either a fresh prompt on
   every single retry in interactive mode, or an outright denial in
   autonomous mode (which never prompts for anything not already cached).
   Either would defeat the point of scoping at all. Resolved: the scoped
   run bypasses the gate for this one specific internal call, executing
   directly through the same sandboxed process primitive `run_command`
   itself uses, on the reasoning that the only dynamic input is file
   paths the model already had gated permission to edit via the normal
   edit-tool path — no new capability is granted to the model, only
   automation of an already-authorized action. The full command's
   dispatch path (through `ConfirmationGate`, cached by its own fixed,
   unchanging args) is completely unchanged.

### Pre-existing gap fixed as part of this plan (found during design, not the original backlog framing)

`unverified_edits` (`crates/aivyx-core/src/agent/mod.rs:1472`, the flag that
triggers enforced verification at all) currently only fires when
`edit_file` or `write_file` succeeds — it reuses
`PROMPTED_EDIT_HIDDEN_TOOLS` (`&["edit_file", "write_file"]`), a constant
whose actual purpose is unrelated: hiding the native tool-call forms of
those two tools while `EditFormat::Prompted` is active (since prompted
mode synthesizes SEARCH/REPLACE blocks into those same two calls). That
constant predates `patch_file`/`delete_file`/`move_file` and was never
widened when they shipped, so editing a file via any of those three
*never* triggers auto-verification today, even though they're the same
kind of content/structure mutation `edit_file`/`write_file` already
trigger it for. Confirmed with the user this is a real, pre-existing bug
worth fixing here rather than deferring, since this feature's own
touched-paths accumulator would otherwise be silently inert for 3 of the
5 mutating file tools.

**Fix, precisely**: do **not** widen `PROMPTED_EDIT_HIDDEN_TOOLS` itself —
that would incorrectly hide `patch_file`/`delete_file`/`move_file` from
the model whenever prompted edit mode is active, which is unrelated to
and unwanted alongside this fix (those three tools have nothing to do
with SEARCH/REPLACE block synthesis). Instead, a new, separate constant —
e.g. `VERIFICATION_TRIGGER_TOOLS: &[&str] = &["edit_file", "write_file",
"patch_file", "delete_file", "move_file"]` — is checked at the
`is_edit_call` site (`crates/aivyx-core/src/agent/mod.rs:1472`) instead of
reusing `PROMPTED_EDIT_HIDDEN_TOOLS`, decoupling the two concerns
(prompted-mode tool hiding vs. verification triggering) that constant was
incorrectly conflating.

## Decisions

### Config: a new optional `scoped_command` field

```toml
[verification]
command = "test"               # existing: full suite
scoped_command = "test_scoped" # new, optional
max_auto_verify_retries = 3
```

```toml
[[permissions.allowed_commands]]
name = "test_scoped"
program = "pytest"
args = ["{touched_paths}"]
```

**This example is deliberately `pytest`, not `cargo`** — verified
empirically before writing this doc, not assumed: `pytest` natively
accepts file paths as test-selection arguments (its core, stable
collection mechanism), but `cargo test <file-path-string>` does **not** —
cargo's positional filter matches against a test's fully-qualified *name*
(module-path style, e.g. `tools::patch_file::tests::execute_applies_a_patch`),
not a file path, so passing a raw file path filters out every test and
reports a trivial, silent "0 passed; 0 failed" success
(`cargo test <path>` against this very codebase confirms exactly this:
"test result: ok. 0 passed; 0 failed... 247 filtered out"). A cargo-based
`scoped_command` therefore needs a **user-authored wrapper script** that
translates a file path into an appropriate module-path filter — this
project's own eventual config (if it adopts this feature for itself)
would need one, e.g. a small script stripping `crates/*/src/` and `.rs`
and converting `/` to `::`. This is a real, honest limitation of the
generic-template-substitution approach for name-filtered (as opposed to
path-filtered) test runners, not a gap to paper over — it's the direct
consequence of the resolved "config-driven, no framework awareness"
design decision, and worth documenting plainly rather than presenting a
misleadingly simple one-size-fits-all example.

`aivyx_config::VerificationSettings` (`crates/aivyx-config/src/lib.rs:63-69`)
gains `scoped_command: Option<String>`, defaulting to `None` — fully
backward-compatible; when unset, behavior is byte-for-byte identical to
today. `crates/aivyx/src/agent_builder.rs`'s existing startup validation
(currently warns and disables verification if `command`'s name doesn't
match any `allowed_commands` entry, ~lines 401-406/522-536) gets a
parallel check for `scoped_command`: if configured but the name doesn't
resolve, warn and treat scoping as unavailable for this session
(verification itself stays enabled, falling back to always using the full
command — the existing, safe, pre-feature behavior) rather than disabling
verification outright.

`{touched_paths}` is matched as an **exact, whole-argv-entry token** — an
`args` entry that literally equals the string `"{touched_paths}"` is
replaced by N entries (one per touched path); other entries pass through
unchanged. Deliberately not partial-string interpolation (e.g.
`"--filter={touched_paths}"`) — a user needing that shape writes a small
wrapper script instead, consistent with pushing framework-specific
complexity to config rather than inventing a template-interpolation
mini-language in the agent. If `scoped_command`'s configured `args`
contain no such token at all, substitution is simply a no-op every time
(not specially validated at this pass — a possible future startup-warning
enhancement, not core to this feature).

### `agent_builder.rs`: resolving and handing off the scoped command

At startup, alongside the existing `command` validation,
`agent_builder.rs` resolves `scoped_command`'s name against the same
`command_specs: Vec<CommandSpec>` list already built there (used to
construct `RunCommandTool`), and — if found — passes both the resolved
`CommandSpec` (program, args-with-placeholder, timeout) and a clone of the
already-constructed `Arc<dyn ExecutionConfiner>` to `Agent::set_verification`
(extended to accept these as new optional parameters, or via a new
`Agent::set_scoped_verification(spec: CommandSpec, confiner: Arc<dyn
ExecutionConfiner>)` sibling method — implementation's choice, whichever
reads more clearly against the existing `set_verification`'s shape).
`Agent` needs its own confiner handle for this because `ToolExecutor`
(which already holds one, privately) has no accessor for it, and adding
one would be a more general-purpose surface change than this feature
needs — an explicit, constructor-style handoff matches how `PlanMode`/
`AutonomousMode`/`InjectionTaint` are already threaded into `Agent`.

### Touched-paths accumulator

A new `Agent` field, e.g. `verification_touched_paths: HashSet<PathBuf>`,
populated whenever `edit_file`, `write_file`, `patch_file`, `delete_file`,
or `move_file` (its destination path only — the source no longer exists
post-move) succeeds while `unverified_edits` is `true` (same trigger
condition `unverified_edits` itself already uses, around
`crates/aivyx-core/src/agent/mod.rs:1479-1480`). Accumulates across the
**whole** unverified-edits window, not per-retry-attempt — mirrors
`unverified_edits`'s/`verify_retries`'s own lifetime exactly, since a
later edit made in response to a failed scoped run might fix a bug the
*earlier* touched file's own tests would need to re-confirm, not just the
newest one. Cleared only when the window closes: a passing final full
verification, or exhaustion (both existing reset points for
`unverified_edits`/`verify_retries`, ~lines 1348-1349/1367/1377).

Extraction reuses the same `arguments.get("path")`-style pattern
`describe_tool_call_target` (`crates/aivyx-core/src/agent/mod.rs:1667-1678`)
already applies for its own (unrelated) display-string purpose — this
feature needs a clean, resolved path, not a display string, so it's a new
small helper alongside it rather than a change to that existing function's
return type (which the multi-file-edit-atomicity feature already depends
on unchanged).

Paths are stored resolved (absolute, matching this project's existing
internal path-tracking convention) but substituted into the scoped
command's argv **relative to `cwd`** — matching how a human would
actually write a scoped command's expected input (`pytest
tests/test_foo.py`, not `pytest /home/user/project/tests/test_foo.py`),
and consistent with how `glob.rs`'s own output already strips the `cwd`
prefix before displaying/using paths externally.

### The retry loop: one slot, not two

The existing loop body (one iteration = one retry attempt, gated by
`verify_retries < max_retries`, `crates/aivyx-core/src/agent/mod.rs:1339-1367`)
changes from "run the full command" to "run scoped-then-maybe-full":

1. If a scoped command is configured **and** `verification_touched_paths`
   is non-empty: run it (bypassing the gate, per Decision 4 above). If it
   fails, that's this iteration's result — same `continue`-the-loop
   handling as today, just against a cheaper command.
2. If the scoped run passed (or no scoped command/no touched paths are
   available, i.e. today's exact behavior): run the full command through
   the existing, unchanged, gate-checked dispatch path. Its pass/fail is
   this iteration's result.

Deliberately **one** `verify_retries` increment per iteration regardless
of whether that iteration ran one command or two — the loop already
treats "verification failed this attempt" as the unit of retry budget,
and a scoped-pass-then-full-fail is still exactly one failed attempt from
the model's perspective (it sees one failing result to react to, gets one
more turn). This needed no new counter or double-counting logic, just
extending what happens inside the existing iteration body.

### Per-kind output comparison

`last_verification_output: Option<String>` (`crates/aivyx-core/src/agent/mod.rs:229`)
becomes `Option<(VerificationKind, String)>` where `VerificationKind` is a
new small `enum { Full, Scoped }`. `new_lines_note`
(`crates/aivyx-core/src/agent/mod.rs:1738-1755`) is only invoked when the
new attempt's kind matches the stored kind — comparing a scoped run's
(small, targeted) output against a full run's (large, comprehensive) one
would produce a "what's new" note dominated by irrelevant noise from
tests that were never re-run, not a genuine regression signal. When kinds
differ, the note is simply skipped for that one attempt (same as the
existing `None`-previous-output case), and the new `(kind, text)` pair
always overwrites the stored one regardless, exactly like today's
unconditional overwrite.

## Out of scope for this spec

- Any built-in test-framework/language detection (per the resolved
  scoping-mechanism question above).
- Partial-string placeholder interpolation (`--filter={touched_paths}`) —
  exact-whole-token match only.
- Validating that a configured `scoped_command`'s args actually contain
  the `{touched_paths}` token — a silent no-op-substitution
  misconfiguration is left as a possible future startup-warning
  enhancement, not core to this feature.
- Extending the touched-paths concept to non-file-mutating tools (e.g.
  `run_shell`/`git_commit`) — only the five file-content/structure tools
  listed above contribute, matching the existing `unverified_edits`
  trigger set.
- **Sub-agents (`delegate_task`)**: `DelegateTaskConfig` already threads a
  `verification: Option<(String, u32)>` tuple through to give a sub-agent
  its own `Agent::set_verification` call, mirroring the parent. This plan
  does **not** extend that plumbing to also carry `scoped_command`/the
  confiner handle — sub-agents keep using only the full command,
  unchanged from today. `Agent::set_verification`'s existing signature is
  not touched; scoped verification is wired in via a new, separate,
  additive method the parent agent's own construction calls, so every
  existing call site (including the sub-agent one) keeps compiling and
  behaving exactly as it does today without modification. Extending
  sub-agent support is a reasonable future increment, not a requirement
  of closing this backlog item.

## Testing / verification

Unit tests (`crates/aivyx-core/src/agent/`):

- The touched-paths accumulator collects paths from `edit_file`/
  `write_file`/`patch_file`/`delete_file`/`move_file` across multiple
  failed retry iterations without resetting between them, and resets
  fully on both a passing final verification and on exhaustion.
- `{touched_paths}` substitution: exact-token replacement into N argv
  entries; an args list with no such token is left unchanged; an empty
  touched-paths set correctly falls back to running the full command
  instead of a scoped command with no paths to scope to.
- A scoped-pass-then-full-fail iteration reports failure (not success)
  and re-enters the retry loop with `verify_retries` incremented exactly
  once, not twice.
- A scoped-fail iteration does not also run the full command that
  iteration (proving the cost savings are real, not just theoretical).
- `last_verification_output`'s per-kind comparison never produces a
  "what's new" note comparing a scoped attempt's output against a full
  attempt's, or vice versa.
- The scoped-run bypass path applies the same `ExecutionConfiner`
  confinement and the `CommandSpec`'s own configured timeout, matching
  `RunCommandTool::execute`'s behavior for the equivalent unscoped case.
- Config: `scoped_command` referencing a nonexistent `allowed_commands`
  name warns and leaves verification enabled with scoping unavailable
  (falls back to always-full), rather than disabling verification
  entirely.

**Live E2E verification** (manual follow-up after implementation, matching
how every other feature in this project has been verified — see
`docs/HISTORY.md`): a real fix-and-retry cycle on the bare-metal rig where
the scoped rerun is visibly faster than the full one would have been, and
a deliberately-planted regression in a file *outside* the touched-paths
set is still caught by the mandatory final full run (proving the safety
net actually holds live, not just in unit tests).

## Documentation

`README.md`'s existing verification/enforced-verification section gets a
new subsection or paragraph documenting `scoped_command`, the
`{touched_paths}` placeholder and its exact-token-substitution semantics,
the `pytest`-native worked example shown above alongside the honest
`cargo`-needs-a-wrapper-script caveat, and an explicit note that the
scoped run bypasses the normal per-command approval caching (Decision 4)
— since this is a real, if narrow, and deliberately-scoped exception to
the project's "the model never influences a run_command invocation's
arguments" invariant, worth being honest about in the same place
`README.md` already documents this project's other known trust-model
exceptions (e.g. `editor_approval`'s default-on posture).
