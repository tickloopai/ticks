# Tickloop Ticks

The official Tick marketplace for [Tickloop](https://tickloop.ai). It lists the Tick packages you can install into Tickloop and hosts their prebuilt programs as release assets.

```sh
tickloop tick list --available
tickloop tick install longbridge
tickloop tick uninstall longbridge
```

Installing downloads the package for your platform, checks its SHA-256 against `marketplace.json`, and places it under `~/.tickloop/ticks/`. No Rust toolchain is needed. Installing a package never opens or subscribes to a Tick and grants no market, account or trading permission; a data source's login stays with the package's own command, such as `tick-longbridge auth`.

Prebuilt packages are currently provided for macOS on Apple Silicon (`aarch64-apple-darwin`) only.

## Layout

- `marketplace.json`: every package with its version, description and per-platform download URL and SHA-256.
- Releases: one release per package version, tagged `<package>-v<version>`, carrying `tick-<package>-<version>-<target>.tar.gz`.
