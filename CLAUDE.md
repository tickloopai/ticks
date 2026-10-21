# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

The official Tick marketplace for Tickloop and the source of its Tick packages. Each package under `packages/<name>/` is a Rust crate that builds one program `tick-<name>`, shipped together with `tick-package.json` and a `skills/` directory. The financial Ticks those programs run live in `market/`; Tickloop itself stays domain-agnostic. Tickloop runs the program as a Tick (a protocol-speaking terminal view); the Agent learns how to open it from the package's Skill. Every package version ships for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl` (Linux builds are static musl and need `musl-tools`).

## Commands

```sh
make build           # cargo build --locked --workspace --bins
make test            # node --test scripts/*.test.mjs, then cargo nextest (requires cargo-nextest)
make check           # cargo fmt --check, clippy --all-targets -D warnings, then make test (run before a PR)
make package-ticks   # build into dist/ticks/<target>/<package>/ and the release tarball for this machine (or TICK_TARGETS); prints marketplace.json entries
make dev-ticks       # package-ticks, then `tickloop tick install --path dist/ticks/<host target>/<package>`
```

Select packages with `TICK_PACKAGES="longbridge"`; point `dev-ticks` at a specific binary with `TICKLOOP=<path>`.

Running a single test: tests run under nextest, and `make test` isolates them with a throwaway Tickloop config root and disables proxies for loopback. Mirror that when running a subset:

```sh
TICKLOOP_CONFIG_DIR=$(mktemp -d) NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  cargo nextest run --locked -p tickloop-tick-longbridge -E 'test(rate_limit::)'
```

Package crate names are `tickloop-tick-longbridge`, `tickloop-tick-futu` and `tickloop-tick-tickloop`; the `market/` crates are `tickloop-tick-{provider,market,command,test-support}` (directories `market/{provider,views,command,test-support}`). Integration test names are prefixed by their module in `tests/all/` (e.g. `entry::…`, `auth::…`, `package_skill::…`). Release script tests: `node --test scripts/release.test.mjs`.

Live Longbridge tests are gated behind the `live-api` feature and `#[ignore]` (interactive device login): `cargo test -p tickloop-tick-longbridge --features live-api -- --ignored`, run only in a real terminal.

## Architecture

- **Only domain-agnostic crates come from tickloopai/tickloop.** The root `Cargo.toml` takes `tickloop-tick-{oauth,oauth-test-server,packages,protocol,sdk}`, `tickloop-ui` and the `crossterm` patch by Git from Tickloop's `branch = "main"`: the Tick protocol is a draft, so draft = latest, and `Cargo.lock` holds the exact commit. Move to the latest with `cargo update -p tickloop-tick-sdk` and commit `Cargo.lock`. Track the protocol, never Tickloop's frequent app versions (`v0.2.x`); when the protocol is stable, replace the branch with the protocol's own tag. Tickloop must never depend on this repository. For cross-repo changes, copy `.cargo/config.toml.example` to `.cargo/config.toml` and point it at a tickloop checkout or worktree; lock entries resolved through that override are invalid for CI, so remove it and run `cargo update -p tickloop-tick-sdk` before committing `Cargo.lock`.
- **market** (`market/`): the financial Ticks moved out of Tickloop. `provider` defines the provider traits (`QuoteApi`, `TradeApi`, `NewsApi`, `OptionsApi`, `CommunityApi`); `views` (`tick_market`) holds the nine Ticks, their Agent tools and terminal rendering (incl. `charts`); `command` (`tick_command`) is the CLI and `entry` glue; `test-support` holds mock providers and `tick-mock` (tests only; the release refuses a program that links it).
- **tickloop** (`packages/tickloop/src/main.rs`): the official Tickloop package; each official Tick is a subcommand of `tick-tickloop` (`reader` today).
- **Program shape.** Each `main.rs` hands a clap `Cli` to `tick_command::entry::run`. A Tick subcommand builds provider implementations (`OptionsApi`, `QuoteApi`/trade, `NewsApi`, `CommunityApi` from `tick_provider`) into `entry::Services` and calls `entry::run_tick`; rendering lives in `market/`, and the Tick protocol in the Tickloop crates. `describe` prints Tick descriptors. `--once` and `describe` are refused when running inside a Tickloop instance (`entry::refuse_in_instance`).
- **longbridge** (`packages/longbridge/src/`): `entry.rs` dispatches; when hosted (`entry::in_instance()`), only offline checks run before `connecting::run_hosted` answers the host, and network work happens after. `sdk.rs` adapts the Longbridge SDK to the provider traits; `news_api.rs`/`community_api.rs` cover HTTP endpoints the SDK lacks; `rate_limit.rs` enforces SDK limits; `auth/` + `login.rs` handle OAuth (device flow, region issuers in `region.rs`). `probe.rs` is the `probe-fundamentals` entitlement report; `build.rs` embeds the locked SDK git revision for it.
- **futu** (`packages/futu/src/lib.rs`): a hand-written read-only client for Futu OpenD's JSON protocol over TCP, loopback only, implementing `OptionsApi`.
- **Authentication is Tickloop's job.** Packages never store credentials of their own: Tickloop keeps them under `<config root>/credentials/<package>/` and passes `TICKLOOP_OAUTH_TOKEN_FILE` (OAuth) or the manifest's `env` variable (API key). `tick-longbridge` renews the shared token file itself under its lock, which is why `vendor/longbridge-oauth` patches the SDK's OAuth crate with a `TokenRefresher`. `vendor/dotenv` stops the SDK from reading `.env`. Each vendored crate's README states the reason and the exact changes; the vendored crates are excluded from the workspace.
- **Tests.** Each crate has exactly one integration test binary: `tests/all.rs` declaring modules from `tests/all/*.rs` via `#[path]`. Tests must not need a real broker account or network; use the stand-ins in `tests/all/support/` (fake OpenD, local token endpoint, hosted harness). PTY-driven tests share a limited concurrency group (`.config/nextest.toml`); name them with `pty` or put them in `native_visible_stdio`. The root `tests/package_skill.rs` is included into every package's test binary and checks that each Skill names only tools the Tickloop Agent offers (`AGENT_TOOLS`, kept in sync with tickloop's agent runtime) and does not use `${TICK_PACKAGE_ROOT}`.

## Rules from CONTRIBUTING and README

- A package's `tick-package.json` `version` must equal its Cargo version (tests check this). Do not bump versions in normal changes; `make release` does that.
- Never hand-edit `marketplace.json` or the README's package version table; the release workflow commits them.
- Packages must not widen authorization: installing grants no market, account or trading permission, and credentials come only from Tickloop.
- Changes to the domain-agnostic Tick crates go to tickloopai/tickloop first; financial Tick and provider changes belong in `market/` here.

## Releasing

`make release` (or `node scripts/release.mjs`, `--check` to preview) bumps each selected package's patch version (or `VERSION=x.y.z`), commits `chore(release): …`, and pushes one `<package>-v<version>` tag per package from a temporary checkout. Each tag triggers `.github/workflows/release-ticks.yml`: build and verify one archive per target on a runner of that architecture (`scripts/release-ticks.sh build` with `RELEASE_TARGET`), and only when all four pass, publish the GitHub Release with every archive and update `marketplace.json` on the default branch, then verify downloads by SHA-256. A target is added in `targets` of `scripts/release-ticks.sh`, the build matrix of that workflow and `host_target` of `scripts/package-ticks.sh`. If a stage fails, re-run the failed jobs rather than releasing again.
