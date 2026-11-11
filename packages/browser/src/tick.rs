//! The Browser Tick its host launches, one for each session: it connects to the native host's local
//! socket as its session, offers the extension's tools and relays their calls, and shows the
//! session's tabs in its card. While no native host serves, it waits and tries again.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt};
use ratatui::{Terminal, backend::Backend, layout::Size};
use serde::Serialize;
use serde_json::{Value, json};
use tick_market::{
    config::Settings,
    runtime,
    terminal::{TerminalSession, VisibleTerminalSession},
    viewport::{CardRows, KeyBubbling},
};
use tick_protocol::{INVALID_PARAMS, TOOL_FAILED};
use tick_provider::error::{Error, Result};
use tick_sdk::stream::{InboundFrame, RpcError, TickStream, ToolRequest};
use tokio::{
    io::AsyncWriteExt,
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::mpsc,
    time::Instant,
};

use crate::{
    DEVELOPMENT_INSTALL, EXTENSION_IDS, EXTENSION_STORE_URL, HOST_NAME,
    catalog::{Catalog, STATUS_TOOL},
    frame, registration,
    relay::{self, BrowserState, FromHost, ToHost},
    socket,
    view::{self, View},
};

/// How long a relayed call waits for the extension: inside the host's 60 seconds for a `tick/call`,
/// so the Agent learns what happened from the Tick rather than from the host giving up.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(55);
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// Messages from the native host waiting for the Tick's loop.
const INBOUND_CAPACITY: usize = 64;
/// How long the Tick waits before connecting to the native host again: connecting to a local
/// socket costs next to nothing, so the Tick learns soon that a browser opened.
pub const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
/// How long a refused Tick waits before it tries again, so a refusal is not repeated every second.
pub const REFUSED_RETRY: Duration = Duration::from_secs(10);
/// How many of the latest relayed calls the card shows.
const RECENT_STEPS: usize = 3;
/// The room a call leaves for the session the native host adds to it.
const SESSION_ROOM: usize = 1024;
/// How long the Tick waits for an extension before its state says the wait has run long, so an
/// Agent waiting for the connection hears that and can ask the user. The extension tries again at
/// most 30 seconds apart.
pub const LONG_WAIT: Duration = Duration::from_secs(60);
/// What the state says once no extension has connected for `LONG_WAIT`.
const LONG_WAIT_NOTE: &str = "No extension has connected for over a minute: Chrome, Chromium or Edge may not be open, the Tickloop extension may not be installed or enabled in it, or it is between tries, which come at most 30 seconds apart; opening the extension's side panel tries at once.";

/// The optional request in which Tickloop tells a Tick the title of the Chat that opened it, after
/// initialization and whenever it changes (Tick protocol, Section 9). A host that does not send it
/// leaves the session named by its directory.
const CHAT_METHOD: &str = "tick/chat";

/// The answer to one `tick/call`, by its request id.
pub type Answer = (Value, std::result::Result<Value, RpcError>);

/// The session a Tick opens: one Tick Instance, named by Tickhost's instance identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub title: String,
}

impl Session {
    /// The session of a Tick with the instance identity `instance` (`TICKLOOP_INSTANCE_ID`), run in
    /// `directory` as process `pid`. A Tick run in a terminal has no instance identity and is named
    /// by its process. The title is the directory's name, or else the identity's short form; the
    /// title of the Chat that opened the Tick comes later, from `tick/chat`.
    pub fn new(instance: Option<&str>, directory: Option<&Path>, pid: u32) -> Self {
        let id = match instance
            .map(str::trim)
            .filter(|instance| !instance.is_empty())
        {
            Some(instance) => relay::shorten(instance, relay::MAX_SESSION_CHARS),
            None => format!("terminal:{pid}"),
        };
        let title = directory
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                let short = id.split_once(':').map_or(id.as_str(), |(_, rest)| rest);
                short.chars().take(8).collect()
            });
        Self {
            id,
            title: relay::shorten(&title, relay::MAX_SESSION_TITLE_CHARS),
        }
    }

    /// This process's session.
    pub fn current() -> Self {
        Self::new(
            std::env::var("TICKLOOP_INSTANCE_ID").ok().as_deref(),
            std::env::current_dir().ok().as_deref(),
            std::process::id(),
        )
    }
}

/// The Tick's session, the socket it connects to and how the native messaging host is registered.
#[derive(Clone, Debug)]
pub struct Setup {
    pub session: Session,
    pub socket: PathBuf,
    pub registration: std::result::Result<Vec<registration::Entry>, String>,
}

/// Where the extension stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    /// No extension is connected.
    Waiting,
    /// An extension connected and has not said `hello` yet.
    Connecting,
    Connected {
        version: String,
    },
    /// The extension's `hello` was refused for `message`.
    Refused {
        message: String,
    },
}

impl Connection {
    fn name(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Connecting => "connecting",
            Self::Connected { .. } => "connected",
            Self::Refused { .. } => "refused",
        }
    }
}

/// The last call the Tick relayed and how it ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    /// The relayed call's id, so the step it began is the one its end updates.
    #[serde(skip)]
    pub call: u64,
    pub tool: String,
    pub status: StepStatus,
    pub at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Running,
    Done,
    Failed,
    /// The extension went away or did not answer after a call that may change the browser.
    Unknown,
}

/// A `tick/call` relayed to the extension and not answered yet.
struct InFlight {
    /// The id the call went to the extension with.
    id: u64,
    request: Value,
    tool: String,
    mutates: bool,
    deadline: Instant,
}

/// The connection to the native host.
struct Link {
    generation: u64,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
}

/// What the Tick knows of the browser and the calls it relays. The loop hands it what arrives and
/// sends the answers it returns.
pub struct Browser {
    catalog: Catalog,
    setup: Setup,
    link: Option<Link>,
    connection: Connection,
    state: BrowserState,
    calls: HashMap<u64, InFlight>,
    next_call: u64,
    /// The latest relayed calls, newest first, at most `RECENT_STEPS`.
    steps: std::collections::VecDeque<Step>,
    /// When to connect to the native host again, while not connected.
    retry_at: Option<Instant>,
    /// Since when no extension has served the Tick, from the start or from the last one going
    /// away; none while one does.
    waiting_since: Option<Instant>,
    /// No extension served the Tick for `LONG_WAIT` since `waiting_since`.
    long_wait: bool,
    /// The title of the Chat that opened the Tick, once Tickloop told it one.
    chat_title: Option<String>,
}

impl Browser {
    pub fn new(catalog: Catalog, setup: Setup) -> Self {
        let now = Instant::now();
        Self {
            catalog,
            setup,
            link: None,
            connection: Connection::Waiting,
            state: BrowserState::default(),
            calls: HashMap::new(),
            next_call: 1,
            steps: std::collections::VecDeque::new(),
            retry_at: Some(now),
            waiting_since: Some(now),
            long_wait: false,
            chat_title: None,
        }
    }

    /// The title of the Chat that opened the Tick, as the extension names the session by.
    pub fn chat_title(&self) -> Option<&str> {
        self.chat_title.as_deref()
    }

