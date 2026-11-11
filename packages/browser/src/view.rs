//! The Browser Tick's card: the connection, the session's tabs, what waits for the user and the
//! latest relayed calls, inside the same frame as every Tick's, laid out for 30 columns and 6 rows
//! and using up to 12. The card tells the host how many rows its content needs, so it shrinks
//! when there is little to show and grows again with the tabs.

use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tick_market::{
    config::Settings,
    runtime::{self, EdgeLabels},
    viewport::CardRows,
};
use tick_provider::{
    error::{Error, Result},
    preferences::Language,
};
use tickloop_ui::format::{truncate, width, write_text};

use crate::{
    EXTENSION_STORE_URL, registration,
    relay::{Pending, Tab},
    tick::{Browser, Connection, Setup, Step, StepStatus},
};

/// What the card shows besides the browser.
pub struct View {
    /// Whether the help overlay is open.
    pub help: bool,
    /// Whether the user is in the card, where its keys are worth showing.
    pub focused: bool,
    /// Whether the user asked the extension to stop while it stays connected.
    pub stopped: bool,
    /// The rows the host was last told the content needs.
    pub told_rows: Option<u16>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            help: false,
            focused: !runtime::in_instance() || tick_market::terminal::has_focus(),
            stopped: false,
            told_rows: None,
        }
    }
}

/// The frame's top edge and the bottom edge with its keys.
const FRAME_ROWS: u16 = 2;

/// Draws the card; the terminal writes only what changed since the last frame. First, when the
/// rows the content needs changed, tells the host, which then resizes the card within the rows
/// the Tick declared; the frame after that is drawn for the new size.
pub fn render<B: Backend + CardRows>(
    terminal: &mut Terminal<B>,
    browser: &Browser,
    view: &mut View,
    settings: &Settings,
) -> Result<()> {
    // A focus report a wait dropped still reached the terminal's record of the focus.
    if runtime::in_instance() {
        view.focused = tick_market::terminal::has_focus();
    }
    let settings = &Settings {
        key_hints: settings.key_hints && view.focused,
        ..settings.clone()
    };
    let columns = terminal
        .size()
        .map_err(|error| Error::usage(error.to_string()))?
        .width;
    let rows = wanted_rows(browser, view, settings, columns);
    if view.told_rows != Some(rows) {
        terminal
            .backend_mut()
            .want_rows(rows)
            .map_err(|error| Error::usage(error.to_string()))?;
        view.told_rows = Some(rows);
    }
    terminal
        .draw(|frame| draw(frame, browser, view, settings))
        .map_err(|error| Error::usage(error.to_string()))?;
    Ok(())
}

/// The rows the card's content needs on a frame `columns` wide, the frame included: what it
/// shows with every tab, waiting item and step in view, whatever rows its terminal has now, so
/// the size the host gives never feeds back into what the card asks for.
pub fn wanted_rows(browser: &Browser, view: &View, settings: &Settings, columns: u16) -> u16 {
    let zh = settings.language == Language::ZhCn;
    let width = inner_area(Rect::new(0, 0, columns, u16::MAX)).width;
    let content = if view.help {
        wrapped_rows(&help_keys(zh).join("\n"), false, width)
    } else {
        match browser.connection() {
            Connection::Waiting => wrapped_rows(&waiting_text(browser.setup(), zh), true, width),
            Connection::Connecting => 1,
            Connection::Refused { message } => wrapped_rows(message, true, width),
            Connection::Connected { .. } => connected_rows(browser, settings, width),
        }
    };
    u16::try_from(content)
        .unwrap_or(u16::MAX)
        .saturating_add(FRAME_ROWS)
}

/// The rows `text` takes wrapped at `width`.
fn wrapped_rows(text: &str, trim: bool, width: u16) -> usize {
    Paragraph::new(text.to_owned())
        .wrap(Wrap { trim })
        .line_count(width)
}

/// Where the content goes inside a frame drawn over `area`.
fn inner_area(area: Rect) -> Rect {
    runtime::table_content_area(Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    ))
}

fn help_keys(zh: bool) -> Vec<&'static str> {
    let mut keys = vec![
        if zh {
            "s  停止浏览器里的操作"
        } else {
            "s  Stop the browser's actions"
        },
        if zh {
            "Backspace / ?  关闭帮助"
        } else {
            "Backspace / ?  Close help"
        },
    ];
    if runtime::quits_on_keys() {
        keys.push(if zh {
            "q / Ctrl+C  退出"
        } else {
            "q / Ctrl+C  Quit"
        });
    }
    keys
}

