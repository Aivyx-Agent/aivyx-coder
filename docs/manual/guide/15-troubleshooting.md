# Troubleshooting

## First steps

- **Check the log**: `~/.config/aivyx-coder/aivyx.log` records start-up
  problems and every permission decision.
- **Re-run setup**: `aivyx-coder --setup` re-detects your server, checks the
  model really answers, and reads its context window.
- **Capture the raw traffic**: start with `AIVYX_DEBUG_LOG=/tmp/wire.log
  aivyx-coder` to see exactly what goes to and from the model. The file holds
  everything in plain text — delete it when you're done.

## Common problems

**Replies stop mid-sentence, or a turn ends having done nothing.** Usually
the served context window is smaller than you think. Ollama serves its own
default (often 4096) unless the model sets `num_ctx` or the service sets
`OLLAMA_CONTEXT_LENGTH`; aivyx-coder warns about this at start-up. Set
`[backend] context_tokens` to the window the server really serves. On
llama-server with a reasoning model, a tool call can end up inside an
unclosed thinking block — start the server with
`--chat-template-kwargs '{"enable_thinking": false}'`. See
[Local model servers](09-local-model-servers.md).

**Tool calls appear as plain text instead of running.** The model's chat
template is missing or doesn't support tools: use a tool-capable model, run
llama-server with `--jinja`, and prefer GGUFs from Hugging Face over reused
Ollama blobs. On a small model, try `--edit-format prompted`.

**A false "hidden context window" warning with Lemonade.** Point
`base_url` at the llama-server process Lemonade started, not at Lemonade's
gateway port — see [Local model servers](09-local-model-servers.md).

**Commands fail that work in your own terminal.** On Linux, commands run in
a sandbox. `ssh-agent`, gpg signing, `docker` and socket-based database
clients need `[sandbox] allow_unix_sockets = true`; tools that write to a
fixed `/tmp` path need `share_system_tmp = true`; test runners that create
process groups (cargo-nextest) need `allow_leaving_process_group = true`.
Each opt-out weakens the sandbox — see the details below.

**Commands refuse to run at all, saying confinement isn't available.** The
kernel lacks Landlock (some older kernels, some WSL2 kernels). Set
`require_enforcement = false` under `[sandbox]` to run commands unconfined,
knowing what that gives up.

**`cargo build` keeps failing in a project with a `.env` at its root.**
Create the `target/` folder once (`mkdir target`) — see "Directories holding
a denied entry" below.

**An MCP server is skipped at start-up.** It didn't finish starting within
its `timeout_secs`. `npx`-based servers often need their cache directory
added to `[sandbox] extra_read_paths`, or use the server's installed binary
directly. See [MCP](13-mcp.md).

**`/commit` says it can't write the repository.** You started aivyx-coder in
a subfolder of the repository or in a linked worktree; start it at the
repository root, or commit with git.

**A container can't reach a model server on the host.** See
[Docker](14-docker.md).

## Known limitations

Deliberately not (yet) addressed — documented rather than hidden:

- **Indirect prompt injection**: content the agent reads (files, command
  output, search results) re-enters the model's context with no
  trust/provenance tag — the model cannot structurally distinguish "the user
  said X" from "a file said X." The system prompt instructs the model to treat
  such content as data, not instructions, but this is a mitigation, not a
  guarantee. Be especially careful pointing the agent at untrusted repositories
  while `allowed_commands` is configured — including, under `--auto`, the
  synthetic `detected-tests` entry added automatically when no
  `[verification] command` is set (see [Autonomous and advanced modes](10-autonomous-and-advanced-modes.md)) — since a
  pre-approved command runs without a per-invocation prompt. The heuristic
  injection scan (phrase-list
  matching over tool results as they re-enter context, `aivyx-injection-guard`)
  runs in every mode, on top of the mitigation above, but it's a
  pattern-based heuristic, not a structural fix, and its response to a hit
  differs by mode. In interactive mode (the TUI, and editors via ACP) a hit
  surfaces as a passive notice after the turn — it doesn't pause or block
  anything — because the permission modal (a human reviewing the actual
  command/diff before it executes) is already the primary mitigation there.
  **Autonomous mode** goes further precisely because there's no
  human reviewing each prompt: it pauses the run outright on a
  high-confidence hit, and denies any further mutating or network tool call
  for the rest of that session until a human clears it.
