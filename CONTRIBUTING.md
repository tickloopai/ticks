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
make dev-ticks       # package-ticks, then install each into the development config root ~/.tickloop-dev
```

Run `make check` before opening a pull request. Select packages with `TICK_PACKAGES="longbridge"`; `make package-ticks` builds for this machine, or for each target in `TICK_TARGETS`, and publishes nothing. `make dev-ticks` installs with the `tickloop` on your `PATH`, or the one named by `TICKLOOP=<path>`, into the config root Tickloop's `make dev` uses, `~/.tickloop-dev` unless `TICKLOOP_DEV_CONFIG_DIR` names another, replacing any installed package of the same name there; a `tickloop` run with the default config root does not see them. A config root that keeps Ticks as plugins needs a `tickloop` that manages plugins, so point `TICKLOOP` at the development build, and uninstall a marketplace-installed Tick of the same name first (`tickloop tick uninstall <name> --keep-data`).

`make test` isolates the tests with a throwaway Tickloop config root and disables proxies for loopback. Mirror that when running a subset:

```sh
TICKLOOP_CONFIG_DIR=$(mktemp -d) NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \
  cargo nextest run --locked -p tickloop-tick-longbridge -E 'test(rate_limit::)'
```

## Tick screens in Monitor cards

Tickloop shows a hosted Tick only as a card in its Monitor column, and the user's keys go straight to the Tick while the card has focus; the rules are in the `screen` section of `tick/protocol/SPECIFICATION.md` in tickloopai/tickloop. Every financial Tick runs through `runtime::run` in `market/views/src/runtime.rs`, which implements the shared parts below, so a view only has to lay itself out and use its keys as described here.

- **Frame.** The runtime draws a whole frame around every view and hands the view only what is inside it, so nothing a view draws can show outside the frame. The top edge carries only the title the view gives through `ViewState::title`, as the Monitor's row already shows when the Tick updates; the bottom edge carries the Help and Quit keys on its right and the last error on its left, and the keys give way to the help key alone before what the left says is cut. No row goes to a title or a status of its own. A view whose content is bordered cards (Quote, Watchlist) draws no frame (`LiveView::framed`): its cards fill the screen, and the last card shown carries the bottom edge's labels, led by which cards show when not all fit. The Monitor already names the Tick and its symbols, so a title leaves out the view's name (no `Quotes`, `News · Market`) and keeps what tells this view apart, such as the period, group, account or mode; it must fit whole on a 30-column frame (`runtime::frame_title_columns`), so give each view a shorter fallback rather than let it be cut.
- **Width.** A card is 40 columns unless the user narrows it, down to 30 (`MINIMUM_PANEL_COLUMNS`), its frame included. Lay every view out for 30 columns: give up secondary columns and shorten names and key hints first (`draw_table` column priorities, `fit_hints`, `pack_fields`). Scrolling over the minimum layout is only the fallback for a Panel smaller than the minimum.
- **Height.** `TickKind::minimum_panel_size().rows` is declared to the host as `screen.min_rows`: the fewest rows the Tick stays usable at, its frame included, from 6 to 14. `TickKind::maximum_rows()` is the most rows a card puts to use when the Monitor has room; declare one only for a view that shows more with more rows (a list, table, chart or document) and leave a view such as Quote at its minimum; it is declared as `screen.max_rows`.

  | Tick | Minimum rows | Maximum rows |
  | --- | --- | --- |
  | Quote | 6 | — |
  | Watchlist | 6 | 24 |
  | Positions | 8 | 20 |
  | News, Community | 10 | 24 |
  | Reader | 10 | 30 |
  | Earnings | 14 | 24 |
  | Kline (and its event chart), Options | 14 | 30 |

- **Redrawing.** The runtime composes each frame in memory and writes it only when it differs from the one on screen. When the terminal's size changes (`SIGWINCH`, or a size found to differ before a frame is written) it clears the screen and draws the whole frame again; a frame composed for one size is never written to a terminal of another. A card resizes often, as other cards open and close and as the user drags the Monitor wider, so a view must lay itself out from the size it is given each frame and keep no layout from the last.
- **Keys.** No key ends a hosted Tick: Tickloop ends it with `SIGTERM` or by closing its input, so q and Ctrl+C only quit a Tick run in a terminal of its own. Ctrl+C and Esc cancel an open editor; editors use the plain editing keys only. Outside an editor Esc goes back a level as Backspace does: it closes the help overlay, a popup or an article. Never make a function need Esc twice, since Tickloop takes a second Esc within 300 ms to leave the card.
- **Looking and operating.** A card the user has opened but not entered is only looked at; one the user has entered is operated. A hosted Tick turns on focus reporting (`ESC [ ? 1004 h`) and the host reports entering with `ESC [ I` and leaving with `ESC [ O`; until the first report the card counts as looked at. A looked-at card shows no key hints: none on the frame's bottom edge or in footers, prompts or popups. A line that holds only keys stays, blank, so entering the card moves nothing (`runtime::KEY_LINE_ROWS`). The runtime clears `Settings::key_hints` for such frames, so draw every hint through it (`runtime::key_hints`), and redraws the whole screen when the focus changes. Looking and operating decide only what the screen shows: a looked-at card still answers every key and Tick Tool call, since the Agent sends keys to cards the user is not in. A Tick run in a terminal of its own always shows its keys.
- **Handing keys on.** A hosted Tick announces `ESC ] 7777 ; tickloop ; bubble=on BEL` on its terminal when it starts, and writes `ESC ] 7777 ; tickloop ; key=<name> BEL` after its screen for each of `esc`, `up`, `down`, `left`, `right`, `tab` and `backtab` (Shift+Tab) it has no use for, so the user moves on to the next card, the list or the input. The runtime decides by the screen: a key that leaves it unchanged, such as down on the last row, left on the first candle, Esc with nothing to close or Tab with no field to move to, is handed on. A view therefore needs no code of its own for this, but a key it uses must change the screen, and one it ignores must change nothing. An open editor keeps every key.
- **Tests.** `market/views/tests/all/minimum_width.rs` lays every Tick and mode out at 30 columns by its minimum rows, 40 by its minimum and 40 by its maximum, and checks its key fields and its whole frame; a new view or mode adds its scenarios to `every_tick_at_the_minimum_width`. The same file checks that a looked-at card shows no key hints, still answers keys, and leaves nothing stale as the focus switches back and forth, and that entering a card moves none of its content. `themed_runtime.rs` drags a card through two dozen sizes down to 30×6 and checks which keys are handed on, and the `native_visible_stdio` tests run hosted Ticks on a real PTY.

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
