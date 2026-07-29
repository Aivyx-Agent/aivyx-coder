# `delegate_task` REPL Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `delegate_task` sub-agents from sharing the parent's REPL
session — a sub-agent's tool list should simply never include
`repl_start`/`repl_send`/`repl_stop`.

**Architecture:** A new `ToolRegistry::exclude(&mut self, names: &[&str])`
method removes registered tools by name, in place. `agent_builder.rs`
calls it once, immediately after cloning the parent's registry into
`sub_agent_registry`, naming the three REPL tools. No new `ActionKind`,
`PermissionTarget`, or gate logic — this is pure tool-list composition.

**Tech Stack:** Rust, no new dependencies.

## Global Constraints

- `ToolRegistry`'s existing methods (`register`, `get`, `definitions`,
  `plan_definitions`) and its `#[derive(Default, Clone)]` must not
  change — `exclude` is a purely additive method.
- The parent's own tool registry must be completely unaffected — only
  the `sub_agent_registry` clone passed into `DelegateTaskConfig` loses
  the three REPL tools.
- The exclusion is unconditional (not behind any config flag), matching
  how `delegate_task`'s own self-exclusion is also unconditional.
- `crates/aivyx/src/agent_builder.rs` has zero existing tests and stays
  that way — it is pure integration wiring. The tested logic lives
  entirely in `ToolRegistry::exclude`; the one new line in
  `agent_builder.rs` is a direct, obviously-correct call to it, and is
  checked by the final whole-branch review reading the file directly,
  not by a new unit test.

---

### Task 1: `ToolRegistry::exclude` and wiring it into `delegate_task`'s sub-agent registry

**Files:**
- Modify: `crates/aivyx-tools/src/lib.rs`
- Modify: `crates/aivyx/src/agent_builder.rs`

**Interfaces:**
- Produces: `ToolRegistry::exclude(&mut self, names: &[&str])` — removes
  every registered tool whose name matches an entry in `names`, in
  place; a name with no matching registered tool is silently ignored.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` block in
`crates/aivyx-tools/src/lib.rs` (starting around line 251, right after
the existing `plan_definitions_offer_only_session_safe_tools` test):

```rust
    #[test]
    fn exclude_removes_the_named_tool_and_keeps_others() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));

        registry.exclude(&["write_file"]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn exclude_is_a_no_op_for_an_unregistered_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));

        registry.exclude(&["not_a_real_tool"]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file"]);
    }

    #[test]
    fn exclude_removes_multiple_names_in_one_call() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));

        registry.exclude(&["write_file", "edit_file"]);

        let names: Vec<String> = registry.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file"]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-tools --test-threads=1`
Expected: compile error — `exclude` does not exist yet on `ToolRegistry`.

- [ ] **Step 3: Implement `ToolRegistry::exclude`**

In `crates/aivyx-tools/src/lib.rs`, add this method inside the existing
`impl ToolRegistry { ... }` block (after `plan_definitions`, before the
closing `}` of the impl block):

```rust
    /// Removes every registered tool whose name matches an entry in
    /// `names`, in place. Absent names are silently ignored — a caller
    /// excluding a tool that was never registered (e.g. because a
    /// feature flag left it out) is not an error condition.
    pub fn exclude(&mut self, names: &[&str]) {
        self.tools.retain(|tool| !names.contains(&tool.name()));
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p aivyx-tools --test-threads=1`
Expected: all tests pass, including the three new ones and the
pre-existing `plan_definitions_offer_only_session_safe_tools` (unaffected
— it never calls `exclude`).

- [ ] **Step 5: Wire the exclusion into `agent_builder.rs`**

In `crates/aivyx/src/agent_builder.rs`, find this existing line (around
line 395):

```rust
    let sub_agent_registry = registry.clone();
```

and replace it with:

```rust
    let mut sub_agent_registry = registry.clone();
    sub_agent_registry.exclude(&["repl_start", "repl_send", "repl_stop"]);
```

(`exclude` takes `&mut self`, so the existing `let sub_agent_registry`
must become `let mut sub_agent_registry` — it is currently declared
without `mut`.)

- [ ] **Step 6: Run the full workspace build**

