use crate::support::{run, tick_browser};

#[test]
fn help_names_the_tick_status_describe_and_the_native_host() {
    let home = tempfile::tempdir().unwrap();
    let (code, stdout, _) = run(tick_browser(home.path()).arg("--help"));
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Usage: tick-browser [OPTIONS] <COMMAND>"),
        "{stdout}"
    );
    for command in ["\n  browser", "\n  status", "\n  describe"] {
        assert!(stdout.contains(command), "missing {command}:\n{stdout}");
    }
    assert!(
        stdout.contains("tick-browser chrome-extension://<id>/"),
        "{stdout}"
    );
    assert!(stdout.contains("ai.tickloop.browser"), "{stdout}");
    assert!(stdout.contains("0  success"), "{stdout}");
    // Only tests name their own socket directory.
    assert!(!stdout.contains("--socket-dir"), "{stdout}");
}

#[test]
fn describe_offers_only_status_before_the_extension_connects() {
    let home = tempfile::tempdir().unwrap();
    let (code, stdout, stderr) = run(tick_browser(home.path()).arg("describe"));
    assert_eq!(code, 0, "{stderr}");
    let descriptors: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let browser = &descriptors[0];
    assert_eq!(browser["kind"], "browser");
    assert_eq!(browser["protocol"], 3);
    assert_eq!(
        browser["screen"],
        serde_json::json!({"min_rows": 6, "max_rows": 12})
    );
    assert_eq!(browser["tools"][0]["name"], "status");
    assert_eq!(browser["tools"][0]["mutates"], false);
    assert_eq!(browser["tools"].as_array().unwrap().len(), 1);
}

#[test]
fn the_socket_path_does_not_follow_the_environment() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let socket = |environment: &[(&str, &std::path::Path)]| {
        let mut command = tick_browser(home.path());
        command.arg("status");
        for (name, value) in environment {
            command.env(name, value);
        }
        let (code, stdout, stderr) = run(&mut command);
        assert_eq!(code, 0, "{stderr}");
        let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        status["socket"].as_str().unwrap().to_owned()
    };
    let plain = socket(&[]);
    let shell = socket(&[
        ("TMPDIR", elsewhere.path()),
        ("XDG_RUNTIME_DIR", elsewhere.path()),
    ]);
    assert_eq!(plain, shell);
    assert!(plain.ends_with("/tick.sock"), "{plain}");
    assert!(
        !plain.starts_with(elsewhere.path().to_str().unwrap()),
        "{plain}"
    );
}

#[test]
fn status_reports_the_registration_without_writing_it() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join("s");
    let (code, stdout, stderr) = run(tick_browser(home.path())
        .args(["status", "--socket-dir"])
        .arg(&directory));
    assert_eq!(code, 0, "{stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(status["registration"], serde_json::json!([]));
    assert_eq!(status["host_running"], false);
    assert_eq!(status["host_name"], "ai.tickloop.browser");

    #[cfg(target_os = "macos")]
    let chrome = home
        .path()
        .join("Library/Application Support/Google/Chrome");
    #[cfg(not(target_os = "macos"))]
    let chrome = home.path().join(".config/google-chrome");
    std::fs::create_dir_all(&chrome).unwrap();
    let (_, stdout, _) = run(tick_browser(home.path())
        .args(["status", "--socket-dir"])
        .arg(&directory));
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(status["registration"][0]["browser"], "Google Chrome");
    assert_eq!(status["registration"][0]["status"], "missing");
    assert!(!chrome.join("NativeMessagingHosts").exists());
    assert!(!directory.exists());
}

#[tokio::test]
async fn status_tells_a_serving_host_by_its_lock_without_connecting_to_it() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join("s");
    let status = || {
        let (code, stdout, stderr) = run(tick_browser(home.path())
            .args(["status", "--socket-dir"])
            .arg(&directory));
        assert_eq!(code, 0, "{stderr}");
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap()
    };
    let listening = tick_browser::socket::listen(&directory).unwrap();
    assert_eq!(status()["host_running"], true);
    // The host saw no connection, so no session opened.
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            listening.listener.accept()
        )
        .await
        .is_err()
    );
    drop(listening);
    // A process another test forks at this moment holds a copy of the lock until it executes its
    // program, so the lock may outlive the host by that long.
    let mut running = status()["host_running"].clone();
    for _ in 0..50 {
        if running == false {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        running = status()["host_running"].clone();
    }
    assert_eq!(running, false);
}

#[test]
fn a_tick_instance_refuses_the_one_shot_commands() {
    let home = tempfile::tempdir().unwrap();
    for command in ["describe", "status"] {
        let (code, stdout, stderr) = run(tick_browser(home.path())
            .env("TICKLOOP_INSTANCE_ID", "hosted:one-shot")
            .arg(command));
        assert_eq!(code, 2, "{command}: {stderr}");
        assert!(stdout.is_empty(), "{command}: {stdout}");
        assert!(stderr.contains("requires a live Tick"), "{stderr}");
    }
}

#[test]
fn a_non_utf8_locale_is_rejected_before_serving() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join("s");
    let (code, stdout, stderr) = run(tick_browser(home.path())
        .env("LC_ALL", "C")
        .args(["browser", "--socket-dir"])
        .arg(&directory));
    assert_eq!(code, 2, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.starts_with("tick-browser: "), "{stderr}");
    assert!(!directory.exists());
}
