use crate::{aggr::time::DataPoint, chart::indicator::KlineIndicator};
use exchange::{
    Kline, Trade, UnixMs,
    unit::price::{Price, PriceStep},
    unit::qty::Qty,
};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone)]
pub struct KlineDataPoint {
    pub kline: Kline,
    pub footprint: KlineTrades,
}

impl KlineDataPoint {
    pub fn max_cluster_qty(&self, cluster_kind: ClusterKind, highest: Price, lowest: Price) -> Qty {
        self.footprint
            .max_cluster_qty(cluster_kind, highest, lowest)
    }

    pub fn add_trade(&mut self, trade: &Trade, step: PriceStep) {
        self.footprint.add_trade_to_nearest_bin(trade, step);
    }

    pub fn poc_price(&self) -> Option<Price> {
        self.footprint.poc_price()
    }

    pub fn set_poc_status(&mut self, status: NPoc) {
        self.footprint.set_poc_status(status);
    }

    pub fn clear_trades(&mut self) {
        self.footprint.clear();
    }

    pub fn calculate_poc(&mut self) {
        self.footprint.calculate_poc();
    }

    pub fn last_trade_time(&self) -> Option<UnixMs> {
        self.footprint.last_trade_t()
    }

    pub fn first_trade_time(&self) -> Option<UnixMs> {
        self.footprint.first_trade_t()
    }

    pub fn volume_delta(&self) -> Qty {
        if self.kline.volume.is_directional() {
            self.kline.volume.delta()
        } else if !self.footprint.trades.is_empty() {
            self.footprint
                .trades
                .values()
                .fold(Qty::ZERO, |acc, group| acc + group.delta_qty())
        } else {
            Qty::ZERO
        }
    }

    /// Whether this datapoint has directional (buy vs sell) data.
    pub fn is_directional(&self) -> bool {
        !self.footprint.trades.is_empty() || self.kline.volume.is_directional()
    }
}

impl DataPoint for KlineDataPoint {
    fn add_trade(&mut self, trade: &Trade, step: PriceStep) {
        self.add_trade(trade, step);
    }

    fn clear_trades(&mut self) {
        self.clear_trades();
    }

    fn last_trade_time(&self) -> Option<UnixMs> {
        self.last_trade_time()
    }

    fn first_trade_time(&self) -> Option<UnixMs> {
        self.first_trade_time()
    }

    fn last_price(&self) -> Price {
        self.kline.close
    }

    fn kline(&self) -> Option<&Kline> {
        Some(&self.kline)
    }

    fn value_high(&self) -> Price {
        self.kline.high
    }

