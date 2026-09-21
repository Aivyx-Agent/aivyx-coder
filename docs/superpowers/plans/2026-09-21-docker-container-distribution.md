# Docker Container Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working `Dockerfile` that builds and runs `aivyx-coder` as a secondary, optional container distribution, with its Landlock sandbox verified working inside the container.

**Architecture:** A multi-stage `Dockerfile` — stage 1 compiles the real release binary using the exact same `x86_64-unknown-linux-musl` target and build command `.github/workflows/release.yml` already uses; stage 2 is a minimal Alpine runtime image with `git`/`bash`/coreutils installed, the compiled binary copied in. Verification reuses `aivyx-confine`'s own existing 14 Landlock tests (run inside a container built from the same builder stage) rather than writing new test code — a direct proof the real shipped confinement logic, not a synthetic probe, works inside this container.

**Tech Stack:** Docker, the existing Rust/Cargo toolchain, `aivyx-confine`'s existing test suite.

## Global Constraints

- The container is a **secondary, optional** distribution — the native `x86_64-linux-musl`/`darwin-aarch64` binaries stay primary. No wrapper script, no Docker Compose file this phase.
- Reuse the exact same build target and command `.github/workflows/release.yml` already uses (`x86_64-unknown-linux-musl`, `CC_x86_64_unknown_linux_musl=musl-gcc`, `cargo build --release --target x86_64-unknown-linux-musl -p aivyx`) — not a new/different build path.
- Runtime base image: Alpine, with `git`/`bash`/coreutils installed — not `scratch`/distroless (the agent's own `run_command`/`run_shell`/`git_*` tools need real programs on `PATH`).
- No sandbox code changes anywhere in this plan — `sandbox.require_enforcement` stays at its existing default (`true`).
- On Linux, `host.docker.internal` does **not** resolve by default (confirmed empirically this session) — any documented `docker run` invocation connecting to a host-run backend must include `--add-host=host.docker.internal:host-gateway`.
- File-scoped `rustfmt --edition 2024 --check <path>` / `rustfmt --edition 2024 <path>` only if any `.rs` file is touched (this plan doesn't touch any) — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: `Dockerfile` + `.dockerignore`, built and verified for real

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`
- Modify: `README.md` (new "Docker" subsection, inserted at the end of the existing "Building and running" section, immediately before the `## Serving` heading)

**Interfaces:** none — this is the whole deliverable, no later task consumes it.

- [ ] **Step 1: Write `.dockerignore`**

Create `.dockerignore` at the repo root:

```
target/
.git/
.claude/
```

- [ ] **Step 2: Write the `Dockerfile`**

Create `Dockerfile` at the repo root:

```dockerfile
# syntax=docker/dockerfile:1

# ---- Builder stage: compiles the static x86_64-unknown-linux-musl
# release binary, mirroring .github/workflows/release.yml's own build
# steps exactly (same target, same musl-gcc CC override, same cargo
# invocation) so this image's binary matches what the real release
# pipeline ships, not a separate/divergent build path.
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /build
COPY . .

ENV CC_x86_64_unknown_linux_musl=musl-gcc
RUN cargo build --release --target x86_64-unknown-linux-musl -p aivyx

# ---- Runtime stage: a minimal, real Alpine base with git/bash/coreutils
# so the agent's own run_command/run_shell/git_* tools have real
# programs on PATH to invoke -- not scratch/distroless, which would
# leave most of the agent's real capability unusable.
FROM alpine:latest

RUN apk add --no-cache git bash coreutils

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/aivyx-coder /usr/local/bin/aivyx-coder

ENTRYPOINT ["aivyx-coder"]
```

- [ ] **Step 3: Build the image for real**

Run: `docker build -t aivyx-coder .`
Expected: builds successfully through both stages (this compiles the full workspace's default-feature build from scratch in a fresh container with no cached `target/`, so expect this to take several minutes — that's expected, not a sign of a problem). Ends with `Successfully tagged aivyx-coder:latest` (or the equivalent modern Docker build output confirming the image was created).

- [ ] **Step 4: Verify the compiled binary runs for real inside the runtime image**

Run:
```bash
docker run --rm aivyx-coder --help
docker run --rm aivyx-coder --version
```
Expected: both commands print real output (the CLI's help text and version string) and exit `0` — confirming the binary is genuinely present, executable, and not missing a runtime library `alpine`'s musl libc doesn't already provide.

- [ ] **Step 5: Verify the real project's own Landlock confinement tests pass inside a container built from the builder stage**

Run:
```bash
docker build --target builder -t aivyx-coder-builder .
docker run --rm aivyx-coder-builder cargo test -p aivyx-confine --target x86_64-unknown-linux-musl
```
Expected: the second command reports all `aivyx-confine` tests passing (the crate's real, existing Landlock test suite — 14 tests as of this plan's writing; the exact count may drift as that crate evolves, so match against "all tests reported, 0 failed" rather than the literal number 14). This is a direct proof that `aivyx-confine`'s real `LandlockConfiner` — the exact code the shipped binary's own sandbox uses — genuinely works inside this container's environment (Alpine's musl-based Linux, this specific Docker/kernel combination), not just a synthetic syscall probe.

If any test fails, **STOP and report BLOCKED** with the failure output — do not proceed to Step 6 or document a container distribution whose own sandbox tests don't pass inside it.

- [ ] **Step 6: Add the "Docker" subsection to `README.md`**

In `README.md`, find the end of the "Building and running" section — immediately before the line `## Serving` (search for that exact heading). Insert this new paragraph block immediately before it (after the existing paragraph ending "...it's what actually ships there today." and before the blank line that precedes `## Serving`):

```markdown

**Docker.** A secondary, optional way to run `aivyx-coder` — the native
`x86_64-linux-musl`/`darwin-aarch64` binaries above stay the primary,
recommended distribution. Build and run:

```bash
docker build -t aivyx-coder .
docker run -it --rm \
  --add-host=host.docker.internal:host-gateway \
  -v "$(pwd)":/workspace -w /workspace \
  -v "$HOME/.config/aivyx-coder":/root/.config/aivyx-coder \
  -v "$HOME/.local/state/aivyx-coder":/root/.local/state/aivyx-coder \
  aivyx-coder
```

The three `-v` flags bind-mount the project directory being worked on,
plus `aivyx-coder`'s own config and session-state directories, so
`config.toml` and session history survive across separate `docker run`
invocations rather than being lost every time the container exits (each
run is otherwise a fresh, ephemeral container). If the LLM backend
(Ollama, llama-server, etc.) runs on the host rather than inside another
container, set `base_url` in `config.toml` to
`http://host.docker.internal:<port>/v1` — the `--add-host` flag above is
required for that hostname to resolve on Linux (confirmed: it does
**not** resolve by default there, unlike Docker Desktop on Mac/Windows).

The real OS-level sandbox (Linux Landlock + seccomp) is on by default
inside the container exactly as on a bare host — confirmed directly
(not assumed) via `aivyx-confine`'s own test suite passing inside a
real container built from this same `Dockerfile`, and via a standalone
probe confirming a Landlock grant scoped to a bind-mounted directory
correctly allows reads/writes within it (visible on the real host
filesystem) while denying reads outside it.
```

- [ ] **Step 7: Commit**

```bash
git add Dockerfile .dockerignore README.md
git commit -m "feat: add Docker container distribution"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (multi-stage Dockerfile, same build target, Alpine+git+bash+coreutils runtime) → Step 2. Decision 2 (`.dockerignore`) → Step 1. Decision 3 (no sandbox code changes) → confirmed by this plan touching zero `.rs` files. Decision 4 (documented `docker run` invocation, `--add-host` requirement) → Step 6. Decision 5 (real verification, not assumed) → Steps 3-5, using `aivyx-confine`'s own real test suite rather than a new synthetic probe, satisfying "the real compiled binary's own confinement, not the throwaway standalone C probe." "What this spec does not decide" items are all genuinely untouched: no DMR bundling, no publishing pipeline, no wrapper script/Compose file, no multi-arch/Windows support — this plan produces exactly one `linux/amd64` image, built and verified locally.

**Placeholder scan:** no TBD/TODO; every step shows complete, real content (the full `Dockerfile`, the full `.dockerignore`, the full README paragraph); no "similar to Task N" references (single-task plan).

**Type/interface consistency check:** N/A — no code interfaces are produced or consumed by this plan (pure infrastructure + documentation).
