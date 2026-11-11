//! The native messaging host Chrome starts for the extension: `tick-browser chrome-extension://<id>/`.
//! Once the extension's `hello` is accepted it serves the local socket, opens a session for each Tick
//! that connects, and routes calls, results and state between the extension and the sessions' Ticks.
//! Of two browsers, the host that takes the socket's lock first serves; the other keeps trying. It
//! ends when Chrome closes its input.

use std::{
    collections::HashMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
    task::AbortHandle,
    time::Instant,
};

use crate::{
    catalog, frame, is_allowed_origin,
    relay::{self, FromExtension, FromTick, Hello, PROTOCOL, Refusal, ToExtension, ToTick, code},
    socket::{self, Listening},
};

/// How long the host waits before trying again to serve the socket, as when another browser's host
/// serves it.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// How often the host keeps its socket from the system's cleaning of old temporary files.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long the host waits after the socket fails to accept, such as when it runs out of files.
const ACCEPT_RETRY: Duration = Duration::from_millis(250);
/// Messages read from one side and not yet written to the other.
const QUEUE_CAPACITY: usize = 16;
/// The option a test names its own socket directory with; Chrome never passes it.
const SOCKET_DIR_OPTION: &str = "--socket-dir";

/// How Chrome started this program as a native messaging host: the extension's origin first, and,
/// in tests only, the socket directory.
pub struct Launch {
    pub origin: String,
    pub socket_directory: Option<PathBuf>,
}

/// The launch when `arguments` (without the program) start with an extension origin.
pub fn launched_by_browser(arguments: impl IntoIterator<Item = OsString>) -> Option<Launch> {
    let mut arguments = arguments.into_iter();
    let origin = arguments
        .next()?
        .into_string()
        .ok()
        .filter(|origin| origin.starts_with("chrome-extension://"))?;
    let mut socket_directory = None;
    // Chrome may add arguments of its own, such as `--parent-window` on Windows; they are ignored.
    while let Some(argument) = arguments.next() {
        if argument == SOCKET_DIR_OPTION {
            socket_directory = arguments.next().map(PathBuf::from);
        }
    }
    Some(Launch {
        origin,
        socket_directory,
    })
}

/// Runs the host for `launch` on this process's stdio until Chrome closes it.
pub fn main(launch: Launch) -> ExitCode {
    const PROGRAM: &str = "tick-browser";
    if !is_allowed_origin(&launch.origin) {
        eprintln!(
            "{PROGRAM}: {} is not an extension this host serves",
            launch.origin
        );
        return ExitCode::from(tick_provider::exit_code::USAGE);
    }
    let directory = match launch.socket_directory {
        Some(directory) => directory,
        None => match socket::default_directory() {
            Ok(directory) => directory,
            Err(error) => {
                eprintln!("{PROGRAM}: cannot find the browser socket: {error}");
                return ExitCode::FAILURE;
            }
        },
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{PROGRAM}: could not start async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(relay(tokio::io::stdin(), tokio::io::stdout(), &directory));
    runtime.shutdown_timeout(Duration::from_millis(100));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{PROGRAM}: {error}");
            ExitCode::FAILURE
        }
    }
}

/// One Tick connected to the socket, and the session it opened.
struct Tick {
    /// Framed messages for the Tick; dropping it closes the connection once they are written.
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    reader: AbortHandle,
    session: Option<String>,
}

/// What the host knows: the extension's accepted `hello`, the socket while it serves it, and the
/// Ticks connected to it. What it handles queues messages for the extension in `to_extension`.
struct Host {
    directory: PathBuf,
    hello: Option<Hello>,
    listening: Option<Listening>,
    /// When to try serving the socket again, while the `hello` is accepted and another host serves.
    retry: Option<Instant>,
    /// The code of the refusal last sent while trying to serve, so it is sent once.
    trying: Option<&'static str>,
    ticks: HashMap<u64, Tick>,
    sessions: HashMap<String, u64>,
    next_tick: u64,
    to_extension: Vec<Vec<u8>>,
}

