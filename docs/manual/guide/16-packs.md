# Packs

A **pack** shapes aivyx-coder for one kind of work. A business owner's
pack, say, teaches it to explain changes in plain language and gives it
skills for website updates and small automations. A pack carries
configuration only, never programs:

- **instructions** — an `AGENTS.md`, added to what the model reads;
- **skills** — step-by-step guidance the model can load;
- optionally a **specialist team** roster;
- optionally **MCP servers**, which only run if you agree.

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

To trust a publisher, add their key (from `aivyx-pack keygen`) to your
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
asks before allowing it. An MCP server runs a program on your machine, so
say no unless you know what it is. If a pack's server command ever
changes, it stops running until you run `pack use` again.

## What a pack changes — and what it doesn't

A pack in use **adds**, for that session only:

- its instructions, after yours and before the project's own `AGENTS.md`.
  If they conflict, the project's instructions win, then the pack's, then
  yours. They're scanned for prompt injection like the others;
- its skills, if you haven't set both `[skills] project_dir` and
  `user_dir`;
- its team roster, if you haven't set `[team] roster_path`;
- the MCP servers you allowed, unless you already have one with the same
  name.

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
