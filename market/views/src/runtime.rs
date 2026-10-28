use crate::{
    canvas::CanvasBackend, config::Settings, output::TableData, tools::Tools, viewport::KeyBubbling,
};
use async_trait::async_trait;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt, stream::FuturesUnordered};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    buffer::Buffer,
    layout::{Position, Rect, Size},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use serde_json::{Value, json};
use tick_protocol::{INVALID_PARAMS, TOOL_FAILED};
use tick_provider::{
    api::{ConnectionState, MarketEvent, QuoteServices},
    error::{Error, Result},
};
use tick_sdk::{
    stream::{RpcError, TickStream, ToolRequest},
    tools::{ToolCall, ToolDispatch},
};
use tickloop_ui::{
    format::{pad, width, write_text},
    scroll::{ScrollViewport, ViewportLayout},
};

const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);
const WIDE_PANEL_COLUMNS: u16 = 64;
const TALL_PANEL_ROWS: u16 = 10;
const HELP_TITLE: &str = " Keys · ? / Esc close ";
const NARROW_HELP_TITLE: &str = " Keys · ? close ";
/// Under a help overlay too short for every key.
const HELP_SCROLL_HINT: &str = " j/k more ";
pub const CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
pub const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// A Tick Tool call runs beside the view, so both the number in flight and how
/// long each one may take are bounded.
pub const TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const MAX_CONCURRENT_TOOL_CALLS: usize = 4;

/// The help line for the keys that quit a standalone Tick.
const QUIT_HELP: &str = "q / Ctrl+C  Quit";

/// Whether a host launched this process as a Tick Instance.
pub fn in_instance() -> bool {
    std::env::var_os(tick_protocol::INSTANCE_ID_ENV).is_some()
}

/// Whether the keys quit the Tick. A host ends the Ticks it runs itself and shows them in cards
/// where a stray q or Ctrl+C must not end one, so a hosted Tick quits on no key.
pub fn quits_on_keys() -> bool {
    !in_instance()
}

pub fn is_exit(event: &Event) -> bool {
    quits_on_keys()
        && matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release && (key.code == KeyCode::Char('q') || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))))
}

pub struct NoMarketApi;

#[async_trait]
impl tick_provider::api::QuoteApi for NoMarketApi {
    async fn quotes(&self, _: &[String]) -> Result<Vec<tick_provider::api::Quote>> {
        Err(Error::usage("this Tick uses its own data services"))
    }
}

pub async fn cancellable<T, S: Stream<Item = std::io::Result<Event>> + Unpin>(
    future: impl std::future::Future<Output = Result<T>>,
    input: &mut S,
) -> Result<Option<T>> {
    cancellable_unless_editing(future, input, false).await
}

/// Like [`cancellable`], except that while an in-Panel editor is open q and Ctrl+C belong to it:
/// only the end of input cancels, and keys typed meanwhile are dropped as they are for any wait.
async fn cancellable_unless_editing<T, S: Stream<Item = std::io::Result<Event>> + Unpin>(
    future: impl std::future::Future<Output = Result<T>>,
    input: &mut S,
    editing: bool,
) -> Result<Option<T>> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            result = &mut future => return result.map(Some),
            event = input.next() => match event {
                None => return Ok(None),
                Some(Err(error)) => return Err(error.into()),
                Some(Ok(event)) if !editing && is_exit(&event) => return Ok(None),
                _ => (),
            },
        }
    }
}

pub struct ViewState {
    pub selected: usize,
    pub offset: usize,
    pub help: bool,
    /// How many lines of the help overlay are scrolled past; a short Panel scrolls its keys.
    pub help_offset: u16,
    pub connection: ConnectionState,
    pub last_error: Option<String>,
    /// The canvas column and row of what the view has focused in this frame; a scrolling
    /// Panel keeps them in view.
    pub focus_column: Option<u16>,
    pub focus_row: Option<u16>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            selected: 0,
            offset: 0,
            help: false,
            help_offset: 0,
            connection: ConnectionState::Connected,
            last_error: None,
            focus_column: None,
            focus_row: None,
        }
    }
}

impl ViewState {
    fn failed(&mut self, error: Error) -> Result<()> {
        if error.code == tick_provider::exit_code::NETWORK {
            self.connection = ConnectionState::Disconnected;
            self.last_error = Some(error.message);
            Ok(())
        } else {
            Err(error)
        }
    }

    fn received_data(&mut self) {
        self.connection = ConnectionState::Connected;
        self.last_error = None;
    }

    fn availability(&self) -> &str {
        match self.connection {
            ConnectionState::Connected => "Data received",
            ConnectionState::Reconnecting => "Recovering",
            ConnectionState::Disconnected if self.last_error.is_some() => "Unavailable · retry",
            ConnectionState::Disconnected => "Disconnected",
        }
    }
}

