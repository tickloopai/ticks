# Browser relay protocol

How the Browser Ticks, their native messaging host and the Tickloop browser extension talk. The extension implements the
extension side of this document; `tick-browser` implements the other two. This is version 2: several sessions use one
browser at once, each through its own Browser Tick.

```text
Agent A ←Tick protocol→ tick-browser browser ─┐
Agent B ←Tick protocol→ tick-browser browser ─┼─local socket→ tick-browser (native host) ←native messaging→ extension
          Tickhost starts each one            ┘                Chrome starts it, one per browser profile
```

- **Native host**: `tick-browser`, which Chrome starts when the extension calls `chrome.runtime.connectNative` with the
  host name `ai.tickloop.browser`. Chrome passes the extension's origin, `chrome-extension://<id>/`, as the first
  argument; the host refuses an origin that is not one of the configured extension ids. It serves the local socket once
  the extension's `hello` is accepted, tells the extension which sessions are open, and routes calls, results and state
  between the extension and each session's Tick. It exits when Chrome closes its input, that is when the extension
  disconnects the port or the browser closes.
- **Tick**: `tick-browser browser`, which Tickloop starts as a Tick Instance, one for each session that opens it. It
  connects to the socket as a client, opens its session, and serves the extension's tools through `tick/describe` and
  `tick/call`. While no host serves the socket it waits and tries again; it never exits for want of a browser.
- **Extension**: runs the tools in the user's browser, keeps one tab group for each session, and reports each session's
  state.

## Framing

Every message on both hops is a 4-byte length followed by that many bytes of UTF-8 JSON, one object per message. The
length is an unsigned 32-bit integer in native byte order, as Chrome's native messaging defines it; every supported
target is little-endian, so the socket hop uses little-endian.

| Direction | Largest message | Why |
| --- | --- | --- |
| Tick → host, host → extension | 1 MiB | Chrome's limit for messages from a native host |
| Extension → host, host → Tick | 64 MiB | Chrome's limit for messages to a native host |

A side that reads a length over its limit closes the connection. A message that is not a JSON object with a string
`type` is ignored, and so are unknown `type`s and unknown fields, so later versions can add them. A message of a known
`type` that is malformed, such as a `hello` whose `catalog` is not an array or an `update` without its `session`, is
answered with `refused` (code `malformed`), as below for each hop.

## Version

The relay protocol version is `2`, and every side must speak it: version 1 is not supported, and no side keeps a
compatibility path. The extension states its version in `hello` and the Tick in `open`; the host answers a different
version with `refused` (code `protocol`) whose `message` names both versions, such as `the extension speaks relay
protocol 1 and this host speaks 2; update the one that is older`. A change that keeps every existing field's meaning,
such as a new optional field or message, keeps the version.

## Local socket

The native host serves a Unix domain socket that only the user can reach:

- the directory `<base>/tick-browser-<uid>`, where `<uid>` is the user's id and `<base>` is
  - on macOS, the user's temporary directory as `confstr(_CS_DARWIN_USER_TEMP_DIR)` reports it;
  - on Linux, `/run/user/<uid>` when it exists, belongs to the user and others cannot enter it, and otherwise `/tmp`.

  The directory is created with mode 0700; one that is not owned by the user or that others can enter is refused.
- the socket `tick.sock` in it, with mode 0600, and the lock file `tick.lock` beside it.

The path depends on no environment variable: Chrome started from the Dock or the desktop and Tickloop started from a
shell see different environments, and both must arrive at the same path.

Only one host serves the socket: it holds an exclusive `flock` on `tick.lock` from before it binds the socket until it
stops serving, so of two browsers with the extension, such as Chrome and Edge, the host that takes the lock first serves
Tickloop and the other one answers its extension `refused` (code `busy`). The host that was refused tries the lock again
every second and serves once it gets it, that is once the first browser closes. A socket left by a host that died is
replaced by the next host that takes the lock; a host removes the socket when it stops serving only if it is still the
one it bound. The system cleans temporary files nobody used for days, so a serving host touches the directory, the lock
and the socket every hour, and binds the socket again if it is gone. `tick-browser status` reports `host_running` by the
lock, without connecting.

A Tick connects to the socket and tries again every second while that fails, as when no browser runs, the extension is
not installed, or the host was registered only now and the extension has not started it yet.