    /// Tickloop told the Tick the title of the Chat that opened it: the native host hears a new
    /// one at once when the session is open, and every `open` carries it from now on.
    pub fn chat(&mut self, title: &str) {
        let Some(title) = relay::chat_title(title) else {
            return;
        };
        if self.chat_title.as_deref() == Some(title.as_str()) {
            return;
        }
        self.send(&ToHost::ChatTitle { chat_title: &title });
        self.chat_title = Some(title);
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn state(&self) -> &BrowserState {
        &self.state
    }

    pub fn last_step(&self) -> Option<&Step> {
        self.steps.front()
    }

    /// The latest relayed calls, newest first.
    pub fn recent_steps(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter()
    }

    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    pub fn has_calls(&self) -> bool {
        !self.calls.is_empty()
    }

    /// The connection the Tick holds now, by its generation.
    pub fn generation(&self) -> Option<u64> {
        self.link.as_ref().map(|link| link.generation)
    }

    /// When to try the native host again: while no connection is held.
    pub fn next_connect(&self) -> Option<Instant> {
        self.retry_at.filter(|_| self.link.is_none())
    }

    /// The native host could not be reached at `now`: none serves, so the Tick waits and tries again
    /// shortly. A refusal it showed gives way, since the host that refused is gone.
    pub fn connect_failed(&mut self, now: Instant) {
        self.retry_at = Some(now + RECONNECT_INTERVAL);
        if matches!(self.connection, Connection::Refused { .. }) {
            self.connection = Connection::Waiting;
        }
    }

    /// Takes the connection to the native host `generation`, which `outbound` writes to, and opens
    /// the Tick's session on it. A refusal shown stays until the host says `hello`, so trying again
    /// does not flicker the state.
    pub fn link(&mut self, generation: u64, outbound: mpsc::UnboundedSender<Vec<u8>>) {
        self.link = Some(Link {
            generation,
            outbound,
        });
        self.retry_at = None;
        if !matches!(self.connection, Connection::Refused { .. }) {
            self.connection = Connection::Connecting;
        }
        let session = &self.setup.session;
        self.send(&ToHost::Open {
            protocol: relay::PROTOCOL,
            session: &session.id,
            title: &session.title,
            chat_title: self.chat_title.as_deref(),
        });
    }

    /// The connection `generation` ended at `now`: the extension's tools go, every call it had not
    /// answered fails, and the Tick connects again shortly.
    pub fn unlink(&mut self, generation: u64, now: Instant) -> Vec<Answer> {
        if self.generation() != Some(generation) {
            return Vec::new();
        }
        self.link = None;
        self.retry_at = Some(now + RECONNECT_INTERVAL);
        self.catalog.withdraw();
        // The wait starts again when an extension that served goes; a connection that never got
        // that far leaves it running.
        if matches!(self.connection, Connection::Connected { .. }) {
            self.waiting_since = Some(now);
            self.long_wait = false;
        }
        if !matches!(self.connection, Connection::Refused { .. }) {
            self.connection = Connection::Waiting;
        }
        self.state = BrowserState::default();
        let calls: Vec<_> = self.calls.drain().map(|(_, call)| call).collect();
        calls
            .into_iter()
            .map(|call| self.unanswered(call, "disconnected", "disconnected"))
            .collect()
    }

    /// Handles one message from the native host on the connection `generation`.
    pub fn receive(&mut self, generation: u64, message: &[u8], now: Instant) -> Vec<Answer> {
        if self.generation() != Some(generation) {
            return Vec::new();
        }
        match relay::parse_from_host(message) {
            Ok(None) => Vec::new(),
            Ok(Some(FromHost::Hello(hello))) => {
                let version = hello.version();
                if let Err(error) = self.catalog.serve(hello.catalog) {
                    return self.refuse(format!("the extension's catalog: {error}"), now);
                }
                if !matches!(self.connection, Connection::Connected { .. }) {
                    self.state = BrowserState::default();
                }
                self.connection = Connection::Connected { version };
                self.waiting_since = None;
                self.long_wait = false;
                Vec::new()
            }
            Ok(Some(FromHost::Update(state))) => {
                self.state = state.bounded();
                Vec::new()
            }
            Ok(Some(FromHost::Result(result))) => {
                let Some(call) = self.calls.remove(&result.id) else {
                    return Vec::new();
                };
                let answer = if result.ok {
                    Ok(result.result)
                } else {
                    let error = result.error.unwrap_or(relay::CallError {
                        message: format!("{} failed without saying why", call.tool),
                        code: None,
                    });
                    let failure = RpcError::new(TOOL_FAILED, error.message);
                    Err(match error.code {
                        Some(code) => failure.with_data(json!({"code": code})),
                        None => failure,
                    })
                };
                self.step(
                    call.id,
                    &call.tool,
                    if answer.is_ok() {
                        StepStatus::Done
                    } else {
                        StepStatus::Failed
                    },
                );
                vec![(call.request, answer)]
            }
            Ok(Some(FromHost::Refused { message, .. })) => self.refuse(message, now),
            Err(error) => self.refuse(format!("the native host sent {error}"), now),
        }
    }

    /// Answers a `tick/call` whose revision the stream already checked: the `status` query here,
    /// any extension tool by relaying it. `None` when the extension will answer.
    pub fn call(&mut self, request: ToolRequest, now: Instant) -> Option<Answer> {
        let Some(tool) = self.catalog.extension_tool(&request.tool) else {
            let answer = if request.tool == STATUS_TOOL && !self.catalog.serves_extension() {
                Ok(self.status())
            } else {
                Err(RpcError::unknown_tool(&request.tool))
            };
            return Some((request.id, answer));
        };
        let id = self.next_call;
        // The call is written now, so all of the Tick's time for it is left. The deadline on the
        // wall clock, which the browser on this machine shares, also covers the time the call
        // spends on its way to the extension, which can be seconds while the browser is starved
        // of CPU.
        let message = ToHost::Call {
            id,
            tool: &tool.name,
            args: &request.args,
            timeout_ms: u64::try_from(CALL_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            deadline_ms: unix_ms(SystemTime::now() + CALL_TIMEOUT),
        }
        .to_vec();
        // The host adds the session before it passes the call on, within the extension's limit.
        let frame = match frame::encode(&message, frame::TO_EXTENSION_MAX_BYTES - SESSION_ROOM) {
            Ok(frame) => frame,
            Err(error) => {
                return Some((
                    request.id,
                    Err(RpcError::new(INVALID_PARAMS, error.to_string())),
                ));
            }
        };
        let sent = self
            .link
            .as_ref()
            .is_some_and(|link| link.outbound.send(frame).is_ok());
        if !sent {
            return Some((
                request.id,
                Err(RpcError::new(
                    TOOL_FAILED,
                    "the browser extension is not connected; nothing was sent",
                )
                .with_data(json!({"code": "disconnected"}))),
            ));
        }
        self.next_call += 1;
        self.step(id, &tool.name, StepStatus::Running);
        self.calls.insert(
            id,
            InFlight {
                id,
                request: request.id,
                tool: tool.name,
                mutates: tool.mutates,
                deadline: now + CALL_TIMEOUT,
            },
        );
        None
    }

    /// When the next relayed call runs out of time, or the wait for an extension runs long.
    pub fn next_deadline(&self) -> Option<Instant> {
        let long_wait = self
            .waiting_since
            .filter(|_| !self.long_wait)
            .map(|since| since + LONG_WAIT);
        self.calls
            .values()
            .map(|call| call.deadline)
            .chain(long_wait)
            .min()
    }

    /// Fails every relayed call that ran out of time by `now`, and notes a wait for an extension
    /// that has run long by then.
    pub fn expire(&mut self, now: Instant) -> Vec<Answer> {
        if let Some(since) = self.waiting_since
            && now >= since + LONG_WAIT
        {
            self.long_wait = true;
        }
        let expired: Vec<u64> = self
            .calls
            .iter()
            .filter(|(_, call)| call.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        let how = format!("did not answer within {} seconds", CALL_TIMEOUT.as_secs());
        let calls: Vec<_> = expired
            .into_iter()
            .filter_map(|id| self.calls.remove(&id))
            .collect();
        calls
            .into_iter()
            .map(|call| match self.approval_for(&call.tool) {
                Some((approval, link)) => self.awaiting(call, &approval, link.as_deref()),
                None => self.unanswered(call, &how, "timed_out"),
            })
            .collect()
    }

    /// The summary of what waits for the user's approval before a call to `tool` can go on, with
    /// the link that opens it when it has one.
    fn approval_for(&self, tool: &str) -> Option<(String, Option<String>)> {
        self.state
            .pending
            .iter()
            .find(|item| item.kind == "approval" && item.tool.as_deref() == Some(tool))
            .map(|item| (item.summary.clone(), item.open_url.clone()))
    }

    /// The answer to a call that ran out of time while an approval for its tool waited for the
    /// user: it has not run, and goes on only once the user approves. The approval's link, when it
    /// has one, is for the Agent to give the user.
    fn awaiting(&mut self, call: InFlight, approval: &str, link: Option<&str>) -> Answer {
        let mut message = format!(
            "Waiting for the user to approve \"{approval}\" in the browser's side panel before {tool} can go on. Tell the user in one sentence what to approve there and end your turn, subscribed to this Tick (subscribe with TickManage first if this Chat is not); when a state update shows it allowed, call {tool} again, and when it shows it denied, stop",
            tool = call.tool
        );
        if let Some(link) = link {
            message.push_str(&format!(
                ". The user can open {link} to approve it; give them this link"
            ));
        }
        self.step(call.id, &call.tool, StepStatus::Failed);
        (
            call.request,
            Err(RpcError::new(TOOL_FAILED, message)
                .with_data(json!({"code": "waiting_for_approval"}))),
        )
    }

    /// Asks the extension to stop what it is doing for this session; false when none is connected.
    pub fn stop(&mut self) -> bool {
        matches!(self.connection, Connection::Connected { .. }) && self.send(&ToHost::Stop)
    }

    /// The Tick's state: the connection, and once the extension is connected its version, its tabs
    /// and what waits for the user; the last relayed call throughout.
    pub fn data(&self) -> Value {
        let mut data = json!({"connection": self.connection.name()});
        match &self.connection {
            Connection::Waiting if self.long_wait => {
                data["long_wait"] = json!(true);
                data["hint"] = json!(relay::shorten(
                    &format!("{LONG_WAIT_NOTE} {}", hint(&self.setup)),
                    2 * relay::MAX_MESSAGE_CHARS
                ));
            }
            Connection::Waiting => data["hint"] = json!(hint(&self.setup)),
            Connection::Connecting => (),
            Connection::Connected { version } => {
                data["extension_version"] = json!(version);
                data["tabs"] = json!(self.state.tabs);
                data["pending"] = json!(self.state.pending);
                if let Some(event) = &self.state.last_event {
                    data["last_event"] = json!(event);
                }
                if let Some(label) = &self.state.label {
                    data["label"] = json!(label);
                }
                if let Some(color) = &self.state.group_color {
                    data["group_color"] = json!(color);
                }
            }
            Connection::Refused { message } => data["error"] = json!(message),
        }
        if matches!(self.connection, Connection::Waiting)
            && let Some(url) = EXTENSION_STORE_URL
        {
            data["install_url"] = json!(url);
        }
        if let Some(step) = self.last_step() {
            data["last_step"] = json!(step);
        }
        data
    }

    /// What the `status` query answers.
    pub fn status(&self) -> Value {
        let mut status = status_report(&self.setup);
        status["session"] = json!(self.setup.session.id);
        status["title"] = json!(self.setup.session.title);
        status["connection"] = json!(self.connection.name());
        if let Connection::Refused { message } = &self.connection {
            status["error"] = json!(message);
        }
        status
    }

    /// Shows the refusal `message` and lets the connection go: the extension's tools go, the calls
    /// in flight fail, and the Tick tries again later.
    fn refuse(&mut self, message: String, now: Instant) -> Vec<Answer> {
        // The reason can quote what the extension sent, which has no limit of its own.
        let message = relay::shorten(&message, relay::MAX_MESSAGE_CHARS);
        self.link = None;
        self.retry_at = Some(now + REFUSED_RETRY);
        self.catalog.withdraw();
        self.connection = Connection::Refused { message };
        self.state = BrowserState::default();
        let calls: Vec<_> = self.calls.drain().map(|(_, call)| call).collect();
        calls
            .into_iter()
            .map(|call| self.unanswered(call, "was refused", "disconnected"))
            .collect()
    }

    fn send(&self, message: &ToHost<'_>) -> bool {
        let Ok(frame) = frame::encode(&message.to_vec(), frame::TO_EXTENSION_MAX_BYTES) else {
            return false;
        };
        self.link
            .as_ref()
            .is_some_and(|link| link.outbound.send(frame).is_ok())
    }

    /// Records how the call `call` stands: a call already among the recent steps is updated where it
    /// is, a new one goes first.
    fn step(&mut self, call: u64, tool: &str, status: StepStatus) {
        let step = Step {
            call,
            tool: relay::shorten(tool, relay::MAX_TOOL_CHARS),
            status,
            at: tick_protocol::now_rfc3339(),
        };
        match self.steps.iter_mut().find(|recent| recent.call == call) {
            Some(recent) => *recent = step,
            None => {
                self.steps.push_front(step);
                self.steps.truncate(RECENT_STEPS);
            }
        }
    }

    /// The answer to a relayed call the extension never answered: one that may have changed the
    /// browser has an unknown outcome and must not be repeated blindly.
    fn unanswered(&mut self, call: InFlight, how: &str, code: &str) -> Answer {
        let (message, code, status) = if call.mutates {
            (
                format!(
                    "the browser extension {how} after {} was sent; whether it took effect is unknown. Read the page before deciding whether to try again, and do not repeat it blindly",
                    call.tool
                ),
                "outcome_unknown",
                StepStatus::Unknown,
            )
        } else {
            (
                format!(
                    "the browser extension {how} before answering {}; call it again once the browser is connected",
                    call.tool
                ),
                code,
                StepStatus::Failed,
            )
        };
        self.step(call.id, &call.tool, status);
        (
            call.request,
            Err(RpcError::new(TOOL_FAILED, message).with_data(json!({"code": code}))),
        )
    }
}

/// What the user does to connect the browser, by how the registration went, after what opening the
/// Tick wrote in the browsers' directories.
pub fn hint(setup: &Setup) -> String {
    let mut notes: Vec<String> = setup
        .registration
        .iter()
        .flatten()
        .filter_map(|entry| {
            let manifest = entry.manifest.display();
            match &entry.status {
                registration::Status::Written => Some(format!(
                    "Registered the native messaging host for {} at {manifest}.",
                    entry.browser
                )),
                registration::Status::Replaced { previous_path } => Some(format!(
                    "Pointed {}'s native messaging host at {manifest} to this program; it named {} before.",
                    entry.browser,
                    previous_path.as_deref().unwrap_or("no program"),
                )),
                _ => None,
            }
        })
        .collect();
    notes.push(match &setup.registration {
        Err(error) => format!("The native messaging host could not be registered: {error}"),
        Ok(entries) if entries.is_empty() => "No Chrome, Chromium or Edge was found for this user; install one with the Tickloop extension, then open this Tick again.".into(),
        Ok(entries) => match entries.iter().find_map(|entry| match &entry.status {
            registration::Status::Failed { error } => Some((entry.browser, error)),
            _ => None,
        }) {
            Some((browser, error)) => {
                format!("Registering the native messaging host for {browser} failed: {error}")
            }
            None => "Open Chrome, Chromium or Edge with the Tickloop extension; it connects to this Tick by itself, usually within seconds.".into(),
        },
    });
    notes.push(match EXTENSION_STORE_URL {
        Some(url) => format!("If the extension is not installed, install it from {url}."),
        None => format!("If the extension is not installed, load it in {DEVELOPMENT_INSTALL}."),
    });
    relay::shorten(&notes.join(" "), 2 * relay::MAX_MESSAGE_CHARS)
}

/// Where the Tick connects and how the native messaging host is registered, as `status` reports it.
pub fn status_report(setup: &Setup) -> Value {
    json!({
        "socket": setup.socket,
        "host_name": HOST_NAME,
        "extension_ids": EXTENSION_IDS,
        "registration": match &setup.registration {
            Ok(entries) => json!(entries),
            Err(error) => json!({"error": error}),
        },
    })
}

/// Runs the Browser Tick for its session, connecting to the socket in `socket_directory`: in the
/// terminal its host gave it when hosted, or in this terminal.
pub async fn run(settings: &Settings, size: Option<Size>, socket_directory: PathBuf) -> Result<()> {
    let catalog = Catalog::default();
    let mut updates = TickStream::from_process(catalog.clone())?;
    let registration = match (std::env::home_dir(), registration::current_executable()) {
        (Some(home), Ok(executable)) => Ok(registration::register(&home, &executable)),
        (None, _) => Err("this user has no home directory".to_owned()),
        (_, Err(error)) => Err(format!("cannot find this program's path: {error}")),
    };
    let browser = Browser::new(
        catalog,
        Setup {
            session: Session::current(),
            socket: socket_directory.join(socket::SOCKET_NAME),
            registration,
        },
    );
    let result = if updates.is_native() {
        let mut session = VisibleTerminalSession::enter(size)?;
        let input = tick_market::terminal::visible_input_events()?;
        run_live(
            browser,
            &socket_directory,
            settings,
            &mut session.terminal,
            input,
            &mut updates,
        )
        .await
    } else {
        let mut session = TerminalSession::enter_with_size(size)?;
        let input = tick_market::terminal::input_events()?;
        run_live(
            browser,
            &socket_directory,
            settings,
            &mut session.terminal,
            input,
            &mut updates,
        )
        .await
    };
    updates.close().await;
    result
}

async fn run_live<
    B: Backend + KeyBubbling + CardRows,
    S: Stream<Item = std::io::Result<Event>> + Unpin,
>(
    mut browser: Browser,
    socket_directory: &Path,
    settings: &Settings,
    terminal: &mut Terminal<B>,
    mut input: S,
    updates: &mut TickStream,
) -> Result<()> {
    let mut requests = updates.take_requests();
    let mut requests_closed = false;
    let (inbound_sender, mut inbound) = mpsc::channel::<(u64, Option<Vec<u8>>)>(INBOUND_CAPACITY);
    let mut generation = 0;
    // The task reading the connection the Tick holds, so one it lets go is closed.
    let mut reader: Option<(u64, tokio::task::AbortHandle)> = None;
    let mut view = View::default();
    let mut frames = tokio::time::interval(FRAME_INTERVAL);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut dirty = true;
    publish(updates, &browser)?;
    loop {
        let deadline = browser.next_deadline();
        let connect_at = browser.next_connect();
        tokio::select! {
            // In this order, not at random: when the loop was held up (a busy machine) and both a
            // result that came in time and its call's deadline are ready, the result is taken
            // first, instead of the deadline failing the call it answers.
            biased;
            Some((link, message)) = inbound.recv() => {
                let now = Instant::now();
                let answers = match message {
                    Some(message) => browser.receive(link, &message, now),
                    None => browser.unlink(link, now),
                };
                for (id, answer) in answers {
                    updates.answer(id, answer);
                }
                dirty = true;
                if requests_closed && !browser.has_calls() { break; }
            }
            line = requests.next(), if !requests_closed => match line {
                None => {
                    requests_closed = true;
                    if !browser.has_calls() { break; }
                }
                Some(line) => {
                    if let Some((id, title)) = chat_request(&line) {
                        if let Ok(title) = &title {
                            browser.chat(title);
                        }
                        updates.answer(id, title.map(|_| json!({})));
                    } else if let Some(request) = updates.dispatch(line)
                        && let Some((id, answer)) = browser.call(request, Instant::now())
                    {
                        updates.answer(id, answer);
                    }
                    dirty = true;
                }
            },
            _ = tokio::time::sleep_until(connect_at.unwrap_or_else(Instant::now)), if connect_at.is_some() => {
                match socket::connect(socket_directory).await {
                    Err(_) => browser.connect_failed(Instant::now()),
                    Ok(stream) => {
                        generation += 1;
                        let (read_half, write_half) = stream.into_split();
                        let (outbound, queued) = mpsc::unbounded_channel();
                        tokio::spawn(write_host(write_half, queued));
                        let task = tokio::spawn(read_host(generation, read_half, inbound_sender.clone()));
                        reader = Some((generation, task.abort_handle()));
                        browser.link(generation, outbound);
                    }
                }
                dirty = true;
            }
            _ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                for (id, answer) in browser.expire(Instant::now()) {
                    updates.answer(id, answer);
                }
                dirty = true;
                if requests_closed && !browser.has_calls() { break; }
            }
            _ = frames.tick() => {
                if dirty {
                    view::render(terminal, &browser, &mut view, settings)?;
                    dirty = false;
                }
            }
            event = input.next() => {
                dirty = true;
                match event {
                    None => break,
                    Some(Err(error)) => return Err(error.into()),
                    Some(Ok(event)) if runtime::is_exit(&event) => break,
                    Some(Ok(Event::Resize(..))) => {
                        terminal.clear().map_err(|error| Error::usage(error.to_string()))?;
                    }
                    Some(Ok(event @ (Event::FocusGained | Event::FocusLost))) => {
                        view.focused = event == Event::FocusGained;
                        terminal.clear().map_err(|error| Error::usage(error.to_string()))?;
                    }
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        if let Some(name) = press(&mut view, &mut browser, &key) {
                            terminal
                                .backend_mut()
                                .bubble(name)
                                .map_err(|error| Error::usage(error.to_string()))?;
                        }
                    }
                    Some(Ok(_)) => (),
                }
            }
        }
        // A connection the Tick let go, as after a refusal, is closed on its reading side too.
        if let Some((held, task)) = &reader
            && browser.generation() != Some(*held)
        {
            task.abort();
            reader = None;
        }
        if !matches!(browser.connection(), Connection::Connected { .. }) {
            view.stopped = false;
        }
        publish(updates, &browser)?;
    }
    if dirty {
        view::render(terminal, &browser, &mut view, settings)?;
    }
    Ok(())
}

/// The request id and the title of a `tick/chat` request in `line`, or the error to answer one
/// without a title with. `None` for every other line, which the stream dispatches: the SDK answers
/// methods it does not know with -32601, which tells Tickloop not to send it again.
fn chat_request(line: &InboundFrame) -> Option<(Value, std::result::Result<String, RpcError>)> {
    let InboundFrame::Line(line) = line else {
        return None;
    };
    if !line.contains(CHAT_METHOD) {
        return None;
    }
    let request: Value = serde_json::from_str(line).ok()?;
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || request.get("method").and_then(Value::as_str) != Some(CHAT_METHOD)
    {
        return None;
    }
    let id = request.get("id").filter(|id| !id.is_null())?.clone();
    let title = request
        .pointer("/params/title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tick/chat needs params.title, a string"));
    Some((id, title))
}

