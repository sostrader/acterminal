//! MAGNETS: pre-open auction attraction levels, from premarket book only.
//!
//! The headline `V` and `T` of every level are DEFINED to match the premarket
//! indicator's print for that price, because both read the same `premarket/`
//! snapshots with the same math:
//!
//! - `V` = accumulated queue resting exactly at the price (`bid_at + ask_at`,
//!   summed over every snapshot printed there — the premarket "V" tag);
//! - `T` = accumulated dwell (every stand at the price summed, same
//!   `next_ts - ts` crediting as the premarket "T" tag).
//!
//! A single-snapshot `min()` match is structurally wrong as a headline: at an
//! auction extreme the book is one-sided by nature, so the min of one instant
//! always penalizes the strongest levels (e.g. 5194.5 showing V:336 against a
//! real accumulated 1536). What a magnet means is "how much interest this
//! price concentrated during the whole auction" — hence accumulation. The
//! `min()` survives only as an auxiliary execution diagnostic (`min_exec`:
//! "if I had to execute now, how much of this level is actually matchable?"),
//! kept out of the score.
//!
//! Score and selection: `S = wv * Vn + wt * Tn` per price, with `Vn`/`Tn`
//! min-max normalized over the session's candidates (raw `V * T` lets dwell
//! dominate by scale — a 43s stand with V:988 would outrank V:2529/T:12s,
//! even though the market trades heavy-queue zones, not long stands).
//! Drop prices with accumulated `T < t_min` first, enforce a minimum
//! separation between kept lines (near-duplicates collapse into the
//! stronger one), rank by `S` desc, keep top-K. The run-length change
//! sequence (`N` = index of each theoretical-price change, revisits open a
//! new event) is tracked for structure but never drives volume. Distance
//! from the auction close is display-only: magnets are targets known at the
//! open, never penalized for being far.
//!
//! Source discipline: every input comes from the `premarket/` topic
//! (persisted `hist/premarket` bootstrap + live). No candle, tick, DOM or
//! footprint value enters here. Monitoring/status/alerts are intentionally
//! OUT: this indicator only emits the frozen top-K ladder.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// One book level of a premarket snapshot, venue-space price + size.
/// Comparisons against the snapshot's theoretical price always happen in this
/// same space, so inversion (Tradovate 1/x symbols) can never skew the match.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

/// One premarket book snapshot: theoretical price + full book of that instant.
/// Carries every level the venue sent (5 today, more in the future — the
/// match formula below takes all of them with no logic change).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagnetSnapshot {
    pub ts: i64,
    pub price: f64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

