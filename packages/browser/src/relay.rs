//! The messages of the relay protocol in `PROTOCOL.md`: between the extension and the native host,
//! and between the native host and each session's Tick.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tick_protocol::TickTool;

/// The relay protocol version this program speaks.
pub const PROTOCOL: u32 = 2;

/// The most tabs the Tick keeps of the extension's state.
const MAX_TABS: usize = 32;
/// The most pending items the Tick keeps of the extension's state.
const MAX_PENDING: usize = 16;
const MAX_TITLE_CHARS: usize = 120;
const MAX_URL_CHARS: usize = 1024;
const MAX_SUMMARY_CHARS: usize = 300;
const MAX_VERSION_CHARS: usize = 64;
/// The longest refusal or error the Tick keeps, shows and sends: its text can quote what the
/// extension sent.
pub const MAX_MESSAGE_CHARS: usize = 400;
/// The longest tool name the Tick keeps of a call for its state.
pub const MAX_TOOL_CHARS: usize = 128;
/// The longest `session` a Tick names itself by.
pub const MAX_SESSION_CHARS: usize = 128;
/// The longest session `title`.
pub const MAX_SESSION_TITLE_CHARS: usize = 80;
/// The longest `chat_title`: the title of the Chat that opened the Tick.
pub const MAX_CHAT_TITLE_CHARS: usize = 200;
/// The longest `label` the Tick keeps of the extension's state.
const MAX_LABEL_CHARS: usize = 120;
/// The colors of Chrome's tab groups, as the extension names a session's `group_color`.
pub const GROUP_COLORS: [&str; 9] = [
    "grey", "blue", "red", "yellow", "green", "pink", "purple", "cyan", "orange",
];

/// Why the host refuses the extension or a Tick, as `refused` names it in `code`.
pub mod code {
    pub const PROTOCOL: &str = "protocol";
    pub const MALFORMED: &str = "malformed";
    pub const CATALOG: &str = "catalog";
    pub const BUSY: &str = "busy";
    pub const SOCKET: &str = "socket";
    pub const SESSION: &str = "session";
}

/// A refusal: its `code` and the `message` shown for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: shorten(&message.into(), MAX_MESSAGE_CHARS),
        }
    }
}

// --- Extension ↔ native host ---------------------------------------------------------------------

/// A message from the extension, as the native host reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum FromExtension {
    Hello(Hello),
    /// An `update` for `session`, as the session's Tick gets it: without `session`.
    Update {
        session: String,
        message: Vec<u8>,
    },
    /// A `result` for `session`, as the session's Tick gets it: without `session`.
    Result {
        session: String,
        message: Vec<u8>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Hello {
    pub protocol: u32,
    pub extension_version: String,
    pub catalog: Vec<TickTool>,
}

/// A message from the native host to the extension.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToExtension<'a> {
    Welcome {
        protocol: u32,
    },
    Refused {
        protocol: u32,
        code: &'a str,
        message: &'a str,
    },
    SessionOpened {
        session: &'a str,
        title: &'a str,
        /// The title of the Chat that opened the session's Tick, once Tickloop told it one.
        #[serde(skip_serializing_if = "Option::is_none")]
        chat_title: Option<&'a str>,
    },
    /// The session's Chat has a title now, or a new one.
    ChatTitle {
        session: &'a str,
        chat_title: &'a str,
    },
    SessionClosed {
        session: &'a str,
    },
    Call {
        session: &'a str,
        id: u64,
        tool: &'a str,
        args: &'a Value,
        /// The Tick's `timeout_ms`, as the Tick wrote it.
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<&'a Value>,
        /// The Tick's `deadline_ms`, as the Tick wrote it.
        #[serde(skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<&'a Value>,
    },
    Stop {
        session: &'a str,
    },
}

impl ToExtension<'_> {
    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("relay messages serialize")
    }
}

