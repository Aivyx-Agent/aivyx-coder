# aivyx-coder

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logos/aivyx-lockup-horizontal-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/logos/aivyx-lockup-horizontal-light.svg">
  <img alt="aivyx-coder" src="docs/logos/aivyx-lockup-horizontal-light.svg" width="280">
</picture>

[![CI](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml)
[![License: BUSL-1.1](https://img.shields.io/badge/license-BUSL--1.1-blue.svg)](LICENSE)

A terminal (TUI) coding agent for **local** LLMs only — Ollama, Lemonade
Server, llama.cpp, vLLM or Jan over their OpenAI-compatible `/chat/completions` endpoints. It never
talks to a cloud API. An LLM drives tool calls (read/write/edit files, search,
run commands) against your real filesystem, gated by a layered permission and
sandboxing model designed so that a mistaken — or actively manipulated — model
cannot quietly do damage.

This is a from-scratch Rust project built reliability-first: the security
boundary was designed before the tools that need it, and hardened through
repeated full-codebase audits (see `docs/HISTORY.md` for the phase history).

## Quick start

**1. Install** the latest release into `~/.local/bin` (Linux x86_64; on
Apple Silicon set `T=darwin-aarch64`):

```sh
V=$(curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/Aivyx-Agent/aivyx-coder/releases/latest | sed 's#.*/##')
T=x86_64-linux-musl
curl -fsSL "https://github.com/Aivyx-Agent/aivyx-coder/releases/download/$V/aivyx-coder-$V-$T.tar.gz" | tar xz
mkdir -p ~/.local/bin && mv "aivyx-coder-$V-$T/aivyx-coder" ~/.local/bin/
```

(Make sure `~/.local/bin` is on your `PATH`.)

**2. Start a local model server** with a tool-capable model loaded:
Lemonade Server, Ollama, llama.cpp's `llama-server`, vLLM or Jan.

**3. Run setup** from the project you want to work on:

```sh
aivyx-coder --setup
```

Setup picks up the server that's running, lists its models, checks that
the one you choose really answers, and writes
`~/.config/aivyx-coder/config.toml`. After that, plain `aivyx-coder` in
any project directory opens the TUI. Ask for a change; every file edit
and command waits for your approval (`y` to allow).

## What it does

- **Approves before it acts** — every edit and command waits for you, with
  the exact diff or command shown; your secrets are off-limits, and on Linux
  every command runs in a kernel sandbox.
  [A working session](docs/manual/guide/03-a-working-session.md) ·
  [Security model](docs/manual/reference/05-security-model.md)
- **Plans first, if you like** — read-only plan mode, approve with Ctrl+P.
  [Plan mode](docs/manual/guide/04-plan-mode.md)
- **Takes it back** — `/undo` rewinds a whole turn; `/diff`, `/commit` and
  `/test` review, commit and check the work.
  [Undo, diff, commit and test](docs/manual/guide/05-undo-diff-commit-test.md)
- **Remembers** — saved conversations, `AGENTS.md` instructions, learned
  preferences, notes and a project wiki.
  [Sessions](docs/manual/guide/06-sessions.md) ·
  [Memory and learning](docs/manual/guide/07-memory-and-learning.md)
- **Uses the right model** — routing across several local models, KV-cache
  persistence, GPU sharing with aivyx-broker, an embedded engine.
  [Models and routing](docs/manual/guide/08-models-and-routing.md) ·
  [Local model servers](docs/manual/guide/09-local-model-servers.md)
- **Works alone when asked** — `--auto` toward a goal, verified by your
  tests; `/architect`, `/council`, sub-agents and specialist teams.
  [Autonomous and advanced modes](docs/manual/guide/10-autonomous-and-advanced-modes.md) ·
  [Specialist teams](docs/manual/guide/11-specialist-teams.md)
- **Fits into your tools** — Zed and VS Code over ACP, MCP servers both
  ways, Docker.
  [Editor integration](docs/manual/guide/12-editor-integration.md) ·
  [MCP](docs/manual/guide/13-mcp.md) · [Docker](docs/manual/guide/14-docker.md)
- **Takes on a specialty** — signed packs add instructions, skills, a team
  and MCP servers for one kind of work, switched on per project.
  [Packs](docs/manual/guide/16-packs.md)

## Platform support

Linux x86_64 with the full kernel sandbox, and macOS on Apple Silicon
without it (commands rely on approvals, the deny list and checkpoints).
Details in [Install and first run](docs/manual/guide/02-install-and-first-run.md#platform-support).

## Documentation

**[The manual](docs/manual/README.md)** — a guide for everyday use, a
reference for every flag, command, setting and tool, and a developer part.
Start with [Troubleshooting](docs/manual/guide/15-troubleshooting.md) when
something goes wrong. [`docs/HISTORY.md`](docs/HISTORY.md) is the full
phase-by-phase history and audit trail.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md): every commit is signed off
(`git commit -s`), accepting the [CLA](CLA.md). Building and testing:
[developer guide](docs/manual/developer/03-building-and-testing.md).

## License

Source-available under the [Business Source License 1.1](LICENSE): free for
personal, educational, research and other non-commercial use; commercial use
under [`COMMERCIAL.md`](COMMERCIAL.md). Each version becomes MIT-licensed
four years after its release.
