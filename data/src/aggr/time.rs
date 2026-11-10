use std::collections::BTreeMap;

use crate::chart::Basis;
use crate::chart::heatmap::HeatmapDataPoint;
use crate::chart::kline::{ClusterKind, KlineDataPoint, KlineTrades, NPoc};

use exchange::unit::{Price, PriceStep, Qty};
use exchange::{Kline, Timeframe, Trade, UnixMs, Volume};

pub trait DataPoint {
    fn add_trade(&mut self, trade: &Trade, step: PriceStep);

    fn clear_trades(&mut self);

    fn last_trade_time(&self) -> Option<UnixMs>;

    fn first_trade_time(&self) -> Option<UnixMs>;

    fn last_price(&self) -> Price;

    fn kline(&self) -> Option<&Kline>;

    fn value_high(&self) -> Price;

    fn value_low(&self) -> Price;
}

pub struct TimeSeries<D: DataPoint> {
    pub datapoints: BTreeMap<UnixMs, D>,
    pub interval: Timeframe,
    pub tick_size: PriceStep,
}

impl<D: DataPoint> TimeSeries<D> {
    pub fn base_price(&self) -> Price {
        self.datapoints
            .values()
            .last()
            .map_or(Price::from_f32(0.0), DataPoint::last_price)
    }

    pub fn latest_timestamp(&self) -> Option<UnixMs> {
        self.datapoints.keys().last().copied()
    }

    pub fn latest_kline(&self) -> Option<&Kline> {
        self.datapoints.values().last().and_then(|dp| dp.kline())
    }

    pub fn price_scale(&self, lookback: usize) -> (Price, Price) {
        let mut iter = self.datapoints.iter().rev().take(lookback);

        if let Some((_, first)) = iter.next() {
            let mut high = first.value_high();
            let mut low = first.value_low();

            for (_, dp) in iter {
                let value_high = dp.value_high();
                let value_low = dp.value_low();
                if value_high > high {
                    high = value_high;
                }
                if value_low < low {
                    low = value_low;
                }
            }

            (high, low)
        } else {
            (Price::from_f32(0.0), Price::from_f32(0.0))
        }
    }

    pub fn volume_data<'a>(&'a self) -> BTreeMap<UnixMs, exchange::Volume>
    where
        BTreeMap<UnixMs, exchange::Volume>: From<&'a TimeSeries<D>>,
    {
        self.into()
    }

    pub fn timerange(&self) -> (UnixMs, UnixMs) {
        let earliest = self
            .datapoints
            .keys()
            .next()
            .copied()
            .unwrap_or(UnixMs::ZERO);
        let latest = self
            .datapoints
            .keys()
            .last()
            .copied()
            .unwrap_or(UnixMs::ZERO);

        (earliest, latest)
    }

    pub fn min_max_price_in_range_prices(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
    ) -> Option<(Price, Price)> {
        let mut it = self.datapoints.range(earliest..=latest);

        let (_, first) = it.next()?;
        let mut min_price = first.value_low();
        let mut max_price = first.value_high();

        for (_, dp) in it {
            let low = dp.value_low();
            let high = dp.value_high();
            if low < min_price {
                min_price = low;
            }
            if high > max_price {
                max_price = high;
            }
        }

        Some((min_price, max_price))
    }

    pub fn min_max_price_in_range(&self, earliest: UnixMs, latest: UnixMs) -> Option<(f32, f32)> {
        self.min_max_price_in_range_prices(earliest, latest)
            .map(|(min_p, max_p)| (min_p.to_f32_lossy(), max_p.to_f32_lossy()))
    }

    /// Ensures a datapoint bucket exists at `rounded_t` and ingests all trades into it.
    pub fn ingest_trades_bucket(&mut self, rounded_t: UnixMs, trades: &[Trade], step: PriceStep)
    where
        D: Default,
    {
        let bucket = self.datapoints.entry(rounded_t).or_default();

        for trade in trades {
            bucket.add_trade(trade, step);
        }
    }

    pub fn clear_trades(&mut self) {
        for data_point in self.datapoints.values_mut() {
            data_point.clear_trades();
        }
    }
}

impl TimeSeries<KlineDataPoint> {
    pub fn new(interval: Timeframe, tick_size: PriceStep, klines: &[Kline]) -> Self {
        let mut timeseries = Self {
            datapoints: BTreeMap::new(),
            interval,
            tick_size,
        };

        timeseries.insert_klines(klines);
        timeseries
    }

