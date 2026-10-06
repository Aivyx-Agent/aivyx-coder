# Contributing

The full guidance is in
[`CONTRIBUTING.md`](https://github.com/Aivyx-Agent/aivyx-coder/blob/main/CONTRIBUTING.md).

## Licence and sign-off

aivyx-coder is **source-available** under the Business Source License 1.1:
free for personal, educational, research and other non-commercial use, with
commercial use under a separate licence
([`COMMERCIAL.md`](https://github.com/Aivyx-Agent/aivyx-coder/blob/main/COMMERCIAL.md)).
Each version becomes MIT-licensed four years after its release.

So that contributions can be offered under both, every commit needs a
sign-off:

```sh
git commit -s
```

The `Signed-off-by` line certifies the Developer Certificate of Origin and
accepts the Contributor License Agreement in
[`CLA.md`](https://github.com/Aivyx-Agent/aivyx-coder/blob/main/CLA.md). You
keep your copyright. A pull request with unsigned commits can't be merged.

## Before you build

- Open an issue first for anything beyond a small fix, and agree the
  approach.
- Keep a pull request to one thing.
- Anything touching the gate, the confiner or a tool's `ActionKind`: read
  [Permission gate and sandbox](02-permission-gate-and-sandbox.md) first,
  and add a test that shows the boundary still holds.

## The bar for a pull request

- [ ] Every commit signed off.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] `cargo test --workspace` passes, and new behaviour has tests.
- [ ] The manual is updated for anything a user sees; the generated
      references are re-run if you changed the config or a tool (see
      [Building and testing](03-building-and-testing.md)).

## Security issues

Please don't open a public issue for a vulnerability; see
[`SECURITY.md`](https://github.com/Aivyx-Agent/aivyx-coder/blob/main/SECURITY.md).
