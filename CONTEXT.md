# Tickloop Ticks

Terms specific to this repository. Shared terms (Tick, Tick Instance, Tick Package, Tick Marketplace, Extension Tick and others) are defined in Tickloop's [CONTEXT.md](https://github.com/longbridge/tickloop/blob/main/CONTEXT.md), which wins on any overlap. Behavior lives in [AGENTS.md](./AGENTS.md) and the source.

## Language

**Financial Tick**:
One of the nine Ticks in `market/views` (`TickKind`: quote, watchlist, kline, positions, news, earnings, options, reader, community), run by a package program.
_Avoid_: calling a Financial Tick a package, or a package a Tick

**Browser Tick**:
The Tick `tick-browser browser` runs (`packages/browser`), one for each Tick Instance that opens it: it serves the tools of the Tickloop browser extension, which it does not define itself, and shows the tabs of its Browser Session. Like every Tick here it is an Extension Tick in Tickloop's terms, but it is not a Financial Tick and has no Provider.
_Avoid_: browser extension, or extension alone, for the Tick

**Native Host**:
`tick-browser` as Chrome starts it for the extension (`chrome-extension://<id>/` as its first argument), the native messaging host `ai.tickloop.browser`. It serves the local socket the Browser Ticks connect to, one browser at a time, and routes between the extension and each Browser Session.
_Avoid_: host, alone, which in Tickloop means the program that hosts a Tick Instance

**Browser Session**:
One Browser Tick's share of the browser, named by its Tick Instance identity: its own tab group, state, pending approvals and calls in the extension, opened when the Tick connects to the Native Host and closed when it goes. Several are open at once in one browser.
_Avoid_: Chat, or session alone where Tickloop's own sessions could be meant

**Relay Protocol**:
The messages between the extension and the Native Host (`hello`, `update`, `result`, `welcome`, `refused`, `session_opened`, `session_closed`, `call`, `stop`) and between the Native Host and each Browser Tick (`open`, `hello`, `update`, `result`, `refused`, `call`, `stop`), with their own version, specified in `packages/browser/PROTOCOL.md`.
_Avoid_: Tick protocol, which is the one between a Tick and its host

**Extension Catalog**:
The tool list the extension sends in `hello`, which every Browser Tick offers as its own catalog, with the same revision, while it is connected; before that a Tick offers only `status`.
_Avoid_: hard-coding extension tool names in the Tick

**Tick Command**:
A package program's subcommand that runs one Financial Tick; what every Tick Command shares lives in `market/command`.
_Avoid_: tool, for a subcommand

**Provider**:
An implementation of the provider traits in `market/provider` (`QuoteApi`, `TradeApi`, `NewsApi`, `OptionsApi`, `CommunityApi`) through which a Financial Tick reads its data.
_Avoid_: adapter, which in Tickloop means a Tick Adapter

**Data Source**:
The broker or service a hosted Tick connects to after answering its host, such as Longbridge or Futu OpenD (`DataSource` in `market/command`).
_Avoid_: provider, for the external service itself

**Package Skill**:
The Skill a Tick package ships in `skills/`, telling the Agent how to open its program; it is product content.
_Avoid_: system Skill

**Mock Provider**:
A test-only Provider from `market/test-support`, also run by the `tick-mock` program; no released program may link it.
_Avoid_: demo mode
