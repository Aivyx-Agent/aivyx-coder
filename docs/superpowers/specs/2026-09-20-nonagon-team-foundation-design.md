# Nonagon-Style Team — Foundation Design

## Context

`aivyx-pa` (a separate, unrelated repo — see the workspace root
`CLAUDE.md`) ships a multi-agent team capability called the **Nonagon**:
a lead agent that decomposes a mission into a DAG, delegates steps to up
to 9 ephemeral, capability-attenuated specialist agents, verifies their
output, and synthesizes a result (`aivyx-pa/docs/NONAGON.md`,
`aivyx-pa/crates/aivyx-team`). `aivyx-coder` has no equivalent
capability of its own — the only cross-product relationship today is the
other direction: `aivyx-pa`'s own `NONAGON.md` (§9, "worked example")
already documents an `aivyx-pa` Nonagon team delegating a bounded coding
task *out* to `aivyx-coder`, bridged in as an out-of-process MCP
specialist. This spec covers giving `aivyx-coder` itself a Nonagon-style
team capability — approved as a full analog to `aivyx-pa`'s feature set,
scoped across 6 phases (this spec covers Phase 1, Foundation, only).

## Grounding

Read directly, not assumed:

- `aivyx-pa/docs/NONAGON.md` — the full design: `TeamConfig` (declarative
  TOML, each member = a Role: `system_prompt` + `tool_allowlist` +
  `capability_scopes` + `trust_ceiling`), the `attenuate_for_member`
  invariant (NT-02: "a specialist can never exceed its lead" —
  `specialist.caps = declared.filter(|s| lead.grants(s))`), the core
  architectural insight ("not an agent-loop rewrite" — the lead is a
  normal agent whose tools are `delegate_task`/`spawn_specialist`/
  `query_agent`/`synthesize_results`/`verify_output`; calling one spins
  up a specialist's own turn loop and returns the result), the 9-role
  default roster, the `MissionPlan` DAG + `TeamRuntime`, and the 7-phase
  build (J.1 Foundation → J.7 TUI).
- `aivyx-coder/crates/aivyx-core/src/council.rs` — the existing
  `/council` mode: several models independently answer the *same*
  question, anonymously rank each other, a chairman synthesizes one
  recommendation. No tools, no delegation, no task decomposition — a
  distinct, existing pattern (ensemble/voting) this spec does not touch
  or replace.
- `aivyx-coder/crates/aivyx-mcp-server/src/tiers.rs` /
  `session.rs:118` (`build_session_agent`) — the closest existing
  precedent for "build a scoped, isolated `Agent` instance": each MCP
  session gets its own fresh `Agent`, filtered to an `AccessLevel`
  (`Plan`/`Edit`/`Execute`, `at_most(&ceiling)`) ceiling via a filtered
  base tool registry. Directly reusable pattern for building a
  specialist's own `Agent`, though `AccessLevel` itself (three
  hard-coded tiers) is too coarse for per-member tool_allowlist
  attenuation and isn't reused directly.
- `aivyx-coder/crates/aivyx-tools/src/tools/grep.rs` (and
  `glob.rs`/`move_file.rs`/`git_commit.rs`) — the established
  constructor-injected `deny_paths: Vec<PathBuf>` +
  `aivyx_sandbox::path_is_denied(path, deny_paths)` pattern every
  path-touching tool already follows. The natural mechanism for a
  specialist's attenuated deny-list, not a new capability-scope type.
