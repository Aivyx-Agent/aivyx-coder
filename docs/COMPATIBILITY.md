# Compatibility

From 1.0, aivyx-coder follows semantic versioning for its stable surface.

## Stable

- **Config keys and their defaults.** Every key path, with the default
  from `Settings::default()`. Snapshot: `docs/stable/config.txt`.
- **The CLI flags.** The subcommands and long flags, including `--auto`,
  `--headless` and `--report`. Snapshot: `docs/stable/cli.txt`.
- **The `--report` JSON schema.** It carries `report_version`, and the
  version is part of the promise. Snapshot: `docs/stable/formats.txt`.
- **Session files.** `--resume` works across 1.x. The session format
  version is in `docs/stable/formats.txt`.
- **The ACP behaviour.** What the ACP registry entry relies on: starting
  with `--acp` and speaking the protocol to an editor.
- **Pack format 2.** Config-only packs. Snapshot: `docs/stable/formats.txt`.

## Not stable

- **Previews and experimental options.** Anything the manual marks
  experimental or a preview can change or be removed in any release.
  Nothing is so marked today.
- **The TUI's look**, and keys beyond the documented ones.
- **Internal crates.** The crates in this repository are not a public API.
- **The tools the model sees.** Their names, arguments and descriptions
  change as models change.

## Rules

- A breaking change to the stable surface is a new major version.
- A minor release may add to it and deprecate parts of it. A deprecated
  item keeps working for at least one minor release and warns once when
  used.
- A config key is never silently given a new meaning; a changed meaning
  gets a new key.
- Data written by 1.0 is read by every 1.x; migrations run forward on
  their own and say so.

## How this is checked

`docs/stable/` is generated from the code; a test fails when it changes.
The test is `stable_surface_matches_the_snapshot` in
`crates/aivyx/src/main.rs`. To update the snapshot on purpose, run
`UPDATE_STABLE_SURFACE=1 cargo test -p aivyx stable_surface`.

The check has limits:

- It freezes config keys and their defaults. A default that is an empty
  list or table (for example `mcp.servers`, `routing.endpoints` and
  `verification.final_checks`) freezes only the key, not the fields inside.
  Review covers those.
- The CLI snapshot lists subcommands and long flags. Short flags and
  aliases of long flags are not in it.
- The format snapshot lists versions. It does not test that old sessions
  still load.
- The ACP behaviour is not in a snapshot. It is covered by the ACP tests
  and by review.
