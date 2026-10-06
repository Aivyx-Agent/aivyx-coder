# Building and testing

## Build and run

```sh
cargo run -p aivyx                  # the TUI, from a checkout
cargo run -p aivyx -- --acp         # as an ACP server
cargo build --workspace
```

The package is `aivyx`; the binary it produces is `aivyx-coder`.

## Features

| Feature | Default | What it does |
|---|---|---|
| `sandbox-backend` | on | Landlock + seccomp confinement. Linux-only; `--no-default-features` builds without it (`NoopConfiner`), which is how the macOS release is built — and changes runtime behaviour, see `[sandbox] require_enforcement`. |
| `provider-mistral-rs` | off | The embedded mistral.rs engine, CPU only |
| `provider-mistral-rs-cuda` / `-metal` / `-accelerate` | off | The embedded engine with GPU acceleration — pick one |

Every workspace crate that re-forwards `sandbox-backend` depends on its
siblings with `default-features = false`; leaving one edge unguarded pulls
landlock back in through it. Check with `cargo tree -i landlock`.

## Release builds

```sh
scripts/build-release.sh
```

builds the static Linux x86_64 (musl) binary into
`dist/aivyx-coder-v<version>-x86_64-linux-musl.tar.gz` with a `.sha256`.
It needs `musl-gcc` (the `musl` / `musl-tools` package) for tree-sitter's C
sources. Tagged releases (`vX.Y.Z`) are built by GitHub Actions for Linux
x86_64 and macOS Apple Silicon, and a Docker image is published to
`ghcr.io/aivyx-agent/aivyx-coder`.

## Test and lint

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p aivyx-core some_test_name     # one crate / one test
```

Some sandbox tests need a kernel with Landlock; CI runs on one.

## Documentation checks

Two tests keep the manual honest: one in `crates/aivyx` fails if a
command-line flag has no section in
[`reference/01-command-line.md`](../reference/01-command-line.md), one in
`aivyx-core` if a slash command has none in
[`reference/02-slash-commands.md`](../reference/02-slash-commands.md).

Two reference pages are generated — re-run after changing the config schema
or a tool, and commit the result:

```sh
python3 scripts/gen-config-reference.py > docs/manual/reference/03-configuration.md
python3 scripts/gen-tools-reference.py  > docs/manual/reference/04-tools.md
```

Both scripts stop with an error when something new isn't covered (a config
section without an intro, a key without a meaning, a tool without a group
or a known `ActionKind`), so the fix is usually a line in the script.

## Debugging a model

`AIVYX_DEBUG_LOG=/tmp/wire.log` captures the raw traffic to and from the
model in plain text. Delete it afterwards.
