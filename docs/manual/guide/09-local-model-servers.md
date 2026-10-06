# Local model servers

aivyx-coder talks to any server with an OpenAI-compatible API on your own
machine or network. This chapter covers choosing and running one, sharing a
GPU with other programs, and the embedded engine.

**The one setting that matters most:** `[backend] context_tokens` must
match the context window your server **actually serves**. Too small a
served window makes replies stop mid-thought; too large a setting makes
aivyx-coder overfill it. Setup reads the served window where the server
reports it.

## Choosing a server
aivyx speaks the OpenAI-compatible `/v1` API, so any local server works.
A few documented options:

### Ollama — the quick start

Zero-setup, and the shipped default. One trap to
know: Ollama serves its *own* default context window (typically 4096)
unless the model sets `num_ctx` or the service sets
`OLLAMA_CONTEXT_LENGTH` — `/v1` cannot request more, and a too-small
window truncates responses mid-thought (reasoning models burn it
invisibly). aivyx probes for this at startup and warns in the transcript;
the fix is a derived model (see `context_tokens` in the [configuration reference](../reference/03-configuration.md)).

### llama-server — recommended for serious use

Same GGUF models and
kernels as Ollama, everything explicit — the hidden-window trap can't
exist when `-c` is on the command line.

*Installing it*: use your distro's packaged build where one exists (Arch:
AUR `llama.cpp-cuda`, which installs `/usr/bin/llama-server`), or the
prebuilt binaries from ggml-org's releases, or a source build. One
CUDA-build gotcha that has bitten twice here: ggml's cmake auto-adopts any
`ccache`/`sccache` it finds on PATH (`GGML_CCACHE` defaults ON), and
sccache-wrapped nvcc corrupts parallel builds with
`fatbinary: Could not open input file '*.cubin'` errors. Disable it
explicitly — for the AUR package:

```
LLAMA_BUILD_EXTRA_ARGS="-DGGML_CCACHE=OFF -DCMAKE_CUDA_ARCHITECTURES=89" makepkg -si
```

(arch `89` = RTX 40-series; use your GPU's compute capability. AUR helpers
don't always pass environment through to `build()` — if the build fails
with the cubin error, check `GGML_CCACHE` in the build dir's
`CMakeCache.txt` and prefer running `makepkg` directly.)

*Running it*:

```
llama-server -hf unsloth/Qwen3.5-9B-GGUF:Q4_K_M \
  -c 16384 -ngl 99 --jinja --cache-reuse 256 \
  --chat-template-kwargs '{"enable_thinking": false}' \
  --temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty 1.5 \
  --host 127.0.0.1 --port 8080
```

`--jinja` is load-bearing (native tool-call templating); `--cache-reuse`
keeps agent-loop prompts warm across tool round-trips. Point aivyx at it
with `base_url = "http://127.0.0.1:8080/v1"`. The startup probe reads
llama-server's `/props` and confirms the served window.

#### Sampling does not migrate from Ollama

Ollama applies the Modelfile's
sampling parameters server-side; llama-server uses its own generic
defaults instead — and reasoning models are sensitive to this. Always
pass the model family's recommended sampling flags (the values above are
Qwen's non-thinking set; `ollama show <model>` lists what Ollama used).

#### Disable thinking for agent work

Measured here: qwen3.5 on
continuation turns (after a tool result) emits its next tool call *inside
an unclosed think block*; llama-server's reasoning parser then classifies
the entire action as `reasoning_content` and the turn ends having done
nothing — and the same happens to prompted SEARCH/REPLACE blocks, so no
edit format escapes it. `--reasoning-budget 0` is inert on templates
without `enable_thinking` support; the switch that works is
`--chat-template-kwargs '{"enable_thinking": false}'`. With thinking
disabled, the aivyx edit benchmark went from 0/3 on affected tasks to
9/9 overall at ~2s per edit — thinking buys nothing for tool-driving on
this model class and costs both latency and, on llama-server, silent
no-op turns.

