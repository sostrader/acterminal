use crate::chart::{Basis, Message, ViewState};

use data::chart::indicator::KlineIndicator;
use data::chart::kline::KlineDataPoint;
use data::chart::{BasisSeries, PlotData};
use exchange::adapter::Exchange;
use exchange::{Kline, Timeframe, Trade};

use super::plot::AnySeries;

pub mod bar_analysis;
pub mod cumulative_delta;
pub mod volume;

/// UI adapter methods for converting domain `BasisSeries` into plot-ready series.
trait BasisSeriesExt<T> {
    fn as_plot_series(&self) -> AnySeries<'_, T>;
}

impl<T> BasisSeriesExt<T> for BasisSeries<T> {
    fn as_plot_series(&self) -> AnySeries<'_, T> {
        match self {
            BasisSeries::Time(data) => AnySeries::forward_unix_ms(data),
            BasisSeries::Tick(data) => AnySeries::reversed_u64(data),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq)]
pub enum IndicatorAvailability {
    /// Indicator can be rendered normally.
    #[default]
    Available,
    /// Availability cannot be determined yet (e.g. no datapoints loaded).
    Unknown,
    /// Indicator cannot be rendered for the current source/context.
    Unavailable(AvailabilityCause),
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum AvailabilityCause {
    Exchange(Exchange),
    Timeframe(Timeframe),
    Basis(Basis),
    TradeData,
}

impl IndicatorAvailability {
    pub fn unavailable_message(&self, indicator: &str) -> Option<String> {
        match self {
            IndicatorAvailability::Available | IndicatorAvailability::Unknown => None,
            IndicatorAvailability::Unavailable(cause) => Some(match cause {
                AvailabilityCause::Exchange(exchange) => {
                    format!(
                        "{indicator} is not available for {}.",
                        exchange.market_type()
                    )
                }
                AvailabilityCause::Timeframe(timeframe) => {
                    format!("{indicator} is not available on {timeframe} timeframe.")
                }
                AvailabilityCause::Basis(Basis::Tick(_)) => {
                    format!("{indicator} is not available for tick charts.")
                }
                AvailabilityCause::Basis(basis) => {
                    format!("{indicator} is not available on {basis} basis.")
                }
                AvailabilityCause::TradeData => {
                    format!("{indicator} requires directional trade-volume data.")
                }
            }),
        }
    }
}

pub trait KlineIndicatorImpl {
    /// Clear all caches for a full redraw
    fn clear_all_caches(&mut self);

    /// Clear caches related to crosshair only
    /// e.g. tooltips and scale labels for a partial redraw
    fn clear_crosshair_caches(&mut self);

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        // Whether to show last value labels on top right/left when not hovering
        data_labels_always_visible: bool,
        visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message>;

    fn availability(&self, _chart: &ViewState) -> IndicatorAvailability {
        IndicatorAvailability::Available
    }

    fn unavailable_message(&self, chart: &ViewState, indicator: &str) -> Option<String> {
        self.availability(chart).unavailable_message(indicator)
    }

    /// Rebuild data using kline(OHLCV) source
    fn rebuild_from_source(&mut self, _source: &PlotData<KlineDataPoint>) {}

    fn on_insert_klines(&mut self, _klines: &[Kline], _source: &PlotData<KlineDataPoint>) {}

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        _old_dp_len: usize,
        _source: &PlotData<KlineDataPoint>,
    ) {
    }

    fn on_ticksize_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    /// Timeframe/tick interval has changed
    fn on_basis_change(&mut self, _source: &PlotData<KlineDataPoint>) {}
}

pub fn make_empty(which: KlineIndicator) -> Box<dyn KlineIndicatorImpl> {
    match which {
        KlineIndicator::Volume => Box::new(super::kline::volume::VolumeIndicator::new()),
        KlineIndicator::BarAnalysis => {
            Box::new(super::kline::bar_analysis::BarAnalysisIndicator::new())
        }
        KlineIndicator::CumulativeDelta => {
            Box::new(super::kline::cumulative_delta::CumulativeDeltaIndicator::new())
        }
        // Premarket H/L is a roof-level wall overlay owned by the KlineChart itself
        // (no sub-panel). `KlineChart::toggle_indicator` special-cases it and never
        // lands here; this arm only exists to keep the match exhaustive.
        // Masterpice / Masterpice Profile / V2 seguem o mesmo padrão (roof overlay).
        KlineIndicator::PremarketHighLow
        | KlineIndicator::TheoricTargets
        | KlineIndicator::Magnets
        | KlineIndicator::Masterpice
        | KlineIndicator::MasterpiceProfile
        | KlineIndicator::MasterpiceV2 => Box::new(PremarketPainter),
    }
}

/// No-op `KlineIndicatorImpl` for the premarket wall overlay, which paints into the
/// chart's main canvas rather than a sub-panel. Kept only for exhaustiveness.
#[derive(Debug, Default)]
struct PremarketPainter;

impl KlineIndicatorImpl for PremarketPainter {
    fn clear_all_caches(&mut self) {}

    fn clear_crosshair_caches(&mut self) {}

    fn element<'a>(
        &'a self,
        _chart: &'a ViewState,
        _data_labels_always_visible: bool,
        _visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        iced::widget::space().into()
    }
}
