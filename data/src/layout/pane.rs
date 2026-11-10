use exchange::{TickMultiplier, TickerInfo, Timeframe};
use serde::{Deserialize, Serialize};

use crate::chart::{bookmap, comparison, heatmap, kline};
use crate::panel::{ladder, microstructure, timeandsales};
use crate::stream::PersistStreamKind;
use crate::util::ok_or_default;

use crate::chart::{
    Basis, ViewConfig,
    heatmap::HeatmapStudy,
    indicator::{HeatmapIndicator, KlineIndicator},
    kline::KlineChartKind,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize)]
pub enum Pane {
    Split {
        axis: Axis,
        ratio: f32,
        a: Box<Pane>,
        b: Box<Pane>,
    },
    Starter {
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    BookmapChart {
        #[serde(deserialize_with = "ok_or_default", default)]
        studies: Vec<HeatmapStudy>,
        #[serde(deserialize_with = "ok_or_default", default)]
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        indicators: Vec<HeatmapIndicator>,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    KlineChart {
        layout: ViewConfig,
        kind: KlineChartKind,
        #[serde(deserialize_with = "ok_or_default", default)]
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        indicators: Vec<KlineIndicator>,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    ComparisonChart {
        stream_type: Vec<PersistStreamKind>,
        #[serde(deserialize_with = "ok_or_default")]
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    TimeAndSales {
        stream_type: Vec<PersistStreamKind>,
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    Ladder {
        stream_type: Vec<PersistStreamKind>,
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
    Microstructure {
        stream_type: Vec<PersistStreamKind>,
        settings: Settings,
        #[serde(deserialize_with = "ok_or_default", default)]
        link_group: Option<LinkGroup>,
    },
}

/// Hand-written `Deserialize` (rather than the derive) so that saved layouts from
/// before the Heatmap/Bookmap GPU migration — which used the now-removed
/// `Pane::HeatmapChart` (legacy CPU chart) and `Pane::ShaderHeatmap` (GPU chart,
/// since folded into `Pane::BookmapChart`) tags — still load instead of failing
/// `read_from_file` and resetting the user's entire saved state (see
/// `layout::load_saved_state` in the `acterminal` binary crate).
impl<'de> Deserialize<'de> for Pane {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum PaneOnDisk {
            Split {
                axis: Axis,
                ratio: f32,
                a: Box<Pane>,
                b: Box<Pane>,
            },
            Starter {
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            /// Deprecated: legacy CPU heatmap chart, removed outright.
            HeatmapChart {
                #[serde(deserialize_with = "ok_or_default", default)]
                studies: Vec<HeatmapStudy>,
                #[serde(deserialize_with = "ok_or_default", default)]
                stream_type: Vec<PersistStreamKind>,
                #[serde(deserialize_with = "ok_or_default")]
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                indicators: Vec<HeatmapIndicator>,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            /// Deprecated: GPU heatmap chart, folded into `BookmapChart`.
            ShaderHeatmap {
                #[serde(deserialize_with = "ok_or_default", default)]
                studies: Vec<HeatmapStudy>,
                #[serde(deserialize_with = "ok_or_default", default)]
                stream_type: Vec<PersistStreamKind>,
                #[serde(deserialize_with = "ok_or_default")]
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                indicators: Vec<HeatmapIndicator>,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            BookmapChart {
                #[serde(deserialize_with = "ok_or_default", default)]
                studies: Vec<HeatmapStudy>,
                #[serde(deserialize_with = "ok_or_default", default)]
                stream_type: Vec<PersistStreamKind>,
                #[serde(deserialize_with = "ok_or_default")]
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                indicators: Vec<HeatmapIndicator>,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            KlineChart {
                layout: ViewConfig,
                kind: KlineChartKind,
                #[serde(deserialize_with = "ok_or_default", default)]
                stream_type: Vec<PersistStreamKind>,
                #[serde(deserialize_with = "ok_or_default")]
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                indicators: Vec<KlineIndicator>,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            ComparisonChart {
                stream_type: Vec<PersistStreamKind>,
                #[serde(deserialize_with = "ok_or_default")]
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            TimeAndSales {
                stream_type: Vec<PersistStreamKind>,
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            Ladder {
                stream_type: Vec<PersistStreamKind>,
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
            Microstructure {
                stream_type: Vec<PersistStreamKind>,
                settings: Settings,
                #[serde(deserialize_with = "ok_or_default", default)]
                link_group: Option<LinkGroup>,
            },
        }

        Ok(match PaneOnDisk::deserialize(deserializer)? {
            PaneOnDisk::Split { axis, ratio, a, b } => Pane::Split { axis, ratio, a, b },
            PaneOnDisk::Starter { link_group } => Pane::Starter { link_group },
            // Both deprecated variants remap onto the now-sole depth-chart pane kind.
            PaneOnDisk::HeatmapChart {
                studies,
                stream_type,
                settings,
                indicators,
                link_group,
            }
            | PaneOnDisk::ShaderHeatmap {
                studies,
                stream_type,
                settings,
                indicators,
                link_group,
            }
            | PaneOnDisk::BookmapChart {
                studies,
                stream_type,
                settings,
                indicators,
                link_group,
            } => Pane::BookmapChart {
                studies,
                stream_type,
                settings,
                indicators,
                link_group,
            },
            PaneOnDisk::KlineChart {
                layout,
                kind,
                stream_type,
                settings,
                indicators,
                link_group,
            } => Pane::KlineChart {
                layout,
                kind,
                stream_type,
                settings,
                indicators,
                link_group,
            },
            PaneOnDisk::ComparisonChart {
                stream_type,
                settings,
                link_group,
            } => Pane::ComparisonChart {
                stream_type,
                settings,
                link_group,
            },
            PaneOnDisk::TimeAndSales {
                stream_type,
                settings,
                link_group,
            } => Pane::TimeAndSales {
                stream_type,
                settings,
                link_group,
            },
            PaneOnDisk::Ladder {
                stream_type,
                settings,
                link_group,
            } => Pane::Ladder {
                stream_type,
                settings,
                link_group,
            },
            PaneOnDisk::Microstructure {
                stream_type,
                settings,
                link_group,
            } => Pane::Microstructure {
                stream_type,
                settings,
                link_group,
            },
        })
    }
}

impl Default for Pane {
    fn default() -> Self {
        Pane::Starter { link_group: None }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Settings {
    pub tick_multiply: Option<exchange::TickMultiplier>,
    pub visual_config: Option<VisualConfig>,
    pub selected_basis: Option<Basis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum LinkGroup {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
}

impl LinkGroup {
    pub const ALL: [LinkGroup; 9] = [
        LinkGroup::A,
        LinkGroup::B,
        LinkGroup::C,
        LinkGroup::D,
        LinkGroup::E,
        LinkGroup::F,
        LinkGroup::G,
        LinkGroup::H,
        LinkGroup::I,
    ];
}

impl std::fmt::Display for LinkGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let c = match self {
            LinkGroup::A => "1",
            LinkGroup::B => "2",
            LinkGroup::C => "3",
            LinkGroup::D => "4",
            LinkGroup::E => "5",
            LinkGroup::F => "6",
            LinkGroup::G => "7",
            LinkGroup::H => "8",
            LinkGroup::I => "9",
        };
        write!(f, "{c}")
    }
}

/// Defines the specific configuration for different types of pane settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum VisualConfig {
    Heatmap(heatmap::Config),
    Bookmap(bookmap::Config),
    TimeAndSales(timeandsales::Config),
    Kline(kline::Config),
    Ladder(ladder::Config),
    Comparison(comparison::Config),
    Microstructure(microstructure::Config),
}

impl VisualConfig {
    pub fn heatmap(&self) -> Option<heatmap::Config> {
        match self {
            Self::Heatmap(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn bookmap(&self) -> Option<bookmap::Config> {
        match self {
            Self::Bookmap(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn time_and_sales(&self) -> Option<timeandsales::Config> {
        match self {
            Self::TimeAndSales(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn kline(&self) -> Option<kline::Config> {
        match self {
            Self::Kline(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn ladder(&self) -> Option<ladder::Config> {
        match self {
            Self::Ladder(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn microstructure(&self) -> Option<microstructure::Config> {
        match self {
            Self::Microstructure(cfg) => Some(*cfg),
            _ => None,
        }
    }

    pub fn comparison(&self) -> Option<comparison::Config> {
        match self {
            Self::Comparison(cfg) => Some(cfg.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Starter,
    BookmapChart,
    FootprintChart,
    CandlestickChart,
    ComparisonChart,
    TimeAndSales,
    Ladder,
    Microstructure,
}

impl ContentKind {
    pub const ALL: [ContentKind; 8] = [
        ContentKind::Starter,
        ContentKind::BookmapChart,
        ContentKind::FootprintChart,
        ContentKind::CandlestickChart,
        ContentKind::ComparisonChart,
        ContentKind::TimeAndSales,
        ContentKind::Ladder,
        ContentKind::Microstructure,
    ];
}

impl std::fmt::Display for ContentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ContentKind::Starter => "Starter Pane",
            ContentKind::BookmapChart => "Heatmap Chart",
            ContentKind::FootprintChart => "Footprint Chart",
            ContentKind::CandlestickChart => "Candlestick Chart",
            ContentKind::ComparisonChart => "Comparison Chart",
            ContentKind::TimeAndSales => "Time&Sales",
            ContentKind::Ladder => "DOM/Ladder",
            ContentKind::Microstructure => "Microstructure",
        };
        write!(f, "{s}")
    }
}

#[derive(Clone, Copy)]
pub struct PaneSetup {
    pub ticker_info: exchange::TickerInfo,
    pub basis: Option<Basis>,
    pub tick_multiplier: Option<TickMultiplier>,
    pub price_step: exchange::unit::PriceStep,
    pub depth_aggr: exchange::adapter::StreamTicksize,
    pub push_freq: exchange::PushFrequency,
}

impl PaneSetup {
    pub fn new(
        content_kind: ContentKind,
        base_ticker: TickerInfo,
        current_basis: Option<Basis>,
        current_tick_multiplier: Option<TickMultiplier>,
    ) -> Self {
        let exchange = base_ticker.ticker.exchange;

        let basis =
            match content_kind {
                ContentKind::BookmapChart => {
                    let current = current_basis.and_then(|b| match b {
                        Basis::Time(tf) if exchange.supports_heatmap_timeframe(tf) => Some(b),
                        _ => None,
                    });

                    Some(current.unwrap_or_else(|| Basis::default_heatmap_time(Some(base_ticker))))
                }
                ContentKind::Ladder => Some(
                    current_basis.unwrap_or_else(|| Basis::default_heatmap_time(Some(base_ticker))),
                ),
                ContentKind::FootprintChart => {
                    let current = current_basis.and_then(|b| match b {
                        Basis::Time(tf) if exchange.supports_kline_timeframe(tf) => Some(b),
                        Basis::Tick(_) => Some(b),
                        _ => None,
                    });

                    Some(current.unwrap_or_else(|| {
                        Basis::default_kline_time(Some(base_ticker), Timeframe::M5)
                    }))
                }
                ContentKind::CandlestickChart | ContentKind::ComparisonChart => {
                    let current = current_basis.and_then(|b| match b {
                        Basis::Time(tf) if exchange.supports_kline_timeframe(tf) => Some(b),
                        _ => None,
                    });

                    Some(current.unwrap_or_else(|| {
                        Basis::default_kline_time(Some(base_ticker), Timeframe::M5)
                    }))
                }
                ContentKind::Starter | ContentKind::TimeAndSales | ContentKind::Microstructure => {
                    None
                }
            };

        let tick_multiplier = match content_kind {
            // CME depth is always a shallow, complete book (~10 real levels/side) —
            // no aggregation is needed or offered, so the ladder and bookmap heatmap
            // always render at the ticker's raw tick, regardless of any multiplier
            // saved from before (e.g. from an older layout, or the crypto-era default).
            ContentKind::Ladder | ContentKind::BookmapChart => Some(TickMultiplier(1)),
            ContentKind::FootprintChart => {
                Some(current_tick_multiplier.unwrap_or(TickMultiplier(1)))
            }
            ContentKind::CandlestickChart
            | ContentKind::ComparisonChart
            | ContentKind::TimeAndSales
            | ContentKind::Microstructure
            | ContentKind::Starter => current_tick_multiplier,
        };

        let price_step = match tick_multiplier {
            Some(tm) => tm.multiply_with_min_tick_step(base_ticker),
            None => base_ticker.min_ticksize,
        };

        let depth_aggr = exchange.stream_ticksize(tick_multiplier, TickMultiplier(50));

        let push_freq = match content_kind {
            ContentKind::BookmapChart if exchange.is_custom_push_freq() => match basis {
                Some(Basis::Time(tf)) if exchange.supports_heatmap_timeframe(tf) => {
                    exchange::PushFrequency::Custom(tf)
                }
                _ => exchange::PushFrequency::ServerDefault,
            },
            _ => exchange::PushFrequency::ServerDefault,
        };

        Self {
            ticker_info: base_ticker,
            basis,
            tick_multiplier,
            price_step,
            depth_aggr,
            push_freq,
        }
    }
}
