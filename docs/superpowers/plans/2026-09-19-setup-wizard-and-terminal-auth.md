# Setup Wizard + ACP Terminal Auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `aivyx-coder --setup`, a real interactive first-run wizard
(pick a backend, pick/verify a model, write `config.toml`), and wire it
as a genuine ACP `terminal` authentication method so Zed/JetBrains/any
ACP client can launch it inline before the agent is usable. This is the
first, plan-worthy half of the ACP registry listing initiative — see
`docs/superpowers/specs/2026-09-19-acp-registry-listing-design.md` for
the full rationale. Release-platform expansion and the actual registry
PR are separate, later work this plan doesn't touch.

**Architecture:** Three small, focused pieces. (1) `aivyx-llm` gains two
model-listing helpers (`list_ollama_models`, `list_openai_compatible_models`),
each split into a network call plus a pure parse function — matching this
crate's own existing convention in `probe.rs` (`probe_served_context` +
`parse_llama_props`/`parse_ollama_show`), where only the pure parse
functions get unit tests, not the network wrapper. (2) `aivyx-config`
gains one new, narrowly-scoped public method reusing the existing
private, first-run-only `write_to` internally — no new write path, no
change to its documented 0600-at-create-time safety property, since this
plan deliberately keeps the wizard first-run-only (re-running on an
already-configured install refuses immediately, before any prompts —
re-runnable reconfiguration is explicitly out of scope, a separate future
decision). (3) A new `setup_wizard` module in the `aivyx` binary crate
does the actual interactive flow, split into a pure "turn collected
answers into `BackendSettings`" function (tested) and a thin, untested-by-unit-test
I/O layer using `dialoguer` (new dependency — nothing in this workspace
does interactive CLI prompts yet) that calls pieces (1) and the existing
`aivyx_llm::probe::probe_served_context` to verify the choice before
writing.

**Tech Stack:** Rust, `dialoguer` (new — interactive CLI select/input
prompts), `agent-client-protocol`'s `unstable_auth_methods` Cargo feature
(new, deliberate — see Global Constraints).

## Global Constraints

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --check` must stay clean.
- **`AuthMethod::Terminal` requires the `unstable_auth_methods` Cargo
  feature on `agent-client-protocol`** (confirmed by reading the crate's
  own `Cargo.toml`: `default = []`, `unstable_auth_methods` is not part
  of any default feature set). This is a real, deliberate dependency on
  an explicitly-unstable corner of a third-party crate's Rust API
  surface — the underlying wire-protocol shape (`type: "terminal"` in
  `authMethods`) is a real, documented, registry-CI-checked-for part of
  ACP itself (confirmed directly against
  `https://agentclientprotocol.com/rfds/auth-methods`), so this is a
  crate-API-stability risk, not a protocol-legality risk. Enable only
  `unstable_auth_methods` specifically, not the umbrella `unstable`
  feature (which also pulls in unrelated unstable surfaces this plan
  doesn't need: elicitation, session-fork, MCP-over-ACP).
- **The wizard is first-run-only.** `aivyx-coder --setup` must check
  whether `config.toml` already exists *before* running any interactive
  prompt, and refuse immediately with a clear message if it does — never
  overwrite an existing config. This is a deliberate scope decision
  (confirmed with the project owner), not an oversight: re-runnable
  reconfiguration would require fixing a real, already-documented gap in
  `Settings::write_to`'s 0600-permission-tightening approach (it only
  applies at file-creation time), which is explicitly out of scope here.
- The new `aivyx-config` method must reuse the existing private
  `write_to` internally, not duplicate its file-writing logic — the
  0600-at-create/0700-parent-dir safety properties must stay
  single-sourced.
- `list_ollama_models`/`list_openai_compatible_models` follow
  `probe.rs`'s own established split (network call + pure parse
  function); only the parse functions get unit tests, matching
  `parse_llama_props`/`parse_ollama_show`'s own precedent — don't
  introduce a new HTTP-mocking test dependency (e.g. `wiremock`) this
  crate doesn't already use.
- Re-read every file cited by line number below before editing — accurate
  as of this plan's own research (2026-09-19) but the repo moves.
- **Deviation from the spec's literal wording, confirmed correct at plan
  time:** the design spec's Decision 2 says "a new subcommand,
  `aivyx-coder setup`," written on the assumption `clap` subcommands
  already exist in this binary. Direct research while writing this plan
  found `crates/aivyx/src/main.rs`'s `Cli` is actually a single flat,
  flag-based struct (`--acp`, `--mcp-server`, `--resume`, etc., all
  `#[arg(long)]`, no subcommand enum at all) — there is no subcommand
  list for `setup` to join or collide with. This plan therefore
  implements it as `--setup`, matching the existing flag pattern exactly,
  not a new subcommand. Re-verify this against the real current
  `main.rs` before Task 4 — if subcommands have since been introduced,
  ask before deciding which form to use rather than assuming this note
  still holds.

---