    pub fn with_trades(&self, trades: &[Trade]) -> TimeSeries<KlineDataPoint> {
        let mut new_series = Self {
            datapoints: self.datapoints.clone(),
            interval: self.interval,
            tick_size: self.tick_size,
        };

        new_series.insert_trades_or_create_bucket(trades);
        new_series
    }

    pub fn insert_klines(&mut self, klines: &[Kline]) {
        for kline in klines {
            let entry = self
                .datapoints
                .entry(kline.time)
                .or_insert_with(|| KlineDataPoint {
                    kline: *kline,
                    footprint: KlineTrades::new(),
                });

            entry.kline = *kline;

            self.fill_quiet_buckets_before(kline.time);
        }

        self.update_poc_status();
    }

    /// Carries the previous close across buckets in which nothing traded.
    ///
    /// A bucket with no prints produces no kline at all, and the chart plots datapoints
    /// by timestamp on a linear time axis — so a quiet stretch renders as a hole in the
    /// series rather than as a flat stretch of price. Thin products hit this constantly
    /// (CME FX futures routinely go 10-15 minutes without a print mid-session), which
    /// reads as missing history even though nothing was lost.
    ///
    /// The synthesized candles are flat at the previous close with zero volume and an
    /// empty footprint — which is exactly what happened in that bucket.
    ///
    /// A *closure* is the one gap not carried across: nothing happened over the weekend
    /// because the market was shut, and filling it would claim two days of flat trading
    /// that never took place — as well as hiding the closure from
    /// [`crate::chart::TimeAxis`], which is what squeezes it out of the axis.
    fn fill_quiet_buckets_before(&mut self, time: UnixMs) {
        /// Guards against a pathological first insert after a long outage turning into
        /// millions of flat candles. Ten days of 1m buckets is already past anything
        /// `persist` retains.
        const MAX_FILLED_PER_GAP: usize = 16_384;

        let step = self.interval.to_milliseconds();
        if step == 0 {
            return;
        }

        let Some((&prev_time, prev)) = self.datapoints.range(..time).next_back() else {
            return;
        };

        let empty_span = time
            .as_u64()
            .saturating_sub(prev_time.as_u64())
            .saturating_sub(step);
        if crate::chart::axis::is_closure(empty_span, step) {
            return;
        }

        let close = prev.kline.close;
        let mut at = prev_time.saturating_add(step);
        let mut filled = 0usize;

        while at < time && filled < MAX_FILLED_PER_GAP {
            self.datapoints.insert(
                at,
                KlineDataPoint {
                    kline: Kline {
                        time: at,
                        open: close,
                        high: close,
                        low: close,
                        close,
                        volume: Volume::BuySell(Qty::ZERO, Qty::ZERO),
                    },
                    footprint: KlineTrades::new(),
                },
            );

            at = at.saturating_add(step);
            filled += 1;
        }
    }

    pub fn insert_trades_or_create_bucket(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        let mut updated_times = Vec::new();

        buffer.iter().for_each(|trade| {
            let rounded_time = trade.time.floor_to(self.interval);

            if !updated_times.contains(&rounded_time) {
                updated_times.push(rounded_time);
            }

            let entry = self
                .datapoints
                .entry(rounded_time)
                .or_insert_with(|| KlineDataPoint {
                    kline: Kline {
                        time: rounded_time,
                        open: trade.price,
                        high: trade.price,
                        low: trade.price,
                        close: trade.price,
                        volume: Volume::empty_buy_sell(),
                    },
                    footprint: KlineTrades::new(),
                });

            entry.add_trade(trade, self.tick_size);
        });

        for time in updated_times {
            if let Some(data_point) = self.datapoints.get_mut(&time) {
                data_point.calculate_poc();
            }
        }
    }

    pub fn insert_trades_existing_buckets(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        let mut updated_times: Vec<UnixMs> = Vec::new();

        for trade in buffer {
            let rounded_time = trade.time.floor_to(self.interval);

            if let Some(entry) = self.datapoints.get_mut(&rounded_time) {
                if !updated_times.contains(&rounded_time) {
                    updated_times.push(rounded_time);
                }
                entry.add_trade(trade, self.tick_size);
            }
        }

        for time in updated_times {
            if let Some(data_point) = self.datapoints.get_mut(&time) {
                data_point.calculate_poc();
            }
        }
    }

