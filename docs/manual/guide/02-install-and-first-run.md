# Install and first run

## 1. Install

Install the latest release (Linux x86_64 or an Apple Silicon Mac):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Aivyx-Agent/aivyx-coder/releases/latest/download/aivyx-coder-installer.sh | sh
```

The installer checks the download against its published SHA-256, puts
`aivyx-coder` in `~/.cargo/bin` (beside Aivyx PA's `aivyx-pa`, if you have
it) and adds that folder to your `PATH`. Check it with
`aivyx-coder --version`; if your shell says "command not found", open a
new terminal. Installed an earlier version into `~/.local/bin`? Remove it
(`rm ~/.local/bin/aivyx-coder`) so the new one is the one you run. To
build it yourself instead,
see [Building and testing](../developer/03-building-and-testing.md), or run
it in a container — see [Docker](14-docker.md).

## 2. Start a local model server

aivyx-coder needs a server with a **tool-capable** model loaded: Lemonade
Server, Ollama, llama.cpp's `llama-server`, vLLM or Jan. If you don't have
one yet, [Local model servers](09-local-model-servers.md) walks through it.

## 3. Run setup

From the project you want to work on:

```sh
aivyx-coder --setup
```

Setup finds the servers already running (Ollama, Lemonade Server, Docker
Model Runner, or one on port 8080) and offers them first, lists the models, checks that the one you
pick really answers, reads the context window it's served with, and writes
`~/.config/aivyx-coder/config.toml`. You don't strictly need `--setup` the
first time: a plain `aivyx-coder` at a terminal runs the same setup when
there's no config yet.

Run `--setup` again whenever you want a different server or model; it asks
before replacing the config and keeps the old one as `config.toml.bak`.

## 4. Start working

```sh
cd ~/projects/my-app
aivyx-coder
```

The current folder is the project. Ask for a change; every file edit and
command waits for your approval (`y` to allow). The next chapter,
[A working session](03-a-working-session.md), explains what you'll see.

## Platform support

- **Linux (x86_64)** — the full sandbox: every command the model runs is
  confined by the kernel (Landlock and seccomp).
- **macOS (Apple Silicon)** — works the same, but there is **no kernel
  sandbox**: commands rely on the approval prompts, the deny list and
  checkpoints alone. The binary is unsigned and unnotarized; installing
  with the command above avoids Gatekeeper's quarantine (a browser
  download may be refused).
- **Windows** — no native build. WSL2 runs the Linux binary; if its
  kernel lacks Landlock, commands refuse to run rather than run unconfined
  (see `require_enforcement` in the
  [configuration reference](../reference/03-configuration.md)). Or use
  [Docker](14-docker.md).