/// One run-length event: the theoretical price stood at `price` from
/// `ts_start` until `ts_end` (next change, or auction close for the last).
/// Structural only — volume ranking lives on price aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MagnetEvent {
    /// 1-based change index (NOT a touch count).
    pub n: usize,
    pub price: f64,
    pub ts_start: i64,
    pub ts_end: i64,
    /// This stand's dwell in milliseconds.
    pub t_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MagnetSide {
    AboveOpen,
    BelowOpen,
    AtOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MagnetStatus {
    Pending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MagnetLevel {
    pub rank: usize,
    pub price: f64,
    pub score: f64,
    /// Accumulated at-price queue (== premarket "V" for this price).
    pub volume: f64,
    /// Accumulated dwell in milliseconds (== premarket "T" for this price).
    pub duration_ms: i64,
    /// First run-change index that touched this price.
    pub sequence_index: usize,
    pub distance_from_open: f64,
    pub side: MagnetSide,
    pub status: MagnetStatus,
    /// Instant matchability on the LATEST snapshot at this price
    /// (`min(bids >= P, asks <= P)`): execution-risk diagnostic, NOT ranked.
    pub min_exec: f64,
    pub min_exec_buy: f64,
    pub min_exec_sell: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagnetsConfig {
    /// How many magnet lines to emit.
    pub k: usize,
    /// Minimum ACCUMULATED dwell for a price to enter the ranking; shorter
    /// stands are pass-through recompute noise (the T:0s lines of the tape).
    pub t_min_ms: i64,
    /// Weight of normalized volume in the score.
    pub weight_vol: f64,
    /// Weight of normalized dwell in the score.
    pub weight_time: f64,
    /// Minimum price separation between kept lines; a candidate within
    /// `min_sep` of an already-picked stronger line is the same signal and
    /// is dropped (e.g. 5164.0 and 5165.5 never print together).
    pub min_sep: f64,
}

impl Default for MagnetsConfig {
    fn default() -> Self {
        Self {
            k: 3,
            t_min_ms: 1000,
            weight_vol: 0.6,
            weight_time: 0.4,
            min_sep: 2.0,
        }
    }
}

/// Tolerance for book-vs-theoretical price comparison (float noise only).
/// Same epsilon the premarket path uses, so the queue sums match exactly.
const PRICE_EPS: f64 = 1e-9;

/// Queue resting exactly at `price` in one snapshot's book, split by side.
fn at_price(bids: &[BookLevel], asks: &[BookLevel], price: f64) -> (f64, f64) {
    let at = |levels: &[BookLevel]| -> f64 {
        levels
            .iter()
            .filter(|l| (l.price - price).abs() < PRICE_EPS)
            .map(|l| l.size.max(0.0))
            .sum()
    };
    (at(bids), at(asks))
}

/// Instant matchability at `price` for one snapshot's book
/// (`min(bids >= P, asks <= P)`).
fn matchable(bids: &[BookLevel], asks: &[BookLevel], price: f64) -> (f64, f64, f64) {
    let buy: f64 = bids
        .iter()
        .filter(|l| l.price + PRICE_EPS >= price)
        .map(|l| l.size.max(0.0))
        .sum();
    let sell: f64 = asks
        .iter()
        .filter(|l| l.price - PRICE_EPS <= price)
        .map(|l| l.size.max(0.0))
        .sum();
    (buy.min(sell), buy, sell)
}

/// One price's session aggregates (the ranking input).
#[derive(Debug, Clone, PartialEq)]
pub struct PriceAggregate {
    pub price: f64,
    pub vol: f64,
    pub dwell_ms: i64,
    pub first_n: usize,
    pub min_exec: f64,
    pub min_exec_buy: f64,
    pub min_exec_sell: f64,
}

/// Auction excursion range over run events, if any.
pub fn aer(events: &[MagnetEvent]) -> Option<f64> {
    if events.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = (events[0].price, events[0].price);
    for e in &events[1..] {
        lo = lo.min(e.price);
        hi = hi.max(e.price);
    }
    Some(hi - lo)
}

/// Select the top-K magnets: drop prices with accumulated dwell below
/// `t_min`, score the rest with normalized `S = wv * Vn + wt * Tn`, enforce
/// the minimum separation (near-duplicates collapse into the stronger line),
/// keep top-K. Output order is rank order.
pub fn select_magnets(
    aggs: &[PriceAggregate],
    open_price: f64,
    cfg: &MagnetsConfig,
) -> Vec<MagnetLevel> {
    let cands: Vec<&PriceAggregate> = aggs.iter().filter(|a| a.dwell_ms >= cfg.t_min_ms).collect();
    if cands.is_empty() {
        return vec![];
    }
    let (mut min_v, mut max_v) = (cands[0].vol, cands[0].vol);
    let (mut min_t, mut max_t) = (cands[0].dwell_ms, cands[0].dwell_ms);
    for a in &cands[1..] {
        min_v = min_v.min(a.vol);
        max_v = max_v.max(a.vol);
        min_t = min_t.min(a.dwell_ms);
        max_t = max_t.max(a.dwell_ms);
    }
    let v_span = (max_v - min_v).max(f64::EPSILON);
    let t_span = ((max_t - min_t) as f64).max(f64::EPSILON);
    let score = |a: &PriceAggregate| {
        let vn = (a.vol - min_v) / v_span;
        let tn = (a.dwell_ms - min_t) as f64 / t_span;
        cfg.weight_vol * vn + cfg.weight_time * tn
    };
    let mut ranked: Vec<&PriceAggregate> = cands;
    ranked.sort_by(|a, b| {
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Separation pass: keep a candidate only if it is farther than `min_sep`
    // from every already-picked (stronger) line, so one slot is never spent
    // on a near-duplicate. Backfills from deeper ranks to still emit K lines
    // when possible.
    let mut picked: Vec<&PriceAggregate> = Vec::with_capacity(cfg.k.max(1));
    for a in ranked {
        if picked.len() >= cfg.k.max(1) {
            break;
        }
        if picked
            .iter()
            .all(|p: &&PriceAggregate| (p.price - a.price).abs() > cfg.min_sep)
        {
            picked.push(a);
        }
    }
    picked
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            let dist = a.price - open_price;
            MagnetLevel {
                rank: i + 1,
                price: a.price,
                score: score(a),
                volume: a.vol,
                duration_ms: a.dwell_ms,
                sequence_index: a.first_n,
                distance_from_open: dist,
                side: if dist > 0.0 {
                    MagnetSide::AboveOpen
                } else if dist < 0.0 {
                    MagnetSide::BelowOpen
                } else {
                    MagnetSide::AtOpen
                },
                status: MagnetStatus::Pending,
                min_exec: a.min_exec,
                min_exec_buy: a.min_exec_buy,
                min_exec_sell: a.min_exec_sell,
            }
        })
        .collect()
}

/// Per-price accumulator, parallel to `order`.
#[derive(Debug, Clone, Default)]
struct PriceStats {
    vol: f64,
    dwell_ms: i64,
    first_n: usize,
    min_exec: f64,
    min_exec_buy: f64,
    min_exec_sell: f64,
}

/// Session-day tracker feeding the builders above: accumulates the same-day
/// at-price queue + dwell per price (identical numbers to the premarket
/// print), tracks run-change events and the latest-book min-exec per price,
/// and re-ranks top-K on every ingest. Freezes once the feed stops
/// publishing `premarket/` for the day (no time-of-day gate, so B3
/// prorrogação belongs to the session).
#[derive(Debug, Clone, Default)]
pub struct MagnetStore {
    raw: Vec<MagnetSnapshot>,
    order: Vec<f64>,
    seen: HashMap<i64, usize>,
    stats: Vec<PriceStats>,
    runs: Vec<MagnetEvent>,
    session_day: Option<i64>,
    loaded_ts: Option<i64>,
    open_price: Option<f64>,
    prev: Option<(i64, i64)>,
    levels: Vec<MagnetLevel>,
    version: u64,
    cfg: MagnetsConfig,
}

impl MagnetStore {
    pub fn new(cfg: MagnetsConfig) -> Self {
        Self {
            cfg,
            ..Self::default()
        }
    }

    pub fn len(&self) -> usize {
        self.levels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty() && self.order.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn session_day(&self) -> Option<i64> {
        self.session_day
    }

    pub fn events(&self) -> &[MagnetEvent] {
        &self.runs
    }

    pub fn levels(&self) -> &[MagnetLevel] {
        &self.levels
    }

    pub fn config(&self) -> &MagnetsConfig {
        &self.cfg
    }

    pub fn set_config(&mut self, cfg: MagnetsConfig) {
        self.cfg = cfg;
        self.refresh();
    }

    /// Replace the whole session with a bootstrap history snapshot.
    pub fn bootstrap<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = MagnetSnapshot>,
    {
        let mut raw: Vec<MagnetSnapshot> = points.into_iter().collect();
        raw.sort_by_key(|s| s.ts);
        raw.dedup_by_key(|s| s.ts);
        let max_ts = raw.iter().map(|s| s.ts).max();
        // Version-gated views (`KlineChart::bootstrap_magnets`) only redraw
        // when `version` moves, so a bootstrap that changes the ladder must
        // bump it, otherwise the fetched history stays invisible until the
        // next live tick or a manual toggle.
        let before = (self.order.clone(), self.levels.clone());
        self.raw = raw;
        self.session_day = max_ts.map(day_key);
        self.loaded_ts = max_ts;
        self.reset_accumulators();
        if self.session_day.is_some() {
            for snap in self.raw.clone() {
                if Some(day_key(snap.ts)) == self.session_day {
                    self.ingest(snap);
                }
            }
        }
        self.refresh();
        if self.order != before.0 || self.levels != before.1 {
            self.version += 1;
        }
    }

    /// Ingest one live `premarket/` snapshot. Same-day only, newer than the
    /// bootstrap watermark; a newer day rolls the session forward so a pane
    /// opened before today's auction still picks it up.
    pub fn add_point(&mut self, snap: MagnetSnapshot) {
        let day = day_key(snap.ts);
        match self.session_day {
            None => self.session_day = Some(day),
            Some(cur) if cur != day => {
                if day < cur {
                    return;
                }
                self.raw.clear();
                self.reset_accumulators();
                self.session_day = Some(day);
            }
            Some(_) => {}
        }
        if let Some(wm) = self.loaded_ts
            && snap.ts <= wm
        {
            return;
        }
        self.loaded_ts = Some(self.loaded_ts.map_or(snap.ts, |w| w.max(snap.ts)));
        self.ingest(snap);
        self.refresh();
        self.version += 1;
    }

    fn reset_accumulators(&mut self) {
        self.order.clear();
        self.seen.clear();
        self.stats.clear();
        self.runs.clear();
        self.open_price = None;
        self.prev = None;
        self.levels.clear();
    }

    /// Fold one accepted same-day snapshot into the accumulators.
    fn ingest(&mut self, snap: MagnetSnapshot) {
        // Auction open for D: last theoretical print wins (chronological scan).
        self.open_price = Some(snap.price);
        let key = price_key(snap.price);
        // Dwell: the previous price stood from prev_ts until now.
        if let Some((prev_ts, prev_key)) = self.prev
            && snap.ts > prev_ts
            && let Some(&idx) = self.seen.get(&prev_key)
        {
            self.stats[idx].dwell_ms += snap.ts - prev_ts;
        }
        // Run-change event: a new price (or the first print) opens a new N.
        let new_run = match self.prev {
            None => true,
            Some((_, prev_key)) => prev_key != key,
        };
        if new_run {
            let n = self.runs.len() + 1;
            let ts = snap.ts;
            self.runs.push(MagnetEvent {
                n,
                price: snap.price,
                ts_start: ts,
                ts_end: ts,
                t_ms: 0,
            });
        } else if let Some(run) = self.runs.last_mut() {
            run.ts_end = snap.ts;
            run.t_ms = (run.ts_end - run.ts_start).max(0);
        }
        self.prev = Some((snap.ts, key));
        let run_n = self.runs.len();
        let (bid_at, ask_at) = at_price(&snap.bids, &snap.asks, snap.price);
        let (exec, exec_buy, exec_sell) = matchable(&snap.bids, &snap.asks, snap.price);
        if let Some(&idx) = self.seen.get(&key) {
            self.stats[idx].vol += bid_at + ask_at;
            // Latest snapshot at this price wins for the exec diagnostic.
            self.stats[idx].min_exec = exec;
            self.stats[idx].min_exec_buy = exec_buy;
            self.stats[idx].min_exec_sell = exec_sell;
        } else {
            self.seen.insert(key, self.order.len());
            self.order.push(snap.price);
            self.stats.push(PriceStats {
                vol: bid_at + ask_at,
                dwell_ms: 0,
                first_n: run_n,
                min_exec: exec,
                min_exec_buy: exec_buy,
                min_exec_sell: exec_sell,
            });
        }
    }

    fn refresh(&mut self) {
        let open = self.open_price.unwrap_or(0.0);
        let aggs: Vec<PriceAggregate> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, &price)| {
                let st = &self.stats[i];
                PriceAggregate {
                    price,
                    vol: st.vol,
                    dwell_ms: st.dwell_ms,
                    first_n: st.first_n,
                    min_exec: st.min_exec,
                    min_exec_buy: st.min_exec_buy,
                    min_exec_sell: st.min_exec_sell,
                }
            })
            .collect();
        self.levels = select_magnets(&aggs, open, &self.cfg);
    }
}

/// f64 -> i64 keying at a fixed atomic scale (same as the premarket path, so
/// level identity matches the premarket print exactly).
fn price_key(price: f64) -> i64 {
    (price * 1e11).round() as i64
}

/// Build `YYYYMMDD` (UTC) for a millisecond timestamp (Howard Hinnant).
fn day_key(ts_ms: i64) -> i64 {
    let secs = ts_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lvl(price: f64, size: f64) -> BookLevel {
        BookLevel { price, size }
    }

    /// Book with an at-price queue of `at` per side plus off-price depth that
    /// makes the instant min() deliberately different from the queue.
    fn snap(ts: i64, price: f64, at: f64) -> MagnetSnapshot {
        MagnetSnapshot {
            ts,
            price,
            bids: vec![
                lvl(price + 1.0, 900.0),
                lvl(price, at),
                lvl(price - 1.0, 100.0),
            ],
            asks: vec![
                lvl(price - 1.0, 800.0),
                lvl(price, at),
                lvl(price + 1.0, 50.0),
            ],
        }
    }

    #[test]
    fn volume_and_dwell_match_premarket_basis() {
        // P=100 printed 3x (queues 300, 300, 400 per side): V = 2*(300+300+400).
        // Stands 0->10s and 10s->20s credited (20s); last stand open, uncredited.
        let mut st = MagnetStore::new(MagnetsConfig {
            k: 5,
            t_min_ms: 0,
            ..MagnetsConfig::default()
        });
        st.bootstrap([
            snap(0, 100.0, 300.0),
            snap(10_000, 100.0, 300.0),
            snap(20_000, 101.0, 10.0),
            snap(30_000, 100.0, 400.0),
        ]);
        let aggs: Vec<(f64, f64, i64)> = st
            .order
            .iter()
            .enumerate()
            .map(|(i, &p)| (p, st.stats[i].vol, st.stats[i].dwell_ms))
            .collect();
        assert_eq!(aggs[0], (100.0, 2000.0, 20_000));
        assert_eq!(aggs[1].0, 101.0);
        assert_eq!(aggs[1].2, 10_000);
        // Revisit opens a new run-change event; first touch keeps seq 1.
        assert_eq!(st.events().len(), 3);
        assert_eq!(st.events()[2].n, 3);
        assert_eq!(st.stats[0].first_n, 1);
    }

    #[test]
    fn min_exec_comes_from_latest_snapshot_not_score() {
        let mut st = MagnetStore::new(MagnetsConfig {
            k: 5,
            t_min_ms: 0,
            ..MagnetsConfig::default()
        });
        st.bootstrap([snap(0, 100.0, 300.0), snap(10_000, 100.0, 400.0)]);
        // Latest book at 100: bids >= 100 -> 900+400 = 1300; asks <= 100 ->
        // 800+400 = 1200; min = 1200 (queue basis V = 2*(300+400) = 1400).
        let only = &st.levels()[0];
        assert_eq!(only.volume, 1400.0);
        assert_eq!(
            (only.min_exec, only.min_exec_buy, only.min_exec_sell),
            (1200.0, 1300.0, 1200.0)
        );
    }

    #[test]
    fn t_min_drops_passthrough_before_ranking() {
        // 100 stands 30s (S huge); 101 flashed once (T=0, dropped even first).
        let mut st = MagnetStore::new(MagnetsConfig {
            k: 5,
            t_min_ms: 1000,
            ..MagnetsConfig::default()
        });
        st.bootstrap([
            snap(0, 100.0, 300.0),
            snap(30_000, 100.0, 300.0),
            snap(30_100, 101.0, 500.0),
            snap(30_200, 102.0, 10.0),
        ]);
        let prices: Vec<f64> = st.levels().iter().map(|l| l.price).collect();
        assert!(
            !prices.contains(&101.0),
            "pass-through must not rank: {prices:?}"
        );
        assert_eq!(prices[0], 100.0);
        assert_eq!(st.levels()[0].rank, 1);
    }

    #[test]
    fn ranking_side_distance_and_score() {
        let mut st = MagnetStore::new(MagnetsConfig {
            k: 2,
            t_min_ms: 0,
            ..MagnetsConfig::default()
        });
        st.bootstrap([
            snap(0, 100.0, 100.0),
            snap(20_000, 100.0, 100.0), // V=400, T=25s -> S=10000
            snap(25_000, 103.0, 50.0),
            snap(30_000, 103.0, 50.0),
            snap(35_000, 103.0, 50.0), // V=300, T=10s -> S=3000
        ]);
        // Open = last theoretical = 103.
        let lv = st.levels();
        assert_eq!(lv.len(), 2);
        assert_eq!(lv[0].price, 100.0);
        assert_eq!(lv[0].distance_from_open, -3.0);
        assert_eq!(lv[0].side, MagnetSide::BelowOpen);
        assert_eq!(lv[1].side, MagnetSide::AtOpen);
        assert_eq!(lv[1].sequence_index, 2);
    }

    #[test]
    fn normalized_score_prefers_heavy_queue_over_long_stand() {
        // Raw V*T would rank 5165.5 first (988 x 43.7s); normalized 0.6/0.4
        // puts the heavy-queue zone (5169, 5159) on top.
        let aggs = vec![
            PriceAggregate {
                price: 5159.0,
                vol: 2529.0,
                dwell_ms: 12_000,
                first_n: 1,
                min_exec: 500.0,
                min_exec_buy: 500.0,
                min_exec_sell: 600.0,
            },
            PriceAggregate {
                price: 5165.5,
                vol: 988.0,
                dwell_ms: 43_700,
                first_n: 2,
                min_exec: 494.0,
                min_exec_buy: 494.0,
                min_exec_sell: 500.0,
            },
            PriceAggregate {
                price: 5169.0,
                vol: 2983.0,
                dwell_ms: 4_000,
                first_n: 3,
                min_exec: 493.0,
                min_exec_buy: 493.0,
                min_exec_sell: 600.0,
            },
        ];
        let cfg = MagnetsConfig {
            k: 3,
            t_min_ms: 0,
            min_sep: 0.0,
            ..MagnetsConfig::default()
        };
        let lv = select_magnets(&aggs, 5159.0, &cfg);
        let prices: Vec<f64> = lv.iter().map(|l| l.price).collect();
        assert_eq!(prices, vec![5169.0, 5159.0, 5165.5]);
        assert!(lv[0].score > lv[1].score && lv[1].score > lv[2].score);
    }

    #[test]
    fn separation_collapses_near_duplicates_into_stronger() {
        // 5158.0 and 5158.5 (one tick apart) never print together: the weaker
        // backfills nothing, the stronger keeps its slot.
        let aggs = vec![
            PriceAggregate {
                price: 5158.0,
                vol: 2050.0,
                dwell_ms: 8_000,
                first_n: 1,
                min_exec: 600.0,
                min_exec_buy: 600.0,
                min_exec_sell: 700.0,
            },
            PriceAggregate {
                price: 5158.5,
                vol: 1333.0,
                dwell_ms: 2_000,
                first_n: 2,
                min_exec: 400.0,
                min_exec_buy: 400.0,
                min_exec_sell: 450.0,
            },
            PriceAggregate {
                price: 5171.0,
                vol: 1026.0,
                dwell_ms: 21_000,
                first_n: 3,
                min_exec: 300.0,
                min_exec_buy: 300.0,
                min_exec_sell: 350.0,
            },
        ];
        let cfg = MagnetsConfig::default();
        let lv = select_magnets(&aggs, 5150.0, &cfg);
        let prices: Vec<f64> = lv.iter().map(|l| l.price).collect();
        assert_eq!(prices, vec![5158.0, 5171.0]);
        assert_eq!((lv[0].rank, lv[1].rank), (1, 2));
    }

    #[test]
    fn store_same_day_rollover_and_watermark() {
        let day1 = chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(11, 55, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day2 = day1 + 86_400_000;
        let mut st = MagnetStore::new(MagnetsConfig {
            k: 2,
            t_min_ms: 0,
            ..MagnetsConfig::default()
        });
        st.bootstrap([snap(day1, 100.0, 100.0), snap(day1 + 10_000, 101.0, 100.0)]);
        assert!(!st.is_empty());
        st.add_point(snap(day1 + 5_000, 100.0, 100.0));
        assert_eq!(st.events().len(), 2);
        st.add_point(snap(day2, 200.0, 100.0));
        st.add_point(snap(day2 + 5_000, 200.0, 100.0));
        assert_eq!(st.events().len(), 1);
        assert_eq!(st.events()[0].price, 200.0);
    }

    #[test]
    fn bootstrap_bumps_version_so_views_replot() {
        // Regression: `KlineChart::bootstrap_magnets` only clears the canvas
        // when `version` moves. A bootstrap that silently kept version at 0
        // left fetched history invisible until a manual toggle.
        let mut st = MagnetStore::new(MagnetsConfig {
            k: 2,
            t_min_ms: 0,
            ..MagnetsConfig::default()
        });
        assert_eq!(st.version(), 0);
        st.bootstrap([snap(0, 100.0, 100.0), snap(10_000, 101.0, 100.0)]);
        assert!(!st.is_empty());
        assert_ne!(st.version(), 0);
        // Re-bootstrapping identical history is a no-op for views.
        let v = st.version();
        st.bootstrap([snap(0, 100.0, 100.0), snap(10_000, 101.0, 100.0)]);
        assert_eq!(st.version(), v);
    }
}
