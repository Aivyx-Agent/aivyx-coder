# Docker Model Runner Container Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document, in the four places this project already tracks it, how a containerized `aivyx-coder` connects to Docker Model Runner (DMR) — including the real firewall/hairpin-NAT fix required to make it work — and correct/narrow the existing DMR documentation's unverified claims now that live testing has happened.

**Architecture:** Pure documentation change across four existing files — no code, no `Dockerfile` change (DMR runs on the host, not inside `aivyx-coder`'s own image). Each file gets targeted edits to existing sections/paragraphs rather than new files.

**Tech Stack:** Markdown only.

## Global Constraints

- No `Dockerfile` changes — DMR is host-level, not something the image can bundle or depend on at build time.
- No `probe.rs` or any other `.rs` file changes — the diagnostic-endpoint extension stays confirmed-blocked, not implemented.
- The context-window runtime default and end-to-end tool-calling remain explicitly flagged unverified in every file touched — this plan does not claim either is confirmed.
- File-scoped `rustfmt --edition 2024 --check <path>` only if any `.rs` file is touched (this plan doesn't touch any) — **never** a package-scoped `cargo fmt -p <crate>` command with no file argument, per this project's own repeated, documented incident history.

---

### Task 1: Update `README.md`, `ROADMAP.md`, `docs/HISTORY.md`

**Files:**
- Modify: `README.md` (two locations: the "Docker" subsection under "Building and running", and the "Docker Model Runner" subsection under "## Serving")
- Modify: `ROADMAP.md` (the "Docker Model Runner serving support" backlog entry)
- Modify: `docs/HISTORY.md` (the "Docker Model Runner serving support" chapter)

**Interfaces:** none — this is the whole deliverable, no later task consumes it.

- [ ] **Step 1: Add the DMR connection + firewall-troubleshooting subsection to `README.md`'s "Docker" section**

In `README.md`, find this exact paragraph (in the "Docker" subsection, immediately before the `## Serving` heading):

```
The real OS-level sandbox (Linux Landlock + seccomp) is on by default
inside the container exactly as on a bare host — confirmed directly
(not assumed) via `aivyx-confine`'s own test suite passing inside a
real container built from this same `Dockerfile`, and via a standalone
probe confirming a Landlock grant scoped to a bind-mounted directory
correctly allows reads/writes within it (visible on the real host
filesystem) while denying reads outside it.

## Serving
```

Replace it with (adds two new paragraphs before `## Serving`, keeping the existing paragraph unchanged above the insertion):

```
The real OS-level sandbox (Linux Landlock + seccomp) is on by default
inside the container exactly as on a bare host — confirmed directly
(not assumed) via `aivyx-confine`'s own test suite passing inside a
real container built from this same `Dockerfile`, and via a standalone
probe confirming a Landlock grant scoped to a bind-mounted directory
correctly allows reads/writes within it (visible on the real host
filesystem) while denying reads outside it.

**Connecting to Docker Model Runner (DMR).** If the LLM backend is DMR
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

**Troubleshooting: container can't reach DMR (or any host-published
service) even with `--add-host` set.** Symptom: `curl` from inside the
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

## Serving
```

- [ ] **Step 2: Narrow the "Not yet live-verified" banner in `README.md`'s Serving-section DMR subsection**

In `README.md`, find this exact paragraph:

```
**Docker Model Runner (Docker Desktop/Engine's built-in local model
runner).** ⚠️ **Not yet live-verified** — everything in this subsection
comes from Docker's own documentation and third-party write-ups
gathered during research, not from a real running instance (unlike
every other backend above, which was confirmed live before being
written down). Treat the specifics here as a starting point, not a
guarantee, until someone runs it.
```

Replace it with:

```
**Docker Model Runner (Docker Desktop/Engine's built-in local model
runner).** Base URL, model-naming convention, and basic chat completions
confirmed live 2026-09-21 against a real running instance (see
`docs/HISTORY.md`). ⚠️ **The context-window default and end-to-end
tool-calling below are still unverified.**
```

- [ ] **Step 3: Correct the context-window paragraph in the same subsection**

In `README.md`, find this exact paragraph (immediately after the model-naming `toml` block and the "explicit engine" note, in the same DMR subsection):

```
**The same hidden-context-window trap as Ollama, reportedly**: DMR's
underlying llama.cpp engine defaults to a 4096-token context unless
explicitly configured. Set it with `docker model configure
--context-size N <model>` (or a `context_size:` key under `models:` in
a Docker Compose file) before pointing aivyx at it — and note that
aivyx's own startup probe (which catches this automatically for Ollama)
does **not** currently detect it for DMR, since DMR's diagnostic
endpoint shape isn't confirmed yet (see `probe.rs`). Until that's
extended, confirm your configured context size manually rather than
relying on a truncation warning. One third-party report (recent, but not
precisely dated) found a specific Docker CUDA runtime image that
hard-coded `--ctx-size 4096` regardless of the `configure` setting —
worth checking for on whatever version you actually install, not
assumed fixed or still-broken.
```

Replace it with:

```
**The same hidden-context-window trap as Ollama, reportedly**: DMR's
underlying llama.cpp engine defaults to a 4096-token context unless
explicitly configured. Set it with `docker model configure
--context-size N <model>` (or a `context_size:` key under `models:` in
a Docker Compose file) before pointing aivyx at it — and note that
aivyx's own startup probe (which catches this automatically for Ollama)
does **not** detect it for DMR: confirmed 2026-09-21 that no
`/props`-equivalent diagnostic endpoint exists at either
`.../engines/v1/props` or `.../engines/llama.cpp/v1/props` (both return
`not found`), so a `probe.rs` extension for DMR stays blocked, not just
unimplemented. Until DMR ships some other diagnostic shape, confirm your
configured context size manually rather than relying on a truncation
warning. **The 4096-default claim itself is still unconfirmed** — a real
pulled model's own GGUF metadata (`docker model inspect`,
`llama.context_length`) reports its trained/max context (8192 for the
model tested), but that is a different, weaker fact than the actual
*runtime serving* default DMR applies, which wasn't directly tested (no
diagnostic endpoint to read it from, and no large-prompt truncation test
was run). One third-party report (recent, but not precisely dated) found
a specific Docker CUDA runtime image that hard-coded `--ctx-size 4096`
regardless of the `configure` setting — worth checking for on whatever
version you actually install, not assumed fixed or still-broken.
```

- [ ] **Step 4: Narrow `ROADMAP.md`'s DMR backlog entry**

In `ROADMAP.md`, find this exact paragraph:

```
**Docker Model Runner serving support — documented, pending live
verification.** A new `README.md` "Serving" subsection covers Docker
Model Runner (DMR) as another local-LLM backend option — no code
changes needed, since `base_url` already targets any OpenAI-compatible
endpoint generically. Unlike every other backend documented in this
project, none of this subsection's technical claims (the exact `base_url`
path, the context-window default behavior, whether tool-calling works
end-to-end through aivyx's native edit format) have been confirmed
against a real running instance yet — see `docs/HISTORY.md` for the full
account of what's confirmed vs. still open.
```

Replace it with:

```
**Docker Model Runner serving support — partially live-verified,
container connectivity also shipped.** Base URL, model-naming
convention, and basic chat completions confirmed live 2026-09-21 against
a real DMR instance (both from a native `aivyx-coder` and, new this
entry, from inside `aivyx-coder`'s own Docker container via
`host.docker.internal:12434/engines/v1` — including a real host-firewall
hairpin-NAT fix required to make that path work at all). Still open: the
context-window runtime default (a real pulled model's GGUF metadata
shows 8192, a different fact from the actual serving-time default, which
remains unconfirmed) and whether tool-calling works end-to-end through
aivyx's native edit format. `probe.rs`'s DMR extension is now confirmed
blocked, not just deferred — no diagnostic endpoint exists at either
shape checked. See `docs/HISTORY.md` for the full account.
```

- [ ] **Step 5: Update `docs/HISTORY.md`'s "Docker Model Runner serving support" chapter**

In `docs/HISTORY.md`, find this exact heading:

```
### Docker Model Runner serving support — documented, live verification pending
```

Replace it with:

```
### Docker Model Runner serving support — base connectivity verified 2026-09-21, container support added
```

Then, in the same chapter, find this exact paragraph (the one starting "**Everything in the new subsection is honestly flagged as unverified**"):

```
**Everything in the new subsection is honestly flagged as unverified**,
a deliberate departure from how every other backend in this project has
been documented — Ollama, llama-server, Lemonade, and vLLM were each
confirmed against a real running instance (live E2E tests, acceptance
benchmarks, or at minimum a manual compat check) before being written
into `README.md`. For DMR: no running Docker daemon was accessible
during this chapter's research, so the base URL
(`http://localhost:12434/engines/v1`), the model-naming convention
(`namespace/name[:tag]`), the reported hidden-context-window default
(4096, unless set via `docker model configure --context-size N`), and
whether tool/function calling actually works end-to-end through aivyx's
native edit format are all drawn from Docker's own docs and third-party
write-ups, not confirmed firsthand. One specific claim worth
double-checking on a real install: a third-party report found a Docker
CUDA runtime image that hard-coded `--ctx-size 4096` regardless of the
`configure` setting — possibly since fixed, possibly not.
```

Replace it with:

```
**Live-verified 2026-09-21, correcting the original "everything
unverified" framing below.** DMR was installed for real on this
session's own machine (CachyOS/Arch, Docker CE v29.7.2, no Docker
Desktop) via the `docker-model-plugin` AUR package, `docker model
install-runner` (which itself runs DMR as a real Docker container,
`docker/model-runner:latest-cuda` — not a bare host process, correcting
Docker's own marketing description of "host-native" inference), and
`docker model pull ai/smollm2:135M-Q4_K_M`. Confirmed against a real
running instance: the base URL (`http://localhost:12434/engines/v1`),
the model-naming convention (`namespace/name[:tag]`), and a real chat
completion request/response round-trip with usage/timing stats. **Still
unconfirmed**: the reported hidden-context-window default (4096, unless
set via `docker model configure --context-size N`) — a pulled model's
own GGUF metadata (`docker model inspect`) reports `llama.context_length:
8192`, but that's the model's trained/max context, a different and
weaker fact than the actual runtime serving default, which no available
diagnostic endpoint could confirm (see below) and no large-prompt
truncation test was run to check directly — and whether tool/function
calling actually works end-to-end through aivyx's native edit format,
still not exercised. The specific third-party report of a Docker CUDA
runtime image hard-coding `--ctx-size 4096` regardless of the
`configure` setting also remains unchecked on the version installed
here.
```

Then, in the same chapter, find this exact paragraph (the one starting "**`probe.rs`'s automatic context-window detection was deliberately not extended for DMR in this chapter.**"):

```
**`probe.rs`'s automatic context-window detection was deliberately not
extended for DMR in this chapter.** Its origin-derivation logic
(`base_url.trim_end_matches('/').trim_end_matches("/v1")`) only strips a
trailing `/v1`; for DMR's `.../engines/v1` base URL this leaves
`.../engines` as the computed origin, an assumption not confirmed to
line up with wherever DMR's actual diagnostic endpoint (if one even
exists in an Ollama-`/api/show`-compatible shape) actually lives.
Shipping a guessed implementation would have been shipping unverified
parsing logic — low-risk, since both existing parsers fail safe to
`ServedContext::Unknown` on any shape mismatch rather than misreporting
a wrong number, but still speculative code with no way to confirm it
helps anyone until tested live. **General lesson, consistent with this
project's established practice** (see the `diffy` and `cargo test`
path-filtering findings elsewhere in this history): verify a
dependency's or service's actual behavior before writing code against
assumptions about it — documentation with an honest "unverified" label
is more useful than code that quietly might not work.
```

Replace it with:

```
**`probe.rs`'s automatic context-window detection stays unextended for
DMR — now confirmed blocked, not just deferred.** Its origin-derivation
logic (`base_url.trim_end_matches('/').trim_end_matches("/v1")`) only
strips a trailing `/v1`; for DMR's `.../engines/v1` base URL this leaves
`.../engines` as the computed origin. Checked directly 2026-09-21
against a real running DMR instance: neither
`.../engines/v1/props` nor `.../engines/llama.cpp/v1/props` exist (both
return `not found`) — there is no Ollama-`/api/show`-compatible
diagnostic endpoint at either shape a reasonable guess would produce.
Shipping a guessed implementation would have been shipping unverified
parsing logic against an endpoint now confirmed not to exist at the
locations checked — low-risk either way, since both existing parsers
fail safe to `ServedContext::Unknown` on any shape mismatch rather than
misreporting a wrong number. **General lesson, consistent with this
project's established practice** (see the `diffy` and `cargo test`
path-filtering findings elsewhere in this history): verify a
dependency's or service's actual behavior before writing code against
assumptions about it — documentation with an honest "unverified" label
is more useful than code that quietly might not work.
```

Then, in the same chapter, find this exact paragraph (the one starting "**Still open, the actual next step for this chapter**"):

```
**Still open, the actual next step for this chapter**: a live check
against a real Docker Model Runner instance — confirming the base URL
and model-naming convention actually work, confirming or correcting the
context-window default behavior on whatever version is actually
installed, checking tool-calling end-to-end through aivyx's native edit
format, and — if a real diagnostic endpoint is found — a follow-up
`probe.rs` extension using the now-confirmed shape.
```

Replace it with:

```
**Still open**: confirming or correcting the context-window runtime
default (as distinct from the model's own trained/max context, which
*is* now confirmed via `docker model inspect`), and checking
tool-calling end-to-end through aivyx's native edit format. The
`probe.rs` extension is not "still open" in the original sense — it's
confirmed blocked absent DMR shipping some other diagnostic endpoint
shape in a future version.

