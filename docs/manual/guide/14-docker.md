# Docker

A secondary, optional way to run `aivyx-coder` — the native
`x86_64-linux-musl`/`darwin-aarch64` binaries (see
[Install and first run](02-install-and-first-run.md)) stay the primary,
recommended distribution.

## Build and run

Each release is also published as `ghcr.io/aivyx-agent/aivyx-coder`
(`:latest` or a version tag) — use that in place of `aivyx-coder` below to
skip the build. Or build it yourself:

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

## The sandbox in a container

The real OS-level sandbox (Linux Landlock + seccomp) is on by default
inside the container exactly as on a bare host — confirmed directly
(not assumed) via `aivyx-confine`'s own test suite passing inside a
real container built from this same `Dockerfile`, and via a standalone
probe confirming a Landlock grant scoped to a bind-mounted directory
correctly allows reads/writes within it (visible on the real host
filesystem) while denying reads outside it.

## Connecting to Docker Model Runner

If the LLM backend is DMR
instead of Ollama/llama-server, the same `--add-host` flag above is
required, but the `base_url` path differs — DMR serves its
OpenAI-compatible API under `/engines/v1`, not bare `/v1`:

```toml
[backend]
base_url = "http://host.docker.internal:12434/engines/v1"
model = "ai/smollm2:135M-Q4_K_M"
```

Confirmed live end-to-end (2026-09-21): a real chat completion
round-trip through this exact path from inside a container, against a
real DMR instance running on the host.

## Can't reach the host from the container

Symptom: `curl` from inside the
container times out against `host.docker.internal:<port>`, while the
same address works fine from the host itself. Cause, confirmed on one
real machine: a host firewall (`iptables`/`ufw`) with a default-deny
`INPUT` policy blocks the container→host-gateway path even though
Docker's own port-publish networking is correctly configured — Docker's
port-publish DNAT rule deliberately excludes traffic *originating from*
the bridge network (`docker0`) to avoid a hairpin-NAT loop, so the
packet is delivered locally instead of NAT'd, and a strict host firewall
then drops it with no matching rule. Fix (scoped to exactly this
traffic, on Linux):

```
sudo iptables -I INPUT -i docker0 -p tcp --dport <port> -j ACCEPT
```

This is a property of the host's own firewall configuration, not
something `aivyx-coder`'s `Dockerfile` or config can detect or fix — if
a `host.docker.internal`-based connection times out (not "connection
refused"), check the host firewall before assuming the backend is
misconfigured. See `docs/HISTORY.md`'s Docker Model Runner chapter for
the full diagnosis.