## Task 1: `aivyx-llm` model-listing helpers

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-llm/src/list_models.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-llm/src/lib.rs`

**Interfaces:**
- Produces: `pub async fn list_ollama_models(base_url: &str) -> Result<Vec<String>, ListModelsError>`,
  `pub async fn list_openai_compatible_models(base_url: &str) -> Result<Vec<String>, ListModelsError>`,
  `pub enum ListModelsError { Transport(String), Unsupported }` (`Unsupported`
  for a well-formed-but-empty/unrecognized response — e.g. a server with no
  `/v1/models` route returning a non-JSON 404 body — distinct from a real
  transport failure, so callers can fall back to manual entry with a
  different message).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ollama_tags_response() {
        let json = serde_json::json!({
            "models": [
                {"name": "qwen3.5:9b", "size": 123},
                {"name": "llama3.2:3b", "size": 456}
            ]
        });
        assert_eq!(
            parse_ollama_tags(&json),
            vec!["qwen3.5:9b".to_string(), "llama3.2:3b".to_string()]
        );
    }

    #[test]
    fn parses_ollama_tags_response_with_no_models_field() {
        assert_eq!(parse_ollama_tags(&serde_json::json!({})), Vec::<String>::new());
    }

    #[test]
    fn parses_openai_models_response() {
        let json = serde_json::json!({
            "data": [
                {"id": "gpt-4o", "object": "model"},
                {"id": "gpt-4o-mini", "object": "model"}
            ]
        });
        assert_eq!(
            parse_openai_models(&json),
            vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()]
        );
    }

    #[test]
    fn parses_openai_models_response_with_no_data_field() {
        assert_eq!(parse_openai_models(&serde_json::json!({})), Vec::<String>::new());
    }

    #[test]
    fn parses_openai_models_response_skipping_entries_with_no_id() {
        let json = serde_json::json!({
            "data": [
                {"id": "gpt-4o"},
                {"object": "model"}
            ]
        });
        assert_eq!(parse_openai_models(&json), vec!["gpt-4o".to_string()]);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-llm list_models:: -- --nocapture
```

Expected: compile error — `list_models` module, `parse_ollama_tags`,
`parse_openai_models` don't exist yet.

- [ ] **Step 3: Implement**

```rust
//! Lists models a locally-running backend actually has available, so
//! `aivyx-coder --setup` can offer a real pick-list instead of asking
//! the operator to type a model name blind. Split into a network call
//! plus a pure parse function each, matching `probe.rs`'s own
//! established convention (only the parse functions are unit-tested).

use std::time::Duration;

const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum ListModelsError {
    #[error("could not reach the server: {0}")]
    Transport(String),
    #[error("server did not return a recognizable model list")]
    Unsupported,
}

/// Lists models an Ollama server has already pulled, via its native
/// `/api/tags` endpoint (not the OpenAI-compat `/v1/models`, which
/// Ollama also exposes but with less reliable coverage of locally-pulled
/// models across versions).
pub async fn list_ollama_models(base_url: &str) -> Result<Vec<String>, ListModelsError> {
    let origin = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let client = reqwest::Client::builder()
        .timeout(LIST_MODELS_TIMEOUT)
        .build()
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    let response = client
        .get(format!("{origin}/api/tags"))
        .send()
        .await
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ListModelsError::Unsupported);
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| ListModelsError::Unsupported)?;
    let models = parse_ollama_tags(&json);
    if models.is_empty() {
        return Err(ListModelsError::Unsupported);
    }
    Ok(models)
}

fn parse_ollama_tags(json: &serde_json::Value) -> Vec<String> {
    json.get("models")
        .and_then(|v| v.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Lists models a generic OpenAI-compatible server (llama-server, vLLM,
/// ...) reports via the standard `GET /v1/models` route. Not every
/// server implements this — a non-success status or unparseable body is
/// `Unsupported`, not a hard error, so the wizard can fall back to
/// manual entry.
pub async fn list_openai_compatible_models(
    base_url: &str,
) -> Result<Vec<String>, ListModelsError> {
    let origin = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let client = reqwest::Client::builder()
        .timeout(LIST_MODELS_TIMEOUT)
        .build()
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    let response = client
        .get(format!("{origin}/v1/models"))
        .send()
        .await
        .map_err(|e| ListModelsError::Transport(e.to_string()))?;
    if !response.status().is_success() {
        return Err(ListModelsError::Unsupported);
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|_| ListModelsError::Unsupported)?;
    let models = parse_openai_models(&json);
    if models.is_empty() {
        return Err(ListModelsError::Unsupported);
    }
    Ok(models)
}

fn parse_openai_models(json: &serde_json::Value) -> Vec<String> {
    json.get("data")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("id").and_then(|id| id.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // (Step 1's tests land here.)
}
```

Save as `crates/aivyx-llm/src/list_models.rs`.

- [ ] **Step 4: Wire the module into `lib.rs`**

Add `pub mod list_models;` to `crates/aivyx-llm/src/lib.rs` (check its
current module-list style — likely alongside an existing `pub mod probe;`
line — and match its exact ordering/placement convention).

- [ ] **Step 5: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-llm list_models:: -- --nocapture
```

Expected: all 5 new tests pass.

- [ ] **Step 6: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-llm
cargo clippy -p aivyx-llm --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-llm/src/list_models.rs crates/aivyx-llm/src/lib.rs
git commit -m "feat: add aivyx-llm model-listing helpers for Ollama and OpenAI-compatible servers

list_ollama_models (GET /api/tags) and list_openai_compatible_models
(GET /v1/models), each split into a network call plus a pure parse
function -- matching probe.rs's own established convention, where only
the parse functions get unit tests. Lets a future setup wizard offer a
real pick-list instead of asking the operator to type a model name
blind."
```

---

## Task 2: `aivyx-config` — safe first-run-only write path

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-config/src/lib.rs`

**Interfaces:**
- Produces: `pub fn write_if_absent(&self) -> Result<PathBuf, ConfigError>`
  (writes `self` to the standard config path via the existing private
  `write_to`, but ONLY if the file doesn't already exist — returns
  `ConfigError::AlreadyExists { path: PathBuf }`, a new variant, if it
  does; returns the path written to on success, so the wizard can print
  it).

- [ ] **Step 1: Write the failing tests**

Add to `aivyx-config`'s existing test module (find its exact location and
match its existing fixture style — likely a `tempfile::tempdir()`-based
pattern, check the file's other tests for the precise idiom before
writing these):

```rust
#[test]
fn write_if_absent_writes_a_fresh_file_and_returns_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // (Check how existing tests override `config_path()` -- likely via
    // an env var or a test-only constructor; match that exact mechanism
    // rather than assuming one here.)
    let settings = Settings {
        backend: BackendSettings {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let written = settings.write_if_absent_at(&path).unwrap();
    assert_eq!(written, path);
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("qwen3.5:9b"));
}

