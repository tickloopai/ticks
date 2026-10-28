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
const HELP_TITLE: &str = " Keys · ? / Esc / Backspace close ";
const NARROW_HELP_TITLE: &str = " Keys · ? close ";
/// Under a help overlay too short for every key.
const HELP_SCROLL_HINT: &str = " j/k more ";
/// The Tick's frame takes a row above and below its view and a column either side.
pub const FRAME_WIDTH: u16 = 1;
/// The corners, a space either side of the title, and a dash after it.
const TITLE_MARGIN: usize = 5;
/// The key a crowded bottom edge keeps, which lists the others.
const HELP_KEY: &str = "?";
/// The narrowest frame that still carries a title.
const MINIMUM_FRAME_COLUMNS: u16 = 8;
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
    /// The title the view gives this frame, shown on the top edge of the Tick's frame; views
    /// set it while they draw.
    pub title: String,
    /// How wide the Tick's frame is this frame.
    pub frame_columns: u16,
    /// What the bottom edge of the Tick's frame carries this frame, which a view drawn without
    /// the frame shows itself.
    pub edge: EdgeLabels,
    /// Whether the user is in the Tick's card, where its keys are worth showing: a card the user
    /// only looks at shows no key hints, though the Tick still answers every key it receives.
    pub focused: bool,
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
            title: String::new(),
            frame_columns: 0,
            edge: EdgeLabels::default(),
            focused: !in_instance() || crate::terminal::has_focus(),
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
    /// Whether the runtime draws the Tick's frame around the view. A view of bordered cards
    /// takes the whole screen instead and shows the frame's bottom edge on its last card.
    fn framed(&self) -> bool {
        true
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
            frame.area(),
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
                        Some(Ok(event @ (Event::FocusGained | Event::FocusLost))) => {
                            state.focused = event == Event::FocusGained;
                            terminal.clear().map_err(|error| Error::usage(error.to_string()))?;
                            shown = None;
                        }
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

/// The whole screen of `size` the view shows now, with its cursor: the view inside a frame that
/// carries its title above and its keys and errors below, scrolled over the
/// canvas of the Tick's minimum when the frame is smaller, and the help overlay.
fn compose<V: LiveView>(
    view: &V,
    size: Size,
    state: &mut ViewState,
    viewport: &mut ScrollViewport,
    settings: &Settings,
) -> (Buffer, Option<Position>) {
    // A focus report a wait dropped still reached the terminal's record of the focus.
    if in_instance() {
        state.focused = crate::terminal::has_focus();
    }
    let settings = &Settings {
        key_hints: settings.key_hints && state.focused,
        ..settings.clone()
    };
    let framed = view.framed();
    let frame_width = if framed { FRAME_WIDTH } else { 0 };
    let content = Rect::new(
        frame_width,
        frame_width,
        size.width.saturating_sub(frame_width * 2),
        size.height.saturating_sub(frame_width * 2),
    );
    let minimum = view.minimum_panel_size();
    viewport.set_layout(ViewportLayout::of(
        content,
        Size::new(
            minimum.columns.saturating_sub(frame_width * 2),
            minimum.rows.saturating_sub(frame_width * 2),
        ),
    ));
    state.edge = edge_labels(view, state, size.width, settings);
    state.frame_columns = size.width;
    let layout = viewport.layout();
    // ratatui hands out a Frame only through a Terminal, so the view draws into an in-memory one.
    let Ok(mut canvas) = Terminal::new(CanvasBackend::new(
        layout.map_or(content.as_size(), |layout| layout.canvas),
    ));
    // The frame's own buffer, not what the backend was sent: a link's row is one cell that spans
    // the cells after it, which the diff skips and only the frame keeps marked.
    let mut drawn = Buffer::empty(Rect::default());
    let Ok(_) = canvas.draw(|frame| {
        draw_view(view, frame, state, settings);
        drawn = frame.buffer_mut().clone();
    });
    let drawn_cursor = canvas.backend().cursor();
    if layout.is_some() {
        viewport.follow(
            drawn_cursor.map(|cursor| cursor.x).or(state.focus_column),
            drawn_cursor.map(|cursor| cursor.y).or(state.focus_row),
        );
    }
    let mut composed = Buffer::empty(Rect::default());
    let Ok(mut screen) = Terminal::new(CanvasBackend::new(size));
    let Ok(_) = screen.draw(|frame| {
        let styles = settings.styles();
        let area = frame.area();
        frame.buffer_mut().set_style(area, styles.canvas);
        if layout.is_some() {
            viewport.render(&drawn, frame.buffer_mut(), styles, settings.ambiguous_width);
            if let Some(cursor) = drawn_cursor.and_then(|cursor| viewport.on_panel(cursor)) {
                frame.set_cursor_position(cursor);
            }
        } else {
            for position in drawn.area.positions() {
                let target = Position::new(position.x + content.x, position.y + content.y);
                if content.contains(target) {
                    frame.buffer_mut()[target] = drawn[position].clone();
                }
            }
            if let Some(cursor) = drawn_cursor {
                frame
                    .set_cursor_position(Position::new(cursor.x + content.x, cursor.y + content.y));
            }
        }
        if framed {
            draw_frame(frame, state, settings);
        }
        if state.help {
            let area = content;
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
                    .min(area.width),
            );
            let lines = u16::try_from(help.line_count(width)).unwrap_or(u16::MAX);
            // A short Panel gives the overlay every row inside the frame and scrolls the keys.
            let height = (area.height * 3 / 4).max(lines.min(area.height));
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
        composed = frame.buffer_mut().clone();
    });
    (composed, screen.backend().cursor())
}

/// How wide a title the Tick's frame shows whole this frame; a frame smaller than the Tick's
/// minimum scrolls over a wider view, so the view asks rather than measure itself.
pub fn frame_title_columns(state: &ViewState) -> usize {
    usize::from(state.frame_columns).saturating_sub(TITLE_MARGIN)
}

/// Draws the view into the frame, which is the inside of the Tick's frame or, for one smaller than
/// the Tick's minimum, the canvas it scrolls over.
fn draw_view<V: LiveView>(
    view: &V,
    frame: &mut Frame<'_>,
    state: &mut ViewState,
    settings: &Settings,
) {
    let area = frame.area();
    frame.buffer_mut().set_style(area, settings.styles().canvas);
    state.focus_column = None;
    state.focus_row = None;
    state.title.clear();
    view.draw(frame, state, settings);
}

/// The Tick's frame around the whole screen: the view's title on the left of the top edge and
/// its bottom edge's labels below, so neither takes a row of its own.
fn draw_frame(frame: &mut Frame<'_>, state: &ViewState, settings: &Settings) {
    let area = frame.area();
    let styles = settings.styles();
    let edge = styles.panel_base().patch(styles.border);
    frame.render_widget(
        Block::default().borders(Borders::ALL).border_style(edge),
        area,
    );
    if area.width < MINIMUM_FRAME_COLUMNS || area.height < 2 {
        return;
    }
    // The Monitor's row tells when the Tick updates, so the top edge carries only the title.
    let title_columns = usize::from(area.width).saturating_sub(TITLE_MARGIN);
    let title =
        tickloop_ui::format::truncate(&state.title, title_columns, settings.ambiguous_width);
    if !title.is_empty() {
        write_text(
            frame.buffer_mut(),
            area.x + 1,
            area.y,
            &format!(" {title} "),
            title_columns + 2,
            settings.ambiguous_width,
            edge,
        );
    }
    let bottom = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    draw_bottom_edge(frame, bottom, "", &state.edge, settings);
}

/// What the bottom edge of the Tick's frame carries: the keys on its right, and the last problem
/// with the data on its left.
#[derive(Clone, Debug, Default)]
pub struct EdgeLabels {
    pub keys: String,
    pub problem: Option<String>,
}

fn edge_labels<V: LiveView>(
    view: &V,
    state: &ViewState,
    columns: u16,
    settings: &Settings,
) -> EdgeLabels {
    let keys = if !settings.key_hints {
        ""
    } else if view.captures_text() && columns >= 40 {
        "Enter Apply  Ctrl+C Cancel"
    } else if view.captures_text() {
        "Ctrl+C Cancel"
    } else if !quits_on_keys() {
        "? Help"
    } else if columns >= 40 {
        "? Help  q Quit"
    } else {
        "?  q Quit"
    };
    let problem = match (&state.last_error, state.connection) {
        (Some(error), ConnectionState::Connected) => Some(error.clone()),
        (Some(error), _) => Some(format!("{} · {error}", state.availability())),
        (None, ConnectionState::Connected) => None,
        (None, _) => Some(state.availability().to_owned()),
    };
    EdgeLabels {
        keys: keys.to_owned(),
        problem,
    }
}

/// Writes `labels` on `edge`, a bottom border row with its corners: the keys on the right, and
/// `lead` then the problem on the left in the room the keys leave.
pub fn draw_bottom_edge(
    frame: &mut Frame<'_>,
    edge: Rect,
    lead: &str,
    labels: &EdgeLabels,
    settings: &Settings,
) {
    let styles = settings.styles();
    let border = styles.panel_base().patch(styles.border);
    let spaced = |text: &str| {
        if text.is_empty() {
            String::new()
        } else {
            format!(" {text} ")
        }
    };
    let left = [Some(lead), labels.problem.as_deref()]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    // What the left says outranks the keys, which give way to the help key alone.
    let mut keys = spaced(&labels.keys);
    if !keys.is_empty()
        && width(&spaced(&left), settings.ambiguous_width)
            + width(&keys, settings.ambiguous_width)
            + 3
            > usize::from(edge.width)
    {
        keys = spaced(HELP_KEY);
    }
    let keys_width = width(&keys, settings.ambiguous_width) as u16;
    write_text(
        frame.buffer_mut(),
        edge.right().saturating_sub(2 + keys_width),
        edge.y,
        &keys,
        usize::from(keys_width),
        settings.ambiguous_width,
        border.patch(styles.muted_text),
    );
    // The corners, a dash either side and the spaces around the text.
    let room = usize::from(edge.width.saturating_sub(keys_width + 5));
    let left = tickloop_ui::format::truncate(&left, room, settings.ambiguous_width);
    write_text(
        frame.buffer_mut(),
        edge.x + 1,
        edge.y,
        &spaced(&left),
        room + 2,
        settings.ambiguous_width,
        border.patch(if labels.problem.is_some() {
            styles.error
        } else {
            styles.muted_text
        }),
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
            // The same whole frame as a view's, with the status as its first line inside.
            let edge = styles.panel_base().patch(styles.border);
            let block = Block::default().borders(Borders::ALL).border_style(edge);
            let inner = table_content_area(block.inner(area));
            frame.render_widget(block, area);
            write_text(
                frame.buffer_mut(),
                inner.x,
                inner.y,
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
                    Rect {
                        y: inner.y + 1,
                        height: inner.height.saturating_sub(1),
                        ..inner
                    },
                );
            }
        })
        .map_err(|error| Error::usage(error.to_string()))?;
    Ok(())
}