#[async_trait]
pub trait LiveView: Send {
    fn research_context(&self) -> Option<crate::research::ResearchContext> {
        None
    }
    fn take_navigation(&mut self) -> Option<crate::research::ResearchNavigation> {
        None
    }
    fn tools(&self) -> Tools {
        crate::tools::for_kind(self.tick())
    }
    fn tick(&self) -> crate::metadata::TickKind {
        crate::metadata::TickKind::Quote
    }
    /// The Panel this view needs now: the Tick's declared minimum unless the view is in a
    /// mode with a wider layout of its own.
    fn minimum_panel_size(&self) -> crate::metadata::MinimumPanelSize {
        self.tick().minimum_panel_size()
    }
    fn title(&self) -> String;
    fn table(&self) -> TableData;
    fn content(&self) -> serde_json::Value {
        self.table().json()
    }
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        None
    }
    fn refresh_on_recovery(&self) -> bool {
        false
    }
    async fn refresh(&mut self, _api: &QuoteServices) -> Result<()> {
        Ok(())
    }
    fn priority(&self) -> &[usize];
    fn on_market(&mut self, event: &MarketEvent);
    async fn on_key(
        &mut self,
        _key: KeyCode,
        _api: &QuoteServices,
        _state: &mut ViewState,
    ) -> Result<bool> {
        Ok(false)
    }
    /// Applies an action Tick Tool. The runtime publishes the next Tick Update
    /// by itself when `content()` changed, so a view never publishes here.
    async fn act(&mut self, call: ToolCall) -> Result<Value> {
        Err(Error::usage(format!(
            "this Tick offers no action Tick Tool {:?}",
            call.tool
        )))
    }
    fn help(&self) -> &'static str {
        "?  Help\nj / k / Ctrl+N / Ctrl+P / arrows  Move\nPageUp / PageDown  Page\ng / G  First / last"
    }
    /// While an in-Panel editor is open the view owns every printable key and Backspace, and
    /// Ctrl+C cancels the edit instead of quitting.
    fn captures_text(&self) -> bool {
        false
    }
    /// Closes the open in-Panel editor without applying it.
    fn cancel_text(&mut self) {}
    fn draw(&self, frame: &mut Frame<'_>, state: &mut ViewState, settings: &Settings) {
        draw_table(
            frame,
            &self.title(),
            &self.table(),
            self.priority(),
            state,
            settings,
        );
    }
}

/// A query Tick Tool call running beside the view. Its id stays readable, so a run that fails
/// still answers it.
struct RunningToolCall<'a> {
    id: Value,
    result: futures_util::future::BoxFuture<'a, std::result::Result<Value, RpcError>>,
}

impl std::future::Future for RunningToolCall<'_> {
    type Output = (Value, std::result::Result<Value, RpcError>);

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.result
            .as_mut()
            .poll(context)
            .map(|result| (self.id.clone(), result))
    }
}

/// An accepted `tick/call`: a query already running beside the view, or an
/// action the loop still has to apply on the UI path.
enum AcceptedToolCall<'a> {
    Query(RunningToolCall<'a>),
    Action { id: Value, call: ToolCall },
}

fn tool_failure(error: &Error) -> RpcError {
    RpcError::new(TOOL_FAILED, error.message.clone()).with_data(json!({"code": error.code}))
}

/// Checks one `tick/call` against the declared catalog and either starts it
/// beside the view or hands the action on; otherwise produces the error to
/// answer it with. A query never borrows the view, so the Panel keeps drawing
/// while a market round trip is in flight.
fn accept_tool_call<'a>(
    tools: &Tools,
    services: &'a QuoteServices,
    call: ToolRequest,
    calls_in_flight: usize,
) -> std::result::Result<AcceptedToolCall<'a>, (Value, RpcError)> {
    let Some(tool) = tools.get(&call.tool) else {
        return Err((call.id, RpcError::unknown_tool(&call.tool)));
    };
    if !tool.mutates() && calls_in_flight >= MAX_CONCURRENT_TOOL_CALLS {
        return Err((
            call.id,
            RpcError::new(
                TOOL_FAILED,
                format!(
                    "this Tick already runs {MAX_CONCURRENT_TOOL_CALLS} Tick Tool calls; retry when one finishes"
                ),
            ),
        ));
    }
    let dispatch = tool.prepare(services, call.args).map_err(|message| {
        (
            call.id.clone(),
            RpcError::new(INVALID_PARAMS, message)
                .with_data(json!({"schema": tool.input_schema()})),
        )
    })?;
    let id = call.id;
    Ok(match dispatch {
        ToolDispatch::Action(call) => AcceptedToolCall::Action { id, call },
        ToolDispatch::Query(running) => AcceptedToolCall::Query(RunningToolCall {
            id,
            result: Box::pin(async move {
                tokio::time::timeout(TOOL_CALL_TIMEOUT, running)
                    .await
                    .unwrap_or_else(|_| Err(Error::network("Tick Tool call timed out")))
                    .map_err(|error| tool_failure(&error))
            }),
        }),
    })
}

