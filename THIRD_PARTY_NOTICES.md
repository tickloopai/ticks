# Third-party notices

This file records the source and applicable license of third-party code adapted into the Tick packages in this repository, and into the [Tickloop](https://github.com/tickloopai/tickloop) crates their programs link. Every package archive carries this file and the license copies it names. The Tickloop repository keeps its own notices for its crates. Other dependencies retain their own licenses.

## Longbridge Terminal pure functions

- Upstream: [Longbridge Terminal](https://github.com/longbridge/longbridge-terminal).
- Reference revision: `b834538` (2026-09-07).
- Upstream sources: `src/utils/text.rs`, `src/utils/number.rs`, `src/openapi/rate_limiter.rs`, `src/tui/nav.rs`, `src/cli/output.rs`.
- Adapted source in this repository: `packages/longbridge/src/rate_limit.rs`.
- Adapted source in the linked Tickloop crates: `ui/src/format.rs` (width, padding, truncation, number abbreviation), `tick/provider/src/api.rs` (symbol normalization), and `tick/market/src/output.rs` (header keys).
- Applicable license: Apache License, Version 2.0. An unmodified copy is included in [licenses/Longbridge-Terminal-Apache-2.0.txt](./licenses/Longbridge-Terminal-Apache-2.0.txt).

The adaptation adds configurable ambiguous width, grapheme-aware truncation, and Chinese units, and removes global state from the limiter. Longbridge Terminal is a source reference, not a build dependency.

## Longbridge OpenAPI OAuth token refresher patch

- Upstream: [Longbridge OpenAPI](https://github.com/longbridge/openapi).
- Reference revision: `59294104c48a3812049158ee52124f32e5046008` (`5929410`).
- Upstream source: `rust/crates/oauth`.
- Vendored source: `vendor/longbridge-oauth`, patched through the root Cargo manifest.
- Changes: a `TokenRefresher` hook through which the application renews tokens in place of the client's own refresh; standalone Cargo dependency declarations and unit-test constructor updates. Details are in [the patch README](./vendor/longbridge-oauth/README.md).
- Applicable licenses: MIT OR Apache License, Version 2.0. Unmodified copies are included in [LICENSE-MIT](./vendor/longbridge-oauth/LICENSE-MIT) and [LICENSE-APACHE](./vendor/longbridge-oauth/LICENSE-APACHE).

## Crossterm input patch

- Upstream: [crossterm](https://github.com/crossterm-rs/crossterm) 0.29.0, as published on crates.io.
- Patched source: `vendor/crossterm` in the Tickloop repository, used here through the root Cargo manifest's `[patch.crates-io]`.
- Changes: one shared Unix input parser that keeps a second Escape from swallowing the terminal sequence it starts; details are in that directory's `UPSTREAM.md`.
- Applicable license: MIT. The upstream copyright notice and license are included in [licenses/Crossterm-MIT.txt](./licenses/Crossterm-MIT.txt).

## CLI Candlestick Chart glyph rules

- Upstream: [cli-candlestick-chart](https://github.com/Julien-R44/cli-candlestick-chart).
- Reference copy: Longbridge Terminal revision `b834538`, `crates/cli-candlestick-chart/src/chart_renderer.rs`.
- Adapted source in the linked Tickloop crates: `ui/src/charts.rs`, `candle_glyph`; quarter-cell wick/body boundary rules extracted as a pure function. Braille crossing attribution in the same file also follows `src/ai/chart.rs` in the Apache-2.0 Longbridge Terminal reference above.
- Applicable license for candle glyph rules: MIT. The copyright notice and license are included in [licenses/Cli-Candlestick-Chart-MIT.txt](./licenses/Cli-Candlestick-Chart-MIT.txt).
