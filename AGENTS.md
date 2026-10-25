# Tickloop Ticks

The official Tick marketplace for Tickloop and the source of its Tick packages. Each package under `packages/<name>/` is a Rust crate that builds one program `tick-<name>`, shipped with its `tick-package.json` and a `skills/` directory. The financial Ticks those programs run live in `market/`; Tickloop itself stays domain-agnostic. Tickloop runs a package program as a Tick, and the Agent learns how to open it from the package's Skill. Every package version ships for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl`.

## Baseline and documents

- Shared terms follow Tickloop's [CONTEXT.md](https://github.com/tickloopai/tickloop/blob/main/CONTEXT.md); terms specific to this repository are in [CONTEXT.md](./CONTEXT.md). Use those names in code, UI and discussion.
- When a change conflicts with documented behavior, say so; never change a meaning silently. When documents and source disagree, the source wins.
- Division of documents: agent instructions live only in AGENTS.md (no CLAUDE.md); CONTEXT.md holds term definitions only; README.md describes user-facing usage only; development, checks, tests and releases are in [CONTRIBUTING.md](./CONTRIBUTING.md).
- `.scratch/` and `docs/` are maintainer-local and excluded by `.gitignore`; a new feature's spec goes in `.scratch/<feature>/`. Pointers to them apply only when they exist.
- Maintainer-local supplement: @~/tickloopai/AGENTS.md (ignore when absent).

## Core behavior

- **Program shape**: each package's `main.rs` hands a clap `Cli` to `tick_command::entry::run`. A Tick subcommand builds provider implementations into `entry::Services` and calls `entry::run_tick`; rendering lives in `market/views`, the Tick protocol in the Tickloop crates. `describe` prints Tick descriptors. `--once` and `describe` are refused inside a Tickloop instance (`entry::refuse_in_instance`).
- **Hosted startup**: when hosted (`entry::in_instance()`), a program runs only offline checks before `connecting::run_hosted` answers the host; network work and connecting to the data source happen afterwards.
- **Package Skills** (`packages/<name>/skills/*/SKILL.md`) ship to the Agent with the package and are product content. They open the installed program by name, leave logging in to Tickloop, and name only tools the Tickloop Agent offers; `tests/package_skill.rs` checks this against `AGENT_TOOLS`, which must be kept in sync with tickloop's agent runtime.

## Permissions and credentials

- Packages must not widen authorization: installing grants no market, account or trading permission.
- Authentication is Tickloop's job. Packages store no credentials of their own: Tickloop keeps them under `<config root>/credentials/<package>/` and passes `TICKLOOP_OAUTH_TOKEN_FILE` (OAuth) or the manifest's `env` variable (API key).
- `tick-longbridge` renews the shared token file itself under its lock; that is why `vendor/longbridge-oauth` patches the SDK's OAuth crate with a `TokenRefresher`. `vendor/dotenv` stops the SDK from reading `.env`. Each vendored crate's README states the reason and the exact changes; vendored crates are excluded from the workspace.
- `futu` talks to Futu OpenD on loopback only and is read-only.

## Structure and dependency boundaries

| Path | Crate | Responsibility |
| --- | --- | --- |
| `market/provider/` | `tickloop-tick-provider` (`tick_provider`) | Provider traits: `QuoteApi`, `TradeApi`, `NewsApi`, `OptionsApi`, `CommunityApi` |
| `market/views/` | `tickloop-tick-market` (`tick_market`) | The nine financial Ticks (quote, watchlist, kline, positions, news, earnings, options, reader, community), their Agent tools and terminal rendering, including charts |
| `market/command/` | `tickloop-tick-command` (`tick_command`) | CLI flags, configuration, locale check, `describe`, `entry` and hosted `connecting` |
| `market/test-support/` | `tickloop-tick-test-support` | Mock providers and the `tick-mock` program; tests only, and the release refuses a program that links it |
| `packages/longbridge/` | `tickloop-tick-longbridge` | `tick-longbridge`: `entry.rs` dispatch, `sdk.rs` SDK adapter, `news_api.rs`/`community_api.rs` for HTTP endpoints the SDK lacks, `rate_limit.rs`, `auth/` + `login.rs` + `region.rs` OAuth device flow, `probe.rs` (`probe-fundamentals`; `build.rs` embeds the locked SDK revision) |
| `packages/futu/` | `tickloop-tick-futu` | `tick-futu`: a hand-written client for Futu OpenD's JSON protocol over TCP, implementing `OptionsApi` |
| `packages/tickloop/` | `tickloop-tick-tickloop` | `tick-tickloop`: official Tickloop Ticks as subcommands (`reader` today) |
| `tests/package_skill.rs` | — | Skill checks included into every package's test binary |
| `vendor/` | — | Patched third-party crates (`dotenv`, `longbridge-oauth`), each with a README |

Dependency boundaries:

- Only domain-agnostic crates come from tickloopai/tickloop: `tickloop-tick-{oauth,oauth-test-server,packages,protocol,sdk}`, `tickloop-ui` and the `crossterm` patch, by Git from `branch = "main"`; `Cargo.lock` holds the exact commit. Track the Tick protocol, never Tickloop's app versions. Tickloop must never depend on this repository.
- Changes to the domain-agnostic Tick crates go to tickloopai/tickloop first; financial Tick and provider changes belong in `market/` here. For cross-repo work use `.cargo/config.toml.example`; see "Building against a local Tickloop" in CONTRIBUTING.md.
- Packages depend on `market/` crates, never on each other; `market/provider` depends on no other workspace crate.

## Read before changing

| Change | Read first |
| --- | --- |
| A package Skill | `tests/package_skill.rs` |
| Longbridge login or token renewal | `vendor/longbridge-oauth/README.md`, `packages/longbridge/src/auth/` |
| Longbridge SDK settings or environment | `vendor/dotenv/README.md` |
| Code adapted from third parties | [THIRD_PARTY_NOTICES.md](./THIRD_PARTY_NOTICES.md) |
| `tick-package.json` or authentication declarations | "Package manifest" in CONTRIBUTING.md |
| Releases, targets, `marketplace.json` | "Releasing" in CONTRIBUTING.md |
| The Tick protocol or SDK | `tick/protocol/SPECIFICATION.md` in tickloopai/tickloop |

## Verification

- Check commands, test layout and PR rules are only in [CONTRIBUTING.md](./CONTRIBUTING.md); a change is done when `make check` passes.
- A package's `tick-package.json` `version` must equal its Cargo version. Do not bump versions in normal changes, and never hand-edit `marketplace.json` or the README's "Packages" section; the release does both.
- When reporting verification, list local automated checks separately from runs against a real Tickloop host or a real broker account.

## Agent skills

These files are maintainer-local (outside Git); skip this section when they are absent. Public collaboration uses GitHub Issues and Pull Requests.

### Issue tracker

Specs and tickets are local Markdown under `.scratch/<feature>/`, never committed. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default roles (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`) as `Status:` lines in ticket files. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: CONTEXT.md at the root, ADRs in `docs/adr/`. See `docs/agents/domain.md`.