async fn refresh_cancellable<V: LiveView, S: Stream<Item = std::io::Result<Event>> + Unpin>(
    view: &mut V,
    api: &QuoteServices,
    input: &mut S,
) -> Result<Option<()>> {
    let editing = view.captures_text();
    cancellable_unless_editing(
        async {
            tokio::time::timeout(REFRESH_TIMEOUT, view.refresh(api))
                .await
                .unwrap_or_else(|_| Err(Error::network("refresh timed out")))
        },
        input,
        editing,
    )
    .await
}

pub async fn run<
    V: LiveView,
    B: Backend + KeyBubbling,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
>(
    view: &mut V,
    api: &QuoteServices,
    settings: &Settings,
    terminal: &mut Terminal<B>,
    mut input: S,
    mut market: tokio::sync::mpsc::UnboundedReceiver<MarketEvent>,
    updates: &mut TickStream,
) -> Result<()> {
    let mut requests = updates.take_requests();
    let mut requests_closed = false;
    let tools = view.tools();
    let mut running_calls = FuturesUnordered::new();
    let mut state = ViewState::default();
    let mut viewport = ScrollViewport::default();
    let mut shown = None;
    let mut market_open = true;
    let mut frames = tokio::time::interval(FRAME_INTERVAL);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    let mut configured_refresh = view.refresh_interval();
    let refresh_interval = configured_refresh.unwrap_or(std::time::Duration::from_secs(86400));
    let mut refreshes = tokio::time::interval_at(
        tokio::time::Instant::now() + refresh_interval,
        refresh_interval,
    );
    refreshes.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    updates.publish(view.content())?;
    let outcome: Result<()> = async {
        loop {
            if view.take_navigation().is_some() {
                state.last_error = Some("Cross-Tick navigation is unavailable in this host.".into());
                dirty = true;
            }
            let next_refresh = view.refresh_interval();
            if next_refresh != configured_refresh {
                configured_refresh = next_refresh;
                if let Some(interval) = configured_refresh {
                    refreshes =
                        tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
                    refreshes.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                }
            }
            tokio::select! {
                line = requests.next(), if !requests_closed => match line {
                    None => {
                        requests_closed = true;
                        if running_calls.is_empty() { break; }
                    }
                    Some(line) => {
                        if let Some(call) = updates.dispatch(line) {
                            match accept_tool_call(&tools, api, call, running_calls.len()) {
                                Ok(AcceptedToolCall::Query(running)) => running_calls.push(running),
                                Ok(AcceptedToolCall::Action { id, call }) => {
                                    let previous_content = view.content();
                                    let applied = view.act(call).await;
                                    if applied.is_ok() {
                                        let content = view.content();
                                        if content != previous_content {
                                            updates.publish(content)?;
                                        }
                                        dirty = true;
                                    }
                                    updates.answer(id, applied.map_err(|error| tool_failure(&error)));
                                }
                                Err((id, error)) => updates.answer(id, Err(error)),
                            }
                        }
                    }
                },
                finished = running_calls.next(), if !running_calls.is_empty() => {
                    if let Some((id, result)) = finished {
                        updates.answer(id, result);
                    }
                    if requests_closed && running_calls.is_empty() { break; }
                }
                _ = refreshes.tick(), if configured_refresh.is_some() => {
                    if state.connection != ConnectionState::Connected {
                        state.connection = ConnectionState::Reconnecting;
                        render(view, terminal, &mut state, &mut viewport, &mut shown, settings)?;
                    }
                    match refresh_cancellable(view, api, &mut input).await {
                        Ok(None) => break,
                        Ok(Some(())) => { state.received_data(); updates.publish(view.content())?; }
                        Err(error) => state.failed(error)?,
                    }
                    dirty = true;
                }
                _ = frames.tick() => {
                    if dirty { render(view, terminal, &mut state, &mut viewport, &mut shown, settings)?; dirty = false; }
                }
                event = market.recv(), if market_open => {
                    dirty = true;
                    match event {
                        Some(event) => {
                            match event {
                                MarketEvent::Failure { code, message } => state.failed(Error::new(code, message))?,
                                MarketEvent::Connection(connection) => {
                                    let recovered = connection == ConnectionState::Connected && state.connection != ConnectionState::Connected;
                                    if recovered && view.refresh_on_recovery() {
                                        state.connection = ConnectionState::Reconnecting;
                                        render(view, terminal, &mut state, &mut viewport, &mut shown, settings)?;
                                        match refresh_cancellable(view, api, &mut input).await {
                                            Ok(None) => break,
                                            Ok(Some(())) => { state.received_data(); updates.publish(view.content())?; }
                                            Err(error) => state.failed(error)?,
                                        }
                                    } else {
                                        state.connection = connection;
                                        if connection == ConnectionState::Connected { state.received_data(); }
                                    }
                                }
                                event => {
                                    let previous_content = view.content();
                                    view.on_market(&event);
                                    let content = view.content();
                                    if content != previous_content {
                                        state.received_data();
                                        updates.publish(content)?;
                                    }
                                }
                            }
                        }
                        None => { market_open = false; state.connection = ConnectionState::Disconnected; }
                    }
                }
                event = input.next() => {
                    dirty = true;
                    match event {
                        None => break,
                        Some(Err(error)) => return Err(error.into()),
                        Some(Ok(Event::Resize(_, _))) => { terminal.clear().map_err(|error| Error::usage(error.to_string()))?; shown = None; }
                        Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                            let bubble = bubbled_key_name(&key, view.captures_text());
                            let size = terminal.size().map_err(|error| Error::usage(error.to_string()))?;
                            let before = bubble.map(|_| compose(view, size, &mut state, &mut viewport, settings));
                            if handle_key(key, view, api, terminal, &mut state, &mut viewport, &mut input, updates).await? == KeyOutcome::Quit {
                                break;
                            }
                            // A key that changed nothing on screen is one the Tick has no use for here.
                            if let (Some(name), Some(before)) = (bubble, before)
                                && compose(view, size, &mut state, &mut viewport, settings) == before
                            {
                                terminal.backend_mut().bubble(name).map_err(|error| Error::usage(error.to_string()))?;
                            }
                        }
                        _ => (),
                    }
                }
            }
        }
        if dirty {
            render(view, terminal, &mut state, &mut viewport, &mut shown, settings)?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = &outcome {
        // The host would otherwise wait out its own timeout for the queries still running.
        for call in running_calls.iter() {
            updates.answer(call.id.clone(), Err(tool_failure(error)));
        }
    }
    outcome
}

/// The screen as last written to the terminal, with its cursor.
type ShownScreen = Option<(Buffer, Option<Position>)>;

/// Composes the screen in memory and writes it only when it differs from the one last written,
/// so a frame with no change sends the terminal nothing; a resize clears `shown` and the next
/// frame is written whole.
/// How many times a frame is composed again when the terminal keeps resizing while it is written;
/// the SIGWINCH of a later change redraws it after that.
const RESIZED_FRAME_ATTEMPTS: usize = 4;

fn render<V: LiveView, B: Backend>(
    view: &V,
    terminal: &mut Terminal<B>,
    state: &mut ViewState,
    viewport: &mut ScrollViewport,
    shown: &mut ShownScreen,
    settings: &Settings,
) -> Result<()> {
    for _ in 0..RESIZED_FRAME_ATTEMPTS {
        if render_once(view, terminal, state, viewport, shown, settings)? {
            break;
        }
    }
    Ok(())
}

/// Writes one frame, and whether it was written whole: false when the terminal resized after the
/// frame was composed.
fn render_once<V: LiveView, B: Backend>(
    view: &V,
    terminal: &mut Terminal<B>,
    state: &mut ViewState,
    viewport: &mut ScrollViewport,
    shown: &mut ShownScreen,
    settings: &Settings,
) -> Result<bool> {
    // A terminal that changed size since the last frame is cleared and drawn whole, never diffed
    // against a frame of another size.
    terminal
        .autoresize()
        .map_err(|error| Error::usage(error.to_string()))?;
    let size = terminal.get_frame().area().as_size();
    if shown
        .as_ref()
        .is_some_and(|(buffer, _)| buffer.area.as_size() != size)
    {
        *shown = None;
    }
    let composed = compose(view, size, state, viewport, settings);
    if shown.as_ref() == Some(&composed) {
        return Ok(true);
    }
    let mut whole = true;
    terminal
        .draw(|frame| {
            let area = frame.area();
            if area == composed.0.area {
                frame.buffer_mut().clone_from(&composed.0);
                if let Some(cursor) = composed.1 {
                    frame.set_cursor_position(cursor);
                }
                return;
            }
            // The terminal resized after the screen was composed: show what still fits until the
            // frame is composed again for the new size.
            whole = false;
            let overlap = area.intersection(composed.0.area);
            for position in overlap.positions() {
                frame.buffer_mut()[position] = composed.0[position].clone();
            }
        })
        .map_err(|error| Error::usage(error.to_string()))?;
    *shown = whole.then_some(composed);
    Ok(whole)
}

/// What a key did to the Tick.
#[derive(Debug, PartialEq, Eq)]
enum KeyOutcome {
    Handled,
    Quit,
}

/// The name a host knows `key` by when the Tick hands it on: the keys that move between cards.
/// An open editor keeps every key.
fn bubbled_key_name(key: &crossterm::event::KeyEvent, capturing: bool) -> Option<&'static str> {
    if capturing
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }
    Some(match key.code {
        KeyCode::Esc => "esc",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "backtab",
        _ => return None,
    })
}

