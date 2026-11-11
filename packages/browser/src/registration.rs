//! Registers `tick-browser` as the native messaging host the extension connects to: the host
//! manifest in the user data directory of each Chromium browser the user has. Installing the package
//! runs nothing, so the Tick writes these when it opens.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{EXTENSION_IDS, HOST_NAME, extension_origin};

/// A Chromium browser that reads native messaging hosts from its user data directory.
pub struct Browser {
    pub name: &'static str,
    /// The user data directory, relative to the home directory.
    pub data_directory: &'static str,
}

#[cfg(target_os = "macos")]
pub const BROWSERS: [Browser; 3] = [
    Browser {
        name: "Google Chrome",
        data_directory: "Library/Application Support/Google/Chrome",
    },
    Browser {
        name: "Chromium",
        data_directory: "Library/Application Support/Chromium",
    },
    Browser {
        name: "Microsoft Edge",
        data_directory: "Library/Application Support/Microsoft Edge",
    },
];

#[cfg(not(target_os = "macos"))]
pub const BROWSERS: [Browser; 3] = [
    Browser {
        name: "Google Chrome",
        data_directory: ".config/google-chrome",
    },
    Browser {
        name: "Chromium",
        data_directory: ".config/chromium",
    },
    Browser {
        name: "Microsoft Edge",
        data_directory: ".config/microsoft-edge",
    },
];

/// The native messaging host manifest that starts `executable` for the configured extensions.
pub fn manifest(executable: &Path) -> Value {
    json!({
        "name": HOST_NAME,
        "description": "Tickloop Browser Tick relay",
        "path": executable,
        "type": "stdio",
        "allowed_origins": EXTENSION_IDS.iter().map(|id| extension_origin(id)).collect::<Vec<_>>(),
    })
}

/// Where a browser whose user data directory is `data_directory` looks for this host's manifest.
fn manifest_path(data_directory: &Path) -> PathBuf {
    data_directory
        .join("NativeMessagingHosts")
        .join(format!("{HOST_NAME}.json"))
}

/// How one browser's registration stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    /// The manifest was already there as it should be.
    Current,
    /// There is no manifest; [`register`] writes it.
    Missing,
    /// A manifest differs, such as one naming another program; [`register`] replaces it.
    Outdated {
        /// The program it names, when it names one.
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_path: Option<String>,
    },
    /// [`register`] wrote the manifest just now, where there was none.
    Written,
    /// [`register`] replaced a manifest that differed just now.
    Replaced {
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_path: Option<String>,
    },
    Failed {
        error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub browser: &'static str,
    pub manifest: PathBuf,
    #[serde(flatten)]
    pub status: Status,
}

/// How the host is registered with each browser the user has, under `home`, without writing.
pub fn inspect(home: &Path, executable: &Path) -> Vec<Entry> {
    let expected = manifest(executable);
    BROWSERS
        .iter()
        .filter(|browser| home.join(browser.data_directory).is_dir())
        .map(|browser| {
            let path = manifest_path(&home.join(browser.data_directory));
            let status = match std::fs::read(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Status::Missing,
                Err(_) => Status::Outdated {
                    previous_path: None,
                },
                Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                    Ok(written) if written == expected => Status::Current,
                    written => Status::Outdated {
                        previous_path: written
                            .ok()
                            .and_then(|written| written["path"].as_str().map(str::to_owned)),
                    },
                },
            };
            Entry {
                browser: browser.name,
                manifest: path,
                status,
            }
        })
        .collect()
}

/// Writes the manifest for each browser the user has whose manifest is missing or differs, such as
/// one naming where the package was before, or another Tickloop installation's program. The host
/// name is Tickloop's own, so the Tick that opens takes it; the entry says what it replaced.
pub fn register(home: &Path, executable: &Path) -> Vec<Entry> {
    let mut bytes = serde_json::to_vec_pretty(&manifest(executable)).expect("manifests serialize");
    bytes.push(b'\n');
    inspect(home, executable)
        .into_iter()
        .map(|mut entry| {
            let written = match &entry.status {
                Status::Missing => Status::Written,
                Status::Outdated { previous_path } => Status::Replaced {
                    previous_path: previous_path.clone(),
                },
                _ => return entry,
            };
            entry.status = match write(&entry.manifest, &bytes) {
                Ok(()) => written,
                Err(error) => Status::Failed {
                    error: error.to_string(),
                },
            };
            entry
        })
        .collect()
}

/// Writes `bytes` to `path` through a temporary file beside it, so a browser never reads half a
/// manifest.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path.parent().expect("a manifest path has a directory");
    std::fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(".{HOST_NAME}.json.{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// The program the manifests name: this one, by its absolute path.
pub fn current_executable() -> std::io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_starts_this_program_for_the_configured_extensions_only() {
        let manifest = manifest(Path::new("/opt/ticks/browser/bin/tick-browser"));
        assert_eq!(
            manifest,
            json!({
                "name": "ai.tickloop.browser",
                "description": "Tickloop Browser Tick relay",
                "path": "/opt/ticks/browser/bin/tick-browser",
                "type": "stdio",
                "allowed_origins": ["chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/"],
            })
        );
    }

    #[test]
    fn only_the_browsers_the_user_has_are_registered_and_a_moved_program_is_rewritten() {
        let home = tempfile::tempdir().unwrap();
        assert!(register(home.path(), Path::new("/old/tick-browser")).is_empty());

        let chrome = home.path().join(BROWSERS[0].data_directory);
        std::fs::create_dir_all(&chrome).unwrap();
        let path = chrome.join("NativeMessagingHosts/ai.tickloop.browser.json");
        let entries = register(home.path(), Path::new("/old/tick-browser"));
        assert_eq!(
            entries,
            [Entry {
                browser: BROWSERS[0].name,
                manifest: path.clone(),
                status: Status::Written,
            }]
        );
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written, manifest(Path::new("/old/tick-browser")));
        assert_eq!(
            register(home.path(), Path::new("/old/tick-browser"))[0].status,
            Status::Current
        );

        // The package moved, or another installation opened its Tick: the manifest names the
        // program where it is now, and says which one it named before.
        let before = Some("/old/tick-browser".to_owned());
        assert_eq!(
            inspect(home.path(), Path::new("/new/tick-browser"))[0].status,
            Status::Outdated {
                previous_path: before.clone()
            }
        );
        let entries = register(home.path(), Path::new("/new/tick-browser"));
        assert_eq!(
            entries[0].status,
            Status::Replaced {
                previous_path: before
            }
        );
        assert_eq!(
            serde_json::to_value(&entries[0]).unwrap(),
            json!({
                "browser": "Google Chrome",
                "manifest": path,
                "status": "replaced",
                "previous_path": "/old/tick-browser",
            })
        );
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["path"], "/new/tick-browser");
        let leftovers = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1);

        // A manifest edited by hand into something that is not one is replaced, and says so.
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(
            register(home.path(), Path::new("/new/tick-browser"))[0].status,
            Status::Replaced {
                previous_path: None
            }
        );
    }

    #[test]
    fn a_manifest_that_cannot_be_written_is_reported() {
        let home = tempfile::tempdir().unwrap();
        let edge = home.path().join(BROWSERS[2].data_directory);
        std::fs::create_dir_all(&edge).unwrap();
        // A file where the directory of manifests belongs.
        std::fs::write(edge.join("NativeMessagingHosts"), b"").unwrap();
        let entries = register(home.path(), Path::new("/bin/tick-browser"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].browser, "Microsoft Edge");
        assert!(
            matches!(entries[0].status, Status::Failed { .. }),
            "{entries:?}"
        );
    }
}