/// Reads one message from the extension. `Ok(None)` is a message to ignore: one that is not a JSON
/// object with a string `type`, or one of a `type` this version does not know. A refusal says why a
/// `hello` speaks another version or a message of a known `type` is malformed.
pub fn parse_from_extension(message: &[u8]) -> Result<Option<FromExtension>, Refusal> {
    let Some((kind, mut object)) = typed(message) else {
        return Ok(None);
    };
    let malformed = |error: String| {
        Refusal::new(
            code::MALFORMED,
            format!("a malformed {kind:?} message: {error}"),
        )
    };
    Ok(Some(match kind.as_str() {
        "hello" => {
            check_version("the extension", "this host", &object)?;
            FromExtension::Hello(
                serde_json::from_value(Value::Object(object))
                    .map_err(|error| malformed(error.to_string()))?,
            )
        }
        "update" | "result" => {
            let session = match object.remove("session") {
                Some(Value::String(session)) => session,
                _ => return Err(malformed("it names no session".into())),
            };
            if kind == "update" && !object.get("state").is_some_and(Value::is_object) {
                return Err(malformed("its state is not an object".into()));
            }
            if kind == "result" && !object.get("id").is_some_and(Value::is_u64) {
                return Err(malformed("its id is not a call's".into()));
            }
            let message = serde_json::to_vec(&object).expect("JSON serializes");
            if kind == "update" {
                FromExtension::Update { session, message }
            } else {
                FromExtension::Result { session, message }
            }
        }
        _ => return Ok(None),
    }))
}

// --- Tick ↔ native host, as the host reads and writes it ------------------------------------------

/// A message from a Tick, as the native host reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum FromTick {
    Open {
        session: String,
        title: String,
        chat_title: Option<String>,
    },
    /// The title of the Chat that opened the Tick, now or anew.
    ChatTitle {
        chat_title: String,
    },
    Call {
        id: u64,
        tool: String,
        args: Value,
        /// What is left of the Tick's time for the call, passed on unread.
        timeout_ms: Option<Value>,
        /// When the Tick gives up on the call, passed on unread.
        deadline_ms: Option<Value>,
    },
    Stop,
}

/// A message from the native host to a Tick, besides the extension's `update` and `result` it
/// passes on.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToTick<'a> {
    Hello {
        extension_version: &'a str,
        catalog: &'a [TickTool],
    },
    Refused {
        protocol: u32,
        code: &'a str,
        message: &'a str,
    },
}

impl ToTick<'_> {
    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("relay messages serialize")
    }
}

/// Reads one message from a Tick, as [`parse_from_extension`] does from the extension.
pub fn parse_from_tick(message: &[u8]) -> Result<Option<FromTick>, Refusal> {
    let Some((kind, object)) = typed(message) else {
        return Ok(None);
    };
    let malformed = |error: &str| {
        Refusal::new(
            code::MALFORMED,
            format!("a malformed {kind:?} message: {error}"),
        )
    };
    Ok(Some(match kind.as_str() {
        "open" => {
            check_version("the Tick", "this host", &object)?;
            let session = match object.get("session") {
                Some(Value::String(session))
                    if !session.trim().is_empty()
                        && session.chars().count() <= MAX_SESSION_CHARS =>
                {
                    session.clone()
                }
                _ => {
                    return Err(malformed(&format!(
                        "its session is not a name of 1 to {MAX_SESSION_CHARS} characters"
                    )));
                }
            };
            let title = match object.get("title") {
                Some(Value::String(title)) if !title.trim().is_empty() => {
                    shorten(title, MAX_SESSION_TITLE_CHARS)
                }
                _ => return Err(malformed("its title is not a name")),
            };
            // Optional: a Tick that has none, or a host that has no use for it, does without.
            let chat_title = object
                .get("chat_title")
                .and_then(Value::as_str)
                .and_then(chat_title);
            FromTick::Open {
                session,
                title,
                chat_title,
            }
        }
        "chat_title" => match object
            .get("chat_title")
            .and_then(Value::as_str)
            .and_then(chat_title)
        {
            Some(chat_title) => FromTick::ChatTitle { chat_title },
            None => return Err(malformed("its chat_title is not a title")),
        },
        "call" => {
            let Some(id) = object.get("id").and_then(Value::as_u64) else {
                return Err(malformed("its id is not a call's"));
            };
            let Some(tool) = object.get("tool").and_then(Value::as_str) else {
                return Err(malformed("it names no tool"));
            };
            FromTick::Call {
                id,
                tool: tool.to_owned(),
                args: object.get("args").cloned().unwrap_or(Value::Null),
                timeout_ms: object.get("timeout_ms").cloned(),
                deadline_ms: object.get("deadline_ms").cloned(),
            }
        }
        "stop" => FromTick::Stop,
        _ => return Ok(None),
    }))
}