impl Host {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            hello: None,
            listening: None,
            retry: None,
            trying: None,
            ticks: HashMap::new(),
            sessions: HashMap::new(),
            next_tick: 0,
            to_extension: Vec::new(),
        }
    }

    fn tell_extension(&mut self, message: &ToExtension<'_>) {
        self.to_extension.push(message.to_vec());
    }

    fn refuse_extension(&mut self, refusal: &Refusal) {
        self.tell_extension(&ToExtension::Refused {
            protocol: PROTOCOL,
            code: refusal.code,
            message: &refusal.message,
        });
    }

    /// Handles one message from the extension.
    fn on_extension(&mut self, message: &[u8]) {
        match relay::parse_from_extension(message) {
            Ok(None) => (),
            Err(refusal) => self.stop_serving(&refusal),
            Ok(Some(FromExtension::Hello(hello))) => {
                if let Err(error) = catalog::check(&hello.catalog) {
                    let refusal =
                        Refusal::new(code::CATALOG, format!("the extension's catalog: {error}"));
                    return self.stop_serving(&refusal);
                }
                self.hello = Some(hello);
                if self.listening.is_none() {
                    return self.serve();
                }
                self.tell_extension(&ToExtension::Welcome { protocol: PROTOCOL });
                let hello = self.tick_hello();
                let opened: Vec<u64> = self.sessions.values().copied().collect();
                for tick in opened {
                    self.tell_tick(tick, &hello);
                }
            }
            Ok(Some(
                FromExtension::Update { session, message }
                | FromExtension::Result { session, message },
            )) => {
                // What comes for a session that is not open, as one that just closed, is dropped.
                if let Some(&tick) = self.sessions.get(&session) {
                    self.tell_tick(tick, &message);
                }
            }
        }
    }

    /// Serves the socket for the accepted `hello`, or tells the extension why not and tries again
    /// later.
    fn serve(&mut self) {
        if self.hello.is_none() || self.listening.is_some() {
            return;
        }
        match socket::listen(&self.directory) {
            Ok(listening) => {
                self.listening = Some(listening);
                self.retry = None;
                self.trying = None;
                self.tell_extension(&ToExtension::Welcome { protocol: PROTOCOL });
            }
            Err(error) => {
                let refusal = if error.kind() == io::ErrorKind::AddrInUse {
                    Refusal::new(code::BUSY, error.to_string())
                } else {
                    Refusal::new(
                        code::SOCKET,
                        format!(
                            "cannot serve the browser socket in {}: {error}",
                            self.directory.display()
                        ),
                    )
                };
                if self.trying != Some(refusal.code) {
                    self.trying = Some(refusal.code);
                    self.refuse_extension(&refusal);
                }
                self.retry = Some(Instant::now() + RETRY_INTERVAL);
            }
        }
    }

    /// Stops serving for `refusal`: every session closes, the socket goes, and nothing is served
    /// until the extension sends a `hello` the host accepts.
    fn stop_serving(&mut self, refusal: &Refusal) {
        let sessions: Vec<String> = self.sessions.drain().map(|(session, _)| session).collect();
        for session in &sessions {
            self.tell_extension(&ToExtension::SessionClosed { session });
        }
        for (_, tick) in self.ticks.drain() {
            tick.reader.abort();
        }
        self.listening = None;
        self.hello = None;
        self.retry = None;
        self.trying = None;
        self.refuse_extension(refusal);
    }

    /// Takes a Tick that connected to the socket; its messages arrive through `inbound`.
    fn connect(&mut self, stream: UnixStream, inbound: &mpsc::Sender<(u64, Option<Vec<u8>>)>) {
        self.next_tick += 1;
        let id = self.next_tick;
        let (reader, writer) = stream.into_split();
        let (outbound, queued) = mpsc::unbounded_channel();
        tokio::spawn(write_tick(writer, queued));
        let reader = tokio::spawn(read_tick(id, reader, inbound.clone())).abort_handle();
        self.ticks.insert(
            id,
            Tick {
                outbound,
                reader,
                session: None,
            },
        );
    }

    /// Handles one message from the Tick connected as `id`.
    fn on_tick(&mut self, id: u64, message: &[u8]) {
        let Some(tick) = self.ticks.get(&id) else {
            return;
        };
        let opened = tick.session.clone();
        match (relay::parse_from_tick(message), opened) {
            (Ok(None), _) => (),
            (Err(refusal), _) => self.refuse_tick(id, &refusal),
            (
                Ok(Some(FromTick::Open {
                    session,
                    title,
                    chat_title,
                })),
                None,
            ) => {
                if self.sessions.contains_key(&session) {
                    let refusal = Refusal::new(
                        code::SESSION,
                        format!("a Tick of the session {session} is already connected"),
                    );
                    return self.refuse_tick(id, &refusal);
                }
                self.tell_extension(&ToExtension::SessionOpened {
                    session: &session,
                    title: &title,
                    chat_title: chat_title.as_deref(),
                });
                self.sessions.insert(session.clone(), id);
                if let Some(tick) = self.ticks.get_mut(&id) {
                    tick.session = Some(session);
                }
                let hello = self.tick_hello();
                self.tell_tick(id, &hello);
            }
            (Ok(Some(FromTick::Open { .. })), Some(_)) => {
                self.refuse_tick(
                    id,
                    &Refusal::new(code::MALFORMED, "the session is open already"),
                );
            }
            (Ok(Some(_)), None) => {
                let refusal = Refusal::new(code::MALFORMED, "the first message must be \"open\"");
                self.refuse_tick(id, &refusal);
            }
            (
                Ok(Some(FromTick::Call {
                    id: call,
                    tool,
                    args,
                    timeout_ms,
                    deadline_ms,
                })),
                Some(session),
            ) => {
                let message = ToExtension::Call {
                    session: &session,
                    id: call,
                    tool: &tool,
                    args: &args,
                    timeout_ms: timeout_ms.as_ref(),
                    deadline_ms: deadline_ms.as_ref(),
                }
                .to_vec();
                if message.len() > frame::TO_EXTENSION_MAX_BYTES {
                    let failed = json!({
                        "type": "result",
                        "id": call,
                        "ok": false,
                        "error": {"message": format!("the call to {tool} is too large to send to the browser extension")},
                    });
                    self.tell_tick(id, &serde_json::to_vec(&failed).expect("JSON serializes"));
                } else {
                    self.to_extension.push(message);
                }
            }
            (Ok(Some(FromTick::Stop)), Some(session)) => {
                self.tell_extension(&ToExtension::Stop { session: &session });
            }
            (Ok(Some(FromTick::ChatTitle { chat_title })), Some(session)) => {
                self.tell_extension(&ToExtension::ChatTitle {
                    session: &session,
                    chat_title: &chat_title,
                });
            }
        }
    }

    /// Refuses the Tick connected as `id` and closes its connection.
    fn refuse_tick(&mut self, id: u64, refusal: &Refusal) {
        let message = ToTick::Refused {
            protocol: PROTOCOL,
            code: refusal.code,
            message: &refusal.message,
        }
        .to_vec();
        self.tell_tick(id, &message);
        self.disconnected(id);
    }

    /// The Tick connected as `id` went away, or is let go: its session closes.
    fn disconnected(&mut self, id: u64) {
        let Some(tick) = self.ticks.remove(&id) else {
            return;
        };
        tick.reader.abort();
        if let Some(session) = tick.session {
            self.sessions.remove(&session);
            self.tell_extension(&ToExtension::SessionClosed { session: &session });
        }
    }

    /// The `hello` a Tick gets when its session opens and whenever the extension replaces it.
    fn tick_hello(&self) -> Vec<u8> {
        let hello = self
            .hello
            .as_ref()
            .expect("Ticks connect only while a hello is accepted");
        ToTick::Hello {
            extension_version: &hello.extension_version,
            catalog: &hello.catalog,
        }
        .to_vec()
    }

    fn tell_tick(&self, id: u64, message: &[u8]) {
        if let Some(tick) = self.ticks.get(&id)
            && let Ok(frame) = frame::encode(message, frame::FROM_EXTENSION_MAX_BYTES)
        {
            let _ = tick.outbound.send(frame);
        }
    }
}

