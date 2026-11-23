# Changelog

## [2.1.1] - Unreleased

### Fixed

- Config notes (old spellings such as `locality = "cloud"`, and unknown keys) now show in the TUI transcript at start, instead of on stderr behind the TUI. Headless and `--acp` show them too (ACP as start-up warnings); `--mcp-server`, `aivyx-coder pack` and a failed agent build print them to stderr.

## [2.1.0] - 2026-11-23

### Changed

- The routing tier for a model server on your network that you trust less than this machine is now called "shared" (aivyx-route 0.3): write `locality = "shared"`. aivyx-coder still refuses a shared endpoint at startup, as it did a "cloud" one. `locality = "cloud"` is read as `"shared"` until 3.0, with a startup note: ``locality = "cloud" is now "shared"; the old name works until 3.0``.
- Downgrading: a 2.0 binary can't read a config with `locality = "shared"` (the stable config surface, `docs/stable/config.txt`, now shows `local | shared`, and 2.1 serializes the tier as `"shared"`). To go back to 2.0, change it to `"cloud"` first.
- Building backends, council seats and the architect now goes through the new `connect*` constructors instead of an ad-hoc `spawn_blocking` wrapper. Tests now prove that every async local-only check (construction, before each request, routed models) runs off the runtime thread.
- aivyx-llm: new async constructors `OpenAiCompatBackend::connect`, `connect_with_idle_timeout` and `connect_with_resolver`, which run the local-only check off the async runtime. The sync constructors are unchanged and documented as blocking.

### Fixed

- The `--setup` wizard's base-URL prompt and its local-only check no longer block the async runtime.

## [2.0.0] - 2026-11-23

aivyx-coder is local-only: models run on your machine or your own network (llama-server, Ollama, Jan, vLLM, Lemonade).

### Breaking

- The cloud routing kinds `anthropic` and `openai` are removed. A config that names one fails to load with `provider "<kind>" was removed in 2.0`.
- Every model URL must be local (loopback, private LAN (RFC 1918), link-local, IPv6 ULA (`fc00::/7`), Tailscale/CGNAT (`100.64.0.0/10`), a `.local`/`.lan`/`.internal`/`.home.arpa` name, or a single-label name; a DNS name counts only if every address it resolves to is local). This is checked at load, before each request, and at connect time. A non-local URL stops startup with `<url> is not local (<reason>) — aivyx is local-only; point it at a model server on this machine or your own network`.
- Redirects from a model server are refused, never followed.
- aivyx-llm API changes: `OpenAiCompatBackend::new` and `with_idle_timeout` return `Result<Self, NotLocal>`; new `NotLocal` variants (`LlmError::NotLocal`, `ServedContext::NotLocal`, `ListModelsError::NotLocal`); new `aivyx_llm::local::require_local_with` and `OpenAiCompatBackend::with_resolver`; `wrap_with_routing` returns a `Wrapped` struct (carrying the routing notice); `model_client_builder()` refuses redirects and installs the local-only resolver.
- A non-local `api_key` host warning is gone, superseded by the hard failure. `[vision] mold_url` must be local when vision is on.

### Migration

If a `[routing.endpoints.*]` table uses `kind = "anthropic"` or `"openai"`, change it to a local kind (`ollama`, `llama_router`, `openai_compat` or `lemonade`) with a local `base_url`, and list models that server serves. If your model server has a DNS name on your own network (for example `gpu.example.com`), set `locality = "local"` on its routing endpoint. Stored API keys are not deleted. See the models and routing chapter of the manual.

### Security

- `url` 2.5.9 ([RUSTSEC-2026-0592](https://rustsec.org/advisories/RUSTSEC-2026-0592): a stack overflow in `Url::origin()` on nested `blob:` URLs).

## [1.1.0] — 2026-11-18

### Changed

- `aivyx-coder pack install` now refuses packs that fail the new checks (`AGENTS.md` must name each of the pack's skills). A near-miss tool name only warns at install and the install proceeds; `pack check` still fails on it.

### Added

- `aivyx-coder pack check` (and `pack install`) now flags a backticked name in a pack's skills or AGENTS.md that looks like a misspelt aivyx-coder tool (an error in `pack check`, a warning at install), and checks that AGENTS.md names each of the pack's skills.
