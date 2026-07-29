# `delegate_task` REPL Isolation — Design

**Status:** Approved by user 2026-07-29.

## Context

The second and last item in the 2026-07-28 capability audit's backlog:
`delegate_task` sub-agents share the parent's single global REPL session
slot, breaking the "fresh, isolated agent, completely separate
conversation history" invariant `delegate_task` is documented to provide.

`crates/aivyx/src/agent_builder.rs` registers `ReplStartTool`/
`ReplSendTool`/`ReplStopTool` onto the parent's `ToolRegistry`, all three
constructed against the same `SharedReplSession`
(`Arc<AsyncMutex<Option<ReplSession>>>`, from `new_shared_repl_session()`),
*before* `let sub_agent_registry = registry.clone();` runs. `ToolRegistry`
is a thin `Vec<Arc<dyn Tool>>` wrapper, so cloning it clones the `Arc`
pointers, not the underlying tools — a sub-agent's `sub_agent_registry`
therefore holds the *exact same* `ReplStartTool`/`ReplSendTool`/
`ReplStopTool` instances as the parent, sharing the identical session
state. `sub_agent_registry_never_contains_delegate_task_itself`
(`crates/aivyx-core/src/delegate.rs`) is the only sub-agent tool
exclusion that exists today, and it works purely by *ordering*
(`delegate_task` is registered onto the parent's registry *after* the
clone, so the clone structurally never has it) — there is no
name-based exclusion mechanism anywhere in the codebase yet.

Concretely, this means: a sub-agent's `repl_start` call is invisible to
the parent's own conversation history (only the sub-agent's final text
answer folds back in), yet the process it starts outlives the sub-agent
and can collide with the parent's own REPL usage — a sub-agent's
`repl_start` failing with "already running" for a session the parent
started and doesn't know the sub-agent even attempted to touch, or vice
versa.

Three fix shapes were considered with the user:

1. **Exclude REPL tools from the sub-agent registry entirely** — the
   sub-agent simply never sees `repl_start`/`repl_send`/`repl_stop` as
   available tools, the same "just never offered" outcome `delegate_task`
   already achieves for itself, just via an explicit name-based removal
   instead of ordering (REPL tools are registered *before* the clone
   point, unlike `delegate_task`). Smallest, safest change; sub-agents
   lose the ability to hold an interactive multi-turn session open, but
   keep `run_command`/`run_shell` for one-shot needs.
2. **Give each sub-agent a private REPL session** — every `delegate_task`
   call would build a fresh `SharedReplSession` and fresh tool instances
   bound to it, swapped into that call's registry. Preserves full
   capability with real isolation, but needs new plumbing: a
   registry-mutation primitive plus threading the REPL tools'
   constructor settings (`quiet_window`/`max_wait`/`idle_timeout`) into
   `DelegateTaskConfig` so they can be rebuilt per call.
3. **Keep sharing, auto-stop on completion** — cheapest to build, but
   doesn't fix the collision case (a sub-agent's `repl_start` failing
   because the parent already has a session running, or vice versa),
   only prevents a session leaking past the sub-agent's own lifetime.

**Resolved: option 1.** Smallest, safest, and sub-agent REPL usage is a
narrow edge case for `delegate_task`'s stated use ("explore or work on
something unfamiliar" — see `README.md`) that doesn't obviously need an
interactive session; a one-shot `run_command`/`run_shell` call covers
the common case.

## Decisions

### New primitive: `ToolRegistry::exclude`

`crates/aivyx-tools/src/lib.rs` gains a new method on `ToolRegistry`:

```rust
impl ToolRegistry {
    /// Removes every registered tool whose name matches an entry in
    /// `names`, in place. Absent names are silently ignored — a caller
    /// excluding a tool that was never registered (e.g. because a
    /// feature flag left it out) is not an error condition.
    pub fn exclude(&mut self, names: &[&str]) {
        self.tools.retain(|tool| !names.contains(&tool.name()));
    }
}
```