/// Serves the extension on the browser's `input` and `output` and the Ticks on the socket in
/// `directory`, until the browser closes `input`.
pub async fn relay(
    input: impl AsyncRead + Unpin + Send + 'static,
    mut output: impl AsyncWrite + Unpin,
    directory: &Path,
) -> io::Result<()> {
    let (browser_sender, mut from_browser) = mpsc::channel(QUEUE_CAPACITY);
    tokio::spawn(forward(
        input,
        frame::FROM_EXTENSION_MAX_BYTES,
        browser_sender,
    ));
    let (tick_sender, mut from_ticks) = mpsc::channel(QUEUE_CAPACITY);
    let mut host = Host::new(directory.to_owned());
    let mut refresh = tokio::time::interval_at(Instant::now() + REFRESH_INTERVAL, REFRESH_INTERVAL);
    loop {
        for message in std::mem::take(&mut host.to_extension) {
            frame::write(&mut output, &message, frame::TO_EXTENSION_MAX_BYTES).await?;
        }
        let retry = host.retry;
        tokio::select! {
            message = from_browser.recv() => match message {
                None => return Ok(()),
                Some(message) => host.on_extension(&message),
            },
            accepted = async {
                match &host.listening {
                    Some(listening) => listening.listener.accept().await,
                    None => std::future::pending().await,
                }
            } => match accepted {
                // Accepting again at once would only fail again.
                Err(_) => tokio::time::sleep(ACCEPT_RETRY).await,
                Ok((stream, _)) => host.connect(stream, &tick_sender),
            },
            Some((tick, message)) = from_ticks.recv() => match message {
                Some(message) => host.on_tick(tick, &message),
                None => host.disconnected(tick),
            },
            _ = tokio::time::sleep_until(retry.unwrap_or_else(Instant::now)), if retry.is_some() => {
                host.serve();
            }
            _ = refresh.tick() => {
                // A socket that cannot be kept leaves the host serving the Ticks it has.
                if let Some(listening) = &mut host.listening {
                    let _ = listening.refresh();
                }
            }
        }
    }
}