fn draw(frame: &mut Frame<'_>, browser: &Browser, view: &View, settings: &Settings) {
    let area = frame.area();
    let styles = settings.styles();
    let edge = styles.panel_base().patch(styles.border);
    frame.buffer_mut().set_style(area, styles.panel_base());
    frame.render_widget(
        Block::default().borders(Borders::ALL).border_style(edge),
        area,
    );
    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner = inner_area(area);
    let zh = settings.language == Language::ZhCn;
    let title_columns = runtime::frame_title_columns(&runtime::ViewState {
        frame_columns: area.width,
        ..Default::default()
    });
    let title = if view.help {
        String::from(if zh { "按键" } else { "Keys" })
    } else {
        title(browser, zh, title_columns, settings)
    };
    let title = truncate(&title, title_columns, settings.ambiguous_width);
    let connected = matches!(browser.connection(), Connection::Connected { .. });
    if !title.is_empty() {
        write_text(
            frame.buffer_mut(),
            area.x + 1,
            area.y,
            &format!(" {title} "),
            title_columns + 2,
            settings.ambiguous_width,
            edge,
        );
        // The dot takes the color of the session's tab group, so the card and the group match.
        if connected
            && !view.help
            && title.starts_with('●')
            && let Some(color) = browser.state().group_color.as_deref().and_then(group_color)
            && let Some(dot) = frame.buffer_mut().cell_mut((area.x + 2, area.y))
        {
            dot.set_fg(color);
        }
    }
    let keys = if !settings.key_hints {
        String::new()
    } else {
        let mut keys = Vec::new();
        if connected && !view.help {
            keys.push("s Stop");
        }
        keys.push("? Help");
        if runtime::quits_on_keys() {
            keys.push("q Quit");
        }
        keys.join("  ")
    };
    let lead = match (view.stopped, zh) {
        (false, _) => "",
        (true, true) => "已请求停止",
        (true, false) => "Stop sent",
    };
    runtime::draw_bottom_edge(
        frame,
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
        lead,
        &EdgeLabels {
            keys,
            problem: None,
        },
        settings,
    );
    if view.help {
        frame.render_widget(
            Paragraph::new(help_keys(zh).join("\n"))
                .wrap(Wrap { trim: false })
                .style(styles.panel_base().patch(styles.text)),
            inner,
        );
        return;
    }
    let mut lines = Lines {
        frame,
        area: inner,
        row: 0,
        settings,
    };
    match browser.connection() {
        Connection::Waiting => {
            lines.paragraph(&waiting_text(browser.setup(), zh), styles.muted_text);
        }
        Connection::Connecting => lines.line(
            if zh {
                "正在浏览器里打开本会话…"
            } else {
                "Opening this session in the browser…"
            },
            styles.muted_text,
        ),
        Connection::Refused { message } => lines.paragraph(message, styles.error),
        Connection::Connected { .. } => connected_lines(&mut lines, browser, zh),
    }
}

/// What a connected card lists, by what there is: the session's tabs, the current one first and
/// marked, each with its site; what waits for the user, an approval with the link that opens it on
/// the row under it when the link fits whole; the latest steps. Every waiting item gets a row before
/// the rest, then one tab, then the links, then the steps; the rows left go to the other tabs, the
/// last of them saying how many more there are when they do not all fit.
fn connected_lines(lines: &mut Lines<'_, '_>, browser: &Browser, zh: bool) {
    let styles = lines.settings.styles();
    let state = browser.state();
    let tabs: Vec<&Tab> = state
        .tabs
        .iter()
        .filter(|tab| tab.active)
        .chain(state.tabs.iter().filter(|tab| !tab.active))
        .collect();
    let steps: Vec<&Step> = browser.recent_steps().collect();
    let mut left = usize::from(lines.area.height);
    let mut take = |wanted: usize| {
        let taken = wanted.min(left);
        left -= taken;
        taken
    };
    let pending = take(state.pending.len());
    let tab_rows = tabs.len().max(1);
    let mut shown_tabs = take(1);
    let columns = usize::from(lines.area.width).saturating_sub(2);
    let links: Vec<Option<String>> = state
        .pending
        .iter()
        .take(pending)
        .map(|item| link(item, columns, lines.settings).map(str::to_owned))
        .collect();
    let mut shown_links = take(links.iter().flatten().count());
    let shown_steps = take(steps.len());
    shown_tabs += take(tab_rows - shown_tabs.min(tab_rows));
    // The last row of tabs that do not all fit counts the rest instead.
    let more = (shown_tabs >= 2 && shown_tabs < tabs.len()).then(|| tabs.len() - shown_tabs + 1);
    // An empty group says where pages will show, when a row is spare.
    let explain_empty = tabs.is_empty() && take(1) == 1;

    if tabs.is_empty() {
        if shown_tabs > 0 {
            lines.line(
                if zh {
                    "分组里还没有标签页"
                } else {
                    "No tab in the group yet"
                },
                styles.muted_text,
            );
        }
        if explain_empty {
            lines.line(
                if zh {
                    "Agent 打开的网页会列在这里"
                } else {
                    "Pages the Agent opens show here"
                },
                styles.muted_text,
            );
        }
    }
    for tab in tabs.iter().take(shown_tabs - usize::from(more.is_some())) {
        let title = if tab.title.is_empty() {
            &tab.url
        } else {
            &tab.title
        };
        // The frame's title names the current tab's site already, so its own title comes first.
        let text = with_site(title, host(&tab.url), columns, !tab.active, lines.settings);
        if tab.active {
            lines.line(&format!("▸ {text}"), styles.strong);
        } else {
            lines.line(&format!("  {text}"), styles.muted_text);
        }
    }
    if let Some(more) = more {
        lines.line(
            &if zh {
                format!("  另有 {more} 个标签页")
            } else {
                format!("  {more} more tabs")
            },
            styles.muted_text,
        );
    }
    for (pending, link) in state.pending.iter().take(pending).zip(links) {
        let label = match (pending.kind.as_str(), zh) {
            ("hand_back", true) => "要你亲手做",
            ("hand_back", false) => "Yours to do",
            (_, true) => "等你批准",
            (_, false) => "To approve",
        };
        let separator = if zh { "：" } else { ": " };
        lines.line(
            &format!("! {label}{separator}{}", pending.summary),
            styles.error,
        );
        if let Some(link) = link
            && shown_links > 0
        {
            shown_links -= 1;
            lines.line(&format!("  {link}"), styles.error);
        }
    }
    for step in steps.iter().take(shown_steps) {
        step_line(lines, step, zh);
    }
}

