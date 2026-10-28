use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::Arc;
use tick_market::{
    args::{
        CommunityArgs, EarningsArgs, EventChartArgs, KlineArgs, NewsArgs, OnceArgs, OptionsArgs,
        QuoteArgs, WatchlistArgs,
    },
    commands,
    config::Settings,
    event_chart::{EventSession, MINIMUM_PANEL_SIZE as EVENT_CHART_MINIMUM},
    metadata::{MinimumPanelSize, TickKind},
};
use tick_provider::api::{CandleAdjust, CandlePeriod};
use tick_sdk::stream::TickStream;
use tick_test_support::{MockApi, MockCommunity, MockOptions, mock_library};

/// The narrowest terminal every Tick is laid out for in full: a laptop at 80 columns.
const LAPTOP: (u16, u16) = (80, 24);
/// The narrowest Panel every Tick is laid out for at all: Tickloop's narrowest Monitor column,
/// 32 columns less its separator.
const MONITOR_COLUMNS: u16 = 30;
/// The width of a Monitor card the user has not narrowed.
const MONITOR_CARD_COLUMNS: u16 = 40;
/// What the frame's bottom edge carries: the help and quit keys, or an open editor's keys.
const STATUS_KEYS: [&str; 2] = [" ? ", "Ctrl+C Cancel"];
const HELP_SCROLL_HINT: &str = "j/k more";
/// Fields a card of the Tick's minimum rows leaves for a taller one: the event chart's window
/// and time-zone lines rank below its release, baseline and candle, and the Reader shows its
/// document's start, with later prose a scroll away.
const BELOW_THE_FOLD: [&str; 3] = [
    "Auto event window ·",
    "Times in America/New_York",
    "Revenue was $12.8",
];
/// Narrower than any Tick's minimum, so the Panel scrolls over the minimum layout.
const BELOW_MINIMUM: [u16; 2] = [24, 20];
/// The width the Ticks were laid out for before 80 columns became the target.
const WIDE: (u16, u16) = (120, 24);
const ROWS: u16 = 24;
const SCROLL_HINT: &str = "scroll";

struct Scenario {
    kind: TickKind,
    event_chart: bool,
    /// The source the mock data is named after; the mock's own name when `None`.
    source: Option<&'static str>,
    keys: Vec<KeyCode>,
    shows: &'static [&'static str],
}

fn scenario(kind: TickKind, keys: &[KeyCode], shows: &'static [&'static str]) -> Scenario {
    Scenario {
        kind,
        event_chart: false,
        source: None,
        keys: keys.to_vec(),
        shows,
    }
}

impl Scenario {
    fn minimum(&self) -> MinimumPanelSize {
        if self.event_chart {
            EVENT_CHART_MINIMUM
        } else {
            self.kind.minimum_panel_size()
        }
    }

    /// The rows a host grows the Tick's card to when it has room.
    fn maximum_rows(&self) -> Option<u16> {
        self.kind.maximum_rows()
    }

    /// Whether the Tick shows cards, the only borders it draws.
    fn has_cards(&self) -> bool {
        matches!(self.kind, TickKind::Quote | TickKind::Watchlist)
    }

    fn label(&self, columns: u16, rows: u16) -> String {
        let chart = if self.event_chart { " event chart" } else { "" };
        format!(
            "{}{chart} {:?} at {columns}x{rows}",
            self.kind.name(),
            self.keys
        )
    }
}

