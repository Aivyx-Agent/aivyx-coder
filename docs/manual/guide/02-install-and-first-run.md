# Install and first run

## 1. Install

Download the latest release into `~/.local/bin` (Linux x86_64; on Apple
Silicon set `T=darwin-aarch64`):

```sh
V=$(curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/Aivyx-Agent/aivyx-coder/releases/latest | sed 's#.*/##')
T=x86_64-linux-musl
curl -fsSL "https://github.com/Aivyx-Agent/aivyx-coder/releases/download/$V/aivyx-coder-$V-$T.tar.gz" | tar xz
mkdir -p ~/.local/bin && mv "aivyx-coder-$V-$T/aivyx-coder" ~/.local/bin/
```

Make sure `~/.local/bin` is on your `PATH`. To build it yourself instead,
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

Setup finds the servers already running (Ollama, Lemonade Server, or one on
port 8080) and offers them first, lists the models, checks that the one you
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
  with `curl | tar` as above avoids Gatekeeper's quarantine (a browser
  download may be refused).
- **Windows** — no native build. WSL2 runs the Linux binary; if its
  kernel lacks Landlock, commands refuse to run rather than run unconfined
  (see `require_enforcement` in the
  [configuration reference](../reference/03-configuration.md)). Or use
  [Docker](14-docker.md).