// --- Tick ↔ native host, as the Tick reads and writes it ------------------------------------------

/// A message from the native host, as a Tick reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum FromHost {
    Hello(HostHello),
    Update(BrowserState),
    Result(CallResult),
    Refused {
        code: Option<String>,
        message: String,
    },
}

/// The extension's catalog and version, as the host hands them to a Tick.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct HostHello {
    pub extension_version: String,
    pub catalog: Vec<TickTool>,
}

/// The extension's answer to one `call`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CallResult {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub error: Option<CallError>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CallError {
    pub message: String,
    /// A short machine-readable reason, such as `denied` or `hand_back`.
    #[serde(default)]
    pub code: Option<String>,
}

/// A message from a Tick to the native host.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToHost<'a> {
    Open {
        protocol: u32,
        session: &'a str,
        title: &'a str,
        /// The title of the Chat that opened the Tick, when Tickloop told it one.
        #[serde(skip_serializing_if = "Option::is_none")]
        chat_title: Option<&'a str>,
    },
    /// Tickloop told the Tick its Chat's title, or a new one, after `open`.
    ChatTitle {
        chat_title: &'a str,
    },
    Call {
        id: u64,
        tool: &'a str,
        args: &'a Value,
        /// How many milliseconds are left of the Tick's time for the call as it writes it.
        timeout_ms: u64,
        /// When the Tick gives up on the call, in milliseconds since the Unix epoch.
        deadline_ms: u64,
    },
    Stop,
}

impl ToHost<'_> {
    pub fn to_vec(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("relay messages serialize")
    }
}

/// Reads one message from the native host, as [`parse_from_extension`] does from the extension;
/// the error says why a message of a known `type` is malformed.
pub fn parse_from_host(message: &[u8]) -> Result<Option<FromHost>, String> {
    let Some((kind, mut object)) = typed(message) else {
        return Ok(None);
    };
    let malformed = |error: serde_json::Error| format!("a malformed {kind:?} message: {error}");
    Ok(Some(match kind.as_str() {
        "hello" => {
            FromHost::Hello(serde_json::from_value(Value::Object(object)).map_err(malformed)?)
        }
        "update" => {
            let state = object
                .remove("state")
                .ok_or_else(|| "an \"update\" message without its state".to_owned())?;
            FromHost::Update(serde_json::from_value(state).map_err(malformed)?)
        }
        "result" => {
            FromHost::Result(serde_json::from_value(Value::Object(object)).map_err(malformed)?)
        }
        "refused" => FromHost::Refused {
            code: object
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_owned),
            message: object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the native host refused this Tick without saying why")
                .to_owned(),
        },
        _ => return Ok(None),
    }))
}

/// What the extension reports of the browser: the Agent's tabs and what waits for the user.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct BrowserState {
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub pending: Vec<Pending>,
    /// The latest change the Agent did not make itself, such as the user navigating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
    /// The session's name as the browser shows it, on its tab group and in the side panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The color of the session's tab group, one of [`GROUP_COLORS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_color: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Tab {
    pub id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub active: bool,
}

/// Something that waits for the user: an `approval` or a `hand_back` step.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Pending {
    pub id: String,
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// The tool whose call waits on this approval, for one that lets the Agent on a site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// For an approval: the link that opens it in the browser with the extension, for the card and
    /// for the Agent to give the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_url: Option<String>,
}

// --- Shared --------------------------------------------------------------------------------------

/// The `type` and the fields of a JSON object with a string `type`.
fn typed(message: &[u8]) -> Option<(String, Map<String, Value>)> {
    let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(message) else {
        return None;
    };
    let kind = object.get("type")?.as_str()?.to_owned();
    Some((kind, object))
}