#[test]
fn write_if_absent_refuses_when_the_file_already_exists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "provider = \"already here\"\n").unwrap();
    let settings = Settings::default();
    let err = settings.write_if_absent_at(&path).unwrap_err();
    assert!(matches!(err, ConfigError::AlreadyExists { .. }));
    // The pre-existing file must be untouched.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(raw, "provider = \"already here\"\n");
}
```

(Note: the brief above sketches a testable `write_if_absent_at(&self,
path: &Path)` — check `write_to`'s exact signature and whether the
existing test suite already has a path-parameterized variant to test
against, or only tests the real XDG-resolved path indirectly. If
`write_to` itself already takes an explicit `&Path` (it does, per
`fn write_to(&self, path: &Path)`), the new public method should be a
thin wrapper: `write_if_absent_at(&self, path: &Path)` doing the
existence check + delegating to `write_to`, with the real
`write_if_absent(&self)` (no path argument) resolving the standard
config path via the same helper `load()` uses and calling
`write_if_absent_at` — write both, test only the path-parameterized one
directly, matching how other tests in this file that need a real
filesystem path already avoid touching the real XDG directory.)

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config write_if_absent -- --nocapture
```

Expected: compile error — `write_if_absent_at`, `ConfigError::AlreadyExists`
don't exist yet.

- [ ] **Step 3: Implement**

Add a new `ConfigError` variant (find the enum's current definition and
add alongside its existing variants, matching their exact `thiserror`
attribute style):

```rust
    #[error("config file already exists at {path:?} -- edit it directly, or delete it to re-run setup")]
    AlreadyExists { path: PathBuf },
```

Add two new public methods on `Settings`, near `write_to`:

```rust
    /// Writes `self` to `path`, but only if nothing is there yet -- never
    /// overwrites an existing config file. Reuses the existing, private
    /// `write_to` internally (same 0600-at-create/0700-parent-dir
    /// guarantees, single-sourced), so this carries no new file-writing
    /// logic of its own. Returns the path written to on success.
    pub fn write_if_absent_at(&self, path: &Path) -> Result<PathBuf, ConfigError> {
        if path.exists() {
            return Err(ConfigError::AlreadyExists {
                path: path.to_path_buf(),
            });
        }
        self.write_to(path)?;
        Ok(path.to_path_buf())
    }

    /// Same as `write_if_absent_at`, resolved against the standard XDG
    /// config path (the same one `load()` itself uses).
    pub fn write_if_absent(&self) -> Result<PathBuf, ConfigError> {
        let path = Self::config_path()?;
        self.write_if_absent_at(&path)
    }
```

(Confirm the exact name of the existing private helper that resolves the
standard config path — referenced above as `Self::config_path()` to
match `load()`'s own `let path = Self::config_path()?;` call, re-verify
this against the real current source rather than assuming the name.)

- [ ] **Step 4: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config write_if_absent -- --nocapture
```

Expected: both new tests pass, plus every pre-existing `aivyx-config`
test still passes.

- [ ] **Step 5: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-config
cargo clippy -p aivyx-config --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx-config/src/lib.rs
git commit -m "feat: add Settings::write_if_absent -- a safe, first-run-only write path

Reuses the existing private write_to internally (same 0600-at-create/
0700-parent-dir guarantees, single-sourced, no new file-writing logic).
Refuses with a new ConfigError::AlreadyExists variant rather than
overwriting an existing config.toml. Deliberately first-run-only --
write_to's own doc comment already flags that overwriting an existing
file would need a different (currently unimplemented) permission-
tightening approach; a future setup wizard built on this stays scoped to
fresh installs only, not a general reconfigure path."
```

---

## Task 3: The setup wizard