/// Every Tick and mode with the fields that must stay whole: the key columns and values, the
/// header lines and the footer keys.
fn every_tick() -> Vec<Scenario> {
    vec![
        scenario(
            TickKind::Quote,
            &[],
            &[
                "700.HK",
                "Demo · 腾讯控股",
                "480.20",
                "-11.80",
                "-2.40%",
                "H 4",
                "? Help  q Quit",
            ],
        ),
        scenario(
            TickKind::Quote,
            &[KeyCode::Char('?')],
            &["Keys", "PageUp / PageDown  Page", "g / G  First / last"],
        ),
        scenario(
            TickKind::Watchlist,
            &[],
            &[
                "Demo · US",
                "▶ TSLA.US",
                "Demo · Tesla",
                "248.50",
                "+8.50",
                "+3.54%",
                "H 2",
            ],
        ),
        scenario(
            TickKind::Kline,
            &[],
            &[
                "700.HK · day · none",
                "2026-",
                "O 4",
                "H 4",
                "L 4",
                "C 4",
                "V 2",
                "Change -",
                "1–8 Period · m MA off · Live + 30s sync",
                "▲",
            ],
        ),
        Scenario {
            event_chart: true,
            ..scenario(
                TickKind::Kline,
                &[],
                &[
                    "DEMO.US · 2026-08-07",
                    "Release: 2026-08-07T16:05:00-04:00",
                    "Baseline 2026-08-07 regular close",
                    "America/New_York",
                    "v View · r / [ ] Range · e Event · Enter Detail",
                    "? Help  q Quit",
                ],
            )
        },
        scenario(
            TickKind::Positions,
            &[],
            &[
                "模拟盘账户",
                "Qty",
                "P/L %",
                "TSLA.US",
                "120",
                "248.50",
                "2.98万",
                "1920",
                "+6.88%",
                "Total HKD",
            ],
        ),
        scenario(
            TickKind::News,
            &[],
            &[
                "Market",
                "09:30",
                "示例资讯",
                "Demo · Market recap",
                "Enter Read",
            ],
        ),
        scenario(
            TickKind::News,
            &[KeyCode::Enter],
            &["示例资讯正文", "Backspace Back · j/k Scroll"],
        ),
        scenario(
            TickKind::Community,
            &[],
            &[
                "Demo · 示例用户",
                "09-10",
                "Article",
                "$Demo · Tesla",
                "Enter Read",
            ],
        ),
        scenario(
            TickKind::Community,
            &[KeyCode::Enter],
            &[
                "今天放量突破",
                "09:30 · topic 1003",
                "Backspace Back · j/k Scroll",
            ],
        ),
        scenario(
            TickKind::Earnings,
            &[],
            &[
                "TSLA.US · Compare periods",
                "EPS Act",
                "EPS YoY",
                "Rev Act",
                "Revenue YoY",
                "Q2 2026",
                "273.00亿",
                "11.43%",
                "· Enter detail",
            ],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Enter],
            &[
                "TSLA.US · FY2026-Q2",
                "Metric",
                "Estimate",
                "27300000000",
                "26400000000",
                "· estimates not archived",
            ],
        ),
        // The longest source a live read names, with its time read to the nanosecond: the
        // short mock name once hid that the caveat overflowed the minimum width.
        Scenario {
            source: Some("longbridge"),
            ..scenario(
                TickKind::Earnings,
                &[KeyCode::Enter],
                &[
                    "TSLA.US · FY2026-Q2",
                    "Longbridge · ",
                    " UTC · estimates not archived",
                ],
            )
        },
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('r')],
            &[
                "Market reactions",
                "EPS Surp",
                "Move",
                "2026-07-21",
                "+0.79%",
                "Counts",
            ],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('f')],
            &["Period originals", "Source URL", "body not verified"],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('u')],
            &[
                "Upcoming earnings · preparing",
                "Available value",
                "awaiting actuals",
            ],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('s')],
            &["Symbol:", "Enter apply · Ctrl+C cancel"],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('?')],
            &[
                "Enter  Period detail / selected metric original · c",
                "r  Market reactions · f  Period originals · R  Refresh",
            ],
        ),
        scenario(
            TickKind::Options,
            &[],
            &[
                "AAPL.US · 2026-",
                "Spot 100.00",
                "through",
                "Observed",
                "Call bid/ask",
                "Put bid/ask",
                "C IV%",
                "P IV%",
                "3.00/3.20",
                "[ ] expiry",
                "No last-price substitution",
            ],
        ),
        scenario(
            TickKind::Reader,
            &[],
            &["财报原文", "Quarterly results", "Revenue was $12.8", "1/4"],
        ),
    ]
}

