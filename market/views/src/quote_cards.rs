//! Quotes as a grid of cards: as many cards across as the Panel holds at their narrowest, each
//! showing the price and its change, then the session's other figures its width allows.

use crate::{config::Settings, output::DisplayCell, runtime::ViewState};
use crossterm::event::KeyCode;
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
};
use std::cell::Cell;
use tick_provider::api::Quote;
use tickloop_ui::{
    format::{truncate, width, write_text},
    settings::AmbiguousWidth,
    styles::PriceDirection,
};

/// The narrowest card, which fills the narrowest Panel; a wider Panel fits as many cards across
/// as this allows and shares the rest between them.
const MINIMUM_CARD_COLUMNS: usize = crate::metadata::MINIMUM_PANEL_COLUMNS as usize;
const CARD_ROWS: usize = 4;
const CARD_GAP: usize = 2;
const FIGURE_GAP: &str = "  ";
const LATEST_PRICE: usize = 2;
const CHANGE: usize = 3;
const CHANGE_PERCENT: usize = 4;
const OPEN: usize = 5;
const HIGH: usize = 6;
const LOW: usize = 7;
const PREVIOUS_CLOSE: usize = 8;
const VOLUME: usize = 9;
const TURNOVER: usize = 10;

/// The cards of a quote list and the number across them as last drawn, which moving up and
/// down steps by.
pub struct CardGrid {
    cards_per_row: Cell<usize>,
}

impl Default for CardGrid {
    fn default() -> Self {
        Self {
            cards_per_row: Cell::new(1),
        }
    }
}

impl CardGrid {
    /// Moves the selection among `count` cards: j / k and the up and down arrows by a row of
    /// cards, h / l by one card. Returns whether `key` was one of them.
    pub fn move_selection(&self, key: KeyCode, count: usize, state: &mut ViewState) -> bool {
        let last = count.saturating_sub(1);
        let per_row = self.cards_per_row.get();
        state.selected = match key {
            KeyCode::Char('j') | KeyCode::Down => (state.selected + per_row).min(last),
            KeyCode::Char('k') | KeyCode::Up => state.selected.saturating_sub(per_row),
            KeyCode::Char('l') => (state.selected + 1).min(last),
            KeyCode::Char('h') => state.selected.saturating_sub(1),
            _ => return false,
        };
        true
    }

    /// Draws a card for each quote across the whole screen, `markers` giving each card's symbol a
    /// prefix. The cards are the Tick's only borders, so the last one shown carries what the
    /// frame's bottom edge would: `title` and which cards show when not all fit, the problem
    /// with the data, and the keys.
    pub fn draw(
        &self,
        frame: &mut ratatui::Frame<'_>,
        title: &str,
        quotes: &[Quote],
        markers: &[&str],
        state: &mut ViewState,
        settings: &Settings,
    ) {
        let area = frame.area();
        let styles = settings.styles();
        frame.buffer_mut().set_style(area, styles.panel_base());
        let inner = area;
        if quotes.is_empty() {
            write_text(
                frame.buffer_mut(),
                inner.x,
                inner.y,
                "No data",
                inner.width as usize,
                settings.ambiguous_width,
                styles.panel_base().patch(styles.muted_text),
            );
            return;
        }
        let per_row = (usize::from(inner.width) + CARD_GAP) / (MINIMUM_CARD_COLUMNS + CARD_GAP);
        let per_row = per_row.clamp(1, quotes.len());
        self.cards_per_row.set(per_row);
        let card_width = (usize::from(inner.width) - CARD_GAP * (per_row - 1)) / per_row;
        let visible_rows = (usize::from(inner.height) / CARD_ROWS).max(1);
        state.selected = state.selected.min(quotes.len() - 1);
        let selected_row = state.selected / per_row;
        if selected_row < state.offset {
            state.offset = selected_row;
        }
        if selected_row >= state.offset + visible_rows {
            state.offset = selected_row + 1 - visible_rows;
        }
        let total_rows = quotes.len().div_ceil(per_row);
        let range = (total_rows > visible_rows).then(|| {
            let first = state.offset * per_row + 1;
            let last = ((state.offset + visible_rows) * per_row).min(quotes.len());
            format!("{first}–{last}/{}", quotes.len())
        });
        // Which cards show stays whole; the title gives way first.
        let lead = [range.as_deref(), Some(title)]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let mut last_card = None;
        let cells = crate::commands::quote::table(quotes).display_rows(settings);
        for (index, (quote, cells)) in quotes.iter().zip(&cells).enumerate() {
            let marker = markers.get(index).copied().unwrap_or_default();
            let row = index / per_row;
            if row < state.offset || row >= state.offset + visible_rows {
                continue;
            }
            let card = Rect::new(
                inner.x + ((index % per_row) * (card_width + CARD_GAP)) as u16,
                inner.y + ((row - state.offset) * CARD_ROWS) as u16,
                card_width as u16,
                CARD_ROWS as u16,
            )
            .intersection(inner);
            let selected = index == state.selected;
            if selected {
                state.focus_row = Some(card.y);
            }
            draw_card(frame, card, quote, marker, cells, selected, settings);
            last_card = Some(card);
        }
        if let Some(card) = last_card.filter(|card| card.height == CARD_ROWS as u16) {
            let edge = Rect::new(card.x, card.bottom() - 1, card.width, 1);
            crate::runtime::draw_bottom_edge(frame, edge, &lead, &state.edge, settings);
        }
    }
}