/// Refuses a first message that names another protocol version than this program's, naming both.
fn check_version(sender: &str, receiver: &str, object: &Map<String, Value>) -> Result<(), Refusal> {
    match object.get("protocol").and_then(Value::as_u64) {
        Some(version) if version == u64::from(PROTOCOL) => Ok(()),
        Some(version) => Err(Refusal::new(
            code::PROTOCOL,
            format!(
                "{sender} speaks relay protocol {version} and {receiver} speaks {PROTOCOL}; update the one that is older"
            ),
        )),
        None => Err(Refusal::new(
            code::PROTOCOL,
            format!("{sender} names no relay protocol version, and {receiver} speaks {PROTOCOL}"),
        )),
    }
}

impl HostHello {
    /// The extension's version as the Tick shows it, shortened.
    pub fn version(&self) -> String {
        shorten(&self.extension_version, MAX_VERSION_CHARS)
    }
}

impl BrowserState {
    /// The state within the Tick's limits, so it stays small enough to read and to update.
    /// The active tab is always among the tabs kept.
    pub fn bounded(mut self) -> Self {
        if self.tabs.len() > MAX_TABS {
            let active = self
                .tabs
                .iter()
                .position(|tab| tab.active)
                .filter(|&index| index >= MAX_TABS)
                .map(|index| self.tabs.remove(index));
            self.tabs.truncate(MAX_TABS - usize::from(active.is_some()));
            self.tabs.extend(active);
        }
        for tab in &mut self.tabs {
            tab.title = shorten(&tab.title, MAX_TITLE_CHARS);
            tab.url = shorten(&tab.url, MAX_URL_CHARS);
        }
        self.pending.truncate(MAX_PENDING);
        for pending in &mut self.pending {
            pending.id = shorten(&pending.id, MAX_VERSION_CHARS);
            pending.kind = shorten(&pending.kind, MAX_VERSION_CHARS);
            pending.summary = shorten(&pending.summary, MAX_SUMMARY_CHARS);
            pending.domain = pending
                .domain
                .as_deref()
                .map(|domain| shorten(domain, MAX_TITLE_CHARS));
            pending.tool = pending
                .tool
                .as_deref()
                .map(|tool| shorten(tool, MAX_TOOL_CHARS));
            // A link cut short opens nothing: one too long goes.
            pending.open_url = pending
                .open_url
                .take()
                .filter(|url| url.chars().count() <= MAX_URL_CHARS);
        }
        self.last_event = self
            .last_event
            .as_deref()
            .map(|event| shorten(event, MAX_SUMMARY_CHARS));
        self.label = self
            .label
            .as_deref()
            .map(|label| one_line(label, MAX_LABEL_CHARS))
            .filter(|label| !label.is_empty());
        // A color the browser has no tab group of is no color.
        self.group_color = self
            .group_color
            .take()
            .filter(|color| GROUP_COLORS.contains(&color.as_str()));
        self
    }

    /// The tab the Agent works in.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.active)
    }
}

/// The Chat title `title` as the relay carries it: on one line, at most [`MAX_CHAT_TITLE_CHARS`],
/// and none when it is blank.
pub fn chat_title(title: &str) -> Option<String> {
    Some(one_line(title, MAX_CHAT_TITLE_CHARS)).filter(|title| !title.is_empty())
}

/// `text` on one line, its control characters as spaces and without spaces around it, cut to
/// `limit` characters.
fn one_line(text: &str, limit: usize) -> String {
    let line: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    shorten(line.trim(), limit)
}