**Files:**
- Create: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/src/setup_wizard.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/Cargo.toml`

**Interfaces:**
- Consumes: `aivyx_llm::list_models::{list_ollama_models, list_openai_compatible_models}`
  (Task 1), `aivyx_llm::probe::probe_served_context` (existing),
  `aivyx_config::Settings::write_if_absent` (Task 2).
- Produces: `pub async fn run() -> anyhow::Result<()>` — the whole
  wizard's entry point, called from `main.rs` (Task 4). Internally:
  `pub(crate) fn backend_settings_from_answers(answers: &WizardAnswers) -> BackendSettings`
  (pure, tested) and `pub(crate) struct WizardAnswers { backend_choice: BackendChoice, base_url: String, model: String }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_settings_from_answers_maps_ollama_choice_to_generic_kind() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::Ollama,
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
        };
        let settings = backend_settings_from_answers(&answers);
        assert_eq!(settings.base_url, "http://localhost:11434/v1");
        assert_eq!(settings.model, "qwen3.5:9b");
        assert_eq!(settings.kind, aivyx_config::BackendKind::Generic);
    }

    #[test]
    fn backend_settings_from_answers_maps_generic_openai_choice_the_same_way() {
        let answers = WizardAnswers {
            backend_choice: BackendChoice::GenericOpenAiCompatible,
            base_url: "http://localhost:8080/v1".to_string(),
            model: "some-model".to_string(),
        };
        let settings = backend_settings_from_answers(&answers);
        assert_eq!(settings.base_url, "http://localhost:8080/v1");
        assert_eq!(settings.model, "some-model");
        assert_eq!(settings.kind, aivyx_config::BackendKind::Generic);
    }

    #[test]
    fn default_base_url_for_ollama_is_the_well_known_local_port() {
        assert_eq!(default_base_url(BackendChoice::Ollama), "http://localhost:11434/v1");
    }

    #[test]
    fn default_base_url_for_generic_is_llama_server_s_well_known_local_port() {
        assert_eq!(
            default_base_url(BackendChoice::GenericOpenAiCompatible),
            "http://localhost:8080/v1"
        );
    }
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx setup_wizard:: -- --nocapture
```

Expected: compile error — `setup_wizard` module doesn't exist yet.

- [ ] **Step 3: Add `dialoguer`**

In `crates/aivyx/Cargo.toml`'s `[dependencies]`, add:

```toml
dialoguer = "0.11"
```

(No other crate in this workspace does interactive CLI prompts yet —
confirm this is still the case, and confirm `0.11` is a reasonable
current version to pin, before finalizing; this is a new, real
dependency addition, not something to add without a moment's check.)

- [ ] **Step 4: Implement**

```rust
//! `aivyx-coder --setup`: an interactive first-run wizard that picks a
//! backend, picks (or verifies) a model, and writes `config.toml`.
//! First-run only -- refuses immediately if a config already exists,
//! before any prompt. See `docs/superpowers/specs/
//! 2026-09-19-acp-registry-listing-design.md` for the full rationale,
//! including why this is also wired as an ACP `terminal` auth method
//! (Task 4).
//!
//! Split into a pure decision layer (`backend_settings_from_answers`,
//! `default_base_url` -- both tested) and a thin interactive-I/O layer
//! (`run`, not unit-tested -- real stdin/stdout via `dialoguer`) that
//! collects a `WizardAnswers` and calls Task 1's model-listing helpers
//! plus the existing `probe_served_context` before writing.

use aivyx_config::{BackendKind, BackendSettings, Settings};
use aivyx_llm::list_models::{list_ollama_models, list_openai_compatible_models};
use aivyx_llm::probe::{ServedContext, probe_served_context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendChoice {
    Ollama,
    GenericOpenAiCompatible,
}

#[derive(Debug, Clone)]
pub(crate) struct WizardAnswers {
    pub(crate) backend_choice: BackendChoice,
    pub(crate) base_url: String,
    pub(crate) model: String,
}

pub(crate) fn default_base_url(choice: BackendChoice) -> &'static str {
    match choice {
        BackendChoice::Ollama => "http://localhost:11434/v1",
        BackendChoice::GenericOpenAiCompatible => "http://localhost:8080/v1",
    }
}

pub(crate) fn backend_settings_from_answers(answers: &WizardAnswers) -> BackendSettings {
    BackendSettings {
        base_url: answers.base_url.clone(),
        model: answers.model.clone(),
        kind: BackendKind::Generic,
        ..Default::default()
    }
}

/// The wizard's real entry point -- checks for an existing config first,
/// then runs the interactive flow, verifies the choice, and writes.
pub async fn run() -> anyhow::Result<()> {
    let config_path = aivyx_config::Settings::config_path()?;
    if config_path.exists() {
        println!(
            "config.toml already exists at {}. Edit it directly, or delete it and re-run --setup.",
            config_path.display()
        );
        return Ok(());
    }

    let backend_choice = prompt_backend_choice()?;
    let base_url = prompt_base_url(backend_choice)?;
    let model = prompt_model(backend_choice, &base_url).await?;

    println!("Verifying {model} at {base_url} ...");
    match probe_served_context(&base_url, &model).await {
        ServedContext::Known(n) => println!("  served context window: {n} tokens"),
        ServedContext::OllamaDefaultUnknown => println!(
            "  warning: this model's served context window could not be determined -- \
             Ollama serves a 4096-token default unless the model or service says \
             otherwise; set context_tokens in config.toml once you know the real value"
        ),
        ServedContext::Unknown => {
            println!("  warning: could not verify the server responded at all -- writing config anyway")
        }
    }

    let answers = WizardAnswers {
        backend_choice,
        base_url,
        model,
    };
    let backend = backend_settings_from_answers(&answers);
    let settings = Settings {
        backend,
        ..Default::default()
    };
    let written = settings.write_if_absent()?;
    println!("Wrote {}", written.display());

    if std::env::var("AIVYX_CODER_ACP_TERMINAL_AUTH").is_ok() {
        println!("Setup complete -- reconnecting...");
    } else {
        println!("Setup complete. Run `aivyx-coder` to start.");
    }

    Ok(())
}

fn prompt_backend_choice() -> anyhow::Result<BackendChoice> {
    let choices = ["Ollama (recommended, zero setup)", "A running OpenAI-compatible server (llama-server, vLLM, ...)"];
    let selection = dialoguer::Select::new()
        .with_prompt("Which backend are you using?")
        .items(&choices)
        .default(0)
        .interact()?;
    Ok(if selection == 0 {
        BackendChoice::Ollama
    } else {
        BackendChoice::GenericOpenAiCompatible
    })
}

fn prompt_base_url(choice: BackendChoice) -> anyhow::Result<String> {
    let default = default_base_url(choice);
    let base_url: String = dialoguer::Input::new()
        .with_prompt("Base URL")
        .default(default.to_string())
        .interact_text()?;
    Ok(base_url)
}

async fn prompt_model(choice: BackendChoice, base_url: &str) -> anyhow::Result<String> {
    let listed = match choice {
        BackendChoice::Ollama => list_ollama_models(base_url).await,
        BackendChoice::GenericOpenAiCompatible => list_openai_compatible_models(base_url).await,
    };
    match listed {
        Ok(models) if !models.is_empty() => {
            let selection = dialoguer::Select::new()
                .with_prompt("Model")
                .items(&models)
                .default(0)
                .interact()?;
            Ok(models[selection].clone())
        }
        _ => {
            let model: String = dialoguer::Input::new()
                .with_prompt("Model (could not list available models -- enter one manually)")
                .interact_text()?;
            Ok(model)
        }
    }
}

