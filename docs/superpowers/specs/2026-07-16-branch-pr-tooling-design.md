# Branch/PR Tooling — Design

**Status:** Approved by user 2026-07-16.

## Problem

Branch creation/switching, pushing, and PR creation are currently only
reachable through `run_shell` (arbitrary shell execution). This works, but
gives the model no structured, purpose-built surface the way `git_read`/
`git_commit` already do for inspection and committing — the same gap
`web_fetch`/`web_search` closed for HTTP access instead of leaving it to
`run_shell` alone. This is explicitly the lowest-priority item from the
original tool/capability audit (framed there as "already reachable via
`run_shell`" — an ergonomics improvement, not a missing capability), so
scope stays narrow and every new tool follows `git_commit`'s already-proven
shape rather than inventing anything new.

## Scope

Full branch + push + PR creation, in one phase (not phased further):

- Branch listing added as a fourth mode on the existing `git_read` tool.
- **`git_branch`** (new): create or switch branches.
- **`git_push`** (new): push the current branch.
- **`git_pr`** (new): open a pull request via the `gh` CLI.

## Permission tier

All three new tools use `ActionKind::Execute` with `PermissionTarget::Command`
— the exact tier `git_commit` already uses. No new `ActionKind` is
introduced: unlike MCP tool calls (arbitrary, unverifiable third-party
code), these are structured invocations of the same trusted `git`/`gh`
CLIs this project already shells out to for `git_commit`, so the existing
tier's reasoning applies unchanged. All three are always confirm-gated
(never auto-allowed) and use a fixed-argv-shape internally — model input is
validated and passed as discrete arguments, never interpolated into a shell
string, matching `git_read`/`git_commit`'s existing discipline.

`git_read`'s new `branches` mode stays `ActionKind::Read` (auto-allow) on
the existing tool, since listing is read-only — mirrors why `git_read` and
`git_commit` are already two separate tools rather than one (Plan Mode's
tool filtering is static per-tool via `mutates_outside_session`, so
inspection must remain available while planning, and mutation must not).

## Tool shapes

**`git_read`'s new `branches` mode** (extends the existing `GitReadMode`
enum): runs `git branch -vv` (local branches with upstream tracking info)
plus current-branch indicator. No new arguments beyond the existing `mode`
selector.

**`git_branch(mode, name, base?)`**:
- `mode: "create"` runs `git checkout -b <name> [<base>]` — creation
  implies switching to the new branch in the same call, matching how
  `git_commit` already does "stage + commit" as one action rather than two
  separate tool calls. `base` is optional (omitted ⇒ branch from the
  current `HEAD`, git's own default for `checkout -b`).
- `mode: "switch"` runs `git checkout <name>` for an existing branch.
- Permission preview shows current branch → target branch name (and base,
  for create). A switch that git itself refuses (e.g. uncommitted changes
  that would be overwritten) surfaces git's own clear stderr as the tool
  error — no special-casing needed, and this project's automatic
  pre-mutation checkpoints already provide a rewind safety net regardless.

**`git_push(remote?)`**: pushes the current branch to `remote` (default
`"origin"`). Runs `git push -u <remote> <branch>` unconditionally — the
`-u` (set-upstream) flag is a no-op on subsequent pushes once tracking is
already configured, so one code path handles both first-push and later
pushes. **No `--force`/`--force-with-lease` support at all** — deliberately
excluded from the tool's argv construction entirely (not merely
undocumented), since force-push is a materially more destructive operation
class this narrow phase doesn't take on. Permission preview shows the
commits that would be pushed (`git log <remote>/<branch>..HEAD --oneline`),
gracefully empty/omitted if there's no upstream yet (first push).

**`git_pr(title, body?, base?, draft?)`**: requires the current branch
already pushed with a remote tracking branch — checked deterministically
*before* invoking `gh` at all, via `git rev-parse --abbrev-ref --symbolic-
full-name @{u}` (fails with a known, git-owned error if no upstream is
configured). If that preflight check fails, `git_pr` returns a clear error
directing the model to call `git_push` first, without ever invoking `gh`
(deliberately not implemented by parsing `gh pr create`'s own stderr for a
particular phrase, since that text isn't a stable contract across `gh`
versions). Once the preflight passes, runs
`gh pr create --title <title> [--body <body>] [--base <base>] [--draft]`,
fixed argv only — this keeps each tool single-purpose: `git_pr` never has a
hidden network side effect beyond talking to the GitHub API through `gh`.
On success, returns the created PR's URL as `ToolOutput::Ok`.

`gh` missing from `PATH`, or present but not authenticated (`gh auth
status` failing), surfaces as a clear tool-result error naming the fix
("install the GitHub CLI" / "run `gh auth login`") — mirroring the LSP
client's missing-`rust-analyzer` precedent. `git_pr` is **always
registered**, no new config flag: a missing/unauthenticated `gh` is an
environmental accident, not a deliberate off-switch, the same reasoning
that keeps `go_to_definition`/`find_references` always registered
regardless of whether `rust-analyzer` happens to be installed.

## Testing strategy

Following this project's established "test doubles / real local git repos,
never a real network call" convention (`git_commit`'s own test suite
already does exactly this via `crate::checkpoint::test_support::{git,
init_repo}`):

- `git_read`'s new `branches` mode: unit tests against a real temporary git
  repo with multiple local branches, asserting the current branch is
  identifiable in the output.
- `git_branch`: unit tests for `create` (new branch exists and is checked
  out, optionally from a given `base`), `switch` (existing branch becomes
  current), and the permission-request preview content, all against real
  temporary repos — no real network involved since these are local-only
  git operations.
- `git_push`: since a real push needs a real remote, tests exercise `git_push`
  against a **local bare repository** added as the test repo's `origin`
  remote (a real `git remote add`/`git push` round-trip entirely on local
  disk, no network) — proving the tool's argv construction and upstream-
  setting behavior work end-to-end without needing GitHub or any external
  service. Cover: first push (sets upstream), subsequent push (upstream
  already set, still succeeds), and the preview's commit-list content.
- `git_pr`: unit tests for argv construction (title/body/base/draft
  combinations) and the "no upstream configured" clear-error path, using a
  fake `gh` stand-in on `PATH` (a tiny script test fixture, matching how
  this project already avoids real subprocess dependencies in tests
  elsewhere) rather than a real `gh` invocation or a real GitHub PR — this
  project has no live GitHub account/repo to test against safely, so no
  live E2E through the real `gh pr create` command is in scope; the live
  E2E for this phase covers `git_branch`/`git_push` only, through a real
  local-bare-repo remote.

## Non-Goals

- `--force`/`--force-with-lease` push support.
- Branch deletion, renaming, or merging — this phase covers only
  create/switch/list/push/PR-create.
- PR review, PR merge, PR comment, or any other `gh pr`/`gh api` surface
  beyond `gh pr create`.
- Any search backend or API other than `gh` for PR creation — no direct
  GitHub REST/GraphQL API calls, matching this project's "shell out to the
  user's own trusted, already-authenticated CLI" posture (the same reason
  `git_commit` shells out to the user's real `git` rather than
  reimplementing git plumbing).
- A live E2E through a real `gh pr create` call against a real GitHub
  repository — no safe, repeatable way to do this without a live account
  and a disposable public repo; `git_pr`'s live-through-the-real-binary
  verification is bounded by what a fake `gh` stand-in can prove, per the
  Testing Strategy section.