    pub fn change_tick_size(&mut self, tick_size: PriceStep, raw_trades: &[Trade]) {
        self.tick_size = tick_size;

        self.clear_trades();

        if !raw_trades.is_empty() {
            self.insert_trades_existing_buckets(raw_trades);
        }
    }

    pub fn update_poc_status(&mut self) {
        let updates = self
            .datapoints
            .iter()
            .filter_map(|(&time, dp)| dp.poc_price().map(|price| (time, price)))
            .collect::<Vec<_>>();

        for (current_time, poc_price) in updates {
            let mut npoc = NPoc::default();

            for (&next_time, next_dp) in self.datapoints.range(current_time.saturating_add(1)..) {
                let next_dp_low = next_dp.kline.low.round_to_side_step(true, self.tick_size);
                let next_dp_high = next_dp.kline.high.round_to_side_step(false, self.tick_size);

                if next_dp_low <= poc_price && next_dp_high >= poc_price {
                    npoc.filled(next_time.as_u64());
                    break;
                } else {
                    npoc.unfilled();
                }
            }

            if let Some(data_point) = self.datapoints.get_mut(&current_time) {
                data_point.set_poc_status(npoc);
            }
        }
    }

    pub fn min_max_footprint_price_in_range(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
    ) -> Option<(Price, Price)> {
        if latest < earliest {
            return None;
        }

        let mut min_price: Option<Price> = None;
        let mut max_price: Option<Price> = None;

        let mut track_price = |price: Price| {
            min_price = Some(match min_price {
                Some(current) => current.min(price),
                None => price,
            });
            max_price = Some(match max_price {
                Some(current) => current.max(price),
                None => price,
            });
        };

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                track_price(dp.kline.low);
                track_price(dp.kline.high);

                for price in dp.footprint.trades.keys() {
                    track_price(*price);
                }
            });

        match (min_price, max_price) {
            (Some(low), Some(high)) => Some((low, high)),
            _ => None,
        }
    }

    pub fn suggest_trade_fetch_range(
        &self,
        visible_earliest: UnixMs,
        visible_latest: UnixMs,
    ) -> Option<(UnixMs, UnixMs)> {
        if self.datapoints.is_empty() {
            return None;
        }

        self.find_trade_gap()
            .and_then(|(last_t_before_gap, first_t_after_gap)| {
                // Cold start: no trade has landed anywhere in the loaded range yet (e.g.
                // the market is closed and no live tick has arrived), so both bounds are
                // `None`. Falling through instead of bailing here lets the `map_or`s below
                // substitute the full loaded range, which is exactly the bootstrap fetch
                // this pane needs — without it, a footprint chart opened while no live
                // trade is streaming never requests its historical ticks at all.
                let (data_earliest, data_latest) = self.timerange();

                let fetch_from = last_t_before_gap
                    .map_or(data_earliest, |t| t.saturating_add(1))
                    .max(visible_earliest);
                let fetch_to = first_t_after_gap
                    .map_or(data_latest, |t| t.saturating_sub(1))
                    .min(visible_latest);

                if fetch_from < fetch_to {
                    Some((fetch_from, fetch_to))
                } else {
                    None
                }
            })
    }

    fn find_trade_gap(&self) -> Option<(Option<UnixMs>, Option<UnixMs>)> {
        let empty_kline_time = self
            .datapoints
            .iter()
            .rev()
            .find(|(_, dp)| dp.footprint.trades.is_empty())
            .map(|(&time, _)| time);

        if let Some(target_time) = empty_kline_time {
            let last_t_before_gap = self
                .datapoints
                .range(..target_time)
                .rev()
                .find_map(|(_, dp)| dp.last_trade_time());

            let first_t_after_gap = self
                .datapoints
                .range(target_time.saturating_add(1)..)
                .find_map(|(_, dp)| dp.first_trade_time());

            Some((last_t_before_gap, first_t_after_gap))
        } else {
            None
        }
    }

    pub fn max_qty_ts_range(
        &self,
        cluster_kind: ClusterKind,
        earliest: UnixMs,
        latest: UnixMs,
        highest: Price,
        lowest: Price,
    ) -> Qty {
        let mut max_cluster_qty: Qty = Qty::default();

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                max_cluster_qty =
                    max_cluster_qty.max(dp.max_cluster_qty(cluster_kind, highest, lowest));
            });

        max_cluster_qty
    }
}

