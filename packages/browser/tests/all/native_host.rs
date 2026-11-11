use std::{
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        net::UnixStream,
    },
    time::Duration,
};

use serde_json::json;

use crate::support::{EXTENSION_ORIGIN, Extension, read_message, run, tick_browser, write_message};

#[test]
fn the_host_serves_the_socket_once_its_extension_is_accepted_and_routes_a_session() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("s");
    let mut extension = Extension::start(root.path(), &directory);
    // Nothing is served before the extension's hello.
    std::thread::sleep(Duration::from_millis(200));
    assert!(UnixStream::connect(directory.join("tick.sock")).is_err());
    extension.send(&json!({"type": "hello", "protocol": 2, "extension_version": "0.2.0", "catalog": [], "anything": [1, 2]}));
    assert_eq!(
        extension.receive(),
        json!({"type": "welcome", "protocol": 2})
    );
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&directory), 0o700);
    assert_eq!(mode(&directory.join("tick.sock")), 0o600);

    // A Tick connects and opens its session.
    let mut tick = UnixStream::connect(directory.join("tick.sock")).unwrap();
    tick.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write_message(
        &mut tick,
        &json!({"type": "open", "protocol": 2, "session": "hosted:a", "title": "ticks"}),
    );
    assert_eq!(
        extension.receive(),
        json!({"type": "session_opened", "session": "hosted:a", "title": "ticks"})
    );
    assert_eq!(
        read_message(&mut tick).unwrap(),
        json!({"type": "hello", "extension_version": "0.2.0", "catalog": []})
    );
    write_message(
        &mut tick,
        &json!({"type": "call", "id": 1, "tool": "read_page", "args": {"tab_id": 4}}),
    );
    assert_eq!(
        extension.receive(),
        json!({"type": "call", "session": "hosted:a", "id": 1, "tool": "read_page", "args": {"tab_id": 4}})
    );
    extension.send(
        &json!({"type": "result", "session": "hosted:a", "id": 1, "ok": true, "result": "- main"}),
    );
    assert_eq!(
        read_message(&mut tick).unwrap(),
        json!({"type": "result", "id": 1, "ok": true, "result": "- main"})
    );

    // The Tick closes: its session closes.
    drop(tick);
    assert_eq!(
        extension.receive(),
        json!({"type": "session_closed", "session": "hosted:a"})
    );

    // The extension disconnects: Chrome closes the host's input, it exits, and the socket goes.
    assert!(extension.disconnect().success());
    assert!(!directory.join("tick.sock").exists());
}

#[test]
fn the_host_serves_only_the_configured_extensions() {
    let home = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(tick_browser(home.path())
        .arg("chrome-extension://abcdefghijklmnopabcdefghijklmnop/")
        .arg("--socket-dir")
        .arg(home.path()));
    assert_eq!(code, 2, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains("is not an extension this host serves"),
        "{stderr}"
    );
    assert!(EXTENSION_ORIGIN.starts_with("chrome-extension://"));
}

#[test]
fn the_host_refuses_a_socket_directory_others_can_enter() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("open");
    std::fs::DirBuilder::new()
        .mode(0o777)
        .create(&directory)
        .unwrap();
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .unwrap();
    let mut extension = Extension::start(root.path(), &directory);
    extension.send(
        &json!({"type": "hello", "protocol": 2, "extension_version": "0.2.0", "catalog": []}),
    );
    let refused = extension.receive();
    assert_eq!(refused["code"], "socket", "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("others cannot enter"),
        "{refused}"
    );
    assert!(!directory.join("tick.sock").exists());
    assert!(extension.disconnect().success());
}