- **Sandbox side effects you may hit** (Linux confinement — see the [security model](../reference/05-security-model.md)):
  - *No local daemons by default*: ssh-agent, gpg signing, `docker`,
    socket-based database clients and keyring-backed credential helpers
    fail inside confined commands unless `[sandbox] allow_unix_sockets =
    true`.
  - *Private `TMPDIR`*: commands get their own temp directory, one per
    confiner (the lead, each specialist session, each delegation), so a
    file a specialist leaves in `$TMPDIR` isn't in yours, and it is gone
    once that specialist's session or delegation ends. A tool that writes
    to a hard-coded `/tmp` path fails (`share_system_tmp = true` to opt
    out). The lead's is deleted when aivyx-coder exits normally (after a
    crash it's left in the system temp dir as `aivyx-confine-*`).
  - *`setsid`/`setpgid` are blocked*: test runners and supervisors that put
    children in their own process groups (cargo-nextest's per-test process
    groups, Python's `start_new_session=True`, the `setsid` tool,
    interactive job control) may fail; `allow_leaving_process_group = true`
    opts out. `git_commit` and `/commit` run git with
    `-c maintenance.auto=false`, so git doesn't try to start its detached
    auto-maintenance (your own git still does it). A `git commit` the model
    runs through `run_shell` does, and prints `fatal: setsid failed` while
    the commit itself succeeds.
  - *REPLs have no controlling terminal*: a confined command already
    leads its own process group, and POSIX refuses `setsid()` to a group
    leader, so a REPL started under the confiner can't take the pty as its
    controlling terminal — whatever `allow_leaving_process_group` says. It
    gets no `SIGWINCH` on resize, and a Ctrl-C byte in `repl_send` doesn't
    interrupt it (use `repl_stop`).
  - *Directories holding a denied entry*: a directory that directly
    contains a `deny_paths` match (often the project root, for `.env`)
    gets list-and-create rights only. `rm`/`mv` of entries **directly** in
    it fail, and a file or directory created directly in it by a command
    can't be written by that same command (`echo a > new.txt` leaves an
    empty file) — the next command can, so a retry usually works. Not for
    `cargo build`, though: cargo creates `target/` by renaming a temp
    directory, which is never allowed there, so in a Rust project whose root
    holds a denied file (`.env` is denied by default) confined builds keep
    failing — and leave an undeletable `targetXXXXXX` directory behind —
    until `target/` exists. Create it once (`mkdir target`, confined or
    not) and builds work from then on. Deeper subdirectories are
    unaffected, and aivyx-coder's own file tools (`write_file`,
    `delete_file`, `move_file`) aren't confined. aivyx-coder prints a
    notice at startup when it sees a Rust project in this state.
  - *Rarer edges* (details in aivyx-confine's README, "Known limits"):
    when aivyx-coder runs in `$HOME` or an ancestor of it, a symlinked
    `~/.cargo`/`~/.rustup` is not granted (a command could have planted
    the link), so toolchains reached through one fail, and an existing
    `~/.git-credentials` makes `$HOME` itself list-and-create only (no
    `rm`/`mv` of its direct entries); a working directory
    inside a denied directory gets no access at all; an
    `extra_read_paths` entry inside the working directory that is a
    symlink grants nothing; a denied file nested about a thousand
    directories deep can exhaust a 1024-descriptor limit and every
    confined spawn is then refused (`EMFILE`); with basename-pattern
    `deny_paths` (`.env`, `*.pem`) every spawn rescans the working
    directory (cached by mtime, but noticeable on huge trees); a
    `SOCK_DGRAM` `socketpair()` is refused along with Unix sockets; on a
    kernel whose Landlock ABI is older than 7 the newer restrictions (signal
    and abstract-socket scoping below ABI 6) simply aren't enforced.
- **Network is not restricted** by the sandbox. A command you approve can make
  network connections (needed for `cargo build`, `npm install`, `git clone`,
  etc.). Combined with the read scope, an approved command could in principle
  exfiltrate anything in that scope.
- **Environment variables are inherited** by spawned commands (needed by real
  toolchains). A command like `env` will see whatever secrets are in your
  shell environment. This is standard for shell tools but worth knowing.
- **TOCTOU windows**: `write_file`/`edit_file`/`read_file`/`patch_file` resolve
  their path once for the confirmation preview and again at execution; a
  symlink swapped in between could redirect the operation. `deny_paths` and
  the Landlock scope still bound the blast radius.
- **`AIVYX_DEBUG_LOG`**: if you set this env var to capture raw wire traffic
  for debugging, it logs the full conversation (including file/command content)
  in plaintext, append-only, forever. The file is `0600` but has no rotation or
  expiry — treat it as sensitive and delete it when done.
- **`repl_start`/`repl_send` use a real pseudo-terminal (PTY)**, not plain
  pipes — a program run this way sees `isatty()` as true, so
  readline/history, color, and window-size-aware output all work as they
  would at a real terminal, and the pty resizes live as `aivyx-coder`'s
  own terminal does. Two consequences worth knowing, both deliberate:
  the pty's default line discipline echoes input back before the
  program's own response (matching what a human typing at a real
  terminal would see); and standard control characters in `repl_send`'s
  `input` (e.g. a literal Ctrl-C byte) are interpreted by the tty driver
  as signals to the process, not passed through as literal data — again,
  same as at a real terminal (except under the Linux sandbox, where the
  pty can't be the REPL's controlling terminal; see "Sandbox side effects you may hit" above).
- **`Tool::execute` bypass**: the "all tool calls go through the permission
  gate" property is enforced by convention (the executor is the only caller),
  not by the type system.
- **Git specifics**: `git_commit` (re)stages the paths it commits, so a
  carefully staged partial hunk within those paths is staged in full (the
  modal preview shows the full scope first). Commit signing (GPG/SSH) and
  hooks that read paths outside the sandbox's grants will fail under
  confinement. `git_read`'s status/diff exclude `deny_paths` via pathspecs,
  but `log` shows committed history as-is — anything already committed is
  considered yours to see.
- **`move_file` cross-filesystem moves**: refused outright rather than
  transparently falling back to a recursive copy+delete, to keep the
  tool's atomicity guarantee honest. In practice this only bites when
  `from`/`to` resolve onto different mounted filesystems, which is rare
  for an in-project rename.