#[cfg(test)]
mod tests {
    // (Step 1's tests land here.)
}
```

Save as `crates/aivyx/src/setup_wizard.rs`.

(Confirm `aivyx_config::Settings::config_path()` is genuinely `pub` — it's
referenced as `Self::config_path()` inside `load()` today, which only
requires it to be visible within the crate; if it's not already `pub`,
promote it as a small, additional part of this task, matching Task 2's
own "reuse existing internals, don't duplicate" principle.)

- [ ] **Step 5: Wire the module into the crate**

Add `mod setup_wizard;` to `crates/aivyx/src/main.rs` (near its other
`mod` declarations, if any are present at the crate-root level — check
current structure; if `main.rs` has no existing `mod` list, this is a
new top-level module declaration).

- [ ] **Step 6: Run to verify they pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx setup_wizard:: -- --nocapture
```

Expected: all 4 new tests pass.

- [ ] **Step 7: Run full crate check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx
cargo clippy -p aivyx --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 8: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx/src/setup_wizard.rs crates/aivyx/src/main.rs crates/aivyx/Cargo.toml
git commit -m "feat: add the setup_wizard module (interactive first-run backend/model setup)

Pure decision logic (backend_settings_from_answers, default_base_url)
tested directly; the real interactive flow (dialoguer-based prompts,
Task 1's model-listing helpers, the existing probe_served_context for
verification, Task 2's write_if_absent) is a thin, manually-verified I/O
layer on top. First-run only -- checks for an existing config.toml
before any prompt. Not yet wired into main.rs's CLI dispatch or ACP's
terminal auth -- Task 4."
```

---

## Task 4: Wire `--setup` into the CLI, wire ACP `terminal` auth, docs, final verification

**Files:**
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx/src/main.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-acp/Cargo.toml`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-acp/src/session.rs`
- Modify: `/home/julian/Projects/Rust/aivyx-coder/README.md`

**Interfaces:** none new — wiring + documentation only.

- [ ] **Step 1: Add the `--setup` flag**

In `crates/aivyx/src/main.rs`'s `Cli` struct, add a new field matching
the existing flags' exact doc-comment/attribute style (near `--acp`/
`--mcp-server`, since it's the same kind of "alternate run mode" flag):

```rust
    /// Run the interactive first-run setup wizard (pick a backend, pick
    /// a model, write config.toml) instead of starting the agent.
    /// First-run only -- refuses if config.toml already exists. Also the
    /// entry point Zed/JetBrains/other ACP clients launch for this
    /// agent's "terminal" authentication method (see aivyx-acp's own
    /// InitializeResponse wiring).
    #[arg(long)]
    setup: bool,
```

At the very top of `main()`, before `Settings::load()?` is ever called
(since `--setup` must run *before* the existing first-run-writes-defaults
behavior in `load()` would otherwise silently create the file this
wizard is specifically supposed to create interactively):

```rust
    if cli.setup {
        return crate::setup_wizard::run().await;
    }
```

- [ ] **Step 2: Enable `unstable_auth_methods` on `agent-client-protocol`**

In `crates/aivyx-acp/Cargo.toml`, change:

```toml
agent-client-protocol = "2.0.0"
```

to:

```toml
# unstable_auth_methods enables AuthMethod::Terminal -- see this crate's
# own session.rs InitializeResponse wiring and docs/superpowers/specs/
# 2026-09-19-acp-registry-listing-design.md for why this is a deliberate
# dependency on an explicitly-unstable part of this crate's own Rust API
# surface (the underlying wire-protocol shape is stable/documented ACP,
# confirmed directly against agentclientprotocol.com's own RFD -- this is
# a crate-API-stability risk, not a protocol-legality one). Only this one
# feature, not the umbrella "unstable" (which also pulls in unrelated
# surfaces: elicitation, session-fork, MCP-over-ACP).
agent-client-protocol = { version = "2.0.0", features = ["unstable_auth_methods"] }
```

- [ ] **Step 3: Add the `terminal` auth method to `InitializeResponse`**

In `crates/aivyx-acp/src/session.rs`, replace the current
`InitializeRequest` handler body (currently lines 187-191):

```rust
            async move |req: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
```

with:

```rust
            async move |req: InitializeRequest, responder, _connection| {
                let terminal_auth = agent_client_protocol::schema::v1::AuthMethodTerminal {
                    id: agent_client_protocol::schema::v1::AuthMethodId::new("setup"),
                    name: "Run first-run setup".to_string(),
                    description: Some(
                        "Pick a backend and model, and write config.toml, before this agent \
                         can start."
                            .to_string(),
                    ),
                    args: vec!["--setup".to_string()],
                    env: std::collections::HashMap::from([(
                        "AIVYX_CODER_ACP_TERMINAL_AUTH".to_string(),
                        "1".to_string(),
                    )]),
                    meta: None,
                };
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new())
                        .auth_methods(vec![agent_client_protocol::schema::v1::AuthMethod::Terminal(
                            terminal_auth,
                        )]),
                )
            },
```

No explicit binary path is needed in `args`/`env`: per the
`agentclientprotocol.com` `terminal` auth RFD, the client launches the
`terminal` process using the agent program and base launch configuration
it already has configured (the same binary/command the client used to
start this very ACP connection), plus only the method descriptor's own
`args`/`env` layered on top — so `args: vec!["--setup".to_string()]`
alone is the complete, correct descriptor.

Note: `InitializeResponse` is declared `#[non_exhaustive]`-shaped via
its builder pattern already used here (`.agent_capabilities(...)`), so
`.auth_methods(...)` chains the same way — confirm this compiles as
written; if the builder's `auth_methods` signature differs from
`Vec<AuthMethod>` in a way that doesn't match, adjust to the real
signature (already confirmed as `pub fn auth_methods(mut self,
auth_methods: Vec<AuthMethod>) -> Self` by reading the crate source
directly — this should compile as written).