    fn value_low(&self) -> Price {
        self.kline.low
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupedTrades {
    pub buy_qty: Qty,
    pub sell_qty: Qty,
    pub first_time: UnixMs,
    pub last_time: UnixMs,
    pub buy_count: usize,
    pub sell_count: usize,
}

impl GroupedTrades {
    fn new(trade: &Trade) -> Self {
        Self {
            buy_qty: if trade.is_sell {
                Qty::default()
            } else {
                trade.qty
            },
            sell_qty: if trade.is_sell {
                trade.qty
            } else {
                Qty::default()
            },
            first_time: trade.time,
            last_time: trade.time,
            buy_count: if trade.is_sell { 0 } else { 1 },
            sell_count: if trade.is_sell { 1 } else { 0 },
        }
    }

    fn add_trade(&mut self, trade: &Trade) {
        if trade.is_sell {
            self.sell_qty += trade.qty;
            self.sell_count += 1;
        } else {
            self.buy_qty += trade.qty;
            self.buy_count += 1;
        }
        self.last_time = trade.time;
    }

    pub fn total_qty(&self) -> Qty {
        self.buy_qty + self.sell_qty
    }

    pub fn delta_qty(&self) -> Qty {
        self.buy_qty - self.sell_qty
    }

    pub fn max_cluster_qty(&self, cluster_kind: ClusterKind) -> Qty {
        match cluster_kind {
            ClusterKind::BidAsk | ClusterKind::Table => self.buy_qty.max(self.sell_qty),
            ClusterKind::DeltaProfile => self.buy_qty.abs_diff(self.sell_qty),
            ClusterKind::VolumeProfile => self.total_qty(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct KlineTrades {
    pub trades: FxHashMap<Price, GroupedTrades>,
    pub poc: Option<PointOfControl>,
}

impl KlineTrades {
    pub fn new() -> Self {
        Self {
            trades: FxHashMap::default(),
            poc: None,
        }
    }

    pub fn first_trade_t(&self) -> Option<UnixMs> {
        self.trades.values().map(|group| group.first_time).min()
    }

    pub fn last_trade_t(&self) -> Option<UnixMs> {
        self.trades.values().map(|group| group.last_time).max()
    }

    /// Add trade to the bin at the step multiple computed with side-based rounding.
    /// Intended for order-book ladder/quotes; Floor for sells, ceil for buys.
    /// Introduces side bias at bin edges and should not be used for OHLC/footprint aggregation
    pub fn add_trade_to_side_bin(&mut self, trade: &Trade, step: PriceStep) {
        let price = trade.price.round_to_side_step(trade.is_sell, step);

        self.trades
            .entry(price)
            .and_modify(|group| group.add_trade(trade))
            .or_insert_with(|| GroupedTrades::new(trade));
    }

    /// Add trade to the bin at the nearest step multiple (side-agnostic).
    /// Ties (exactly half a step) round up to the higher multiple.
    /// Intended for footprint/OHLC trade aggregation
    pub fn add_trade_to_nearest_bin(&mut self, trade: &Trade, step: PriceStep) {
        let price = trade.price.round_to_step(step);

        self.trades
            .entry(price)
            .and_modify(|group| group.add_trade(trade))
            .or_insert_with(|| GroupedTrades::new(trade));
    }

    pub fn max_qty_by<F>(&self, highest: Price, lowest: Price, f: F) -> Qty
    where
        F: Fn(&GroupedTrades) -> Qty,
    {
        let mut max_qty = Qty::default();
        for (price, group) in &self.trades {
            if *price >= lowest && *price <= highest {
                max_qty = max_qty.max(f(group));
            }
        }
        max_qty
    }

    pub fn max_cluster_qty(&self, cluster_kind: ClusterKind, highest: Price, lowest: Price) -> Qty {
        self.max_qty_by(highest, lowest, |group| group.max_cluster_qty(cluster_kind))
    }

    pub fn calculate_poc(&mut self) {
        if self.trades.is_empty() {
            return;
        }

        let mut max_volume = Qty::ZERO;
        let mut poc_price = Price::from_f32(0.0);

        for (price, group) in &self.trades {
            let total_volume = group.total_qty();
            if total_volume > max_volume {
                max_volume = total_volume;
                poc_price = *price;
            }
        }

        self.poc = Some(PointOfControl {
            price: poc_price,
            volume: max_volume,
            status: NPoc::default(),
        });
    }

    pub fn set_poc_status(&mut self, status: NPoc) {
        if let Some(poc) = &mut self.poc {
            poc.status = status;
        }
    }

    pub fn poc_price(&self) -> Option<Price> {
        self.poc.map(|poc| poc.price)
    }

    pub fn clear(&mut self) {
        self.trades.clear();
        self.poc = None;
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FootprintSummary {
    pub buy: Qty,
    pub sell: Qty,
    pub total: Qty,
    pub delta: Qty,
    pub delta_pct: f64,
}

impl FootprintSummary {
    pub fn new(buy: Qty, sell: Qty) -> Self {
        let total = buy + sell;
        let delta = buy - sell;
        let total_f = total.to_f64();
        let delta_pct = if total_f > 0.0 {
            (delta.to_f64() / total_f) * 100.0
        } else {
            0.0
        };

        Self {
            buy,
            sell,
            total,
            delta,
            delta_pct,
        }
    }

    pub fn from_trades(footprint: &KlineTrades) -> Option<Self> {
        if footprint.trades.is_empty() {
            return None;
        }

        let (buy, sell) = footprint
            .trades
            .values()
            .fold((Qty::ZERO, Qty::ZERO), |(buy, sell), group| {
                (buy + group.buy_qty, sell + group.sell_qty)
            });

        Some(Self::new(buy, sell))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum KlineChartKind {
    #[default]
    Candles,
    Footprint {
        clusters: ClusterKind,
        #[serde(default)]
        scaling: ClusterScaling,
        studies: Vec<FootprintStudy>,
    },
}

impl KlineChartKind {
    pub fn allows_indicator(&self, indicator: KlineIndicator) -> bool {
        match (self, indicator) {
            // Bar analysis is a candle-chart study.
            (KlineChartKind::Candles, KlineIndicator::BarAnalysis) => false,
            // Premarket H/L is a candle-chart overlay only: footprint clusters
            // aggregate the session differently and must not render it.
            (KlineChartKind::Footprint { .. }, KlineIndicator::PremarketHighLow) => false,
            (KlineChartKind::Footprint { .. }, KlineIndicator::TheoricTargets) => false,
            (KlineChartKind::Footprint { .. }, KlineIndicator::Magnets) => false,
            // Masterpice + Masterpice Profile + V2: overlay de candles apenas.
            (KlineChartKind::Footprint { .. }, KlineIndicator::Masterpice) => false,
            (KlineChartKind::Footprint { .. }, KlineIndicator::MasterpiceProfile) => false,
            (KlineChartKind::Footprint { .. }, KlineIndicator::MasterpiceV2) => false,
            _ => true,
        }
    }

    pub fn min_scaling(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 0.4,
            KlineChartKind::Candles => 0.6,
        }
    }

    pub fn max_scaling(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 1.2,
            KlineChartKind::Candles => 2.5,
        }
    }

    pub fn max_cell_width(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 360.0,
            KlineChartKind::Candles => 16.0,
        }
    }

    pub fn min_cell_width(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 80.0,
            KlineChartKind::Candles => 1.0,
        }
    }

    pub fn max_cell_height(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 90.0,
            KlineChartKind::Candles => 8.0,
        }
    }

    pub fn min_cell_height(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 1.0,
            KlineChartKind::Candles => 0.001,
        }
    }

    pub fn default_cell_width(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 80.0,
            KlineChartKind::Candles => 4.0,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum ClusterKind {
    BidAsk,
    VolumeProfile,
    DeltaProfile,
    #[default]
    Table,
}

impl ClusterKind {
    pub const ALL: [ClusterKind; 4] = [
        ClusterKind::BidAsk,
        ClusterKind::VolumeProfile,
        ClusterKind::DeltaProfile,
        ClusterKind::Table,
    ];
}

impl std::fmt::Display for ClusterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClusterKind::BidAsk => write!(f, "Bid/Ask"),
            ClusterKind::VolumeProfile => write!(f, "Volume Profile"),
            ClusterKind::DeltaProfile => write!(f, "Delta Profile"),
            ClusterKind::Table => write!(f, "Table"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    // Whether to show last value labels on top right/left when not hovering
    // e.g. OHLC/bar change values for the main chart, or last value of an indicator series
    pub data_labels_always_visible: bool,
    // Whether to show the footprint per-bar summary below each candle.
    pub show_footprint_summary: bool,
    // Whether to show a small candle next to footprint table clusters.
    pub show_footprint_table_candle: bool,
    // Volume profile overlay on the candlestick chart (off by default).
    // Aggregates buy/sell ticks per price level with candle colors.
    pub show_volume_profile: bool,
    // History window in hours (0..=24, default 1) for the overlay bootstrap.
    // After bootstrap, live ticks keep it updated in realtime.
    pub volume_profile_history_hours: u8,
    // Which chart edge the overlay anchors to (default right).
    pub volume_profile_side: VolumeProfileSide,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_labels_always_visible: false,
            show_footprint_summary: true,
            show_footprint_table_candle: true,
            show_volume_profile: false,
            volume_profile_history_hours: 1,
            volume_profile_side: VolumeProfileSide::default(),
        }
    }
}

/// Anchor edge for the candlestick volume-profile overlay.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum VolumeProfileSide {
    #[default]
    Right,
    Left,
}

impl VolumeProfileSide {
    pub const ALL: [VolumeProfileSide; 2] = [VolumeProfileSide::Right, VolumeProfileSide::Left];
}

impl std::fmt::Display for VolumeProfileSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VolumeProfileSide::Right => write!(f, "Right"),
            VolumeProfileSide::Left => write!(f, "Left"),
        }
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub enum ClusterScaling {
    #[default]
    /// Scale based on the maximum quantity in the visible range.
    VisibleRange,
    /// Blend global VisibleRange and per-cluster Individual using a weight in [0.0, 1.0].
    /// weight = fraction of global contribution (1.0 == all-global, 0.0 == all-individual).
    Hybrid { weight: f32 },
    /// Scale based only on the maximum quantity inside the datapoint (per-candle).
    Datapoint,
}

impl ClusterScaling {
    pub const ALL: [ClusterScaling; 3] = [
        ClusterScaling::VisibleRange,
        ClusterScaling::Hybrid { weight: 0.2 },
        ClusterScaling::Datapoint,
    ];
}

impl std::fmt::Display for ClusterScaling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClusterScaling::VisibleRange => write!(f, "Visible Range"),
            ClusterScaling::Hybrid { weight } => write!(f, "Hybrid (weight: {:.2})", weight),
            ClusterScaling::Datapoint => write!(f, "Per-candle"),
        }
    }
}

