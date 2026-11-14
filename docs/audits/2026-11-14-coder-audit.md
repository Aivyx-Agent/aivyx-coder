# aivyx-coder audit, 2026-11-14

A pre-1.0 review of aivyx-coder at `c4917f7`: v0.8.2, plus today's setup and
`/wiki` fixes. This report states and proves the findings. It fixes none of
them; the fixes are separate work.

## Scope

- **New code.** All 116 commits since the 2026-10-28 code audit were read.
  Most of them are that audit's fixes and the 2026-10-31 ACP audit's fixes.
  The new code that no audit has covered got depth:
  - pack verification commands and their consent (87ecb0b, 223fb7f, 8436579);
  - `final_check_retries` (5b16e26);
  - the vision tools: 3D, upscale and inpaint, and the agent seeing its own
    images (93bab46, 80b1ad3, c84a3e2, d527e1f, 374b83e);
  - the macOS build without a sandbox (717b20c, 19dfdd6);
  - setup: Docker Model Runner detection, minimal config writes, and today's
    tool-call probe.
- **Health.** `cargo audit` and `cargo deny`, the workspace tests and clippy.
- **Live, on Qwen3.5-9B** (llama-server on `:8090`, separate XDG folders):
  - headless `--auto` with verification and a final check;
  - ACP (initialize, session/new, a prompt);
  - the MCP server, including its access ceiling;
  - the setup wizard driven through a pty, against Docker Model Runner's
    `smollm2:135M` and the 9B.
- **Not repeated.** The TUI (audited 2026-11-04, every item fixed) and the
  shared crates (audited 2026-10-28) were not re-audited. The documented
  limits in `docs/manual/guide/15-troubleshooting.md` and earlier decisions
  are not re-reported.

## Method

Ranks follow the 2026-10-28 audit:

- **Critical:** an attacker or the model controls something it must not.
- **High:** the same with a precondition, or data loss.
- **Medium:** a real defect a user will hit, or a hardening gap with a
  plausible path.
- **Low:** everything else worth noting.

A Medium or worse finding counts only when shown real: either by a failing
test on the local branch `audit/2026-11-14` (447ff21, never pushed) or by a
recorded live run.

## Already fixed today (found from the user's own install, 2026-11-13)

| | Finding | Fix |
|---|---|---|
| F1 | Setup accepted a model that can't call tools: Docker Model Runner's `smollm2:135M` got "ok", then produced nonsense | e5675a8: a tool-call probe with a "Use this model anyway?" question; a warning for a window under 16,384 tokens |
| F2 | `/wiki` documented a fixed list of aivyx-coder's own files in every project | c4917f7: the coverage comes from the project; a folder with nothing to document says so |

## Findings

### E1 (High): a failed command that prints `(success)` counts as passed

`command_reported_success` (`crates/aivyx-core/src/agent/mod.rs:4405`)
returns true when the output contains both `exit status:` and `(success)`
anywhere. Every command result begins with the footer
`exit status: N (verdict)`, so the first test always holds. A command that
fails, but whose own output contains `(success)`, therefore counts as passed.

The check decides three things:

- automatic verification after edits (`agent/mod.rs:2476`, where it settles
  `last_verification_passed`);
- each `[verification] final_checks` result, and with it the retry loop;
- headless's exit code (`crates/aivyx/src/headless.rs:37`).

The model writes the project's code and tests, so it controls what that
output contains. It can therefore pass a gate that exists to catch it, and a
CI job or the bench sees exit 0. The same thing can happen by accident: any
test or tool that prints "(success)" will do it.

**Proof.**

- Unit test `a_failing_command_that_prints_success_is_not_a_pass` fails on
  `audit/2026-11-14`.
- Live headless run on the 9B: the final check `fakecheck`
  (`sh -c "echo 'lint step (success)'; exit 1"`) is logged as
  `exit status: 1 (failed)`. The summary still says
  "final check `fakecheck`: passed", `--report` records `passed: true`, and
  the process exits 0.