/// `hints` joined to fit `columns` as [`tickloop_ui::format::fit_hints`] joins them, or nothing
/// on a screen that shows no key hints.
pub fn key_hints(
    hints: &[tickloop_ui::format::Hint<'_>],
    separator: &str,
    columns: usize,
    settings: &Settings,
) -> String {
    if !settings.key_hints {
        return String::new();
    }
    tickloop_ui::format::fit_hints(hints, separator, columns, settings.ambiguous_width)
}

/// The row a view keeps for its line of key hints whether or not the screen shows them, so
/// entering or leaving the card does not move what the view shows.
pub const KEY_LINE_ROWS: u16 = 1;

/// The key line under a list of articles or posts; the frame offers Help and Quit.
pub fn reading_list_keys(columns: usize, settings: &Settings) -> String {
    use tickloop_ui::format::Hint;
    key_hints(&[Hint::whole("Enter Read")], "  ·  ", columns, settings)
}

/// The key line under an open article or post.
pub fn reading_keys(columns: usize, settings: &Settings) -> String {
    use tickloop_ui::format::Hint;
    key_hints(
        &[Hint::whole("Backspace Back"), Hint::whole("j/k Scroll")],
        " · ",
        columns,
        settings,
    )
}

/// Where a view inside the Tick's frame lays out its rows: all of it, a column in from either
/// edge of a wide frame. The frame carries the title, so no row goes to one.
pub fn table_content_area(area: Rect) -> Rect {
    let horizontal_padding = u16::from(area.width >= WIDE_PANEL_COLUMNS);
    Rect::new(
        area.x + horizontal_padding,
        area.y,
        area.width.saturating_sub(horizontal_padding * 2),
        area.height,
    )
}

/// Draws `table` in `area`, its columns by `priority` as the width allows, and gives the Tick's
/// frame `title`.
pub fn draw_table(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    table: &TableData,
    priority: &[usize],
    state: &mut ViewState,
    settings: &Settings,
) {
    let styles = settings.styles();
    frame.buffer_mut().set_style(area, styles.panel_base());
    state.title = title.to_owned();
    if area.width < 8 || area.height < 2 {
        frame.render_widget(Paragraph::new("Too small").style(styles.muted_text), area);
        return;
    }
    let inner = table_content_area(area);
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
            // The selection's style marks it; a wide frame also has the column for a pointer.
            let pointer = if inner.x > area.x { ">" } else { "" };
            write_text(
                frame.buffer_mut(),
                area.x,
                y,
                pointer,
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