- `aivyx-coder/CLAUDE.md` — `ToolExecutor::dispatch` centralizes the
  permission check ("all tool calls go through the gate" — enforced by
  convention, the executor is the only caller of `Tool::execute`);
  `ConfirmationGate` is one gate for ALL tools in-process (unlike
  `aivyx-pa`'s separate-process model); `needs_checkpoint()`
  auto-checkpoints any mutating tool call regardless of which logical
  "agent" issued it. This spec's design leans on all three being reused
  unchanged, mirroring `aivyx-pa`'s own "reused unchanged" list for its
  Nonagon (§10 of `NONAGON.md`).
- `aivyx-coder` has no `Scope`/`CapabilitySet`/`TrustTier`/`Role`/
  `Persona` types at all (those are `aivyx-pa`-specific, defined in its
  own `aivyx-capability` crate) — confirmed by their absence from this
  workspace's crate list. This spec cannot port NT-02's mechanism
  verbatim; it needs an aivyx-coder-shaped equivalent (Decision 2).

## Decisions

**1. Phase roadmap (approved, this spec covers Phase 1 only):**
   1. **Foundation** (this spec) — `TeamConfig` schema, attenuation
      invariant, default roster (data only, no delegation tooling yet).
   2. **Pool + delegation** — spin up attenuated specialist `Agent`
      instances in-process; `delegate_task`/`query_agent` tools.
   3. **Mission structure** — task decomposition, verify, synthesize;
      sequential only (no DAG/concurrency yet).
   4. **Message bus / peer dialogue** — specialists messaging each other
      mid-mission.
   5. **Entry point + audit trail** — a `--team`/`/team` invocation
      (matching `/council`'s existing slash-command pattern), and how a
      mission's actions surface via the existing checkpoint mechanism.
   6. **TUI surface** — a live missions view, analogous to `aivyx-pa`'s
      Missions panel.

   No `aivyx-pa`-style vertical-pack phase (J.6) — `aivyx-coder` has no
   vertical/pack concept.

**2. The attenuation invariant, aivyx-coder-shaped.** Since there is no
`CapabilitySet`/`Scope` to reuse, the analog is built from primitives
`aivyx-coder` already has:
   - A specialist's `tool_allowlist` must be a subset of the lead's
     actual registered tool names (validated at `TeamConfig` load time —
     an unknown/ungranted tool name is a load error, never a silent
     no-op).
   - A specialist's effective `deny_paths` is the **union** of the
     lead's `deny_paths` and the member's own `extra_deny_paths` — a
     specialist can only ever be handed *more* restriction than the
     lead, never less. No mechanism exists (or is added) for a
     specialist to see a path the lead itself denies.
   - This is the direct analog of NT-02 ("specialist can never exceed
     its lead"), expressed as: `specialist.tools ⊆ lead.tools` and
     `specialist.deny_paths ⊇ lead.deny_paths`.

**3. Core reuse principle carries over unchanged from `aivyx-pa`'s own
design.** The lead is a normal `Agent` running its ordinary
`run_turn` loop; later phases' delegation tools (`delegate_task` etc.)
are ordinary `Tool` implementations dispatched through the existing
`ToolExecutor`/`ConfirmationGate` — no new gate, no new checkpoint
mechanism, no new audit surface, no agent-loop rewrite. A specialist's
own `Agent` instance is built the same way `build_session_agent`
already builds a fresh, tool-filtered `Agent` per MCP session — a
directly reusable construction pattern, not a new one.

**4. `TeamConfig` schema (new `aivyx-team` crate, matching the
`aivyx-team` naming `aivyx-pa` already established for the same
concept):**

```rust
pub struct TeamConfig {
    pub lead: String,               // must match one member's `name`
    pub members: Vec<TeamMember>,
}

pub struct TeamMember {
    pub name: String,
    pub role: String,               // short label, e.g. "Reviewer"
    pub persona: String,            // system-prompt fragment, appended
                                     // to the existing SYSTEM_PROMPT_PREAMBLE
                                     // -- no separate Role/Persona type
    pub tool_allowlist: Vec<String>,
    pub extra_deny_paths: Vec<String>,
}
```

One shared backend/model for the lead and every specialist — model
diversity stays `/council`'s distinct feature, not re-litigated here.

**5. Default roster: coding-shaped, not `aivyx-pa`'s generic 9 roles.**
`aivyx-pa`'s roster (`coordinator`/`researcher`/`analyst`/`coder`/
`writer`/`reviewer`/`planner`/`ops`/`archivist`) targets a general-purpose
personal assistant. `aivyx-coder`'s default ships four roles instead,
matching what coding delegation actually looks like (closer to this
project's own subagent-driven-development convention than to a general
office team): `coordinator` (lead), `implementer`, `reviewer`, `tester`.
Ships as schema-valid, loadable config data in Phase 1 even though no
delegation tooling exists yet to invoke it (Phase 2's job) — matching
`aivyx-pa`'s own J.1 precedent of shipping role definitions ahead of the
tooling that uses them.

**6. Validation happens at config-load time, not delegation time.** A
`TeamConfig` with an unknown tool name in some member's `tool_allowlist`,
or no member matching `lead`, is a load error. This mirrors this
project's existing convention of failing fast at config load rather than
at first use (e.g. `[mcp_server].max_access_level` refusing to start
unset).

**7. Testing scope for Foundation.** Schema/validation unit tests only
(unknown tool name rejected, deny-paths union computed and enforced
correctly including a qualified/nested-path case — matching `aivyx-pa`'s
own `attenuate_for_member` test suite's stated edge-case coverage, no
integration tests yet since there is no delegation tooling to integrate
against until Phase 2.

## What this spec does not decide

- Phases 2-6's own designs — each gets its own spec/plan cycle when
  reached, per the approved roadmap.
- Whether `deny_paths` union computation happens eagerly (baked into the
  `TeamConfig` at load time) or lazily (computed per-delegation in
  Phase 2) — a real implementation choice deferred to Phase 2, since
  Foundation only needs the invariant defined and testable, not wired
  into a real delegation path yet.
- Where a `TeamConfig` file lives / how it's loaded into a running
  session (a new `[team]` section in `config.toml`? A separate
  `team.toml` like `aivyx-pa`'s `--config <pack.toml>`? A CLI flag?) —
  Phase 5's "entry point" concern, not Foundation's.
- Whether the default 4-role roster is the final shape or just Phase 1's
  starting point — open to revision once Phase 2's delegation tools
  exist and the roster can actually be exercised.