/// Hands each message of the Tick connected as `id` to the host, then its end.
async fn read_tick(
    id: u64,
    mut reader: OwnedReadHalf,
    inbound: mpsc::Sender<(u64, Option<Vec<u8>>)>,
) {
    loop {
        let message = frame::read(&mut reader, frame::TO_EXTENSION_MAX_BYTES)
            .await
            .ok()
            .flatten();
        let ended = message.is_none();
        if inbound.send((id, message)).await.is_err() || ended {
            return;
        }
    }
}

/// Writes the framed messages queued for one Tick until the host lets it go.
async fn write_tick(mut writer: OwnedWriteHalf, mut queued: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(frame) = queued.recv().await {
        if writer.write_all(&frame).await.is_err() {
            return;
        }
    }
    let _ = writer.shutdown().await;
}

/// Hands each message read from `reader` on until it ends, breaks a frame, or nothing takes them.
async fn forward(
    mut reader: impl AsyncRead + Unpin,
    maximum: usize,
    sender: mpsc::Sender<Vec<u8>>,
) {
    while let Ok(Some(message)) = frame::read(&mut reader, maximum).await {
        if sender.send(message).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    /// One side of a framed connection: the extension's port or a Tick's socket.
    struct Peer<R, W> {
        reader: R,
        writer: W,
    }

    impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Peer<R, W> {
        async fn send(&mut self, message: Value) {
            let bytes = serde_json::to_vec(&message).unwrap();
            frame::write(&mut self.writer, &bytes, frame::FROM_EXTENSION_MAX_BYTES)
                .await
                .unwrap();
        }

        /// The next message, or `None` when the other side closed.
        async fn next(&mut self) -> Option<Value> {
            let message = tokio::time::timeout(
                WAIT,
                frame::read(&mut self.reader, frame::FROM_EXTENSION_MAX_BYTES),
            )
            .await
            .expect("a message within the wait")
            .ok()
            .flatten()?;
            Some(serde_json::from_slice(&message).unwrap())
        }

        async fn receive(&mut self) -> Value {
            self.next().await.expect("the connection is open")
        }

        /// Nothing arrives for a moment.
        async fn quiet(&mut self) {
            let next = tokio::time::timeout(
                Duration::from_millis(200),
                frame::read(&mut self.reader, frame::FROM_EXTENSION_MAX_BYTES),
            )
            .await;
            assert!(
                next.is_err(),
                "{:?}",
                next.map(|message| message
                    .ok()
                    .flatten()
                    .map(|m| String::from_utf8_lossy(&m).into_owned()))
            );
        }
    }

    type Extension = Peer<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;
    type TickPeer = Peer<OwnedReadHalf, OwnedWriteHalf>;

    /// A host serving `directory` for a stand-in extension; dropping the extension closes the
    /// host's input, as Chrome does when the port disconnects.
    fn start(directory: &Path) -> (Extension, tokio::task::JoinHandle<io::Result<()>>) {
        let (extension, host) = tokio::io::duplex(1024 * 1024);
        let (host_input, host_output) = tokio::io::split(host);
        let directory = directory.to_owned();
        let task = tokio::spawn(async move { relay(host_input, host_output, &directory).await });
        let (reader, writer) = tokio::io::split(extension);
        (Peer { reader, writer }, task)
    }

    fn catalog(names: &[&str]) -> Value {
        names
            .iter()
            .map(|name| json!({"name": name, "description": "A tool.", "input_schema": {"type": "object"}, "mutates": false}))
            .collect()
    }

    fn hello(names: &[&str]) -> Value {
        json!({"type": "hello", "protocol": 2, "extension_version": "0.2.0", "catalog": catalog(names)})
    }

    async fn served(directory: &Path) -> Extension {
        let (mut extension, _task) = start(directory);
        extension.send(hello(&["read_page"])).await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "welcome", "protocol": 2})
        );
        extension
    }

    async fn connect(directory: &Path) -> io::Result<TickPeer> {
        let (reader, writer) = socket::connect(directory).await?.into_split();
        Ok(Peer { reader, writer })
    }

    /// A Tick that opened `session` titled `title` and got the extension's hello.
    async fn open(
        directory: &Path,
        extension: &mut Extension,
        session: &str,
        title: &str,
    ) -> TickPeer {
        let mut tick = connect(directory).await.unwrap();
        tick.send(json!({"type": "open", "protocol": 2, "session": session, "title": title}))
            .await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "session_opened", "session": session, "title": title})
        );
        assert_eq!(
            tick.receive().await,
            json!({"type": "hello", "extension_version": "0.2.0", "catalog": catalog(&["read_page"])})
        );
        tick
    }

    fn directory() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("s");
        (root, directory)
    }

    #[tokio::test]
    async fn two_sessions_share_one_extension_and_each_hears_only_its_own() {
        let (_root, directory) = directory();
        let mut extension = served(&directory).await;
        let mut first = open(&directory, &mut extension, "hosted:a", "ticks").await;
        let mut second = open(&directory, &mut extension, "hosted:b", "ticks").await;

        // Calls with the same id from both sessions reach the extension told apart by session,
        // with the time left and the deadline as the Tick wrote them, if it did.
        first
            .send(json!({"type": "call", "id": 1, "tool": "read_page", "args": {"tab_id": 4}, "timeout_ms": 54980, "deadline_ms": 1_762_700_055_000_u64}))
            .await;
        second
            .send(json!({"type": "call", "id": 1, "tool": "read_page", "args": {"tab_id": 9}}))
            .await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "call", "session": "hosted:a", "id": 1, "tool": "read_page", "args": {"tab_id": 4}, "timeout_ms": 54980, "deadline_ms": 1_762_700_055_000_u64})
        );
        assert_eq!(
            extension.receive().await,
            json!({"type": "call", "session": "hosted:b", "id": 1, "tool": "read_page", "args": {"tab_id": 9}})
        );
        second.send(json!({"type": "stop"})).await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "stop", "session": "hosted:b"})
        );

        // Results and state go back only to their own session, without the session.
        extension.send(json!({"type": "result", "session": "hosted:b", "id": 1, "ok": true, "result": "b"})).await;
        extension
            .send(json!({"type": "update", "session": "hosted:a", "state": {"tabs": []}}))
            .await;
        extension.send(json!({"type": "result", "session": "hosted:a", "id": 1, "ok": true, "result": "a"})).await;
        // For a session that is not open, nothing goes anywhere.
        extension
            .send(json!({"type": "result", "session": "hosted:z", "id": 1, "ok": true}))
            .await;
        assert_eq!(
            second.receive().await,
            json!({"type": "result", "id": 1, "ok": true, "result": "b"})
        );
        assert_eq!(
            first.receive().await,
            json!({"type": "update", "state": {"tabs": []}})
        );
        assert_eq!(
            first.receive().await,
            json!({"type": "result", "id": 1, "ok": true, "result": "a"})
        );
        second.quiet().await;

        // A Tick that goes away closes its session; the other keeps going.
        drop(first);
        assert_eq!(
            extension.receive().await,
            json!({"type": "session_closed", "session": "hosted:a"})
        );
        extension
            .send(json!({"type": "result", "session": "hosted:a", "id": 2, "ok": true}))
            .await;
        second
            .send(json!({"type": "call", "id": 2, "tool": "read_page", "args": {}}))
            .await;
        assert_eq!(extension.receive().await["session"], "hosted:b");

        // A new hello reaches every open session.
        extension.send(hello(&["read_page", "tabs"])).await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "welcome", "protocol": 2})
        );
        assert_eq!(
            second.receive().await["catalog"],
            catalog(&["read_page", "tabs"])
        );
    }

    #[tokio::test]
    async fn the_extension_hears_the_title_of_each_sessions_chat_when_it_opens_and_when_it_changes()
    {
        let (_root, directory) = directory();
        let mut extension = served(&directory).await;
        let mut titled = connect(&directory).await.unwrap();
        titled
            .send(json!({"type": "open", "protocol": 2, "session": "hosted:a", "title": "ticks", "chat_title": "Fix the login page"}))
            .await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "session_opened", "session": "hosted:a", "title": "ticks", "chat_title": "Fix the login page"})
        );
        assert_eq!(titled.receive().await["type"], "hello");
        // A session opened without one has none until its Tick says it.
        let mut untitled = open(&directory, &mut extension, "hosted:b", "ticks").await;
        untitled
            .send(json!({"type": "chat_title", "chat_title": "Plan the trip"}))
            .await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "chat_title", "session": "hosted:b", "chat_title": "Plan the trip"})
        );
        titled
            .send(json!({"type": "chat_title", "chat_title": "Fix the login redirect"}))
            .await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "chat_title", "session": "hosted:a", "chat_title": "Fix the login redirect"})
        );

        // A blank title is malformed; it closes that session alone.
        untitled
            .send(json!({"type": "chat_title", "chat_title": " "}))
            .await;
        let refused = untitled.receive().await;
        assert_eq!(refused["code"], "malformed", "{refused}");
        assert_eq!(
            extension.receive().await,
            json!({"type": "session_closed", "session": "hosted:b"})
        );
        titled.quiet().await;
    }

    #[tokio::test]
    async fn a_tick_is_refused_for_another_version_a_session_in_use_or_no_open() {
        let (_root, directory) = directory();
        let mut extension = served(&directory).await;
        let _held = open(&directory, &mut extension, "hosted:a", "ticks").await;

        let mut old = connect(&directory).await.unwrap();
        old.send(json!({"type": "open", "protocol": 1, "session": "hosted:o", "title": "old"}))
            .await;
        assert_eq!(
            old.receive().await,
            json!({"type": "refused", "protocol": 2, "code": "protocol", "message": "the Tick speaks relay protocol 1 and this host speaks 2; update the one that is older"})
        );
        assert_eq!(old.next().await, None);

        let mut twin = connect(&directory).await.unwrap();
        twin.send(json!({"type": "open", "protocol": 2, "session": "hosted:a", "title": "ticks"}))
            .await;
        let refused = twin.receive().await;
        assert_eq!(refused["code"], "session");
        assert_eq!(twin.next().await, None);

        let mut eager = connect(&directory).await.unwrap();
        eager
            .send(json!({"type": "call", "id": 1, "tool": "read_page", "args": {}}))
            .await;
        assert_eq!(eager.receive().await["code"], "malformed");
        assert_eq!(eager.next().await, None);

        // None of them reached the extension.
        extension.quiet().await;
    }

    #[tokio::test]
    async fn a_call_too_large_for_the_extension_fails_back_to_its_tick() {
        let (_root, directory) = directory();
        let mut extension = served(&directory).await;
        let mut tick = open(&directory, &mut extension, "hosted:a", "ticks").await;
        // A call exactly at the limit of a Tick's message, which the session pushes over it.
        let call =
            |text: &str| json!({"type": "call", "id": 3, "tool": "type", "args": {"text": text}});
        let room = frame::TO_EXTENSION_MAX_BYTES - serde_json::to_vec(&call("")).unwrap().len();
        tick.send(call(&"x".repeat(room))).await;
        let failed = tick.receive().await;
        assert_eq!(
            (
                failed["type"].as_str(),
                failed["id"].as_u64(),
                failed["ok"].as_bool()
            ),
            (Some("result"), Some(3), Some(false))
        );
        extension.quiet().await;
    }

    #[tokio::test]
    async fn an_extension_of_another_version_or_with_a_bad_catalog_is_served_nothing() {
        let (_root, directory) = directory();
        let (mut extension, _task) = start(&directory);
        extension
            .send(json!({"type": "hello", "protocol": 1, "extension_version": "0.1.0", "catalog": []}))
            .await;
        assert_eq!(
            extension.receive().await,
            json!({"type": "refused", "protocol": 2, "code": "protocol", "message": "the extension speaks relay protocol 1 and this host speaks 2; update the one that is older"})
        );
        assert!(connect(&directory).await.is_err());
        extension.send(hello(&["tabs", "tabs"])).await;
        let refused = extension.receive().await;
        assert_eq!(refused["code"], "catalog");
        assert!(
            refused["message"].as_str().unwrap().contains("duplicate"),
            "{refused}"
        );
        assert!(connect(&directory).await.is_err());
        // A hello it accepts serves.
        extension.send(hello(&["tabs"])).await;
        assert_eq!(extension.receive().await["type"], "welcome");
        assert!(connect(&directory).await.is_ok());
    }

    #[tokio::test]
    async fn a_malformed_message_from_the_extension_closes_every_session_and_the_socket() {
        let (_root, directory) = directory();
        let mut extension = served(&directory).await;
        let mut first = open(&directory, &mut extension, "hosted:a", "ticks").await;
        let mut second = open(&directory, &mut extension, "hosted:b", "docs").await;
        extension.send(json!({"type": "update", "state": {}})).await;
        let mut closed = vec![extension.receive().await, extension.receive().await];
        closed.sort_by_key(|message| message["session"].as_str().map(str::to_owned));
        assert_eq!(
            closed,
            [
                json!({"type": "session_closed", "session": "hosted:a"}),
                json!({"type": "session_closed", "session": "hosted:b"}),
            ]
        );
        let refused = extension.receive().await;
        assert_eq!(refused["code"], "malformed");
        assert_eq!(first.next().await, None);
        assert_eq!(second.next().await, None);
        assert!(connect(&directory).await.is_err());
        assert!(!socket::is_served(&directory));
    }

    #[tokio::test]
    async fn of_two_browsers_the_first_serves_and_the_second_takes_over_when_it_closes() {
        let (_root, directory) = directory();
        let mut chrome = served(&directory).await;
        let mut tick = open(&directory, &mut chrome, "hosted:a", "ticks").await;

        let (mut edge, edge_task) = start(&directory);
        edge.send(hello(&["read_page"])).await;
        assert_eq!(
            edge.receive().await,
            json!({"type": "refused", "protocol": 2, "code": "busy", "message": "another browser is serving Tickloop; close it there to use this one"})
        );
        // It says so once while it keeps trying.
        tokio::time::timeout(Duration::from_millis(2500), edge.quiet())
            .await
            .ok();

        // Chrome closes: its host exits, its sessions' Ticks lose it, and Edge's host takes over.
        drop(chrome);
        assert_eq!(tick.next().await, None);
        assert_eq!(
            edge.receive().await,
            json!({"type": "welcome", "protocol": 2})
        );
        let _tick = open(&directory, &mut edge, "hosted:a", "ticks").await;
        drop(edge);
        edge_task.await.unwrap().unwrap();
        assert!(!socket::is_served(&directory));
    }

    #[test]
    fn chrome_launches_the_host_with_the_extensions_origin() {
        let arguments = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let launch = launched_by_browser(arguments(&[
            "chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/",
            "--parent-window=0",
        ]))
        .unwrap();
        assert_eq!(
            launch.origin,
            "chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/"
        );
        assert!(is_allowed_origin(&launch.origin));
        assert_eq!(launch.socket_directory, None);
        let launch = launched_by_browser(arguments(&[
            "chrome-extension://other/",
            "--socket-dir",
            "/tmp/s",
        ]))
        .unwrap();
        assert!(!is_allowed_origin(&launch.origin));
        assert_eq!(launch.socket_directory, Some(PathBuf::from("/tmp/s")));
        assert!(launched_by_browser(arguments(&["browser"])).is_none());
        assert!(launched_by_browser(arguments(&[])).is_none());
    }
}
