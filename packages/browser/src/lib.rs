//! The Browser Tick: the Agent's tab group in the user's own Chrome, Chromium or Edge, served by the
//! Tickloop browser extension. `tick-browser browser` is the Tick a host launches; Chrome launches
//! `tick-browser` itself as the native messaging host that relays between the extension and the Tick
//! over a local socket. `PROTOCOL.md` describes the relay.

pub mod catalog;
pub mod frame;
pub mod native_host;
pub mod registration;
pub mod relay;
pub mod socket;
pub mod tick;
mod view;

/// The Tick package this program comes from.
pub const PACKAGE: &str = "browser";

/// The native messaging host name the extension connects to.
pub const HOST_NAME: &str = "ai.tickloop.browser";

/// The extensions allowed to start the native host: the development build's id today. Add the Chrome
/// Web Store id here once the extension is published.
pub const EXTENSION_IDS: &[&str] = &["phamnhcllaeigpghejaebpihbmfnbcln"];

/// The Chrome Web Store page the user installs the extension from. None until the extension is
/// published there (ticket 09, `.scratch/chrome-extension/issues/09-release.md` in tickloop-cloud);
/// until then the card and the hint tell the development build's way, [`DEVELOPMENT_INSTALL`].
pub const EXTENSION_STORE_URL: Option<&str> = None;

/// Where a development build of the extension is installed from: loaded unpacked, in Developer
/// mode.
pub const DEVELOPMENT_INSTALL: &str = "chrome://extensions (turn on Developer mode, choose Load unpacked and pick the extension's folder)";

/// The origin Chrome names an extension by, as `allowed_origins` lists it and as it passes it to the
/// native host.
pub fn extension_origin(id: &str) -> String {
    format!("chrome-extension://{id}/")
}

/// Whether `origin`, as Chrome passes it to the native host, is one of [`EXTENSION_IDS`].
pub fn is_allowed_origin(origin: &str) -> bool {
    EXTENSION_IDS
        .iter()
        .any(|id| extension_origin(id) == origin)
}
