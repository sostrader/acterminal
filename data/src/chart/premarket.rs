//! Premarket levels indicator: tracks the distinct auction prices touched during
//! the premarket session, the order each was first reached (the "sequence"), and
//! the session OHLC.
//!
//! Each level also carries:
//! - `vol`: book size parada exatamente naquele preço — soma dos `size` de
//!   `bids`/`offers` cujos preços batem com o preço do nível ("V" na tag);
//! - `dwell`: how long the theoretical price stood at that level, i.e. the
//!   accumulated `next_ts - ts` over every snapshot printed at that price
//!   ("T" na tag, em segundos).
//!
//! Session window: source-driven, NOT a hard 12:00 UTC cutoff. The feed
//! publishes on the `premarket/` topic only while the B3 auction is active
//! (`em_leilao`), including B3 prorrogações past 09:00 BRT — those points
//! belong to the session and must move high/low/close. Regular trading
//! arrives on other topics (`ticks/`/`doms/`) and never enters this model,
//! so no time-of-day check is needed to keep the open session out: a 5194
//! print from regular trading simply never arrives here.
//!
//! A sequence number is assigned to a distinct price the first time the auction
//! touches it. `high`/`low` are the numeric extremes: these are rendered in one
//! colour (yellow) while every intermediate level is rendered in another (white),
//! so a viewer can tell at a glance which levels are the session extremes.
//! `open`/`close` are the first/last session prints in chronological order;
//! `close` is the auction close (last theoretical print, prorrogação
//! included) and freezes once the feed stops publishing `premarket/`.
//!
//! The model is fed two ways:
//! - bootstrap history from `hist/premarket/<product>` as
//!   `(ts, price, size, bid_vol, ask_vol, bid_at, ask_at)` snapshots (the
//!   persisted `premarket/` topic, auction-only by construction);
//! - live ticks arriving at runtime from the same topic.
//!
//! The snapshot is sorted by `ts` on ingest (the store makes no order promise),
//! so open/close/sequence always follow the tape. Only points from the plotted
//! session day (`YYYYMMDD` UTC) are accepted, so volume is always same-day.
//! Live ticks must be newer than the bootstrap watermark (an optional
//! `loaded_ts`); duplicate prices update vol/dwell but never take a new
//! sequence number, so replayed overlap between the snapshot tail and the
//! live stream cannot inflate the sequence. A live point from a NEWER day
//! rolls the session forward (pane opened before today's auction): levels
//! reset and the new day builds from zero.

use std::collections::HashMap;

/// A single premarket level: its price, the order it was first touched,
/// the accumulated book size printed at it, and how long the price stood there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PremarketLevel {
    pub price: f64,
    /// 1-based order the auction first touched `price`.
    pub seq: usize,
    /// Summed book size resting exactly at `price`, over every snapshot printed there.
    pub vol: f64,
    /// Summed size over ALL bid levels of every snapshot printed at `price`.
    pub bid_vol: f64,
    /// Summed size over ALL offer levels of every snapshot printed at `price`.
    pub ask_vol: f64,
    /// Bid queue resting exactly at `price` (Theoric sell side).
    pub bid_at: f64,
    /// Offer queue resting exactly at `price` (Theoric buy side).
    pub ask_at: f64,
    /// Accumulated dwell time at `price`, in milliseconds.
    pub dwell_ms: i64,
}

impl PremarketLevel {
    /// Dwell time at this level, in whole seconds (floor).
    pub fn dwell_secs(&self) -> i64 {
        (self.dwell_ms / 1000).max(0)
    }
}

/// Per-level accumulators, parallel to `order`.
#[derive(Debug, Clone, Copy, Default)]
struct LevelStats {
    vol: f64,
    bid_vol: f64,
    ask_vol: f64,
    bid_at: f64,
    ask_at: f64,
    dwell_ms: i64,
}

