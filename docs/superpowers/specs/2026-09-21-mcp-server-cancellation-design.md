# MCP Server Real Cancellation + `cap_hit` Fix Design

## Context

`ROADMAP.md`'s "New backlog, from the MCP-server frontend's own final
review" section lists two deliberately-deferred, coupled Minor findings:
`code`/`code_reply` construct a fresh, disconnected `CancellationToken::new()`
per call that nothing external can ever trigger — an MCP
`notifications/cancelled` or a client disconnect cannot stop a runaway
turn early, `max_iterations` is the only real bound — and
`run_bounded_turn`'s cap-hit signal (a local variable named `paused` in
the real code, not literally `cap_hit`) would mislabel a cancelled turn as
"reached its iteration budget" the moment real cancellation exists. This
spec is that fix, chosen as the first of this session's small
`ROADMAP.md` backlog queue (queue item #4) specifically because the
second finding only becomes a real, reachable bug once the first is
fixed — they're naturally one initiative, not two.

## Grounding

Read directly in the current codebase and the pinned `rmcp` 0.11.0
source, not assumed:

- **`rmcp` already solves the hard part.** `crates/aivyx-mcp-server/src/server.rs`
  uses `rmcp`'s `#[tool_router]`/`#[tool]` macro system (not a manual
  `ServerHandler::call_tool` override). Confirmed in the pinned crate
  source (`~/.cargo/registry/.../rmcp-0.11.0/src/handler/server/common.rs`):
  `impl<C> FromContextPart<C> for tokio_util::sync::CancellationToken`
  exists, returning `context.as_request_context().ct.clone()` — meaning a
  `#[tool]`-annotated function can simply declare an extra
  `tokio_util::sync::CancellationToken` parameter and the macro
  auto-injects the request's real, live token. No manual `request_id`
  correlation is needed.
- **Confirmed exactly how that token gets cancelled**, in `rmcp-0.11.0/src/service.rs`'s
  serve loop: each incoming request is given a child token, pooled by
  `id` in `local_ct_pool`; on a `notifications/cancelled` message
  matching that `id`, the pooled token's `.cancel()` is called. The
  request-handling task itself runs inside a real `tokio::spawn` — **not**
  forcibly aborted or dropped by this cancellation; only the token flips.
  This means the original ROADMAP note's more pessimistic "future dropped
  mid-turn, `put_back` never runs" framing is not quite how `rmcp`
  actually behaves — cancellation is cooperative, not a hard abort.
- **`run_bounded_turn` (`crates/aivyx-mcp-server/src/session.rs:241`)
  already accepts and correctly threads a `cancellation: CancellationToken`
  parameter** straight into `agent.run_turn(turn_input, cwd,
  cancellation.clone())` — the exact mechanism the TUI's own `--auto`
  driver loop already uses for Ctrl+C. The only real gap is that `code`/
  `code_reply` (`server.rs`, two call sites) pass `CancellationToken::new()`
  — freshly constructed, connected to nothing — instead of a real one.
  Once wired, `run_bounded_turn`'s own loop-continuation guard already
  checks `!cancellation.is_cancelled()` (`session.rs`, existing code) as
  one of its AND-conditions, so a cancelled turn causes the loop to stop
  gracefully (not via an `Err`) — meaning `code_reply` will still reach
  its own `put_back` step normally on a cancelled turn. The "session
  silently stuck as taken()" scenario remains a real risk only for an
  actual raw connection/process death, not a graceful MCP-level cancel —
  no code in this fix can address that harder case, and it's out of
  scope.
- **The `cap_hit`/`paused` bug, precisely traced.** Current code:
  ```rust
  let paused = result.is_ok() && agent.last_turn_paused();
  let injection_tainted = agent.injection_taint().current().is_some();
  if paused && injection_tainted {
      // ... injection message
  } else if paused {
      // ... "(session stopped: reached its iteration budget ...)"
  }
  ```
  `agent.last_turn_paused()` reflects only the *last* round-trip's own
  internal per-turn pause state (set inside `aivyx-core`'s `run_turn_inner`
  on injection-taint or hitting the model's own `max_tool_iterations`,
  unrelated to `run_bounded_turn`'s own *outer* `iterations_used`/
  `max_iterations` counters) — it says nothing about *why* the outer loop
  actually stopped continuing. Confirmed by tracing the outer loop's own
  continue-guard (`result.is_ok() && agent.last_turn_paused() &&
  iterations_used < max_iterations && !cancellation.is_cancelled() &&
  agent.injection_taint().current().is_none()`): once cancellation is
  real, a cancelled turn can leave `paused` true purely because the last
  round-trip itself happened to pause internally, with the outer loop
  then stopping for an unrelated reason (cancellation) — exactly the
  mislabeling the ROADMAP note flagged.

## Decisions

**1. `code` and `code_reply` each gain an extra
`cancellation: tokio_util::sync::CancellationToken` parameter**, injected
by `rmcp`'s macro system (no `RequestContext` plumbing needed elsewhere).
Both call sites pass this real token into `run_bounded_turn` instead of
`CancellationToken::new()`. No signature change to `run_bounded_turn`
itself — it already accepts exactly this type.

**2. `run_bounded_turn`'s post-loop stop-reason logic becomes a 3-way,
exhaustive, explicitly-ordered check**, replacing the current 2-way one:

```rust
let would_have_continued = result.is_ok() && agent.last_turn_paused();
let injection_tainted = agent.injection_taint().current().is_some();
let cancelled = cancellation.is_cancelled();

if would_have_continued && injection_tainted {
    // unchanged message
} else if would_have_continued && cancelled {
    // new: "(session stopped: cancelled by the client -- the above is
    // its best-effort partial result.)"
} else if would_have_continued && iterations_used >= max_iterations {
    // unchanged message text, now correctly gated on the real outer
    // budget instead of being inferred from `paused` alone
}
```

Confirmed exhaustive by tracing the outer loop's own continue-guard: the
only way to reach this code with `would_have_continued == true` and none
of `injection_tainted`/`cancelled`/`iterations_used >= max_iterations`
true is impossible, since the loop's own guard would have continued
instead of exiting in that exact state.

**3. Cancellation notice wording**: a cancelled turn gets its own
distinct trailing notice (confirmed with the project owner), matching
the existing precedent that injection-tainted and budget-exhausted turns
both already get explanatory trailing text — a caller seeing partial text
with no explanation at all could mistake it for a complete answer.

## What this spec does not decide

- Any handling for a raw MCP connection/process death (as opposed to a
  graceful `notifications/cancelled`) — confirmed out of reach of any
  code-level fix here, since `rmcp`'s spawned request-handling task isn't
  something this crate's own code controls the lifecycle of on a hard
  disconnect.
- Any change to `ServerHandler::on_cancelled` (the trait method rmcp
  offers for observing a cancellation notification directly) — not
  needed, since the auto-injected `CancellationToken` parameter already
  gets cancelled by rmcp's own dispatch loop without this crate needing
  to override that method.
- Any change to `max_iterations`'s own value/config surface, or to
  `aivyx-core`'s `run_turn_inner`/`last_turn_paused` mechanism itself —
  this fix consumes those as-is.
- The KV-cache multi-process slot contention backlog item — separate,
  later scope (queue item after this one), unrelated to MCP cancellation.
