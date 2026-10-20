# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

The official Tick marketplace for Tickloop and the source of its Tick packages. Each package under `packages/<name>/` is a Rust crate that builds one program `tick-<name>`, shipped together with `tick-package.json` and a `skills/` directory. Tickloop runs the program as a Tick (a protocol-speaking terminal view); the Agent learns how to open it from the package's Skill. Only macOS on Apple Silicon is built and shipped.

## Commands

```sh
make build           # cargo build --locked --workspace --bins
make test            # node --test scripts/*.test.mjs, then cargo nextest (requires cargo-nextest)
make check           # cargo fmt --check, clippy --all-targets -D warnings, then make test (run before a PR)
make package-ticks   # build into dist/ticks/<package>/ and the release tarball; prints marketplace.json entries
make dev-ticks       # package-ticks, then `tickloop tick install --path dist/ticks/<package>`
```

Select packages with `TICK_PACKAGES="longbridge"`; point `dev-ticks` at a specific binary with `TICKLOOP=<path>`.

Running a single test: tests run under nextest, and `make test` isolates them with a throwaway Tickloop config root and disables proxies for loopback. Mirror that when running a subset:

```sh
TICKLOOP_CONFIG_DIR=$(mktemp -d) NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  cargo nextest run --locked -p tickloop-tick-longbridge -E 'test(rate_limit::)'
```

Crate names are `tickloop-tick-longbridge` and `tickloop-tick-futu`. Integration test names are prefixed by their module in `tests/all/` (e.g. `entry::…`, `auth::…`, `package_skill::…`). Release script tests: `node --test scripts/release.test.mjs`.

Live Longbridge tests are gated behind the `live-api` feature and `#[ignore]` (interactive device login): `cargo test -p tickloop-tick-longbridge --features live-api -- --ignored`, run only in a real terminal.

## Architecture

- **Tick crates come from tickloopai/tickloop by Git tag.** The root `Cargo.toml` pins `tickloop-tick-{command,market,oauth,packages,provider,sdk,test-support}` and `tickloop-ui` to one release tag; their library names are `tick_command`, `tick_market`, `tick_provider`, etc. The same tag must also be used for the `crossterm` entry in `[patch.crates-io]`. Upgrade by changing every `tag = "…"` and running `cargo update -p tickloop-tick-command`. For cross-repo changes, clone tickloop to `../tickloop` and copy `.cargo/config.toml.example` to `.cargo/config.toml`; lock entries resolved through that override are invalid for CI, so remove it and `cargo update` against the tag before committing `Cargo.lock`.
- **Program shape.** Each `main.rs` hands a clap `Cli` to `tick_command::entry::run`. A Tick subcommand builds provider implementations (`OptionsApi`, `QuoteApi`/trade, `NewsApi`, `CommunityApi` from `tick_provider`) into `entry::Services` and calls `entry::run_tick`; rendering and the Tick protocol live in the Tickloop crates, not here. `describe` prints Tick descriptors. `--once` and `describe` are refused when running inside a Tickloop instance (`entry::refuse_in_instance`).
- **longbridge** (`packages/longbridge/src/`): `entry.rs` dispatches; when hosted (`entry::in_instance()`), only offline checks run before `connecting::run_hosted` answers the host, and network work happens after. `sdk.rs` adapts the Longbridge SDK to the provider traits; `news_api.rs`/`community_api.rs` cover HTTP endpoints the SDK lacks; `rate_limit.rs` enforces SDK limits; `auth/` + `login.rs` handle OAuth (device flow, region issuers in `region.rs`). `probe.rs` is the `probe-fundamentals` entitlement report; `build.rs` embeds the locked SDK git revision for it.
- **futu** (`packages/futu/src/lib.rs`): a hand-written read-only client for Futu OpenD's JSON protocol over TCP, loopback only, implementing `OptionsApi`.
- **Authentication is Tickloop's job.** Packages never store credentials of their own: Tickloop keeps them under `<config root>/credentials/<package>/` and passes `TICKLOOP_OAUTH_TOKEN_FILE` (OAuth) or the manifest's `env` variable (API key). `tick-longbridge` renews the shared token file itself under its lock, which is why `vendor/longbridge-oauth` patches the SDK's OAuth crate with a `TokenRefresher`. `vendor/dotenv` stops the SDK from reading `.env`. Each vendored crate's README states the reason and the exact changes; the vendored crates are excluded from the workspace.
- **Tests.** Each crate has exactly one integration test binary: `tests/all.rs` declaring modules from `tests/all/*.rs` via `#[path]`. Tests must not need a real broker account or network; use the stand-ins in `tests/all/support/` (fake OpenD, local token endpoint, hosted harness). PTY-driven tests share a limited concurrency group (`.config/nextest.toml`); name them with `pty` or put them in `native_visible_stdio`. The root `tests/package_skill.rs` is included into every crate's test binary and checks that each Skill names only tools the Tickloop Agent offers (`AGENT_TOOLS`, kept in sync with tickloop's agent runtime) and uses `${TICK_PACKAGE_ROOT}`.

## Rules from CONTRIBUTING and README

- A package's `tick-package.json` `version` must equal its Cargo version (tests check this). Do not bump versions in normal changes; `make release` does that.
- Never hand-edit `marketplace.json` or the README's package version table; the release workflow commits them.
- Packages must not widen authorization: installing grants no market, account or trading permission, and credentials come only from Tickloop.
- Changes that need the Tick crates changed go to tickloopai/tickloop first.

## Releasing

`make release` (or `node scripts/release.mjs`, `--check` to preview) bumps each selected package's patch version (or `VERSION=x.y.z`), commits `chore(release): …`, and pushes one `<package>-v<version>` tag per package from a temporary checkout. Each tag triggers `.github/workflows/release-ticks.yml`: build and verify the archive, publish the GitHub Release and update `marketplace.json` on the default branch, then verify downloads by SHA-256. If a stage fails, re-run the failed jobs rather than releasing again.
