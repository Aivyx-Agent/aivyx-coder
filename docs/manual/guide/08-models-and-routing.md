# Models and routing

By default aivyx-coder sends everything to one model: the one in
`[backend]`. If you run several local models, **routing** picks one per
call instead — a big model for editing code, a small fast one for
summaries — and always says why it chose. It prefers a model that's
already loaded, so it doesn't keep swapping models in and out.

Routing is **off** until you turn it on. Off, nothing changes at all.
Selection is done by the shared
[`aivyx-route`](https://github.com/Aivyx-Agent/aivyx-route) library: hard
needs (tool calling, a big enough context window) rule candidates out, then
the call's task ranks the rest by tier and strengths.

## Using it day to day

| Command | What it does |
|---|---|
| `/models` | The candidates, what each can do, and what's loaded |
| `/models why` | Why the last call went where it did |
| `/models refresh` | Look for models on your servers again |
| `/model <id>` | Pin this conversation to one model |
| `/model auto` | Let routing choose again |

When the conversation first picks a model, or moves to another, the
transcript shows `routing → <model>: <reason>` and the status line shows
the model. The rest of this chapter is the full setup.

## Configuration

```toml
[routing]
enabled = true
discover = true           # default; probe each [routing.endpoints.*] at startup
# vram_bytes = 25769803776 # host GPU memory, for residency scoring without a broker

[routing.endpoints.gpu]
kind = "ollama"           # ollama | llama_router | openai_compat | lemonade
base_url = "http://localhost:11434"

[[routing.models]]
id = "qwen3-coder:30b"
endpoint = "gpu"          # omitted ⇒ "backend", i.e. the [backend] section
tier = "large"            # small | medium | large
strengths = ["code", "reasoning"]
priority = 10

[[routing.models]]
id = "llava:13b"
endpoint = "gpu"
tier = "small"
capabilities = ["vision"]        # added to what discovery found
capabilities_deny = ["tools"]    # removed from discovered and declared
# context_window = 16384         # the window the server actually serves

[routing.tasks]
summarize = { tier = "small" }
```

- The default endpoint is called **`backend`** and is the `[backend]`
  section itself. `[routing.endpoints.backend]` is a reserved name and is
  rejected at startup. The default endpoint is never auto-discovered.
- The `[backend] model` is always a candidate: if no `[[routing.models]]`
  entry names it, it is added as an implicit entry on `backend`. **Declare
  it yourself.** The implicit entry has unknown capabilities and an unknown
  context window, and routing ranks those below known ones, so it loses
  every main-loop call (which needs tools and a minimum window) to any
  discovered tool-capable model. Startup logs a warning when this happens.
  Add an entry like this, with the window your server actually serves:

  ```toml
  [[routing.models]]
  id = "qwen3.5:9b"                 # your [backend] model
  capabilities = ["tools"]
  context_window = 32768
  ```
- Every model discovery finds becomes a candidate, even without a
  `[[routing.models]]` entry. Such a model is `medium` tier with no
  strengths.
- Only local endpoints are accepted; `aivyx-coder` never routes to a cloud
  API. An endpoint is local or cloud by `aivyx-route`'s rule: `kind =
  "anthropic"` or `"openai"` is always cloud, and any other kind is local
  only when its `base_url` host is a loopback, private (`10/8`,
  `172.16/12`, `192.168/16`, `fc00::/7`), link-local or CGNAT/Tailscale
  (`100.64.0.0/10`) address, or a name that is `localhost`, has no dots,
  or ends in `.local`, `.lan`, `.internal` or `.home.arpa`. So
  `kind = "openai_compat"` at `https://api.groq.com/openai/v1` is cloud,
  and so is an `openai_compat` endpoint with no `base_url`. A cloud
  endpoint stops startup with an error. If a server on your own network
  has a public-looking name (`gpu.example.com`, a `*.ts.net` MagicDNS
  name), set `locality = "local"` on its `[routing.endpoints.*]` table;
  `locality = "cloud"` forces cloud.
- The `[backend]` model is judged by the same rule, from the address chat
  connects to (`broker_base_url` for `kind = "llama_server_broker"`, else
  `base_url`; the embedded `mistral_rs` backend is always local). With
  routing on, a `[backend]` that counts as cloud is not used for routed
  calls — the main loop, `/architect` without an `[architect]` model and
  `/commit` drafts are all routed — so unless another local model is a
  candidate, every turn fails with a routing error. Only calls that carry
  no routing hint still go to it. Startup says so in the transcript. If
  the server is on your own network, set `locality = "local"` in
  `[backend]`; otherwise add a capable local model to
  `[[routing.models]]`.
- Endpoints are configured as discovery sees them
  (`http://localhost:11434`); chat requests go to their
  OpenAI-compatible `/v1` path.
- `kind = "lemonade"` targets [Lemonade Server](https://github.com/lemonade-sdk/lemonade)'s
  own gateway (default `http://127.0.0.1:13305/api`, port 13305 — not the
  underlying llama-server port [Local model servers](09-local-model-servers.md) recommends for
  `[backend]`). Chat goes to `.../api/v1/chat/completions`. Lemonade holds
  one LLM at a time and reports which one via its own `/v1/health` +
  `/v1/models`, so — unlike a plain `openai_compat` endpoint — it is a real
  residency source (see "Model residency" below).
- **Discovery** (`discover`, default `true`) probes every
  `[routing.endpoints.*]` at startup (Ollama `/api/tags` + `/api/show`,
  llama-server router mode `/models`, or `/v1/models`). Endpoints are
  probed concurrently, and the whole run stops after 15 s: an endpoint
  that hasn't answered by then is unreachable and its models stay
  unavailable until `/models refresh`, and an Ollama model whose details
  hadn't arrived keeps its capabilities unknown. `discover = false` skips
  probing and uses only the roster. `/models refresh` re-runs discovery
  later.
- **Ollama's discovered context window is the model's trained maximum**,
  not the window Ollama serves by default (often 4096, see [Local model servers](09-local-model-servers.md)).
  Declare `context_window` for every Ollama model you route to.
- **Don't point a routing endpoint at the `[backend]` server** unless you
  mean to. The same model is then listed twice (`id@gpu` and
  `id@backend`), and the discovered copy wins unless the `backend` one is
  declared as above. KV-cache slot pinning only applies to the `@backend`
  copy.
- Tiers and strengths only ever come from the roster; no server reports
  model quality.

## Unknown capabilities

Some sources can't say what a model supports (`/v1/models` lists ids only;
a roster-only model has nothing discovered). Those capabilities are
*unknown*, not absent: the model can still be chosen for a call that needs
them, but ranks below any model known to have them, and the reason says
"assumed but unverified". Declare `capabilities` / `capabilities_deny` on
the roster entry to make them known either way.

## Which calls are routed

| Call | Task kind | Notes |
|---|---|---|
| Main agent loop | `code_edit` | Sticky per conversation: once a model is chosen it is kept while it still meets every hard need. The first call is chosen by ranking. |
| `/architect` | `plan` | Only when no `[architect]` section is configured. A configured `[architect]` is used as-is (an explicit pin) and is not a routing candidate. |
| Team specialists | `code_edit`, or the member's `task` | A roster member (`[team] roster_path`) may set `task = "judge"`, `"plan"`, or a custom `[routing.tasks]` name. Only `code_edit` and `chat` are sticky: a member with any other `task` is routed per call, so a multi-round specialist session may switch models. The team lead's `task` is not applied to the main agent, which stays `code_edit`. |
| `delegate_task` sub-agents, MCP-server sessions | `code_edit` | Each has its own sticky session, separate from the main conversation. `/models` and `/model` are not available to them. |

Other calls (KV-cache warm-up, council members, SVG generation, and other
side calls) are not tagged and go to the `[backend]` model exactly as
before.

## Failures and fallback

A call that fails with a retryable error (connection error, timeout, or
HTTP 404, 408, or 5xx) moves on to the next candidate, and the failed
model is skipped for 60 seconds. A model whose backend can't be built
(for example, its endpoint has no `base_url`) is skipped the same way. If nothing else qualifies while a model
is cooling down, the cooling models are retried anyway rather than the
call failing outright. Other errors (a 400, a malformed response) are
returned as-is, since they would fail the same way on any model. A
failure after the response has started streaming is not retried.

A fallback, or a choice made while some model was cooling down, is
temporary: the conversation returns to its own model once the cooldown
ends. A `/model` pin has no fallback.

## Model residency

Every 5 seconds, a background task polls whichever residency sources are
configured — Ollama (`/api/ps` + `/api/tags`), llama-server router mode
(`/models`), or Lemonade (`/v1/health` + `/v1/models`) on any
`[routing.endpoints.*]` entry of that kind, plus `[backend]` itself when it
is `kind = "llama_server"` (resident and router-mode-probed),
`kind = "llama_server_broker"` (`aivyx-broker`'s residency report, which
also supplies VRAM), or the embedded `kind = "mistral_rs"` backend (always
resident, nothing to poll); a `[backend]` that counts as cloud (see "Configuration" above) is never polled — and feeds the result to the router, which
prefers already-loaded models over ones that would need a load. There is no
`[backend] kind = "lemonade"`: point `[backend] kind = "generic"` at
`base_url = "http://127.0.0.1:13305/api/v1"` to use Lemonade as the default
backend (see [Local model servers](09-local-model-servers.md)), and list the same server under
`[routing.endpoints.*] kind = "lemonade"` as
`base_url = "http://127.0.0.1:13305/api"` (no `/v1`: discovery and
residency append `/v1/...` themselves) to get its residency reporting —
the `[backend]` copy itself has no residency signal, same as any other
`generic` endpoint. A Lemonade routing endpoint is never itself marked
resident (it reports per-model residency instead, and holds only one model
loaded at a time). `kind = "generic"` otherwise has no residency signal: it
may be Ollama's OpenAI-compatible API, which lists many models and loads on
demand. If a `generic` `[backend]` actually points at an Ollama server that
is also listed under `[routing.endpoints]`, the same model appears twice
(once `@backend`, once `@<endpoint>`), and only the `@<endpoint>` copy gets
residency. The poll never blocks a call — it only ever makes a prior,
cheaper decision available to the next one —
and is skipped entirely when no source is configured. `/models` shows
the current snapshot.

## Commands

| Command | What it does |
|---|---|
| `/models` | Lists the candidates: `id@endpoint`, tier, capabilities (unknown ones marked `?`), context window, availability, and (when a residency source is configured) a `Residency:` block of loaded / needs-load / may-not-fit models and the host's VRAM. `*` marks this conversation's current model; a pin is marked `(pinned)`. |
| `/models refresh` | Re-runs discovery, rebuilds the candidate list, and clears every cooldown. |
| `/models why` | The last routing decision in this conversation and its reason. |
| `/model <id>` / `/model <id@endpoint>` | Pins this conversation's main loop to that model. A bare id must be served by exactly one endpoint. A pin that lacks a hard need is used anyway, with a warning in the reason. If `/models refresh` drops the pinned model, the pin is ignored (routing chooses as if unpinned, and the reason starts "ignored pin to") but kept, so it applies again once the model is back. |
| `/model auto` | Clears the pin; routing chooses this conversation's model again on the next call. `/model` alone shows the current pin. |

With routing off, these commands reply that they need
`[routing] enabled = true` and the interactive session. They are handled
inside the agent, so they work in both the TUI and ACP.
`/clear` forgets the conversation's current model but keeps a `/model`
pin.

On the conversation's first routed call, and whenever it moves to a
different model, the TUI prints a `routing → <model>: <reason>` notice and the status line shows
`model <id@endpoint>`.

## Interactions with other settings

- **KV-cache slot pinning** (`id_slot`) and broker slot hints apply only
  to the `[backend]` model. A call routed to any other model is sent
  without them.
- **`[backend] context_tokens` still drives compaction** and the status
  line's context budget for every model. Set it to the smallest context
  window you route to, or declare `context_window` per roster entry so
  routing avoids models whose window is smaller than the prompt.
- With the embedded mistral.rs backend, the `backend` endpoint serves only
  the configured model.

