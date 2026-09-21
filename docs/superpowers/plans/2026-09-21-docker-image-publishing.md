# Docker Image Publishing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Task 2 pushes a branch to `origin` and triggers a real, visible GitHub Actions run — a real external action.** If using subagent-driven-development, the controller should get the project owner's explicit go-ahead immediately before Task 2's push step, then execute Task 2 itself rather than delegating it to an autonomous subagent (mirroring how the earlier ACP registry submission plan handled its own external-action task).

**Goal:** A working, tested `docker-publish.yml` GitHub Actions workflow that publishes `aivyx-coder`'s Docker image to GHCR.

**Architecture:** Task 1 writes the new, self-contained workflow file (no changes to the existing `release.yml`). Task 2 is the real, human-gated verification: push the branch so the workflow file exists on a real ref, trigger it via `workflow_dispatch`, confirm the run succeeds, then confirm the image is genuinely pullable and the GHCR package is public.

**Tech Stack:** GitHub Actions YAML, `docker`/`gh` CLI.

## Global Constraints

- A new, separate `.github/workflows/docker-publish.yml` — the existing `release.yml` is not modified at all.
- Triggers: `push: tags: ['v*']` (same as `release.yml`) plus `workflow_dispatch` for manual testing.
- Registry: GHCR only (`ghcr.io/aivyx-agent/aivyx-coder`, hardcoded lowercase — GHCR requires lowercase image paths, and `github.repository_owner`'s real case (`Aivyx-Agent`) must not be interpolated directly into the image reference).
- Platform: `linux/amd64` only — no multi-arch this phase.
- Tagging: a real tag push produces `<tag>` (exact, e.g. `v0.1.1`) and `latest`; a `workflow_dispatch` run produces `dev-<short-sha>` only, never `latest`, never a version-shaped tag.
- Auth: the workflow's own built-in `GITHUB_TOKEN` (needs `permissions: packages: write` added; `contents: read` is sufficient, not `write` — this workflow never modifies repo contents).
- Build/push via plain `docker` CLI commands, not `docker/build-push-action`/`docker/login-action`.
- Task 2's push-and-dispatch step requires the project owner's explicit confirmation immediately before it runs, per this plan's own header note.

---

### Task 1: `docker-publish.yml`

**Files:**
- Create: `.github/workflows/docker-publish.yml`

**Interfaces:** none — Task 2 consumes this file by its real behavior when triggered, not by any code interface.

- [ ] **Step 1: Write the workflow file**

Create `.github/workflows/docker-publish.yml`:

```yaml
name: Docker Publish

on:
  push:
    tags:
      - 'v*'
  workflow_dispatch:

permissions:
  contents: read
  packages: write

jobs:
  docker-publish:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Determine image tags
        id: tags
        run: |
          if [ "${{ github.ref_type }}" = "tag" ]; then
            echo "primary_tag=${{ github.ref_name }}" >> "$GITHUB_OUTPUT"
            echo "push_latest=true" >> "$GITHUB_OUTPUT"
          else
            SHORT_SHA=$(echo "${{ github.sha }}" | cut -c1-7)
            echo "primary_tag=dev-${SHORT_SHA}" >> "$GITHUB_OUTPUT"
            echo "push_latest=false" >> "$GITHUB_OUTPUT"
          fi

      - name: Log in to GHCR
        run: echo "${{ secrets.GITHUB_TOKEN }}" | docker login ghcr.io -u "${{ github.actor }}" --password-stdin

      - name: Build image
        run: docker build -t "ghcr.io/aivyx-agent/aivyx-coder:${{ steps.tags.outputs.primary_tag }}" .

      - name: Push primary tag
        run: docker push "ghcr.io/aivyx-agent/aivyx-coder:${{ steps.tags.outputs.primary_tag }}"

      - name: Tag and push latest
        if: steps.tags.outputs.push_latest == 'true'
        run: |
          docker tag "ghcr.io/aivyx-agent/aivyx-coder:${{ steps.tags.outputs.primary_tag }}" "ghcr.io/aivyx-agent/aivyx-coder:latest"
          docker push "ghcr.io/aivyx-agent/aivyx-coder:latest"
```

- [ ] **Step 2: Validate the YAML is well-formed**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/docker-publish.yml'))" && echo "valid YAML"`
Expected: `valid YAML`

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/docker-publish.yml
git commit -m "feat: add Docker image publishing workflow (GHCR)"
```

---

### Task 2: Push, trigger for real, and verify

**⚠️ This task pushes a branch to `origin` and triggers a real, visible GitHub Actions run. Do not dispatch this task to an autonomous subagent. Do not begin any step until the project owner has explicitly confirmed, immediately before execution.**

**Files:** none — this task's changes are entirely external (a GitHub Actions run, a GHCR package), not local file changes.

**Interfaces:**
- Consumes: `.github/workflows/docker-publish.yml` (Task 1's committed file).
- Produces: a real, running, verified GHCR package at `ghcr.io/aivyx-agent/aivyx-coder`, confirmed public and pullable.

- [ ] **Step 1: Confirm with the project owner before proceeding**

State plainly what's about to happen: the current branch will be pushed to `origin` (making the new workflow file visible on GitHub), then the workflow will be triggered via `workflow_dispatch` — a real, visible GitHub Actions run, and a real GHCR package will be created under the `Aivyx-Agent` org. Do not continue to Step 2 without an explicit yes.

- [ ] **Step 2: Push the branch**

Run: `git push -u origin HEAD`
Expected: the branch pushes successfully. (If working directly on `main` inside a worktree per this project's usual workflow, this is the same `main` branch the rest of this initiative already pushes to — no special branch name handling needed.)

- [ ] **Step 3: Trigger the workflow and wait for it to finish**

Run:
```bash
gh workflow run docker-publish.yml
sleep 8
RUN_ID=$(gh run list --workflow=docker-publish.yml --limit 1 --json databaseId --jq '.[0].databaseId')
echo "Watching run $RUN_ID"
gh run watch "$RUN_ID" --exit-status
```
Expected: `gh run watch` blocks until the run finishes and exits `0` — confirming every step (login, build, push) succeeded for real. If it exits non-zero, **STOP and report BLOCKED** with `gh run view "$RUN_ID" --log-failed` output — do not proceed to Step 4 with a failed publish.

- [ ] **Step 4: Determine the real pushed tag and confirm the image is genuinely pullable**

Run:
```bash
SHORT_SHA=$(git rev-parse --short=7 HEAD)
docker pull "ghcr.io/aivyx-agent/aivyx-coder:dev-${SHORT_SHA}"
docker run --rm "ghcr.io/aivyx-agent/aivyx-coder:dev-${SHORT_SHA}" --help
```
Expected: `docker pull` succeeds (downloading the real image just published, not a locally-cached one — if a local image with the same tag already exists from local testing, run `docker rmi "ghcr.io/aivyx-agent/aivyx-coder:dev-${SHORT_SHA}"` first to force a genuine remote pull before this step), and `--help` prints real output with exit `0`.

- [ ] **Step 5: Confirm the GHCR package's visibility is public**

Run: `gh api "/orgs/Aivyx-Agent/packages/container/aivyx-coder" --jq '.visibility'`
Expected: `public`. If it reports `private`, **STOP and report BLOCKED** rather than silently leaving a private package that `docker pull` (Step 4) only succeeded against because the local environment happens to be authenticated — a real end user would not be able to pull a private package. Report the exact finding; do not attempt to change the visibility setting without the project owner's explicit go-ahead, since changing a package's visibility is itself a real, external, visible action.

- [ ] **Step 6: Report back**

State plainly: the workflow succeeded, the real image reference (`ghcr.io/aivyx-agent/aivyx-coder:dev-<short-sha>`) that was verified pullable, and the confirmed `public` visibility. No further action needed — the next real version tag push will automatically produce a real `<tag>`/`latest`-tagged publish using this same, now-verified mechanism.

---

## Self-Review Notes

**Spec coverage:** Decision 1 (new, separate workflow file, both triggers, `packages: write`) → Task 1 Step 1. Decision 2 (tagging scheme, branching on trigger type) → Task 1 Step 1's `Determine image tags` step. Decision 3 (plain `docker` CLI, no third-party Actions) → Task 1 Step 1. Decision 4 (real verification: trigger for real, confirm real pull, confirm visibility) → Task 2 Steps 3-5. "What this spec does not decide" items are all genuinely untouched: no `release.yml` changes, no multi-arch, no Docker Hub, no DMR bundling, no retention policy.

**Global Constraints deviation:** none — this plan implements the spec's decisions directly.

**Placeholder scan:** no TBD/TODO; every step shows complete, real content (the full workflow YAML, real `gh`/`docker` commands with real expected exit-code/output behavior); no "similar to Task N" references.

**Type/interface consistency check:** the image reference `ghcr.io/aivyx-agent/aivyx-coder` and the tag-naming scheme (`dev-<short-sha>` for dispatch, `<tag>`/`latest` for real releases) are used identically in Task 1's workflow file and Task 2's verification commands — same lowercase org/repo path, same short-sha derivation logic (`cut -c1-7` in the workflow vs. `git rev-parse --short=7` in Task 2's local verification — both produce a 7-character short SHA from the same commit, since Task 2 runs immediately after the same push the workflow itself triggered on).
