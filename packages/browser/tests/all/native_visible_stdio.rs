#![cfg(unix)]

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::{fd::FromRawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::support::{Extension, UTF8_LOCALE};

const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// `tick-browser browser` as its host launches it for the instance `instance`, in the directory
/// `<home>/<title>`: a Tick Instance with the protocol on stdin/stdout and its view on the
/// controlling terminal of a new session, which `screen` collects.
fn hosted_browser(
    home: &Path,
    socket_directory: &Path,
    instance: &str,
    title: &str,
) -> (Child, File, mpsc::Receiver<Value>, Arc<Mutex<Vec<u8>>>) {
    let directory = home.join(title);
    std::fs::create_dir_all(&directory).unwrap();
    let (mut master, mut slave) = (0, 0);
    let mut size = libc::winsize {
        ws_row: 6,
        ws_col: 30,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        },
        0
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_tick-browser"));
    command
        .args(["--language", "en", "browser", "--socket-dir"])
        .arg(socket_directory)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("TICKLOOP_INSTANCE_ID", instance)
        .current_dir(&directory)
        .env("LANG", UTF8_LOCALE)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::dup2(slave, libc::STDERR_FILENO);
            if slave > libc::STDERR_FILENO {
                libc::close(slave);
            }
            libc::close(master);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    unsafe {
        libc::close(slave);
    }
    let tty = unsafe { File::from_raw_fd(master) };
    let mut terminal = tty.try_clone().unwrap();
    let screen = Arc::new(Mutex::new(Vec::new()));
    let drawn = Arc::clone(&screen);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(length) = terminal.read(&mut chunk) {
            if length == 0 {
                break;
            }
            drawn.lock().unwrap().extend_from_slice(&chunk[..length]);
        }
    });
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            sender
                .send(serde_json::from_str(&line).expect("stdout must contain JSON-RPC only"))
                .unwrap();
        }
    });
    (child, tty, receiver, screen)
}

fn frame_where(frames: &mpsc::Receiver<Value>, matches: impl Fn(&Value) -> bool) -> Value {
    loop {
        let frame = frames.recv_timeout(FRAME_TIMEOUT).unwrap();
        if matches(&frame) {
            return frame;
        }
    }
}

fn request(child: &mut Child, id: u64, method: &str, params: Value) {
    writeln!(
        child.stdin.as_mut().unwrap(),
        "{}",
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
    )
    .unwrap();
}

fn describe(child: &mut Child, frames: &mpsc::Receiver<Value>, id: u64) -> Value {
    request(child, id, "tick/describe", json!({}));
    frame_where(frames, |frame| frame["id"] == id)["result"].clone()
}

/// Waits until the terminal has shown `text`.
fn wait_for_screen(screen: &Mutex<Vec<u8>>, text: &str) {
    let deadline = Instant::now() + FRAME_TIMEOUT;
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&screen.lock().unwrap()).contains(text) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "the screen never showed {text:?}: {:?}",
        String::from_utf8_lossy(&screen.lock().unwrap())
    );
}

fn wait_for_exit(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
        panic!("tick-browser browser did not exit after SIGTERM");
    }
    assert!(child.wait().unwrap().success());
}

fn hello() -> Value {
    json!({
        "type": "hello",
        "protocol": 2,
        "extension_version": "0.2.0",
        "catalog": [
            {"name": "read_page", "description": "Read a tab of the session's group.",
             "input_schema": {"type": "object", "properties": {"tab_id": {"type": "integer"}}, "required": ["tab_id"]},
             "mutates": false},
            {"name": "computer", "description": "Click or type in a tab of the session's group.",
             "input_schema": {"type": "object", "properties": {"tab_id": {"type": "integer"}, "action": {"type": "string"}}},
             "mutates": true}
        ]
    })
}