- [ ] **Step 4: Run to verify existing ACP tests still pass**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-acp 2>&1 | tail -30
```

Expected: all pre-existing tests pass unchanged (none of them assert on
`auth_methods` being empty, so this addition shouldn't break anything —
confirm this is genuinely true by reading the existing
`InitializeResponse`-touching tests, not just assuming).

- [ ] **Step 5: Manual smoke check**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build -p aivyx
rm -f /tmp/aivyx-setup-smoke-test-config.toml  # in case of a prior run
XDG_CONFIG_HOME=/tmp/aivyx-setup-smoke-test ./target/debug/aivyx-coder --setup
```

Expected: the interactive wizard runs end to end (you'll need a real
Ollama or llama-server instance running locally to get past the
verification step meaningfully, or just confirm the prompts/flow
correctly, then Ctrl-C if no backend is available in this environment —
note in your report which you were able to test).

- [ ] **Step 6: Update `README.md`**

Add a short new subsection near wherever `--acp`/`--mcp-server` are
documented (find their existing entries — likely under a "Running"/
"Usage" or "Editor integration (ACP)" heading, match its style exactly):

```markdown
### First-run setup

`aivyx-coder --setup` runs an interactive wizard (pick a backend, pick a
model, verify it responds) and writes `config.toml` for you, instead of
the silent defaults-on-first-run behavior. First-run only -- if
`config.toml` already exists, it tells you to edit it directly rather
than overwriting it. This is also the entry point Zed/JetBrains/other
ACP clients launch automatically as this agent's `terminal` authentication
method, before the agent is otherwise usable.
```

- [ ] **Step 7: Final full-workspace verification**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace 2>&1 | tail -40
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: all green.

- [ ] **Step 8: Commit**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git add crates/aivyx/src/main.rs crates/aivyx-acp/Cargo.toml crates/aivyx-acp/src/session.rs README.md
git commit -m "feat: wire --setup into the CLI and as ACP's terminal auth method

--setup runs before Settings::load()'s own first-run-defaults behavior
would otherwise fire, so the wizard genuinely gets to run interactively
on a fresh install rather than racing a silent default write.
InitializeResponse now declares a real AuthMethod::Terminal pointing at
--setup, satisfying the ACP registry's authMethods requirement honestly
(this is aivyx-coder's real onboarding gate, not a relabeled no-op) --
requires enabling agent-client-protocol's unstable_auth_methods feature,
a deliberate, documented dependency on an unstable corner of the crate's
Rust API (the wire-protocol shape itself is stable, documented ACP).
Completes the first half of the ACP registry listing initiative --
release-platform expansion and the actual registry PR are separate,
later work."
```

---

## Final verification (after all 4 tasks land)

- [ ] Run the complete workspace test suite once, not per-task:

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test --workspace 2>&1 | tail -40
```

- [ ] Run the documented clippy + fmt commands once more:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] This plan does not decide whether to push a branch / open a PR —
  follow `superpowers:finishing-a-development-branch` once all tasks are
  individually reviewed and a final whole-branch review has passed, same
  as every other plan executed this cycle.

## Addendum: Tasks 5-6 (added post-hoc from the final whole-branch review)

The final whole-branch review (opus, after Tasks 1-4 were each independently
approved) found a Critical cross-task seam no single task's diff could show:
an editor client must launch `aivyx-coder --acp` to receive the `initialize`
response advertising the `terminal` auth method — but that same launch
already runs `Settings::load()`, which silently writes a default
`config.toml` if none exists (this is deliberate, desired behavior for the
TUI/MCP-server paths). By the time the client then launches `--setup`
(per the advertised auth method), the config already exists, the wizard
refuses immediately (by design, per Task 3), and the client treats the
exit-0 refusal as "authentication succeeded" — the gate never gates
anything. Confirmed as a real, forced consequence of the current control
flow, not a hypothetical: traced end to end against the actual code.

Per-conversation decision: fix this for real now (not just document the
limitation), since the whole point of wiring `terminal` auth was for it to
work. Two small tasks, executed via the same subagent-driven-development
flow as Tasks 1-4.