/// The rows a connected card fills with everything in view, as [`connected_lines`] lays them out
/// in a content area `width` columns wide: every waiting item and the link under it, every tab
/// (or the line saying there is none and the one saying where pages will show), every step.
fn connected_rows(browser: &Browser, settings: &Settings, width: u16) -> usize {
    let state = browser.state();
    let columns = usize::from(width).saturating_sub(2);
    let links = state
        .pending
        .iter()
        .filter(|item| link(item, columns, settings).is_some())
        .count();
    let tabs = if state.tabs.is_empty() {
        2
    } else {
        state.tabs.len()
    };
    state.pending.len() + links + tabs + browser.recent_steps().count()
}

/// The link that opens what `item` waits for, when it fits whole in `columns`: a link cut short
/// opens nothing.
fn link<'a>(item: &'a Pending, columns: usize, settings: &Settings) -> Option<&'a str> {
    item.open_url
        .as_deref()
        .filter(|url| width(url, settings.ambiguous_width) <= columns)
}

/// `title`, then its site when both fit in `columns`. When they do not, the site goes, unless
/// `cut_title`, when the title is cut first and the site goes only when not even a short title would
/// fit beside it.
fn with_site(
    title: &str,
    host: &str,
    columns: usize,
    cut_title: bool,
    settings: &Settings,
) -> String {
    let site = host.strip_prefix("www.").unwrap_or(host);
    let joined = format!("{title} · {site}");
    if site.is_empty() || width(&joined, settings.ambiguous_width) <= columns {
        return if site.is_empty() {
            title.to_owned()
        } else {
            joined
        };
    }
    let room = columns.saturating_sub(width(site, settings.ambiguous_width) + 3);
    if !cut_title || room < 8 {
        return truncate(title, columns, settings.ambiguous_width);
    }
    format!(
        "{} · {site}",
        truncate(title, room, settings.ambiguous_width)
    )
}

/// The frame's title: where the browser stands and, once it is connected, the site of the tab the
/// Agent works in. Each falls back to a shorter form rather than be cut in `columns`.
fn title(browser: &Browser, zh: bool, columns: usize, settings: &Settings) -> String {
    let forms: Vec<String> = match (browser.connection(), zh) {
        (Connection::Waiting, true) => vec!["○ 等待浏览器连接".into(), "○ 等待".into()],
        (Connection::Waiting, false) => {
            vec!["○ Waiting for the browser".into(), "○ Waiting".into()]
        }
        (Connection::Connecting, true) => vec!["◌ 扩展正在连接".into(), "◌ 连接中".into()],
        (Connection::Connecting, false) => {
            vec!["◌ Extension connecting".into(), "◌ Connecting".into()]
        }
        (Connection::Refused { .. }, true) => vec!["× 扩展未被接受".into(), "× 未接受".into()],
        (Connection::Refused { .. }, false) => {
            vec!["× Extension refused".into(), "× Refused".into()]
        }
        (Connection::Connected { .. }, _) => {
            let mut forms = Vec::new();
            let host = browser.state().active_tab().map(|tab| host(&tab.url));
            match browser.state().label.as_deref() {
                // The session's name as the browser shows it comes first, then the site.
                Some(label) => {
                    if let Some(host) = host {
                        forms.push(format!("● {label} · {host}"));
                        forms.push(format!("● {label} · {}", site(host)));
                    }
                    forms.push(format!("● {label}"));
                    let room = columns.saturating_sub(width("● ", settings.ambiguous_width));
                    if room > 1 {
                        forms.push(format!(
                            "● {}",
                            truncate(label, room, settings.ambiguous_width)
                        ));
                    }
                }
                None => {
                    if let Some(host) = host {
                        forms.push(format!("● {host}"));
                        forms.push(format!("● {}", site(host)));
                    }
                    forms.push(String::from(if zh {
                        "● 已连接"
                    } else {
                        "● Connected"
                    }));
                }
            }
            forms
        }
    };
    let mark = forms[0]
        .chars()
        .next()
        .map(String::from)
        .unwrap_or_default();
    forms
        .into_iter()
        .find(|form| width(form, settings.ambiguous_width) <= columns)
        .unwrap_or(mark)
}

/// The terminal color nearest to the color of Chrome's tab group `name`.
fn group_color(name: &str) -> Option<Color> {
    Some(match name {
        "grey" => Color::DarkGray,
        "blue" => Color::Blue,
        "red" => Color::Red,
        "yellow" => Color::Yellow,
        "green" => Color::Green,
        "pink" => Color::LightMagenta,
        "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        // Of the 16 colors, the nearest to Chrome's orange.
        "orange" => Color::LightRed,
        _ => return None,
    })
}

