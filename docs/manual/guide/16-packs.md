# Packs

A **pack** shapes aivyx-coder for one kind of work. A business owner's
pack, say, teaches it to explain changes in plain language and gives it
skills for website updates and small automations. A pack carries
configuration only, never programs:

- **instructions** — an `AGENTS.md`, added to what the model reads;
- **skills** — step-by-step guidance the model can load;
- optionally a **specialist team** roster;
- optionally **MCP servers**, which only run if you agree;
- optionally **verification commands** — the tests and checks to run —
  which also only run if you agree.

Packs are signed. aivyx-coder installs one only if its signature matches a
publisher you trust.

## Install one

```sh
aivyx-coder pack inspect business-manager.aivyxpack   # what it contains
aivyx-coder pack install business-manager.aivyxpack
```

`inspect` shows the pack's instructions, skills and MCP servers (with the
exact commands they would run) and whether it passes its checks.
`install` verifies the signature, checks the pack and keeps it under
`~/.config/aivyx-coder/packs/`. Installing a newer version replaces the
older one.

Packs from Aivyx itself install as they are: aivyx-coder has the Aivyx
publisher keys built in. The Hub lists them on its
[home page](https://aivyx-hub.com/#trust-heading) so you can compare. To trust
another publisher, add their key (from `aivyx-pack keygen`) to your
config:

```toml
[pack]
trusted_publishers = ["<base64 key>"]
```

## Use it in a project

```sh
cd ~/my-shop-website
aivyx-coder pack use business-manager            # this project
aivyx-coder pack use business-manager --global   # every project
```

It takes effect the next time aivyx-coder starts there, and a notice says
which pack is in use. A project's own choice wins over the `--global` one.
`aivyx-coder pack list` shows what's installed and where it's in use.

If the pack has MCP servers, `pack use` shows each one's command line and
the environment variables it sets, and asks before allowing it. An MCP
server runs a program on your machine, so say no unless you know what it
is. If a pack's server command, arguments or environment ever change, it
stops running until you run `pack use` again.

For a script — a setup script, or a benchmark harness — `pack use --yes`
approves every MCP server and command without asking, printing each one
with `approved:` in front, so the log shows exactly what was agreed to —
including that the model may run the approved commands without asking.

## Commands a pack can run

A pack can name the commands aivyx-coder uses to check its work, in a
`coder/settings.toml`:

```toml
[verification]
command = "cargo test"               # run after edits, before a turn ends
scoped_command = "cargo test {touched_paths}"   # optional: on the changed files
final_checks = ["cargo fmt --check"] # run when an --auto goal is reached
```

Each command is one program and its arguments, run without a shell, so
shell syntax can't work: `&&`, `|`, `;`, redirections, `$`, quotes, globs
and the like are refused. To run two commands, use one that runs both (a
`just` or `make` target, say). `{touched_paths}` — replaced by the files
changed since the last check — must be a whole word on its own
(`cargo test {touched_paths}`, not `--files={touched_paths}`).

That's all a pack may set: any other key, an empty command, a control
character or shell syntax makes `pack check`, `install` and `pack use`
refuse the pack, and a pack already in use whose file turns bad gets a
notice at start-up instead. (A pack with a settings file needs aivyx-coder
1.0.0 or later.)

`pack use` shows the commands and asks before allowing them:

```text
This pack wants aivyx-coder to run these commands to check its work:
  runs: cargo test
  final check: cargo fmt --check
They run without asking each time, and the model may run them without asking too.
Allow them? [y/N]
```

If a command's name matches one of your own
`[[permissions.allowed_commands]]` entries that runs something else, the
prompt says so under it — `(your own allowed command of that name runs
instead: cargo nextest run)` — since yours is what will run.

Without a terminal the answer is no, unless you pass `--yes`. aivyx-coder
records exactly the commands you approved; if a later version of the pack
changes them, they aren't used, and a notice at start-up asks you to run
`pack use` again to review them. A no is remembered too, so declined
commands stay off quietly; but commands you haven't reviewed — added by a
newer version of the pack, or changed since you declined them — get one
notice each time aivyx-coder starts:
`Pack <name> has commands you haven't approved — run aivyx-coder pack use
<name> to review them.`

An approved command runs like one of your own
`[[permissions.allowed_commands]]` entries, named by its command line: it
is split at spaces and run directly (no shell), inside the sandbox, and
the model may run it too without asking. If you already have an entry of
that name, yours is what runs.

**Your own settings win.** A pack's command fills only a `[verification]`
field you left unset (`final_checks` only when yours is empty); otherwise
a notice says it isn't used.

## What a pack changes — and what it doesn't

A pack in use **adds**, for that session only:

- its instructions, after yours and before the project's own `AGENTS.md`.
  If they conflict, the project's instructions win, then the pack's, then
  yours. They're scanned for prompt injection like the others;
- its skills, if you haven't set both `[skills] project_dir` and
  `user_dir`;
- its team roster, if you haven't set `[team] roster_path`;
- the MCP servers you allowed, unless you already have one with the same
  name;
- the verification commands you allowed, into each `[verification]` field
  you left unset.

It **never** edits your `config.toml`, your `AGENTS.md`, or any file in
the project, and a file in a repository can't switch a pack on: only
`pack use` on your machine can. Anything a pack can't apply is explained
in a notice at start-up.

## Switch it off, remove it

```sh
aivyx-coder pack off              # this project
aivyx-coder pack off --global     # the one used everywhere
aivyx-coder pack remove business-manager
```

`remove` deletes the pack and switches it off everywhere.

## Making a pack

Packs are built with the `aivyx-pack` command from the
[aivyx-pack](https://github.com/Aivyx-Agent/aivyx-pack) repository. Check a
pack folder's aivyx-coder part with `aivyx-coder pack check <folder>`; it
lists every problem it finds. See the
[command-line reference](../reference/01-command-line.md#subcommands).