### Task 5: A non-writing config check, and an ACP server that gates on it

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs`
- Modify: `crates/aivyx-acp/src/session.rs`
- Modify: `crates/aivyx/src/main.rs`

**Interfaces:**
- Produces: `Settings::load_existing() -> Result<Option<Settings>, ConfigError>`
  (never writes; `None` if no config file exists yet).
- Produces: `aivyx_acp::run_unconfigured() -> agent_client_protocol::Result<()>`
  (a minimal ACP server: `initialize` advertises the same `terminal` auth
  method as today; `session/new` always fails with `auth_required` — no
  `Agent` is ever built, since there's no configured backend to build one
  from).

- [ ] **Step 1: Add `Settings::load_existing()`**

```rust
/// Like `load()`, but never creates a config file — returns `None` if
/// none exists yet, `Some(Settings)` parsed the same way `load()` parses
/// one if it does. Used by the ACP frontend: advertising a `terminal`
/// auth method that gates on "no config yet" would be self-defeating if
/// simply checking for that state silently created it (as `load()`
/// deliberately does for the TUI/MCP-server paths, where that's the
/// desired first-run behavior).
pub fn load_existing() -> Result<Option<Self>, ConfigError> {
    let path = Self::config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .map_err(|source| ConfigError::Read { path: path.clone(), source })?;
    let settings = toml::from_str(&raw).map_err(|source| ConfigError::Parse { path, source })?;
    Ok(Some(settings))
}
```

Place near `load()`. Add a unit test mirroring `load()`'s own existing
test fixtures (temp dir, no real XDG path touched): one asserting `None`
when no file exists, one asserting `Some` with correct fields when a
real file does, using the same test-isolation mechanism the file's other
tests already use.

- [ ] **Step 2: Factor the `terminal` auth method into a shared helper, add `run_unconfigured`**

In `crates/aivyx-acp/src/session.rs`, extract the existing inline
`terminal_auth`-building code (currently inside `run()`'s
`InitializeRequest` handler, lines ~189-206) into a private helper:

```rust
/// The one `terminal` auth method this agent advertises, shared by both
/// `run()` (a real session is possible) and `run_unconfigured()` (no
/// config exists yet, so `session/new` will always fail with
/// `auth_required` until the client runs this method and reconnects).
fn terminal_auth_method() -> agent_client_protocol::schema::v1::AuthMethod {
    // `AuthMethodTerminal` is `#[non_exhaustive]`, so it's built via its
    // own builder methods rather than a struct literal.
    let terminal_auth = agent_client_protocol::schema::v1::AuthMethodTerminal::new(
        agent_client_protocol::schema::v1::AuthMethodId::new("setup"),
        "Run first-run setup",
    )
    .description(
        "Pick a backend and model, and write config.toml, before this agent can start.",
    )
    .args(vec!["--setup".to_string()])
    .env(std::collections::HashMap::from([(
        "AIVYX_CODER_ACP_TERMINAL_AUTH".to_string(),
        "1".to_string(),
    )]));
    agent_client_protocol::schema::v1::AuthMethod::Terminal(terminal_auth)
}
```

Update `run()`'s `InitializeRequest` handler to call
`.auth_methods(vec![terminal_auth_method()])` instead of building the
value inline.

Add the new entry point, in the same file:

```rust
/// A minimal ACP server for when no `config.toml` exists yet:
/// `initialize` advertises the same `terminal` auth method `run()`
/// does, but `session/new` always fails with `auth_required` -- there
/// is no configured backend to build a real `Agent` from. The client is
/// expected to launch this process's own `--setup` (per the advertised
/// method's `args`/`env`), then reconnect to a *fresh* `aivyx --acp`
/// process -- which by then finds `Settings::load_existing()` returning
/// `Some` and runs `run()` normally instead of this function.
pub async fn run_unconfigured() -> Result<()> {
    AcpAgentBuilder
        .builder()
        .name("aivyx-coder")
        .on_receive_request(
            async move |req: InitializeRequest, responder, _connection| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new())
                        .auth_methods(vec![terminal_auth_method()]),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest, responder, _connection| {
                responder.respond_with_error(agent_client_protocol::Error::auth_required())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}
```

Verify `agent_client_protocol::Error::auth_required()` is the correct,
real call before trusting this snippet verbatim — confirm
`agent_client_protocol::Error` really is a re-export of
`agent_client_protocol_schema::v1::Error` (check the crate's own
`lib.rs` re-exports) and that `Error::auth_required()` exists on it,
matching `respond_with_error(self, error: crate::Error)`'s expected
type.

- [ ] **Step 3: Wire the branch in `main.rs`**

Restructure the `cli.acp` handling so it no longer shares the unconditional
`Settings::load()` call the TUI/MCP-server paths use. Move the ACP
mutual-exclusivity checks (`--plan`, `--auto`, `--resume`) up, before any
settings loading, and give ACP its own `load_existing()`-based path:

```rust
if cli.acp {
    if cli.plan {
        anyhow::bail!("--acp and --plan cannot be used together");
    }
    if cli.auto.is_some() {
        anyhow::bail!("--acp and --auto cannot be used together");
    }
    if cli.resume {
        anyhow::bail!(
            "--acp and --resume cannot be used together (the editor manages its own \
             conversation view; resumed history would be invisible to it)"
        );
    }
    let Some(mut settings) = aivyx_config::Settings::load_existing()? else {
        return aivyx_acp::run_unconfigured()
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()));
    };
    settings.apply_overrides(cli.base_url.clone(), cli.model.clone());
    let (deferred, acp_prompter_installer) = aivyx_acp::deferred_prompter();
    let built =
        crate::agent_builder::build_agent(&cli, &settings, Arc::new(deferred)).await?;
    return aivyx_acp::run(aivyx_acp::AcpSessionConfig {
        agent: built.agent,
        events_rx: built.events_rx,
        cwd: built.cwd,
        plan_mode: built.plan_mode,
        prompter_installer: acp_prompter_installer,
    })
    .await
    .map_err(|e| anyhow::anyhow!(e.to_string()));
}
```

Place this block where the current `if cli.acp { ... }` block is (after
`init_tracing()?`, replacing both the shared `Settings::load()` call's
effect on the ACP path and the existing `if cli.acp` block) — but the
shared `let mut settings = Settings::load()?; settings.apply_overrides(...);`
line, and the shared `build_agent` call currently above the `if cli.acp`
check, must remain intact for the TUI and `--mcp-server` paths, which
still want `load()`'s write-defaults-on-first-run behavior. Read the
full current `main()` function before editing — the exact restructuring
needed depends on the real current control flow around the shared
`settings`/`built` bindings the TUI/MCP-server branches below still
consume; do not break those paths while carving out ACP's own early
branch.

- [ ] **Step 4: Tests and verification**

Add the new `load_existing` unit tests (Step 1). Extend the existing
ACP e2e test (`crates/aivyx/tests/acp_e2e.rs`) with a new test that
spawns `aivyx-coder --acp` in a temp `XDG_CONFIG_HOME` with no
`config.toml` present, sends `initialize`, then sends `session/new`, and
asserts the response is a JSON-RPC error with `auth_required`'s code
(-32000) rather than a success — this is the one behavior this whole
addendum exists to guarantee, so it must have direct test coverage, not
just manual verification. Match the existing e2e test's harness/style
(spawn pattern, temp dir setup) exactly.

Run `cargo build --workspace`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings` (the pre-existing,
out-of-scope fmt drift noted elsewhere in this plan is expected to still
be present and is not this task's concern), `cargo fmt --check` scoped to
just the files this task touches.

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-config/src/lib.rs crates/aivyx-acp/src/session.rs crates/aivyx/src/main.rs crates/aivyx/tests/acp_e2e.rs
git commit -m "fix: make ACP terminal auth a real gate, not a structural no-op

Settings::load_existing() never writes a default config (unlike load(),
whose write-defaults-on-first-run behavior is correct for the TUI/MCP-
server paths but was silently defeating the ACP terminal auth method:
the client had to boot --acp to see the auth method was advertised,
which already created config.toml, so the wizard --setup launched to
satisfy it always found the file already there and refused immediately,
reporting exit-0 success for a gate that never gated anything).

aivyx_acp::run_unconfigured() is what --acp now runs when no config
exists yet: initialize still advertises the terminal auth method, but
session/new always fails with auth_required instead of ever building a
real Agent. The client launches --setup per the advertised method,
then reconnects to a fresh --acp process, which now finds
load_existing() returning Some and runs the normal session flow."
```

### Task 6: Bundled Important fixes (context window, test coverage, README placement)

Three small, independent fixes the final review flagged as Important —
bundled into one implementer dispatch per the subagent-driven-development
skill's guidance against one-fixer-per-finding, since each is small and
none touch the same lines as another.

**Files:**
- Modify: `crates/aivyx/src/setup_wizard.rs`
- Modify: `crates/aivyx/tests/acp_e2e.rs` (or wherever Task 5's new test
  landed — add the `auth_methods` shape assertion here too, in the same
  spawned-process test if one already exists post-Task-5, to avoid a
  second full binary spawn)
- Modify: `README.md`

**Fix A — the wizard discards the context window it just measured.**
`backend_settings_from_answers` currently uses `..Default::default()`,
so `context_tokens` is always the default (8192) regardless of what
`probe_served_context` actually measured. Thread the probed value
through: change `backend_settings_from_answers`'s signature to accept
the measured context (or add a second pure function/field), set
`context_tokens` from `ServedContext::Known(n)` when available, and call
the existing, already-public `aivyx_llm::context_warning(configured,
&served)` helper (`crates/aivyx-llm/src/probe.rs`) to print a real
warning on a mismatch instead of the wizard's own weaker hand-rolled
message. Read `probe.rs`'s `context_warning` signature and all three
`ServedContext` variants' real current messages before writing this —
do not duplicate logic `context_warning` already provides correctly.
Add/update a unit test on `backend_settings_from_answers` (or its
replacement) confirming a measured context value actually lands in the
written `BackendSettings`.

**Fix B — `auth_methods` shape has no direct test.** If Task 5's own new
e2e test didn't already assert on the exact `auth_methods` contents
(only that `session/new` fails), add an assertion to the `initialize`
response step: `auth_methods` contains exactly one `AuthMethod::Terminal`
with `id.0 == "setup"` and `args == ["--setup"]`. If Task 5's test
already covers this exactly, skip this fix and note in your report that
it was already covered.

**Fix C — README's "Getting started" doesn't mention `--setup`.** Find
the existing "first run" paragraph (near the top of `README.md`, the one
describing the config file being written with defaults on first run) and
add one sentence recommending `aivyx-coder --setup` as the first-run
path, pointing at the fuller `### First-run setup` section already added
in Task 4. Keep the existing ACP-section entry as-is — do not duplicate
its full content, just cross-link.

- [ ] **Step 1: Implement all three fixes**
- [ ] **Step 2: Run the affected crates' tests, plus the full workspace suite once**
- [ ] **Step 3: Commit**

```bash
git add crates/aivyx/src/setup_wizard.rs crates/aivyx/tests/acp_e2e.rs README.md
git commit -m "fix: wizard writes the context window it measures; test auth_methods shape; surface --setup in Getting started

The wizard was printing the real served context window from
probe_served_context and then writing the unrelated 8192 default
regardless -- the exact silent-truncation footgun the design spec cites
as this verification step's reason to exist. Now threads the measured
value through and reuses the existing context_warning helper instead of
a weaker hand-rolled message. Also adds direct e2e coverage of the
auth_methods shape the ACP registry's own CI validates, and surfaces
--setup where a first-run user will actually read it."
```

## Explicitly out of scope for this plan

(Copied forward from the design spec's own "What this spec does not
decide" section, plus this plan's own additional scoping decisions, so a
future reader doesn't mistake either for an oversight.)

- Release-platform expansion (`darwin-aarch64` etc.) and the actual
  `agentclientprotocol/registry` PR (`agent.json` + `icon.svg`) — separate,
  later plan, per the design spec's own Decision 4.
- Re-runnable reconfiguration (`--setup` on an already-configured
  install) — deliberately first-run-only this pass; would require fixing
  `Settings::write_to`'s documented 0600-re-tightening gap first.
- Any change to `BackendKind::LlamaServerBroker`/`MistralRs` — the wizard
  only ever writes `BackendKind::Generic`; advanced backend kinds stay
  config.toml-only, matching the spec's own Decision 2.
- Whether the wizard should support non-interactive/scripted invocation
  (e.g. all answers via flags, for CI or automated provisioning) — not
  raised in the spec, not addressed here.