## Sessions

A session is one Tick Instance, named by the Tick when it opens:

- `session`: the instance identity Tickhost gives the Tick in `TICKLOOP_INSTANCE_ID`, such as `hosted:4kq…`; a Tick run
  directly in a terminal uses `terminal:<process id>`. At most 128 characters. It names the session for the host and the
  extension only and is not a credential.
- `title`: a short name for the user, at most 80 characters: the name of the Tick's working directory, or else the short
  form of `session` (its first 8 characters after the `hosted:` or `terminal:` prefix).
- `chat_title`, optional: the title of the Tickloop Chat that opened the Tick, which Tickloop tells the Tick with the
  Tick protocol's `tick/chat` request (a host that does not send it leaves it out): on one line, at most 200
  characters, never blank. The Tick sends it in `open` once it has one and in a `chat_title` message when it changes.

The extension names each session for the user, on its tab group and in the side panel: by `chat_title`, shortened when
long, or else by `title`, adding the end of `session` only when two open sessions would have the same name. It reports
that name and the group's color in the session's state (`label` and `group_color`), and the Tick's card shows them, so
the user can match the card in the terminal with the group in the browser.

One host serves any number of sessions at once. Each has its own tab group, state, pending approvals and calls in the
extension; the catalog is the extension's, the same for every session, with the same `revision`.

## Extension ↔ native host

### Extension → host

`hello` — the first message on the port; it may be sent again later to replace the catalog or the version:

```json
{
  "type": "hello",
  "protocol": 2,
  "extension_version": "0.2.0",
  "catalog": [
    {
      "name": "read_page",
      "description": "Read the tab as an accessibility snapshot…",
      "input_schema": {"type": "object", "properties": {"tab_id": {"type": "integer"}}, "required": ["tab_id"]},
      "mutates": false
    }
  ]
}
```

`catalog` is the tool list every session's `tick/describe` offers, unchanged: `name` unique and not blank,
`description`, `input_schema` a JSON Schema whose top level is an object, and `mutates`. A tool whose call can change
anything outside its own result (click, type, navigate, open or close a tab, run JavaScript, approve a plan) **must**
declare `mutates: true`: Tickloop asks the user before such a call and never repeats it on its own. Its encoded Tick
descriptor must stay within 512 KiB. The `revision` is a digest of `catalog`, so the same catalog keeps the same
revision across reconnections and a changed one makes Tickloop prepare its calls again.

The host answers `welcome` once it serves the socket for this catalog, or `refused`.

`update` — one session's complete current state, whenever it changes, and once after `session_opened`:

```json
{
  "type": "update",
  "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe",
  "state": {
    "tabs": [
      {"id": 412, "title": "Checkout", "url": "https://shop.example.com/checkout", "active": true}
    ],
    "pending": [
      {"id": "approval-3", "kind": "approval", "summary": "Allow shop.example.com", "domain": "shop.example.com", "tool": "navigate", "open_url": "https://tickloop.ai/#tickloop-approve=hosted%3A4kqY8bXc2RnT0mWv7sLpQe/approval-3"},
      {"id": "hand-back-1", "kind": "hand_back", "summary": "Pay ¥299.00 on shop.example.com", "domain": "shop.example.com"}
    ],
    "last_event": "The user navigated tab 412 to https://shop.example.com/checkout"
  }
}
```

- `tabs`: the tabs in this session's tab group; `active` marks the one the Agent works in.
- `pending`: what waits for the user in this session: `approval` (a site, a plan or a risky action to approve in the
  side panel) or `hand_back` (a step the user has to do themselves, such as paying or entering a password), with a
  one-line `summary` and the site's `domain` when there is one. An approval to let the Agent on a site also names the
  `tool` whose call waits on it; other items name none. An approval may carry `open_url`, a link that opens it in the
  browser with the extension, where the user can answer it without the side panel; the Tick's card shows it under the
  approval when it fits whole, and the Agent gives it to the user.
- `last_event`, optional: one line on the latest change in this session's group the Agent did not make itself, such as a
  navigation, a tab opened or closed, or the user acting in the page.
- `label`, optional: the session's name as the browser shows it, on its tab group and in the side panel, such as
  `Fix the login page` or `ticks · 9897`. The Tick's card shows it in its title and its state carries it, so the Agent
  calls the session by it.
