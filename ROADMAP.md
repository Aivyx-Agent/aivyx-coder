# aivyx-coder Roadmap

_Current release: v0.6.5 (2026-11-01)._

A terminal (TUI) coding agent for local LLMs only (Ollama, vLLM, or
llama.cpp) — see `README.md` for what it does and how to run it. This
file is the current-status summary and the open backlog. The log of shipped
and live-verified work (everything that used to fill "Current status") is in
[`docs/history/ROADMAP-to-2026-10-28.md`](docs/history/ROADMAP-to-2026-10-28.md);
the full phase-by-phase history, every design decision, and the evidence
behind it lives in `docs/HISTORY.md`. Every phase's original design spec and
implementation plan is tracked in `docs/superpowers/specs/` and
`docs/superpowers/plans/`, for the curious.

## Current status

All planned phases and the capability-gap, Nonagon team, ACP, MCP-server,
packs and routing work are shipped and live-verified; see the archived log
for each item,. The
audits of 2026-10-28 and 2026-10-31 fixed every High and Medium finding;
what they left open is in the last two backlog sections below.

every item above.

## Backlog — capability opportunities, not yet scheduled

A follow-up audit (2026-07-22) covering security posture, tool coverage,
test quality, and documentation closed 10 findings directly (argument-blind
MCP Always-Allow cache, sub-agent injection-taint isolation, thin
`deny_paths` defaults, five documentation-accuracy fixes, an untested
injection-scan tie-break rule, unlabeled web_fetch/web_search injection
sources, and Plan mode not surviving `--resume`). The remaining finding,
verification test-selection, needed a real design pass of its own (tool-trait
shape, permission/`ActionKind` wiring, config surface) rather than a
same-session patch; it shipped as `[verification] scoped_command` (see
"Verification test-selection" above). With that done, this audit's
backlog is now fully resolved.

A fresh audit (2026-07-28) covering the same four dimensions against the
current codebase found two documentation-accuracy gaps, fixed directly
(the autonomous-mode paragraph was missing `repl_start`/MCP-tool/
`remember_preference` denials added since it was written; the TOCTOU
known-limitation bullet was missing `patch_file`, which has the identical
resolve-twice exposure), and two genuine new-capability gaps, both since
shipped: `deny_paths` basename-glob matching and `delegate_task` REPL
isolation (see "Current status" above for both). With both done, this
audit's backlog is now fully resolved.

No new capability opportunities were found in test quality or the
security/gate-tier-order/Landlock dimension this pass — see
`docs/HISTORY.md`'s "2026-07-28 capability audit" chapter for the full
account of what was checked.