/// A browser whose extension says hello and is served.
fn browser(home: &Path, sockets: &Path) -> Extension {
    let mut extension = Extension::start(home, sockets);
    extension.send(&hello());
    assert_eq!(
        extension.receive(),
        json!({"type": "welcome", "protocol": 2})
    );
    extension
}

fn update_where(frames: &mpsc::Receiver<Value>, connection: &str) -> Value {
    frame_where(frames, |frame| {
        frame["method"] == "tick/update" && frame["params"]["data"]["connection"] == connection
    })
}

fn stop(child: &mut Child) {
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    wait_for_exit(child);
}

#[test]
fn a_hosted_browser_waits_for_the_browser_then_relays_through_the_native_host_and_waits_again() {
    let home = tempfile::tempdir().unwrap();
    #[cfg(target_os = "macos")]
    let chrome = home
        .path()
        .join("Library/Application Support/Google/Chrome");
    #[cfg(not(target_os = "macos"))]
    let chrome = home.path().join(".config/google-chrome");
    std::fs::create_dir_all(&chrome).unwrap();
    let sockets = home.path().join("s");
    let session = "hosted:visible-browser";
    // The Tick opens before the browser.
    let (mut child, mut tty, frames, screen) =
        hosted_browser(home.path(), &sockets, session, "ticks");

    let waiting = frame_where(&frames, |frame| frame["method"] == "tick/update");
    assert_eq!(waiting["params"]["data"]["connection"], "waiting");
    let before = describe(&mut child, &frames, 1);
    assert_eq!(before["kind"], "browser");
    assert_eq!(before["tools"][0]["name"], "status");
    request(
        &mut child,
        2,
        "tick/call",
        json!({"tool": "status", "args": {}, "revision": before["revision"]}),
    );
    let status = frame_where(&frames, |frame| frame["id"] == 2)["result"].clone();
    assert_eq!(status["connection"], "waiting");
    assert_eq!(status["session"], session);
    assert_eq!(status["title"], "ticks");
    assert_eq!(status["registration"][0]["browser"], "Google Chrome");
    assert_eq!(status["registration"][0]["status"], "written");
    // Opening the Tick registered this program as the native host, for the configured extension only.
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(chrome.join("NativeMessagingHosts/ai.tickloop.browser.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        Path::new(manifest["path"].as_str().unwrap()),
        Path::new(env!("CARGO_BIN_EXE_tick-browser"))
            .canonicalize()
            .unwrap()
    );
    assert_eq!(
        manifest["allowed_origins"],
        json!(["chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/"])
    );
    // The Tick serves no socket of its own.
    assert!(!sockets.join("tick.sock").exists());
    wait_for_screen(&screen, "Waiting for the browser");
    // The card tells the host how many rows its content needs.
    wait_for_screen(&screen, "\x1b]7777;tickloop;rows=");
    // The keys that move between cards are handed on to the host.
    wait_for_screen(&screen, "\x1b]7777;tickloop;bubble=on\x07");
    tty.write_all(b"\x1b[A\t").unwrap();
    wait_for_screen(&screen, "\x1b]7777;tickloop;key=up\x07");
    wait_for_screen(&screen, "\x1b]7777;tickloop;key=tab\x07");

    // The browser opens: its native host serves, and the Tick opens its session there.
    let mut extension = browser(home.path(), &sockets);
    assert_eq!(
        extension.receive(),
        json!({"type": "session_opened", "session": session, "title": "ticks"})
    );
    extension.send(&json!({"type": "update", "session": session, "state": {
        "tabs": [{"id": 4, "title": "Example Domain", "url": "https://example.com/", "active": true}]
    }}));
    let connected = frame_where(&frames, |frame| {
        frame["method"] == "tick/update"
            && frame["params"]["data"]["tabs"][0]["title"] == "Example Domain"
    });
    assert_eq!(connected["params"]["data"]["connection"], "connected");
    assert_eq!(connected["params"]["data"]["extension_version"], "0.2.0");
    let served = describe(&mut child, &frames, 3);
    let names: Vec<_> = served["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["read_page", "computer"]);
    assert_ne!(served["revision"], before["revision"]);
    wait_for_screen(&screen, "Example Domain");

    // A call goes to the extension with the session, and its result comes back.
    request(
        &mut child,
        4,
        "tick/call",
        json!({"tool": "read_page", "args": {"tab_id": 4}, "revision": served["revision"]}),
    );
    let call = extension.receive_where(|message| message["type"] == "call");
    assert_eq!(call["session"], session);
    assert_eq!(call["tool"], "read_page");
    assert_eq!(call["args"], json!({"tab_id": 4}));
    extension.send(&json!({"type": "result", "session": session, "id": call["id"], "ok": true, "result": "- heading \"Example Domain\""}));
    let answer = frame_where(&frames, |frame| frame["id"] == 4);
    assert_eq!(answer["result"], "- heading \"Example Domain\"");

    // The extension reports the user navigating; the Agent is told.
    extension.send(&json!({"type": "update", "session": session, "state": {
        "tabs": [{"id": 4, "title": "IANA", "url": "https://www.iana.org/", "active": true}],
        "last_event": "The user navigated tab 4 to https://www.iana.org/"
    }}));
    let navigated = frame_where(&frames, |frame| {
        frame["method"] == "tick/update" && frame["params"]["data"]["tabs"][0]["title"] == "IANA"
    });
    assert_eq!(
        navigated["params"]["data"]["last_event"],
        "The user navigated tab 4 to https://www.iana.org/"
    );
    assert!(
        navigated["params"]["seq"].as_u64().unwrap() > connected["params"]["seq"].as_u64().unwrap()
    );

    // A change the extension never answers: the browser closes, and the outcome is reported
    // unknown.
    request(
        &mut child,
        5,
        "tick/call",
        json!({"tool": "computer", "args": {"tab_id": 4, "action": "left_click"}, "revision": served["revision"]}),
    );
    extension.receive_where(|message| message["type"] == "call" && message["tool"] == "computer");
    assert!(extension.disconnect().success());
    // The stream writes the newest update ahead of the answers queued with it, so either comes first.
    let (mut unknown, mut gone) = (None, None);
    while unknown.is_none() || gone.is_none() {
        let frame = frames.recv_timeout(FRAME_TIMEOUT).unwrap();
        if frame["id"] == 5 {
            unknown = Some(frame);
        } else if frame["method"] == "tick/update"
            && frame["params"]["data"]["connection"] == "waiting"
        {
            gone = Some(frame);
        }
    }
    let (unknown, gone) = (unknown.unwrap(), gone.unwrap());
    assert_eq!(unknown["error"]["code"], -32000);
    assert_eq!(unknown["error"]["data"]["code"], "outcome_unknown");
    assert_eq!(gone["params"]["data"]["last_step"]["status"], "unknown");
    // A call prepared against the extension's catalog is refused unrun.
    request(
        &mut child,
        6,
        "tick/call",
        json!({"tool": "read_page", "args": {"tab_id": 4}, "revision": served["revision"]}),
    );
    assert_eq!(
        frame_where(&frames, |frame| frame["id"] == 6)["error"]["code"],
        -32004
    );
    assert_eq!(
        describe(&mut child, &frames, 7)["revision"],
        before["revision"]
    );
    assert!(!sockets.join("tick.sock").exists());

    // The browser opens again: the Tick, still running, opens its session there again.
    let extension = browser(home.path(), &sockets);
    assert_eq!(extension.receive()["type"], "session_opened");
    update_where(&frames, "connected");
    assert_eq!(
        describe(&mut child, &frames, 8)["revision"],
        served["revision"]
    );

    // The Tick exits: its session closes in the browser, which keeps serving.
    stop(&mut child);
    assert_eq!(
        extension.receive(),
        json!({"type": "session_closed", "session": session})
    );
    assert!(sockets.join("tick.sock").exists());
    assert!(extension.disconnect().success());
    assert!(!sockets.join("tick.sock").exists());
}

#[test]
fn two_sessions_use_one_browser_at_once() {
    let home = tempfile::tempdir().unwrap();
    let sockets = home.path().join("s");
    let mut extension = browser(home.path(), &sockets);
    let (mut first, _first_tty, first_frames, _) =
        hosted_browser(home.path(), &sockets, "hosted:first", "ticks");
    let (mut second, _second_tty, second_frames, _) =
        hosted_browser(home.path(), &sockets, "hosted:second", "docs");
    let mut opened = vec![extension.receive(), extension.receive()];
    opened.sort_by_key(|message| message["session"].as_str().map(str::to_owned));
    assert_eq!(
        opened,
        [
            json!({"type": "session_opened", "session": "hosted:first", "title": "ticks"}),
            json!({"type": "session_opened", "session": "hosted:second", "title": "docs"}),
        ]
    );
    update_where(&first_frames, "connected");
    update_where(&second_frames, "connected");
    let revision = describe(&mut first, &first_frames, 1)["revision"].clone();
    assert_eq!(
        describe(&mut second, &second_frames, 1)["revision"],
        revision
    );

    // Each session's state and results reach only its own Tick.
    for (session, title) in [("hosted:first", "First"), ("hosted:second", "Second")] {
        extension.send(&json!({"type": "update", "session": session, "state": {
            "tabs": [{"id": 1, "title": title, "url": "https://example.com/", "active": true}]
        }}));
    }
    for (frames, title) in [(&first_frames, "First"), (&second_frames, "Second")] {
        frame_where(frames, |frame| {
            frame["method"] == "tick/update" && frame["params"]["data"]["tabs"][0]["title"] == title
        });
    }
    for (child, id) in [(&mut first, 2), (&mut second, 3)] {
        request(
            child,
            id,
            "tick/call",
            json!({"tool": "read_page", "args": {"tab_id": 1}, "revision": revision}),
        );
    }
    let mut calls = vec![
        extension.receive_where(|message| message["type"] == "call"),
        extension.receive_where(|message| message["type"] == "call"),
    ];
    calls.sort_by_key(|message| message["session"].as_str().map(str::to_owned));
    for call in &calls {
        extension.send(&json!({"type": "result", "session": call["session"], "id": call["id"], "ok": true, "result": call["session"]}));
    }
    assert_eq!(
        frame_where(&first_frames, |frame| frame["id"] == 2)["result"],
        "hosted:first"
    );
    assert_eq!(
        frame_where(&second_frames, |frame| frame["id"] == 3)["result"],
        "hosted:second"
    );

    // One session closes; the other keeps the browser.
    stop(&mut first);
    assert_eq!(
        extension.receive(),
        json!({"type": "session_closed", "session": "hosted:first"})
    );
    request(
        &mut second,
        4,
        "tick/call",
        json!({"tool": "read_page", "args": {"tab_id": 1}, "revision": revision}),
    );
    let call = extension.receive_where(|message| message["type"] == "call");
    assert_eq!(call["session"], "hosted:second");
    extension.send(&json!({"type": "result", "session": "hosted:second", "id": call["id"], "ok": true, "result": "still here"}));
    assert_eq!(
        frame_where(&second_frames, |frame| frame["id"] == 4)["result"],
        "still here"
    );
    stop(&mut second);
    assert!(extension.disconnect().success());
}

#[test]
fn the_chats_title_reaches_the_browser_and_the_card_names_the_session_as_the_browser_does() {
    let home = tempfile::tempdir().unwrap();
    let sockets = home.path().join("s");
    let session = "hosted:titled";
    let (mut child, _tty, frames, screen) = hosted_browser(home.path(), &sockets, session, "ticks");
    update_where(&frames, "waiting");

    // Tickloop tells the Tick its Chat's title before any browser serves; the Tick takes it.
    request(
        &mut child,
        1,
        "tick/chat",
        json!({"title": "Fix the login page"}),
    );
    assert_eq!(
        frame_where(&frames, |frame| frame["id"] == 1),
        json!({"jsonrpc": "2.0", "id": 1, "result": {}})
    );
    // Other host requests it does not know are still refused, as every Tick does.
    request(&mut child, 2, "tick/unknown", json!({}));
    assert_eq!(
        frame_where(&frames, |frame| frame["id"] == 2)["error"]["code"],
        -32601
    );

    // The browser opens: the session opens with the title, and a new one follows at once.
    let mut extension = browser(home.path(), &sockets);
    assert_eq!(
        extension.receive(),
        json!({"type": "session_opened", "session": session, "title": "ticks", "chat_title": "Fix the login page"})
    );
    update_where(&frames, "connected");
    request(
        &mut child,
        3,
        "tick/chat",
        json!({"title": "Fix the login redirect"}),
    );
    frame_where(&frames, |frame| frame["id"] == 3);
    assert_eq!(
        extension.receive_where(|message| message["type"] == "chat_title"),
        json!({"type": "chat_title", "session": session, "chat_title": "Fix the login redirect"})
    );

    // The browser names the session; the card's title and the Tick's state use that name.
    extension.send(&json!({"type": "update", "session": session, "state": {
        "tabs": [{"id": 4, "title": "Login", "url": "https://example.com/login", "active": true}],
        "label": "Fix the login redirect",
        "group_color": "green",
    }}));
    let named = frame_where(&frames, |frame| {
        frame["method"] == "tick/update" && frame["params"]["data"]["label"].is_string()
    });
    assert_eq!(named["params"]["data"]["label"], "Fix the login redirect");
    assert_eq!(named["params"]["data"]["group_color"], "green");
    // The terminal gets only the cells that changed, so the name arrives in pieces; the dot
    // takes the group's color.
    wait_for_screen(&screen, "\x1b[38;5;2;49m●");
    wait_for_screen(&screen, "redirect ─");

    stop(&mut child);
    assert!(extension.disconnect().success());
}

#[test]
fn a_second_browser_waits_while_the_first_serves_and_takes_over_when_it_closes() {
    let home = tempfile::tempdir().unwrap();
    let sockets = home.path().join("s");
    let (mut child, _tty, frames, _screen) =
        hosted_browser(home.path(), &sockets, "hosted:one", "ticks");
    let chrome = browser(home.path(), &sockets);
    assert_eq!(chrome.receive()["type"], "session_opened");
    update_where(&frames, "connected");

    // Edge's extension connects too; its host finds the socket served.
    let mut edge = Extension::start(home.path(), &sockets);
    edge.send(&hello());
    assert_eq!(
        edge.receive(),
        json!({"type": "refused", "protocol": 2, "code": "busy", "message": "another browser is serving Tickloop; close it there to use this one"})
    );

    // Chrome closes: the Tick waits, Edge's host takes the socket, and the Tick opens its session
    // there.
    assert!(chrome.disconnect().success());
    update_where(&frames, "waiting");
    assert_eq!(edge.receive(), json!({"type": "welcome", "protocol": 2}));
    assert_eq!(
        edge.receive(),
        json!({"type": "session_opened", "session": "hosted:one", "title": "ticks"})
    );
    update_where(&frames, "connected");

    stop(&mut child);
    assert_eq!(edge.receive()["type"], "session_closed");
    assert!(edge.disconnect().success());
}

#[test]
fn native_visible_mode_requires_a_controlling_tty() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tick-browser"))
        .args(["browser", "--socket-dir"])
        .arg(home.path().join("s"))
        .env("TICKLOOP_INSTANCE_ID", "hosted:without-tty")
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("controlling TTY"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