/// Every Tick and mode with the fields a Panel at the minimum width keeps whole; the rest give
/// way to them.
fn every_tick_at_the_minimum_width() -> Vec<Scenario> {
    vec![
        scenario(
            TickKind::Quote,
            &[],
            &["700.HK", "480.20", "-2.40%", "-11.80", "H 4", "?  q Quit"],
        ),
        scenario(
            TickKind::Quote,
            &[KeyCode::Char('?')],
            &["Keys · ?", "PageUp / PageDown  Page", "g / G  First / last"],
        ),
        scenario(
            TickKind::Watchlist,
            &[],
            &["Demo · US", "▶ TSLA.US", "248.50", "+3.54%", "+8.50"],
        ),
        scenario(
            TickKind::Kline,
            &[],
            &[
                "700.HK · day · none",
                "2026-",
                "O 4",
                "H 4",
                "L 4",
                "C 4",
                "V 2",
                "Change -",
                "1–8 Period · m MA off · Live",
                "▲",
            ],
        ),
        Scenario {
            event_chart: true,
            ..scenario(
                TickKind::Kline,
                &[],
                &[
                    "Event line / 1m",
                    "Auto event window ·",
                    "Release 2026-08-07 16:05",
                    "Baseline 243.4 · 2026-08-07",
                    "Times in America/New_York",
                    "v View · r Range · e Event",
                ],
            )
        },
        scenario(
            TickKind::Positions,
            &[],
            &[
                "模拟盘账户",
                "P/L",
                "P/L %",
                "TSLA.US",
                "1920",
                "+6.88%",
                "Total HKD",
            ],
        ),
        scenario(
            TickKind::News,
            &[],
            &[
                "Market",
                "Time",
                "09:30",
                "Demo · Market recap",
                "Enter Read",
            ],
        ),
        scenario(
            TickKind::News,
            &[KeyCode::Enter],
            &["示例资讯正文", "Backspace Back · j/k Scroll"],
        ),
        scenario(
            TickKind::Community,
            &[],
            &[
                "Demo · 示例用户",
                "Date",
                "09-10",
                "$Demo · Tesla",
                "Enter Read",
            ],
        ),
        scenario(
            TickKind::Community,
            &[KeyCode::Enter],
            &["今天", "topic", "Backspace Back · j/k Scroll"],
        ),
        scenario(
            TickKind::Earnings,
            &[],
            &["Compare periods", "UTC", "EPS Act", "EPS YoY", "Q2 2026"],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Enter],
            &[
                "TSLA.US · FY2026-Q2",
                "estimates not archived",
                "Metric",
                "Actual",
                "27300000000",
            ],
        ),
        // The longest source a live read names gives way to the caveat.
        Scenario {
            source: Some("longbridge"),
            ..scenario(
                TickKind::Earnings,
                &[KeyCode::Enter],
                &["TSLA.US · FY2026-Q2", "estimates not archived"],
            )
        },
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('r')],
            &["Market reactions", "Day", "Move", "+0.79%", "Counts"],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('f')],
            &["Period originals", "body not verified", "Original"],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('u')],
            &[
                "Upcoming earnings",
                "awaiting actuals",
                "Upcoming",
                "Not identified",
            ],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('s')],
            &["Symbol:", "Enter apply", "Ctrl+C Cancel"],
        ),
        scenario(
            TickKind::Earnings,
            &[KeyCode::Char('?')],
            &["Keys · ?", "Enter  Period detail /"],
        ),
        scenario(
            TickKind::Options,
            &[],
            &[
                "AAPL.US · 2026-10-29",
                "Spot 100.00 · ATM ±6.00%",
                "Observed",
                "Strike",
                "C mid",
                "P mid",
                "80.0",
                "3.10",
                "2.90",
                "[ ] expiry",
                "No last-price substitution;",
            ],
        ),
        scenario(
            TickKind::Reader,
            &[],
            &[
                "财报原文",
                "DEMO · FY2026 Q2 Earnings",
                "Quarterly results",
                "Revenue was $12.8",
                "1/4",
            ],
        ),
    ]
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// `events`, then q, each a while after the one before.
fn input(events: Vec<Event>) -> impl futures_util::Stream<Item = std::io::Result<Event>> + Unpin {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for event in events.into_iter().chain([press(KeyCode::Char('q'))]) {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            sender.send(Ok(event)).unwrap();
        }
    });
    tokio_stream::wrappers::UnboundedReceiverStream::new(receiver)
}

/// Each cell's symbol, row by row; the cell after a wide glyph holds a blank.
fn cells(terminal: &Terminal<TestBackend>) -> Vec<Vec<String>> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol().to_owned())
                .collect()
        })
        .collect()
}