/// Hands `key` to the help overlay, an open editor, the view or the shared list selection.
#[allow(clippy::too_many_arguments)]
async fn handle_key<V: LiveView, B: Backend, S: Stream<Item = std::io::Result<Event>> + Unpin>(
    key: crossterm::event::KeyEvent,
    view: &mut V,
    api: &QuoteServices,
    terminal: &Terminal<B>,
    state: &mut ViewState,
    viewport: &mut ScrollViewport,
    input: &mut S,
    updates: &mut TickStream,
) -> Result<KeyOutcome> {
    let capturing = view.captures_text();
    let interrupted =
        key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    if (interrupted || key.code == KeyCode::Esc) && capturing {
        view.cancel_text();
        return Ok(KeyOutcome::Handled);
    }
    if !capturing && is_exit(&Event::Key(key)) {
        return Ok(KeyOutcome::Quit);
    }
    if !capturing && key.code == KeyCode::Char('?') {
        state.help = !state.help;
        state.help_offset = 0;
        return Ok(KeyOutcome::Handled);
    }
    if state.help {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = terminal
            .size()
            .map_err(|error| Error::usage(error.to_string()))?
            .height
            .saturating_sub(3)
            .max(1);
        // The renderer clamps the offset to the keys the overlay holds.
        match (key.code, control) {
            (KeyCode::Backspace | KeyCode::Esc, false) => state.help = false,
            (KeyCode::Char('j') | KeyCode::Down, false) | (KeyCode::Char('n'), true) => {
                state.help_offset = state.help_offset.saturating_add(1)
            }
            (KeyCode::Char('k') | KeyCode::Up, false) | (KeyCode::Char('p'), true) => {
                state.help_offset = state.help_offset.saturating_sub(1)
            }
            (KeyCode::PageDown, false) => {
                state.help_offset = state.help_offset.saturating_add(page)
            }
            (KeyCode::PageUp, false) => state.help_offset = state.help_offset.saturating_sub(page),
            (KeyCode::Char('g') | KeyCode::Home, false) => state.help_offset = 0,
            (KeyCode::Char('G') | KeyCode::End, false) => state.help_offset = u16::MAX,
            _ => (),
        }
        return Ok(KeyOutcome::Handled);
    }
    if !capturing
        && let KeyCode::Char(character) = key.code
        && viewport.handle_key(character)
    {
        return Ok(KeyOutcome::Handled);
    }
    let code = if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('n') => KeyCode::Down,
            KeyCode::Char('p') => KeyCode::Up,
            _ => return Ok(KeyOutcome::Handled),
        }
    } else if key
        .modifiers
        .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return Ok(KeyOutcome::Handled);
    } else if key.code == KeyCode::Esc {
        // Esc goes back a level as Backspace does; a host takes a second Esc soon after to leave
        // the card, so nothing waits for one.
        KeyCode::Backspace
    } else {
        key.code
    };
    let previous_content = view.content();
    // Enter submits the edit, so what it waits for is no longer editing and q / Ctrl+C quit again.
    let editing = capturing && code != KeyCode::Enter;
    let handled =
        match cancellable_unless_editing(view.on_key(code, api, state), input, editing).await {
            Ok(Some(handled)) => handled,
            Ok(None) => return Ok(KeyOutcome::Quit),
            Err(error) => {
                state.failed(error)?;
                return Ok(KeyOutcome::Handled);
            }
        };
    if handled {
        let content = view.content();
        if content != previous_content {
            state.received_data();
            updates.publish(content)?;
        }
        return Ok(KeyOutcome::Handled);
    }
    let length = view.table().rows.len();
    let page = terminal
        .size()
        .map_err(|error| Error::usage(error.to_string()))?
        .height
        .saturating_sub(4)
        .max(1) as usize;
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            state.selected = (state.selected + 1).min(length.saturating_sub(1))
        }
        KeyCode::Char('k') | KeyCode::Up => state.selected = state.selected.saturating_sub(1),
        KeyCode::PageDown => state.selected = (state.selected + page).min(length.saturating_sub(1)),
        KeyCode::PageUp => state.selected = state.selected.saturating_sub(page),
        KeyCode::Char('g') | KeyCode::Home => state.selected = 0,
        KeyCode::Char('G') | KeyCode::End => state.selected = length.saturating_sub(1),
        _ => (),
    }
    Ok(KeyOutcome::Handled)
}