A follow-up audit (2026-07-30) covering the same four dimensions against
everything shipped since 2026-07-28 (`deny_paths` basename-glob matching,
`delegate_task` REPL isolation, Landlock + `aivyx-repomap` basename-glob
enforcement, Docker Model Runner docs, and the `wiki_pointer_lines`/
`Agent::refresh_agents_files` `deny_paths` fixes) found nothing new in
security posture, tool coverage, or test quality. One documentation-only
gap: this entry itself was missing — `docs/HISTORY.md`'s Docker Model
Runner chapter states that whether Docker's default seccomp profile
blocks the Landlock syscalls needed for `aivyx-coder`'s own sandbox is
"logged as a separate, real, future design question," but no
corresponding backlog entry existed here. ~~It's a real open question,
not yet scheduled: **does containerizing `aivyx-coder` itself defeat its
own Landlock enforcement?** Docker's default seccomp profile likely
blocks `landlock_create_ruleset`/`landlock_add_rule`/
`landlock_restrict_self` (reasonably corroborated via research, not
empirically confirmed — no Docker daemon was accessible to test
directly).~~ **Empirically resolved 2026-09-21 — the answer is no, it
does not.** A real Docker daemon (v29.7.2) was accessible this session.
Built a minimal static (musl) C probe performing the full real Landlock
restriction cycle — `landlock_create_ruleset` (a genuine ruleset, not
just an ABI-version query), `landlock_add_rule` (a real path-beneath
rule granting read access to `/`), `prctl(PR_SET_NO_NEW_PRIVS)`, then
`landlock_restrict_self` — and ran it via a completely default
`docker run` (no `--privileged`, no custom `--security-opt`, no
`--cap-add`) against a stock `alpine:latest` image. All three syscalls
succeeded end to end, identical to the bare-host baseline (ABI version
9 both times). Confirmed this wasn't accidentally privileged/unconfined
three independent ways: `cat /proc/self/status` inside the container
showed `Seccomp: 2` (`SECCOMP_MODE_FILTER`, a real BPF filter active —
not `0`/disabled) and `Seccomp_filters: 1` (exactly one filter, Docker's
own default, not zero for unconfined or several for a custom stack);
`docker info --format '{{.SecurityOptions}}'` reported
`profile=builtin` (Docker's real, current built-in default profile, not
`unconfined`); and the container's effective capability set matched
Docker's normal non-privileged default, not a privileged/full set.
**Conclusion**: Landlock's three syscalls aren't on Docker's default
seccomp profile's denylist (that profile blocks a fixed list of ~44
specifically dangerous syscalls — `ptrace`, `mount`, `reboot`, and
similar — rather than allowlisting only pre-approved ones, so a newer
syscall like Landlock's, added in kernel 5.13, passes through
unaffected unless Docker's maintainers specifically added it to the
denylist, which they haven't). A containerized `aivyx-coder` build does
**not** need `require_enforcement: false` and does **not** silently run
unconfined — Landlock enforcement works identically inside a
completely default container as on the bare host. This **unblocks**
the original, larger "package aivyx-coder as a container for end users"
idea this question was gating — see `docs/HISTORY.md`'s "Docker Model
Runner serving support" chapter for the prior descoping story, now
superseded by this finding. (Caveat, worth remembering: this result is
specific to the kernel/Docker version tested — v29.7.2, kernel Landlock
ABI 9 — and to `alpine:latest`'s own base image; a different
kernel/Docker/container-runtime combination, or a host running Docker
inside another layer of containerization — e.g. Docker-in-Docker, a
managed CI runner, gVisor/Kata — was not tested and could plausibly
differ, since some of those add their own additional seccomp/namespace
restrictions on top of Docker's own default.)

A follow-up audit (2026-08-01) covering the same four dimensions against
everything shipped since 2026-07-30 (the real PTY for `repl_start`/
`repl_send`, and the slash command framework) found nothing new in
security posture, tool coverage, test quality, or documentation. Two
risks specific to those features' own history were checked with extra
scrutiny and confirmed still closed: the `O_CLOEXEC` fix (no raw-fd-open
site anywhere in the workspace reintroduced a similar leak) and the
exhaustive `AgentEvent` match count (all three sites — `aivyx-tui`'s
`handle_agent_event` and `sub_agent_event_text`, and `aivyx-acp`'s
`translate_event` — still explicitly handle `ConversationCleared`, no
fourth site and no silent wildcard arm anywhere). See `docs/HISTORY.md`'s
"2026-08-01 capability audit" chapter for the full account of what was
checked.

The final whole-branch review of cross-session memory (`aivyx-recall`,
2026-08-09 — see `docs/HISTORY.md`'s own entry for that chapter) found
and closed a real bug before merge (`memory_write`/`memory_forget`
sharing one Always-Allow cache key) plus three gaps (a cross-repo
dependency that would have broken on the next release tag, a missing
`deny_paths` default, a missing `docs/HISTORY.md` entry). Three smaller
findings from that same review are logged here rather than fixed ad hoc:

- ~~`repl_send`/`repl_stop` share the identical `PermissionKey`
  collision class Fix 1 (above) just closed for `memory_write`/
  `memory_forget`.~~ **Fixed 2026-08-10.** Confirmed by reading
  `confirmation.rs` directly that this one was never actually
  exploitable — `ActionKind::Interact` returns `Allow` unconditionally
  before the function ever reaches the Always-Allow cache, so it never
  touched `self.always_allow` in either direction. Both tools'
  `PermissionTarget::Other` strings are now tool-qualified
  (`"repl_send session"`/`"repl_stop session"`) as defense-in-depth
  against a future refactor that starts caching `Interact`, backed by a
  new gate-level test proving the prompter is never even called for an
  Interact request, plus a tool-level test proving the two targets now
  differ. The broader "audit every `PermissionTarget::Other` producer in
  the workspace" sweep this bullet also called for was not done —
  `repl_send`/`repl_stop` were the only other pair found, not a
  full audit.
- ~~`aivyx-recall` (the new sibling repo) has no `CLAUDE.md`, isn't
  listed in the root `~/Projects/Rust/CLAUDE.md` workspace table~~
  **Resolved.** `aivyx-recall` has its own `CLAUDE.md`; root `CLAUDE.md`
  now points at the new `aivyx-ecosystem` repo (`README.md`/`ROADMAP.md`/
  `GLOSSARY.md`), which lists it and keeps the table current going
  forward instead of duplicating it here.
- ~~The new `write_approval_does_not_satisfy_a_forget_on_the_same_topic`
  gate test lives in `aivyx-sandbox`, which has no dependency on
  `aivyx-tools`~~ — it proves the cache mechanism directly with
  hand-built `PermissionRequest`s, but can't itself detect a regression
  in `memory_write.rs`/`memory_forget.rs`'s actual `permission_request`
  output (that's covered by each tool's own updated unit test instead,
  as two decoupled halves rather than one end-to-end proof). ~~An
  `aivyx-tools`-side test driving both tools' real `permission_request()`
  through a real `ConfirmationGate` would close the loop properly.~~
  **Fixed 2026-08-26.** Added
  `write_approval_does_not_satisfy_a_forget_via_real_tool_output` in
  `aivyx-tools/src/lib.rs`, driving both tools' real `permission_request()`
  output through a real `ConfirmationGate::check()`. Mutation-proofed: made
  `memory_forget.rs`'s target collide with `memory_write.rs`'s, confirmed
  the test failed (1 prompt instead of 2), restored.

**New backlog, from the MCP-server frontend's own final review** (see
"MCP-server frontend — shipped" above for the two Important findings
that WERE fixed before merge; these Minor ones were deliberately
deferred, not overlooked):
- ~~`code`/`code_reply` construct a fresh `CancellationToken::new()` per
  call that nothing ever cancels — an MCP `notifications/cancelled` or a
  client disconnect currently cannot stop a runaway turn early;
  `max_iterations` is the only real bound.~~ — **Fixed 2026-09-21.** Both
  tools now accept a `CancellationToken` parameter, auto-injected by
  `rmcp`'s own `#[tool]` macro system per-request (via its
  `FromContextPart` extractor — no manual `request_id` tracking needed),
  threaded straight into `run_bounded_turn`. Confirmed the request-handling
  task itself isn't forcibly dropped on cancellation — only the token
  flips — so the earlier "`code_reply` future dropped mid-turn, `put_back`
  never runs" worry doesn't materialize for a graceful MCP-level cancel;
  it remains a real risk only for an actual raw connection/process death,
  out of reach of any code-level fix here.
