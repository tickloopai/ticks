use std::sync::Mutex;

use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier, Style},
};
use serde_json::json;
use tick_market::{
    args::{OnceArgs, QuoteArgs},
    commands::quote,
    config::Settings,
    metadata::TickKind,
};
use tick_provider::{
    api::{MarketEvent, Quote, QuoteApi},
    error::Result,
};
use tick_sdk::stream::TickStream;

struct Quotes(Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<MarketEvent>>>);

#[async_trait]
impl QuoteApi for Quotes {
    async fn quotes(&self, _: &[String]) -> Result<Vec<Quote>> {
        Ok(serde_json::from_value(json!([
            {"symbol": "AAPL.US", "name": "Apple", "latest_price": "100", "previous_close": "90"},
            {"symbol": "FAIL.US", "name": "Unavailable", "error": "Permission denied"}
        ]))
        .unwrap())
    }
    async fn subscribe_quotes(&self, _: &[String]) -> Result<()> {
        Ok(())
    }
    async fn take_events(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<MarketEvent>> {
        Ok(self.0.lock().unwrap().take().unwrap())
    }
}

fn settings() -> Settings {
    let mut styles = Settings::default().styles();
    styles.canvas = Style::reset()
        .fg(Color::Indexed(2))
        .bg(Color::Indexed(1))
        .add_modifier(Modifier::DIM);
    styles.text = Style::new()
        .fg(Color::Indexed(3))
        .add_modifier(Modifier::ITALIC);
    styles.panel = Style::new().bg(Color::Indexed(4));
    styles.selection = Style::new()
        .bg(Color::Indexed(5))
        .add_modifier(Modifier::REVERSED)
        .remove_modifier(Modifier::DIM);
    styles.overlay = Style::new()
        .fg(Color::Indexed(6))
        .bg(Color::Indexed(7))
        .add_modifier(Modifier::UNDERLINED);
    styles.title = Style::new()
        .fg(Color::Indexed(8))
        .add_modifier(Modifier::BOLD);
    styles.border = Style::new().fg(Color::Indexed(9));
    styles.status_bar = Style::new()
        .bg(Color::Indexed(10))
        .add_modifier(Modifier::CROSSED_OUT);
    styles.muted_text = Style::new().fg(Color::Indexed(11));
    styles.error = Style::new()
        .fg(Color::Indexed(12))
        .add_modifier(Modifier::BOLD);
    styles.price_up = Style::new()
        .fg(Color::Indexed(13))
        .add_modifier(Modifier::SLOW_BLINK);
    Settings {
        renderer_styles: Some(styles),
        ..Settings::default()
    }
}

async fn screen(
    width: u16,
    height: u16,
    keys: Vec<KeyCode>,
    settings: Settings,
) -> Terminal<TestBackend> {
    let (_push, events) = tokio::sync::mpsc::unbounded_channel();
    let api = Quotes(Mutex::new(Some(events)));
    let input = futures_util::stream::iter(
        keys.into_iter()
            .map(|code| Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))),
    );
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut events = TickStream::in_memory(TickKind::Quote);
    quote::run_live(
        &QuoteArgs {
            symbols: vec!["AAPL.US".into(), "FAIL.US".into()],
            output: OnceArgs::default(),
        },
        &api,
        &settings,
        &mut terminal,
        input,
        &mut events,
    )
    .await
    .unwrap();
    assert_eq!(events.records().len(), 1);
    terminal
}

/// Where `symbol` first appears in the buffer.
fn position_of(terminal: &Terminal<TestBackend>, symbol: &str) -> (u16, u16) {
    let buffer = terminal.backend().buffer();
    let index = buffer
        .content
        .iter()
        .position(|cell| cell.symbol() == symbol)
        .unwrap();
    buffer.pos_of(index)
}

