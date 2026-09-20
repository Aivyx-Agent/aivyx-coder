# ACP Registry Submission Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Task 2 is an external, irreversible action (forking a third-party repo and opening a real PR) and must NOT be dispatched to an autonomous subagent** — it requires the project owner's explicit go-ahead immediately before execution, per this plan's own Global Constraints. If using subagent-driven-development, the controller should pause after Task 1's review and get that confirmation directly, then execute Task 2's steps itself rather than delegating them.

**Goal:** Author, validate, and submit `aivyx-coder`'s ACP Registry entry (`agent.json` + `icon.svg`) to `agentclientprotocol/registry`.

**Architecture:** Task 1 authors and offline-validates the two submission files inside this repo (`docs/registry-submission/aivyx-coder/`), against the real, fetched registry schema and the real, already-published `v0.1.0` release asset — no placeholders, no values pinned "at execution time." Task 2 is the external action: fork the registry, add the two files, commit, push, open the PR.

**Tech Stack:** Plain JSON (`agent.json`), SVG (`icon.svg`), a small stdlib-only Python validation script (no `pip`/`jsonschema` available in this environment — confirmed by direct check), `gh`/`git`.

## Global Constraints

- `agent.json` must **omit** the `icon` field entirely — the registry's own schema documents it as "set automatically by the build from the required icon.svg file," so a hand-written `icon` field would be incorrect, not just redundant.
- `distribution.binary` uses `linux-x86_64` only this submission (the real, only-published platform) — `darwin-aarch64` is deliberately deferred to a later, separate PR once a real tagged release actually produces that asset (confirmed with the project owner).
- The real, fetched, exact values to use (no placeholders, no "verify at execution time" — already confirmed live against the real repo/release as of this plan's writing):
  - Real release asset SHA256: `a0064ac0a94eeeefb26a45d96bf949290b221f657c204cc6145afb851f583069` (64 hex chars, confirmed via both the release's own published `.sha256` sidecar file and a fresh local `sha256sum` of the downloaded archive).
  - Real archive internal layout: the binary is at `aivyx-coder-v0.1.0-x86_64-linux-musl/aivyx-coder` inside the tarball, **not** at the archive root (confirmed via `tar -tzvf`) — `cmd` must be a real relative path into that structure, not a bare binary name.
  - `cmd` resolution semantics, confirmed against the registry's own real CI validation source (`.github/workflows/verify_agents.py`, `resolve_binary_executable`/`normalize_command_path`): `cmd` (with any leading `./` stripped) is matched as a relative path/suffix anywhere in the extracted tree — real precedent from `devin`/`junie`/`cursor`'s own subdirectory-archive `agent.json` entries is to write the genuine relative path from the extraction root (e.g. `devin/agent.json`'s `"cmd": "./bin/devin"`), not rely on the validator's recursive-search fallback.
- Task 2 (the fork + PR) requires the project owner's explicit confirmation immediately before it runs — this is a real, external, third-party-repo action, not a reversible local change.
- No `pip`/`jsonschema` module is available in this environment (confirmed: `python3 -m pip --version` reports "No module named pip") — Task 1's validation script is a small, dependency-free, stdlib-only Python script re-implementing the specific schema rules that apply to this submission, not a general-purpose schema validator.

---

### Task 1: Author and validate `agent.json` + `icon.svg`

**Files:**
- Create: `docs/registry-submission/aivyx-coder/agent.json`
- Create: `docs/registry-submission/aivyx-coder/icon.svg`
- Create: `docs/registry-submission/validate_agent_json.py` (a reusable, stdlib-only validator — lives one level up from the submission files so it can validate future platform-addition PRs too, not just this one)

**Interfaces:**
- Produces: two files at `docs/registry-submission/aivyx-coder/{agent.json,icon.svg}`, both validated and ready to copy into a fork of the registry repo at the same relative path (`aivyx-coder/agent.json`, `aivyx-coder/icon.svg`) — Task 2 consumes them by path, verbatim, no further editing.

- [ ] **Step 1: Write `icon.svg`**

Create `docs/registry-submission/aivyx-coder/icon.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
  <path d="M7,5 L15,12 L7,19 L9.2,19 L17.2,12 L9.2,5 Z M7,20 L17,20 L17,22 L7,22 Z"/>
</svg>
```

A terminal-prompt chevron (`>`) with a cursor/underscore beneath it — a thick right-pointing chevron (the outer edge `M7,5 L15,12 L7,19`, the inner edge offset by 2.2 units for visual thickness) plus a short filled rectangle standing in for a blinking cursor. Single `<path>`, `fill="currentColor"` only (set once on the root `<svg>`, inherited — no hardcoded color anywhere in the file), large `24×24` internal viewBox scaled down for display, matching the real `claude-acp/icon.svg` convention (oversized viewBox, not literal 16×16 coordinates).

- [ ] **Step 2: Verify `icon.svg` has no hardcoded colors**

Run: `grep -E '#[0-9a-fA-F]{3,6}|rgb\(|rgba\(' docs/registry-submission/aivyx-coder/icon.svg`
Expected: no output, and the command exits non-zero (grep found no matches) — confirming only `currentColor` is used, no hardcoded hex/rgb color anywhere in the file.

- [ ] **Step 3: Confirm the real release asset's SHA256 and internal layout**

Run:
```bash
curl -sL "https://github.com/Aivyx-Agent/aivyx-coder/releases/download/v0.1.0/aivyx-coder-v0.1.0-x86_64-linux-musl.tar.gz" -o /tmp/aivyx-coder-registry-check.tar.gz
sha256sum /tmp/aivyx-coder-registry-check.tar.gz
tar -tzvf /tmp/aivyx-coder-registry-check.tar.gz
```
Expected: the `sha256sum` output's first field is exactly `a0064ac0a94eeeefb26a45d96bf949290b221f657c204cc6145afb851f583069`, and the `tar -tzvf` listing shows the binary at `aivyx-coder-v0.1.0-x86_64-linux-musl/aivyx-coder` (inside a subdirectory, not at the archive root). If either differs from this plan's Global Constraints (e.g. the release was re-cut with a different asset since this plan was written), **STOP and report BLOCKED** — do not proceed with stale values.

- [ ] **Step 4: Write `agent.json`**

Create `docs/registry-submission/aivyx-coder/agent.json`:

```json
{
  "id": "aivyx-coder",
  "name": "aivyx-coder",
  "version": "0.1.0",
  "description": "A terminal coding agent for local LLMs only (Ollama, vLLM, llama.cpp) -- never calls a cloud API.",
  "repository": "https://github.com/Aivyx-Agent/aivyx-coder",
  "license": "MIT OR Apache-2.0",
  "license_url": "https://github.com/Aivyx-Agent/aivyx-coder/blob/main/LICENSE",
  "distribution": {
    "binary": {
      "linux-x86_64": {
        "archive": "https://github.com/Aivyx-Agent/aivyx-coder/releases/download/v0.1.0/aivyx-coder-v0.1.0-x86_64-linux-musl.tar.gz",
        "sha256": "a0064ac0a94eeeefb26a45d96bf949290b221f657c204cc6145afb851f583069",
        "cmd": "./aivyx-coder-v0.1.0-x86_64-linux-musl/aivyx-coder",
        "args": ["--acp"]
      }
    }
  }
}
```

Note deliberately **no** `icon` field (per Global Constraints — set automatically by the registry's own build) and deliberately no `authors`/`website`/`preview` fields (all optional, none decided as needed per the design spec's "What this spec does not decide").

- [ ] **Step 5: Write the offline validator**

Create `docs/registry-submission/validate_agent_json.py`:

```python
#!/usr/bin/env python3
"""Offline validation of an ACP registry agent.json against the real
schema rules (fetched 2026-09-21 from agentclientprotocol/registry's
agent.schema.json) -- no network access or third-party dependency
needed, since this environment has neither `jsonschema` nor `pip`
available. Re-implements the rules relevant to a binary-distribution
submission with no preview channel; does not attempt to validate every
possible field the full schema supports (e.g. npx/uvx distribution,
preview channels) since this project's submission never uses them.

Usage: python3 docs/registry-submission/validate_agent_json.py \
    docs/registry-submission/aivyx-coder/agent.json
"""
import json
import re
import sys


def fail(msg: str) -> None:
    print(f"FAIL: {msg}")
    sys.exit(1)


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: validate_agent_json.py <path/to/agent.json>")

    with open(sys.argv[1]) as f:
        data = json.load(f)

    for field in ("id", "name", "version", "description", "distribution"):
        if field not in data:
            fail(f"missing required field: {field}")

    if not re.fullmatch(r"[a-z][a-z0-9-]*", data["id"]):
        fail(f"id {data['id']!r} does not match ^[a-z][a-z0-9-]*$")

    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", data["version"]):
        fail(f"version {data['version']!r} does not match ^[0-9]+\\.[0-9]+\\.[0-9]+$")

    if data["id"] != "dimcode" and "license_url" not in data:
        fail("license_url is required (id is not the 'dimcode' exception)")

    if "icon" in data:
        fail(
            "icon field must be omitted -- set automatically by the "
            "registry's own build from the sibling icon.svg file, not "
            "hand-written"
        )

    dist = data["distribution"]
    if not isinstance(dist, dict) or len(dist) < 1:
        fail("distribution must be an object with at least one property")

    allowed_dist_keys = {"binary", "npx", "uvx"}
    extra_dist_keys = set(dist.keys()) - allowed_dist_keys
    if extra_dist_keys:
        fail(f"distribution has disallowed keys: {extra_dist_keys}")

    if "binary" in dist:
        binary = dist["binary"]
        if not isinstance(binary, dict) or len(binary) < 1:
            fail("distribution.binary must be an object with at least one property")
        allowed_platforms = {
            "darwin-aarch64",
            "darwin-x86_64",
            "linux-aarch64",
            "linux-x86_64",
            "windows-aarch64",
            "windows-x86_64",
        }
        for platform, target in binary.items():
            if platform not in allowed_platforms:
                fail(f"distribution.binary key {platform!r} is not an allowed platform")
            for field in ("archive", "cmd"):
                if field not in target:
                    fail(f"distribution.binary.{platform} missing required field: {field}")
            allowed_target_keys = {"archive", "sha256", "cmd", "args", "env"}
            extra_target_keys = set(target.keys()) - allowed_target_keys
            if extra_target_keys:
                fail(f"distribution.binary.{platform} has disallowed keys: {extra_target_keys}")
            if "sha256" in target and not re.fullmatch(r"[a-fA-F0-9]{64}", target["sha256"]):
                fail(f"distribution.binary.{platform}.sha256 is not 64 hex characters")
            if "/latest/" in target["archive"]:
                fail(f"distribution.binary.{platform}.archive must not contain '/latest/'")

    print("PASS: agent.json matches the real registry schema rules")


if __name__ == "__main__":
    main()
```

- [ ] **Step 6: Run the validator against the real file**

Run: `python3 docs/registry-submission/validate_agent_json.py docs/registry-submission/aivyx-coder/agent.json`
Expected: `PASS: agent.json matches the real registry schema rules`

- [ ] **Step 7: Confirm the `cmd` path genuinely exists inside the real archive**

Run: `tar -tzf /tmp/aivyx-coder-registry-check.tar.gz | grep -F "aivyx-coder-v0.1.0-x86_64-linux-musl/aivyx-coder"`
Expected: prints `aivyx-coder-v0.1.0-x86_64-linux-musl/aivyx-coder` — confirming `agent.json`'s `cmd` value (`./aivyx-coder-v0.1.0-x86_64-linux-musl/aivyx-coder`, with the leading `./` stripped) really resolves to a real path inside the real archive, end to end, not just schema-shape-valid.

- [ ] **Step 8: Validate `agent.json` is syntactically valid JSON (belt-and-braces, in case of a hand-editing slip)**

Run: `python3 -c "import json; json.load(open('docs/registry-submission/aivyx-coder/agent.json'))" && echo "valid JSON"`
Expected: `valid JSON`

- [ ] **Step 9: Commit**

```bash
git add docs/registry-submission/
git commit -m "feat: author and validate ACP registry submission files"
```

---

### Task 2: Fork, add the files, and open the PR

**⚠️ This task performs a real, external, irreversible action against a third-party repository (`agentclientprotocol/registry`) that this project does not own. Do not dispatch this task to an autonomous subagent. Do not begin any step in this task until the project owner has explicitly confirmed, immediately before execution — reviewing Task 1's final committed `agent.json`/`icon.svg` content is not the same as confirming Task 2 itself.**

**Files:** none inside this repo — all changes happen in a separate, forked clone of `agentclientprotocol/registry` outside this working tree.

**Interfaces:**
- Consumes: `docs/registry-submission/aivyx-coder/agent.json`, `docs/registry-submission/aivyx-coder/icon.svg` (Task 1's validated output, copied verbatim).
- Produces: a real, public GitHub PR URL against `agentclientprotocol/registry`, reported back to the project owner. Nothing further (no merging — that's the registry maintainers' own call).

- [ ] **Step 1: Confirm with the project owner before proceeding**

Present the final, committed content of `docs/registry-submission/aivyx-coder/agent.json` and `docs/registry-submission/aivyx-coder/icon.svg` (from Task 1) and ask for explicit confirmation to proceed with the fork + PR. Do not continue to Step 2 without an explicit yes.

- [ ] **Step 2: Fork the registry repo**

Run: `gh repo fork agentclientprotocol/registry --clone=false`
Expected: reports a new fork created under the authenticated user's/org's own GitHub account (e.g. `<your-username>/registry`).

- [ ] **Step 3: Clone the fork into a scratch location and create a branch**

Run (substituting `<fork-owner>` with whatever `gh repo fork`'s output reported in Step 2):
```bash
git clone "https://github.com/<fork-owner>/registry.git" /tmp/acp-registry-fork
cd /tmp/acp-registry-fork
git checkout -b add-aivyx-coder
```
Expected: a clean clone, on a new `add-aivyx-coder` branch.

- [ ] **Step 4: Copy the validated files into the fork at the correct path**

Run:
```bash
mkdir -p /tmp/acp-registry-fork/aivyx-coder
cp /home/julian/Projects/Rust/aivyx-coder/docs/registry-submission/aivyx-coder/agent.json /tmp/acp-registry-fork/aivyx-coder/agent.json
cp /home/julian/Projects/Rust/aivyx-coder/docs/registry-submission/aivyx-coder/icon.svg /tmp/acp-registry-fork/aivyx-coder/icon.svg
```
Expected: `/tmp/acp-registry-fork/aivyx-coder/` now contains exactly the two files, verbatim copies of Task 1's validated output — no re-editing.

- [ ] **Step 5: Commit and push to the fork**

Run:
```bash
cd /tmp/acp-registry-fork
git add aivyx-coder/
git commit -m "Add aivyx-coder"
git push -u origin add-aivyx-coder
```
Expected: the branch pushes successfully to the fork.

- [ ] **Step 6: Open the PR**

Run:
```bash
cd /tmp/acp-registry-fork
gh pr create --repo agentclientprotocol/registry \
  --title "Add aivyx-coder" \
  --body "$(cat <<'EOF'
## Summary
Adds `aivyx-coder` -- a terminal coding agent for local LLMs only (Ollama, vLLM, llama.cpp), never calling a cloud API. Real ACP frontend (`crates/aivyx-acp`), a real `terminal` auth method pointing at its own `aivyx-coder setup` first-run wizard.

Linux-only distribution for now (`linux-x86_64`); `darwin-aarch64` support is planned as a follow-up PR once a tagged release produces that binary.

## Test plan
- [x] `agent.json` validated against the registry's own schema rules (offline, see `aivyx-coder`'s own repo history for the validation script)
- [x] `distribution.binary.linux-x86_64.archive`/`sha256` confirmed against the real, live `v0.1.0` GitHub Release asset
- [x] `cmd` confirmed to resolve to a real path inside the real downloaded archive
- [x] `icon.svg` confirmed to use only `currentColor`, no hardcoded colors
EOF
)"
```
Expected: prints the real PR URL.

- [ ] **Step 7: Report the PR URL back to the project owner**

State the real PR URL plainly — no further action (no merging, no follow-up edits) unless the project owner asks for one.

---

## Self-Review Notes

**Spec coverage:** Decision 1 (two-stage: author+validate locally, then a separate confirmed external step) → Task 1 vs. Task 2's own explicit gating. Decision 2 (`agent.json` content, fully grounded) → Task 1 Step 4, corrected from the spec's own draft (`icon` field removed per the real schema's "set automatically by the build" description, discovered during plan-writing — a legitimate spec-to-plan refinement, not a contradiction, since the spec's own JSON was explicitly marked as needing final pinning at plan time) and with the real `cmd` value (also newly researched at plan-writing time — the spec didn't have this detail, since it wasn't yet known that the archive's binary sits inside a subdirectory). Decision 3 (icon design) → Task 1 Step 1. Decision 4 (offline validation) → Task 1 Steps 2, 6, 7, 8. "What this spec does not decide" items are all genuinely untouched: no `darwin-aarch64` entry, no `npx`/`uvx`, no `website` field.

**Global Constraints deviation, disclosed:** the design spec's draft `agent.json` example included `"icon": "icon.svg"` and a placeholder `cmd` value; both are corrected here based on real research done specifically for this plan (the registry's actual JSON schema description for `icon`, and the real CI validation script's `cmd`-resolution logic plus real precedent from `devin`/`junie`/`cursor`). This is the kind of "exact final field values... pinned at plan-writing/implementation time" work the spec's own Decision 2 explicitly deferred, not an unauthorized change.

**Placeholder scan:** no TBD/TODO; every code/content step shows complete, real content (real SHA256, real archive path, real SVG path data, real validator script); Task 2's `<fork-owner>` substitution is an explicit, necessary runtime value (GitHub username/org is only known once `gh repo fork` actually runs) — not a plan-authoring placeholder, and the step's own instructions say exactly how to fill it in from the prior step's real output.

**Type/interface consistency check:** Task 1's produced file paths (`docs/registry-submission/aivyx-coder/{agent.json,icon.svg}`) match exactly what Task 2 Step 4 copies from. The `cmd` value written in Task 1 Step 4 matches exactly what Task 1 Step 7 confirms exists in the real archive.