/// Live-tracking premarket session levels.
///
/// Default view is a single session day: `bootstrap` keeps the raw snapshot but
/// only builds levels for the day of the newest point (normally "today"), so a
/// multi-day history never plots older sessions on top of the current one.
/// Call `set_session_day` to plot a different date (date picker / scroll-follow:
/// pass the day visible at the chart center); levels rebuild from the stored raw
/// points and live ticks newer than the snapshot keep merging on top.
#[derive(Debug, Clone, Default)]
pub struct PremarketLevels {
    /// Distinct prices in first-touch order.
    order: Vec<f64>,
    /// price -> index into `order` (for O(1) dedup).
    seen: HashMap<i64, usize>,
    /// Per-level accumulators, parallel to `order`.
    stats: Vec<LevelStats>,
    /// Highest `ts` accepted so far (live ticks must be newer than this).
    loaded_ts: Option<i64>,
    /// Full unfiltered bootstrap snapshot, so the session day can change later
    /// without re-fetching. Each point carries `(ts, price, size, bid_vol, ask_vol)`.
    raw: Vec<(i64, f64, f64, f64, f64, f64, f64)>,
    /// Currently plotted session day as `YYYYMMDD` (UTC). `None` = empty.
    session_day: Option<i64>,
    /// First session print in chronological order.
    open: Option<f64>,
    /// Timestamp of the first accepted print (auction start).
    open_ts: Option<i64>,
    /// Last session print in chronological order (auction close, prorrogação included).
    close: Option<f64>,
    /// Timestamp of the last accepted print (auction end, prorrogação included).
    close_ts: Option<i64>,
    /// Previous accepted `(ts, price_key)`: the price stood at `price_key`
    /// from `ts` until the next accepted point, which is when its dwell is known.
    prev: Option<(i64, i64)>,
    /// Bumped on every accepted point (new level or re-touch that moved
    /// vol/dwell); lets views re-render stats without polling levels.
    version: u64,
}

/// Legacy nominal end of the premarket auction as UTC millis-of-day
/// (09:00 BRT = 12:00 UTC). Kept for observability/back-compat only:
/// the session end is source-driven (last `premarket/` print of the day,
/// prorrogação included), never a hard time cutoff. Do NOT use this to
/// discard points.
pub const SESSION_END_UTC_MS: i64 = 12 * 3_600_000;

impl PremarketLevels {
    pub fn new() -> Self {
        Self::default()
    }

    /// Highest timestamp accepted so far (from bootstrap or live).
    pub fn loaded_ts(&self) -> Option<i64> {
        self.loaded_ts
    }

    /// Number of distinct levels.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Monotonic counter of accepted points; views compare it to detect
    /// vol/dwell updates on re-touched levels.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Replace the whole session with a bootstrap history snapshot. Pass `max_ts`
    /// (the highest timestamp in the snapshot) so subsequent live ticks can be
    /// filtered to newer data.
    pub fn bootstrap<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = (i64, f64, f64, f64, f64, f64, f64)>,
    {
        let mut raw: Vec<(i64, f64, f64, f64, f64, f64, f64)> = points.into_iter().collect();
        // The store makes no order promise; OHLC/sequence must follow the tape.
        raw.sort_by_key(|(ts, _, _, _, _, _, _)| *ts);
        let max_ts = raw.iter().map(|(ts, _, _, _, _, _, _)| *ts).max();
        // Default: plot only the newest session day.
        let day = max_ts.map(Self::day_key);
        self.raw = raw;
        self.session_day = day;
        self.rebuild_from_raw(max_ts);
    }

    /// Currently plotted session day as `YYYYMMDD` (UTC), if any.
    pub fn session_day(&self) -> Option<i64> {
        self.session_day
    }

    /// Plot a different session day (`YYYYMMDD` in UTC, e.g. from a date picker
    /// or from the day under the chart's scroll center). Rebuilds levels from
    /// the stored bootstrap snapshot; no re-fetch needed.
    pub fn set_session_day(&mut self, day: i64) {
        if self.raw.is_empty() {
            return;
        }
        self.session_day = Some(day);
        let max_ts = self.raw.iter().map(|(ts, _, _, _, _, _, _)| *ts).max();
        self.rebuild_from_raw(max_ts);
    }

    /// UTC millis-of-day for a millisecond timestamp.
    fn millis_of_day_utc(ts_ms: i64) -> i64 {
        ts_ms.rem_euclid(86_400_000)
    }

    /// Legacy nominal check: whether `ts` falls before 12:00 UTC.
    /// Observability/back-compat only — acceptance is source-driven (any
    /// `premarket/` print of the session day counts, including prorrogação
    /// past 12:00 UTC), so do NOT gate ingest on this.
    pub fn in_session(ts_ms: i64) -> bool {
        Self::millis_of_day_utc(ts_ms) < SESSION_END_UTC_MS
    }

