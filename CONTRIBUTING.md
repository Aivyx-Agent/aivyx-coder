# Contributing to aivyx-coder

Thanks for your interest in contributing.

## Before you open a PR

- For anything beyond a small fix, please open an issue first to describe
  the shape of the change so we can agree on the approach.
- Keep changes focused — a PR should do one thing.

## Building & testing

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets

# Single crate / single test
cargo test -p aivyx-core
cargo test -p aivyx-core some_test_name
```

## License

By contributing, you agree your contributions are licensed under the same
dual Apache-2.0 OR MIT terms as the rest of the project (see
`LICENSE-APACHE` / `LICENSE-MIT`).