- `group_color`, optional: the color of the session's tab group, one of Chrome's `grey`, `blue`, `red`, `yellow`,
  `green`, `pink`, `purple`, `cyan` and `orange`. The card's dot takes the nearest terminal color; the Tick drops any
  other value.

Each `update` replaces that session's previous state whole. The Tick keeps at most 32 tabs and 16 pending items and
shortens long titles and labels (120 characters), URLs (1,024) and summaries (300), so the state stays small; an `open_url` longer
than 1,024 characters is dropped, as a cut link opens nothing. An `update` for a
session that is not open is ignored.

`result` — the answer to one `call`, exactly once for each:

```json
{"type": "result", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe", "id": 7, "ok": true, "result": "<<<page-content 3f9a…>>> …"}
{"type": "result", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe", "id": 8, "ok": false, "error": {"message": "The user denied this.", "code": "denied"}}
```

- On success `result` is any JSON value; the Tick returns it as the `tick/call` result. One larger than 8 MiB once
  encoded fails the call instead.
- On failure `error.message` is shown to the model and `error.code`, optional, is a short machine-readable reason
  (`denied`, `hand_back`, `waiting_for_approval`, `stopped`, `stale_ref`, …) that the Tick passes on as the error's
  `data.code`. The Tick answers every failure with Tick error -32000 (the tool ran and failed), since it cannot tell
  whether the extension acted before failing.
- A `result` for a session that is not open, or whose `id` is not a call in flight in that session, is ignored.

### Host → extension

`welcome` — `hello` was accepted and the host serves the socket; sessions open from now on:

```json
{"type": "welcome", "protocol": 2}
```

`refused` — the host serves no session for this extension now:

```json
{"type": "refused", "protocol": 2, "code": "busy", "message": "another browser is serving Tickloop; close it there to use this one"}
```

| `code` | When | What follows |
| --- | --- | --- |
| `protocol` | `hello` names another version | nothing until a `hello` the host accepts |
| `malformed` | a message of a known type is malformed | nothing until a `hello` the host accepts |
| `catalog` | the catalog breaks a rule above | nothing until a `hello` the host accepts |
| `busy` | another browser's host serves the socket | `welcome` once this host gets the lock; it tries every second |
| `socket` | the socket cannot be served, such as a directory others can enter | `welcome` once it can; it tries every second |

The extension shows `message`. After `protocol`, `malformed` or `catalog` it does not resend the same `hello`. When the
host stops serving after `welcome` (a later `hello` or another message it refuses), it first sends `session_closed` for
every open session, then `refused`, and closes the socket, so every Tick goes back to waiting.

`session_opened` — a Tick opened a session; calls for it may follow at once:

```json
{"type": "session_opened", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe", "title": "ticks", "chat_title": "Fix the login page"}
```

`chat_title` is there when the Tick's `open` had one. The extension creates or reuses that session's tab group and sends
an `update` for it.

`chat_title` — the session's Chat has a title now, or a new one; the extension names the session by it from now on:

```json
{"type": "chat_title", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe", "chat_title": "Fix the login redirect"}
```

`session_closed` — the session's Tick went away (it exited, or its connection broke):

```json
{"type": "session_closed", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe"}
```

The extension ends that session's calls in flight and its pending approvals, and sends nothing more for it; no `result`
is expected for its calls. A session opened again later with the same `session` comes with a new `session_opened`.

`call` — run one tool in one session:

```json
{"type": "call", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe", "id": 7, "tool": "read_page", "args": {"tab_id": 412}, "timeout_ms": 55000, "deadline_ms": 1762700055000}
```

`id` is an integer unique within the session; the extension tells calls apart by `session` and `id` together. `tool` is
in the catalog of the last accepted `hello`, and Tickloop has checked `args` against its `input_schema`; the extension
still validates them, and acts only on the session's own tab group. Calls may overlap, within a session and across
sessions. `timeout_ms` and `deadline_ms`, both optional, are the Tick's, passed on unchanged: how many milliseconds
were left until the Tick gives up on the call when it wrote it, and when it gives up, in milliseconds since the Unix
epoch. The extension answers within `timeout_ms` and within 55 seconds of the call's arrival, whichever is shorter (55
seconds when `timeout_ms` is missing or not a non-negative number), and before `deadline_ms` when it is a non-negative
number: the Tick gives up then, inside Tickloop's 60-second limit for a call, and ignores a later `result`. The Tick
and the browser run on one machine and share its clock, so `deadline_ms` also counts the time the call spent on its
way, which can be seconds while the browser is short of CPU.