    /// Build `YYYYMMDD` (UTC) for a millisecond timestamp.
    pub fn day_key(ts_ms: i64) -> i64 {
        let secs = ts_ms.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        // Convert days-since-epoch to civil date (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let mut y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        y += i64::from(m <= 2);
        y * 10_000 + m * 100 + d
    }

    fn rebuild_from_raw(&mut self, max_ts: Option<i64>) {
        let mut rebuilt = Self {
            raw: std::mem::take(&mut self.raw),
            session_day: self.session_day,
            ..Self::default()
        };
        if let Some(day) = rebuilt.session_day {
            // `raw` is sorted by ts at bootstrap: chronological scan gives the
            // tape order, so open/close/sequence follow the session properly.
            // Source-driven: every stored point of the day is auction (the
            // `premarket/` topic only publishes while em_leilao), including
            // B3 prorrogação past 12:00 UTC — no time-of-day gate.
            for (ts, price, size, bid, ask, bat, aat) in rebuilt.raw.clone() {
                if Self::day_key(ts) == day {
                    rebuilt.add_point(ts, price, size, bid, ask, bat, aat);
                }
            }
        }
        rebuilt.loaded_ts = max_ts;
        *self = rebuilt;
    }

    /// Ingest a single `premarket/` point. Returns `Some(seq)` if it introduced
    /// a brand new level (first touch), or `None` if it was a duplicate, older
    /// than the bootstrap watermark, or from an older session day. There is
    /// intentionally NO time-of-day gate: the topic only publishes while the
    /// B3 auction is active, so prorrogação past 12:00 UTC belongs to the
    /// session. During the bootstrap rebuild `loaded_ts` is still `None`, so
    /// the watermark check only applies to live ticks. A live point from a
    /// NEWER day rolls the session forward (levels reset, new day builds
    /// from zero) so a pane opened before today's auction still picks it up.
    ///
    /// Re-touches of a known level still accumulate `size` into its volume and
    /// extend dwell: the previous price is credited with `ts - prev_ts` once the
    /// next print arrives, so dwell is exact regardless of re-touches.
    pub fn add_point(
        &mut self,
        ts: i64,
        price: f64,
        size: f64,
        bid_vol: f64,
        ask_vol: f64,
        bid_at: f64,
        ask_at: f64,
    ) -> Option<usize> {
        let day = Self::day_key(ts);
        match self.session_day {
            None => self.session_day = Some(day),
            Some(cur) if cur != day => {
                if day < cur {
                    return None;
                }
                // Rollover to a newer auction day seen live.
                self.order.clear();
                self.seen.clear();
                self.stats.clear();
                self.open = None;
                self.open_ts = None;
                self.close = None;
                self.close_ts = None;
                self.prev = None;
                self.session_day = Some(day);
            }
            Some(_) => {}
        }
        if let Some(watermark) = self.loaded_ts
            && ts <= watermark
        {
            return None;
        }
        // Every accepted print moves the close, even re-touches of a level.
        if self.open.is_none() {
            self.open = Some(price);
            self.open_ts = Some(ts);
        }
        self.close = Some(price);
        self.close_ts = Some(ts);
        self.loaded_ts = Some(self.loaded_ts.map_or(ts, |wm| wm.max(ts)));
        let key = Self::price_key(price);
        // Dwell: the previous price stood from prev_ts until now.
        if let Some((prev_ts, prev_key)) = self.prev
            && ts > prev_ts
            && let Some(&idx) = self.seen.get(&prev_key)
        {
            self.stats[idx].dwell_ms += ts - prev_ts;
        }
        self.prev = Some((ts, key));
        self.version += 1;
        if let Some(&idx) = self.seen.get(&key) {
            self.stats[idx].vol += size.max(0.0);
            self.stats[idx].bid_vol += bid_vol.max(0.0);
            self.stats[idx].ask_vol += ask_vol.max(0.0);
            self.stats[idx].bid_at += bid_at.max(0.0);
            self.stats[idx].ask_at += ask_at.max(0.0);
            return None;
        }
        let seq = self.order.len() + 1;
        self.seen.insert(key, self.order.len());
        self.order.push(price);
        self.stats.push(LevelStats {
            vol: size.max(0.0),
            bid_vol: bid_vol.max(0.0),
            ask_vol: ask_vol.max(0.0),
            bid_at: bid_at.max(0.0),
            ask_at: ask_at.max(0.0),
            dwell_ms: 0,
        });
        Some(seq)
    }