/// The whole screen of `size` the view shows now, with its cursor: the view and its status row,
/// scrolled over the canvas of the Tick's minimum when the screen is smaller, and the help overlay.
fn compose<V: LiveView>(
    view: &V,
    size: Size,
    state: &mut ViewState,
    viewport: &mut ScrollViewport,
    settings: &Settings,
) -> (Buffer, Option<Position>) {
    let minimum = view.minimum_panel_size();
    viewport.set_layout(ViewportLayout::of(
        Rect::new(0, 0, size.width, size.height),
        Size::new(minimum.columns, minimum.rows),
    ));
    let canvas = match viewport.layout() {
        Some(layout) => {
            // ratatui hands out a Frame only through a Terminal, so the canvas is an in-memory one.
            let Ok(mut canvas) = Terminal::new(CanvasBackend::new(layout.canvas));
            let Ok(_) = canvas.draw(|frame| draw_canvas(view, frame, state, settings));
            let backend = canvas.backend();
            let cursor = backend.cursor();
            viewport.follow(
                cursor.map(|cursor| cursor.x).or(state.focus_column),
                cursor.map(|cursor| cursor.y).or(state.focus_row),
            );
            Some((backend.buffer().clone(), cursor))
        }
        None => None,
    };
    let Ok(mut screen) = Terminal::new(CanvasBackend::new(size));
    let Ok(_) = screen.draw(|frame| {
        let styles = settings.styles();
        match &canvas {
            None => draw_canvas(view, frame, state, settings),
            Some((buffer, cursor)) => {
                let area = frame.area();
                frame.buffer_mut().set_style(area, styles.canvas);
                viewport.render(buffer, frame.buffer_mut(), styles, settings.ambiguous_width);
                if let Some(cursor) = cursor.and_then(|cursor| viewport.on_panel(cursor)) {
                    frame.set_cursor_position(cursor);
                }
            }
        }
        if state.help {
            let area = frame.area();
            let mut keys = view.help().to_owned();
            if quits_on_keys() {
                keys = format!("{QUIT_HELP}\n{keys}");
            }
            if let Some(scroll_keys) = viewport.help() {
                keys = format!("{keys}\n{scroll_keys}");
            }
            let block = Block::default()
                .title(
                    if width(HELP_TITLE, settings.ambiguous_width) + 4 <= usize::from(area.width) {
                        HELP_TITLE
                    } else {
                        NARROW_HELP_TITLE
                    },
                )
                .borders(Borders::ALL)
                .border_style(styles.border);
            let help = Paragraph::new(keys)
                .block(block.clone())
                .wrap(Wrap { trim: false })
                .style(styles.overlay_base());
            // A narrow Panel grows the overlay toward its edges before wrapping the keys.
            let width = (area.width * 3 / 4).max(
                u16::try_from(help.line_width())
                    .unwrap_or(u16::MAX)
                    .min(area.width.saturating_sub(2)),
            );
            let lines = u16::try_from(help.line_count(width)).unwrap_or(u16::MAX);
            // A short Panel gives the overlay every row above the status row and scrolls the keys.
            let height = (area.height * 3 / 4).max(lines.min(area.height.saturating_sub(1)));
            let hidden = lines.saturating_sub(height);
            state.help_offset = state.help_offset.min(hidden);
            let help = if hidden == 0 {
                help
            } else {
                help.block(block.title_bottom(HELP_SCROLL_HINT))
                    .scroll((state.help_offset, 0))
            };
            let popup = Rect::new(
                area.x + (area.width / 8).min((area.width - width) / 2),
                area.y + (area.height / 8).min((area.height - height) / 2),
                width,
                height,
            );
            frame.render_widget(Clear, popup);
            frame.buffer_mut().set_style(popup, styles.overlay_base());
            frame.render_widget(help, popup);
        }
    });
    (screen.backend().buffer().clone(), screen.backend().cursor())
}