Run: `cargo build --workspace`
Expected: clean build, no errors or warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tools/src/lib.rs crates/aivyx/src/agent_builder.rs
git commit -m "Add ToolRegistry::exclude and use it to keep REPL tools out of delegate_task's sub-agent registry"
```

---

### Task 2: Documentation and backlog closure

**Files:**
- Modify: `README.md`
- Modify: `ROADMAP.md`
- Modify: `docs/HISTORY.md`

**Interfaces:**
- Consumes: nothing (documentation only); should be the last task since
  it describes the finished feature.

- [ ] **Step 1: Update README's "Sub-agent delegation" paragraph**

In `README.md`, find this existing text:

```markdown
**Sub-agent delegation** (`delegate_task`): a tool the model can call
mid-turn to hand a bounded task to a fresh, isolated agent — full tool
access, the same `ConfirmationGate`/checkpoint/plan-mode boundary as the
main session, but a completely separate conversation history, so
exploring or working on something unfamiliar doesn't clutter the main
session's own context window. Only the sub-agent's final text answer
enters the main session's history; its own tool calls/results/reasoning
render live in the transcript (prefixed `sub-agent>`, visually distinct)
but never join history directly. Bounded by `[sub_agent] max_iterations`
(default 10); a sub-agent that runs out of budget still returns its
best-effort partial result rather than failing outright. Delegation is
capped at one level — a sub-agent's own tool list never includes
`delegate_task`.
```

Replace it with (the only change is the new final sentence):

```markdown
**Sub-agent delegation** (`delegate_task`): a tool the model can call
mid-turn to hand a bounded task to a fresh, isolated agent — full tool
access, the same `ConfirmationGate`/checkpoint/plan-mode boundary as the
main session, but a completely separate conversation history, so
exploring or working on something unfamiliar doesn't clutter the main
session's own context window. Only the sub-agent's final text answer
enters the main session's history; its own tool calls/results/reasoning
render live in the transcript (prefixed `sub-agent>`, visually distinct)
but never join history directly. Bounded by `[sub_agent] max_iterations`
(default 10); a sub-agent that runs out of budget still returns its
best-effort partial result rather than failing outright. Delegation is
capped at one level — a sub-agent's own tool list never includes
`delegate_task`. REPL tools (`repl_start`/`repl_send`/`repl_stop`) are
excluded too — a sub-agent sharing the parent's single REPL session slot
would break the isolated-conversation-history guarantee this feature is
built around, so a sub-agent needing to run something falls back to
`run_command`/`run_shell` instead.
```

- [ ] **Step 2: Update ROADMAP.md's Backlog section**

In `ROADMAP.md`, find this existing paragraph (in the "## Backlog"
section):

```markdown
A fresh audit (2026-07-28) covering the same four dimensions against the
current codebase found two documentation-accuracy gaps, fixed directly
(the autonomous-mode paragraph was missing `repl_start`/MCP-tool/
`remember_preference` denials added since it was written; the TOCTOU
known-limitation bullet was missing `patch_file`, which has the identical
resolve-twice exposure), and one genuine new-capability gap, logged here
rather than patched ad hoc: **`delegate_task` sub-agents share the
parent's single global REPL session slot**, breaking the "fresh, isolated
agent, completely separate conversation history" invariant `delegate_task`
is documented to provide. `crates/aivyx/src/agent_builder.rs` clones the
same `ToolRegistry` (same underlying `Arc<Mutex<Option<ReplSession>>>`)
for sub-agents; `sub_agent_registry_never_contains_delegate_task_itself`
is the only sub-agent tool exclusion that exists today. A sub-agent's
`repl_start` call is invisible to the parent's own history, yet the
process it starts outlives the sub-agent and can collide with (or be
silently reused/blocked by) the parent's own REPL usage. Needs its own
design pass: exclude REPL tools from the sub-agent registry entirely,
give each sub-agent a private session slot, or auto-stop any session a
sub-agent leaves running when it completes. (The `deny_paths` gap
previously logged alongside this one shipped — see "`deny_paths`
basename-glob matching" above.)
```

Replace it with:

```markdown
A fresh audit (2026-07-28) covering the same four dimensions against the
current codebase found two documentation-accuracy gaps, fixed directly
(the autonomous-mode paragraph was missing `repl_start`/MCP-tool/
`remember_preference` denials added since it was written; the TOCTOU
known-limitation bullet was missing `patch_file`, which has the identical
resolve-twice exposure), and two genuine new-capability gaps, both since
shipped: `deny_paths` basename-glob matching and `delegate_task` REPL
isolation (see "Current status" above for both). With both done, this
audit's backlog is now fully resolved.
```

Then, in the "## Current status" section, find this existing paragraph
(the end of the `deny_paths` basename-glob matching entry):

```markdown
`aivyx_sandbox::path_is_denied` function as part of this work.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.
```

Insert a new "shipped" paragraph between them:

```markdown
`aivyx_sandbox::path_is_denied` function as part of this work.

**`delegate_task` REPL isolation — shipped.** The last item in the
2026-07-28 capability audit's backlog, closing it out entirely.
`delegate_task` sub-agents previously shared the parent's single global
REPL session (`ToolRegistry::clone()` clones `Arc` pointers, not
underlying state, so a sub-agent's registry held the exact same
`ReplStartTool`/`ReplSendTool`/`ReplStopTool` instances, bound to the
same session, as the parent) — breaking the "fresh, isolated agent,
completely separate conversation history" invariant `delegate_task` is
documented to provide. Fixed with a new `ToolRegistry::exclude` method,
called once in `agent_builder.rs` right after the parent registry is
cloned for sub-agent use: REPL tools are simply never offered to a
sub-agent, the same "just never in the list" outcome `delegate_task`
already achieves for its own one-level recursion cap. No new
`ActionKind`, `PermissionTarget`, or gate logic — pure tool-list
composition. Sub-agents keep `run_command`/`run_shell` for one-shot
needs; only interactive multi-turn REPL sessions are unavailable to them.

See `docs/HISTORY.md` for the full phase-by-phase narrative behind
every item above.
```

Also update the `_Last updated:_` line at the top of `ROADMAP.md` from
`2026-07-28` to today's date.

- [ ] **Step 3: Add a HISTORY.md chapter**

In `docs/HISTORY.md`, find the end of the "### 2026-07-28 capability
audit — done" chapter's final-review addendum (the chapter added for the
`deny_paths` basename-glob feature — it is the last chapter in the
file) and append a new chapter after it:

```markdown
### `delegate_task` REPL isolation — ✅ shipped

The last item in the 2026-07-28 capability audit's backlog, closing it
out entirely. Design spec at
`docs/superpowers/specs/2026-07-29-delegate-task-repl-isolation-design.md`,
implementation plan at
`docs/superpowers/plans/2026-07-29-delegate-task-repl-isolation.md`,
executed via `subagent-driven-development`.

**The problem**: `delegate_task` sub-agents shared the parent's single
global REPL session. `crates/aivyx/src/agent_builder.rs` registers
`ReplStartTool`/`ReplSendTool`/`ReplStopTool` onto the parent's
`ToolRegistry`, all three bound to the same `SharedReplSession`
(`Arc<AsyncMutex<Option<ReplSession>>>`), *before* cloning that registry
into `sub_agent_registry` for `delegate_task`'s use. `ToolRegistry`'s
`Clone` clones `Arc` pointers, not underlying tool state, so a
sub-agent's registry held the *exact same* REPL tool instances as the
parent — a sub-agent's `repl_start` call was invisible to the parent's
own conversation history, yet the process it started outlived the
sub-agent and could collide with the parent's own REPL usage (a
`repl_start` failing with "already running" for a session the other
side didn't know existed, in either direction).

**What shipped**: a new `ToolRegistry::exclude(&mut self, names:
&[&str])` method (`crates/aivyx-tools/src/lib.rs`) removes registered
tools by name, in place — absent names are silently ignored. One new
line in `agent_builder.rs`, immediately after the existing
`sub_agent_registry` clone, calls it with the three REPL tool names.
That is the entire fix: no new `ActionKind`, `PermissionTarget`, or gate
logic, since this is pure tool-list composition, not a new capability
needing its own trust tier.

**Three fix shapes were considered during design, resolved with the
user**: exclude REPL tools from the sub-agent registry entirely
(chosen — smallest, safest, sub-agent REPL access is a narrow edge case
`run_command`/`run_shell` mostly covers); give each sub-agent a private
REPL session (preserves full capability, but needs meaningfully more
plumbing — a registry-mutation primitive plus threading the REPL tools'
constructor settings into `DelegateTaskConfig` to rebuild them per call);
or keep sharing and auto-stop on completion (cheapest, but doesn't fix
the collision case, only prevents a leaked process). The chosen fix
means `delegate_task`'s "full tool access" claim in `README.md` needed a
one-clause correction — updated alongside the code fix.

**A deliberate testing-scope decision, matching this project's
established convention**: no test was added in `agent_builder.rs`
itself, which has zero existing tests and remains pure, untested
integration wiring — the tested logic lives entirely in the new
`ToolRegistry::exclude` primitive (three unit tests: removes a named
tool while keeping others, is a no-op for an absent name, removes
multiple names in one call), and the one-line call site was verified by
the final whole-branch review reading `agent_builder.rs` directly rather
than by a dedicated unit test that can't see the real registration
order anyway. Same pattern this project already used for
`[verification] scoped_command`'s resolution logic, tested at `Agent`'s
own level rather than at the `agent_builder.rs` call site.

With this shipped, the entire 2026-07-28 capability-audit backlog is
closed — no tracked items remain.
```

- [ ] **Step 4: Commit**

```bash
git add README.md ROADMAP.md docs/HISTORY.md
git commit -m "Document delegate_task REPL isolation; close the backlog item"
```