- ~~`run_bounded_turn`'s `cap_hit` (`session.rs`) is `result.is_ok() &&
  agent.last_turn_paused()`, which is also true if a turn stopped via
  cancellation rather than genuinely exhausting `max_iterations` — today
  unreachable (see above), but would mislabel a cancelled turn as
  "reached its iteration budget" the moment real cancellation is wired
  in. Distinguish via `iterations_used >= max_iterations` instead.~~ —
  **Fixed 2026-09-21**, alongside the item above (the two were always
  coupled: this one only became a reachable bug once real cancellation
  existed). The stop-reason check is now a 3-way, explicitly-ordered
  chain: cancellation is checked first and unconditionally (found mid-fix
  to need this — gating it on `agent.last_turn_paused()`, as this entry
  originally proposed, turned out to make that branch structurally
  unreachable, since `run_turn_inner`'s own cancellation checkpoints
  always fire before the code that could set that flag), then
  injection-taint, then genuine budget exhaustion via
  `iterations_used >= max_iterations` as originally proposed.
- ~~`[mcp_server].session_ttl_secs`/`max_concurrent_sessions` accept `0`
  with no startup validation~~ — **Fixed 2026-08-26.**
  `validate_mcp_server_session_limits` in `crates/aivyx/src/main.rs` now
  rejects both at startup (matching the existing `max_access_level`
  rejection), called right after the `--mcp-server` flag's config is
  parsed. Mutation-proofed: temporarily short-circuited the `ttl_secs ==
  0` check, confirmed the rejection test failed, restored.
- ~~The three tier tool-name lists in `tiers.rs` are string denylists
  against `ToolRegistry::exclude`, which is a documented no-op for an
  unregistered name~~ — correct today (every name independently verified
  against real registered tools during the final review), but a future
  tool rename in `aivyx-tools` would silently widen a tier with no test
  failure. ~~A test asserting every listed name is actually present in a
  full registry would make a rename loud instead of silent.~~ **Fixed
  2026-08-26.** Added `every_excluded_tool_name_matches_a_real_registered_tool`
  in `tiers.rs`, constructing all 14 real tools and asserting each's
  `Tool::name()` matches. Mutation-proofed: corrupted one `EDIT_ONLY`
  entry, confirmed the test failed, restored.
- ~~`code`'s turn-error path discards both the `Agent` and any
  accumulated partial text (the session never reaches `insert`), while
  `code_reply`'s turn-error path keeps the session alive via `put_back`
  regardless of outcome~~ — a transient backend failure on a session's
  first turn loses everything; the identical failure on its second
  doesn't. Defensible (a `code` failure never had a session identity to
  preserve in the first place), ~~but undocumented as an intentional
  asymmetry.~~ **Fixed 2026-08-26.** Documented at both call sites in
  `server.rs`, cross-referencing each other — no behavior change, this
  was always the intended shape.

**New backlog, from KV-cache persistence's (`aivyx-kvcache`) task and
final reviews** (see "KV-cache persistence — shipped" above for the three
Important findings that WERE fixed before merge; these were deliberately
deferred, not overlooked):
- `KvSlotPool` has no concurrent-checkout stress test — a nice-to-have
  regression guard for its `Mutex<HashSet<u32>>` checkout logic, not a
  known bug.
- The warm-up request sends the *unfiltered* tool list, while real turns
  filter it by plan-mode/prompted-edit/autonomous-mode — this narrows
  cache reuse in those modes (a plan-mode turn's real tool list won't
  match the warmed prefix hash), but is not a correctness risk.
- ~~No `info`-level observability for kvcache hit/miss/save~~ — only `warn`
  on failure previously, so an operator couldn't easily tell from normal
  logs whether the feature was silently never engaging. **Fixed
  2026-08-26.** `ensure_kv_slot_checked_out` now logs `info` on hit, miss,
  and successful save, each carrying `prefix_hash`/`slot_id`.
- ~~Multi-process contention: `KvSlotPool`'s deterministic lowest-first
  checkout means two concurrent `aivyx-coder` processes against one
  shared `llama-server` both check out slot 0 first, pinning both
  sessions to the same physical slot and mutually invalidating each
  other's cache. A lock file or a per-process pool-index offset would
  fix this.~~ — **Fixed 2026-09-21.** `SlotPoolLock`
  (`crates/aivyx-llm/src/slot_pool_lock.rs`) claims a distinct starting
  offset per process via a real OS-level advisory file lock, scoped by a
  stable hash of the `llama-server` origin, held for the process's
  lifetime (auto-released on exit or crash). Falls back to offset 0 on
  any lock-acquisition failure — confirmed at final review to be a
  strict improvement over the old behavior even in that fallback case,
  never a regression (the overflow subset beyond real slot count still
  contends, which is unavoidable once genuinely oversubscribed).
  **Known real gap, not fixable in this repo**: this only coordinates
  `aivyx-coder` processes against each other — `aivyx-pa`, which
  `README.md`'s "KV-cache persistence" section explicitly documents as
  able to share the same store and `llama-server`, has no equivalent
  locking and will still default to slot 0, so the exact collision this
  fix targets can still occur in that specific cross-product scenario.
  The natural fix is lifting `SlotPoolLock` into the shared
  `aivyx-kvcache` crate both products already depend on — cross-repo
  follow-up, not scoped here.
- Cache-key volatility: any repo-map/tool-set/system-prompt change mints
  a new, potentially hundreds-of-MB cache entry, so real-world hit rates
  depend heavily on how stable a given project's own context is — a
  known, plan-mandated trade-off (see README's "KV-cache persistence"
  section), not a bug.
- ~~`LlamaServerSlotStore`'s own HTTP client (`aivyx-kvcache`, not this
  repo) has no per-request timeout on `restore_into_slot`/`save_from_slot`~~
  — the same unbounded-hang class the `/props` probe fix closed here, but
  on the *per-turn* path (`ensure_kv_slot_checked_out` runs at the top of
  every `run_turn`, before the cancellation check). A hung llama-server on
  a slot restore/save stalled the turn indefinitely and un-cancellably.
  **Fixed 2026-08-26** upstream in `aivyx-kvcache` (`src/llama_server.rs`,
  commit `59631f5`): added `SLOT_HTTP_TIMEOUT` (60s -- generous for
  multi-GB transfers, but finite) to the client `open()` builds.
  Mutation-proofed with a short-timeout test variant, not the real 60s
  value. `aivyx-coder`'s own pinned rev is unchanged by this entry --
  bumping it to pick up the fix is separate follow-on work.
- ~~The `hit` pre-check in `ensure_kv_slot_checked_out` (a `store.find()`
  call) is now fully redundant with `restore_into_slot`'s own internal
  `find()`~~ — **Fixed 2026-08-26.** Collapsed to one lookup; `Ok(true)`/
  `Ok(false)`/`Err` from `restore_into_slot` alone now drives the
  warm-up decision. No behavior change, one fewer HTTP request per
  session start.
- ~~No regression test exercises the three behavioral fixes from the final
  review (`Ok(false)` reaching the warm-up path, the `/props` probe
  client carrying a timeout, `kv_slot_id` being set before the first
  `.await`) — verified by code reading at review time, not by an
  automated test. `agent_builder.rs`'s kvcache-construction block also has
  zero test coverage of its own, since it's inline in `build_agent`~~ —
  same structural gap shape as the MCP-server tier-boundary finding from
  the previous chapter. **Fixed 2026-08-26.** Added two wiremock-driven
  regression tests in `aivyx-core/src/agent/tests.rs` (restore Ok(false)
  → warm-up; kv_slot_id recorded before the first await, no leak on
  cancellation) and extracted+tested `kv_cache_props_client()` in
  `agent_builder.rs`. `parse_llama_slots_info` (the JSON half of the
  block) already had its own tests in `aivyx-llm`; a full wiremock-driven
  end-to-end test of `build_agent`'s entire block would need a complete
  `Settings`/`Cli` fixture this crate has no harness for, left as a
  heavier follow-up. All three new tests mutation-proofed.

**New backlog, from specialist deny-paths attenuation's
(`crates/aivyx-core/src/specialist_enforcement.rs`,
`delegate_to_specialist.rs`, `specialist_sessions.rs`,
`crates/aivyx/src/agent_builder.rs`) own final review** — deliberately
deferred, not overlooked:
- `scoped_gate_and_confiner` does a real (potentially slow) filesystem
  walk to build the Landlock ruleset on *every* `delegate_to_specialist`/
  `spawn_specialist` call, unlike the lead's own gate/confiner, built
  once per process — synchronous inside an async fn, no `spawn_blocking`.
  Memoizing the `(gate, confiner)` pair per `(member name, cwd)` would
  fix this and also fix the next item in one change.
- A specialist's Always-Allow cache resets on every delegation call, not
  just once per session, since a fresh `ConfirmationGate` is built each
  time — meaning the same command re-prompts (interactive mode) or gets
  denied (autonomous mode, since a previously-lead-earned approval no
  longer applies) on every single delegation, not just the first.
- No test exercises the confiner (OS-level Landlock) side of this
  feature directly — only the gate side, via `write_file`. A Linux-gated
  test asserting a specialist's `run_command` genuinely cannot read/write
  a file under its own `extra_deny_paths` would be the highest-value
  addition.
- `GrepTool`/`GlobTool`/`RepoMap` still carry the lead's `deny_paths`,
  not the specialist's narrower union — and this is a real content leak,
  not just an existence leak: `GrepTool` returns full matched line text,
  so a specialist holding `grep` in its `tool_allowlist` can read the
  complete content of any small file (under `MAX_MATCHES = 200` lines)
  inside its own `extra_deny_paths`. See the design spec's "What this
  spec does not decide" section for the corrected severity writeup.

## Backlog from the 2026-10-28 code audit (`docs/audits/2026-10-28-code-audit.md`)

Left open on purpose; every Medium and High finding is fixed.

- **L1 `memory_read` returns recalled text unfenced.** Same as every other
  tool result and scanned by the per-result injection scan; a labelled fence
  is cheap hardening but changes the tool's output format, so it needs its own
  pass over the prompts that mention it.
- **L2 `memory_write` Always Allow is keyed by topic.** Consistent with the
  path-keyed `write_file`; wants a sentence in the approval text, not a code
  change.
- **L4 `-v/--verbose` is ignored with the TUI `--auto`.** Harmless; fold into
  the next CLI pass.
- **L5 `has_commit_hook` runs plain `git rev-parse` unconfined.** It cannot use
  `run_git` (that sets `core.hooksPath=/dev/null`, hiding the hooks it looks
  for); probed not exploitable today. Needs a guarded helper that keeps the
  hooks path.
- **L6 A goal-achieved run exits 0 when the verification command never ran.**
  Pinned by existing tests and documented; changing the exit table is a
  decision for the next headless revision.
- **L7 `generate_image` reads any non-denied `reference_image`.** The backend is
  the user's own and normally local; revisit with the vision toolset.
- **C23 (second half) the first ACP prompt waits ~30 s for an MCP server that
  does not speak MCP.** The notice is fixed; shortening or backgrounding the
  start-up wait changes MCP start-up semantics.

## Backlog from the 2026-10-31 editor integration audit (`docs/audits/2026-10-31-acp-audit.md`)

- **A10 registry coverage.** The entry lists darwin-aarch64 and linux-x86_64
  only; linux-aarch64 (Landlock works there), darwin-x86_64 and windows get no
  install. Adding a platform means a release leg and an `agent.json` entry;
  left for the release that ships the audit fixes.
- **A8 beyond a notice.** Editor-supplied MCP servers are reported and ignored;
  running them through the existing consent and confinement path is a feature,
  not a fix.