/// Publishes the Tick's state. The state is bounded, but should it still be too large to send, the
/// connection alone goes out instead, so nothing the extension sends can end the Tick.
fn publish(updates: &mut TickStream, browser: &Browser) -> Result<()> {
    if updates.publish(browser.data()).is_err() {
        updates.publish(json!({
            "connection": browser.connection.name(),
            "error": "the Tick's state is too large to report",
        }))?;
    }
    Ok(())
}

/// Does what `key` does in the card, whether the user is in it or only looks at it, since the Agent
/// sends keys to cards the user is not in. The name of a key to hand on to the host, when the Tick
/// has no use for it.
fn press(
    view: &mut View,
    browser: &mut Browser,
    key: &crossterm::event::KeyEvent,
) -> Option<&'static str> {
    match key.code {
        KeyCode::Char('?') => view.help = !view.help,
        KeyCode::Backspace if view.help => view.help = false,
        KeyCode::Char('s') if !view.help => view.stopped = browser.stop(),
        // The Tick uses none of the keys that move between cards.
        _ => return bubbled_key_name(key),
    }
    None
}

/// The name a host knows `key` by when the Tick hands it on: the keys that move between cards.
fn bubbled_key_name(key: &crossterm::event::KeyEvent) -> Option<&'static str> {
    if key
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