/// `text` cut to `limit` characters, with an ellipsis when it was longer.
pub fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(limit.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn json(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn the_extension_messages_parse_and_the_host_routes_them_without_their_session() {
        let hello = parse_from_extension(
            br#"{"type":"hello","protocol":2,"extension_version":"0.2.0","catalog":[{"name":"tabs","description":"List the tabs.","input_schema":{"type":"object"},"mutates":false}],"later":true}"#,
        )
        .unwrap();
        let Some(FromExtension::Hello(hello)) = hello else {
            panic!("{hello:?}");
        };
        assert_eq!(hello.catalog[0].name, "tabs");

        let Some(FromExtension::Update { session, message }) = parse_from_extension(
            br#"{"type":"update","session":"hosted:a","state":{"tabs":[{"id":4,"title":"Home","url":"https://example.com/","active":true}]}}"#,
        )
        .unwrap() else {
            panic!("not an update");
        };
        assert_eq!(session, "hosted:a");
        let Some(FromHost::Update(state)) = parse_from_host(&message).unwrap() else {
            panic!("{:?}", json(&message));
        };
        assert_eq!(state.active_tab().unwrap().id, 4);

        let Some(FromExtension::Result { session, message }) = parse_from_extension(
            br#"{"type":"result","session":"hosted:b","id":3,"ok":false,"error":{"message":"The user denied this.","code":"denied"}}"#,
        )
        .unwrap() else {
            panic!("not a result");
        };
        assert_eq!(session, "hosted:b");
        assert_eq!(
            parse_from_host(&message).unwrap(),
            Some(FromHost::Result(CallResult {
                id: 3,
                ok: false,
                result: Value::Null,
                error: Some(CallError {
                    message: "The user denied this.".into(),
                    code: Some("denied".into()),
                }),
            }))
        );

        // What is not a JSON object with a string type, or of an unknown type, is ignored.
        for ignored in [
            &b"not json"[..],
            b"[1,2]",
            br#""x""#,
            b"7",
            br#"{"type":3}"#,
            br#"{"type":"later"}"#,
            br#"{"no":"type"}"#,
        ] {
            assert_eq!(parse_from_extension(ignored).unwrap(), None, "{ignored:?}");
            assert_eq!(parse_from_tick(ignored).unwrap(), None, "{ignored:?}");
            assert_eq!(parse_from_host(ignored).unwrap(), None, "{ignored:?}");
        }
    }

    #[test]
    fn another_version_and_a_malformed_message_are_refused_with_their_code() {
        // Version 1, as an extension not yet updated says it.
        let refusal = parse_from_extension(
            br#"{"type":"hello","protocol":1,"extension_version":"0.1.0","catalog":[]}"#,
        )
        .unwrap_err();
        assert_eq!(refusal.code, code::PROTOCOL);
        assert_eq!(
            refusal.message,
            "the extension speaks relay protocol 1 and this host speaks 2; update the one that is older"
        );
        let refusal =
            parse_from_tick(br#"{"type":"open","protocol":3,"session":"hosted:a","title":"t"}"#)
                .unwrap_err();
        assert_eq!(
            (refusal.code, refusal.message.as_str()),
            (
                code::PROTOCOL,
                "the Tick speaks relay protocol 3 and this host speaks 2; update the one that is older"
            )
        );
        assert_eq!(
            parse_from_extension(br#"{"type":"hello","extension_version":"0.2.0","catalog":[]}"#)
                .unwrap_err()
                .code,
            code::PROTOCOL
        );

        for malformed in [
            &br#"{"type":"hello","protocol":2,"extension_version":"0.2.0","catalog":{}}"#[..],
            br#"{"type":"hello","protocol":2}"#,
            br#"{"type":"update","state":{}}"#,
            br#"{"type":"update","session":"hosted:a"}"#,
            br#"{"type":"result","session":7,"id":1,"ok":true}"#,
            br#"{"type":"result","session":"hosted:a","ok":true}"#,
        ] {
            assert_eq!(
                parse_from_extension(malformed).unwrap_err().code,
                code::MALFORMED,
                "{malformed:?}"
            );
        }
        let long = "s".repeat(MAX_SESSION_CHARS + 1);
        for malformed in [
            br#"{"type":"open","protocol":2,"title":"t"}"#.to_vec(),
            br#"{"type":"open","protocol":2,"session":" ","title":"t"}"#.to_vec(),
            format!(r#"{{"type":"open","protocol":2,"session":"{long}","title":"t"}}"#)
                .into_bytes(),
            br#"{"type":"open","protocol":2,"session":"hosted:a"}"#.to_vec(),
            br#"{"type":"call","tool":"read_page"}"#.to_vec(),
            br#"{"type":"call","id":1}"#.to_vec(),
        ] {
            assert_eq!(
                parse_from_tick(&malformed).unwrap_err().code,
                code::MALFORMED,
                "{}",
                String::from_utf8_lossy(&malformed)
            );
        }
        assert!(parse_from_host(br#"{"type":"hello","catalog":[]}"#).is_err());
        assert!(parse_from_host(br#"{"type":"update"}"#).is_err());
    }

    #[test]
    fn a_chat_title_is_one_line_within_its_limit_and_a_blank_one_is_none() {
        let open = |chat_title: Value| {
            let message = json!({"type": "open", "protocol": 2, "session": "hosted:a", "title": "ticks", "chat_title": chat_title});
            match parse_from_tick(&serde_json::to_vec(&message).unwrap()).unwrap() {
                Some(FromTick::Open { chat_title, .. }) => chat_title,
                other => panic!("not an open: {other:?}"),
            }
        };
        assert_eq!(
            open(json!("  Fix the\nlogin page ")).as_deref(),
            Some("Fix the login page")
        );
        let long = open(json!("t".repeat(500))).unwrap();
        assert_eq!(long.chars().count(), MAX_CHAT_TITLE_CHARS);
        assert!(long.ends_with('…'));
        // In `open` it is optional: one that is no title is as none.
        assert_eq!(open(json!(" \t")), None);
        assert_eq!(open(json!(7)), None);

        assert_eq!(
            parse_from_tick(br#"{"type":"chat_title","chat_title":"Plan the trip"}"#).unwrap(),
            Some(FromTick::ChatTitle {
                chat_title: "Plan the trip".into()
            })
        );
        // On its own it is the message's point: one that is no title is malformed.
        for message in [
            br#"{"type":"chat_title"}"#.as_slice(),
            br#"{"type":"chat_title","chat_title":" "}"#,
            br#"{"type":"chat_title","chat_title":null}"#,
        ] {
            let refusal = parse_from_tick(message).unwrap_err();
            assert_eq!(refusal.code, code::MALFORMED);
        }
    }

    #[test]
    fn a_label_and_a_group_color_are_kept_when_they_are_ones_the_card_can_show() {
        let state: BrowserState = serde_json::from_value(json!({
            "tabs": [],
            "label": format!("ticks\u{1b}[31m · 9897{}", "x".repeat(200)),
            "group_color": "purple",
        }))
        .unwrap();
        let state = state.bounded();
        let label = state.label.unwrap();
        assert!(label.starts_with("ticks [31m · 9897x"), "{label}");
        assert_eq!(label.chars().count(), MAX_LABEL_CHARS);
        assert_eq!(state.group_color.as_deref(), Some("purple"));

        let state: BrowserState =
            serde_json::from_value(json!({"label": "  ", "group_color": "magenta"})).unwrap();
        let state = state.bounded();
        assert_eq!((state.label, state.group_color), (None, None));
        // An extension that says neither, as an older one, leaves both out of the state.
        let state: BrowserState = serde_json::from_value(json!({"tabs": []})).unwrap();
        assert_eq!(
            serde_json::to_value(state.bounded()).unwrap(),
            json!({"tabs": [], "pending": []})
        );
    }

    #[test]
    fn a_ticks_open_keeps_its_session_and_shortens_its_title() {
        let title = "t".repeat(200);
        let open = format!(
            r#"{{"type":"open","protocol":2,"session":"hosted:4kqY8bXc2RnT0mWv7sLpQe","title":"{title}"}}"#
        );
        let Some(FromTick::Open {
            session,
            title,
            chat_title,
        }) = parse_from_tick(open.as_bytes()).unwrap()
        else {
            panic!("not an open");
        };
        assert_eq!(session, "hosted:4kqY8bXc2RnT0mWv7sLpQe");
        assert_eq!(title.chars().count(), MAX_SESSION_TITLE_CHARS);
        assert_eq!(chat_title, None);
        assert_eq!(
            parse_from_tick(br#"{"type":"call","id":7,"tool":"read_page","args":{"tab_id":412}}"#)
                .unwrap(),
            Some(FromTick::Call {
                id: 7,
                tool: "read_page".into(),
                args: json!({"tab_id": 412}),
                timeout_ms: None,
                deadline_ms: None,
            })
        );
        // The host keeps `timeout_ms` and `deadline_ms` as they came, whatever they hold, to pass
        // them on.
        for value in ["55000", "-1", "\"soon\""] {
            let call = format!(
                r#"{{"type":"call","id":8,"tool":"find","timeout_ms":{value},"deadline_ms":{value}}}"#
            );
            let Some(FromTick::Call {
                timeout_ms,
                deadline_ms,
                ..
            }) = parse_from_tick(call.as_bytes()).unwrap()
            else {
                panic!("not a call");
            };
            assert_eq!(timeout_ms, Some(serde_json::from_str(value).unwrap()));
            assert_eq!(deadline_ms, Some(serde_json::from_str(value).unwrap()));
        }
        assert_eq!(
            parse_from_tick(br#"{"type":"stop"}"#).unwrap(),
            Some(FromTick::Stop)
        );
        assert_eq!(
            parse_from_host(
                br#"{"type":"refused","protocol":2,"code":"session","message":"taken"}"#
            )
            .unwrap(),
            Some(FromHost::Refused {
                code: Some("session".into()),
                message: "taken".into()
            })
        );
    }

    #[test]
    fn the_messages_are_written_as_the_protocol_shows_them() {
        let args = json!({"tab_id": 412});
        let session = "hosted:4kqY8bXc2RnT0mWv7sLpQe";
        assert_eq!(
            json(
                &ToExtension::Call {
                    session,
                    id: 7,
                    tool: "read_page",
                    args: &args,
                    timeout_ms: Some(&json!(54980)),
                    deadline_ms: Some(&json!(1_762_700_055_000_u64)),
                }
                .to_vec()
            ),
            json!({"type": "call", "session": session, "id": 7, "tool": "read_page", "args": {"tab_id": 412}, "timeout_ms": 54980, "deadline_ms": 1_762_700_055_000_u64})
        );
        assert_eq!(
            json(
                &ToExtension::Call {
                    session,
                    id: 7,
                    tool: "read_page",
                    args: &args,
                    timeout_ms: None,
                    deadline_ms: None,
                }
                .to_vec()
            ),
            json!({"type": "call", "session": session, "id": 7, "tool": "read_page", "args": {"tab_id": 412}})
        );
        assert_eq!(
            json(
                &ToExtension::Refused {
                    protocol: PROTOCOL,
                    code: code::BUSY,
                    message: "busy"
                }
                .to_vec()
            ),
            json!({"type": "refused", "protocol": 2, "code": "busy", "message": "busy"})
        );
        assert_eq!(
            json(&ToExtension::Welcome { protocol: PROTOCOL }.to_vec()),
            json!({"type": "welcome", "protocol": 2})
        );
        assert_eq!(
            json(
                &ToExtension::SessionOpened {
                    session,
                    title: "ticks",
                    chat_title: None,
                }
                .to_vec()
            ),
            json!({"type": "session_opened", "session": session, "title": "ticks"})
        );
        assert_eq!(
            json(
                &ToExtension::SessionOpened {
                    session,
                    title: "ticks",
                    chat_title: Some("Fix the login page"),
                }
                .to_vec()
            ),
            json!({"type": "session_opened", "session": session, "title": "ticks", "chat_title": "Fix the login page"})
        );
        assert_eq!(
            json(
                &ToExtension::ChatTitle {
                    session,
                    chat_title: "Plan the trip"
                }
                .to_vec()
            ),
            json!({"type": "chat_title", "session": session, "chat_title": "Plan the trip"})
        );
        assert_eq!(
            json(&ToExtension::SessionClosed { session }.to_vec()),
            json!({"type": "session_closed", "session": session})
        );
        assert_eq!(
            json(&ToExtension::Stop { session }.to_vec()),
            json!({"type": "stop", "session": session})
        );
        assert_eq!(
            json(
                &ToHost::Open {
                    protocol: PROTOCOL,
                    session,
                    title: "ticks",
                    chat_title: None,
                }
                .to_vec()
            ),
            json!({"type": "open", "protocol": 2, "session": session, "title": "ticks"})
        );
        assert_eq!(
            json(
                &ToHost::Open {
                    protocol: PROTOCOL,
                    session,
                    title: "ticks",
                    chat_title: Some("Plan the trip"),
                }
                .to_vec()
            ),
            json!({"type": "open", "protocol": 2, "session": session, "title": "ticks", "chat_title": "Plan the trip"})
        );
        assert_eq!(
            json(
                &ToHost::ChatTitle {
                    chat_title: "Plan the trip"
                }
                .to_vec()
            ),
            json!({"type": "chat_title", "chat_title": "Plan the trip"})
        );
        assert_eq!(
            json(
                &ToHost::Call {
                    id: 7,
                    tool: "read_page",
                    args: &args,
                    timeout_ms: 55000,
                    deadline_ms: 1_762_700_055_000,
                }
                .to_vec()
            ),
            json!({"type": "call", "id": 7, "tool": "read_page", "args": {"tab_id": 412}, "timeout_ms": 55000, "deadline_ms": 1_762_700_055_000_u64})
        );
        assert_eq!(ToHost::Stop.to_vec(), br#"{"type":"stop"}"#);
        assert_eq!(
            json(
                &ToTick::Hello {
                    extension_version: "0.2.0",
                    catalog: &[]
                }
                .to_vec()
            ),
            json!({"type": "hello", "extension_version": "0.2.0", "catalog": []})
        );
        assert_eq!(
            json(
                &ToTick::Refused {
                    protocol: PROTOCOL,
                    code: code::SESSION,
                    message: "taken"
                }
                .to_vec()
            ),
            json!({"type": "refused", "protocol": 2, "code": "session", "message": "taken"})
        );
    }

    #[test]
    fn a_large_state_is_kept_within_bounds() {
        let state = BrowserState {
            tabs: (0..100)
                .map(|id| Tab {
                    id,
                    title: "t".repeat(500),
                    url: "u".repeat(5000),
                    active: id == 0,
                })
                .collect(),
            pending: (0..40)
                .map(|id| Pending {
                    id: id.to_string(),
                    kind: "approval".into(),
                    summary: "s".repeat(1000),
                    domain: None,
                    tool: Some("x".repeat(1000)),
                    open_url: Some(format!("https://tickloop.ai/#{}", "u".repeat(id * 100))),
                })
                .collect(),
            last_event: Some("e".repeat(1000)),
            ..BrowserState::default()
        }
        .bounded();
        assert_eq!(state.tabs.len(), MAX_TABS);
        assert_eq!(state.tabs[0].title.chars().count(), MAX_TITLE_CHARS);
        assert!(state.tabs[0].title.ends_with('…'));
        assert_eq!(state.tabs[0].url.chars().count(), MAX_URL_CHARS);
        assert_eq!(state.pending.len(), MAX_PENDING);
        assert_eq!(
            state.pending[0].tool.as_ref().unwrap().chars().count(),
            MAX_TOOL_CHARS
        );
        assert!(state.pending[10].open_url.is_some());
        assert_eq!(state.pending[11].open_url, None);
        assert_eq!(state.last_event.unwrap().chars().count(), MAX_SUMMARY_CHARS);
        assert_eq!(shorten("short", 10), "short");
    }

    #[test]
    fn the_active_tab_is_kept_when_the_tabs_are_cut() {
        // 40 tabs, the 35th active.
        let tabs = (0..40)
            .map(|id| Tab {
                id,
                title: format!("Tab {id}"),
                url: format!("https://example.com/{id}"),
                active: id == 34,
            })
            .collect();
        let state = BrowserState {
            tabs,
            ..BrowserState::default()
        }
        .bounded();
        assert_eq!(state.tabs.len(), MAX_TABS);
        assert_eq!(state.active_tab().unwrap().id, 34);
        let ids: Vec<_> = state.tabs.iter().map(|tab| tab.id).collect();
        assert_eq!(ids[..MAX_TABS - 1], (0..31).collect::<Vec<_>>()[..]);
        assert_eq!(ids[MAX_TABS - 1], 34);
    }
}