`stop` — the user pressed Stop in that session's Tick: end what runs for the session, answer its calls in flight with
`ok: false` and `code: "stopped"`, and detach the debugger from its tabs.

```json
{"type": "stop", "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe"}
```

### Order

1. The extension connects the port and sends `hello`.
2. The host checks it and answers `refused` (`protocol`, `malformed`, `catalog`), or takes the lock and serves the socket
   and answers `welcome`, or answers `refused` (`busy`, `socket`) and keeps trying, answering `welcome` when it serves.
3. From `welcome` on, the host sends `session_opened`, `chat_title`, `call`, `stop` and `session_closed` as Ticks come and go; the
   extension sends `update` and `result`, and may send `hello` again, which the host answers again.

## Tick ↔ native host

### Tick → host

`open` — the first message on every connection:

```json
{"type": "open", "protocol": 2, "session": "hosted:4kqY8bXc2RnT0mWv7sLpQe", "title": "ticks", "chat_title": "Fix the login page"}
```

`chat_title` is optional, as in Sessions above; a host that has none for the session passes none on. The host answers
`hello` when it opened the session, after it sent the extension `session_opened`, or `refused` and closes the
connection.

`chat_title` — Tickloop told the Tick a new title of its Chat after `open`; the host passes it to the extension with the
session added. One that is blank or not a string is malformed.

```json
{"type": "chat_title", "chat_title": "Fix the login redirect"}
```

`call` — one `tick/call` to the extension's tools, after `hello`:

```json
{"type": "call", "id": 7, "tool": "read_page", "args": {"tab_id": 412}, "timeout_ms": 55000, "deadline_ms": 1762700055000}
```

`id` is an integer unique on this connection. `timeout_ms`, a non-negative integer, is how many milliseconds are left
until the Tick gives up on the call as it writes the message; the Tick gives every call 55 seconds. `deadline_ms`, a
non-negative integer, is when the Tick gives up on it, in milliseconds since the Unix epoch. The host passes the call to
the extension with the session added and `timeout_ms` and `deadline_ms` as they came.

`stop` — the user pressed Stop in the Tick; the host passes it to the extension with the session added.

```json
{"type": "stop"}
```

### Host → Tick

`hello` — the session is open, and the extension's catalog and version; sent again whenever the extension's `hello`
replaces them:

```json
{"type": "hello", "extension_version": "0.2.0", "catalog": [ … ]}
```

`update` and `result` — the extension's `update` and `result` for this session, as the extension sent them, without
`session`.

`refused` — the session was not opened, or a message was malformed; the host closes the connection after it:

```json
{"type": "refused", "protocol": 2, "code": "session", "message": "a Tick of the session hosted:4kq… is already connected"}
```

| `code` | When |
| --- | --- |
| `protocol` | `open` names another version; `message` names both |
| `malformed` | the first message is not an `open`, or a message of a known type is malformed |
| `session` | a Tick of the same `session` is connected already |

### Order and going away

1. The Tick connects and sends `open`.
2. The host answers `hello`, or `refused` and closes the connection.
3. From `hello` on, the Tick sends `call`, `stop` and `chat_title`; the host sends `update`, `result` and `hello`.
4. When the Tick's connection ends, the host sends the extension `session_closed`. When the host goes away (the browser
   closed, or the host stopped serving), every Tick's connection ends: its calls in flight fail as below, it goes back
   to waiting, and it connects again every second.

A Tick that was refused shows the refusal and tries again every 10 seconds; a `hello` accepted later replaces it.

## What the Agent sees

While no host serves, `tick/describe` offers only `status`, a query tool that reports the connection, the local socket
and the native messaging registration, and the Tick's state is `waiting` with a `hint`, which also says how to install the extension, and
`install_url` once the extension has a store page. After a minute without an extension it adds `long_wait: true` and
names the likely reasons. Connected but not yet sent `hello`, it is
`connecting`. Once `hello` arrives, the catalog is the extension's and the state carries the session's `tabs`,
`pending` and `last_event`, the session's `label` and `group_color` when the extension names them, the extension's
version and the last call the Tick relayed. A refused Tick's state is
`refused` with the host's `error`.