    /// The distinct levels in first-touch order, each tagged with its sequence,
    /// accumulated volume and dwell time.
    pub fn levels(&self) -> impl Iterator<Item = PremarketLevel> + '_ {
        self.order.iter().enumerate().map(|(i, &price)| {
            let stats = self.stats.get(i).copied().unwrap_or_default();
            PremarketLevel {
                price,
                seq: i + 1,
                vol: stats.vol,
                bid_vol: stats.bid_vol,
                ask_vol: stats.ask_vol,
                bid_at: stats.bid_at,
                ask_at: stats.ask_at,
                dwell_ms: stats.dwell_ms,
            }
        })
    }

    /// Session extreme values, if any.
    pub fn high(&self) -> Option<f64> {
        self.order.iter().copied().reduce(f64::max)
    }

    /// Session extreme values, if any.
    pub fn low(&self) -> Option<f64> {
        self.order.iter().copied().reduce(f64::min)
    }

    /// Session open: first accepted print in chronological order.
    pub fn open(&self) -> Option<f64> {
        self.open
    }

    /// Timestamp of the auction start (first accepted print), if any.
    pub fn open_ts(&self) -> Option<i64> {
        self.open_ts
    }

    /// Session close: last accepted print in chronological order (auction
    /// close, prorrogação included). Frozen once the feed stops publishing.
    pub fn close(&self) -> Option<f64> {
        self.close
    }

    /// Timestamp of the auction close (last accepted print), if any.
    pub fn close_ts(&self) -> Option<i64> {
        self.close_ts
    }

    /// Full session OHLC, if any prints were accepted.
    pub fn ohlc(&self) -> Option<(f64, f64, f64, f64)> {
        match (self.open, self.high(), self.low(), self.close) {
            (Some(o), Some(h), Some(l), Some(c)) => Some((o, h, l, c)),
            _ => None,
        }
    }

    fn price_key(price: f64) -> i64 {
        // f64 -> i64 keying at a fixed atomic scale; identical prices map to the
        // same key and distinct prices (beyond float noise) land on distinct keys.
        (price * 1e11).round() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prorrogacao_same_day_ohlc_and_sort() {
        // 2026-09-11: auction 11:55 + B3 prorrogação past 12:00 UTC.
        // Source-driven: every premarket/ print of the day belongs to the
        // session, including prorrogação — no hard 12:00 cutoff.
        let base = chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let t = |h: u32, m: u32, s: u32| base + ((h * 3600 + m * 60 + s) as i64) * 1000;

        let mut pm = PremarketLevels::new();
        // Deliberately unsorted bootstrap, with prorrogação prints.
        pm.bootstrap([
            (t(12, 2, 0), 5183.0, 10.0, 4.0, 6.0, 4.0, 6.0),
            (t(11, 55, 0), 5170.0, 10.0, 4.0, 6.0, 4.0, 6.0),
            (t(12, 0, 30), 5181.0, 10.0, 4.0, 6.0, 4.0, 6.0),
            (t(11, 58, 0), 5175.0, 10.0, 4.0, 6.0, 4.0, 6.0),
        ]);
        assert_eq!(pm.high(), Some(5183.0));
        assert_eq!(pm.low(), Some(5170.0));
        assert_eq!(pm.open(), Some(5170.0));
        assert_eq!(pm.open_ts(), Some(t(11, 55, 0)));
        assert_eq!(pm.close(), Some(5183.0));
        assert_eq!(pm.close_ts(), Some(t(12, 2, 0)));
        assert_eq!(pm.ohlc(), Some((5170.0, 5183.0, 5170.0, 5183.0)));

        // Live prorrogação print moves the close (auction still active).
        assert_eq!(
            pm.add_point(t(12, 3, 0), 5185.0, 5.0, 2.0, 3.0, 2.0, 3.0),
            Some(5)
        );
        assert_eq!(pm.high(), Some(5185.0));
        assert_eq!(pm.close(), Some(5185.0));
        assert_eq!(pm.close_ts(), Some(t(12, 3, 0)));
    }

    #[test]
    fn same_day_only_and_rollover() {
        let day1 = chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day2 = chrono::NaiveDate::from_ymd_opt(2026, 9, 12)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let t1 = |h: u32, m: u32| day1 + ((h * 3600 + m * 60) as i64) * 1000;
        let t2 = |h: u32, m: u32| day2 + ((h * 3600 + m * 60) as i64) * 1000;

        // Bootstrap spans two days: only the newest day is plotted.
        let mut pm = PremarketLevels::new();
        pm.bootstrap([
            (t1(11, 56), 5170.0, 10.0, 4.0, 6.0, 4.0, 6.0),
            (t2(11, 56), 5190.0, 10.0, 4.0, 6.0, 4.0, 6.0),
        ]);
        assert_eq!(pm.session_day(), Some(PremarketLevels::day_key(t2(11, 56))));
        assert_eq!(pm.high(), Some(5190.0));
        assert_eq!(pm.len(), 1);

        // Pane opened before today's auction: bootstrap de ontem, live de hoje
        // faz rollover e constrói o dia novo do zero.
        let mut pm2 = PremarketLevels::new();
        pm2.bootstrap([(t1(11, 56), 5170.0, 10.0, 4.0, 6.0, 4.0, 6.0)]);
        assert_eq!(pm2.close(), Some(5170.0));
        assert_eq!(
            pm2.add_point(t2(11, 55), 5190.0, 8.0, 3.0, 5.0, 3.0, 5.0),
            Some(1)
        );
        assert_eq!(
            pm2.session_day(),
            Some(PremarketLevels::day_key(t2(11, 55)))
        );
        assert_eq!(pm2.len(), 1);
        assert_eq!(pm2.open(), Some(5190.0));
        assert_eq!(pm2.open_ts(), Some(t2(11, 55)));
        assert_eq!(pm2.close(), Some(5190.0));

        // Stale point from the older day is ignored after rollover.
        assert_eq!(
            pm2.add_point(t1(11, 57), 5171.0, 1.0, 0.0, 1.0, 0.0, 1.0),
            None
        );
        assert_eq!(pm2.len(), 1);
    }

    #[test]
    fn first_touch_sequence_and_extremes() {
        let mut pm = PremarketLevels::new();
        // Bootstrap snapshot of history, highest ts 1000.
        pm.bootstrap([
            (100, 100.0, 5.0, 2.0, 3.0, 2.0, 3.0),
            (200, 102.0, 7.0, 3.0, 4.0, 3.0, 4.0),
            (300, 101.0, 3.0, 1.0, 2.0, 1.0, 2.0),
            (400, 100.0, 11.0, 5.0, 6.0, 5.0, 6.0),
        ]);
        assert_eq!(pm.high(), Some(102.0));
        assert_eq!(pm.low(), Some(100.0));
        let levels: Vec<_> = pm.levels().collect();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].seq, 1);
        assert_eq!(levels[0].price, 100.0);
        assert_eq!(levels[1].seq, 2);
        assert_eq!(levels[1].price, 102.0);
        assert_eq!(levels[2].seq, 3);
        assert_eq!(levels[2].price, 101.0);

        // A replayed/overlapping old point must not create a new level.
        assert_eq!(pm.add_point(500, 100.0, 2.0, 1.0, 1.0, 1.0, 1.0), None);
        assert_eq!(pm.len(), 3);

        // A genuinely new live level gets the next sequence.
        assert_eq!(pm.add_point(1001, 103.0, 4.0, 2.0, 2.0, 2.0, 2.0), Some(4));
        assert_eq!(pm.len(), 4);
        assert_eq!(pm.high(), Some(103.0));
    }

    #[test]
    fn volume_and_dwell_accumulate() {
        let mut pm = PremarketLevels::new();
        // 100.0 printed at t=0 (size 5) and t=10s (size 7); 101.0 at t=30s.
        // Dwell of 100.0 = 30s (stood from 0 until the 101 print at 30s);
        // vol of 100.0 = 12.
        pm.bootstrap([
            (0, 100.0, 5.0, 2.0, 3.0, 2.0, 3.0),
            (10_000, 100.0, 7.0, 3.0, 4.0, 3.0, 4.0),
            (30_000, 101.0, 3.0, 1.0, 2.0, 1.0, 2.0),
        ]);
        let levels: Vec<_> = pm.levels().collect();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].vol, 12.0);
        assert_eq!(levels[0].dwell_secs(), 30);
        assert_eq!(levels[1].vol, 3.0);
        assert_eq!(levels[1].dwell_secs(), 0);
    }
}