This is a small, pure, easily-unit-tested addition alongside the
existing `register`/`get`/`definitions`/`plan_definitions` methods — it
does not touch `Tool`, `ToolExecutor`, `ConfirmationGate`,
`ActionKind`, or `PermissionTarget` at all. This is a pure tool-list
composition change, not a new gate primitive, matching how `patch_file`
needed zero new gate surface for an analogous reason (reusing an
existing shape rather than inventing one).

### `agent_builder.rs`: one new line

Immediately after the existing line:

```rust
let sub_agent_registry = registry.clone();
```

add:

```rust
sub_agent_registry.exclude(&["repl_start", "repl_send", "repl_stop"]);
```

This is the entire wiring change. The parent's own `registry` is
untouched — only the clone assigned to `DelegateTaskConfig::sub_agent_registry`
loses the three REPL tools. No new config field, no conditional logic:
REPL tools are excluded from every sub-agent's tool list unconditionally,
matching how `delegate_task`'s own self-exclusion is also unconditional
(not gated behind any setting).

`crates/aivyx/src/agent_builder.rs` has zero existing tests today (it is
a pure integration/wiring function — building a real `Agent` requires a
full LLM backend, sandbox, and config setup that isn't practical to unit
test directly) and this change doesn't alter that: the new logic lives
entirely in the tested `ToolRegistry::exclude` primitive, and this one
line is a direct, obviously-correct call to it. This matches the
existing precedent in this codebase (e.g. `[verification] scoped_command`'s
resolution logic is tested at `Agent`'s own level in `aivyx-core`, not at
the `agent_builder.rs` call site that wires it up).

## Out of scope for this spec

- Any new config field to make REPL sub-agent access configurable —
  the audit finding and the user's chosen fix are both unconditional.
- Giving sub-agents a private, isolated REPL session (option 2 above) —
  a reasonable future increment if a real need for sub-agent REPL access
  emerges, not required to close this backlog item.
- Any change to the parent's own REPL tool registration, behavior, or
  the `ReplSession`/`SharedReplSession` types themselves.

## Testing / verification

Unit tests added to the existing `mod tests` block in
`crates/aivyx-tools/src/lib.rs` (already present, starting around line
251, currently exercising `ToolExecutor`/`ToolRegistry` setup):

- Excluding a name that is registered removes exactly that tool from
  `definitions()` — verified by registering two distinct tools, excluding
  one by name, and asserting the other is still present and the excluded
  one is gone.
- Excluding a name that was never registered is a no-op — the registry's
  `definitions()` before and after are identical.
- Excluding multiple names in one call removes exactly those tools,
  leaving everything else untouched (covers the real call site's
  three-name-at-once shape).

No test is added in `agent_builder.rs` itself, consistent with that
file's existing untested-wiring convention described above. The final
whole-branch review should independently confirm (by reading
`agent_builder.rs`) that the one new line is placed correctly — after
the `sub_agent_registry` clone, referencing the exact three REPL tool
names `ReplStartTool`/`ReplSendTool`/`ReplStopTool` actually register
under (`"repl_start"`/`"repl_send"`/`"repl_stop"`) — rather than trusting
a unit test that can't see the real registration call sites at all.

**Live E2E verification** (manual follow-up after implementation, matching
how every other feature in this project has been verified — see
`docs/HISTORY.md`): confirm via a real model session that `delegate_task`
sub-agents no longer see `repl_start` in their offered tool list (e.g. by
asking a sub-agent to attempt an interactive REPL session and observing
it falls back to `run_command`/`run_shell` or reports the tool
unavailable), while the parent session's own REPL tools remain fully
functional.

## Documentation

`README.md`'s "Sub-agent delegation" paragraph currently says a
sub-agent gets "full tool access" unqualified — now inaccurate. Add a
one-clause correction parallel to how the same paragraph already notes
the one-level recursion cap ("Delegation is capped at one level — a
sub-agent's own tool list never includes `delegate_task`."): a
sentence noting REPL tools are excluded too, and why (session-sharing
would break the isolated-conversation-history guarantee). `ROADMAP.md`'s
backlog entry for this item is removed and replaced with a "shipped"
paragraph in Current status, matching every other closed backlog item's
treatment; `docs/HISTORY.md` gets a narrative chapter, matching the
existing chapters' depth. With this item closed, the entire 2026-07-28
capability audit backlog is resolved.
