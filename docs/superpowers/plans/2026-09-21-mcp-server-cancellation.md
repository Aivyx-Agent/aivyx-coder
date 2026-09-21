# MCP Server Real Cancellation + `cap_hit` Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `code`/`code_reply` respect real MCP-level cancellation (`notifications/cancelled` or client disconnect), and the resulting stop-reason message correctly distinguishes cancellation from genuine iteration-budget exhaustion instead of mislabeling one as the other.

**Architecture:** `rmcp`'s `#[tool]` macro auto-injects a live, per-request `CancellationToken` into any tool function that declares it as a parameter (confirmed via the pinned crate's own `FromContextPart` trait and generic call-dispatch code — no manual `request_id` bookkeeping needed). `code`/`code_reply` gain this parameter and thread it into the already-cancellation-aware `run_bounded_turn`, whose post-loop stop-reason logic becomes an explicit 3-way check (injection / cancelled / budget-exhausted) instead of the current 2-way one that conflates the last two.

**Tech Stack:** Rust, `tokio`, `tokio_util::sync::CancellationToken`, `rmcp` 0.11.0.

## Global Constraints

- No change to `run_bounded_turn`'s signature — it already accepts exactly the `cancellation: CancellationToken` type needed.
- No change to `ServerHandler::on_cancelled` or any manual `request_id` tracking — the auto-injected per-request token already gets cancelled by `rmcp`'s own dispatch loop.
- The three stop-reason branches (injection-tainted / cancelled / budget-exhausted) must be mutually exclusive in priority order — injection first, matching existing behavior exactly.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: Wire real cancellation into `code`/`code_reply`, fix the stop-reason logic

