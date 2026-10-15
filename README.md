# Tickloop Ticks

The official Tick marketplace for [Tickloop](https://tickloop.ai), and the source of its Tick packages. It lists the Tick packages you can install into Tickloop, hosts their prebuilt programs as release assets, and holds the code they are built from.

A Tick package is a prebuilt program, its manifest `tick-package.json` and a `skills/` directory, grouped by data source. Tickloop opens the program as a Tick, and the Agent learns to use it from the package's Skills.

```sh
tickloop tick list --available
tickloop tick install longbridge
tickloop tick uninstall longbridge
```

Installing downloads the package for your platform, checks its SHA-256 against `marketplace.json`, and places it under `~/.tickloop/ticks/`. No Rust toolchain is needed. Installing a package never opens or subscribes to a Tick and grants no market, account or trading permission; you log in to a package's data source with `/ticks` in the Agent, or with `tickloop tick login <package>` in a terminal.

Prebuilt packages are currently provided for macOS on Apple Silicon (`aarch64-apple-darwin`) only.

## Packages

Install one with `tickloop tick install <package>`.

| Package | Version | Description |
| --- | --- | --- |
| [`futu`](packages/futu) | 0.2.3 | Futu OpenD option chains, option pricing and implied volatility |
| [`longbridge`](packages/longbridge) | 0.2.3 | Longbridge quotes, watchlist, kline, positions, news, earnings and community posts |

## Layout

- `marketplace.json`: every package with its version, description and per-platform download URL and SHA-256. Tickloop reads it from this path, so it stays at the repository root.
- `packages/<package>/`: one Rust crate per package, building its program `tick-<package>`, with `tick-package.json` and `skills/`.
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

The packages build with current stable Rust (edition 2024) on macOS. They link Tick crates from [tickloopai/tickloop](https://github.com/tickloopai/tickloop), fetched by Git at the commit pinned in the root `Cargo.toml`.

```sh
make build           # cargo build of both programs
make check           # cargo fmt --check, clippy -D warnings, then make test
make test            # release script tests and cargo nextest (cargo install --locked cargo-nextest)
make package-ticks   # build for Apple Silicon into dist/ticks/ and print each marketplace.json entry
make dev-ticks       # package-ticks, then tickloop tick install --path dist/ticks/<package>
```

`make package-ticks` assembles `dist/ticks/<package>/` and the archive `dist/ticks/tick-<package>-<version>-aarch64-apple-darwin.tar.gz` (which unpacks to that directory), with this repository's license and the third-party notices; it publishes nothing. `make dev-ticks` installs with the `tickloop` on your `PATH`, or the one named by `TICKLOOP=<path>`, replacing any installed package of the same name. Select packages with `TICK_PACKAGES="longbridge"`.

To build against a local Tickloop checkout instead of the pinned commit, clone it beside this repository and copy the override:

```sh
git clone https://github.com/tickloopai/tickloop ../tickloop
cp .cargo/config.toml.example .cargo/config.toml
```

`.cargo/config.toml` is ignored by Git. Lock entries resolved through it are not valid for CI, so remove it and run `cargo update` against the pinned commit before committing `Cargo.lock`. To move to a newer Tickloop, change every `rev = "…"` in `Cargo.toml` (or replace it with a Tickloop release tag) and run `cargo update -p tickloop-tick-command`.

## Releasing

```sh
make release                                   # every package, each from its own version, patch + 1
make release TICK_PACKAGES=longbridge          # only longbridge
VERSION=0.3.0 make release TICK_PACKAGES=futu  # an explicit, higher version
node scripts/release.mjs --check               # preview; changes nothing
```

Versions come from each package's `Cargo.toml`, kept in step with its `tick-package.json` and `Cargo.lock`, independently of other packages and of Tickloop. The release runs from the committed `HEAD` of a branch that includes `origin`'s commits (pull first: the workflow commits marketplace updates there); other working-tree changes are left alone. In a temporary checkout it bumps the selected versions and runs `cargo check`, then commits the version files as one `chore(release): …`, creates one `<package>-v<version>` tag per package on that commit, and pushes the branch and tags atomically, at most three tags per push. A version must be newer than the package's existing tags and its `marketplace.json` entry.

Each tag starts the release workflow, queued one package at a time:

1. Build and check: build the program, check the archive's contents, architecture, ad-hoc signature and reported version, and install it with the latest released Tickloop.
2. Publish: create the package's Release on its tag, then commit the updated `marketplace.json` and the version table above to the default branch. A Release that already exists with the same archive is kept; a different archive is refused, and `marketplace.json` never moves to an older version.
3. Verify: download every asset anonymously and compare its SHA-256.

If a stage fails, fix the cause and re-run the failed jobs; do not release again to bump versions. The job log prints the commands that finish or withdraw a partial release.

Repository settings: the publish job writes with the workflow's own `GITHUB_TOKEN` (it requests `contents: write`), so no secret is needed; if the default branch is protected, let `github-actions[bot]` push to it. The publish job runs in the `tick-marketplace` environment, created on its first run, where you can require reviewers. The build needs `tickloopai/tickloop` to be public to fetch the Tick crates.

## License

[MIT](LICENSE). Third-party code and its licenses are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
