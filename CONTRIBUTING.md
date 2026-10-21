# Contributing

Issues and pull requests are welcome.

- Build and test on macOS with current stable Rust; see [Building from source](README.md#building-from-source). Before opening a pull request, run `make check` (format, Clippy with warnings denied, and all tests).
- Put tests in the crate they cover, in its single integration test binary (`tests/all.rs` and `tests/all/*.rs`). Tests must not need a real broker account or network access; use the stand-ins under `tests/all/support/`.
- A package's `tick-package.json` version must equal its Cargo version. Do not bump versions in a pull request; releases do that (see [Releasing](README.md#releasing)).
- The financial Ticks and provider interfaces live in `market/` here. Changes that need the domain-agnostic Tick crates (protocol, SDK, UI, package credentials, OAuth) changed go to [tickloopai/tickloop](https://github.com/tickloopai/tickloop) first; build against a local checkout with `.cargo/config.toml.example` meanwhile.
- `marketplace.json` and the version table in the README are updated by the release workflow; do not edit them by hand.
- Packages must not widen what the user has authorized: installing grants no market, account or trading permission, and credentials come only from Tickloop.

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