**Files:**
- Modify: `crates/aivyx-mcp-server/src/server.rs` (`code`, `code_reply`, and their 3 existing tests)
- Modify: `crates/aivyx-mcp-server/src/session.rs` (`run_bounded_turn`'s post-loop logic, plus a new test)

**Interfaces:** none — this is the whole deliverable, no later task consumes it.

- [ ] **Step 1: Add the `cancellation` parameter to `code`, pass it through**

In `crates/aivyx-mcp-server/src/server.rs`, find this exact block:

```rust
    async fn code(
        &self,
        Parameters(params): Parameters<CodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let level = AccessLevel::parse(&params.access_level).map_err(mcp_error)?;
        if !level.at_most(&self.max_access_level) {
            return Err(mcp_error(format!(
                "access_level {:?} exceeds this server's configured ceiling",
                params.access_level
            )));
        }

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = build_session_agent(&self.session_config, level, events_tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut events_rx,
            params.task,
            &self.session_config.cwd,
            self.max_iterations,
            CancellationToken::new(),
        )
        .await;
```

Replace it with:

```rust
    async fn code(
        &self,
        Parameters(params): Parameters<CodeParams>,
        cancellation: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        let level = AccessLevel::parse(&params.access_level).map_err(mcp_error)?;
        if !level.at_most(&self.max_access_level) {
            return Err(mcp_error(format!(
                "access_level {:?} exceeds this server's configured ceiling",
                params.access_level
            )));
        }

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = build_session_agent(&self.session_config, level, events_tx).await;
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut events_rx,
            params.task,
            &self.session_config.cwd,
            self.max_iterations,
            cancellation,
        )
        .await;
```

(`CancellationToken` — a real, live, per-request token — is auto-injected by `rmcp`'s `#[tool]` macro system via its `FromContextPart` extractor; no other wiring is needed. See this plan's design spec's Grounding section for the confirmed mechanism.)

- [ ] **Step 2: Add the same parameter to `code_reply`, pass it through**

In `crates/aivyx-mcp-server/src/server.rs`, find this exact block:

```rust
    async fn code_reply(
        &self,
        Parameters(params): Parameters<CodeReplyParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut session = {
            let mut sessions = self.sessions.lock().await;
            sessions.evict_stale();
            sessions.take(&params.session_id).ok_or_else(|| {
                mcp_error(format!(
                    "no session {:?} -- it may have expired (idle past the configured TTL) or \
                     is already processing another request",
                    params.session_id
                ))
            })?
        }; // lock released here -- the turn below runs with no lock held

        // Reuses the SAME events_rx `code` first created (stored alongside
        // the Agent in StoredSession) -- Agent::run_turn sends to whatever
        // channel it was constructed with, baked in once at
        // build_session_agent time, so a fresh, disconnected channel here
        // would drain nothing and always return empty text.
        let (result, text) = run_bounded_turn(
            &mut session.agent,
            &mut session.events_rx,
            params.message,
            &self.session_config.cwd,
            self.max_iterations,
            CancellationToken::new(),
        )
        .await;
```

Replace it with:

```rust
    async fn code_reply(
        &self,
        Parameters(params): Parameters<CodeReplyParams>,
        cancellation: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        let mut session = {
            let mut sessions = self.sessions.lock().await;
            sessions.evict_stale();
            sessions.take(&params.session_id).ok_or_else(|| {
                mcp_error(format!(
                    "no session {:?} -- it may have expired (idle past the configured TTL) or \
                     is already processing another request",
                    params.session_id
                ))
            })?
        }; // lock released here -- the turn below runs with no lock held

        // Reuses the SAME events_rx `code` first created (stored alongside
        // the Agent in StoredSession) -- Agent::run_turn sends to whatever
        // channel it was constructed with, baked in once at
        // build_session_agent time, so a fresh, disconnected channel here
        // would drain nothing and always return empty text.
        let (result, text) = run_bounded_turn(
            &mut session.agent,
            &mut session.events_rx,
            params.message,
            &self.session_config.cwd,
            self.max_iterations,
            cancellation,
        )
        .await;
```

- [ ] **Step 3: Verify `aivyx-mcp-server` compiles with the new parameters**

Run: `cargo check -p aivyx-mcp-server`
Expected: FAILS to compile at this point — the 3 existing tests in `server.rs` call `.code(...)`/`.code_reply(...)` with only one argument each, and now need a second. This is expected; Step 4 fixes it. If the error is instead about `CancellationToken` not implementing a required trait for macro-based extraction (rather than a plain "wrong number of arguments" mismatch), **STOP and report BLOCKED** with the exact compiler error — this would mean the auto-injection mechanism doesn't work the way this plan's design spec confirmed via source-reading, and needs escalation rather than a workaround.

- [ ] **Step 4: Update the 3 existing tests' call sites to pass a token**

In `crates/aivyx-mcp-server/src/server.rs`, find this exact block:

```rust
    #[tokio::test]
    async fn code_then_code_reply_round_trips_real_conversation_state() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let first = server
            .code(Parameters(CodeParams { task: "start".to_string(), access_level: "plan".to_string() }))
            .await
            .expect("code call should succeed");
        let CallToolResult { content, .. } = first;
        let first_json: serde_json::Value = content[0].raw.as_text().unwrap().text.parse().unwrap_or_else(|_| {
            serde_json::from_str(&content[0].raw.as_text().unwrap().text).unwrap()
        });
        let session_id = first_json["session_id"].as_str().unwrap().to_string();
        assert_eq!(first_json["result"], "first answer");

        let second = server
            .code_reply(Parameters(CodeReplyParams { session_id: session_id.clone(), message: "continue".to_string() }))
            .await
            .expect("code_reply should succeed");
        let second_json: serde_json::Value =
            serde_json::from_str(&second.content[0].raw.as_text().unwrap().text).unwrap();
        assert_eq!(
            second_json["result"], "second answer",
            "code_reply must return its OWN turn's real text, not an empty string from a \
             disconnected events channel"
        );
        assert_eq!(second_json["session_id"], session_id);
    }

    #[tokio::test]
    async fn code_call_above_the_ceiling_is_rejected_before_any_agent_is_built() {
        let server = server_with_ceiling(AccessLevel::Plan);
        let outcome = server
            .code(Parameters(CodeParams { task: "do something".to_string(), access_level: "execute".to_string() }))
            .await;
        assert!(outcome.is_err(), "execute must be rejected when the ceiling is plan");
    }

    #[tokio::test]
    async fn code_reply_against_an_unknown_session_id_fails_clearly() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let outcome = server
            .code_reply(Parameters(CodeReplyParams {
                session_id: "does-not-exist".to_string(),
                message: "hi".to_string(),
            }))
            .await;
        assert!(outcome.is_err());
    }
}
```

Replace it with:

```rust
    #[tokio::test]
    async fn code_then_code_reply_round_trips_real_conversation_state() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let first = server
            .code(
                Parameters(CodeParams { task: "start".to_string(), access_level: "plan".to_string() }),
                CancellationToken::new(),
            )
            .await
            .expect("code call should succeed");
        let CallToolResult { content, .. } = first;
        let first_json: serde_json::Value = content[0].raw.as_text().unwrap().text.parse().unwrap_or_else(|_| {
            serde_json::from_str(&content[0].raw.as_text().unwrap().text).unwrap()
        });
        let session_id = first_json["session_id"].as_str().unwrap().to_string();
        assert_eq!(first_json["result"], "first answer");

        let second = server
            .code_reply(
                Parameters(CodeReplyParams { session_id: session_id.clone(), message: "continue".to_string() }),
                CancellationToken::new(),
            )
            .await
            .expect("code_reply should succeed");
        let second_json: serde_json::Value =
            serde_json::from_str(&second.content[0].raw.as_text().unwrap().text).unwrap();
        assert_eq!(
            second_json["result"], "second answer",
            "code_reply must return its OWN turn's real text, not an empty string from a \
             disconnected events channel"
        );
        assert_eq!(second_json["session_id"], session_id);
    }

    #[tokio::test]
    async fn code_call_above_the_ceiling_is_rejected_before_any_agent_is_built() {
        let server = server_with_ceiling(AccessLevel::Plan);
        let outcome = server
            .code(
                Parameters(CodeParams { task: "do something".to_string(), access_level: "execute".to_string() }),
                CancellationToken::new(),
            )
            .await;
        assert!(outcome.is_err(), "execute must be rejected when the ceiling is plan");
    }

    #[tokio::test]
    async fn code_reply_against_an_unknown_session_id_fails_clearly() {
        let server = server_with_ceiling(AccessLevel::Execute);
        let outcome = server
            .code_reply(
                Parameters(CodeReplyParams {
                    session_id: "does-not-exist".to_string(),
                    message: "hi".to_string(),
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(outcome.is_err());
    }
}
```

- [ ] **Step 5: Verify `aivyx-mcp-server` now compiles and its existing tests pass**

Run: `cargo test -p aivyx-mcp-server code_then_code_reply code_call_above code_reply_against -- --nocapture`
Expected: all 3 tests pass (assertions unchanged — only the call sites gained a second argument).

- [ ] **Step 6: Fix `run_bounded_turn`'s post-loop stop-reason logic**

In `crates/aivyx-mcp-server/src/session.rs`, find this exact block:

```rust
    let paused = result.is_ok() && agent.last_turn_paused();
    let injection_tainted = agent.injection_taint().current().is_some();

    if paused && injection_tainted {
        accumulated.push_str(
            "\n\n(session stopped: a tool result was flagged as a possible prompt injection -- \
             the above is its best-effort partial result; further mutating tool calls will keep \
             being denied for the rest of this session.)",
        );
    } else if paused {
        accumulated.push_str(
            "\n\n(session stopped: reached its iteration budget before finishing -- the above is its best-effort partial result.)",
```

Replace it with:

```rust
    let would_have_continued = result.is_ok() && agent.last_turn_paused();
    let injection_tainted = agent.injection_taint().current().is_some();
    let cancelled = cancellation.is_cancelled();

    if would_have_continued && injection_tainted {
        accumulated.push_str(
            "\n\n(session stopped: a tool result was flagged as a possible prompt injection -- \
             the above is its best-effort partial result; further mutating tool calls will keep \
             being denied for the rest of this session.)",
        );
    } else if would_have_continued && cancelled {
        accumulated.push_str(
            "\n\n(session stopped: cancelled by the client -- the above is its best-effort partial result.)",
        );
    } else if would_have_continued && iterations_used >= max_iterations {
        accumulated.push_str(
            "\n\n(session stopped: reached its iteration budget before finishing -- the above is its best-effort partial result.)",
```

Note: this changes the budget-exhausted branch's *condition* (from bare `would_have_continued`/`paused` to `would_have_continued && iterations_used >= max_iterations`) but leaves the message text on that branch, and everything after it (the closing `);` and any remaining lines), completely unchanged — only the `if`/`else if` conditions above it are edited. Every reachable post-loop state is covered by these three conditions: exhaustively tracing the outer loop's own continue-guard (`result.is_ok() && agent.last_turn_paused() && iterations_used < max_iterations && !cancellation.is_cancelled() && agent.injection_taint().current().is_none()`) confirms `would_have_continued == true` with none of `injection_tainted`/`cancelled`/`iterations_used >= max_iterations` true is unreachable — the loop would have continued instead of exiting in that exact state.

- [ ] **Step 7: Add a regression test distinguishing cancellation from budget exhaustion**

In `crates/aivyx-mcp-server/src/session.rs`'s test module, find the existing test `run_bounded_turn_appends_a_cutoff_notice_on_budget_exhaustion` (it defines a `LoopingBackend` struct that always returns a tool call to a nonexistent tool, causing the agent to pause every round). Immediately after that test's closing `}`, add:

```rust
    #[tokio::test]
    async fn run_bounded_turn_distinguishes_cancellation_from_budget_exhaustion() {
        // Reuses this file's own LoopingBackend (defined in the test above)
        // -- causes agent.last_turn_paused() to be true every round, so
        // `would_have_continued` is true regardless of why the loop
        // actually stops. A pre-cancelled token with a generous
        // max_iterations (10) proves the loop stops on iteration 1
        // specifically because of cancellation, not because the budget
        // was exhausted (iterations_used == 1, far below max_iterations).
        let mut cfg = config(full_registry());
        cfg.llm = std::sync::Arc::new(LoopingBackend);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut agent = build_session_agent(&cfg, AccessLevel::Plan, tx).await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (result, text) = run_bounded_turn(
            &mut agent,
            &mut rx,
            "do the thing".to_string(),
            &std::env::temp_dir(),
            10,
            cancellation,
        )
        .await;
        assert!(result.is_ok());
        assert!(
            text.contains("session stopped: cancelled by the client"),
            "a pre-cancelled token must produce the cancellation notice, not the budget-exhausted \
             one, even though the agent itself would have kept going: got {text:?}"
        );
        assert!(
            !text.contains("reached its iteration budget"),
            "must not also claim budget exhaustion when the real reason was cancellation: got {text:?}"
        );
    }
```

- [ ] **Step 8: Run the affected tests**

Run: `cargo test -p aivyx-mcp-server run_bounded_turn -- --nocapture`
Expected: all `run_bounded_turn`-prefixed tests pass, including the new `run_bounded_turn_distinguishes_cancellation_from_budget_exhaustion` and the pre-existing `run_bounded_turn_appends_a_cutoff_notice_on_budget_exhaustion` (still passing — a genuinely budget-exhausted run, no cancellation involved, must still produce the original message).

- [ ] **Step 9: Format the exact files touched, and build/test/lint the full workspace**

Run:
```bash
rustfmt --edition 2024 crates/aivyx-mcp-server/src/server.rs
rustfmt --edition 2024 crates/aivyx-mcp-server/src/session.rs
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```
Expected: `rustfmt` applies only whitespace consistent with the code above (or reports no diff needed); `cargo build`/`cargo test` succeed with zero failures; `cargo clippy` reports zero warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/aivyx-mcp-server/src/server.rs crates/aivyx-mcp-server/src/session.rs
git commit -m "fix: wire real MCP cancellation into code/code_reply, fix budget-vs-cancelled mislabeling"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (`code`/`code_reply` gain a `CancellationToken` parameter, auto-injected by `rmcp`, threaded into `run_bounded_turn`) → Steps 1-2. Decision 2 (the 3-way, exhaustively-ordered stop-reason check) → Step 6. Decision 3 (cancellation gets its own distinct trailing notice) → Step 6's new `else if would_have_continued && cancelled` branch and Step 7's test. "What this spec does not decide" items are all genuinely untouched: no `on_cancelled` override, no `max_iterations` config change, no `aivyx-core` change, no KV-cache work.

**Global Constraints deviation:** none — `run_bounded_turn`'s signature is unchanged, no manual `request_id` tracking added, the three branches stay mutually exclusive with injection checked first (unchanged from the original code's own precedence), only file-scoped `rustfmt` is used.

**Placeholder scan:** no TBD/TODO; every step shows complete, real code (full function signatures, full test bodies); no "similar to Task N" references (single-task plan).

**Type/interface consistency check:** `run_bounded_turn`'s signature (`cancellation: CancellationToken` as its last parameter) is unchanged between Steps 1-2's call sites and its own existing definition in `session.rs` — the plan only changes what value is passed at the call site (`cancellation` instead of `CancellationToken::new()`), never the function's own type signature. Step 7's new test reuses `LoopingBackend`, `config`, `full_registry`, `build_session_agent`, and `mpsc` — all already defined/imported in `session.rs`'s existing test module (confirmed by the pre-existing `run_bounded_turn_appends_a_cutoff_notice_on_budget_exhaustion` test using the identical set), so no new imports are needed for Step 7.