A call in flight when the host goes away, or that gets no `result` within 55 seconds, fails with Tick error -32000. For
a tool declared `mutates: true` the error says its outcome is unknown and is not to be repeated before the page has been
read again (`data.code` is `outcome_unknown`); a query fails with `data.code` `disconnected` or `timed_out`. Nothing is
resent. A call that runs out of time while the session's latest state has an `approval` naming its `tool` has not run
yet: it fails with `data.code` `waiting_for_approval` instead, telling the Agent to tell the user what to approve in the
side panel and end the turn, then to call again once a state update shows it allowed, or stop when it shows it denied;
when that approval has an `open_url`, the message ends with it for the Agent to give the user.

## Native messaging registration

When a Tick starts it writes the native messaging host manifest `ai.tickloop.browser.json` for each Chromium browser
whose user data directory exists, and replaces one whose content differs, such as after the package moved or when
another Tickloop installation's Tick opened last: the host name is Tickloop's own, so the Tick that opens takes it. The
Tick's card and its state's `hint` say what it wrote or replaced, and `status` reports each browser as `current`,
`missing`, `outdated` (with the `previous_path` it names) or, after the Tick wrote it, `written`, `replaced` (with
`previous_path`) or `failed`. A host already running stays the program it started as; the browser starts the one the
manifest names the next time the extension connects the port.

```json
{
  "name": "ai.tickloop.browser",
  "description": "Tickloop Browser Tick relay",
  "path": "/absolute/path/to/tick-browser",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://phamnhcllaeigpghejaebpihbmfnbcln/"]
}
```

| Browser | macOS (under `~/Library/Application Support/`) | Linux (under `~/.config/`) |
| --- | --- | --- |
| Google Chrome | `Google/Chrome/NativeMessagingHosts/` | `google-chrome/NativeMessagingHosts/` |
| Chromium | `Chromium/NativeMessagingHosts/` | `chromium/NativeMessagingHosts/` |
| Microsoft Edge | `Microsoft Edge/NativeMessagingHosts/` | `microsoft-edge/NativeMessagingHosts/` |

`allowed_origins` lists only the configured extension ids (`EXTENSION_IDS` in `src/lib.rs`).

## Known limitations

- **Any process of the user can pose as a Tick.** The socket keeps out other users, not other programs the same user
  runs: while a browser serves, one of them can connect, open a session and send `call`s that the extension runs with
  the user's signed-in browser, without Tickloop or its approval before each `mutates: true` call taking part. What
  still guards the browser is the extension's own gating of risky actions (approval in the side panel and `hand_back`),
  which therefore must never count on Tickloop having asked. That is the trust model of every Tick, which runs with the
  user's permissions and trusts what the user runs; such a program could read the browser's profile as well. A
  low-cost mitigation for later: the host checks the peer of each connection (`LOCAL_PEERPID` on macOS, `SO_PEERCRED`
  on Linux) and serves only a process whose executable is the host's own canonical path, so only `tick-browser` can
  open a session.
- **Any process of the user can pose as the host.** While no browser serves, another program of the same user can
  listen on the socket path, which is fixed and in the user's own directory, and Ticks connect to it: it learns their
  calls and can answer them with made-up page content. It does not reach the browser, and what a page returns is
  already untrusted content to the Agent. The same peer check, made by the Tick, would refuse it.
- **A session is named, not proven.** `session` only tells sessions apart. A local process can open a session with the
  name of one that is not connected and take over its tab group; while the session's Tick is connected, the host
  refuses a second one.
- **On Linux without `/run/user/<uid>`, the socket directory is predictable under `/tmp`.** Another user can create
  `/tmp/tick-browser-<uid>` first; it is then refused as not the user's own, which keeps the data safe but keeps the
  host from serving until that directory is removed. Desktop sessions have `/run/user/<uid>`, so this needs a hostile
  local user on a machine without one.
- **Approval follows tools, not actions.** Tickloop asks before every call to a `mutates: true` tool, whatever its
  arguments, so a tool that both reads and acts asks for its reads too: `computer` is one tool declared
  `mutates: true`, so `screenshot` and `wait` need approval like a click. Splitting such a tool into a reading and an
  acting tool removes the extra approvals; the protocol has no per-action `mutates`.