**Fix:** read only the first line, as `run_facts::warnings_in` already does.
That means `output.lines().next()` starts with `exit status: ` and contains
` (success)`. Add a test that adds `(success)` to a failing run's stdout.

### M1 (Medium): a build without the sandbox says so only in the log

On macOS (and on a Linux `--no-default-features` build), `NoopConfiner` is
in effect: commands run unconfined. The only sign at runtime is a
`tracing::warn!` in `aivyx.log` (`crates/aivyx/src/agent_builder.rs:775`).
The transcript, the ACP client and headless stderr show nothing.

This matters most in `--auto`. Pre-approved commands such as `cargo test`
run without a prompt, edits inside the project are auto-allowed, and so the
model's own code runs as the user with no confinement at all. The manual
says macOS is "gate-only" (`02-install-and-first-run.md`, Platform support;
`05-security-model.md` §3), but nowhere spells out what that means for
`--auto`.

**Proof:** reading the code. No macOS machine was used. The gate path is
`confirmation.rs` (`PermissionTarget::Command` + `always_allow` →
`AllowAlways`) followed by `NoopConfiner`.

**Fix:**
- Show a one-time startup notice in every frontend when `is_sandboxed` is
  false.
- In `--auto`, print the same notice to stderr and record
  `"sandbox": false` in `--report`.
- Add one sentence on `--auto` to Platform support.

### Low

- **L1:** `cargo audit` exits non-zero on `quinn-udp` 0.5.15
  (RUSTSEC-2026-0538, patched in 0.5.16). It is a leftover entry in
  `Cargo.lock` that no feature set builds, so `cargo deny` is clean.
  `cargo update -p quinn-udp` removes the noise.
- **L2:** the `ROADMAP.md` header still says
  "Current release: v0.6.5 (2026-11-01)"; the current release is v0.8.2.
- **L3:** headless stderr prints "(The model answered only in its thinking,
  shown above.)", but headless never prints the thinking
  (`agent/mod.rs:3576`; seen in the live run). Word it per frontend, or say
  "(the reply was only in its thinking)".
- **L4:** `09-local-model-servers.md` still marks Docker Model Runner's tool
  calling as "unverified". It's now known that the default `smollm2:135M`
  can't call tools, while a tool-capable model served by Docker Model Runner
  goes through the same probe. Point readers at setup's check.
- **L5 (setup):** detection tries only the default ports (11434, 13305,
  12434, 8080). A llama-server on any other port is found only through
  "A running OpenAI-compatible server" with the URL typed in. The menu could
  say that in that line ("… or any other port").

## Checked and sound

- **Pack verification commands.**
  - Approval is an exact match, and a changed set of commands is refused
    until approved again (`packs.rs` `resolve_layer`).
  - Shell syntax is refused.
  - Without a terminal the answer is no.
  - The consent text says the model may run them too.
  - A name clash with the user's own entry is shown.
- **Final checks** run through `ToolExecutor::dispatch`, so they get the
  gate, the confiner and checkpoints.
- **Vision inputs** (reference, mask, upscale, 3D) are checked against
  `deny_paths` before they're read. That's the same rule as `read_file`.
  Previews are capped at `MAX_TOOL_IMAGES` (4).
- **`--auto` gate.**
  - MCP, memory and REPL tools are denied.
  - Writes are limited to the worktree.
  - Commands must be pre-approved.
  - Injection taint stops writes, commands and network use.
- **ACP:** initialize answers with the protocol version and agent info;
  session/new and a prompt work live.
- **MCP server:** a call above `max_access_level` is refused (-32602, live).
- **Setup:** it writes the config `0600`, keeps the old one as `.bak`, and
  the new probe sends nothing but a `read_file` tool offer.
- **Health:**
  - workspace tests: 0 failures;
  - clippy `-D warnings`: clean;
  - `cargo deny check advisories`: ok (the accepted zip/mistralrs entries
    have reasons).
