# Compatibility

From 1.0, aivyx-coder follows semantic versioning for its stable surface.

## Stable

- **Config keys and their defaults.** Every key path, with the default
  from `Settings::default()`, the values each enum key accepts, the
  fields of list and table entries, and the deprecated spellings still
  read. Snapshot: `docs/stable/config.txt`.
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

- It freezes config keys with their defaults (`null` is unset), and the
  values an enum key accepts (`(one of: ...)`). The keys and the values
  come from tracing the config types' own deserializer, so an unset
  optional key is listed too.
- Fields of list and table entries (`permissions.allowed_commands[].name`,
  `routing.endpoints.*.kind`) are frozen with their types: an entry has no
  default to freeze. What an omitted entry field means is covered by
  review.
- The CLI snapshot lists subcommands and long flags. Short flags and
  aliases of long flags are not in it.
- The format snapshot lists versions. That old sessions still load is
  tested separately: `crates/aivyx-core/tests/fixtures/session-v1.json`
  is a session file as 1.0 writes it, and `tests/session_fixture.rs`
  loads it. A session file from another version is kept unchanged, and
  `--resume` says it can't be resumed.
- The ACP behaviour is not in a snapshot. It is covered by the ACP tests
  and by review.