impl std::cmp::Eq for ClusterScaling {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum FootprintStudy {
    NPoC {
        lookback: usize,
    },
    Imbalance {
        threshold: usize,
        color_scale: Option<usize>,
        ignore_zeros: bool,
    },
}

impl FootprintStudy {
    pub fn is_same_type(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (FootprintStudy::NPoC { .. }, FootprintStudy::NPoC { .. })
                | (
                    FootprintStudy::Imbalance { .. },
                    FootprintStudy::Imbalance { .. }
                )
        )
    }
}

impl FootprintStudy {
    pub const ALL: [FootprintStudy; 2] = [
        FootprintStudy::NPoC { lookback: 80 },
        FootprintStudy::Imbalance {
            threshold: 200,
            color_scale: Some(400),
            ignore_zeros: true,
        },
    ];
}

impl std::fmt::Display for FootprintStudy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FootprintStudy::NPoC { .. } => write!(f, "Naked Point of Control"),
            FootprintStudy::Imbalance { .. } => write!(f, "Imbalance"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PointOfControl {
    pub price: Price,
    pub volume: Qty,
    pub status: NPoc,
}

impl Default for PointOfControl {
    fn default() -> Self {
        Self {
            price: Price::from_f32(0.0),
            volume: Qty::ZERO,
            status: NPoc::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum NPoc {
    #[default]
    None,
    Naked,
    Filled {
        at: u64,
    },
}

impl NPoc {
    pub fn filled(&mut self, at: u64) {
        *self = NPoc::Filled { at };
    }

    pub fn unfilled(&mut self) {
        *self = NPoc::Naked;
    }
}

/// Trailing time-window accumulator backing the candlestick volume-profile
/// overlay: buy/sell ticks grouped by price level over the last N hours.
///
/// Unlike the footprint buckets (per-kline-interval, OHLC-tied), this is
/// OHLC-independent — every tick in `[feed_now - window, feed_now]` counts,
/// however the candles are aggregated. History is bootstrapped once via a
/// `FetchRange::Trades` fetch (full window on enable, backwards delta when the
/// window grows); live ticks then keep accumulating in realtime while `prune`
/// slides the window forward.
#[derive(Debug, Clone, Default)]
pub struct CandleVolumeProfile {
    ticks: VecDeque<ProfileTick>,
    per_price: FxHashMap<Price, (Qty, Qty)>,
    feed_now: Option<UnixMs>,
    fetched_from: Option<UnixMs>,
}

#[derive(Debug, Clone, Copy)]
struct ProfileTick {
    time: UnixMs,
    /// Raw trade price; binned with the chart tick-size on insert/prune/rebuild.
    price: Price,
    qty: Qty,
    is_sell: bool,
}

impl CandleVolumeProfile {
    pub fn is_empty(&self) -> bool {
        self.per_price.is_empty()
    }

    pub fn fetched_from(&self) -> Option<UnixMs> {
        self.fetched_from
    }

    pub fn feed_now(&self) -> Option<UnixMs> {
        self.feed_now
    }

    pub fn insert(&mut self, trades: &[Trade], step: PriceStep) {
        for trade in trades {
            if trade.qty <= Qty::ZERO {
                continue;
            }
            let bin = trade.price.round_to_step(step);
            self.feed_now = Some(
                self.feed_now
                    .map_or(trade.time, |seen| seen.max(trade.time)),
            );
            let entry = self.per_price.entry(bin).or_insert((Qty::ZERO, Qty::ZERO));
            if trade.is_sell {
                entry.1 += trade.qty;
            } else {
                entry.0 += trade.qty;
            }
            self.ticks.push_back(ProfileTick {
                time: trade.time,
                price: trade.price,
                qty: trade.qty,
                is_sell: trade.is_sell,
            });
        }
    }

    /// Record that history `[from, _]` has been fetched (merge, keep oldest).
    pub fn mark_fetched(&mut self, from: UnixMs) {
        self.fetched_from = Some(self.fetched_from.map_or(from, |seen| seen.min(from)));
    }

    /// Insert a backfilled batch without double-counting on redelivery: only
    /// ticks older than everything already fetched are new (batches page
    /// backwards); the rest is already in the accumulator.
    pub fn insert_batch(&mut self, trades: &[Trade], step: PriceStep) {
        match self.fetched_from {
            None => self.insert(trades, step),
            Some(fetched_from) => {
                let fresh: Vec<Trade> = trades
                    .iter()
                    .copied()
                    .filter(|t| t.time < fetched_from)
                    .collect();
                self.insert(&fresh, step);
            }
        }
        if let Some(oldest) = trades.iter().map(|t| t.time).min() {
            self.mark_fetched(oldest);
        }
    }

    fn sub_tick(&mut self, tick: &ProfileTick, step: PriceStep) {
        let bin = tick.price.round_to_step(step);
        if let Some((buy, sell)) = self.per_price.get_mut(&bin) {
            if tick.is_sell {
                *sell = if sell.units >= tick.qty.units {
                    *sell - tick.qty
                } else {
                    Qty::ZERO
                };
            } else {
                *buy = if buy.units >= tick.qty.units {
                    *buy - tick.qty
                } else {
                    Qty::ZERO
                };
            }
            if *buy <= Qty::ZERO && *sell <= Qty::ZERO {
                self.per_price.remove(&bin);
            }
        }
    }

    /// Drop everything older than `feed_now - window_hours`, subtracting from
    /// the per-price totals. A smaller window never triggers a fetch — it just
    /// prunes locally.
    pub fn prune(&mut self, step: PriceStep, window_hours: u8) {
        let Some(now) = self.feed_now else {
            return;
        };
        let window_ms = u64::from(window_hours).saturating_mul(3_600_000);
        let cutoff = now.as_u64().saturating_sub(window_ms);
        while let Some(front) = self.ticks.front() {
            if front.time.as_u64() >= cutoff {
                break;
            }
            let old = self.ticks.pop_front().expect("checked above");
            self.sub_tick(&old, step);
        }
    }

    /// Re-bin every queued tick (tick-size change), then re-apply the window.
    pub fn rebuild(&mut self, step: PriceStep, window_hours: u8) {
        let ticks: Vec<ProfileTick> = self.ticks.iter().copied().collect();
        self.ticks.clear();
        self.per_price.clear();
        self.feed_now = None;
        for tick in ticks {
            let bin = tick.price.round_to_step(step);
            self.feed_now = Some(self.feed_now.map_or(tick.time, |seen| seen.max(tick.time)));
            let entry = self.per_price.entry(bin).or_insert((Qty::ZERO, Qty::ZERO));
            if tick.is_sell {
                entry.1 += tick.qty;
            } else {
                entry.0 += tick.qty;
            }
            self.ticks.push_back(tick);
        }
        self.prune(step, window_hours);
    }

    pub fn levels(&self) -> impl Iterator<Item = (&Price, &(Qty, Qty))> {
        self.per_price.iter()
    }

    pub fn max_total(&self) -> f64 {
        self.per_price
            .values()
            .map(|(b, s)| (*b + *s).to_f64())
            .fold(0.0f64, f64::max)
    }

    /// Point of Control over the whole window: price level with the largest
    /// total (buy + sell) volume. Unlike footprint's per-candle PoC, this is
    /// singular for the entire profile.
    pub fn poc_price(&self) -> Option<Price> {
        self.per_price
            .iter()
            .max_by(|(_, (bb, sb)), (_, (bo, so))| (*bb + *sb).units.cmp(&(*bo + *so).units))
            .map(|(price, _)| *price)
    }

    pub fn clear(&mut self) {
        self.ticks.clear();
        self.per_price.clear();
        self.feed_now = None;
        self.fetched_from = None;
    }
}

#[cfg(test)]
mod candle_profile_tests {
    use super::*;
    use exchange::unit::price::PriceStep;

    const STEP: PriceStep = PriceStep { units: 5 };
    const HOUR: u64 = 3_600_000;

    fn trade(ms: u64, price: f64, qty: f64, is_sell: bool) -> Trade {
        Trade {
            time: UnixMs::new(ms),
            is_sell,
            price: Price::from_f64(price),
            qty: Qty::from_f64(qty),
        }
    }

    #[test]
    fn groups_buys_and_sells_per_price_level() {
        let mut p = CandleVolumeProfile::default();
        p.insert(
            &[
                trade(1_000, 100.0, 3.0, false),
                trade(2_000, 100.0, 2.0, true),
                trade(3_000, 101.0, 7.0, false),
            ],
            STEP,
        );
        assert!(!p.is_empty());
        let bin = Price::from_f64(100.0).round_to_step(STEP);
        let (buy, sell) = p.per_price[&bin];
        assert_eq!((buy, sell), (Qty::from_f64(3.0), Qty::from_f64(2.0)));
        assert!((p.max_total() - 7.0).abs() < 1e-9);
    }

    #[test]
    fn prune_slides_the_window_forward() {
        let mut p = CandleVolumeProfile::default();
        p.insert(&[trade(0, 100.0, 5.0, false)], STEP);
        // feed_now = 2h later via a newer tick; 1h window must evict the old one.
        p.insert(&[trade(2 * HOUR, 100.0, 1.0, false)], STEP);
        p.prune(STEP, 1);
        let bin = Price::from_f64(100.0).round_to_step(STEP);
        let (buy, sell) = p.per_price[&bin];
        assert_eq!((buy, sell), (Qty::from_f64(1.0), Qty::ZERO));
    }

    #[test]
    fn insert_batch_ignores_redelivered_ranges() {
        let mut p = CandleVolumeProfile::default();
        let batch = [
            trade(1_000, 100.0, 5.0, false),
            trade(2_000, 100.0, 5.0, true),
        ];
        p.insert_batch(&batch, STEP);
        p.insert_batch(&batch, STEP);
        let bin = Price::from_f64(100.0).round_to_step(STEP);
        let (buy, sell) = p.per_price[&bin];
        assert_eq!((buy, sell), (Qty::from_f64(5.0), Qty::from_f64(5.0)));
        assert_eq!(p.fetched_from(), Some(UnixMs::new(1_000)));
    }

    #[test]
    fn poc_is_the_level_with_largest_total_volume() {
        let mut p = CandleVolumeProfile::default();
        p.insert(
            &[
                trade(1_000, 100.0, 5.0, false),
                trade(2_000, 101.0, 4.0, false),
                trade(3_000, 101.0, 4.0, true),
                trade(4_000, 102.0, 2.0, true),
            ],
            STEP,
        );
        assert_eq!(
            p.poc_price(),
            Some(Price::from_f64(101.0).round_to_step(STEP))
        );
        assert_eq!(CandleVolumeProfile::default().poc_price(), None);
    }

    #[test]
    fn backward_delta_batch_only_adds_older_ticks() {
        let mut p = CandleVolumeProfile::default();
        p.insert_batch(&[trade(2_000, 100.0, 5.0, false)], STEP);
        p.insert_batch(
            &[
                trade(1_000, 100.0, 3.0, true),
                trade(2_000, 100.0, 9.0, true),
            ],
            STEP,
        );
        let bin = Price::from_f64(100.0).round_to_step(STEP);
        let (buy, sell) = p.per_price[&bin];
        assert_eq!((buy, sell), (Qty::from_f64(5.0), Qty::from_f64(3.0)));
    }
}