Prefer GGUFs from HuggingFace (`-hf repo:QUANT` downloads and caches
them). Reusing Ollama's blob files directly (`ollama show --modelfile`
reveals the path) sometimes works but is **not reliable**: Ollama's fork
writes metadata upstream llama.cpp may reject, and Ollama stores chat
templates outside the GGUF — a missing template silently breaks native
tool-calling (calls stream through as plain text).

Example systemd user unit (`~/.config/systemd/user/llama-server.service`):

```ini
[Unit]
Description=llama-server for aivyx
[Service]
ExecStart=/usr/bin/llama-server \
  -hf unsloth/Qwen3.5-9B-GGUF:Q4_K_M -c 16384 -ngl 99 \
  --jinja --cache-reuse 256 \
  --chat-template-kwargs '{"enable_thinking": false}' \
  --temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty 1.5 \
  --host 127.0.0.1 --port 8080
Restart=on-failure
[Install]
WantedBy=default.target
```

Not every Qwen3 release has a thinking toggle at all: the `-Instruct-2507`
line (e.g. `Qwen3-4B-Instruct-2507`) ships non-thinking only — no
`enable_thinking` template variable, no `<think>` tags regardless of
flags — so `--chat-template-kwargs` is simply inert there, not something
to debug if it appears to do nothing.

### Lemonade Server — a packaged alternative to llama-server

