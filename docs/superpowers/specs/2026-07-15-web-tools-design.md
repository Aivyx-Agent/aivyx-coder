# Web Search & Fetch Tools — Design

**Status:** Approved by user 2026-07-15. Resolves the Medium-priority
web-network values question from the tool/capability audit.

## Values decision

"Local-only" describes where LLM inference happens (Ollama/vLLM/llama.cpp,
llama.cpp prioritized) — it does not mean aivyx-coder itself is
network-isolated. The user has explicitly confirmed the agent should have
complete network capabilities even though it only ever talks to a local
model. Network-reaching tools are therefore in scope; this design adds the
first two.

This resolves the audit's open question in the affirmative: build the
tools, not "stay strictly local-only." Cloud LLM provider support remains
separately and explicitly deferred (per prior, unrelated decisions) — this
design has no bearing on that question.

## Problem

Every comparable coding agent (Claude Code's `WebFetch`/`WebSearch`, and
equivalents elsewhere) can look up documentation, error messages, or API
references the model doesn't already know. aivyx-coder has no tool whose
purpose is reaching the network — `run_shell`/`run_command` can incidentally
reach it (a user-approved `curl`, `cargo build` hitting crates.io), but
that's gated arbitrary-command execution, not a purpose-built tool with its
own trust story.

## Scope

Two new tools, both required (not a phased "fetch now, search later" split):

- **`web_fetch(url: String)`** — fetches a URL, converts HTML to readable
  text/markdown, returns it.
- **`web_search(query: String)`** — queries a configured SearXNG instance,
  returns ranked results.

SearXNG (open-source, self-hostable metasearch, no API key required) is the
search backend — the only option here with no external paid/free-tier
signup, matching this project's avoid-external-service-dependencies
posture. A user points `[web] search_base_url` at any instance they trust,
public or self-hosted.

## Architecture

Both tools live in `crates/aivyx-tools/src/tools/` (`web_fetch.rs`,
`web_search.rs`), following the established `Tool` trait pattern — no new
crate. Two new dependencies for `aivyx-tools`:

- **`reqwest`** — already a workspace dependency via `aivyx-llm` (v0.13.4,
  `rustls` backend, no default features) for talking to the local LLM
  backend's HTTP API. Reused as-is for both new tools, not reintroduced
  with different settings.
- **`html2text`** — new. Real-world HTML (nav bars, scripts, malformed
  markup, ads) is messy enough that hand-rolling extraction would be a
  correctness trap, not a principled minimalism win — unlike this
  project's deliberately hand-rolled frontmatter parser, which only ever
  needs to handle one simple, project-authored format. This is a
  deliberate, justified exception to the "hand-roll over heavy dependency"
  convention, not a quiet abandonment of it.

## Permission tier

Both tools use `ActionKind::Read`, auto-allowed exactly like `read_file`/
`grep`/`go_to_definition` — no confirmation modal. Fetched/search-result
content is already covered by the existing system-prompt convention that
tool output is untrusted data, never instructions.

## SSRF protection (`web_fetch`)

Since there is no per-call human confirmation gating this tool, it carries
its own pre-flight safety net: before connecting, it resolves the target
URL's host to its IP address(es) and refuses to proceed if any resolved
address falls in a loopback, private, or link-local range:

- IPv4: `127.0.0.0/8`, `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
  `169.254.0.0/16`.
- IPv6: `::1`, `fc00::/7` (unique local), `fe80::/10` (link-local).

This is a **new mechanism internal to the tool** — `ConfirmationGate`'s
existing `deny_paths` matches filesystem path strings and has no
applicability to URLs or resolved IP addresses, so it cannot be reused
here. Refusal returns a clear, actionable tool-result error naming the
resolved address and the range it matched, not a silent failure.
`[web] allow_private_targets = true` disables this check entirely, for
users who deliberately want to reach an internal service (e.g. a local
dev server or self-hosted SearXNG instance running on the same host — note
this interacts with the config's own `search_base_url`, addressed below).

`web_search` has no equivalent check: it only ever talks to the one
explicitly-configured `search_base_url`, which the user already trusts by
having configured it (including if that's a `localhost`-hosted SearXNG
instance — `allow_private_targets` does not need to be set just to let
`web_search` reach a local SearXNG instance, since `web_search` doesn't run
`web_fetch`'s IP-range check at all).

## Configuration

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSettings {
    pub enabled: bool,
    pub search_base_url: Option<String>,
    pub max_search_results: u32,
    pub fetch_timeout_secs: u64,
    pub allow_private_targets: bool,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            search_base_url: None,
            max_search_results: 10,
            fetch_timeout_secs: 30,
            allow_private_targets: false,
        }
    }
}
```

