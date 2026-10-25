# Contributing

Issues and pull requests are welcome. Agent instructions and the repository layout are in [AGENTS.md](AGENTS.md); terms are in [CONTEXT.md](CONTEXT.md).

## Development environment

The packages build with current stable Rust (edition 2024) on macOS and Linux. Linux builds of the released musl targets need `musl-tools` (`musl-gcc`). `make test` needs [cargo-nextest](https://nexte.st) (`cargo install --locked cargo-nextest`) and Node.js for the release script tests.

The packages link the domain-agnostic Tick crates from [tickloopai/tickloop](https://github.com/tickloopai/tickloop) (the Tick protocol and SDK, terminal UI, package credentials and OAuth), fetched by Git from Tickloop's `main` while the Tick protocol is a draft; `Cargo.lock` records the exact commit. To move to Tickloop's latest `main`, run `cargo update -p tickloop-tick-sdk` and commit `Cargo.lock`. The dependency follows the Tick protocol, not Tickloop's release versions: once the protocol is stable, `branch = "main"` becomes the protocol's own tag.

### Building against a local Tickloop

```sh
git clone https://github.com/tickloopai/tickloop ../tickloop
cp .cargo/config.toml.example .cargo/config.toml
```

`.cargo/config.toml` is ignored by Git. Lock entries resolved through it are not valid for CI, so remove it and run `cargo update -p tickloop-tick-sdk` before committing `Cargo.lock`. Changes the domain-agnostic Tick crates need go to tickloopai/tickloop first.

## Checks

```sh
make build           # cargo build --locked --workspace --bins
make test            # release script tests (node --test scripts/*.test.mjs), then cargo nextest
make check           # cargo fmt --check, clippy --all-targets -D warnings, then make test
make package-ticks   # build into dist/ticks/<target>/<package>/ and its archive, and print the marketplace.json entries
make dev-ticks       # package-ticks, then tickloop tick install --path dist/ticks/<host target>/<package>
```

Run `make check` before opening a pull request. Select packages with `TICK_PACKAGES="longbridge"`; `make package-ticks` builds for this machine, or for each target in `TICK_TARGETS`, and publishes nothing. `make dev-ticks` installs with the `tickloop` on your `PATH`, or the one named by `TICKLOOP=<path>`, replacing any installed package of the same name.

`make test` isolates the tests with a throwaway Tickloop config root and disables proxies for loopback. Mirror that when running a subset:

```sh
TICKLOOP_CONFIG_DIR=$(mktemp -d) NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  cargo nextest run --locked -p tickloop-tick-longbridge -E 'test(rate_limit::)'
```

## Writing tests

- Put tests in the crate they cover. Each crate has at most one integration test binary, `tests/all.rs`, declaring modules from `tests/all/*.rs` with `#[path]`; test names are prefixed by that module (`entry::…`, `auth::…`, `package_skill::…`).
- Tests must not need a real broker account or network access. Use the stand-ins under `tests/all/support/` (a fake OpenD, a local token endpoint, a hosted harness) and the Mock Providers in `market/test-support`.
- Tests that drive a PTY share a limited concurrency group (`.config/nextest.toml`): name them with `pty` or put them in a `native_visible_stdio` module.
- The root `tests/package_skill.rs` is included into every package's test binary. It checks that each Package Skill names only tools the Tickloop Agent offers (`AGENT_TOOLS`, kept in sync with tickloop's agent runtime) and does not use `${TICK_PACKAGE_ROOT}`.
- Live Longbridge tests are behind the `live-api` feature and `#[ignore]` (interactive device login). Run them only in a real terminal: `cargo test -p tickloop-tick-longbridge --features live-api -- --ignored`.

## Pull requests

- Commits follow Conventional Commits (`feat:`, `fix:`, `docs:`, …); release commits are `chore(release): …`.
- A package's `tick-package.json` version must equal its Cargo version (the tests check it). Do not bump versions in a pull request; releases do that.
- `marketplace.json` and the "Packages" section of the README are updated by the release; do not edit them by hand.
- Packages must not widen what the user has authorized: installing grants no market, account or trading permission, and credentials come only from Tickloop.

## Package manifest

`tick-package.json` names the package (`name`: lowercase letters, digits and `-`), its `version` (equal to the crate's Cargo version), a `description`, the `executable` inside the package (named `tick-<name>`), `protocol` (whether the program speaks the Tick protocol), and an optional `auth`:

- `{"type": "none"}` (the default): no login.
- `{"type": "api_key", "env": "<VARIABLE>", "label": "<label>"}`: Tickloop stores a key the user enters and passes it to the program in that environment variable.
- `{"type": "oauth2", "issuers": [{"label": "<name>", "issuer": "<https URL>"}, …], "scopes"?: [...]}`: Tickloop logs in with the OAuth 2.0 device flow and passes the token file's path in `TICKLOOP_OAUTH_TOKEN_FILE`; the program renews the token itself.

Credentials live in Tickloop's config root under `credentials/<package>/`, never in a package. `longbridge` uses `oauth2`; `futu` needs no login because the user logs in inside Futu OpenD.

## Releasing

```sh
scripts/release-local.sh check .               # make check on this Mac and both Linux machines; run before releasing
make release                                   # every package, each from its own version, patch + 1; built here
make release TICK_PACKAGES=longbridge          # only longbridge
VERSION=0.3.0 make release TICK_PACKAGES=futu  # an explicit, higher version
make release REMOTE=1                          # the same, but GitHub Actions checks, builds and publishes
node scripts/release.mjs --check               # preview; changes nothing
```

Each release is one `<package>-v<version>` tag carrying `tick-<package>-<version>-<target>.tar.gz` for all four targets. Versions come from each package's `Cargo.toml`, kept in step with its `tick-package.json` and `Cargo.lock`, independently of other packages and of Tickloop. A version must be newer than the package's existing tags and its `marketplace.json` entry.

The release runs from the committed `HEAD` of a branch that includes `origin`'s commits (pull first: releases commit marketplace updates there); other working-tree changes are left alone. In a temporary checkout it bumps the selected versions and runs `cargo check`, then commits the version files as one `chore(release): …`, creates one tag per package on that commit, and pushes the branch and tags atomically, at most three tags per push.

**Local** (`make release`, `scripts/release-local.sh`): runs no tests, so run `scripts/release-local.sh check .` first; it needs the OrbStack Linux machines `tl-arm` and `tl-x86` (`TICKS_LINUX_ARM64_MACHINE`, `TICKS_LINUX_X86_64_MACHINE`; a missing toolchain is installed in them). In a snapshot of the bumped versions it builds and checks every package for all four targets, the macOS ones on this Mac and each Linux one in the machine of its architecture. Only when every build passed does it commit and push the tags; it then publishes each Release with its four archives, updates `marketplace.json` from a temporary checkout of the default branch, verifies the downloads and fast-forwards your branch to the marketplace update. It needs OrbStack, Rosetta, the `aarch64-apple-darwin` and `x86_64-apple-darwin` Rust targets, and `gh` logged in with write access. If publishing fails after the push, fix the cause and run `scripts/release-local.sh publish dist/release-local <tags>`, which keeps what was published.

**Remote** (`make release REMOTE=1`): pushes the version commit and tags the same way, without local checks or builds, and starts `.github/workflows/release-ticks.yml` on each tag with `gh workflow run`. To release entirely from GitHub, run the **Release** workflow from the Actions tab on the default branch, with the packages (empty for every package) and an optional version; it runs `node scripts/release.mjs` there and then starts the release workflow on each tag it pushed, since tags pushed with a workflow's `GITHUB_TOKEN` start no workflow by themselves.

Pushing a tag starts nothing by itself. The release workflow, one package at a time:

0. CI: the full suite on macOS and Linux (`.github/workflows/ci.yml`). Pushes and pull requests do not start CI, to save hosted runners; run `make check` locally, or start CI by hand from the Actions tab.
1. Build and check, once per target on a runner of that architecture (`macos-15` for both macOS targets, the x86_64 one under Rosetta; `ubuntu-24.04` and `ubuntu-24.04-arm` for Linux): build the program, check the archive's contents, its architecture (and on Linux that it is statically linked), the ad-hoc signature on macOS, that `--help` runs and `--version` reports the version, and install it with the latest released Tickloop for that runner.
2. Publish, only after all four targets built: create the package's Release on its tag with every target's archive (completing a Release an earlier attempt left part way), then commit the updated `marketplace.json` and the README's "Packages" section to the default branch. A Release that already exists with the same archive is kept; a different archive is refused, and `marketplace.json` never moves to an older version.
3. Verify: download every asset anonymously and compare its SHA-256.

If a stage fails, fix the cause and re-run the failed jobs; do not release again to bump versions. The job log prints the commands that finish or withdraw a partial release.

A new target is added in `targets` of `scripts/release-ticks.sh`, the build matrix of `release-ticks.yml` and `host_target` of `scripts/package-ticks.sh`.

Repository settings: the publish job writes with the workflow's own `GITHUB_TOKEN` (it requests `contents: write`), so no secret is needed; if the default branch is protected, let `github-actions[bot]` push to it. The publish job runs in the `tick-marketplace` environment, where you can require reviewers. The build needs `tickloopai/tickloop` to be public to fetch the Tick crates.

## License

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE).