#[tokio::test]
async fn injected_styles_cover_blank_panel_status_price_and_selection_modifiers() {
    let terminal = screen(100, 14, vec![KeyCode::Char('q')], settings()).await;
    let buffer = terminal.backend().buffer();
    let blank = &buffer[(90, 8)];
    assert_eq!(blank.bg, Color::Indexed(4));
    assert_eq!(blank.fg, Color::Indexed(3));
    assert_eq!(blank.modifier, Modifier::DIM | Modifier::ITALIC);
    let selected = &buffer[position_of(&terminal, "┌")];
    assert_eq!(selected.bg, Color::Indexed(5));
    assert_eq!(selected.fg, Color::Indexed(3));
    assert_eq!(selected.modifier, Modifier::ITALIC | Modifier::REVERSED);
    let (_, card_top) = position_of(&terminal, "┌");
    let unselected = (0..100)
        .rev()
        .map(|column| &buffer[(column, card_top)])
        .find(|cell| cell.symbol() == "┌")
        .unwrap();
    assert_eq!(unselected.fg, Color::Indexed(9));
    assert_eq!(unselected.bg, Color::Indexed(4));
    let gain = &buffer[position_of(&terminal, "+")];
    assert_eq!(gain.fg, Color::Indexed(13));
    assert_eq!(gain.bg, Color::Indexed(4));
    assert_eq!(
        gain.modifier,
        Modifier::DIM | Modifier::ITALIC | Modifier::SLOW_BLINK
    );
    assert_eq!(buffer[(98, 13)].bg, Color::Indexed(10));
    assert!(buffer[(98, 13)].modifier.contains(Modifier::CROSSED_OUT));
    assert_eq!(buffer[(0, 6)].fg, Color::Indexed(3));
    assert_eq!(buffer[(2, 0)].fg, Color::Indexed(8));
}

#[tokio::test]
async fn a_selected_error_card_patches_error_style_over_selection() {
    let terminal = screen(
        100,
        14,
        vec![KeyCode::Right, KeyCode::Char('q')],
        settings(),
    )
    .await;
    let cell = &terminal.backend().buffer()[position_of(&terminal, "!")];
    assert_eq!(cell.fg, Color::Indexed(12));
    assert_eq!(cell.bg, Color::Indexed(5));
    assert_eq!(
        cell.modifier,
        Modifier::ITALIC | Modifier::BOLD | Modifier::REVERSED
    );
}

#[tokio::test]
async fn help_overlay_and_undersized_canvas_keep_the_injected_background_on_blank_cells() {
    let terminal = screen(
        100,
        14,
        vec![KeyCode::Char('?'), KeyCode::Char('q')],
        settings(),
    )
    .await;
    let blank = &terminal.backend().buffer()[(80, 9)];
    assert_eq!(terminal.backend().buffer()[(14, 1)].fg, Color::Indexed(9));
    assert_eq!(blank.symbol(), " ");
    assert_eq!(blank.bg, Color::Indexed(7));
    assert_eq!(blank.fg, Color::Indexed(6));
    assert_eq!(
        blank.modifier,
        Modifier::DIM | Modifier::ITALIC | Modifier::UNDERLINED
    );
    let small = screen(20, 4, vec![KeyCode::Char('q')], settings()).await;
    assert_eq!(small.backend().buffer()[(19, 0)].bg, Color::Indexed(4));
    assert_eq!(small.backend().buffer()[(19, 3)].bg, Color::Indexed(10));
}

#[tokio::test]
async fn the_tick_title_shows_and_card_values_meet_the_card_edges() {
    let terminal = screen(64, 14, vec![KeyCode::Char('q')], Settings::default()).await;
    let lines: Vec<String> = terminal
        .backend()
        .buffer()
        .content
        .chunks(64)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect();
    assert!(lines[0].contains("Quotes"));
    assert!(lines[2].contains("┌ AAPL.US "), "{}", lines[2]);
    assert!(lines[3].contains("│100.00 "), "{}", lines[3]);
    assert!(lines[3].contains(" +11.11%│"), "{}", lines[3]);
    assert!(lines[4].contains("│+10.00 "), "{}", lines[4]);
    assert!(lines[4].contains("Permission denied"), "{}", lines[4]);
}
