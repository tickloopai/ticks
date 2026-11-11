# Tickloop Ticks

The official Tick marketplace for [Tickloop](https://tickloop.ai). It lists the Tick packages you can install into Tickloop, hosts their prebuilt programs, and holds the code they are built from.

A Tick package adds live views to Tickloop, such as stock quotes, charts, positions, news or option chains, and teaches the Agent how to open them.

## Install a package

```sh
tickloop tick list --available    # see what you can install
tickloop tick install longbridge  # install a package
tickloop tick login longbridge    # log in to its data source
```

Installing needs no Rust toolchain: Tickloop downloads the prebuilt program for your computer and checks it before installing. Packages run on macOS (Apple Silicon and Intel) and Linux (ARM64 and x86_64, any distribution).

Installing a package opens nothing and grants no market, account or trading access. You log in to a package's data source with `/ticks` in the Agent, or with `tickloop tick login <package>` in a terminal; Tickloop keeps the login, not the package. `futu` needs no login in Tickloop: you log in inside Futu OpenD, which must run on the same computer.

## Get started

After installing, ask the Agent in plain words, for example "show me Tesla's K-line" or "open my positions". The Agent opens the right Tick for you.

## The browser package

`browser` lets the Agent read and operate web pages in your own Chrome, Chromium or Edge, signed in as you, through the Tickloop browser extension (until it is in the Chrome Web Store, load its development build unpacked in `chrome://extensions`, in Developer mode); it only uses the tabs in the Agent's own tab group, one group for each session that opens it, so several sessions use the browser at once. It needs no login, and installing it runs nothing. When the Agent first opens the Browser Tick, the Tick registers itself as the native messaging host `ai.tickloop.browser` with each of these browsers you have, by writing `ai.tickloop.browser.json` into its `NativeMessagingHosts` directory:

| Browser | macOS (under `~/Library/Application Support/`) | Linux (under `~/.config/`) |
| --- | --- | --- |
| Google Chrome | `Google/Chrome/NativeMessagingHosts/` | `google-chrome/NativeMessagingHosts/` |
| Chromium | `Chromium/NativeMessagingHosts/` | `chromium/NativeMessagingHosts/` |
| Microsoft Edge | `Microsoft Edge/NativeMessagingHosts/` | `microsoft-edge/NativeMessagingHosts/` |

```json
{
  "name": "ai.tickloop.browser",
  "description": "Tickloop Browser Tick relay",
  "path": "/absolute/path/to/the/installed/tick-browser",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/"]
}
```

Only the Tickloop extension may start it. It is rewritten when the package moves or when another Tickloop installation's Browser Tick opened last, and the Tick's card says when it wrote or replaced one; you can delete it to undo the registration. `tick-browser status` shows where it is registered and whether a browser is serving Tickloop. Of two browsers with the extension, such as Chrome and Edge, the one opened first serves; the other takes over when the first closes. How the Tick, the host and the extension talk is in [packages/browser/PROTOCOL.md](packages/browser/PROTOCOL.md).

## Packages

Install one with `tickloop tick install <package>`.

| Package | Version | Description |
| --- | --- | --- |
| [`futu`](packages/futu) | 0.2.11 | Futu OpenD option chains, option pricing and implied volatility |
| [`longbridge`](packages/longbridge) | 0.2.11 | Longbridge quotes, watchlist, kline, positions, news, earnings and community posts |
| [`tickloop`](packages/tickloop) | 0.1.7 | Official Tickloop Ticks: the Reader for original financial documents from URLs or local files |

## Common commands

```sh
tickloop tick list                  # installed packages and their login state
tickloop tick list --available      # packages in the marketplace
tickloop tick install <package>     # install or update a package
tickloop tick login <package>       # log in to the package's data source
tickloop tick logout <package>      # delete the saved login
tickloop tick uninstall <package>   # remove a package (its saved login stays)
```

## Contributing

To build from source, run the tests or release packages, see [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE). Third-party code and its licenses are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