/// Draws one quote as a bordered card: its marker, symbol and name on the border, the price and its change
/// inside, then as many of the session's other figures as the card's width holds.
fn draw_card(
    frame: &mut ratatui::Frame<'_>,
    card: Rect,
    quote: &Quote,
    marker: &str,
    cells: &[DisplayCell],
    selected: bool,
    settings: &Settings,
) {
    let styles = settings.styles();
    let ambiguous = settings.ambiguous_width;
    let border_style = if selected {
        styles.panel_base().patch(styles.selection)
    } else {
        styles.panel_base().patch(styles.border)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);
    let body = block.inner(card);
    frame.render_widget(block, card);
    let symbol = if quote.error.is_some() {
        format!(" ! {} ", quote.symbol)
    } else if marker.is_empty() {
        format!(" {} ", quote.symbol)
    } else {
        format!(" {marker} {} ", quote.symbol)
    };
    let symbol_width = width(&symbol, ambiguous);
    let mut symbol_style = border_style.patch(styles.title);
    if quote.error.is_some() {
        symbol_style = symbol_style.patch(styles.error);
    }
    write_text(
        frame.buffer_mut(),
        card.x + 1,
        card.y,
        &symbol,
        usize::from(card.width.saturating_sub(2)),
        ambiguous,
        symbol_style,
    );
    let name_room = usize::from(card.width.saturating_sub(2)).saturating_sub(symbol_width + 1);
    if name_room > 2 && !quote.name.is_empty() {
        let name = format!(" {} ", truncate(&quote.name, name_room - 2, ambiguous));
        write_text(
            frame.buffer_mut(),
            card.right() - 1 - width(&name, ambiguous) as u16,
            card.y,
            &name,
            name_room,
            ambiguous,
            border_style.patch(styles.muted_text),
        );
    }
    let columns = usize::from(body.width);
    if let Some(error) = &quote.error {
        let error_style = styles.panel_base().patch(styles.error);
        write_text(
            frame.buffer_mut(),
            body.x,
            body.y,
            "Error",
            columns,
            ambiguous,
            error_style,
        );
        write_text(
            frame.buffer_mut(),
            body.x,
            body.y + 1,
            &truncate(error, columns, ambiguous),
            columns,
            ambiguous,
            error_style,
        );
        return;
    }
    let price_style = |cell: &DisplayCell| styles.panel_base().patch(styles.price(cell.direction));
    let percent = &cells[CHANGE_PERCENT];
    write_text(
        frame.buffer_mut(),
        body.x,
        body.y,
        &cells[LATEST_PRICE].text,
        columns,
        ambiguous,
        price_style(percent).patch(styles.strong),
    );
    write_right(
        frame,
        body,
        body.y,
        &percent.text,
        price_style(percent),
        ambiguous,
    );
    let change = &cells[CHANGE];
    let signed_change = match change.direction {
        PriceDirection::Up => format!("+{}", change.text),
        _ => change.text.clone(),
    };
    write_text(
        frame.buffer_mut(),
        body.x,
        body.y + 1,
        &signed_change,
        columns,
        ambiguous,
        price_style(change),
    );
    let figures = [
        format!("H {} L {}", cells[HIGH].text, cells[LOW].text),
        format!("Open {}", cells[OPEN].text),
        format!("Prev {}", cells[PREVIOUS_CLOSE].text),
        format!("Vol {}", cells[VOLUME].text),
        format!("Turnover {}", cells[TURNOVER].text),
    ];
    let room = columns.saturating_sub(width(&signed_change, ambiguous) + FIGURE_GAP.len());
    let mut shown = 0;
    while shown < figures.len() && width(&figures[..=shown].join(FIGURE_GAP), ambiguous) <= room {
        shown += 1;
    }
    write_right(
        frame,
        body,
        body.y + 1,
        &figures[..shown].join(FIGURE_GAP),
        styles.panel_base().patch(styles.muted_text),
        ambiguous,
    );
}

fn write_right(
    frame: &mut ratatui::Frame<'_>,
    body: Rect,
    y: u16,
    text: &str,
    style: ratatui::style::Style,
    ambiguous: AmbiguousWidth,
) {
    let text_width = width(text, ambiguous).min(usize::from(body.width));
    write_text(
        frame.buffer_mut(),
        body.right() - text_width as u16,
        y,
        text,
        text_width,
        ambiguous,
        style,
    );
}
