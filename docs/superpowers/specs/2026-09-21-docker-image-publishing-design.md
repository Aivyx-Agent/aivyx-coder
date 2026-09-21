# Docker Image Publishing Design

## Context

`docs/superpowers/specs/2026-09-21-docker-container-distribution-design.md`
shipped a real, working `Dockerfile` (merged `main` commit `129ad2f`) but
explicitly deferred publishing anywhere: "Publishing the image anywhere
(Docker Hub, GHCR, wiring it into the existing
`.github/workflows/release.yml` pipeline) — separate, later scope; this
spec only covers `docker build` working locally from a checked-out
repo." This spec is that deferred piece: making `docker pull` actually
work for someone who isn't building from source.

## Grounding

Read directly, not assumed:

- `Dockerfile` (repo root, shipped 2026-09-21) — a self-contained,
  multi-stage build that compiles from source; needs nothing the
  existing `release.yml` binary-build job produces, so a new publish
  job has no dependency on that job.
- `.github/workflows/release.yml` — the existing `contents: write`
  permission pattern this spec's new `packages: write` permission
  mirrors; confirmed this file's binary-build and GitHub-Release jobs
  assume a real version tag (`GITHUB_REF_NAME` is embedded into release
  artifact names and the GitHub Release itself) — a manual
  `workflow_dispatch` run without a real tag would produce nonsensical
  output from those jobs, which is why this spec's new workflow is a
  separate file rather than added job(s) inside this one.
- Confirmed with the project owner: GHCR (GitHub Container Registry)
  over Docker Hub — zero new accounts/secrets, since the repo is
  already on GitHub and CI can publish using the workflow's own
  built-in `GITHUB_TOKEN`.
- Confirmed with the project owner: `linux/amd64` only this phase,
  matching exactly what the `Dockerfile` already builds and had its
  Landlock/seccomp behavior verified against — multi-arch stays
  deferred, separate, later scope (as the prior spec already stated).
- Confirmed with the project owner: a new, separate
  `.github/workflows/docker-publish.yml` file, not new jobs inside
  `release.yml` — keeps the existing, already-proven release workflow
  completely untouched, and allows a `workflow_dispatch` trigger for
  real, safe manual testing without needing to fake real-release
  conditions.

## Decisions

**1. A new `.github/workflows/docker-publish.yml`**, triggered on
`push: tags: 'v*'` (the same trigger `release.yml` already uses — one
`git tag` push produces the binary releases and the container image
together) **plus `workflow_dispatch`** for manual, on-demand testing
without cutting a real release tag. `permissions: packages: write` is
added (alongside `contents: read`, the minimum this workflow actually
needs — it doesn't touch repo contents beyond checking them out) —
mirrors `release.yml`'s own minimal-permission convention.

**2. Image tagging scheme, branching on trigger type**:
- On a real tag push (`github.ref_type == 'tag'`): push
  `ghcr.io/aivyx-agent/aivyx-coder:<tag>` (the exact tag, e.g. `v0.1.1`,
  no stripping/reformatting) and
  `ghcr.io/aivyx-agent/aivyx-coder:latest`.
- On a manual `workflow_dispatch` run (no real tag present): push
  `ghcr.io/aivyx-agent/aivyx-coder:dev-<short-sha>` only — deliberately
  never `latest` and never a version-shaped tag, so a manual test run
  can never be mistaken for, or accidentally overwrite, a real release
  artifact.

**3. Build and push using plain `docker` CLI commands** (`docker login
ghcr.io -u $GITHUB_ACTOR -p ${{ secrets.GITHUB_TOKEN }}`, `docker
build`, `docker push`), matching the same tooling this project's own
Docker container distribution spec already used and verified directly
— not introducing `docker/build-push-action` or `docker/login-action`
as new, unverified third-party Action dependencies for what plain CLI
commands already do correctly.

**4. Verification is real**: trigger the new workflow for real via `gh
workflow run docker-publish.yml`, confirm the run succeeds, then `docker
pull ghcr.io/aivyx-agent/aivyx-coder:dev-<short-sha>` for real from a
local environment to confirm the image is genuinely public and
pullable — not just "the workflow reported success." Confirm the GHCR
package's visibility is public (GitHub's package settings), since
default visibility inheritance for a freshly created package isn't
something to assume without checking.

## What this spec does not decide

- `linux/arm64` / multi-arch images — deliberately deferred, per the
  project owner's explicit choice this session.
- Docker Hub as an additional or alternative registry — GHCR only, this
  phase.
- Bundling Docker Model Runner — a separate, later idea (already
  identified as a distinct next step after this one).
- Any change to `release.yml` itself — untouched by this spec.
- Automatic cleanup/retention policy for old `dev-<sha>` test tags in
  GHCR — not addressed; if this becomes a real clutter problem later,
  it's separate, later scope.
