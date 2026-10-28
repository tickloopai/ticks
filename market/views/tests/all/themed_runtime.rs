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

/// A terminal that counts the frames written to it: Ratatui shows or hides the cursor once per
/// frame it writes, even when no cell changed.
struct CountingBackend {
    inner: TestBackend,
    frames: usize,
    clears: usize,
}

impl CountingBackend {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            inner: TestBackend::new(columns, rows),
            frames: 0,
            clears: 0,
        }
    }
}

impl ratatui::backend::Backend for CountingBackend {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::result::Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.frames += 1;
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.frames += 1;
        self.inner.show_cursor()
    }
    fn get_cursor_position(
        &mut self,
    ) -> std::result::Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> std::result::Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        self.clears += 1;
        self.inner.clear()
    }
    fn clear_region(
        &mut self,
        clear_type: ratatui::backend::ClearType,
    ) -> std::result::Result<(), Self::Error> {
        self.clears += usize::from(clear_type == ratatui::backend::ClearType::All);
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> std::result::Result<ratatui::layout::Size, Self::Error> {
        self.inner.size()
    }
    fn window_size(&mut self) -> std::result::Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_screen_is_not_written_again_and_a_resize_redraws_it_whole() {
    let (_push, market) = tokio::sync::mpsc::unbounded_channel();
    let api = Quotes(Mutex::new(Some(market)));
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let key = |code| Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        // Keys no Quote view binds, a resize, then quit; each arrives on a frame of its own.
        for event in [
            key(KeyCode::F(12)),
            key(KeyCode::F(12)),
            key(KeyCode::Char('x')),
            Ok(Event::Resize(40, 12)),
            key(KeyCode::Char('q')),
        ] {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            sender.send(event).unwrap();
        }
    });
    let mut terminal = Terminal::new(CountingBackend::new(40, 12)).unwrap();
    quote::run_live(
        &QuoteArgs {
            symbols: vec!["AAPL.US".into()],
            output: OnceArgs::default(),
        },
        &api,
        &Settings::default(),
        &mut terminal,
        tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
        &mut TickStream::in_memory(TickKind::Quote),
    )
    .await
    .unwrap();
    let backend = terminal.backend();
    assert_eq!(backend.clears, 1, "the resize clears the terminal once");
    assert_eq!(
        backend.frames, 2,
        "only the first frame and the one after the resize are written"
    );
}

type Keys = tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>;
type Sizes = std::sync::Arc<Mutex<std::collections::VecDeque<ratatui::layout::Size>>>;

/// A terminal a user drags through sizes. The front of `sizes` is the current size, and each time
/// the size is asked for the drag moves on to the next queued one and signals the resize, as
/// SIGWINCH does after the change: sizes queued together change between asking for the size and
/// writing a frame of it.
struct DraggedBackend {
    inner: TestBackend,
    sizes: Sizes,
    signals: Keys,
}

impl ratatui::backend::Backend for DraggedBackend {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::result::Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        let size = self.sizes.lock().unwrap()[0];
        if self.inner.size()? != size {
            self.inner.resize(size.width, size.height);
        }
        // Cells of a frame composed before the latest change may fall outside the terminal.
        let content: Vec<_> = content
            .filter(|(x, y, _)| *x < size.width && *y < size.height)
            .collect();
        self.inner.draw(content.into_iter())
    }
    fn hide_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(
        &mut self,
    ) -> std::result::Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> std::result::Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.clear()
    }
    fn clear_region(
        &mut self,
        clear_type: ratatui::backend::ClearType,
    ) -> std::result::Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }
    fn size(&self) -> std::result::Result<ratatui::layout::Size, Self::Error> {
        let mut sizes = self.sizes.lock().unwrap();
        if sizes.len() > 1 {
            sizes.pop_front();
            let _ = self
                .signals
                .send(Ok(Event::Resize(sizes[0].width, sizes[0].height)));
        }
        Ok(sizes[0])
    }
    fn window_size(&mut self) -> std::result::Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// Runs the Quote Tick on `terminal` with `keys` sending its input, and quits once `script` has
/// run and the Tick has settled.
async fn run_quote<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    keys: (
        Keys,
        tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Event>>,
    ),
    script: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let (_push, market) = tokio::sync::mpsc::unbounded_channel();
    let api = Quotes(Mutex::new(Some(market)));
    let (sender, receiver) = keys;
    tokio::spawn(async move {
        script.await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let quit = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        sender.send(Ok(quit)).unwrap();
    });
    quote::run_live(
        &QuoteArgs {
            symbols: vec!["AAPL.US".into(), "TSLA.US".into()],
            output: OnceArgs::default(),
        },
        &api,
        &Settings::default(),
        terminal,
        tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
        &mut TickStream::in_memory(TickKind::Quote),
    )
    .await
    .unwrap();
}

fn screen_text(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    buffer
        .content()
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn dragging_a_card_through_many_sizes_ends_on_a_whole_screen_of_the_last_size() {
    use ratatui::layout::Size;
    let last = Size::new(30, 6);
    // Widths and heights both change, down to 30x6; a few changes come at a time, so some land
    // between a frame being composed and written.
    let drag = [
        (80, 24),
        (64, 20),
        (52, 18),
        (40, 14),
        (33, 9),
        (30, 6),
        (31, 7),
        (45, 12),
        (70, 22),
        (100, 30),
        (36, 8),
        (30, 6),
        (58, 16),
        (41, 11),
        (30, 10),
        (90, 26),
        (47, 13),
        (35, 6),
        (30, 14),
        (60, 6),
        (30, 7),
        (44, 19),
        (32, 6),
        (30, 6),
    ]
    .map(|(columns, rows)| Size::new(columns, rows));
    let keys = tokio::sync::mpsc::unbounded_channel();
    let sizes: Sizes = std::sync::Arc::new(Mutex::new([Size::new(80, 24)].into()));
    let mut dragged = Terminal::new(DraggedBackend {
        inner: TestBackend::new(80, 24),
        sizes: std::sync::Arc::clone(&sizes),
        signals: keys.0.clone(),
    })
    .unwrap();
    let signals = keys.0.clone();
    let script = async move {
        for batch in drag.chunks(3) {
            tokio::time::sleep(std::time::Duration::from_millis(7)).await;
            sizes.lock().unwrap().extend(batch);
            signals
                .send(Ok(Event::Resize(batch[0].width, batch[0].height)))
                .unwrap();
        }
    };
    run_quote(&mut dragged, keys, script).await;

    let mut fresh = Terminal::new(TestBackend::new(last.width, last.height)).unwrap();
    run_quote(&mut fresh, tokio::sync::mpsc::unbounded_channel(), async {}).await;
    let backend = dragged.backend();
    assert_eq!(backend.inner.buffer().area.as_size(), last);
    assert_eq!(
        screen_text(backend.inner.buffer()),
        screen_text(fresh.backend().buffer())
    );
}