/// `at` in milliseconds since the Unix epoch, as the extension reads a deadline.
fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// Hands each message of the native host on the connection `link` to the loop, then its end.
async fn read_host(
    link: u64,
    mut reader: OwnedReadHalf,
    inbound: mpsc::Sender<(u64, Option<Vec<u8>>)>,
) {
    loop {
        let message = frame::read(&mut reader, frame::FROM_EXTENSION_MAX_BYTES)
            .await
            .ok()
            .flatten();
        let ended = message.is_none();
        if inbound.send((link, message)).await.is_err() || ended {
            return;
        }
    }
}

/// Writes the framed messages queued for the native host until the Tick lets the connection go.
async fn write_host(mut writer: OwnedWriteHalf, mut queued: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(frame) = queued.recv().await {
        if writer.write_all(&frame).await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use tick_protocol::TickTool;

    use super::*;

    const SESSION: &str = "hosted:4kqY8bXc2RnT0mWv7sLpQe";

    fn browser() -> (Browser, Catalog) {
        let catalog = Catalog::default();
        let browser = Browser::new(
            catalog.clone(),
            Setup {
                session: Session::new(Some(SESSION), Some(Path::new("/work/ticks")), 1),
                socket: PathBuf::from("/tmp/tick-browser-501/tick.sock"),
                registration: Ok(Vec::new()),
            },
        );
        (browser, catalog)
    }

    /// The native host's `hello` with the extension's `tools`.
    fn hello(tools: &[(&str, bool)]) -> Vec<u8> {
        let catalog: Vec<_> = tools
            .iter()
            .map(|(name, mutates)| TickTool {
                name: (*name).to_owned(),
                description: "A tool in the Agent's tab group.".to_owned(),
                input_schema: json!({"type": "object"}),
                mutates: *mutates,
            })
            .collect();
        serde_json::to_vec(&json!({
            "type": "hello",
            "extension_version": "0.1.0",
            "catalog": catalog,
        }))
        .unwrap()
    }

    fn update(tabs: &[Value]) -> Vec<u8> {
        serde_json::to_vec(&json!({"type": "update", "state": {"tabs": tabs}})).unwrap()
    }

    fn home() -> Value {
        json!({"id": 4, "title": "Home", "url": "https://example.com/", "active": true})
    }

    fn request(id: u64, tool: &str) -> ToolRequest {
        ToolRequest {
            id: json!(id),
            tool: tool.to_owned(),
            args: json!({"tab_id": 4}),
        }
    }

    /// The relay messages the Tick queued for the native host.
    fn sent(queued: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<Value> {
        let mut messages = Vec::new();
        while let Ok(frame) = queued.try_recv() {
            let length = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
            assert_eq!(length, frame.len() - 4);
            messages.push(serde_json::from_slice(&frame[4..]).unwrap());
        }
        messages
    }

    fn open() -> Value {
        json!({"type": "open", "protocol": 2, "session": SESSION, "title": "ticks"})
    }

    fn connected() -> (Browser, Catalog, mpsc::UnboundedReceiver<Vec<u8>>) {
        let (mut browser, catalog) = browser();
        let (outbound, mut queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        assert_eq!(sent(&mut queued), [open()]);
        let now = Instant::now();
        assert!(
            browser
                .receive(1, &hello(&[("read_page", false), ("computer", true)]), now)
                .is_empty()
        );
        assert!(browser.receive(1, &update(&[home()]), now).is_empty());
        (browser, catalog, queued)
    }

    #[test]
    fn a_session_is_named_by_its_instance_and_titled_by_its_directory() {
        assert_eq!(
            Session::new(Some(SESSION), Some(Path::new("/work/ticks")), 7),
            Session {
                id: SESSION.into(),
                title: "ticks".into()
            }
        );
        // Without a directory name, the identity's short form.
        assert_eq!(
            Session::new(Some(SESSION), Some(Path::new("/")), 7).title,
            "4kqY8bXc"
        );
        assert_eq!(Session::new(Some(SESSION), None, 7).title, "4kqY8bXc");
        // Run in a terminal, by its process.
        let terminal = Session::new(None, None, 4242);
        assert_eq!(
            (terminal.id.as_str(), terminal.title.as_str()),
            ("terminal:4242", "4242")
        );
        assert_eq!(Session::new(Some("  "), None, 9).id, "terminal:9");
        let long = Session::new(Some(&"x".repeat(500)), Some(Path::new(&"d".repeat(300))), 1);
        assert_eq!(long.id.chars().count(), relay::MAX_SESSION_CHARS);
        assert_eq!(long.title.chars().count(), relay::MAX_SESSION_TITLE_CHARS);
    }

    #[test]
    fn describe_offers_the_extensions_tools_only_while_the_host_serves_them() {
        let (mut browser, catalog) = browser();
        let waiting = catalog.descriptor();
        assert_eq!(browser.data()["connection"], "waiting");
        let (status_id, status) = browser
            .call(request(1, STATUS_TOOL), Instant::now())
            .unwrap();
        assert_eq!(status_id, 1);
        let status = status.unwrap();
        assert_eq!(status["connection"], "waiting");
        assert_eq!(status["session"], SESSION);
        assert_eq!(status["title"], "ticks");
        assert_eq!(status["socket"], "/tmp/tick-browser-501/tick.sock");
        assert_eq!(status["host_name"], "ai.tickloop.browser");

        let (outbound, mut queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        // The session opens first on every connection.
        assert_eq!(sent(&mut queued), [open()]);
        assert_eq!(browser.data()["connection"], "connecting");
        assert_eq!(catalog.descriptor(), waiting);
        let now = Instant::now();
        browser.receive(1, &hello(&[("read_page", false)]), now);
        browser.receive(1, &update(&[home()]), now);
        let served = catalog.descriptor();
        assert_eq!(served.tools.len(), 1);
        assert_eq!(served.tools[0].name, "read_page");
        assert_ne!(served.revision, waiting.revision);
        let data = browser.data();
        assert_eq!(data["connection"], "connected");
        assert_eq!(data["extension_version"], "0.1.0");
        assert_eq!(data["tabs"][0]["title"], "Home");
        // The status query belongs to the waiting catalog.
        let (_, answer) = browser
            .call(request(2, STATUS_TOOL), Instant::now())
            .unwrap();
        assert_eq!(answer.unwrap_err().code, INVALID_PARAMS);
        // A later hello replaces the catalog and keeps the session's state.
        browser.receive(1, &hello(&[("read_page", false), ("tabs", false)]), now);
        assert_eq!(catalog.descriptor().tools.len(), 2);
        assert_eq!(browser.data()["tabs"][0]["title"], "Home");

        assert!(browser.unlink(1, now).is_empty());
        assert_eq!(catalog.descriptor(), waiting);
        assert_eq!(browser.data()["connection"], "waiting");
        assert!(browser.data().get("tabs").is_none());
    }

    #[test]
    fn a_tick_without_a_host_tries_again_every_second_and_a_lost_one_too() {
        let (mut browser, _) = browser();
        let start = browser.next_connect().expect("a new Tick connects at once");
        assert!(start <= Instant::now());
        let waiting = browser.data();
        let now = Instant::now();
        browser.connect_failed(now);
        assert_eq!(browser.next_connect(), Some(now + RECONNECT_INTERVAL));
        // Failing again changes nothing the Agent sees.
        assert_eq!(browser.data(), waiting);

        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        assert_eq!(browser.next_connect(), None);
        browser.receive(1, &hello(&[("read_page", false)]), now);
        // The host goes away, as when the browser closes: the Tick waits and connects again.
        let later = now + Duration::from_secs(5);
        browser.unlink(1, later);
        assert_eq!(browser.data()["connection"], "waiting");
        assert_eq!(browser.next_connect(), Some(later + RECONNECT_INTERVAL));
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(2, outbound);
        browser.receive(2, &hello(&[("read_page", false)]), later);
        assert_eq!(browser.data()["connection"], "connected");
        // What the first connection's reader still hands over no longer counts.
        assert!(browser.unlink(1, later).is_empty());
        assert_eq!(browser.data()["connection"], "connected");
    }

    #[test]
    fn calls_are_relayed_and_answered_with_the_extensions_result() {
        let (mut browser, _, mut queued) = connected();
        let now = Instant::now();
        let before = unix_ms(SystemTime::now());
        assert!(browser.call(request(10, "read_page"), now).is_none());
        assert!(browser.call(request(11, "computer"), now).is_none());
        let after = unix_ms(SystemTime::now());
        let mut calls = sent(&mut queued);
        // Each call says when the Tick gives up on it, on the wall clock: 55 seconds from now.
        for call in &mut calls {
            let deadline = call["deadline_ms"].as_u64().unwrap();
            assert!((before + 55_000..=after + 55_000).contains(&deadline));
            call.as_object_mut().unwrap().remove("deadline_ms");
        }
        assert_eq!(
            calls,
            [
                json!({"type": "call", "id": 1, "tool": "read_page", "args": {"tab_id": 4}, "timeout_ms": 55000}),
                json!({"type": "call", "id": 2, "tool": "computer", "args": {"tab_id": 4}, "timeout_ms": 55000}),
            ]
        );
        assert_eq!(browser.last_step().unwrap().status, StepStatus::Running);

        let answers = browser.receive(
            1,
            br#"{"type":"result","id":2,"ok":false,"error":{"message":"The user denied this.","code":"denied"}}"#,
            now,
        );
        let error = answers[0].1.clone().unwrap_err();
        assert_eq!(answers[0].0, json!(11));
        assert_eq!(error.code, TOOL_FAILED);
        assert_eq!(error.message, "The user denied this.");
        assert_eq!(error.data, Some(json!({"code": "denied"})));
        assert_eq!(browser.last_step().unwrap().status, StepStatus::Failed);

        let answers = browser.receive(
            1,
            br#"{"type":"result","id":1,"ok":true,"result":"- main"}"#,
            now,
        );
        assert_eq!(answers, [(json!(10), Ok(json!("- main")))]);
        // Each call's step is updated where it is, the latest call first.
        let steps: Vec<_> = browser
            .recent_steps()
            .map(|step| (step.tool.as_str(), step.status))
            .collect();
        assert_eq!(
            steps,
            [
                ("computer", StepStatus::Failed),
                ("read_page", StepStatus::Done)
            ]
        );
        assert_eq!(browser.data()["last_step"]["tool"], "computer");
        assert_eq!(browser.data()["last_step"]["status"], "failed");
        assert!(browser.data()["last_step"].get("call").is_none());
        // An answer to no call in flight, or a second one, changes nothing.
        assert!(
            browser
                .receive(1, br#"{"type":"result","id":1,"ok":true}"#, now)
                .is_empty()
        );
        assert!(!browser.has_calls());
        assert!(
            browser
                .call(request(12, "unknown"), now)
                .unwrap()
                .1
                .is_err()
        );
        // A call too large to send once the host adds the session fails here.
        let mut large = request(13, "read_page");
        large.args = json!({"text": "x".repeat(frame::TO_EXTENSION_MAX_BYTES - 100)});
        assert_eq!(
            browser.call(large, now).unwrap().1.unwrap_err().code,
            INVALID_PARAMS
        );
        assert!(sent(&mut queued).is_empty());
    }

    #[test]
    fn a_call_the_extension_does_not_answer_in_time_fails_and_a_change_is_unknown() {
        let (mut browser, _, _queued) = connected();
        let now = Instant::now();
        browser.call(request(10, "read_page"), now);
        browser.call(request(11, "computer"), now + Duration::from_secs(1));
        assert_eq!(browser.next_deadline(), Some(now + CALL_TIMEOUT));
        assert!(
            browser
                .expire(now + CALL_TIMEOUT - Duration::from_millis(1))
                .is_empty()
        );

        let answers = browser.expire(now + CALL_TIMEOUT);
        assert_eq!(answers.len(), 1);
        let error = answers[0].1.clone().unwrap_err();
        assert_eq!(error.code, TOOL_FAILED);
        assert_eq!(error.data, Some(json!({"code": "timed_out"})));
        assert!(
            error.message.contains("did not answer within 55 seconds"),
            "{}",
            error.message
        );

        let answers = browser.expire(now + CALL_TIMEOUT + Duration::from_secs(1));
        let error = answers[0].1.clone().unwrap_err();
        assert_eq!(answers[0].0, json!(11));
        assert_eq!(error.data, Some(json!({"code": "outcome_unknown"})));
        assert!(
            error.message.contains("whether it took effect is unknown"),
            "{}",
            error.message
        );
        assert_eq!(browser.last_step().unwrap().status, StepStatus::Unknown);
        assert_eq!(browser.next_deadline(), None);
        // The late result is dropped.
        assert!(
            browser
                .receive(1, br#"{"type":"result","id":2,"ok":true}"#, now)
                .is_empty()
        );
    }

    #[test]
    fn a_call_that_runs_out_of_time_on_an_approval_for_its_tool_waits_for_the_user() {
        let pending = |items: Value| {
            serde_json::to_vec(
                &json!({"type": "update", "state": {"tabs": [home()], "pending": items}}),
            )
            .unwrap()
        };
        let approval = json!({
            "id": "approval-3",
            "kind": "approval",
            "summary": "Allow the Agent on github.com (computer)",
            "domain": "github.com",
            "tool": "computer",
            "open_url": "https://tickloop.ai/#tickloop-approve=s/approval-3",
        });
        let cases = [
            // An approval for this call's tool: the call waits for the user, nothing ran.
            (json!([approval.clone()]), "waiting_for_approval"),
            // Only approvals for another tool, a plan without a tool, or a hand-back for this one.
            (
                json!([
                    {"id": "approval-4", "kind": "approval", "summary": "Allow example.org", "tool": "read_page"},
                    {"id": "plan-1", "kind": "approval", "summary": "Approve the plan"},
                    {"id": "hand-back-1", "kind": "hand_back", "summary": "Pay", "tool": "computer"},
                ]),
                "outcome_unknown",
            ),
            // Nothing waits.
            (json!([]), "outcome_unknown"),
        ];
        for (items, code) in cases {
            let (mut browser, _, mut queued) = connected();
            let now = Instant::now();
            assert!(browser.call(request(11, "computer"), now).is_none());
            sent(&mut queued);
            assert!(browser.receive(1, &pending(items), now).is_empty());
            let answers = browser.expire(now + CALL_TIMEOUT);
            assert_eq!(answers.len(), 1);
            assert_eq!(answers[0].0, json!(11));
            let error = answers[0].1.clone().unwrap_err();
            assert_eq!(error.code, TOOL_FAILED);
            assert_eq!(error.data, Some(json!({"code": code})));
            if code == "waiting_for_approval" {
                for part in [
                    "approve \"Allow the Agent on github.com (computer)\"",
                    "side panel",
                    "Tell the user in one sentence what to approve",
                    "subscribed to this Tick (subscribe with TickManage first if this Chat is not)",
                    "shows it allowed, call computer again",
                    "shows it denied, stop. The user can open https://tickloop.ai/#tickloop-approve=s/approval-3 to approve it; give them this link",
                ] {
                    assert!(error.message.contains(part), "{}", error.message);
                }
                assert_eq!(browser.last_step().unwrap().status, StepStatus::Failed);
            }
            assert_eq!(browser.next_deadline(), None);
        }
    }

    #[test]
    fn calls_in_flight_fail_when_the_host_goes_away_and_nothing_is_resent() {
        let (mut browser, catalog, mut queued) = connected();
        browser.call(request(10, "read_page"), Instant::now());
        browser.call(request(11, "computer"), Instant::now());
        sent(&mut queued);
        let mut answers = browser.unlink(1, Instant::now());
        answers.sort_by_key(|(id, _)| id.as_u64());
        let codes: Vec<_> = answers
            .iter()
            .map(|(_, answer)| answer.clone().unwrap_err().data.unwrap()["code"].clone())
            .collect();
        assert_eq!(codes, [json!("disconnected"), json!("outcome_unknown")]);
        assert!(sent(&mut queued).is_empty());
        assert_eq!(catalog.descriptor().tools[0].name, STATUS_TOOL);
        // The stale connection's messages no longer count.
        assert!(
            browser
                .receive(1, br#"{"type":"result","id":1,"ok":true}"#, Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn a_refusal_is_shown_and_tried_again_later_without_flickering() {
        let (mut browser, catalog, mut queued) = connected();
        browser.call(request(10, "computer"), Instant::now());
        sent(&mut queued);
        let now = Instant::now();
        // A host of another version refuses the session.
        let answers = browser.receive(
            1,
            br#"{"type":"refused","protocol":3,"code":"protocol","message":"the Tick speaks relay protocol 2 and this host speaks 3; update the one that is older"}"#,
            now,
        );
        assert_eq!(
            answers[0].1.clone().unwrap_err().data,
            Some(json!({"code": "outcome_unknown"}))
        );
        let refused = browser.data();
        assert_eq!(refused["connection"], "refused");
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("this host speaks 3")
        );
        assert_eq!(catalog.descriptor().tools[0].name, STATUS_TOOL);
        // The Tick lets the connection go, and tries again only after a while.
        assert_eq!(browser.generation(), None);
        assert!(queued.is_closed() || queued.try_recv().is_err());
        assert_eq!(browser.next_connect(), Some(now + REFUSED_RETRY));
        assert_eq!(browser.status()["error"], refused["error"]);

        // Trying again shows the refusal until a hello replaces it.
        let (outbound, mut queued) = mpsc::unbounded_channel();
        browser.link(2, outbound);
        assert_eq!(sent(&mut queued), [open()]);
        assert_eq!(browser.data(), refused);
        browser.receive(2, &hello(&[("read_page", false)]), now);
        assert_eq!(browser.data()["connection"], "connected");

        // A catalog this Tick cannot serve is refused here, and a gone host ends the refusal.
        browser.receive(2, &hello(&[("tabs", false), ("tabs", true)]), now);
        assert!(
            browser.data()["error"]
                .as_str()
                .unwrap()
                .contains("duplicate"),
            "{}",
            browser.data()
        );
        browser.connect_failed(now);
        assert_eq!(browser.data()["connection"], "waiting");
    }

    #[test]
    fn the_chats_title_goes_to_the_host_with_every_open_and_at_once_when_it_changes() {
        let (mut browser, _) = browser();
        // Told before any host serves, it waits for the next open.
        browser.chat("  Fix the login page\n");
        assert_eq!(browser.chat_title(), Some("Fix the login page"));
        let (outbound, mut queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        let mut titled = open();
        titled["chat_title"] = json!("Fix the login page");
        assert_eq!(sent(&mut queued), [titled.clone()]);

        // A new one goes at once; the same one again, or a blank one, sends nothing.
        browser.chat("Fix the login redirect");
        browser.chat("Fix the login redirect");
        browser.chat(" ");
        assert_eq!(
            sent(&mut queued),
            [json!({"type": "chat_title", "chat_title": "Fix the login redirect"})]
        );

        // The next connection opens with the latest.
        browser.unlink(1, Instant::now());
        let (outbound, mut queued) = mpsc::unbounded_channel();
        browser.link(2, outbound);
        titled["chat_title"] = json!("Fix the login redirect");
        assert_eq!(sent(&mut queued), [titled]);
    }

    #[test]
    fn a_tick_chat_request_is_taken_before_the_stream_and_any_other_line_is_left_to_it() {
        let line = |text: &str| InboundFrame::Line(text.to_owned());
        let (id, title) = chat_request(&line(
            r#"{"jsonrpc":"2.0","id":9,"method":"tick/chat","params":{"title":"Plan the trip"}}"#,
        ))
        .unwrap();
        assert_eq!((id, title.unwrap()), (json!(9), "Plan the trip".to_owned()));
        let (id, title) = chat_request(&line(
            r#"{"jsonrpc":"2.0","id":"c","method":"tick/chat","params":{}}"#,
        ))
        .unwrap();
        assert_eq!(id, json!("c"));
        assert_eq!(title.unwrap_err().code, INVALID_PARAMS);
        for other in [
            r#"{"jsonrpc":"2.0","id":1,"method":"tick/read","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tick/call","params":{"tool":"tick/chat"}}"#,
            r#"{"jsonrpc":"2.0","method":"tick/chat","params":{"title":"no id"}}"#,
            r#"{"jsonrpc":"1.0","id":3,"method":"tick/chat","params":{"title":"t"}}"#,
            "tick/chat",
        ] {
            assert!(chat_request(&line(other)).is_none(), "{other}");
        }
        assert!(chat_request(&InboundFrame::TooLarge(b"tick/chat".to_vec())).is_none());
    }

    #[test]
    fn the_state_carries_the_name_and_color_the_browser_shows_the_session_by() {
        let (mut browser, _, _queued) = connected();
        assert!(browser.data().get("label").is_none());
        browser.receive(
            1,
            br#"{"type":"update","state":{"tabs":[],"label":"ticks \u00b7 9897","group_color":"cyan"}}"#,
            Instant::now(),
        );
        let data = browser.data();
        assert_eq!(data["label"], "ticks · 9897");
        assert_eq!(data["group_color"], "cyan");
    }

    #[test]
    fn updates_replace_the_session_state_and_stop_reaches_the_host() {
        let (mut browser, _, mut queued) = connected();
        browser.receive(
            1,
            br#"{"type":"update","state":{"tabs":[{"id":5,"title":"Cart","url":"https://shop.example/cart","active":true}],"pending":[{"id":"a1","kind":"approval","summary":"Allow shop.example","domain":"shop.example"}],"last_event":"The user opened the cart"}}"#,
            Instant::now(),
        );
        let data = browser.data();
        assert_eq!(data["tabs"][0]["id"], 5);
        assert_eq!(data["pending"][0]["kind"], "approval");
        assert_eq!(data["last_event"], "The user opened the cart");
        assert!(browser.stop());
        assert_eq!(sent(&mut queued), [json!({"type": "stop"})]);
        browser.unlink(1, Instant::now());
        assert!(!browser.stop());
    }

    #[test]
    fn what_is_not_a_message_is_ignored_and_a_malformed_one_is_refused_even_when_connected() {
        let (mut browser, catalog) = browser();
        let (outbound, mut queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        sent(&mut queued);
        let now = Instant::now();
        // A first frame that is JSON but no object with a type.
        for ignored in [&b"[1,2]"[..], br#""x""#] {
            assert!(browser.receive(1, ignored, now).is_empty());
        }
        assert_eq!(browser.data()["connection"], "connecting");

        browser.receive(1, &hello(&[("read_page", false), ("computer", true)]), now);
        assert!(browser.call(request(10, "computer"), now).is_none());
        // A hello whose catalog is not an array, while connected.
        let answers = browser.receive(
            1,
            br#"{"type":"hello","extension_version":"0.2.0","catalog":{}}"#,
            now,
        );
        let data = browser.data();
        assert_eq!(data["connection"], "refused");
        assert!(
            data["error"]
                .as_str()
                .unwrap()
                .starts_with("the native host sent a malformed \"hello\""),
            "{data}"
        );
        assert_eq!(catalog.descriptor().tools[0].name, STATUS_TOOL);
        // The change in flight has an unknown outcome, as when the host goes away.
        assert_eq!(answers.len(), 1);
        assert_eq!(
            answers[0].1.clone().unwrap_err().data,
            Some(json!({"code": "outcome_unknown"}))
        );
    }

    /// Whether `data` fits a `tick/update`, with room for the update's own fields.
    fn fits_an_update(data: &Value) -> bool {
        serde_json::to_vec(data).unwrap().len() + 1024 < tick_protocol::MAX_UPDATE_BYTES
    }

    #[test]
    fn what_the_host_passes_on_cannot_grow_the_state_past_an_update() {
        let kib = |count: usize| "x".repeat(count * 1024);
        let now = Instant::now();
        // A refusal whose message is 600 KiB long.
        let (mut browser, catalog) = browser();
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        let refused =
            json!({"type": "refused", "protocol": 2, "code": "malformed", "message": kib(600)});
        browser.receive(1, &serde_json::to_vec(&refused).unwrap(), now);
        assert_eq!(browser.data()["connection"], "refused");
        assert!(fits_an_update(&browser.data()));
        assert!(fits_an_update(&browser.status()));
        assert!(
            browser.data()["error"].as_str().unwrap().chars().count() <= relay::MAX_MESSAGE_CHARS
        );
        assert_eq!(catalog.descriptor().tools[0].name, STATUS_TOOL);

        // A catalog with two tools of the same name over 300 KiB long.
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(2, outbound);
        let name = kib(300);
        browser.receive(2, &hello(&[(&name, false), (&name, true)]), now);
        assert_eq!(browser.data()["connection"], "refused");
        assert!(fits_an_update(&browser.data()));

        // A tool whose name is 400 KiB long, accepted and called, beside 40 tabs at their limits.
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(3, outbound);
        let name = kib(400);
        browser.receive(3, &hello(&[(&name, true)]), now);
        let tabs: Vec<_> = (0..40)
            .map(|id| json!({"id": id, "title": "\u{1}".repeat(500), "url": "\"".repeat(5000), "active": id == 0}))
            .collect();
        browser.receive(3, &update(&tabs), now);
        assert_eq!(
            browser.data()["connection"],
            "connected",
            "{}",
            browser.data()["error"]
        );
        assert!(browser.call(request(1, &name), now).is_none());
        let data = browser.data();
        assert_eq!(data["tabs"].as_array().unwrap().len(), 32);
        assert!(
            data["last_step"]["tool"].as_str().unwrap().chars().count() <= relay::MAX_TOOL_CHARS
        );
        assert!(fits_an_update(&data));
    }

    #[test]
    fn a_looked_at_card_answers_keys_and_hands_on_the_ones_it_has_no_use_for() {
        use crossterm::event::KeyEvent;

        let (mut browser, _, mut queued) = connected();
        let mut view = View {
            help: false,
            focused: false,
            stopped: false,
            told_rows: None,
        };
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Char('s'))),
            None
        );
        assert!(view.stopped);
        assert_eq!(sent(&mut queued), [json!({"type": "stop"})]);
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Char('?'))),
            None
        );
        assert!(view.help);
        // Stop is not a key of the help overlay, which Backspace closes; Esc is handed on.
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Char('s'))),
            None
        );
        assert!(sent(&mut queued).is_empty());
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Esc)),
            Some("esc")
        );
        assert!(view.help);
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Backspace)),
            None
        );
        assert!(!view.help);
        for (code, name) in [
            (KeyCode::Esc, "esc"),
            (KeyCode::Up, "up"),
            (KeyCode::Down, "down"),
            (KeyCode::Left, "left"),
            (KeyCode::Right, "right"),
            (KeyCode::Tab, "tab"),
            (KeyCode::BackTab, "backtab"),
        ] {
            assert_eq!(press(&mut view, &mut browser, &key(code)), Some(name));
        }
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Char('x'))),
            None
        );
        assert_eq!(
            press(&mut view, &mut browser, &key(KeyCode::Backspace)),
            None
        );
        let control_up = KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL);
        assert_eq!(press(&mut view, &mut browser, &control_up), None);
    }

    #[test]
    fn a_wait_for_the_extension_that_runs_long_is_said_once_and_starts_again_when_it_goes() {
        let (mut browser, _) = browser();
        let started = Instant::now();
        let due = browser.next_deadline().expect("the wait has a deadline");
        assert!(due >= started + LONG_WAIT - Duration::from_secs(1) && due <= started + LONG_WAIT);
        assert!(browser.expire(due - Duration::from_millis(1)).is_empty());
        assert!(browser.data().get("long_wait").is_none());
        let first = hint(browser.setup());
        assert_eq!(browser.data()["hint"], first);

        // A minute without an extension changes the state, so a subscribed Agent hears it.
        assert!(browser.expire(due).is_empty());
        let data = browser.data();
        assert_eq!(data["connection"], "waiting");
        assert_eq!(data["long_wait"], true);
        let long = data["hint"].as_str().unwrap();
        assert!(
            long.starts_with("No extension has connected for over a minute: Chrome, Chromium or Edge may not be open, the Tickloop extension may not be installed or enabled in it")
                && long.contains("opening the extension's side panel tries at once. ")
                && long.ends_with(&first),
            "{long}"
        );
        assert_eq!(browser.next_deadline(), None);

        // A connection to a host that never says hello leaves the wait as long as it was.
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        assert_eq!(browser.unlink(1, Instant::now()), Vec::new());
        assert_eq!(browser.data()["long_wait"], true);

        // A host's hello ends it, and when the host goes the wait starts over.
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(2, outbound);
        browser.receive(2, &hello(&[("read_page", false)]), Instant::now());
        assert_eq!(browser.data()["connection"], "connected");
        assert!(browser.data().get("long_wait").is_none());
        assert_eq!(browser.next_deadline(), None);
        let gone = Instant::now();
        browser.unlink(2, gone);
        assert!(browser.data().get("long_wait").is_none());
        assert!(browser.next_deadline().unwrap() >= gone + LONG_WAIT);
    }

    #[test]
    fn the_hint_says_how_the_registration_went() {
        let mut setup = Setup {
            session: Session::new(Some(SESSION), None, 1),
            socket: PathBuf::from("/tmp/s"),
            registration: Ok(Vec::new()),
        };
        assert!(hint(&setup).starts_with("No Chrome"));
        setup.registration = Ok(vec![registration::Entry {
            browser: "Google Chrome",
            manifest: PathBuf::from("/m.json"),
            status: registration::Status::Failed {
                error: "read-only".into(),
            },
        }]);
        assert!(hint(&setup).contains("Google Chrome failed: read-only"));
        setup.registration = Err("no home".into());
        assert!(hint(&setup).contains("no home"));

        // What opening the Tick wrote is said first.
        setup.registration = Ok(vec![
            registration::Entry {
                browser: "Google Chrome",
                manifest: PathBuf::from("/c/ai.tickloop.browser.json"),
                status: registration::Status::Written,
            },
            registration::Entry {
                browser: "Microsoft Edge",
                manifest: PathBuf::from("/e/ai.tickloop.browser.json"),
                status: registration::Status::Replaced {
                    previous_path: Some("/other/tick-browser".into()),
                },
            },
        ]);
        let hint = hint(&setup);
        assert!(
            hint.starts_with(
                "Registered the native messaging host for Google Chrome at /c/ai.tickloop.browser.json. Pointed Microsoft Edge's native messaging host at /e/ai.tickloop.browser.json to this program; it named /other/tick-browser before. Open Chrome"
            ) && hint.ends_with("it connects to this Tick by itself, usually within seconds. If the extension is not installed, load it in chrome://extensions (turn on Developer mode, choose Load unpacked and pick the extension's folder)."),
            "{hint}"
        );
    }
}
