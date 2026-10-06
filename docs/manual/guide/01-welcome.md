# Welcome to aivyx-coder

aivyx-coder is a coding agent that lives in your terminal and works with
**local** models only — Ollama, Lemonade Server, llama.cpp, vLLM or Jan. You
describe a change; the model reads your project, edits files, runs your
tests and uses git, and you approve each step. Nothing goes to a cloud API.

## What makes it different

- **Local, always.** It talks only to model servers on your own machine or
  network. There is no cloud mode to switch on by accident.
- **Built for a model that may be wrong.** Every file edit and command
  waits for your approval, with the exact diff or command shown. Your
  secrets (SSH keys, cloud credentials, `.env` files) are off-limits, and on
  Linux every command runs inside a kernel sandbox.
- **Easy to take back.** Each turn is snapshotted in git, so `/undo` rewinds
  everything the assistant just did — edits and command effects together.
- **Made for small models too.** A repository map, a choice of edit
  formats, and careful context management help 4–30B models do real work.

## How to read this guide

Start with [Install and first run](02-install-and-first-run.md) and
[A working session](03-a-working-session.md); together they cover
everyday use. Then pick what you need:

- plan before acting — [Plan mode](04-plan-mode.md)
- review, undo, commit and test — [Undo, diff, commit and test](05-undo-diff-commit-test.md)
- come back to a conversation — [Sessions](06-sessions.md)
- several models — [Models and routing](08-models-and-routing.md),
  [Local model servers](09-local-model-servers.md)
- let it work alone — [Autonomous and advanced modes](10-autonomous-and-advanced-modes.md)
- use it from Zed or VS Code — [Editor integration](12-editor-integration.md)

Every command, setting and tool is in the [reference](../reference/01-command-line.md).
