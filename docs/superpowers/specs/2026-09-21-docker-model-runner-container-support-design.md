# Docker Model Runner Container Support Design

## Context

`docs/HISTORY.md`'s "Docker Model Runner serving support" chapter shipped a
`README.md` "Serving" subsection documenting Docker Model Runner (DMR) as a
backend for a **native** (non-containerized) `aivyx-coder`, but explicitly
flagged every claim in it as unverified — no real Docker daemon was
reachable during that chapter's research. `ROADMAP.md`'s matching backlog
entry lists the still-open next step: "a live check against a real Docker
Model Runner instance."

Separately, this session's own `docs/superpowers/specs/2026-09-21-docker-container-distribution-design.md`
shipped a real `Dockerfile` distributing `aivyx-coder` itself as a
container, and deferred one specific question as out of scope: how a
**containerized** `aivyx-coder` reaches a host-run LLM backend. That spec's
README section already covers Ollama/llama-server via
`--add-host=host.docker.internal:host-gateway`, but DMR was never tested
against that path.

This spec is both of those still-open items, resolved together: a real,
live DMR instance was installed on this session's own machine and
empirically verified end-to-end, including the container-connectivity
question the container-distribution spec deferred.

## Grounding

Read and tested directly, not assumed:

- **DMR installed and run for real** on this machine (CachyOS/Arch, Docker
  CE v29.7.2, no Docker Desktop): `docker-model-plugin` via AUR,
  `docker model install-runner` (which itself runs DMR as a real Docker
  container, `docker/model-runner:latest-cuda`, not a bare host process —
  worth correcting, since Docker's own marketing materials describe
  inference as host-native), `docker model pull ai/smollm2:135M-Q4_K_M`.
- **Base URL and API shape confirmed live**: `GET
  http://localhost:12434/engines/v1/models` returns a real OpenAI-style
  model list; `POST .../engines/v1/chat/completions` returned a real
  generated completion with usage/timing stats. The existing
  `README.md` Serving subsection's guessed base URL
  (`http://localhost:12434/engines/v1`) and model-naming convention
  (`namespace/name[:tag]`, e.g. `ai/smollm2:135M-Q4_K_M`) are both
  confirmed correct as written.
- **No diagnostic/props-equivalent endpoint exists at either guessed
  path**: `.../engines/v1/props` and `.../engines/llama.cpp/v1/props` both
  return `not found`. This directly answers the open question
  `docs/HISTORY.md` left about extending `probe.rs` for DMR — there is
  nothing at those two shapes to parse, so that extension stays blocked,
  not merely unverified.
- **Context-window claim only partially checked**: `docker model inspect`
  shows the pulled model's own GGUF metadata as `llama.context_length:
  8192` — this is the model's own trained/max context, a different,
  weaker fact than the *runtime serving default* `docs/HISTORY.md`'s
  existing text claims is 4096 unless configured. No `/props`-equivalent
  endpoint exists to directly confirm the actual serving-time window (see
  above), and a large-prompt truncation test wasn't run this session — so
  the "4096 default" claim is neither confirmed nor refuted, and stays
  flagged unverified.
- **DMR's container networking, inspected directly**: `docker inspect`
  shows the `docker-model-runner` container publishes port 12434 bound to
  *both* `127.0.0.1:12434` and `172.17.0.1:12434` — the latter being the
  default bridge's gateway IP, i.e. deliberately reachable via the same
  `host.docker.internal` mechanism this project's `Dockerfile`/README
  already document for Ollama/llama-server.
- **Container→DMR connectivity failed initially, root-caused for real, and
  fixed**: from inside a throwaway container (`--add-host=host.docker.internal:host-gateway`),
  `curl http://host.docker.internal:12434/...` timed out (`HTTP 000`)
  even though the same address worked from the host itself. Traced by hand
  through `iptables`: `FORWARD` policy `DROP`, then confirmed the
  `DOCKER`-chain DNAT rule for port 12434 explicitly excludes
  `docker0`-sourced traffic (`!docker0` in its match, Docker's own
  anti-hairpin-loop default) — so the packet is delivered locally instead
  of NAT'd, and the host's `INPUT` chain (also policy `DROP` here, via a
  persisted `iptables`/`ufw` ruleset) drops it with no matching ACCEPT
  rule. A single scoped rule (`iptables -I INPUT -i docker0 -p tcp --dport
  12434 -j ACCEPT`) fixed it; re-tested afterward — both a plain `curl`
  and a real chat completion round-trip through
  `host.docker.internal:12434/engines/v1/chat/completions` succeeded from
  inside a container.

## Decisions

**1. Extend the existing `Dockerfile`/README "Docker" section** (the one
covering container distribution) **with a DMR-specific connection
subsection**, parallel to its existing Ollama/llama-server guidance:
same `--add-host=host.docker.internal:host-gateway` flag, but a different
`base_url` path — `http://host.docker.internal:12434/engines/v1`, not bare
`/v1` — and DMR's full model-reference string as `model`. No `Dockerfile`
changes: DMR runs on the host (confirmed above, as its own Docker
container, not something `aivyx-coder`'s own image can meaningfully bundle
or depend on at build time).

**2. Document the firewall/hairpin-NAT failure mode as a named
troubleshooting item**, concise and actionable in the README (symptom →
one-line cause → the exact fix command), explicitly flagged as
host-specific — not something `aivyx-coder` can detect, fix, or guarantee
against. The full `iptables`/DNAT chain-by-chain diagnosis goes in
`docs/HISTORY.md` instead, matching this project's established split
(README stays a practical how-to; `HISTORY.md` carries the full evidence
trail), the same split already used for the Docker/Landlock seccomp
finding.

**3. Narrow (not remove) the existing Serving-section DMR subsection's
"⚠️ Not yet live-verified" banner.** Base URL, model-naming convention, and
basic chat-completions all move from "unverified" to confirmed-live
2026-09-21, with `docs/HISTORY.md`'s existing chapter updated to match.
The context-window runtime default and end-to-end tool-calling stay
explicitly flagged unverified — this spec did not confirm either, and
overclaiming would repeat the exact mistake (`docs/HISTORY.md`'s own
stated lesson) this project has already called out once. The `probe.rs`
non-extension is upgraded from "unverified, blocked" to "confirmed
blocked" — no diagnostic endpoint exists at either shape checked.

**4. `ROADMAP.md`'s existing DMR backlog entry is narrowed, not
closed.** Base URL/model-naming/chat-completions verification is done;
context-window default and tool-calling-through-native-edit-format remain
open, restated as the entry's new remaining scope.

## What this spec does not decide

- Confirming DMR's actual runtime context-window default, or whether a
  Docker CUDA image hard-codes it — still open, no `/props`-equivalent
  endpoint exists to check it non-destructively; would need a real
  large-prompt truncation test.
- Tool/function-calling correctness through `aivyx-coder`'s native edit
  format against a DMR-served model — still open, not exercised this
  session.
- Any `probe.rs` code change — confirmed blocked (no diagnostic endpoint
  found), not merely deferred.
- Bundling/packaging DMR itself alongside `aivyx-coder`'s own image — not
  possible as stated (DMR is host-level, not something a downstream
  `Dockerfile` can `FROM`/install into its own image), and this spec's
  Decision 1 is the closest achievable version of that original idea.
- Any change to how a *native* (non-containerized) `aivyx-coder` connects
  to DMR — that path (`http://localhost:12434/engines/v1`, no
  `host.docker.internal`/firewall concerns) was already documented and
  isn't touched here beyond the banner narrowing in Decision 3.
