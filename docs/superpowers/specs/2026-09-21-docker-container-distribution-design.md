# Docker Container Distribution Design

## Context

`docs/HISTORY.md`'s "Docker Model Runner serving support" chapter
descoped a larger idea — packaging `aivyx-coder` itself as a
Docker/container-based distribution — over an unresolved risk: whether
Docker's default seccomp profile blocks the Landlock syscalls this
project's sandbox depends on. That risk was empirically resolved
2026-09-21 (see `ROADMAP.md`'s backlog entry and `docs/HISTORY.md`'s
updated chapter): a real Docker daemon, a real minimal Landlock probe,
and a real bind-mounted directory all confirmed the full restriction
cycle works identically inside a completely default container as on the
bare host — including that a write to a Landlock-granted, bind-mounted
path is genuinely visible on the host filesystem, and a read outside
the granted path is genuinely denied. This spec picks the idea back up,
scoped to just the container image itself — not the bundled Docker
Model Runner half of the original idea (a separate, later piece, since
DMR's own serving behavior was never live-verified either, per that
same chapter), and not a publishing pipeline (pushing the image
somewhere, wiring it into the existing release workflow — also
separate, later scope).

## Grounding

Read/tested directly, not assumed:

- No existing `Dockerfile`/`.dockerfile` anywhere in this repo — this is
  genuinely new ground, not an update to prior work.
- `.github/workflows/release.yml` — confirmed the existing release
  pipeline already builds a static `x86_64-unknown-linux-musl` binary
  (no runtime library dependencies of its own) — the same target this
  spec's Docker build reuses, not a new/different build target.
- Empirical test (this session, Docker v29.7.2): `getent hosts
  host.docker.internal` inside a default container returns nothing
  (exit 2) — confirmed Linux Docker does **not** resolve
  `host.docker.internal` by default, unlike Docker Desktop on Mac/
  Windows. Adding `--add-host=host.docker.internal:host-gateway` to
  `docker run` makes it resolve correctly (confirmed: resolves to the
  host gateway IP).
- Empirical test (this session): a real Landlock restriction cycle
  (`create_ruleset` with real read+write access rights,
  `landlock_add_rule` granting only a bind-mounted directory,
  `landlock_restrict_self`) run inside a default `alpine:latest`
  container, bind-mounting a real host directory at `/workspace`.
  Confirmed: reading a file inside the granted directory succeeds;
  writing a new file inside it succeeds AND the write is visible on the
  real host filesystem afterward (proving the bind mount is a live,
  two-way bridge, not a copy); reading `/etc/shadow` (outside the
  granted path) is denied with a real `EACCES`/"Permission denied" —
  i.e. Landlock's confinement genuinely still holds with a bind-mounted
  grant target, not just an in-container-native path.
- `README.md`'s existing "Serving" section structure and tone — the new
  "Docker" subsection follows the same "concrete, tested command,
  honestly flag anything not verified" convention already established
  there (e.g. the existing Docker Model Runner subsection's own
  "everything honestly flagged as unverified" framing).
- Confirmed with the project owner (this session's brainstorm): the
  container is a **secondary, optional** distribution channel — the
  native `x86_64-linux-musl`/`darwin-aarch64` binaries stay primary.
  This licenses documented `docker run` invocations for config/session
  persistence and backend connectivity, rather than a wrapper script or
  Docker Compose file, as proportionate for this phase.

## Decisions

**1. A new root-level `Dockerfile`, multi-stage build.** Stage 1
(builder) compiles the release binary for `x86_64-unknown-linux-musl` —
the same target the existing release pipeline already builds, not a new
one. Stage 2 (runtime) is a minimal Alpine base image with `git`,
`bash`, and standard coreutils installed (per the project owner's
explicit choice: the agent's own `run_command`/`run_shell`/`git_*`
tools need real programs on `PATH` inside the container to be usable at
all — a `scratch`/distroless image would make most of the agent's real
capability unusable), plus the compiled static binary copied in.
Entrypoint is the `aivyx-coder` binary itself, defaulting to the
interactive TUI (matching the native binary's own default, no-subcommand
behavior) — overridable to `--acp`/`--mcp-server` via `docker run`'s own
trailing arguments, no container-specific flag needed.

**2. A companion `.dockerignore`** excluding `target/`, `.git/`,
`.claude/worktrees/`, and other build-irrelevant directories, keeping
the build context small and the build itself fast — a real, necessary
companion file, not optional polish.

**3. No sandbox code changes.** Landlock's behavior inside the
container is already empirically proven identical to the bare host (see
Grounding above) — `sandbox.require_enforcement` stays at its existing
default (`true`, fail-closed), with no container-specific carve-out,
special-casing, or new config knob. The container is not a reason to
weaken or bypass the sandbox; it's simply another environment the
existing sandbox already works correctly in.

**4. Config/session persistence and backend connectivity are documented
`docker run` invocations, not new code.** The `README.md` "Docker"
subsection gives the concrete, tested command:

```bash
docker build -t aivyx-coder .
docker run -it --rm \
  --add-host=host.docker.internal:host-gateway \
  -v "$(pwd)":/workspace -w /workspace \
  -v "$HOME/.config/aivyx-coder":/root/.config/aivyx-coder \
  -v "$HOME/.local/state/aivyx-coder":/root/.local/state/aivyx-coder \
  aivyx-coder
```

— bind-mounting the project directory (the thing being worked on),
plus `~/.config/aivyx-coder`/`~/.local/state/aivyx-coder` (so
`config.toml` and session state survive across separate `docker run`
invocations instead of being lost every time the ephemeral container
exits), plus the `--add-host` flag confirmed necessary for the
container to reach a host-run Ollama/llama-server at
`http://host.docker.internal:<port>/v1` (the `base_url` value the
subsection documents setting in `config.toml`).

**5. Verification is real, not assumed.** `docker build` runs for real;
the compiled binary's own `--help`/`--version` are exercised for real
inside a real container; the Landlock grant/deny/host-write proof
(Grounding above) is re-run through the **real compiled `aivyx-coder`
binary's own confinement** (not the throwaway standalone C probe used
to first establish the mechanism works at all), confirming the real
shipped binary's sandbox — not just a synthetic proxy for it — behaves
identically inside the container as on the bare host. If a real local
LLM backend happens to be reachable in the implementation environment,
a genuine end-to-end turn is exercised too; if not, that gap is stated
plainly rather than claimed, matching this project's own established
"honestly flag what's unverified" convention (see Grounding above).

## What this spec does not decide

- Bundling Docker Model Runner (or any other LLM backend) inside the
  image — a separate, later idea, deliberately not part of this spec's
  scope (confirmed with the project owner).
- Publishing the image anywhere (Docker Hub, GHCR, wiring it into the
  existing `.github/workflows/release.yml` pipeline) — separate, later
  scope; this spec only covers `docker build` working locally from a
  checked-out repo.
- A wrapper script or Docker Compose file automating the `docker run`
  invocation above — deliberately deferred given the "secondary,
  optional" distribution role; worth revisiting if the container's real
  usage ever grows enough to justify it.
- Windows container support, ARM-based container images (`linux/arm64`),
  or multi-arch image builds — this spec's scope is the existing
  `x86_64-unknown-linux-musl` target only, matching what the release
  pipeline already builds; broader platform coverage is separate,
  later scope, the same pattern the native binary's own
  `darwin-aarch64` expansion already followed.