/// The screen as text, each wide glyph once.
fn text(cells: &[Vec<String>]) -> String {
    cells
        .iter()
        .map(|row| {
            let mut line = String::new();
            let mut covered = 0;
            for symbol in row {
                if covered > 0 {
                    covered -= 1;
                    continue;
                }
                covered = tickloop_ui::format::width(symbol, Default::default()).saturating_sub(1);
                line.push_str(symbol);
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The screen with every digit masked: the mock moves prices at random, while layout,
/// selection and mode stay put.
fn layout_of(cells: &[Vec<String>]) -> String {
    text(cells)
        .chars()
        .map(|character| {
            if character.is_ascii_digit() {
                '#'
            } else {
                character
            }
        })
        .collect()
}

async fn render(scenario: &Scenario, columns: u16, rows: u16) -> Vec<Vec<String>> {
    render_keys(scenario, columns, rows, scenario.keys.clone()).await
}

async fn render_keys(
    scenario: &Scenario,
    columns: u16,
    rows: u16,
    codes: Vec<KeyCode>,
) -> Vec<Vec<String>> {
    render_events(
        scenario,
        columns,
        rows,
        codes.into_iter().map(press).collect(),
    )
    .await
}

async fn render_events(
    scenario: &Scenario,
    columns: u16,
    rows: u16,
    events: Vec<Event>,
) -> Vec<Vec<String>> {
    let api = match scenario.source {
        Some(source) => MockApi::named(source),
        None => MockApi::new(),
    }
    .unwrap();
    let settings = Settings::default();
    let mut terminal = Terminal::new(TestBackend::new(columns, rows)).unwrap();
    let mut updates = TickStream::in_memory(scenario.kind);
    let input = input(events);
    match scenario.kind {
        TickKind::Quote => commands::quote::run_live(
            &QuoteArgs {
                symbols: ["700.HK", "AAPL.US", "TSLA.US", "NVDA.US", "9988.HK"]
                    .map(String::from)
                    .to_vec(),
                output: OnceArgs::default(),
            },
            &api,
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::Watchlist => commands::watchlist::run_live(
            &WatchlistArgs::default(),
            &api,
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::Kline => {
            let (symbol, event) = if scenario.event_chart {
                (
                    "DEMO.US",
                    EventChartArgs {
                        event_date: Some("2026-08-07".into()),
                        event_session: EventSession::AfterHours,
                        published_at: Some("2026-08-07T16:05:00-04:00".into()),
                        ..EventChartArgs::default()
                    },
                )
            } else {
                ("700.HK", EventChartArgs::default())
            };
            commands::kline::run_live(
                &KlineArgs {
                    symbol: symbol.into(),
                    period: CandlePeriod::Day,
                    count: None,
                    adjust: CandleAdjust::None,
                    event,
                    output: OnceArgs::default(),
                },
                &api,
                &settings,
                &mut terminal,
                input,
                &mut updates,
            )
            .await
            .unwrap()
        }
        TickKind::Positions => commands::positions::run_live(
            &OnceArgs::default(),
            &api,
            &api,
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::News => commands::news::run_live(
            &NewsArgs {
                symbol: None,
                limit: 20,
                output: OnceArgs::default(),
            },
            &api,
            &api,
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::Earnings => commands::earnings::run_live(
            &EarningsArgs {
                symbol: "TSLA.US".into(),
                count: None,
                output: OnceArgs::default(),
            },
            &api,
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::Options => commands::options::run_live(
            &OptionsArgs {
                check: false,
                symbol: "AAPL.US".into(),
                expiry: None,
                snapshot_dir: None,
                strikes: 21,
                output: OnceArgs::default(),
            },
            Arc::new(MockOptions),
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::Reader => commands::reader::run_live(
            mock_library(),
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
        TickKind::Community => commands::community::run_live(
            &CommunityArgs {
                member_id: 91,
                output: OnceArgs::default(),
            },
            Arc::new(MockCommunity),
            &settings,
            &mut terminal,
            input,
            &mut updates,
        )
        .await
        .unwrap(),
    }
    cells(&terminal)
}

async fn assert_whole(scenarios: Vec<Scenario>, size: impl Fn(&Scenario) -> (u16, u16)) {
    for scenario in scenarios {
        let (columns, rows) = size(&scenario);
        let frame = text(&render(&scenario, columns, rows).await);
        let label = scenario.label(columns, rows);
        assert!(
            !frame.contains(SCROLL_HINT),
            "{label} scrolls although the Panel holds its layout:\n{frame}"
        );
        for field in scenario.shows {
            assert!(frame.contains(field), "{label} hides {field:?}:\n{frame}");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn every_tick_shows_its_key_fields_whole_at_its_own_minimum_width() {
    assert_whole(every_tick_at_the_minimum_width(), |scenario| {
        (scenario.minimum().columns, ROWS)
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn every_tick_holds_its_layout_at_its_full_minimum_size() {
    for scenario in every_tick_at_the_minimum_width() {
        let minimum = scenario.minimum();
        let frame = text(&render(&scenario, minimum.columns, minimum.rows).await);
        let label = scenario.label(minimum.columns, minimum.rows);
        assert!(!frame.contains(SCROLL_HINT), "{label} scrolls:\n{frame}");
    }
}

/// A Monitor card is 40 columns unless the user narrows it to 30, at least the Tick's minimum
/// rows tall, and up to its maximum when the Monitor has room. The layout holds without scrolling the canvas and
/// keeps the status row on the bottom line; what a short card leaves below the fold, such as
/// an article's body or the positions' totals, the Tick's own keys scroll to.
#[tokio::test(start_paused = true)]
async fn every_tick_fits_the_monitor_card_sizes_inside_a_whole_frame() {
    let mut failures = Vec::new();
    for scenario in every_tick_at_the_minimum_width() {
        let minimum = scenario.minimum().rows;
        let mut sizes = vec![(MONITOR_COLUMNS, minimum), (MONITOR_CARD_COLUMNS, minimum)];
        sizes.extend(
            scenario
                .maximum_rows()
                .map(|maximum| (MONITOR_CARD_COLUMNS, maximum)),
        );
        for (columns, rows) in sizes {
            let cells = render(&scenario, columns, rows).await;
            let frame = text(&cells);
            let label = scenario.label(columns, rows);
            if frame.contains(SCROLL_HINT) {
                failures.push(format!("{label} scrolls:\n{frame}"));
            }
            // The fields are those of the narrowest layout, which a wider card spells out more
            // fully; a help overlay too short for every key scrolls them.
            if columns == MONITOR_COLUMNS && !frame.contains(HELP_SCROLL_HINT) {
                for field in scenario.shows {
                    if !frame.contains(field) && !BELOW_THE_FOLD.contains(field) {
                        failures.push(format!("{label} hides {field:?}:\n{frame}"));
                    }
                }
            }
            let problem = if scenario.has_cards() {
                cards_problem(&cells)
            } else {
                frame_problem(&cells)
            };
            if let Some(problem) = problem {
                failures.push(format!("{label} {problem}:\n{frame}"));
            }
            // The keys are on the frame's bottom edge, or the last card's for a view of cards,
            // which a help overlay covers.
            let covered = scenario.has_cards() && scenario.keys.contains(&KeyCode::Char('?'));
            let bottom_edge = cells
                .iter()
                .rev()
                .map(|line| line.concat())
                .find(|line| line.contains('└'))
                .unwrap_or_default();
            if !covered && !STATUS_KEYS.iter().any(|keys| bottom_edge.contains(keys)) {
                failures.push(format!("{label} has no keys on its bottom edge:\n{frame}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test(start_paused = true)]
async fn a_help_overlay_too_short_for_every_key_scrolls_them() {
    let rows = TickKind::Quote.minimum_panel_size().rows;
    let help = |keys: &[KeyCode]| scenario(TickKind::Quote, keys, &[]);
    let top = text(&render(&help(&[KeyCode::Char('?')]), MONITOR_COLUMNS, rows).await);
    assert!(top.contains(HELP_SCROLL_HINT), "{top}");
    assert!(!top.contains("g / G  First / last"), "{top}");
    let end = help(&[KeyCode::Char('?'), KeyCode::Char('G')]);
    let bottom = text(&render(&end, MONITOR_COLUMNS, rows).await);
    assert!(bottom.contains("g / G  First / last"), "{bottom}");
    let back = help(&[KeyCode::Char('?'), KeyCode::Char('G'), KeyCode::Char('g')]);
    assert_eq!(text(&render(&back, MONITOR_COLUMNS, rows).await), top);
}

#[tokio::test(start_paused = true)]
async fn the_shortest_narrow_event_chart_keeps_its_popups_way_out_and_the_reader_its_prose() {
    let event = |keys: &[KeyCode]| Scenario {
        event_chart: true,
        ..scenario(TickKind::Kline, keys, &[])
    };
    let minimum = EVENT_CHART_MINIMUM;
    let detail = text(&render(&event(&[KeyCode::Enter]), minimum.columns, minimum.rows).await);
    assert!(detail.contains("Backspace Back"), "{detail}");
    let editor = text(&render(&event(&[KeyCode::Char('e')]), minimum.columns, 24).await);
    // The editor's keys wrap whole words inside the frame.
    assert!(editor.contains("Ctrl+C Cancel"), "{editor}");

    let reader = scenario(TickKind::Reader, &[], &[]);
    let minimum = reader.minimum();
    let frame = text(&render(&reader, minimum.columns, minimum.rows).await);
    assert!(frame.contains("Quarterly results"), "{frame}");
}

#[tokio::test(start_paused = true)]
async fn every_tick_shows_its_key_fields_whole_on_an_80_column_laptop() {
    assert_whole(every_tick(), |_| LAPTOP).await;
}

#[test]
fn every_minimum_width_fits_the_narrowest_monitor_column() {
    let minimums = TickKind::ALL
        .map(|kind| (kind.name(), kind.minimum_panel_size()))
        .into_iter()
        .chain([("event chart", EVENT_CHART_MINIMUM)]);
    for (name, minimum) in minimums {
        assert!(minimum.columns <= MONITOR_COLUMNS, "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_laptop_width_keeps_the_options_open_interest_columns() {
    assert_whole(
        vec![scenario(TickKind::Options, &[], &["C OI", "P OI"])],
        |_| LAPTOP,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn wider_panels_keep_the_columns_their_layouts_had() {
    assert_whole(
        vec![
            scenario(TickKind::Positions, &[], &["Available"]),
            scenario(TickKind::Earnings, &[], &["Date", "· [ ] periods"]),
            scenario(TickKind::Earnings, &[KeyCode::Enter], &["QoQ", "Unit"]),
            scenario(
                TickKind::Earnings,
                &[KeyCode::Char('r')],
                &["EPS Est", "EPS Act", "Rev Surp", "+20d"],
            ),
            scenario(TickKind::Options, &[], &["C volume", "P volume"]),
        ],
        |_| WIDE,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_lone_quote_card_widens_to_show_every_session_figure() {
    let api = MockApi::new().unwrap();
    let mut terminal = Terminal::new(TestBackend::new(WIDE.0, WIDE.1)).unwrap();
    commands::quote::run_live(
        &QuoteArgs {
            symbols: vec!["700.HK".into()],
            output: OnceArgs::default(),
        },
        &api,
        &Settings::default(),
        &mut terminal,
        input(Vec::new()),
        &mut TickStream::in_memory(TickKind::Quote),
    )
    .await
    .unwrap();
    let frame = text(&cells(&terminal));
    for figure in ["H 4", "L 4", "Open 4", "Prev 4", "Vol ", "Turnover "] {
        assert!(frame.contains(figure), "missing {figure:?}:\n{frame}");
    }
}

#[tokio::test(start_paused = true)]
async fn quote_cards_move_by_row_down_and_by_card_across() {
    let quote = scenario(TickKind::Quote, &[], &[]);
    // Two cards across and one row of them at a time.
    let (columns, rows) = (62, 6);
    let start = text(&render_keys(&quote, columns, rows, Vec::new()).await);
    assert!(start.contains("1–2/5"), "{start}");
    assert!(
        start.contains("700.HK") && start.contains("AAPL.US"),
        "{start}"
    );

    let across = text(&render_keys(&quote, columns, rows, vec![KeyCode::Char('l')]).await);
    assert!(across.contains("1–2/5"), "{across}");

    let down = text(&render_keys(&quote, columns, rows, vec![KeyCode::Char('j')]).await);
    assert!(down.contains("3–4/5"), "{down}");
    assert!(
        down.contains("TSLA.US") && down.contains("NVDA.US"),
        "{down}"
    );

    let last = text(&render_keys(&quote, columns, rows, vec![KeyCode::Char('j'); 3]).await);
    assert!(last.contains("5–5/5"), "{last}");
    assert!(last.contains("9988.HK"), "{last}");
}

/// What is wrong with the Tick's frame around `cells`: an edge or corner missing, so content
/// shows outside it, a title cut short, or a mark or row of its own for the data state, which
/// the Monitor's row shows.
fn frame_problem(cells: &[Vec<String>]) -> Option<String> {
    let (last_row, last_column) = (cells.len() - 1, cells[0].len() - 1);
    let corners = [
        (0, 0, "┌"),
        (0, last_column, "┐"),
        (last_row, 0, "└"),
        (last_row, last_column, "┘"),
    ];
    if let Some((y, x, corner)) = corners
        .into_iter()
        .find(|(y, x, corner)| cells[*y][*x] != *corner)
    {
        return Some(format!("has no {corner} at {x},{y}"));
    }
    if let Some(y) = (1..last_row).find(|y| cells[*y][0] != "│" || cells[*y][last_column] != "│")
    {
        return Some(format!("has no side edge on row {y}"));
    }
    if row(cells, 0).contains('…') {
        return Some("cuts its title short".into());
    }
    if row(cells, 0).contains(['●', '◌']) {
        return Some("marks the data state on its top edge".into());
    }
    cells
        .iter()
        .any(|line| line.concat().contains("Data received"))
        .then(|| "gives the data state a row".into())
}

/// What is wrong with a view of cards: a frame drawn around them as well, or a mark of the data
/// state.
fn cards_problem(cells: &[Vec<String>]) -> Option<String> {
    let screen = text(cells);
    if screen.contains("││") || !row(cells, 0).starts_with("┌ ") {
        return Some("draws a frame around its cards".into());
    }
    screen
        .contains(['●', '◌'])
        .then(|| "marks the data state".into())
}

fn row(cells: &[Vec<String>], row: usize) -> String {
    cells[row].concat()
}

/// The scroll indicator: the bottom row inside the Tick's frame, `inset` rows and columns in.
fn indicator(cells: &[Vec<String>], inset: usize) -> String {
    let row = &cells[cells.len() - 1 - inset];
    row[inset..row.len() - inset].concat()
}

/// Asserts the visible rows of `narrow` are the columns of `canvas` starting at `offset`, both
/// inside their frames `inset` rows and columns in; the last row inside `narrow`'s frame is its
/// scroll indicator.
fn assert_slice(
    narrow: &[Vec<String>],
    canvas: &[Vec<String>],
    offset: usize,
    inset: usize,
    label: &str,
) {
    let columns = narrow[0].len() - 2 * inset;
    let visible: Vec<Vec<String>> = narrow[inset..narrow.len() - 1 - inset]
        .iter()
        .map(|row| row[inset..inset + columns].to_vec())
        .collect();
    let slice: Vec<Vec<String>> = canvas[inset..canvas.len() - inset]
        .iter()
        .map(|row| row[inset + offset..inset + offset + columns].to_vec())
        .collect();
    assert_eq!(
        layout_of(&visible),
        layout_of(&slice),
        "{label}: the Panel is not the canvas from column {offset}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_panel_narrower_than_the_minimum_scrolls_over_the_minimum_layout() {
    let watchlist = scenario(TickKind::Watchlist, &[], &[]);
    let minimum = watchlist.minimum().columns;
    let canvas = render(&watchlist, minimum, ROWS - 1).await;
    for columns in BELOW_MINIMUM {
        let largest = usize::from(minimum - columns);
        let start = render(&watchlist, columns, ROWS).await;
        // The cards take the whole screen, with no frame to look inside.
        assert_slice(&start, &canvas, 0, 0, "before scrolling");
        let indicator = indicator(&start, 0);
        assert!(indicator.starts_with('━'), "{indicator}");
        assert!(
            // The word gives way first inside the narrowest frame.
            indicator.trim_end().contains("─ < >"),
            "{indicator}"
        );

        let right = vec![KeyCode::Char('>'); 2];
        let scrolled = render_keys(&watchlist, columns, ROWS, right).await;
        assert_slice(&scrolled, &canvas, largest, 0, "after two >");
        let indicator = self::indicator(&scrolled, 0);
        assert!(indicator.starts_with('─'), "{indicator}");
        assert!(indicator.contains("━ < >"), "{indicator}");

        let there_and_back = ['>', '>', '<', '<'].map(KeyCode::Char).to_vec();
        let back = render_keys(&watchlist, columns, ROWS, there_and_back).await;
        assert_slice(&back, &canvas, 0, 0, "after scrolling back");
    }
}

#[tokio::test(start_paused = true)]
async fn the_viewport_reveals_the_kline_cursor_and_keeps_a_manual_scroll() {
    let kline = scenario(TickKind::Kline, &[], &[]);
    // The cursor sits on the newest candle, past the right edge of this Panel.
    let columns = BELOW_MINIMUM[1];
    let start = render(&kline, columns, ROWS).await;
    assert!(text(&start).contains('▲'), "{}", text(&start));
    assert!(indicator(&start, 1).starts_with('─'), "{}", text(&start));

    let scrolled = render_keys(&kline, columns, ROWS, vec![KeyCode::Char('<'); 2]).await;
    let scrolled_text = text(&scrolled);
    assert!(!scrolled_text.contains('▲'), "{scrolled_text}");
    assert!(scrolled_text.contains("2026-"), "{scrolled_text}");
    assert!(indicator(&scrolled, 1).starts_with('━'), "{scrolled_text}");
}

#[tokio::test(start_paused = true)]
async fn a_panel_shorter_than_the_minimum_scrolls_vertically_and_follows_the_selection() {
    let earnings = scenario(TickKind::Earnings, &[], &[]);
    let minimum = earnings.minimum();
    let rows = minimum.rows - 4;
    let start = text(&render(&earnings, minimum.columns, rows).await);
    assert!(start.contains("↕ 1–"), "{start}");
    assert!(start.contains("Q2 2026"), "{start}");
    assert!(!start.contains("Q3 2024"), "{start}");

    let down = vec![KeyCode::Char('j'); 7];
    let followed = text(&render_keys(&earnings, minimum.columns, rows, down.clone()).await);
    assert!(followed.contains("Q3 2024"), "{followed}");
    assert!(!followed.contains("↕ 1–"), "{followed}");

    let manual = [down, vec![KeyCode::Char('{'); 4]].concat();
    let scrolled = text(&render_keys(&earnings, minimum.columns, rows, manual).await);
    assert!(scrolled.contains("↕ 1–"), "{scrolled}");
    assert!(!scrolled.contains("Q3 2024"), "{scrolled}");
}

#[tokio::test(start_paused = true)]
async fn scroll_keys_reach_the_tick_when_the_panel_fits_or_an_editor_owns_the_keys() {
    for kind in [
        TickKind::Quote,
        TickKind::Watchlist,
        TickKind::Kline,
        TickKind::Positions,
        TickKind::News,
        TickKind::Reader,
    ] {
        let fitting = scenario(kind, &[], &[]);
        let (columns, rows) = LAPTOP;
        let untouched = render_keys(&fitting, columns, rows, vec![KeyCode::F(12); 4]).await;
        let pressed = render_keys(
            &fitting,
            columns,
            rows,
            ['<', '>', '{', '}'].map(KeyCode::Char).to_vec(),
        )
        .await;
        // Mock prices move at random and resize their columns, so each row's leading words
        // (selection marker, symbol, heading) and the Kline cursor line show whether a key
        // changed the view.
        let settled = |cells: &[Vec<String>]| -> Vec<String> {
            layout_of(cells)
                .lines()
                .map(|line| {
                    if kind == TickKind::Kline {
                        if line.contains('▲') {
                            line.to_owned()
                        } else {
                            String::new()
                        }
                    } else {
                        line.split_whitespace()
                            .take(2)
                            .collect::<Vec<_>>()
                            .join(" ")
                    }
                })
                .collect()
        };
        assert_eq!(
            settled(&untouched),
            settled(&pressed),
            "{kind:?} binds a scroll key"
        );
    }

    let prompt = scenario(
        TickKind::Earnings,
        &[KeyCode::Char('s'), KeyCode::Char('>')],
        &[],
    );
    let frame = text(&render(&prompt, 40, ROWS).await);
    assert!(frame.contains("Symbol: >"), "{frame}");
}

#[tokio::test(start_paused = true)]
async fn the_help_overlay_lists_the_scroll_keys_while_the_panel_scrolls() {
    let help = scenario(TickKind::Quote, &[KeyCode::Char('?')], &[]);
    let scrolling = text(&render(&help, BELOW_MINIMUM[0], ROWS).await);
    assert!(scrolling.contains("< / >  Scroll"), "{scrolling}");
    let fitting = text(&render(&help, LAPTOP.0, LAPTOP.1).await);
    assert!(!fitting.contains("Scroll"), "{fitting}");
}

/// What a key hint looks like on any Tick's screen; an article may mention keys in its prose.
const KEY_HINTS: [&str; 21] = [
    "? Help",
    "?  q Quit",
    "q Quit",
    "Enter Read",
    "Backspace Back",
    "Backspace back",
    "j/k Scroll",
    "Ctrl+C",
    "Enter Apply",
    "Enter apply",
    "1–8",
    "m MA",
    "d Docs",
    "Enter Select",
    "Enter detail",
    "Enter details",
    "[ ] expiry",
    "v View",
    "a Ask Chat",
    "n/N",
    "R refresh",
];

/// A card the user only looks at, which the host reports by taking the terminal's focus away.
fn looked_at(scenario: &Scenario) -> Vec<Event> {
    std::iter::once(Event::FocusLost)
        .chain(scenario.keys.iter().copied().map(press))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_card_the_user_only_looks_at_shows_no_key_hints() {
    let mut failures = Vec::new();
    // The help overlay lists keys because someone asked for it.
    let scenarios = every_tick_at_the_minimum_width()
        .into_iter()
        .filter(|scenario| !scenario.keys.contains(&KeyCode::Char('?')));
    for scenario in scenarios {
        let (columns, rows) = (MONITOR_CARD_COLUMNS, LAPTOP.1);
        let label = scenario.label(columns, rows);
        let operable = text(&render(&scenario, columns, rows).await);
        if !KEY_HINTS.iter().any(|hint| operable.contains(hint)) {
            failures.push(format!(
                "{label} shows no key hints in its card:\n{operable}"
            ));
        }
        let read_only = text(&render_events(&scenario, columns, rows, looked_at(&scenario)).await);
        for hint in KEY_HINTS.iter().filter(|hint| read_only.contains(*hint)) {
            failures.push(format!("{label} shows {hint:?} to a looker:\n{read_only}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test(start_paused = true)]
async fn switching_focus_back_and_forth_leaves_no_stale_key_hints() {
    let news = scenario(TickKind::News, &[], &[]);
    let (columns, rows) = (MONITOR_CARD_COLUMNS, 12);
    let focus = |gained: bool| {
        if gained {
            Event::FocusGained
        } else {
            Event::FocusLost
        }
    };
    let operable = text(&render(&news, columns, rows).await);
    let read_only = text(&render_events(&news, columns, rows, vec![Event::FocusLost]).await);
    assert_ne!(operable, read_only);
    for last in [false, true] {
        let switches = (0..7)
            .map(|index| focus(index % 2 == 1))
            .chain([focus(last)]);
        let frame = text(&render_events(&news, columns, rows, switches.collect()).await);
        assert_eq!(&frame, if last { &operable } else { &read_only });
    }
}

/// The host's Agent sends a card keys while the user only looks at it, so a read-only card
/// answers every key it receives.
#[tokio::test(start_paused = true)]
async fn a_card_the_user_only_looks_at_still_answers_keys() {
    let news = scenario(TickKind::News, &[], &[]);
    let opened = vec![
        Event::FocusLost,
        press(KeyCode::Down),
        press(KeyCode::Enter),
    ];
    let frame = text(&render_events(&news, MONITOR_CARD_COLUMNS, LAPTOP.1, opened).await);
    assert!(frame.contains("示例资讯正文"), "{frame}");
}

/// Entering a card shows its keys where the card kept room for them, so nothing it shows moves.
#[tokio::test(start_paused = true)]
async fn entering_a_card_moves_none_of_its_content() {
    let mut failures = Vec::new();
    let scenarios = every_tick_at_the_minimum_width()
        .into_iter()
        .filter(|scenario| !scenario.keys.contains(&KeyCode::Char('?')));
    for scenario in scenarios {
        let (columns, rows) = (MONITOR_CARD_COLUMNS, LAPTOP.1);
        let operable = render(&scenario, columns, rows).await;
        let read_only = render_events(&scenario, columns, rows, looked_at(&scenario)).await;
        let row_of = |cells: &[Vec<String>], field: &str| {
            cells.iter().position(|line| line.concat().contains(field))
        };
        let fields = scenario
            .shows
            .iter()
            .filter(|field| !KEY_HINTS.iter().any(|hint| field.contains(hint)));
        for field in fields {
            let (entered, looked) = (row_of(&operable, field), row_of(&read_only, field));
            if entered != looked {
                failures.push(format!(
                    "{} moves {field:?} from row {looked:?} to {entered:?}",
                    scenario.label(columns, rows)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Quotes take the whole screen in their own cards, with no frame around them: which cards show,
/// when not all fit, and the keys sit on the last card's bottom edge, taking no row of their own.
#[tokio::test(start_paused = true)]
async fn quote_cards_fill_the_screen_and_say_which_show_on_the_last_card() {
    let quote = scenario(TickKind::Quote, &[], &[]);
    let cells = render(&quote, MONITOR_COLUMNS, 6).await;
    assert!(row(&cells, 0).starts_with("┌ 700.HK "), "{}", text(&cells));
    let last_edge = row(&cells, 3);
    assert!(last_edge.starts_with("└ 1–1/5 ─"), "{last_edge}");
    assert!(last_edge.contains("q Quit"), "{last_edge}");
}
