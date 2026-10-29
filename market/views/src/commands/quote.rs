use crate::{args::QuoteArgs, config::Settings, output::TableData};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Write;
use tick_provider::{
    api::{Quote, QuoteServices, normalize_symbol},
    error::Result,
};

pub const HEADERS: &[&str] = &[
    "Symbol",
    "Name",
    "Latest Price",
    "Change",
    "Change Percent",
    "Open",
    "High",
    "Low",
    "Previous Close",
    "Volume",
    "Turnover",
    "Updated At",
    "Error",
    "Session",
];

pub fn table(quotes: &[Quote]) -> TableData {
    TableData {
        headers: HEADERS.to_vec(),
        rows: quotes
            .iter()
            .map(|quote| {
                let mut row = vec![json!(quote.symbol), json!(quote.name)];
                if let Some(error) = &quote.error {
                    row.extend(vec![Value::Null; HEADERS.len() - 4]);
                    row.extend([json!(error), Value::Null]);
                    return row;
                }
                let change = quote.latest_price - quote.previous_close;
                let percentage = if quote.previous_close.is_zero() {
                    Value::Null
                } else {
                    json!(
                        (change / quote.previous_close * Decimal::ONE_HUNDRED)
                            .normalize()
                            .to_string()
                    )
                };
                row.extend([
                    json!(quote.latest_price.normalize().to_string()),
                    if quote.previous_close.is_zero() {
                        Value::Null
                    } else {
                        json!(change.normalize().to_string())
                    },
                    percentage,
                    if quote.open.is_zero() {
                        Value::Null
                    } else {
                        json!(quote.open)
                    },
                    json!(quote.high),
                    json!(quote.low),
                    if quote.previous_close.is_zero() {
                        Value::Null
                    } else {
                        json!(quote.previous_close)
                    },
                    json!(quote.volume),
                    json!(quote.turnover),
                    json!(quote.updated_at),
                    Value::Null,
                    json!(quote.session),
                ]);
                row
            })
            .collect(),
    }
}

/// Arguments of the `quotes` Tick Tool; the schema the Agent reads is derived from it.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuotesArgs {
    /// Symbols to price, such as `AAPL.US` or `700.HK`.
    pub symbols: Vec<String>,
}

pub fn tools() -> crate::tools::Tools {
    crate::tools::Tools::default()
        .query::<QuotesArgs>(
            "quotes",
            "Latest price, change, range, and volume for the given symbols, in the Quote table's shape.",
            |api: &QuoteServices, args| {
                Box::pin(async move {
                    let symbols: Vec<String> =
                        args.symbols.iter().map(|symbol| normalize_symbol(symbol)).collect();
                    Ok(table(&api.quotes(&symbols).await?).json())
                })
            },
        )
        .recover_with("quotes")
}

pub async fn run_once(
    args: &QuoteArgs,
    api: &QuoteServices,
    settings: &Settings,
    color: bool,
    output: &mut impl Write,
    updates: &mut tick_sdk::stream::TickStream,
) -> Result<()> {
    let symbols: Vec<String> = args
        .symbols
        .iter()
        .map(|symbol| normalize_symbol(symbol))
        .collect();
    let quotes = api.quotes(&symbols).await?;
    let table = table(&quotes);
    updates.publish(table.json())?;
    table.write(args.output.format, settings, color, output)
}

pub struct QuoteView {
    pub quotes: Vec<Quote>,
    cards: crate::quote_cards::CardGrid,
}

impl QuoteView {
    pub fn new(quotes: Vec<Quote>) -> Self {
        Self {
            quotes,
            cards: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl crate::runtime::LiveView for QuoteView {
    /// The Monitor names the Tick and its symbols, so its frame needs no title of its own.
    /// The cards carry their own borders, so the Tick shows no frame around them.
    fn framed(&self) -> bool {
        false
    }
    fn title(&self) -> String {
        String::new()
    }
    fn table(&self) -> TableData {
        table(&self.quotes)
    }
    fn priority(&self) -> &[usize] {
        &[0, 2, 4, 1, 3, 12, 13, 9, 8, 5, 6, 7, 10, 11]
    }
    fn help(&self) -> &'static str {
        "?  Help\nj / k / Ctrl+N / Ctrl+P / ↓ / ↑  Card below / above\nh / l / ← / →  Previous / next card\nPageUp / PageDown  Page\ng / G  First / last"
    }
    async fn on_key(
        &mut self,
        key: crossterm::event::KeyCode,
        _api: &QuoteServices,
        state: &mut crate::runtime::ViewState,
    ) -> Result<bool> {
        use crossterm::event::KeyCode;
        let key = match key {
            KeyCode::Left => KeyCode::Char('h'),
            KeyCode::Right => KeyCode::Char('l'),
            key => key,
        };
        Ok(self.cards.move_selection(key, self.quotes.len(), state))
    }
    fn draw(
        &self,
        frame: &mut ratatui::Frame<'_>,
        state: &mut crate::runtime::ViewState,
        settings: &Settings,
    ) {
        self.cards
            .draw(frame, &self.title(), &self.quotes, &[], state, settings);
    }
    fn on_market(&mut self, event: &tick_provider::api::MarketEvent) {
        for quote in &mut self.quotes {
            quote.update_event(event);
        }
    }
}

pub async fn run_live<
    B: ratatui::backend::Backend + crate::viewport::KeyBubbling,
    S: futures_util::Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin,
>(
    args: &QuoteArgs,
    api: &QuoteServices,
    settings: &Settings,
    terminal: &mut ratatui::Terminal<B>,
    mut input: S,
    updates: &mut tick_sdk::stream::TickStream,
) -> Result<()> {
    let symbols: Vec<String> = args
        .symbols
        .iter()
        .map(|symbol| normalize_symbol(symbol))
        .collect();
    let Some(quotes) = crate::runtime::cancellable(api.quotes(&symbols), &mut input).await? else {
        return Ok(());
    };
    let subscribed: Vec<String> = quotes
        .iter()
        .filter(|quote| quote.error.is_none())
        .map(|quote| quote.symbol.clone())
        .collect();
    let result = async {
        let Some(market) = crate::runtime::cancellable(api.take_events(), &mut input).await? else {
            return Ok(());
        };
        if !subscribed.is_empty()
            && crate::runtime::cancellable(api.subscribe_quotes(&subscribed), &mut input)
                .await?
                .is_none()
        {
            return Ok(());
        }
        crate::runtime::run(
            &mut QuoteView::new(quotes),
            api,
            settings,
            terminal,
            input,
            market,
            updates,
        )
        .await
    }
    .await;
    let cleanup = if subscribed.is_empty() {
        Ok(())
    } else {
        tokio::time::timeout(
            crate::runtime::CLEANUP_TIMEOUT,
            api.unsubscribe_quotes(&subscribed),
        )
        .await
        .unwrap_or_else(|_| {
            Err(tick_provider::error::Error::network(
                "subscription cleanup timed out",
            ))
        })
    };
    result.and(cleanup)
}