/// The last two labels of `host`, without its port: `shop.example.com` is `example.com`.
fn site(host: &str) -> &str {
    let name = host.split(':').next().unwrap_or(host);
    match name.rmatch_indices('.').nth(1) {
        Some((dot, _)) => &name[dot + 1..],
        None => name,
    }
}

fn step_line(lines: &mut Lines<'_, '_>, step: &Step, zh: bool) {
    let styles = lines.settings.styles();
    let (status, style) = match (step.status, zh) {
        (StepStatus::Running, true) => ("进行中", styles.muted_text),
        (StepStatus::Running, false) => ("running", styles.muted_text),
        (StepStatus::Done, true) => ("完成", styles.muted_text),
        (StepStatus::Done, false) => ("done", styles.muted_text),
        (StepStatus::Failed, true) => ("失败", styles.error),
        (StepStatus::Failed, false) => ("failed", styles.error),
        (StepStatus::Unknown, true) => ("结果未知", styles.error),
        (StepStatus::Unknown, false) => ("outcome unknown", styles.error),
    };
    lines.line(&format!("› {} · {status}", step.tool), style);
}

/// What the card says while no browser is connected: what the user does, by how the registration
/// went, and where the extension is installed; then what opening the Tick wrote in the browsers'
/// directories.
fn waiting_text(setup: &Setup, zh: bool) -> String {
    let browsers = |wanted: fn(&registration::Status) -> bool| {
        let names: Vec<_> = setup
            .registration
            .iter()
            .flatten()
            .filter(|entry| wanted(&entry.status))
            .map(|entry| entry.browser)
            .collect();
        names.join(if zh { "、" } else { ", " })
    };
    let written = browsers(|status| *status == registration::Status::Written);
    let replaced = browsers(|status| matches!(status, registration::Status::Replaced { .. }));
    let mut lines = vec![next_step(setup, zh), install(zh)];
    if !written.is_empty() {
        lines.push(if zh {
            format!("已为 {written} 登记原生消息主机。")
        } else {
            format!("Registered the host for {written}.")
        });
    }
    if !replaced.is_empty() {
        lines.push(if zh {
            format!("已把 {replaced} 的主机改指本程序。")
        } else {
            format!("Repointed {replaced}'s host here.")
        });
    }
    lines.join("\n")
}

/// Where the user installs the extension: the store page once it is published, or else how a
/// development build is loaded.
fn install(zh: bool) -> String {
    match (EXTENSION_STORE_URL, zh) {
        (Some(url), true) => format!("没装扩展？到这里安装：{url}"),
        (Some(url), false) => format!("No extension yet? Install it from {url}"),
        (None, true) => {
            "没装扩展？在 chrome://extensions 打开开发者模式，选「加载已解压的扩展程序」。".into()
        }
        (None, false) => {
            "No extension yet? Load it unpacked in chrome://extensions, in Developer mode.".into()
        }
    }
}

/// What the user does to connect the browser, by how the registration went.
fn next_step(setup: &Setup, zh: bool) -> String {
    let failed = match &setup.registration {
        Err(error) => Some(error.clone()),
        Ok(entries) => entries.iter().find_map(|entry| match &entry.status {
            registration::Status::Failed { error } => Some(format!("{}: {error}", entry.browser)),
            _ => None,
        }),
    };
    match (&setup.registration, failed, zh) {
        (_, Some(error), true) => format!("登记原生消息主机失败：{error}"),
        (_, Some(error), false) => format!("Registering the native messaging host failed: {error}"),
        (Ok(entries), None, true) if entries.is_empty() => {
            "没找到 Chrome、Chromium 或 Edge；装好一个并加上 Tickloop 扩展后再打开这个 Tick。".into()
        }
        (Ok(entries), None, false) if entries.is_empty() => {
            "No Chrome, Chromium or Edge found; install one with the Tickloop extension, then open this Tick again.".into()
        }
        (_, None, true) => "打开装了 Tickloop 扩展的浏览器，它会自己连上。".into(),
        (_, None, false) => "Open the browser with the Tickloop extension.".into(),
    }
}

/// The host of `url`, or `url` itself when it names none.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// Writes the card's lines from the top, each cut to the width, until the rows run out.
struct Lines<'a, 'f> {
    frame: &'a mut Frame<'f>,
    area: Rect,
    row: u16,
    settings: &'a Settings,
}

impl Lines<'_, '_> {
    fn line(&mut self, text: &str, style: Style) {
        if self.row >= self.area.height {
            return;
        }
        let columns = usize::from(self.area.width);
        let text = truncate(text, columns, self.settings.ambiguous_width);
        let base = self.settings.styles().panel_base();
        write_text(
            self.frame.buffer_mut(),
            self.area.x,
            self.area.y + self.row,
            &text,
            columns,
            self.settings.ambiguous_width,
            base.patch(style),
        );
        self.row += 1;
    }

    /// `text` wrapped over the rows left.
    fn paragraph(&mut self, text: &str, style: Style) {
        if self.row >= self.area.height {
            return;
        }
        let area = Rect {
            y: self.area.y + self.row,
            height: self.area.height - self.row,
            ..self.area
        };
        let base = self.settings.styles().panel_base();
        self.frame.render_widget(
            Paragraph::new(text.to_owned())
                .wrap(Wrap { trim: true })
                .style(base.patch(style)),
            area,
        );
        self.row = self.area.height;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};
    use tick_sdk::stream::ToolRequest;
    use tokio::{sync::mpsc, time::Instant};