/// Draws the view and the status row into the frame, which is the Panel itself or, for a Panel
/// smaller than the Tick's minimum, the canvas it scrolls over.
fn draw_canvas<V: LiveView>(
    view: &V,
    frame: &mut Frame<'_>,
    state: &mut ViewState,
    settings: &Settings,
) {
    let area = frame.area();
    let styles = settings.styles();
    frame.buffer_mut().set_style(area, styles.canvas);
    state.focus_column = None;
    state.focus_row = None;
    view.draw(frame, state, settings);
    let horizontal_padding = if area.width >= WIDE_PANEL_COLUMNS {
        2
    } else {
        1
    };
    let status_area = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let mut status_style = styles.status_base().patch(styles.muted_text);
    if state.last_error.is_some() {
        status_style = status_style.patch(styles.error);
    }
    frame
        .buffer_mut()
        .set_style(status_area, styles.status_base());
    let icon = tickloop_ui::icons::Icon::Connection.glyph();
    let status = if let Some(error) = &state.last_error {
        if state.connection == ConnectionState::Connected {
            error.clone()
        } else {
            format!("{icon} {} · {error}", state.availability())
        }
    } else {
        format!("{icon} {}", state.availability())
    };
    let keys = if view.captures_text() && area.width >= 40 {
        "Enter Apply  Ctrl+C Cancel"
    } else if view.captures_text() {
        "Ctrl+C Cancel"
    } else if !quits_on_keys() {
        "? Help"
    } else if area.width >= 40 {
        "? Help  q Quit"
    } else {
        "?  q Quit"
    };
    let keys_width = width(keys, settings.ambiguous_width) as u16;
    write_text(
        frame.buffer_mut(),
        area.x,
        area.bottom() - 1,
        &" ".repeat(area.width as usize),
        area.width as usize,
        settings.ambiguous_width,
        styles.status_base(),
    );
    write_text(
        frame.buffer_mut(),
        area.x + horizontal_padding,
        area.bottom() - 1,
        &status,
        area.width
            .saturating_sub(horizontal_padding * 2 + keys_width + 2) as usize,
        settings.ambiguous_width,
        status_style,
    );
    write_text(
        frame.buffer_mut(),
        area.right() - horizontal_padding - keys_width,
        area.bottom() - 1,
        keys,
        keys_width as usize,
        settings.ambiguous_width,
        styles.status_base().patch(styles.muted_text),
    );
}

