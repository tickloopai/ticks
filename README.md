# Tickloop Ticks

The official Tick marketplace for [Tickloop](https://tickloop.ai), and the source of its Tick packages. It lists the Tick packages you can install into Tickloop, hosts their prebuilt programs as release assets, and holds the code they are built from.

A Tick package is a prebuilt program, its manifest `tick-package.json` and a `skills/` directory, grouped by data source. Tickloop opens the program as a Tick, and the Agent learns to use it from the package's Skills.

```sh
tickloop tick list --available
tickloop tick install longbridge
tickloop tick uninstall longbridge
```

Installing downloads the package for your platform, checks its SHA-256 against `marketplace.json`, and places it under `~/.tickloop/ticks/`. No Rust toolchain is needed. Installing a package never opens or subscribes to a Tick and grants no market, account or trading permission; you log in to a package's data source with `/ticks` in the Agent, or with `tickloop tick login <package>` in a terminal.

Every package version is prebuilt for macOS on Apple Silicon and Intel (`aarch64-apple-darwin`, `x86_64-apple-darwin`) and for Linux on ARM64 and x86_64 (`aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`). The Linux programs are static musl builds, as the Linux Tickloop is, so they run on any distribution.

## Packages

Install one with `tickloop tick install <package>`.

| Package | Version | Description |
| --- | --- | --- |
| [`futu`](packages/futu) | 0.2.5 | Futu OpenD option chains, option pricing and implied volatility |
| [`longbridge`](packages/longbridge) | 0.2.5 | Longbridge quotes, watchlist, kline, positions, news, earnings and community posts |
| [`tickloop`](packages/tickloop) | 0.1.1 | Official Tickloop Ticks: the Reader for original financial documents from URLs or local files |

## Layout

- `marketplace.json`: every package with its version, description and per-platform download URL and SHA-256. Tickloop reads it from this path, so it stays at the repository root.
- `packages/<package>/`: one Rust crate per package, building its program `tick-<package>`, with `tick-package.json` and `skills/`. `tickloop` holds the official Tickloop Ticks as subcommands of `tick-tickloop`, starting with the Reader.
- `market/`: the financial Ticks the packages run, and the provider interfaces they read: `provider` (`tick_provider`), `views` (`tick_market`), `command` (`tick_command`), and `test-support` (mock providers and `tick-mock`, for tests only).
- `tests/package_skill.rs`: checks shared by every package's Skill, such as naming only tools the Agent offers.
- `vendor/`: patched third-party crates used through the root `Cargo.toml`; each has a README with the reason and the changes.
- Releases: one release per package version, tagged `<package>-v<version>`, carrying `tick-<package>-<version>-<target>.tar.gz`.

## Manifest

`tick-package.json` names the package (`name`: lowercase letters, digits and `-`), its `version` (equal to the crate's Cargo version; the tests check it), a `description`, the `executable` inside the package (named `tick-<name>`), `protocol` (whether the program speaks the Tick protocol), and an optional `auth`:

- `{"type": "none"}` (the default): no login.
- `{"type": "api_key", "env": "<VARIABLE>", "label": "<label>"}`: Tickloop stores a key the user enters and passes it to the program in that environment variable.
- `{"type": "oauth2", "issuers": [{"label": "<name>", "issuer": "<https URL>"}, …], "scopes"?: [...]}`: Tickloop logs in with the OAuth 2.0 device flow and passes the token file's path in `TICKLOOP_OAUTH_TOKEN_FILE`; the program renews the token itself.

Tickloop is the authentication client: credentials live in its config root under `credentials/<package>/`, never in a package. `longbridge` uses `oauth2`; `futu` needs no login because you log in inside Futu OpenD.

## Building from source

The packages build with current stable Rust (edition 2024) on macOS and Linux; Linux builds of the released musl targets need `musl-tools` (`musl-gcc`). They link the domain-agnostic Tick crates from [tickloopai/tickloop](https://github.com/tickloopai/tickloop) (the Tick protocol and SDK, terminal UI, package credentials and OAuth), fetched by Git from Tickloop's `main` while the Tick protocol is a draft (`Cargo.lock` records the exact commit); the financial Ticks themselves live in `market/`.

```sh
make build           # cargo build of both programs
make check           # cargo fmt --check, clippy -D warnings, then make test
make test            # release script tests and cargo nextest (cargo install --locked cargo-nextest)
make package-ticks   # build for this machine into dist/ticks/ and print each marketplace.json entry
make dev-ticks       # package-ticks, then tickloop tick install --path dist/ticks/<package>
```

`make package-ticks` assembles `dist/ticks/<target>/<package>/` and the archive `dist/ticks/tick-<package>-<version>-<target>.tar.gz` (which unpacks to that directory) for this machine's target, or for each target in `TICK_TARGETS`, with this repository's license and the third-party notices; it publishes nothing. `make dev-ticks` installs with the `tickloop` on your `PATH`, or the one named by `TICKLOOP=<path>`, replacing any installed package of the same name. Select packages with `TICK_PACKAGES="longbridge"`.

To build against a local Tickloop checkout instead of the locked commit, clone it beside this repository and copy the override:

```sh
git clone https://github.com/tickloopai/tickloop ../tickloop
cp .cargo/config.toml.example .cargo/config.toml
```

`.cargo/config.toml` is ignored by Git. Lock entries resolved through it are not valid for CI, so remove it and run `cargo update -p tickloop-tick-sdk` before committing `Cargo.lock`. To move to Tickloop's latest `main`, run the same command and commit `Cargo.lock`. The dependency follows the Tick protocol, not Tickloop's release versions: once the protocol is stable, `branch = "main"` becomes the protocol's own tag.

## Releasing

```sh
make release                                   # every package, each from its own version, patch + 1; built here
make release TICK_PACKAGES=longbridge          # only longbridge
VERSION=0.3.0 make release TICK_PACKAGES=futu  # an explicit, higher version
make release REMOTE=1                          # the same, but GitHub Actions checks, builds and publishes
node scripts/release.mjs --check               # preview; changes nothing
```

`make release` releases from this Mac, without GitHub Actions (`scripts/release-local.sh`). In a snapshot of the bumped versions it runs `make check` here and in two OrbStack Linux machines, `tl-arm` and `tl-x86` (`TICKS_LINUX_ARM64_MACHINE`, `TICKS_LINUX_X86_64_MACHINE`; a missing toolchain is installed in them), then builds and checks every package for all four targets, the macOS ones here and each Linux one in the machine of its architecture. Only when everything passed does it commit the versions and push the tags; it then publishes each Release with its four archives, updates `marketplace.json` from a temporary checkout of the default branch, verifies the downloads and fast-forwards your branch to the marketplace update. It needs OrbStack, Rosetta, the `aarch64-apple-darwin` and `x86_64-apple-darwin` Rust targets, and `gh` logged in with write access. If publishing fails after the push, fix the cause and run `scripts/release-local.sh publish dist/release-local <tags>`, which keeps what was published.

`make release REMOTE=1` pushes the version commit and tags the same way, without local checks or builds, and starts the release workflow below on each tag. To release entirely from GitHub instead, run the **Release** workflow from the Actions tab on the default branch, with the packages (empty for every package) and an optional version. It runs `make release` there and then starts the release workflow below on each tag it pushed, since tags pushed with a workflow's `GITHUB_TOKEN` start no workflow by themselves.

Versions come from each package's `Cargo.toml`, kept in step with its `tick-package.json` and `Cargo.lock`, independently of other packages and of Tickloop. The release runs from the committed `HEAD` of a branch that includes `origin`'s commits (pull first: the workflow commits marketplace updates there); other working-tree changes are left alone. In a temporary checkout it bumps the selected versions and runs `cargo check`, then commits the version files as one `chore(release): …`, creates one `<package>-v<version>` tag per package on that commit, and pushes the branch and tags atomically, at most three tags per push. A version must be newer than the package's existing tags and its `marketplace.json` entry.

Pushing a tag starts nothing; the release workflow runs on a tag only when `make release REMOTE=1` or the Release workflow starts it, queued one package at a time:

0. CI: the full suite on macOS and Linux (`.github/workflows/ci.yml`). Pushes and pull requests do not start CI, to save hosted runners; run `make check` locally, or start CI by hand from the Actions tab.
1. Build and check, once per target on a runner of that architecture (`macos-15` for both macOS targets, the x86_64 one under Rosetta; `ubuntu-24.04` and `ubuntu-24.04-arm` for Linux): build the program, check the archive's contents, its architecture (and on Linux that it is statically linked), the ad-hoc signature on macOS, that `--help` runs and `--version` reports the version, and install it with the latest released Tickloop for that runner.
2. Publish, only after all four targets built: create the package's Release on its tag with every target's archive (completing a Release an earlier attempt left part way), then commit the updated `marketplace.json`, which lists all four downloads, and the version table above to the default branch. A Release that already exists with the same archive is kept; a different archive is refused, and `marketplace.json` never moves to an older version.
3. Verify: download every asset anonymously and compare its SHA-256.

If a stage fails, fix the cause and re-run the failed jobs; do not release again to bump versions. The job log prints the commands that finish or withdraw a partial release.

Repository settings: the publish job writes with the workflow's own `GITHUB_TOKEN` (it requests `contents: write`), so no secret is needed; if the default branch is protected, let `github-actions[bot]` push to it. The publish job runs in the `tick-marketplace` environment, created on its first run, where you can require reviewers. The build needs `tickloopai/tickloop` to be public to fetch the Tick crates.

## License

[MIT](LICENSE). Third-party code and its licenses are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
