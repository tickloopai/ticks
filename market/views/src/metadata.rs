use serde::Serialize;
use tick_protocol::{ScreenDeclaration, StateDeclaration, StateSeries, TickTool};
use tick_provider::error::{Error, Result};
use tick_sdk::stream::Describe;

/// Ticks whose content comes from services the shared runtime does not hand to query tools, so
/// they declare no recovery tool. The Agent re-reads their Panel content with `TickRead`.
const EXEMPT_FROM_RECOVERY: [TickKind; 2] = [TickKind::Positions, TickKind::News];

#[derive(clap::ValueEnum, Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TickKind {
    Quote,
    Watchlist,
    Kline,
    Positions,
    News,
    Earnings,
    Options,
    Reader,
    Community,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TickRuntimeClass {
    Command,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TickConfigurationRequirement {
    None,
    Optional,
    UserInput,
}

/// The narrowest Panel every Tick lays out whole: the content of Tickloop's narrowest Monitor
/// column, 32 columns less its `│ ` separator, which a Tick is shown in.
pub const MINIMUM_PANEL_COLUMNS: u16 = 30;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct MinimumPanelSize {
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TickDescriptor {
    pub kind: TickKind,
    pub title: &'static str,
    pub description: &'static str,
    pub runtime: TickRuntimeClass,
    pub configuration: TickConfigurationRequirement,
    pub minimum_panel_size: Option<MinimumPanelSize>,
    /// Version of the Tick Instance stream protocol this Tick speaks.
    pub protocol: u32,
    /// The query tool that rebuilds what the latest-only update stream dropped.
    pub recover_with: Option<&'static str>,
    pub tools: Vec<TickTool>,
    /// The arrays in the Tick's state ordered in time.
    pub series: Vec<StateSeries>,
}

impl TickDescriptor {
    /// Rejects a catalog the Agent could not use: a duplicate tool name, a
    /// recovery tool that is not a declared query tool, and — the ADR 0007 rule —
    /// a Tick that pushes Tick Updates without naming the query tool that
    /// recovers what the latest-only stream drops. Every shipped Tick pushes
    /// updates, so only the kinds in [`EXEMPT_FROM_RECOVERY`] may leave it unset.
    pub fn validate(&self) -> Result<()> {
        if let Some(duplicate) = self.tools.iter().enumerate().find_map(|(index, tool)| {
            self.tools[..index]
                .iter()
                .any(|earlier| earlier.name == tool.name)
                .then(|| tool.name.clone())
        }) {
            return Err(Error::usage(format!(
                "{} declares the Tick Tool {duplicate:?} twice",
                self.kind.name()
            )));
        }
        let queries = || self.tools.iter().filter(|tool| !tool.mutates);
        match self.recover_with {
            Some(recovery) if !queries().any(|tool| tool.name == recovery) => {
                Err(Error::usage(format!(
                    "{} recovers Tick Updates with {recovery:?}, which is not a declared query tool",
                    self.kind.name()
                )))
            }
            None if !EXEMPT_FROM_RECOVERY.contains(&self.kind) => Err(Error::usage(format!(
                "{} pushes Tick Updates but names no recover_with query tool",
                self.kind.name()
            ))),
            _ => Ok(()),
        }
    }
}

impl TickKind {
    pub const ALL: [Self; 9] = [
        Self::Quote,
        Self::Watchlist,
        Self::Kline,
        Self::Positions,
        Self::News,
        Self::Earnings,
        Self::Options,
        Self::Reader,
        Self::Community,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Quote => "quote",
            Self::Watchlist => "watchlist",
            Self::Kline => "kline",
            Self::Positions => "positions",
            Self::News => "news",
            Self::Earnings => "earnings",
            Self::Options => "options",
            Self::Reader => "reader",
            Self::Community => "community",
        }
    }

    /// The narrowest and shortest Panel that shows every key field of the Tick whole; a
    /// smaller Panel scrolls over a canvas of this size. The renderer reads this on every
    /// frame, so it stays independent of the Tick Tool catalog. Every Tick is laid out for
    /// [`MINIMUM_PANEL_COLUMNS`]; a narrower layout gives up its secondary fields first.
    pub fn minimum_panel_size(self) -> MinimumPanelSize {
        let rows = match self {
            // Symbol, price and change %; the name gives way first.
            Self::Quote => 6,
            // Symbol, price and change % beside the market-state marker.
            Self::Watchlist => 6,
            // The price summary wrapped over four lines and the period, averages and live keys.
            Self::Kline => 14,
            // Symbol, P/L and P/L % for the selected holding and those beside it; the price,
            // quantity and value give way first.
            Self::Positions => 6,
            // A headline beside the time; the source gives way first.
            Self::News => 6,
            // The mode's caveat, and each table's leading two columns.
            Self::Earnings => 14,
            // Spot, ATM move, and strike with both midpoints.
            Self::Options => 14,
            // Prose below the frame's title; a short Panel leaves out the document heading.
            Self::Reader => 6,
            // A post headline beside the date; the post kind gives way first.
            Self::Community => 6,
        };
        MinimumPanelSize {
            columns: MINIMUM_PANEL_COLUMNS,
            rows,
        }
    }

    pub fn descriptor(self) -> TickDescriptor {
        let (title, description, configuration) = match self {
            Self::Quote => (
                "Quote",
                "Watch current prices for one or more symbols.",
                TickConfigurationRequirement::UserInput,
            ),
            Self::Watchlist => (
                "Watchlist",
                "Monitor a Longbridge watchlist group.",
                TickConfigurationRequirement::Optional,
            ),
            Self::Kline => (
                "Kline",
                "Inspect candlesticks for a symbol.",
                TickConfigurationRequirement::UserInput,
            ),
            Self::Positions => (
                "Positions",
                "Monitor holdings and profit and loss.",
                TickConfigurationRequirement::None,
            ),
            Self::News => (
                "News",
                "Read market-wide or symbol news.",
                TickConfigurationRequirement::Optional,
            ),
            Self::Earnings => (
                "Earnings",
                "Review past earnings and how the market reacted.",
                TickConfigurationRequirement::UserInput,
            ),
            Self::Options => (
                "Futu Options",
                "Inspect option pricing, IV and saved snapshots from Futu OpenD.",
                TickConfigurationRequirement::UserInput,
            ),
            Self::Reader => (
                "Reader",
                "Read original financial documents and cite stable evidence.",
                TickConfigurationRequirement::UserInput,
            ),
            Self::Community => (
                "Community",
                "Follow one Longbridge community member's posts and reply to them.",
                TickConfigurationRequirement::UserInput,
            ),
        };
        let catalog = crate::tools::for_kind(self).catalog();
        TickDescriptor {
            kind: self,
            title,
            description,
            runtime: TickRuntimeClass::Command,
            configuration,
            minimum_panel_size: Some(self.minimum_panel_size()),
            protocol: tick_protocol::NATIVE_PROTOCOL_VERSION,
            recover_with: catalog.recover_with,
            tools: catalog.tools,
            series: catalog.series,
        }
    }
}

/// A shipped Tick's catalog is fixed for its kind while it runs, so one revision names it.
const CATALOG_REVISION: &str = "1";

impl Describe for TickKind {
    fn descriptor(&self) -> std::result::Result<tick_protocol::TickDescriptor, String> {
        let descriptor = TickKind::descriptor(*self);
        descriptor.validate().map_err(|error| error.message)?;
        Ok(tick_protocol::TickDescriptor {
            protocol: descriptor.protocol,
            kind: self.name().to_owned(),
            title: descriptor.title.to_owned(),
            description: descriptor.description.to_owned(),
            revision: CATALOG_REVISION.to_owned(),
            recover_with: descriptor.recover_with.map(str::to_owned),
            tools: descriptor.tools,
            state: (!descriptor.series.is_empty()).then_some(StateDeclaration {
                series: descriptor.series,
            }),
            screen: Some(ScreenDeclaration {
                min_rows: descriptor.minimum_panel_size.map(|minimum| minimum.rows),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, mutates: bool) -> TickTool {
        TickTool {
            name: name.to_owned(),
            description: String::new(),
            input_schema: serde_json::Value::Null,
            mutates,
        }
    }

    fn descriptor(recover_with: Option<&'static str>, tools: Vec<TickTool>) -> TickDescriptor {
        TickDescriptor {
            recover_with,
            tools,
            ..TickKind::Watchlist.descriptor()
        }
    }

    #[test]
    fn every_shipped_tick_declares_a_usable_catalog() {
        for kind in TickKind::ALL {
            kind.descriptor().validate().unwrap();
        }
    }

    #[test]
    fn the_ticks_whose_state_holds_a_time_series_declare_its_newest_end() {
        use tick_protocol::SeriesEnd;
        let declared = |kind: TickKind| {
            Describe::descriptor(&kind)
                .unwrap()
                .state
                .map(|state| {
                    state
                        .series
                        .into_iter()
                        .map(|series| (series.path, series.newest))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            declared(TickKind::News),
            [("/items".to_owned(), SeriesEnd::First)]
        );
        assert_eq!(
            declared(TickKind::Community),
            [("/posts".to_owned(), SeriesEnd::First)]
        );
        // Kline, Earnings and Options publish summaries; their series stay behind their tools.
        for kind in [
            TickKind::Quote,
            TickKind::Watchlist,
            TickKind::Kline,
            TickKind::Positions,
            TickKind::Earnings,
            TickKind::Options,
            TickKind::Reader,
        ] {
            assert!(declared(kind).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn every_tick_declares_the_rows_it_stays_usable_at_within_the_hosts_range() {
        use tick_protocol::{MAX_SCREEN_ROWS, MIN_SCREEN_ROWS};
        let declared = |kind: TickKind| {
            Describe::descriptor(&kind)
                .unwrap()
                .screen
                .and_then(|screen| screen.min_rows)
        };
        for kind in TickKind::ALL {
            let rows = declared(kind).unwrap();
            assert!(
                (MIN_SCREEN_ROWS..=MAX_SCREEN_ROWS).contains(&rows),
                "{kind:?}"
            );
            assert_eq!(rows, kind.minimum_panel_size().rows, "{kind:?}");
        }
        assert_eq!(
            TickKind::ALL.map(declared),
            [6, 6, 14, 6, 6, 14, 14, 6, 6].map(Some)
        );
    }

    #[test]
    fn a_tick_that_pushes_updates_must_name_the_tool_that_recovers_them() {
        let error = descriptor(None, vec![tool("rows", false)])
            .validate()
            .unwrap_err();
        assert!(error.message.contains("recover_with"), "{error:?}");

        let without_any_tool = descriptor(None, Vec::new()).validate().unwrap_err();
        assert!(without_any_tool.message.contains("recover_with"));
    }

    #[test]
    fn only_the_exempt_kinds_may_leave_recover_with_unset() {
        for kind in TickKind::ALL {
            let descriptor = TickDescriptor {
                recover_with: None,
                tools: Vec::new(),
                ..kind.descriptor()
            };
            assert_eq!(
                descriptor.validate().is_ok(),
                EXEMPT_FROM_RECOVERY.contains(&kind),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn recover_with_must_name_a_declared_query_tool() {
        let unknown = descriptor(Some("candles"), vec![tool("rows", false)])
            .validate()
            .unwrap_err();
        assert!(unknown.message.contains("candles"), "{unknown:?}");

        let mutating = descriptor(Some("mark"), vec![tool("mark", true)])
            .validate()
            .unwrap_err();
        assert!(mutating.message.contains("mark"), "{mutating:?}");
    }

    #[test]
    fn a_duplicate_tool_name_is_rejected() {
        let error = descriptor(Some("rows"), vec![tool("rows", false), tool("rows", false)])
            .validate()
            .unwrap_err();
        assert!(error.message.contains("twice"), "{error:?}");
    }
}