/// Why a Tick has no content from its data source yet.
pub enum Connecting<'a> {
    /// Attempt `attempt` is under way or next; `last_error` is why the one before failed.
    Retrying {
        attempt: usize,
        last_error: Option<&'a str>,
    },
    /// The source refused the login; `hint` says how to log in again.
    NotAuthorized { hint: &'a str },
    /// The source turned the Tick's request down with `error`, such as an unknown symbol.
    Rejected { error: &'a str },
}

/// Draws what a Tick shows before its first content while it reaches its data source `source`.
pub fn draw_connecting<B: Backend>(
    terminal: &mut Terminal<B>,
    settings: &Settings,
    source: &str,
    connecting: &Connecting<'_>,
) -> Result<()> {
    use tick_provider::preferences::Language;
    let styles = settings.styles();
    let (status, detail, detail_style) = match (connecting, settings.language) {
        (
            Connecting::Retrying {
                attempt,
                last_error,
            },
            Language::ZhCn,
        ) => (
            format!("正在连接{source}（第 {attempt} 次）"),
            last_error.map(|error| format!("上次错误：{error}")),
            styles.error,
        ),
        (
            Connecting::Retrying {
                attempt,
                last_error,
            },
            Language::En,
        ) => (
            format!("Connecting to {source} (attempt {attempt})"),
            last_error.map(|error| format!("Last error: {error}")),
            styles.error,
        ),
        (Connecting::NotAuthorized { hint }, Language::ZhCn) => (
            format!("{source}需要重新登录"),
            Some((*hint).to_owned()),
            styles.muted_text,
        ),
        (Connecting::NotAuthorized { hint }, Language::En) => (
            format!("Log in to {source} again"),
            Some((*hint).to_owned()),
            styles.muted_text,
        ),
        (Connecting::Rejected { error }, Language::ZhCn) => (
            format!("{source}拒绝了这个 Tick 的请求"),
            Some((*error).to_owned()),
            styles.error,
        ),
        (Connecting::Rejected { error }, Language::En) => (
            format!("{source} turned this Tick's request down"),
            Some((*error).to_owned()),
            styles.error,
        ),
    };
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.buffer_mut().set_style(area, styles.panel_base());
            let inner = table_content_area(area);
            write_text(
                frame.buffer_mut(),
                inner.x,
                area.y,
                &status,
                inner.width as usize,
                settings.ambiguous_width,
                styles.panel_base().patch(styles.title),
            );
            if let Some(detail) = detail {
                frame.render_widget(
                    Paragraph::new(detail)
                        .wrap(Wrap { trim: false })
                        .style(styles.panel_base().patch(detail_style)),
                    inner,
                );
            }
        })
        .map_err(|error| Error::usage(error.to_string()))?;
    Ok(())
}

/// The status hint for the keys that quit a standalone Tick.
pub const QUIT_HINT: &str = "q Quit";

/// `hints` less a trailing [`QUIT_HINT`] when the keys do not quit the Tick.
pub fn standalone_hints<'a, 'b>(
    hints: &'a [tickloop_ui::format::Hint<'b>],
) -> &'a [tickloop_ui::format::Hint<'b>] {
    match hints.split_last() {
        Some((last, rest)) if last.full == QUIT_HINT && !quits_on_keys() => rest,
        _ => hints,
    }
}

/// The key line under a list of articles or posts; a narrow Panel drops the keys the status row
/// already offers.
pub fn reading_list_keys(columns: usize, settings: &Settings) -> String {
    use tickloop_ui::format::{Hint, fit_hints};
    let hints = [
        Hint::whole("Enter Read"),
        Hint::whole("? Help"),
        Hint::whole(QUIT_HINT),
    ];
    fit_hints(
        standalone_hints(&hints),
        "  ·  ",
        columns,
        settings.ambiguous_width,
    )
}

/// The key line under an open article or post.
pub fn reading_keys(columns: usize, settings: &Settings) -> String {
    use tickloop_ui::format::{Hint, fit_hints};
    let hints = [
        Hint::whole("Backspace Back"),
        Hint::whole("j/k Scroll"),
        Hint::whole(QUIT_HINT),
    ];
    fit_hints(
        standalone_hints(&hints),
        " · ",
        columns,
        settings.ambiguous_width,
    )
}