[lemonade-sdk/lemonade](https://github.com/lemonade-sdk/lemonade)
wraps llama.cpp (plus other backends) with model pull/load management and
a distro package (CachyOS/Arch: `lemonade-server`, binary `lemonade` +
`lemond` service) — its CUDA backend ships prebuilt binaries per compute
capability, sidestepping the `GGML_CCACHE` build gotcha above entirely.
Two things verified live (`docs/HISTORY.md` Phase 10) before pointing aivyx
at it:

- **Target the underlying llama-server port, not Lemonade's gateway
  port.** Lemonade spawns a real `llama-server` process per loaded model
  (find its port with `ss -tlnp | grep llama-server` or `ps aux | grep
  llama-server`) alongside its own stable gateway (`lemonade config`'s
  `port`, default 13305). The gateway's `/props` returns Lemonade's web-UI
  HTML, not JSON, and its Ollama-compatible `/api/show` always reports
  `"parameters": "num_ctx -1"` regardless of the model's actual loaded
  context — aivyx's startup probe parses that as "Ollama's hidden
  default" and emits a **false** truncation warning even when the model
  is correctly configured. Pointing `base_url` at the real llama-server
  port instead gets an accurate `/props` and a correct probe, identical
  to a native llama-server install — the tradeoff is that this port isn't
  a documented, stable Lemonade interface and may shift across restarts
  or model reloads.
- **`--llamacpp-args` needs single-quote-wrapping for flags carrying
  embedded JSON.** Lemonade's own argument splitter strips bare double
  quotes before they reach llama-server, so
  `--llamacpp-args "--chat-template-kwargs {\"enable_thinking\":false}"`
  silently breaks the JSON. Wrap the JSON in single quotes instead:
  ```
  lemonade load <model> --ctx-size 16384 \
    --llamacpp-args "--chat-template-kwargs '{\"enable_thinking\":false}' \
    --temp 0.7 --top-k 20 --top-p 0.8 --presence-penalty 1.5"
  ```

Once pointed correctly, everything else behaves exactly like a native
llama-server install: `ctx_size`/`--ctx-size` is an explicit first-class
control (no Ollama-style hidden default), and the Phase 10 acceptance
benchmark reproduced the native 9/9 / prompted 6/9 result exactly against
a Lemonade-managed `qwen3.5:9b`.

### Docker Model Runner

Base URL, model-naming convention, and basic chat completions
confirmed live 2026-09-21 against a real running instance (see
`docs/HISTORY.md`). ⚠️ **The context-window default and end-to-end
tool-calling below are still unverified.**

Docker Model Runner (DMR) serves local models through an
OpenAI-compatible API, integrated into the normal Docker workflow —
pull a model with `docker model pull <name>` (model names look like
`ai/qwen2.5-coder` or `ai/smollm2:360M-Q4_K_M`), then point aivyx at:

```toml
[backend]
base_url = "http://localhost:12434/engines/v1"
model = "ai/qwen2.5-coder"
```

(An explicit engine can also be named in the path —
`http://localhost:12434/engines/llama.cpp/v1` — if the plain form
doesn't resolve on your install.)

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

Tool/function calling is documented as supported (backed by llama.cpp),
but hasn't been checked end-to-end through aivyx's own native edit
format — this project's own experience is that serving configuration,
not the model, is usually the dominant variable for tool-call
reliability (see the Ollama-vs-llama-server serving verdict in
`ROADMAP.md`), so this is worth verifying directly rather than assuming
Docker's own claim transfers.


## KV-cache persistence

For a `llama-server` backend, aivyx can persist the model's KV-cache state
to disk across process restarts, via the standalone
[aivyx-kvcache](https://github.com/Aivyx-Agent/aivyx-kvcache) library. When
it engages, a fresh `aivyx-coder` process's first turn on a repo it has
seen before can skip re-prefilling the stable system prompt + tool
definitions + repo map, instead of paying that cost cold every time the
process restarts.

**Enable it**: set `kind = "llama_server"` under `[backend]`:

```toml
[backend]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.5:9b"
kind = "llama_server"
```

**The one load-bearing operational requirement**: `llama-server` itself
must be started with `--slot-save-path` pointed at *exactly* the
directory this config actually uses — by default
`~/.local/share/aivyx-coder/kvcache/slots` (on Linux; the exact default
path is platform-specific, resolved via the `directories` crate — see
its own docs for the macOS/Windows equivalents), or your own `[backend]
kvcache_store_path`'s `slots` subdirectory if you've set one (see
below — in particular, to share this store with a locally
delegated-from `aivyx-pa` process pointed at the same `llama-server`, see
`aivyx-pa`'s own `docs/MCP_RECIPES.md`). If `--slot-save-path` doesn't match this exact
path, saves and restores still succeed against llama-server's own
`--slot-save-path` directory — no error is surfaced — but the store's own
`fs::metadata` stat on *its* expected path (`.../kvcache/slots`) misses,
so every entry silently falls back to a 1-byte placeholder size instead of
the real (often hundreds-of-MB) file size. That defeats
`kvcache_max_bytes` below: nothing ever looks big enough to evict, so real
slot files accumulate on disk without limit until the mismatch is fixed.

```
llama-server -hf unsloth/Qwen3.5-9B-GGUF:Q4_K_M \
  -c 16384 -ngl 99 --jinja --cache-reuse 256 \
  --slot-save-path ~/.local/share/aivyx-coder/kvcache/slots \
  --host 127.0.0.1 --port 8080
```

**Disk budget**: the on-disk store evicts its least-recently-used entry
once it would exceed `kvcache_max_bytes` under `[backend]` (default 10
GiB):

```toml
[backend]
kvcache_max_bytes = 10737418240  # 10 GiB, the default; bytes, not GiB
```

**What this does and doesn't help.** The cached key covers the *stable
prefix* only — system prompt, tool definitions, and repo map — never
conversation history. That means it helps a **fresh process's first turn**
on a repo it's cached before; it does *not* help turn-to-turn reuse within
one already-running session, since `llama-server`'s own automatic prefix
caching (`--cache-reuse`) already handles that case on its own. Any change
to the repo map, the enabled tool set, or the system prompt mints a new
cache entry — a project whose repo map or tool config changes often will
see correspondingly lower hit rates.

If the `/props` probe at startup can't confirm a real `llama-server`
instance, or the store fails to open, KV-cache persistence is silently
disabled for that run (a `warn`-level log line, nothing else) — aivyx
never fails to start because of it. Likewise, if the multi-process slot
lock (below) can't be acquired for any reason, this process falls back to
starting from slot 0 rather than failing to start.

**Multi-process slot coordination.** When more than one `aivyx-coder`
process points at the same `llama-server`, each claims a distinct starting
slot via a real OS-level advisory file lock (`<store-path>/locks/`,
scoped by a stable hash of the `llama-server`'s own base URL) instead of
every process defaulting to slot 0 — no daemon or extra configuration
needed. The lock is held for the process's whole lifetime and releases
automatically on exit or crash. This is a cache-*efficiency* optimization,
not a correctness guarantee: once concurrent process count reaches the
server's real slot count, the overflow still contends (no worse than
before this existed), and it only coordinates `aivyx-coder` processes
against each other — see "Multi-process GPU sharing" below for what it
does not cover.

## Multi-process GPU sharing (`aivyx-broker`)

A single `llama-server` process only ever serves one GPU-resident model at
a time, and its `/slots` KV-cache mechanism (see "KV-cache persistence"
above) assumes one process is deciding which slot to use. Multiple
`aivyx-coder` processes pointed at the same `llama-server` now coordinate
their *starting* slot automatically (via a real OS-level file lock, no
daemon needed — see "KV-cache persistence" above), so they no longer both
default to slot 0. This does **not** extend across products: `aivyx-coder`
alongside `aivyx-pa` (or any other unrelated process using `llama-server`'s
`/slots` mechanism) still fights over slots uncoordinated, since only
`aivyx-coder` itself participates in this locking scheme.

[`aivyx-broker`](https://github.com/Aivyx-Agent/aivyx-broker) is a
standalone daemon that sits in front of a single shared `llama-server` and
coordinates GPU-slot access across every local process pointed at it — it
owns slot admission and the full restore/warm/save lifecycle itself, so no
individual client (this one included) does its own local slot-picking or
its own `aivyx-kvcache` restore/save calls on this path. See
`aivyx-broker`'s own README for how to install and run it — it's a
separate process from `aivyx-coder`, started independently.

**Enable it**: set `kind = "llama_server_broker"` and point
`broker_base_url` at your running `aivyx-broker` instance instead of at
`llama-server` directly:

```toml
[backend]
model = "qwen3.5:9b"
kind = "llama_server_broker"
broker_base_url = "http://127.0.0.1:8899"
```

`base_url` is no longer contacted directly by this process on this path —
`broker_base_url` is (with or without a trailing `/v1`; requests go to
`<broker_base_url>/v1/chat/completions` either way). `aivyx-broker` exposes the same
`/v1/chat/completions` shape as a plain OpenAI-compatible server, so this
is otherwise a drop-in swap; this repo's own contribution is just an
additive `aivyx_slot_hint` field (a prefix hash, plus an always-omitted
preferred-slot field — this client never picks or tracks a slot id itself)
attached to each outgoing request as a hint. The broker infers locality
purely from the prefix hash and performs its own slot assignment entirely
on its own side — its own occupancy tracking, not this client, is what
makes same-session requests keep landing on a fast, already-warmed slot.

## Embedded Rust-native inference

`aivyx-coder` can run a local LLM **inside its own process** by linking
against the `mistralrs` crate — the same capability `aivyx-pa` (the sibling
Personal Assistant product) shipped in its own Phase 134, ported here
with real token streaming from the start. Zero outbound network calls
during inference; no separate runtime server to install.

### Building with the embedded provider

```bash
# Lean build (default) — no mistralrs dependency, fast compile, small binary:
$ cargo build -p aivyx

# Embedded provider, CPU only — no C compiler, no CUDA toolkit, no Metal SDK required:
$ cargo build -p aivyx --features provider-mistral-rs

# Embedded provider with platform GPU acceleration — pick exactly one:
$ cargo build -p aivyx --features provider-mistral-rs-cuda       # NVIDIA
$ cargo build -p aivyx --features provider-mistral-rs-metal      # Apple Silicon
$ cargo build -p aivyx --features provider-mistral-rs-accelerate # Apple CPU
```

| Backend | Feature | Build prerequisite | Runtime |
|---|---|---|---|
| CPU | `provider-mistral-rs` | None | Any platform |
| CUDA | `provider-mistral-rs-cuda` | CUDA toolkit (>= 11.8) | NVIDIA GPU with CC >= 8.0 |
| Metal | `provider-mistral-rs-metal` | macOS + Xcode | Apple Silicon |
| Accelerate | `provider-mistral-rs-accelerate` | macOS + Xcode | Apple CPU |

### Config

```toml
[backend]
kind = "mistral_rs"
model = "qwen3-4b"  # display name; arbitrary string

# REQUIRED — absolute path to a GGUF file or a directory containing GGUF files.
mistralrs_model_path = "/home/you/models/Qwen3-4B-Q4_K_M.gguf"

# Optional — when mistralrs_model_path is a directory, names the specific file.
# mistralrs_model_file = "qwen3-4b-q4_k_m.gguf"

# Optional — chat template path. Omit to use the template embedded in the GGUF.
# mistralrs_chat_template_path = "/home/you/templates/qwen3.json"
```

### Recommended GGUF models

`aivyx-coder` doesn't bundle any model — download the GGUF yourself and
point `mistralrs_model_path` at it:

| Model | Size (Q4_K_M) | Min RAM | Use case | Download |
|---|---|---|---|---|
| **Qwen3-4B** | ~2.5GB | 6GB | Best general agent; strong tool calling | [HF: Qwen/Qwen3-4B-Instruct-GGUF](https://huggingface.co/Qwen) |
| **Llama-3.2-3B-Instruct** | ~2.0GB | 5GB | Conservative default; well-tested | [HF: bartowski/Llama-3.2-3B-Instruct-GGUF](https://huggingface.co/bartowski) |
| **Phi-4-mini-instruct** | ~2.4GB | 5GB | Microsoft tooling; XML tool-call format | [HF: microsoft/Phi-4-mini-instruct-gguf](https://huggingface.co/microsoft) |
| **SmolLM2-1.7B-Instruct** | ~1.1GB | 3GB | Smallest practical agent; CPU-friendly | [HF: HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF](https://huggingface.co/HuggingFaceTB) |

### When to pick embedded vs. Ollama/llama-server

- **Pick embedded** for a single-binary install with no separate runtime
  to manage, for zero outbound network calls during inference, or when
  recommending `aivyx-coder` to an operator who'd otherwise stall at
  "install Ollama first."
- **Stick with Ollama/llama-server** if you want `ollama pull <model>`
  as your download UX, or you already have one running and aren't
  motivated to rebuild.

### Honest tradeoffs

- **Build cost.** First build with `--features provider-mistral-rs`:
  ~5-10 minutes (mistralrs is a substantial crate; incremental builds
  after that are fast).
- **Binary size.** Release binary adds ~100-200MB on the CPU variant.
- **mistralrs is pre-1.0.** Pinned to `=0.8.*`; upgrades happen
  explicitly, matching aivyx's own upgrade-by-version contract.
- **Shared-lockfile cost, even for the default build.** Adding
  `mistralrs` as an optional dependency at all pins the whole
  workspace's `Cargo.lock` to `regex` 1.12.4 instead of 1.13.0 (via
  `mistralrs-core`'s own transitive `serde-saphyr` dependency, which
  requires `regex < 1.13`) — this applies even to a default (no
  `provider-mistral-rs`) build, since the lockfile is shared. `cargo
  tree -p aivyx-llm` confirms `mistralrs` itself contributes zero
  compiled code to a default build either way. (`aws-lc-rs` is *not* a
  new cost of this branch — it's already present in the default
  dependency graph via `rustls` 0.23's own default crypto provider.)
- **Per-model tool-call format quirks are unverified.** Models with
  non-standard tool-call formats may behave differently through
  mistral.rs's own extraction than through Ollama — not yet empirically
  validated against a real model in this environment (which has neither
  a GPU nor a downloaded GGUF file to test against).

