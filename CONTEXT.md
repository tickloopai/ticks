# Tickloop Ticks

Terms specific to this repository. Shared terms (Tick, Tick Instance, Tick Package, Tick Marketplace, Extension Tick and others) are defined in Tickloop's [CONTEXT.md](https://github.com/tickloopai/tickloop/blob/main/CONTEXT.md), which wins on any overlap. Behavior lives in [AGENTS.md](./AGENTS.md) and the source.

## Language

**Financial Tick**:
One of the nine Ticks in `market/views` (`TickKind`: quote, watchlist, kline, positions, news, earnings, options, reader, community), run by a package program.
_Avoid_: calling a Financial Tick a package, or a package a Tick

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