impl TimeSeries<HeatmapDataPoint> {
    pub fn new(basis: Basis, tick_size: PriceStep) -> Self {
        let timeframe = match basis {
            Basis::Time(interval) => interval,
            Basis::Tick(_) => unimplemented!(),
        };

        Self {
            datapoints: BTreeMap::new(),
            interval: timeframe,
            tick_size,
        }
    }

    pub fn max_trade_qty_and_aggr_volume(&self, earliest: UnixMs, latest: UnixMs) -> (Qty, Qty) {
        let mut max_trade_qty = Qty::ZERO;
        let mut max_aggr_volume = Qty::ZERO;

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                let (mut buy_volume, mut sell_volume) = (Qty::ZERO, Qty::ZERO);

                dp.grouped_trades.iter().for_each(|trade| {
                    let trade_qty = trade.qty;
                    max_trade_qty = max_trade_qty.max(trade_qty);

                    if trade.is_sell {
                        sell_volume += trade_qty;
                    } else {
                        buy_volume += trade_qty;
                    }
                });

                max_aggr_volume = max_aggr_volume.max(buy_volume + sell_volume);
            });

        (max_trade_qty, max_aggr_volume)
    }

    pub fn max_trade_qty_in_range(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
        highest: Price,
        lowest: Price,
    ) -> Qty {
        let mut max_trade_qty = Qty::default();

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                dp.grouped_trades.iter().for_each(|trade| {
                    if trade.price >= lowest && trade.price <= highest {
                        max_trade_qty = max_trade_qty.max(trade.qty);
                    }
                });
            });

        max_trade_qty
    }
}