`enabled` gates registration of **both** tools onto `ToolRegistry` — when
`false`, neither tool is offered to the model at all. This mirrors
`run_command`'s "only registered when configured" pattern (a deliberate
capability toggle) rather than LSP's "always register, fail clearly on
first call" pattern (which exists specifically because a missing
`rust-analyzer` binary is an environmental accident, not a choice). Network
access being off is always a deliberate choice, so hiding the tools
entirely when off is the right default here.

`enabled` defaults to `true` — `web_fetch` works with zero configuration
beyond that; `web_search` additionally needs `search_base_url` set. If
`web_search` is called while `enabled=true` but `search_base_url` is
unset, it returns a clear explanatory tool-result message (how to
configure it) rather than being silently absent from the tool list —
mirroring `/council`'s unconfigured-but-invoked behavior, not repo-map's
silent-degradation behavior, since `web_search` being *offered* but
*non-functional* needs to explain itself the moment it's actually called.

## Tool shapes

- **`web_fetch(url: String) -> ToolOutput::Ok(String)`**: fetches, converts
  to text/markdown via `html2text`, head-truncated (not tail — an
  article's useful content is at the top, unlike command output, matching
  `grep`/`glob`'s own head-truncation convention) at a fixed size cap with
  a `"... N more bytes truncated ..."`-style notice when cut. A request
  exceeding `fetch_timeout_secs`, a DNS failure, a non-2xx HTTP status, or
  the SSRF check failing are all reported as `Err(ToolError::ExecutionFailed(...))`
  with a message naming what went wrong — never a silent empty result.
- **`web_search(query: String) -> ToolOutput::Ok(String)`**: queries
  SearXNG's JSON API (`GET {search_base_url}/search?q={query}&format=json`),
  returns up to `max_search_results` lines formatted `title | url | snippet`,
  one per line — matching `grep`'s one-line-per-match convention. Zero
  results is `Ok` output stating nothing was found (not an error), matching
  `grep`'s own empty-match handling.

## Testing strategy

- Unit tests for the SSRF range check: every blocked range (each IPv4/IPv6
  range above) correctly refused, at least one explicit public-IP case
  correctly allowed, and `allow_private_targets = true` correctly bypassing
  the check.
- Unit tests for HTML→text conversion (a representative fixture page →
  expected readable text), head-truncation behavior at the size cap
  boundary, and URL parsing edge cases (missing scheme, malformed URL).
- Unit tests for `web_search`'s SearXNG JSON response parsing (multiple
  results, zero results, a malformed/unexpected response shape).
- Unit test for `web_search`'s unconfigured-`search_base_url` explanation
  path, and for both tools' absence from the registry when `enabled=false`.
- All of the above run against an in-process mock HTTP server — never a
  real network call — matching this project's established "test doubles
  over real network calls" convention (the same approach LSP's fake
  JSON-RPC server already uses for `rust-analyzer`).
- One live E2E through the real binary: a real `web_fetch` against a real
  public URL, and a real `web_search` against a real SearXNG instance
  (public or self-hosted, whichever is available at implementation time) —
  matching this project's strict live-verification precedent for every
  phase shipped so far.

## Non-goals

- No other search backends (Brave/Google/Bing/etc.) — SearXNG only, for
  the stated no-external-API-key reason. A future phase could add a
  pluggable backend if real demand emerges, matching this project's
  general "ship the narrow version first" pattern.
- No JavaScript rendering for `web_fetch` — a plain HTTP GET plus HTML
  extraction, not a headless browser. Pages that require JS to render
  their content will fetch as sparse/empty, a known limitation, not a bug
  to work around in this phase.
- No caching of fetch/search results across turns or sessions.
- No change to `ConfirmationGate`'s `deny_paths` mechanism — the new
  SSRF check is entirely separate, internal to `web_fetch`.