    use super::*;
    use crate::{
        catalog::Catalog,
        tick::{Session, Setup},
    };

    /// The sizes every card is checked at: 30 by its minimum rows, 40 by its minimum and 40 by its
    /// maximum.
    const SIZES: [(u16, u16); 3] = [(30, 6), (40, 6), (40, crate::catalog::MAX_ROWS)];

    fn settings(focused: bool) -> Settings {
        Settings {
            language: Language::En,
            key_hints: focused,
            ..Settings::default()
        }
    }

    fn view(focused: bool) -> View {
        View {
            help: false,
            focused,
            stopped: false,
            told_rows: None,
        }
    }

    fn screen(browser: &Browser, columns: u16, rows: u16, focused: bool) -> String {
        let mut terminal = Terminal::new(TestBackend::new(columns, rows)).unwrap();
        terminal
            .draw(|frame| draw(frame, browser, &view(focused), &settings(focused)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|row| {
                (0..columns)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn browser() -> Browser {
        Browser::new(
            Catalog::default(),
            Setup {
                session: Session::new(Some("hosted:4kqY8bXc2RnT0mWv7sLpQe"), None, 1),
                socket: PathBuf::from("/tmp/tick.sock"),
                registration: Ok(vec![registration::Entry {
                    browser: "Google Chrome",
                    manifest: PathBuf::from("/m.json"),
                    status: registration::Status::Written,
                }]),
            },
        )
    }

    /// A browser connected on a checkout page with something to do and a finished step.
    fn connected(url: &str) -> Browser {
        let mut browser = browser();
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        let hello = json!({
            "type": "hello", "extension_version": "0.1.0",
            "catalog": [{"name": "read_page", "description": "Read.", "input_schema": {"type": "object"}, "mutates": false}],
        });
        let update = json!({
            "type": "update",
            "state": {
                "tabs": [
                    {"id": 4, "title": "Checkout — Example Shop", "url": url, "active": true},
                    {"id": 5, "title": "Docs", "url": "https://docs.example.com/", "active": false}
                ],
                "pending": [{"id": "h1", "kind": "hand_back", "summary": "Pay ¥299.00"}]
            }
        });
        let now = Instant::now();
        browser.receive(1, &serde_json::to_vec(&hello).unwrap(), now);
        browser.receive(1, &serde_json::to_vec(&update).unwrap(), now);
        let request = ToolRequest {
            id: json!(1),
            tool: "read_page".into(),
            args: json!({"tab_id": 4}),
        };
        assert!(browser.call(request, Instant::now()).is_none());
        browser.receive(
            1,
            br#"{"type":"result","id":1,"ok":true,"result":"- main"}"#,
            now,
        );
        browser
    }

    fn refused() -> Browser {
        let mut browser = browser();
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        browser.receive(
            1,
            br#"{"type":"refused","protocol":2,"code":"protocol","message":"the Tick speaks relay protocol 0 and this host speaks 2; update the one that is older"}"#,
            Instant::now(),
        );
        browser
    }

    fn connecting() -> Browser {
        let mut browser = browser();
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        browser
    }

    /// Checks the whole frame: every row as wide as the card, its corners, and `fields` shown.
    fn check(screen: &str, columns: u16, fields: &[&str]) {
        for line in screen.lines() {
            assert_eq!(
                tickloop_ui::format::width(line, Default::default()),
                usize::from(columns),
                "{screen}"
            );
        }
        assert!(screen.starts_with('┌') && screen.ends_with('┘'), "{screen}");
        for field in fields {
            assert!(screen.contains(field), "missing {field:?}:\n{screen}");
        }
    }

    #[test]
    fn every_state_is_laid_out_at_30_by_6_40_by_6_and_40_by_12() {
        let shop = "https://shop.example.com/checkout";
        for (columns, rows) in SIZES {
            let waiting = screen(&browser(), columns, rows, true);
            check(
                &waiting,
                columns,
                &[
                    "┌ ○ Waiting for the browser ",
                    "Open the browser with the",
                    "No extension yet? Load it",
                    "? Help",
                ],
            );
            assert!(!waiting.contains("s Stop"), "{waiting}");

            check(
                &screen(&connecting(), columns, rows, true),
                columns,
                &["┌ ◌ Extension connecting ", "Opening this session in"],
            );
            check(
                &screen(&refused(), columns, rows, true),
                columns,
                &["┌ × Extension refused ", "speaks 2;"],
            );

            // No row goes to a status: the tab, what waits and the last step all show at 30×6.
            let card = screen(&connected(shop), columns, rows, true);
            check(
                &card,
                columns,
                &[
                    "┌ ● shop.example.com ",
                    "▸ Checkout — Example Shop",
                    "! Yours to do: Pay ¥299.00",
                    "› read_page · done",
                    "s Stop",
                ],
            );
            if columns == 40 {
                assert!(card.contains("  Docs · docs.example.com"), "{card}");
            }
        }
    }

    #[test]
    fn the_title_falls_back_to_a_shorter_form_rather_than_be_cut() {
        let long = "https://checkout.payments.eu-west.example.com/cart";
        let small = screen(&connected(long), 30, 6, true);
        assert!(small.starts_with("┌ ● example.com ─"), "{small}");
        let wide = screen(&connected(long), 60, 6, true);
        assert!(
            wide.starts_with("┌ ● checkout.payments.eu-west.example.com ─"),
            "{wide}"
        );
        let narrow = screen(&browser(), 20, 6, true);
        assert!(narrow.starts_with("┌ ○ Waiting ─"), "{narrow}");
        assert_eq!(site("localhost:8080"), "localhost");
        assert_eq!(site("a.b.example.co:443"), "example.co");
    }

    /// A browser connected on `url` that the extension names `label` with a tab group of `color`.
    fn labeled(url: &str, label: &str, color: &str) -> Browser {
        let mut browser = connected(url);
        let update = json!({
            "type": "update",
            "state": {
                "tabs": [{"id": 4, "title": "Checkout", "url": url, "active": true}],
                "label": label,
                "group_color": color,
            }
        });
        browser.receive(1, &serde_json::to_vec(&update).unwrap(), Instant::now());
        browser
    }

    #[test]
    fn the_title_names_the_session_as_the_browser_does_with_a_dot_of_its_groups_color() {
        let shop = "https://shop.example.com/checkout";
        let wide = screen(&labeled(shop, "ticks · 9897", "purple"), 60, 6, true);
        assert!(
            wide.starts_with("┌ ● ticks · 9897 · shop.example.com ─"),
            "{wide}"
        );
        // Short of room the site goes first, then the name is cut.
        let small = screen(&labeled(shop, "ticks · 9897", "purple"), 30, 6, true);
        assert!(small.starts_with("┌ ● ticks · 9897 ─"), "{small}");
        let long = "Fix the redirect after signing in on the login…";
        let small = screen(&labeled(shop, long, "purple"), 30, 6, true);
        assert!(small.starts_with("┌ ● Fix the redirect"), "{small}");
        check(&small, 30, &["…"]);

        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        for (color, expected) in [
            ("purple", Color::Magenta),
            ("orange", Color::LightRed),
            ("grey", Color::DarkGray),
        ] {
            let browser = labeled(shop, "ticks", color);
            terminal
                .draw(|frame| draw(frame, &browser, &view(true), &settings(true)))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(2, 0)].symbol(), "●");
            assert_eq!(buffer[(2, 0)].fg, expected, "{color}");
            // Only the dot: the name keeps the frame's color.
            assert_ne!(buffer[(4, 0)].fg, expected, "{color}");
        }
        // Without a name the title is as before, its dot in the frame's color.
        let browser = connected(shop);
        terminal
            .draw(|frame| draw(frame, &browser, &view(true), &settings(true)))
            .unwrap();
        assert!(screen(&browser, 40, 6, true).starts_with("┌ ● shop.example.com ─"));
        assert_eq!(
            terminal.backend().buffer()[(2, 0)].fg,
            terminal.backend().buffer()[(4, 0)].fg
        );
    }

    #[test]
    fn a_looked_at_card_shows_no_keys_and_entering_it_moves_nothing() {
        for browser in [browser(), connected("https://shop.example.com/")] {
            for (columns, rows) in SIZES {
                let entered = screen(&browser, columns, rows, true);
                let looked_at = screen(&browser, columns, rows, false);
                assert!(!looked_at.contains("Help"), "{looked_at}");
                assert!(!looked_at.contains("Stop"), "{looked_at}");
                assert!(entered.contains("? Help"), "{entered}");
                // Everything above the bottom edge stays where it is.
                let content = |screen: &str| {
                    let lines: Vec<_> = screen.lines().collect();
                    lines[..lines.len() - 1].join("\n")
                };
                assert_eq!(content(&entered), content(&looked_at));
            }
        }
    }

    #[test]
    fn switching_the_focus_back_and_forth_leaves_nothing_stale() {
        let browser = connected("https://shop.example.com/");
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        for focused in [true, false, true, false] {
            terminal
                .draw(|frame| draw(frame, &browser, &view(focused), &settings(focused)))
                .unwrap();
            let shown: String = {
                let buffer = terminal.backend().buffer();
                (0..6)
                    .map(|row| {
                        (0..40)
                            .map(|column| buffer[(column, row)].symbol())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            assert_eq!(shown, screen(&browser, 40, 6, focused));
        }
    }

    /// A browser connected with `tabs` and `pending`, after `steps` calls answered in turn.
    fn with(tabs: Value, pending: Value, steps: &[&str]) -> Browser {
        let mut browser = browser();
        let (outbound, _queued) = mpsc::unbounded_channel();
        browser.link(1, outbound);
        let now = Instant::now();
        let catalog: Vec<Value> = ["tabs", "read_page", "computer", "find"]
            .iter()
            .map(|name| json!({"name": name, "description": "A tool.", "input_schema": {"type": "object"}, "mutates": false}))
            .collect();
        let hello = json!({"type": "hello", "extension_version": "0.2.0", "catalog": catalog});
        browser.receive(1, &serde_json::to_vec(&hello).unwrap(), now);
        let update = json!({"type": "update", "state": {"tabs": tabs, "pending": pending}});
        browser.receive(1, &serde_json::to_vec(&update).unwrap(), now);
        for (id, tool) in steps.iter().enumerate() {
            let request = ToolRequest {
                id: json!(id),
                tool: (*tool).into(),
                args: json!({}),
            };
            assert!(browser.call(request, now).is_none());
            let result = json!({"type": "result", "id": id + 1, "ok": true, "result": null});
            browser.receive(1, &serde_json::to_vec(&result).unwrap(), now);
        }
        browser
    }

    #[test]
    fn a_connected_card_lists_its_tabs_what_waits_and_the_latest_three_steps() {
        // As in the card that was reported: connected, no tab yet, one step done.
        let empty = screen(&with(json!([]), json!([]), &["tabs"]), 40, 8, true);
        check(
            &empty,
            40,
            &[
                "No tab in the group yet",
                "Pages the Agent opens show here",
                "› tabs · done",
            ],
        );

        // Many tabs, something to approve and four steps, at every size.
        let tabs: Vec<Value> = (0..12)
            .map(|id| json!({"id": id, "title": format!("Page {id}"), "url": format!("https://www.site{id}.example/x"), "active": id == 5}))
            .collect();
        let pending = json!([{"id": "a1", "kind": "approval", "summary": "Allow site5.example"}]);
        let busy = with(
            json!(tabs),
            pending,
            &["tabs", "read_page", "computer", "find"],
        );
        let tall = screen(&busy, 40, 8, true);
        check(
            &tall,
            40,
            &[
                "▸ Page 5 · site5.example",
                "  11 more tabs",
                "! To approve: Allow site5.example",
                "› find · done",
                "› computer · done",
                "› read_page · done",
            ],
        );
        // Only the latest three steps; the current tab comes first.
        assert!(!tall.contains("› tabs"), "{tall}");
        let first_tab = tall.find("▸ Page 5").unwrap();
        assert!(first_tab < tall.find("11 more tabs").unwrap(), "{tall}");
        // Short of rows, the tabs give way first: what waits and the latest step stay.
        let small = screen(&busy, 30, 6, true);
        check(
            &small,
            30,
            &[
                "▸ Page 5 · site5.example",
                "! To approve: Allow",
                "› find · done",
            ],
        );
    }

    #[test]
    fn an_approval_shows_the_link_that_opens_it_under_it_when_the_link_fits_whole() {
        let link = "https://tickloop.ai/#tickloop-approve=s/a1";
        let pending = json!([
            {"id": "a1", "kind": "approval", "summary": "Allow site.example", "open_url": link},
            {"id": "h1", "kind": "hand_back", "summary": "Pay ¥299.00"},
        ]);
        let page =
            json!([{"id": 1, "title": "Page", "url": "https://site.example/", "active": true}]);
        let browser = with(page, pending, &["tabs", "read_page"]);
        let wide = screen(&browser, 50, 8, true);
        check(
            &wide,
            50,
            &[
                "! To approve: Allow site.example",
                &format!("  {link}"),
                "! Yours to do: Pay ¥299.00",
                "› read_page · done",
            ],
        );
        let shown: Vec<&str> = wide.lines().collect();
        let row = shown
            .iter()
            .position(|line| line.contains("To approve"))
            .unwrap();
        assert!(shown[row + 1].contains(link), "{wide}");
        // Too narrow for the whole link, the card leaves it out rather than cut it.
        let narrow = screen(&browser, 40, 8, true);
        check(
            &narrow,
            40,
            &["! To approve: Allow site.example", "› read_page · done"],
        );
        assert!(!narrow.contains("tickloop-approve"), "{narrow}");
    }

    #[test]
    fn a_card_dragged_down_to_30_by_6_is_laid_out_afresh_at_every_size() {
        let browser = connected("https://shop.example.com/");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let sizes = (0..=24u16).map(|step| (80 - step * 50 / 24, 24 - step * 18 / 24));
        for (columns, rows) in sizes {
            terminal.backend_mut().resize(columns, rows);
            terminal.resize(Rect::new(0, 0, columns, rows)).unwrap();
            terminal
                .draw(|frame| draw(frame, &browser, &view(true), &settings(true)))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let shown = (0..rows)
                .map(|row| {
                    (0..columns)
                        .map(|column| buffer[(column, row)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                shown,
                screen(&browser, columns, rows, true),
                "{columns}×{rows}"
            );
        }
        let small = screen(&browser, 30, 6, true);
        check(&small, 30, &["› read_page · done", "? Help"]);
    }

    /// The rows of `screen` between its top and bottom edges, with the empty ones at the end left
    /// out.
    fn content_rows(screen: &str) -> Vec<String> {
        let lines: Vec<&str> = screen.lines().collect();
        let mut content: Vec<String> = lines[1..lines.len() - 1]
            .iter()
            .map(|line| line.trim_matches(|c: char| c == '│' || c == ' ').to_owned())
            .collect();
        while content.last().is_some_and(String::is_empty) {
            content.pop();
        }
        content
    }

    #[test]
    fn the_rows_a_card_wants_are_the_rows_its_content_fills() {
        let link = "https://tickloop.ai/#tickloop-approve=s/a1";
        let tabs: Vec<Value> = (0..4)
            .map(|id| json!({"id": id, "title": format!("Page {id}"), "url": format!("https://site{id}.example/"), "active": id == 0}))
            .collect();
        let cards = [
            browser(),
            connecting(),
            refused(),
            with(json!([]), json!([]), &[]),
            with(json!([]), json!([]), &["tabs"]),
            connected("https://shop.example.com/checkout"),
            with(
                json!(tabs),
                json!([{"id": "a1", "kind": "approval", "summary": "Allow site0.example", "open_url": link}]),
                &["tabs", "read_page", "computer", "find"],
            ),
        ];
        for browser in &cards {
            for columns in [30, 40, 50] {
                let wanted = wanted_rows(browser, &view(true), &settings(true), columns);
                // At the rows it wants, the card shows everything it would with rows to spare,
                // and fills them all.
                let fitting = content_rows(&screen(browser, columns, wanted, true));
                let roomy = content_rows(&screen(browser, columns, wanted + 4, true));
                assert_eq!(fitting, roomy, "{columns} columns");
                assert_eq!(fitting.len(), usize::from(wanted - 2), "{columns} columns");
            }
        }
        // Connected without a tab after one step: the two lines on the tabs and the step.
        let rows = |browser: &Browser, columns| {
            wanted_rows(browser, &view(true), &settings(true), columns)
        };
        assert_eq!(rows(&cards[4], 40), 2 + 2 + 1);
        // Four tabs, an approval with its link where it fits whole, the latest three steps.
        assert_eq!(rows(&cards[6], 50), 2 + 4 + 2 + 3);
        assert_eq!(rows(&cards[6], 40), 2 + 4 + 1 + 3);
        // The help lists its keys, whatever the card shows behind it.
        let help = View {
            help: true,
            ..view(true)
        };
        assert_eq!(
            usize::from(wanted_rows(&cards[6], &help, &settings(true), 40)),
            2 + help_keys(false).len()
        );
        // Twelve tabs want more rows than the card declares; the host keeps it at its maximum.
        let many: Vec<Value> = (0..12)
            .map(|id| json!({"id": id, "title": format!("Page {id}"), "url": "https://a.example/", "active": id == 0}))
            .collect();
        assert!(rows(&with(json!(many), json!([]), &[]), 40) > crate::catalog::MAX_ROWS);
        assert_eq!(
            tick_market::viewport::wanted_rows(9),
            "\x1b]7777;tickloop;rows=9\x07"
        );
    }

    #[test]
    fn rendering_tells_the_host_the_rows_only_when_they_change() {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        let mut view = view(true);
        let empty = with(json!([]), json!([]), &[]);
        render(&mut terminal, &empty, &mut view, &settings(true)).unwrap();
        assert_eq!(view.told_rows, Some(2 + 2));
        let tabs: Vec<Value> = (0..6)
            .map(|id| json!({"id": id, "title": format!("Page {id}"), "url": "https://a.example/", "active": id == 0}))
            .collect();
        let busy = with(json!(tabs), json!([]), &["tabs"]);
        render(&mut terminal, &busy, &mut view, &settings(true)).unwrap();
        assert_eq!(
            view.told_rows,
            Some(2 + 6 + 1),
            "whatever rows the terminal has now"
        );
        render(&mut terminal, &empty, &mut view, &settings(true)).unwrap();
        assert_eq!(view.told_rows, Some(2 + 2), "and back");
    }

    #[test]
    fn a_waiting_card_says_what_opening_the_tick_wrote_and_how_to_connect() {
        let mut browser = browser();
        let tall = screen(&browser, 40, 8, false);
        assert!(
            tall.contains("Registered the host for Google Chrome."),
            "{tall}"
        );
        assert!(
            tall.contains("Open the browser with the Tickloop"),
            "{tall}"
        );
        // Until the store page exists, the development build's way to install.
        assert!(
            tall.contains("No extension yet? Load it unpacked in")
                && tall.contains("chrome://extensions"),
            "{tall}"
        );

        browser = Browser::new(
            Catalog::default(),
            Setup {
                session: Session::new(Some("hosted:4kqY8bXc2RnT0mWv7sLpQe"), None, 1),
                socket: PathBuf::from("/tmp/tick.sock"),
                registration: Ok(vec![
                    registration::Entry {
                        browser: "Google Chrome",
                        manifest: PathBuf::from("/c.json"),
                        status: registration::Status::Current,
                    },
                    registration::Entry {
                        browser: "Microsoft Edge",
                        manifest: PathBuf::from("/e.json"),
                        status: registration::Status::Replaced {
                            previous_path: Some("/other/tick-browser".into()),
                        },
                    },
                ]),
            },
        );
        let tall = screen(&browser, 40, 8, false);
        assert!(
            tall.contains("Repointed Microsoft Edge's host here."),
            "{tall}"
        );
        assert!(!tall.contains("Registered"), "{tall}");
        assert_eq!(
            waiting_text(browser.setup(), true),
            "打开装了 Tickloop 扩展的浏览器，它会自己连上。\n没装扩展？在 chrome://extensions 打开开发者模式，选「加载已解压的扩展程序」。\n已把 Microsoft Edge 的主机改指本程序。"
        );
    }

    #[test]
    fn hosts_come_from_urls() {
        assert_eq!(
            host("https://shop.example.com/cart?x=1"),
            "shop.example.com"
        );
        assert_eq!(host("about:blank"), "about:blank");
        assert_eq!(host("http://localhost:8080#top"), "localhost:8080");
    }
}