pub fn table_content_area(area: Rect) -> Rect {
    let horizontal_padding = if area.width >= WIDE_PANEL_COLUMNS {
        2
    } else {
        1
    };
    let header_y = area.y + if area.height >= TALL_PANEL_ROWS { 2 } else { 1 };
    Rect::new(
        area.x + horizontal_padding,
        header_y,
        area.width.saturating_sub(horizontal_padding * 2),
        area.bottom().saturating_sub(header_y + 1),
    )
}

pub fn draw_table(
    frame: &mut Frame<'_>,
    title: &str,
    table: &TableData,
    priority: &[usize],
    state: &mut ViewState,
    settings: &Settings,
) {
    let area = frame.area();
    let styles = settings.styles();
    frame.buffer_mut().set_style(area, styles.panel_base());
    if area.width < 8 || area.height < 4 {
        frame.render_widget(Paragraph::new("Too small").style(styles.muted_text), area);
        return;
    }
    let inner = table_content_area(area);
    write_text(
        frame.buffer_mut(),
        inner.x,
        area.y,
        title,
        inner.width as usize,
        settings.ambiguous_width,
        styles.panel_base().patch(styles.title),
    );
    let rows = table.display_rows(settings);
    let headers: Vec<&str> = table
        .headers
        .iter()
        .map(|header| crate::output::short_header(header))
        .collect();
    let mut widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| width(&row[index].text, settings.ambiguous_width))
                .max()
                .unwrap_or(0)
                .max(width(header, settings.ambiguous_width))
                .clamp(4, 28)
        })
        .collect();
    // A Panel too narrow for the two leading columns side by side truncates them, the first
    // keeping at least half the room, rather than drop the second: a label is no use alone.
    if let [first, second, ..] = priority
        && *first < widths.len()
        && *second < widths.len()
    {
        let room = (inner.width as usize).saturating_sub(2);
        if widths[*first] + widths[*second] > room {
            widths[*first] = widths[*first].min(room.saturating_sub(widths[*second]).max(room / 2));
            widths[*second] = widths[*second].min(room.saturating_sub(widths[*first]));
        }
    }
    let mut remaining = inner.width as usize;
    let mut columns = Vec::new();
    for &index in priority {
        let is_empty_error = table.headers.get(index) == Some(&"Error")
            && table
                .rows
                .iter()
                .all(|row| row.get(index).is_none_or(serde_json::Value::is_null));
        let spacing = if columns.is_empty() { 0 } else { 2 };
        if index < widths.len() && !is_empty_error && widths[index] + spacing <= remaining {
            columns.push(index);
            remaining -= widths[index] + spacing;
        }
    }
    if columns.is_empty() {
        columns.push(0);
    }
    columns.sort_unstable();
    let visible = inner.height.saturating_sub(1) as usize;
    if state.selected < state.offset {
        state.offset = state.selected;
    }
    if state.selected >= state.offset + visible {
        state.offset = state.selected.saturating_sub(visible.saturating_sub(1));
    }
    let mut x = inner.x;
    for &column in &columns {
        let text = pad(
            headers[column],
            widths[column],
            settings.ambiguous_width,
            column >= 2,
        );
        write_text(
            frame.buffer_mut(),
            x,
            inner.y,
            &text,
            inner.right().saturating_sub(x) as usize,
            settings.ambiguous_width,
            styles.panel_base().patch(styles.muted_text),
        );
        x = x.saturating_add(widths[column] as u16 + 2);
    }
    for (row_index, row) in rows.iter().enumerate().skip(state.offset).take(visible) {
        let y = inner.y + 1 + (row_index - state.offset) as u16;
        if row_index == state.selected {
            state.focus_row = Some(y);
            frame.buffer_mut().set_style(
                Rect::new(inner.x, y, inner.width, 1),
                styles.panel_base().patch(styles.selection),
            );
            write_text(
                frame.buffer_mut(),
                area.x,
                y,
                ">",
                1,
                settings.ambiguous_width,
                styles.panel_base().patch(styles.selection),
            );
        }
        let mut x = inner.x;
        for &column in &columns {
            let cell = &row[column];
            let text = pad(
                &cell.text,
                widths[column],
                settings.ambiguous_width,
                column >= 2,
            );
            let mut style = styles.panel_base().patch(styles.price(cell.direction));
            if cell.is_error {
                style = style.patch(styles.error);
            }
            if row_index == state.selected {
                style = style.patch(styles.selection);
            }
            write_text(
                frame.buffer_mut(),
                x,
                y,
                &text,
                inner.right().saturating_sub(x) as usize,
                settings.ambiguous_width,
                style,
            );
            x = x.saturating_add(widths[column] as u16 + 2);
        }
    }
    if rows.is_empty() {
        write_text(
            frame.buffer_mut(),
            inner.x,
            inner.y + 1,
            "No data",
            inner.width as usize,
            settings.ambiguous_width,
            styles.panel_base().patch(styles.muted_text),
        );
    }
}
