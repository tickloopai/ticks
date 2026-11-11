//! A stand-in for the extension: it starts `tick-browser` as Chrome starts its native host and
//! speaks Chrome's native messaging framing on its stdio.

use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use serde_json::Value;

pub const UTF8_LOCALE: &str = "en_US.UTF-8";
/// The development extension, which the host serves.
pub const EXTENSION_ORIGIN: &str = "chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/";
pub const MESSAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// `tick-browser` with a clean environment and `home` as the user's home directory.
pub fn tick_browser(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tick-browser"));
    command.env_clear();
    command.env("PATH", std::env::var("PATH").unwrap_or_default());
    command.env("LANG", UTF8_LOCALE);
    command.env("HOME", home);
    command
}

pub fn run(command: &mut Command) -> (i32, String, String) {
    let output = command.output().unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// Writes `message` as Chrome frames it: its length in native byte order, then its JSON.
pub fn write_message(writer: &mut impl Write, message: &Value) {
    let bytes = serde_json::to_vec(message).unwrap();
    writer
        .write_all(&(bytes.len() as u32).to_ne_bytes())
        .unwrap();
    writer.write_all(&bytes).unwrap();
    writer.flush().unwrap();
}

/// Reads one framed message; `None` at the end of the stream.
pub fn read_message(reader: &mut impl Read) -> Option<Value> {
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).ok()?;
    let mut bytes = vec![0; u32::from_ne_bytes(length) as usize];
    reader.read_exact(&mut bytes).ok()?;
    Some(serde_json::from_slice(&bytes).expect("the native host writes JSON messages"))
}

/// The native host as Chrome runs it for the extension, serving the socket in `socket_directory`.
pub struct Extension {
    pub host: Child,
    stdin: Option<ChildStdin>,
    messages: mpsc::Receiver<Value>,
}

impl Extension {
    pub fn start(home: &Path, socket_directory: &Path) -> Self {
        let mut host = tick_browser(home)
            .arg(EXTENSION_ORIGIN)
            .arg("--socket-dir")
            .arg(socket_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = host.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(message) = read_message(&mut stdout) {
                if sender.send(message).is_err() {
                    return;
                }
            }
        });
        let stdin = host.stdin.take();
        Self {
            host,
            stdin,
            messages,
        }
    }

    pub fn send(&mut self, message: &Value) {
        write_message(self.stdin.as_mut().unwrap(), message);
    }

    /// The next message from the host, failing the test after [`MESSAGE_TIMEOUT`].
    pub fn receive(&self) -> Value {
        self.messages
            .recv_timeout(MESSAGE_TIMEOUT)
            .expect("the native host sent nothing in time")
    }

    /// Skips messages until one `matches`.
    pub fn receive_where(&self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let message = self.receive();
            if matches(&message) {
                return message;
            }
        }
    }

    /// Closes the host's input as Chrome does when the extension disconnects, and waits for it to
    /// exit.
    pub fn disconnect(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + MESSAGE_TIMEOUT;
        while std::time::Instant::now() < deadline {
            if let Some(status) = self.host.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.host.kill().unwrap();
        panic!("the native host did not exit after its input closed");
    }
}

impl Drop for Extension {
    fn drop(&mut self) {
        let _ = self.host.kill();
        let _ = self.host.wait();
    }
}