impl From<&TimeSeries<KlineDataPoint>> for BTreeMap<UnixMs, exchange::Volume> {
    /// Converts datapoints into a map of timestamps and volume data
    fn from(timeseries: &TimeSeries<KlineDataPoint>) -> Self {
        timeseries
            .datapoints
            .iter()
            .map(|(time, dp)| (*time, dp.kline.volume))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M5: u64 = 300_000;

    fn kline_at(minute_bucket: u64, close: f64) -> Kline {
        let price = Price::from_f64(close);
        Kline {
            time: UnixMs::new(minute_bucket * M5),
            open: price,
            high: price,
            low: price,
            close: price,
            volume: Volume::BuySell(Qty::from_f64(1.0), Qty::from_f64(1.0)),
        }
    }

    fn series() -> TimeSeries<KlineDataPoint> {
        TimeSeries::<KlineDataPoint>::new(Timeframe::M5, PriceStep::from_f64_exact(0.00005), &[])
    }

    fn trade_at(ms: u64, price: f64, qty: f64) -> Trade {
        Trade {
            time: UnixMs::new(ms),
            is_sell: false,
            price: Price::from_f64(price),
            qty: Qty::from_f64(qty),
        }
    }

    fn bucket_volume(ts: &TimeSeries<KlineDataPoint>, minute_bucket: u64) -> f64 {
        ts.datapoints
            .get(&UnixMs::new(minute_bucket * M5))
            .map(|dp| {
                dp.footprint
                    .trades
                    .values()
                    .map(|g| g.buy_qty.to_f64() + g.sell_qty.to_f64())
                    .sum()
            })
            .unwrap_or(0.0)
    }

    /// A freshly opened footprint chart has klines (from the kline backfill) but zero
    /// trades anywhere yet — e.g. the market is closed, so no live tick has landed to
    /// kick off a trades fetch. This must still suggest a fetch spanning the whole loaded
    /// range instead of concluding there's nothing to do.
    #[test]
    fn cold_start_with_no_trades_anywhere_still_suggests_a_fetch() {
        let mut ts = series();
        ts.insert_klines(&[
            kline_at(0, 1.16500),
            kline_at(1, 1.16600),
            kline_at(2, 1.16700),
        ]);

        let visible_earliest = UnixMs::new(0);
        let visible_latest = UnixMs::new(3 * M5);

        let suggested = ts.suggest_trade_fetch_range(visible_earliest, visible_latest);
        assert_eq!(
            suggested,
            Some((visible_earliest, UnixMs::new(2 * M5))),
            "a totally empty tape must still bootstrap a fetch across the loaded range"
        );
    }

    /// A bucket holds the trades of its own interval and nothing else — the whole point
    /// of a footprint bar. Trades from neighbouring 5m windows must not leak into it.
    #[test]
    fn a_bucket_only_ever_holds_its_own_intervals_trades() {
        let mut ts = series();
        ts.insert_klines(&[kline_at(0, 1.16500), kline_at(1, 1.16600)]);

        ts.insert_trades_existing_buckets(&[
            trade_at(1_000, 1.16500, 3.0),
            trade_at(M5 - 1, 1.16510, 2.0),
            trade_at(M5, 1.16600, 7.0),
        ]);

        assert!((bucket_volume(&ts, 0) - 5.0).abs() < 1e-9);
        assert!((bucket_volume(&ts, 1) - 7.0).abs() < 1e-9);
    }

    /// Replaying the tape is how a backfill fills buckets it just opened, and it runs
    /// again on every kline fetch — so it has to be idempotent. Without the clear, each
    /// replay stacked another copy of the tape onto buckets that already had it, which is
    /// how a 5-contract candle ends up reading tens of thousands.
    #[test]
    fn replaying_the_tape_rebuilds_buckets_instead_of_stacking_on_them() {
        let mut ts = series();
        ts.insert_klines(&[kline_at(0, 1.16500)]);

        let tape = [trade_at(1_000, 1.16500, 3.0), trade_at(2_000, 1.16505, 2.0)];
        ts.insert_trades_existing_buckets(&tape);
        assert!((bucket_volume(&ts, 0) - 5.0).abs() < 1e-9);

        for _ in 0..5 {
            ts.clear_trades();
            ts.insert_trades_existing_buckets(&tape);
        }

        assert!(
            (bucket_volume(&ts, 0) - 5.0).abs() < 1e-9,
            "replaying the tape must not multiply the bucket's volume"
        );
    }

    #[test]
    fn quiet_buckets_are_carried_forward_so_the_series_has_no_holes() {
        // Mirrors real 6E data: three consecutive 5m buckets with no prints
        // (08:55/09:00/09:05 in the reference capture) between two that traded.
        let mut ts = series();
        ts.insert_klines(&[kline_at(0, 1.16500), kline_at(4, 1.16600)]);

        let times: Vec<u64> = ts.datapoints.keys().map(|t| t.as_u64() / M5).collect();
        assert_eq!(
            times,
            vec![0, 1, 2, 3, 4],
            "a quiet stretch must render as flat price, not as missing buckets"
        );

        for bucket in 1..=3 {
            let dp = &ts.datapoints[&UnixMs::new(bucket * M5)];
            let carried = Price::from_f64(1.16500);
            assert_eq!(dp.kline.open, carried);
            assert_eq!(dp.kline.high, carried);
            assert_eq!(dp.kline.low, carried);
            assert_eq!(dp.kline.close, carried);
            assert!(
                dp.kline.volume.total().is_zero(),
                "nothing traded in a carried bucket, so its volume must read zero"
            );
        }
    }

    #[test]
    fn a_closure_is_not_carried_across() {
        // Friday's last 5m bucket, then Monday's first: nothing traded in between
        // because nothing could.
        let mut ts = series();
        let monday = (2 * 24 * 60 * 60 * 1000) / M5;
        ts.insert_klines(&[kline_at(0, 1.16500), kline_at(monday, 1.16600)]);

        let times: Vec<u64> = ts.datapoints.keys().map(|t| t.as_u64() / M5).collect();
        assert_eq!(
            times,
            vec![0, monday],
            "a weekend must not be filled in with flat candles nobody traded"
        );
    }

    #[test]
    fn a_traded_bucket_is_never_overwritten_by_a_carried_one() {
        let mut ts = series();
        // Out-of-order arrival: the later bucket lands first, then the gap is filled
        // backwards, and finally the real middle bucket arrives.
        ts.insert_klines(&[kline_at(0, 1.16500), kline_at(2, 1.16600)]);
        ts.insert_klines(&[kline_at(1, 1.16550)]);

        let middle = &ts.datapoints[&UnixMs::new(M5)];
        assert_eq!(middle.kline.close, Price::from_f64(1.16550));
        assert!(!middle.kline.volume.total().is_zero());
    }

    #[test]
    fn the_first_kline_has_nothing_to_carry_from() {
        let mut ts = series();
        ts.insert_klines(&[kline_at(100, 1.16500)]);
        assert_eq!(ts.datapoints.len(), 1, "must not backfill to the epoch");
    }
}