**New this update: container connectivity, verified end-to-end.**
`aivyx-coder`'s own Docker container distribution (see this file's
"Docker container distribution" coverage and `README.md`'s "Docker"
section) can reach a host-run DMR via the same
`--add-host=host.docker.internal:host-gateway` mechanism already
documented for Ollama/llama-server, with a different `base_url` path
(`/engines/v1` instead of bare `/v1`). `docker inspect` on the running
`docker-model-runner` container shows it publishes port 12434 bound to
both `127.0.0.1` and the bridge gateway IP (`172.17.0.1` on this
machine) — a deliberate choice enabling exactly this
`host.docker.internal` path. Initial testing from inside a real
container timed out (`HTTP 000`) against `host.docker.internal:12434`
despite the same address working from the host itself. Root-caused by
hand through `iptables`: `FORWARD` policy `DROP`, then confirmed the
`DOCKER`-chain DNAT rule for port 12434 explicitly excludes
`docker0`-sourced traffic (`!docker0` in its match — Docker's own
anti-hairpin-loop default), so the packet is delivered locally instead
of NAT'd to the actual `docker-model-runner` container, and the host's
`INPUT` chain (also policy `DROP` here, via a persisted `iptables`/`ufw`
ruleset) drops it with no matching ACCEPT rule. Fixed with a single
scoped rule (`iptables -I INPUT -i docker0 -p tcp --dport 12434 -j
ACCEPT`); re-tested afterward — both a plain HTTP request and a real
chat completion round-trip through
`host.docker.internal:12434/engines/v1/chat/completions` succeeded from
inside a container. This firewall dependency is host-specific, not a
DMR or `aivyx-coder` defect — documented as a named troubleshooting item
in `README.md`'s "Docker" section rather than something this project can
detect or fix in code.
```

- [ ] **Step 6: Verify the edits landed correctly**

Run:
```bash
grep -n "Not yet live-verified" README.md
grep -n "host.docker.internal:12434" README.md
grep -n "iptables -I INPUT -i docker0" README.md docs/HISTORY.md
grep -n "documented, pending live" ROADMAP.md
grep -n "documented, live verification pending" docs/HISTORY.md
```
Expected: the first, fourth, and fifth commands print nothing (the old
banner/heading text no longer exists anywhere); the second and third
print at least one match each (the new content is present).

- [ ] **Step 7: Commit**

```bash
git add README.md ROADMAP.md docs/HISTORY.md
git commit -m "docs: verify Docker Model Runner connectivity, add container support"
```

---

## Self-Review Notes

**Spec coverage:** Decision 1 (README Docker-section DMR subsection, `base_url`/`model` example, no `Dockerfile` change) → Step 1. Decision 2 (concise README troubleshooting note + full diagnosis in `docs/HISTORY.md`) → Steps 1 and 5. Decision 3 (narrow, don't remove, the Serving-section banner; correct the context-window paragraph; upgrade `probe.rs` non-extension to confirmed-blocked) → Steps 2, 3, and the `docs/HISTORY.md` `probe.rs` paragraph in Step 5. Decision 4 (narrow `ROADMAP.md`'s backlog entry) → Step 4. "What this spec does not decide" items are all genuinely respected: no context-window runtime-default claim made, no tool-calling claim made, no `probe.rs` code change, no DMR-bundling-in-image claim, no native-connection-path changes beyond the banner narrowing.

**Global Constraints deviation:** none — this plan implements the spec's decisions directly, touches no `.rs` files, and makes no `Dockerfile` change.

**Placeholder scan:** no TBD/TODO; every step shows complete, real content (full paragraph text for every replacement, real verified facts, real commands); no "similar to Task N" references (single-task plan).

**Type/interface consistency check:** N/A — no code interfaces are produced or consumed by this plan (pure documentation). The `base_url`/`model` values used in Step 1's `README.md` Docker-section example (`http://host.docker.internal:12434/engines/v1`, `ai/smollm2:135M-Q4_K_M`) match the real values confirmed live in the spec's Grounding section and reused consistently across Steps 1, 4, and 5.
