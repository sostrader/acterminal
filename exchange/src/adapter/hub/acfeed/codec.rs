//! Raw Zenoh payload parsing + price formatting.
//!
//! This is a near-verbatim port of ACTerminal's `zenoh_feed.rs` (see
//! `acterminal-tauri/src-tauri/src/zenoh_feed.rs`), which must stay logically identical
//! since both consume the same Acfeed-backed Zenoh feed. `invert_price`/`needs_inversion`/
//! `get_decimals` are reused as-is by both the real-time subscriber (`stream.rs`) and the
//! historical fetch worker (`fetch.rs`), since historical bars/ticks come back just as raw
//! as the live feed and need the identical inversion/rounding pass.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI8, AtomicI64, Ordering};

use serde::Deserialize;

use crate::depth::{DeOrder, DepthPayload};
use crate::unit::qty::Qty;
use crate::{Price, Trade, UnixMs};

// ── Symbol helpers (mirror ACTerminal's zenoh_feed.rs transformer) ────────────

/// These CME FX symbols report prices as the reciprocal (e.g. JPY/USD → USD/JPY).
/// "6LNEXT" is 6L's next contract month (see `symbols.rs`), same feed convention.
pub fn needs_inversion(symbol: &str) -> bool {
    matches!(symbol, "6J" | "6L" | "6LNEXT" | "6M" | "6C" | "6S")
}

pub fn get_decimals(symbol: &str) -> usize {
    match symbol {
        "6L" | "6LNEXT" => 2,
        "6J" => 2,
        "6M" | "6C" => 4,
        "6S" => 5,
        "WDO" | "WDONEXT" | "GC" => 1,
        "ES" => 2,
        _ => 5,
    }
}

pub fn get_tick_size_for_symbol(symbol: &str) -> f64 {
    match symbol {
        "6J" => 0.005,
        "6L" | "6LNEXT" => 0.5,
        "6M" => 0.00005,
        "6C" => 0.00005,
        "6S" => 0.00001,
        "WDO" | "WDONEXT" => 0.5,
        "GC" => 0.10,
        "ES" => 0.25,
        _ => 0.00001,
    }
}

/// The price grid an inverted symbol lands on after [`invert_price`].
///
/// `1/x` maps the venue's evenly-spaced raw ladder onto an uneven one, so `invert_price`
/// snaps each level to a fixed grid. That grid — not the underlying CME tick — is the
/// step everything downstream must use: the ladder rows, the heatmap bins and the
/// footprint bins. Returns `None` for directly-quoted products, whose step is just their
/// real tick.
///
/// Kept beside `invert_price` because the two must agree exactly; `symbols.rs` reads it
/// rather than restating the numbers, and a test pins that every inverted price is a
/// whole multiple of it.
pub fn inverted_display_grid(symbol: &str) -> Option<f64> {
    if !needs_inversion(symbol) {
        return None;
    }

    Some(match symbol {
        // Quoted in WDO points (per 1000), snapped to half a point.
        "6L" | "6LNEXT" => 0.5,
        "6J" => 0.01,
        "6S" => 0.00001,
        _ => 0.0001,
    })
}

pub fn invert_price(price: f64, symbol: &str) -> f64 {
    if price <= 0.0 {
        return 0.0;
    }
    let inv = 1.0 / price;
    match symbol {
        "6L" | "6LNEXT" => {
            let wdo = inv * 1000.0;
            (wdo / 0.5).round() * 0.5
        }
        "6J" => (inv * 100.0).round() / 100.0,
        "6S" => (inv * 100_000.0).round() / 100_000.0,
        _ => (inv * 10_000.0).round() / 10_000.0,
    }
}

/// Converts integer wire ticks into a display price.
///
/// The order of operations matters for the reciprocal-quoted products. 6J's raw prices
/// are ~0.0068xxxx, so rounding to 5 decimals *before* `1/x` collapses 0.006802 to
/// 0.00680 and the inverted price jumps several USDJPY cents (147.02 vs 147.06). DOM and
/// quote payloads arrive as full-precision floats and are inverted untouched, so a tape
/// built from pre-rounded ticks prints off its own bid/ask. Invert first, and let
/// `invert_price` do the only rounding, onto the grid the book actually sits on.
///
/// Directly-quoted FX keeps the historical 5-decimal convention, which is its native pip
/// grid.
fn ticks_to_display_price(ticks: i64, tick_size: f64, symbol: &str) -> f64 {
    let raw = ticks as f64 * tick_size;

    if !needs_inversion(symbol) {
        return (raw * 100_000.0).round() / 100_000.0;
    }

    let inverted = invert_price(raw, symbol);

    // `process_dom` publishes the inverted book on the instrument's grid, anchored on the
    // touch. `snap_to_bbo` has already pinned this print to the raw touch, so snapping
    // the inverted result to the same grid puts it exactly on the best bid or ask the
    // ladder and heatmap are drawing — without this the bubble floats a fraction of a
    // tick off its own book.
    super::symbols::find(symbol).map_or(inverted, |info| {
        Price::from_f64(inverted)
            .round_to_step(info.price_step())
            .to_f64()
    })
}

/// Snaps a print onto the side of the book it actually traded against.
///
/// An aggressor buy lifts the ask and an aggressor sell hits the bid; a passive print
/// rests on its own side. Done on the *raw* tick grid, before inversion, so that `1/x`
/// plus the side swap still lands exactly on the inverted BBO — which is what makes the
/// tape agree with the ladder for 6J and friends.
fn snap_to_bbo(side: &str, trade_ticks: i64, bid_ticks: i64, ask_ticks: i64) -> i64 {
    match side {
        "BUY_MARKET" | "SELL_PENDING" if ask_ticks > 0 => ask_ticks,
        "SELL_MARKET" | "BUY_PENDING" if bid_ticks > 0 => bid_ticks,
        _ => trade_ticks,
    }
}

fn invert_side(side: &str) -> &'static str {
    match side {
        "BUY_MARKET" => "SELL_MARKET",
        "SELL_MARKET" => "BUY_MARKET",
        "BUY_PENDING" => "SELL_PENDING",
        _ => "BUY_PENDING",
    }
}

fn deserialize_price<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum PriceOrStr {
        Num(f64),
        Str(String),
    }
    match PriceOrStr::deserialize(deserializer)? {
        PriceOrStr::Num(n) => Ok(n),
        PriceOrStr::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

// ── Raw tick types (wire format, identical to ACTerminal's zenoh_feed.rs) ─────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawTick {
    #[serde(rename = "t")]
    pub time: i64,
    #[serde(rename = "p")]
    pub price: i64,
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "b", default)]
    pub bid: Option<i64>,
    #[serde(rename = "a", default)]
    pub ask: Option<i64>,
    #[serde(rename = "bs", default)]
    pub bid_size: Option<f64>,
    #[serde(rename = "as", default)]
    pub ask_size: Option<f64>,
    #[serde(rename = "id")]
    pub tick_id: i64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawTickPacket {
    #[serde(rename = "bp")]
    pub base_price: i64,
    #[serde(rename = "bt")]
    pub base_timestamp: i64,
    #[serde(rename = "ts")]
    pub tick_size: f64,
    #[serde(rename = "tks")]
    pub ticks: Vec<RawTick>,
    #[serde(default)]
    pub eoh: bool,
}

#[derive(Debug, Deserialize)]
struct RawTickMessage {
    pub ticks: Vec<RawTickPacket>,
}

// ── Raw DOM types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawDomEntry {
    #[serde(deserialize_with = "deserialize_price")]
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RawDomData {
    pub timestamp: String,
    pub bids: Vec<RawDomEntry>,
    pub offers: Vec<RawDomEntry>,
}

#[derive(Debug, Deserialize)]
struct RawDomMessage {
    pub doms: Vec<RawDomData>,
}

// ── Raw Quote types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct RawQuoteEntry {
    #[serde(default)]
    pub price: f64,
}

#[derive(Debug, Deserialize, Default)]
struct RawQuoteEntries {
    #[serde(rename = "Bid")]
    pub bid: Option<RawQuoteEntry>,
    #[serde(rename = "Offer")]
    pub offer: Option<RawQuoteEntry>,
    #[serde(rename = "Trade")]
    pub trade: Option<RawQuoteEntry>,
    #[serde(rename = "OpeningPrice")]
    pub opening_price: Option<RawQuoteEntry>,
    #[serde(rename = "HighPrice")]
    pub high_price: Option<RawQuoteEntry>,
    #[serde(rename = "LowPrice")]
    pub low_price: Option<RawQuoteEntry>,
}

#[derive(Debug, Deserialize)]
struct RawQuoteData {
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub entries: RawQuoteEntries,
}

#[derive(Debug, Deserialize)]
struct RawQuoteMessage {
    pub quotes: Vec<RawQuoteData>,
}

// ── Raw Premarket types (auction/theoretical price, `premarket/<symbol>`) ──────

#[derive(Debug, Deserialize)]
struct RawBookLevel {
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub size: f64,
}

#[derive(Debug, Deserialize)]
struct RawPremarketEntry {
    #[serde(default)]
    pub timestamp: String,
    #[serde(deserialize_with = "deserialize_price")]
    pub price: f64,
    /// Auction book levels; summed into the level volume ("V" na tag).
    #[serde(default)]
    pub bids: Vec<RawBookLevel>,
    #[serde(default)]
    pub offers: Vec<RawBookLevel>,
}

#[derive(Debug, Deserialize)]
struct RawPremarketMessage {
    pub premarket: Vec<RawPremarketEntry>,
}

/// One auction-book level of a premarket snapshot, venue-space price + size.
/// Comparisons against the snapshot's theoretical price always happen in this
/// same space (see `process_premarket`), so inversion can never skew matching.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

/// The pre-open auction's theoretical price, off the venue's `premarket/` topic — see
/// `metatrader/EA_WDO_Premarket_Zenoh.mq5` (the only current publisher of this topic).
#[derive(Debug, Clone, PartialEq)]
pub struct PremarketPayload {
    pub time: UnixMs,
    pub price: Price,
    /// Book size resting exactly at this snapshot's price ("V" na tag do nível).
    pub size: f64,
    /// Summed size over ALL bid levels of this snapshot's auction book.
    pub bid_vol: f64,
    /// Summed size over ALL offer levels of this snapshot's auction book.
    pub ask_vol: f64,
    /// Bid size resting exactly at this snapshot's theoretical price.
    pub bid_at: f64,
    /// Offer size resting exactly at this snapshot's theoretical price.
    pub ask_at: f64,
    /// Full auction book of this snapshot (every level the venue sent — 5
    /// today, more in the future with no formula change). Venue-space prices,
    /// matching `price` pre-inversion; MAGNETS input.
    pub book_bids: Vec<BookLevel>,
    /// Full auction book, offer side (see `book_bids`).
    pub book_offers: Vec<BookLevel>,
}

/// Session statistics the venue publishes on the `quotes/` topic.
///
/// These are the exchange's own session values, which is why they're taken from the feed
/// rather than recomputed from the trades a pane happens to have seen: a pane opened
/// mid-session has no way to derive the session open, and its high/low would only cover
/// the window it was watching.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct QuotePayload {
    pub time: UnixMs,
    pub bid: Option<Price>,
    pub ask: Option<Price>,
    pub last: Option<Price>,
    pub open: Option<Price>,
    pub high: Option<Price>,
    pub low: Option<Price>,
}

impl QuotePayload {
    /// Session range (high - low), when both ends are known.
    pub fn range(&self) -> Option<Price> {
        match (self.high, self.low) {
            (Some(high), Some(low)) if high.units >= low.units => Some(high - low),
            _ => None,
        }
    }
}

// ── Tick-ordering state (per active symbol) ────────────────────────────────

/// Resettable per-symbol state used to infer trade aggressor side, exactly like
/// ACTerminal's `ZenohFeed` fields — must be reset on symbol change to avoid stale
/// ordering carrying over between subscriptions.
#[derive(Default)]
pub struct TickState {
    pub last_price: AtomicI64,
    pub last_bid: AtomicI64,
    pub last_ask: AtomicI64,
    pub last_dir: AtomicI8,
}

impl TickState {
    pub fn reset(&self) {
        self.last_price.store(0, Ordering::SeqCst);
        self.last_bid.store(0, Ordering::SeqCst);
        self.last_ask.store(0, Ordering::SeqCst);
        self.last_dir.store(0, Ordering::SeqCst);
    }
}

// ── Payload processors ─────────────────────────────────────────────────────

/// Process a raw tick payload into `Vec<Trade>`. Exact port of ACTerminal's
/// Absolute trade time from the packet's delta encoding.
///
/// Ticks arrive delta-encoded against the packet header: price is `bp + p` and time is
/// `bt + t`, in epoch milliseconds. `persist` decodes time exactly this way before
/// storing (`store_ticks` in `persist/src/ingest.rs`), so live and historical trades
/// must agree here or they bucket onto different candles — which shows up as a gap
/// opening between the backfilled series and each newly formed live candle.
///
/// Falls back to the local clock only when the packet carries no usable timestamp,
/// which would otherwise place the trade at the epoch.
fn tick_time(base_timestamp: i64, delta: i64) -> UnixMs {
    match base_timestamp.checked_add(delta) {
        Some(ms) if ms > 0 => UnixMs::new(ms as u64),
        _ => UnixMs::now(),
    }
}

/// Absolute time from a payload's ISO timestamp field.
///
/// DOM and quote payloads carry an RFC3339 string, which `persist` parses before storing
/// (`iso_to_ms` in `persist/src/ingest.rs`). Stamping these with the local clock instead
/// mixes two clocks into one chart: on a delayed feed the heatmap would sit at wall-clock
/// "now" while the trade bubbles drawn over it sit at venue time, minutes apart.
///
/// Falls back to the local clock when the field is missing or unparseable.
fn payload_time(timestamp: &str) -> UnixMs {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(timestamp) {
        let ms = dt.timestamp_millis();
        if ms > 0 {
            return UnixMs::new(ms as u64);
        }
    }

    // Some payloads carry plain epoch milliseconds rather than an ISO string.
    if let Ok(ms) = timestamp.parse::<i64>()
        && ms > 0
    {
        return UnixMs::new(ms as u64);
    }

    UnixMs::now()
}

/// `process_tick_message`/`process_ticks` (`zenoh_feed.rs`), targeting acterminal's
/// `Trade` domain type instead of Tauri's `dto::TickData`.
pub fn process_ticks(json: &str, symbol: &str, state: &TickState) -> Vec<Trade> {
    let msg: RawTickMessage = match serde_json::from_str(json) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("[TRADOVATE-TICK-PARSE] {}", e);
            return vec![];
        }
    };

    let invert = needs_inversion(symbol);
    let mut out = Vec::new();

    for packet in msg.ticks {
        if packet.eoh {
            continue;
        }

        let base_price = packet.base_price;
        let base_timestamp = packet.base_timestamp;
        let tick_size = packet.tick_size;

        let mut cur_ask_ticks = state.last_ask.load(Ordering::SeqCst);
        let mut cur_bid_ticks = state.last_bid.load(Ordering::SeqCst);

        for tk in packet.ticks {
            let trade_ticks = base_price + tk.price;

            if let Some(a) = tk.ask {
                cur_ask_ticks = base_price + a;
                state.last_ask.store(cur_ask_ticks, Ordering::SeqCst);
            }
            if let Some(b) = tk.bid {
                cur_bid_ticks = base_price + b;
                state.last_bid.store(cur_bid_ticks, Ordering::SeqCst);
            }

            let last_p = state.last_price.load(Ordering::SeqCst);
            let last_d = state.last_dir.load(Ordering::SeqCst);

            let (side_str, dir_code): (&str, i8) = if last_p > 0 {
                if trade_ticks > last_p {
                    ("BUY_MARKET", 1)
                } else if trade_ticks < last_p {
                    ("SELL_MARKET", -1)
                } else {
                    match last_d {
                        1 | 2 => ("BUY_PENDING", 2),
                        -1 | -2 => ("SELL_PENDING", -2),
                        _ => ("BUY_PENDING", 0),
                    }
                }
            } else if cur_ask_ticks > 0 && trade_ticks >= cur_ask_ticks {
                ("BUY_MARKET", 1)
            } else if cur_bid_ticks > 0 && trade_ticks <= cur_bid_ticks {
                ("SELL_MARKET", -1)
            } else {
                ("BUY_PENDING", 0)
            };

            state.last_price.store(trade_ticks, Ordering::SeqCst);
            state.last_dir.store(dir_code, Ordering::SeqCst);

            // Snap to the raw bid or ask, then convert. For an inverted symbol a
            // BUY_MARKET snaps to the raw ask and flips to SELL_MARKET, so it lands on
            // the inverted bid — the same swap `process_dom` applies to the book.
            let snap_ticks = snap_to_bbo(side_str, trade_ticks, cur_bid_ticks, cur_ask_ticks);
            let final_price = ticks_to_display_price(snap_ticks, tick_size, symbol);
            let final_side = if invert {
                invert_side(side_str)
            } else {
                side_str
            };

            out.push(Trade {
                time: tick_time(base_timestamp, tk.time),
                is_sell: final_side.starts_with("SELL"),
                price: Price::from_f64(final_price),
                qty: Qty::from_f64(tk.size as f64),
            });
        }
    }

    out
}

/// Process a raw DOM payload into a full-book `DepthPayload` snapshot. Exact port of
/// ACTerminal's `process_dom` — Zenoh DOM messages are self-consistent full snapshots
/// (no diff-sync protocol), so every update is replayed as `DepthUpdate::Snapshot`.
pub fn process_dom(json: &str, symbol: &str) -> Option<DepthPayload> {
    let msg: RawDomMessage = serde_json::from_str(json).ok()?;
    let raw = msg.doms.into_iter().next()?;

    let invert = needs_inversion(symbol);
    let step = super::symbols::find(symbol).map(|s| s.price_step());

    let map_entries = |entries: Vec<RawDomEntry>, is_bid: bool| -> Vec<DeOrder> {
        // Raw levels, ordered outward from the touch. Inversion reverses the ordering,
        // so sort explicitly rather than trusting the payload's order.
        let mut levels: Vec<(f64, f64)> = entries
            .into_iter()
            .filter(|e| e.size > 0.0 && e.price > 0.0)
            .map(|e| {
                let price = if invert {
                    invert_price(e.price, symbol)
                } else {
                    e.price
                };
                (price, e.size)
            })
            .collect();

        levels.sort_by(|a, b| {
            if is_bid {
                b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
            }
        });

        let (Some(step), false) = (step, levels.is_empty()) else {
            return levels
                .into_iter()
                .map(|(price, qty)| DeOrder { price, qty })
                .collect();
        };

        if invert {
            // Reciprocal quoting stretches the ladder unevenly: `1/x` maps a raw tick to
            // `tick/price²`, about 1.0-1.5 WDO points for a 6L tick — so consecutive
            // levels generally do NOT invert onto consecutive grid points, and several can
            // even collapse onto the same one. Snapping each to its nearest grid slot then
            // leaves the DOM/heatmap with holes (black stripes) or swallowed rows.
            //
            // To make the inverted book render as a contiguous ladder, each level keeps its
            // own resting size but is *published by rank*: level `n` out from the touch is
            // placed `n` steps away from the best level, on the instrument's grid. That
            // makes the prices a tidy arithmetic sequence (e.g. USDBRL always reads .00/.50
            // with no empty rows). The labelled price is then the grid anchor for the row,
            // not the level's exact reciprocal — the accepted trade-off for a non-striped
            // DOM.
            let touch = Price::from_f64(levels[0].0).round_to_step(step);

            let mut out: Vec<DeOrder> = levels
                .iter()
                .enumerate()
                .map(|(n, (_raw, qty))| {
                    let delta = if is_bid { -(n as i64) } else { n as i64 };
                    DeOrder {
                        price: touch.add_steps(delta, step).to_f64(),
                        qty: *qty,
                    }
                })
                .collect();

            if is_bid {
                // `Depth` expects bids ascending.
                out.reverse();
            }
            return out;
        }

        // Directly-quoted products already come laid out on their own grid, so each level
        // keeps its own price snapped to the step. Merging rather than overwriting so a
        // payload that maps two raw levels onto one grid slot adds up, instead of one
        // silently replacing the other in `Depth::diff_price_levels`.
        let mut merged: BTreeMap<Price, f64> = BTreeMap::new();
        for (price, qty) in levels {
            *merged
                .entry(Price::from_f64(price).round_to_step(step))
                .or_insert(0.0) += qty;
        }

        let mut out: Vec<DeOrder> = merged
            .into_iter()
            .map(|(price, qty)| DeOrder {
                price: price.to_f64(),
                qty,
            })
            .collect();

        if is_bid {
            // `Depth` expects bids ascending.
            out.reverse();
        }
        out
    };

    // When inverted, Zenoh's bids/offers swap roles (buy/sell semantics flip under 1/x).
    let (bids, asks) = if invert {
        (map_entries(raw.offers, true), map_entries(raw.bids, false))
    } else {
        (map_entries(raw.bids, true), map_entries(raw.offers, false))
    };

    Some(DepthPayload {
        last_update_id: 0,
        time: payload_time(&raw.timestamp),
        bids,
        asks,
    })
}

/// Process a raw Quote payload into a [`QuotePayload`]. Exact port of ACTerminal's
/// `process_quote`: under inversion the venue's bid and offer swap roles (buy/sell
/// semantics flip under `1/x`), and every price goes through the same rounding grid as
/// ticks and depth do.
pub fn process_quote(json: &str, symbol: &str) -> Option<QuotePayload> {
    let msg: RawQuoteMessage = match serde_json::from_str(json) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("[TRADOVATE] quote parse failed: {e}");
            return None;
        }
    };

    let quote = msg.quotes.into_iter().next()?;
    let time = payload_time(&quote.timestamp);
    let entries = quote.entries;
    let invert = needs_inversion(symbol);

    let price = |entry: Option<&RawQuoteEntry>| -> Option<Price> {
        let raw = entry?.price;
        if raw <= 0.0 {
            return None;
        }
        Some(Price::from_f64(if invert {
            invert_price(raw, symbol)
        } else {
            raw
        }))
    };

    let (bid_src, ask_src) = if invert {
        (entries.offer.as_ref(), entries.bid.as_ref())
    } else {
        (entries.bid.as_ref(), entries.offer.as_ref())
    };

    // Under inversion the reciprocal also flips the extremes: the session's lowest
    // JPY/USD print is its highest USD/JPY print.
    let (high_src, low_src) = if invert {
        (entries.low_price.as_ref(), entries.high_price.as_ref())
    } else {
        (entries.high_price.as_ref(), entries.low_price.as_ref())
    };

    Some(QuotePayload {
        time,
        bid: price(bid_src),
        ask: price(ask_src),
        last: price(entries.trade.as_ref()),
        open: price(entries.opening_price.as_ref()),
        high: price(high_src),
        low: price(low_src),
    })
}

/// Process a raw premarket payload into a [`PremarketPayload`]. The bridge already
/// publishes an absolute price (no relative-tick encoding, unlike `ticks/`), so this is
/// just inversion + rounding onto the symbol's grid — same treatment as `process_quote`.
/// Kept for the live `premarket/` topic; the app itself only uses fetch (`hist/premarket`).
#[allow(dead_code)]
pub fn process_premarket(json: &str, symbol: &str) -> Option<PremarketPayload> {
    let msg: RawPremarketMessage = match serde_json::from_str(json) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("[TRADOVATE] premarket parse failed: {e}");
            return None;
        }
    };

    let entry = msg.premarket.into_iter().next()?;
    if entry.price <= 0.0 {
        return None;
    }

    let price = if needs_inversion(symbol) {
        invert_price(entry.price, symbol)
    } else {
        entry.price
    };

    let size: f64 = entry
        .bids
        .iter()
        .chain(entry.offers.iter())
        .filter(|lvl| (lvl.price - entry.price).abs() < 1e-9)
        .map(|lvl| lvl.size.max(0.0))
        .sum();
    // Full-book totals per side: summed over every level, not just the queue
    // at the theoretical price.
    let bid_vol: f64 = entry.bids.iter().map(|lvl| lvl.size.max(0.0)).sum();
    let ask_vol: f64 = entry.offers.iter().map(|lvl| lvl.size.max(0.0)).sum();
    // At-price per-side queues — EXACTLY the premarket indicator's "V" basis,
    // split by side (Theoric Targets input: sell = bid_at, buy = ask_at).
    let at_price = |levels: &[RawBookLevel]| -> f64 {
        levels
            .iter()
            .filter(|lvl| (lvl.price - entry.price).abs() < 1e-9)
            .map(|lvl| lvl.size.max(0.0))
            .sum()
    };
    let bid_at = at_price(&entry.bids);
    let ask_at = at_price(&entry.offers);
    // Full book, venue-space (same space as `entry.price`, so MAGNETS matching
    // never crosses an inversion boundary). Every level the venue sent.
    let to_book = |levels: &[RawBookLevel]| -> Vec<BookLevel> {
        levels
            .iter()
            .map(|lvl| BookLevel {
                price: lvl.price,
                size: lvl.size,
            })
            .collect()
    };

    Some(PremarketPayload {
        time: payload_time(&entry.timestamp),
        price: Price::from_f64(price),
        size,
        bid_vol,
        ask_vol,
        bid_at,
        ask_at,
        book_bids: to_book(&entry.bids),
        book_offers: to_book(&entry.offers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inversion_flags_match_acterminal() {
        for sym in ["6J", "6L", "6M", "6C", "6S"] {
            assert!(needs_inversion(sym), "{sym} should invert");
        }
        for sym in ["6E", "6B", "6A", "6N"] {
            assert!(!needs_inversion(sym), "{sym} should not invert");
        }
    }

    #[test]
    fn invert_price_6j_rounds_to_cent_grid() {
        // 1 USD/JPY raw quote of e.g. 0.0066667 (JPY/USD) inverts to ~150.00
        let inverted = invert_price(0.006_666_7, "6J");
        assert!((inverted - 150.0).abs() < 0.02, "got {inverted}");
    }

    #[test]
    fn invert_price_6l_rounds_to_half_grid() {
        let inverted = invert_price(0.2, "6L");
        // 1/0.2 * 1000 = 5000, rounded to nearest 0.5 -> 5000.0
        assert_eq!(inverted, 5000.0);
    }

    #[test]
    fn process_ticks_non_inverted_6e() {
        let json = r#"{"ticks":[{"bp":110000,"bt":0,"ts":0.00001,"tks":[{"t":0,"p":5,"s":2,"id":1}],"eoh":false}]}"#;
        let state = TickState::default();
        let trades = process_ticks(json, "6E", &state);
        assert_eq!(trades.len(), 1);
        // (110000+5) * 0.00001 = 1.10005
        assert!((trades[0].price.to_f64() - 1.10005).abs() < 1e-6);
        assert_eq!(trades[0].qty.to_f64(), 2.0);
    }

    #[test]
    fn live_trades_carry_the_venue_timestamp_not_the_local_clock() {
        // `bt + t`, the same decode `persist/src/ingest.rs::store_ticks` applies before
        // writing `ts`. Stamping these with the local clock instead put live candles on
        // different buckets than the backfill, opening a gap at the live edge.
        let base_ts: i64 = 1_772_000_000_000;
        let json = format!(
            r#"{{"ticks":[{{"bp":110000,"bt":{base_ts},"ts":0.00001,"tks":[
                {{"t":0,"p":5,"s":2,"id":1}},
                {{"t":4500,"p":6,"s":1,"id":2}}
            ],"eoh":false}}]}}"#
        );

        let state = TickState::default();
        let trades = process_ticks(&json, "6E", &state);

        assert_eq!(trades.len(), 2);
        assert_eq!(trades[0].time.as_u64(), base_ts as u64);
        assert_eq!(trades[1].time.as_u64(), base_ts as u64 + 4_500);
    }

    #[test]
    fn a_packet_without_a_timestamp_falls_back_to_the_clock() {
        // The epoch is never a real trade time; landing there would drag the whole
        // series back to 1970 rather than just misplacing one candle.
        let before = UnixMs::now().as_u64();
        let json = r#"{"ticks":[{"bp":110000,"bt":0,"ts":0.00001,"tks":[{"t":0,"p":5,"s":2,"id":1}],"eoh":false}]}"#;

        let trades = process_ticks(json, "6E", &TickState::default());

        assert_eq!(trades.len(), 1);
        assert!(
            trades[0].time.as_u64() >= before,
            "expected a wall-clock fallback, got {}",
            trades[0].time.as_u64()
        );
    }

    #[test]
    fn process_dom_swaps_sides_when_inverted() {
        let json = r#"{"doms":[{"timestamp":"0","bids":[{"price":0.15,"size":10.0}],"offers":[{"price":0.16,"size":5.0}]}]}"#;
        // symbol "6M" (invert=true): raw "bids" become processed asks, raw "offers" become processed bids.
        let payload = process_dom(json, "6M").expect("parses");
        assert_eq!(payload.bids.len(), 1);
        assert_eq!(payload.asks.len(), 1);
        // raw offers (0.16) -> inverted -> processed bids
        assert!((payload.bids[0].price - invert_price(0.16, "6M")).abs() < 1e-9);
        // raw bids (0.15) -> inverted -> processed asks
        assert!((payload.asks[0].price - invert_price(0.15, "6M")).abs() < 1e-9);
    }

    /// Levels that used to collide on the grid — and lose size to
    /// `Depth::diff_price_levels`'s overwrite — now each get their own row, because the
    /// inverted book is published on the grid by rank rather than by arithmetic price.
    #[test]
    fn inverted_levels_each_keep_their_own_row_and_size() {
        // Three 6L levels one raw tick apart; two of them invert onto the same 0.50 point.
        assert_eq!(invert_price(0.185, "6L"), invert_price(0.185_005, "6L"));

        let json = r#"{"doms":[{"timestamp":"0","bids":[],"offers":[
            {"price":0.185,"size":10.0},
            {"price":0.185005,"size":4.0},
            {"price":0.18501,"size":7.0}
        ]}]}"#;

        // Inverted: raw offers become the processed bids.
        let payload = process_dom(json, "6L").expect("parses");

        assert_eq!(payload.bids.len(), 3, "no level may be swallowed");
        assert_eq!(
            payload.bids.iter().map(|o| o.qty).sum::<f64>(),
            21.0,
            "no resting size may be dropped"
        );

        // Every size survives intact rather than being merged into a neighbour.
        let mut sizes: Vec<f64> = payload.bids.iter().map(|o| o.qty).collect();
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(sizes, vec![4.0, 7.0, 10.0]);

        // Prices are contiguous on the 0.50 grid, so nothing renders as a hole.
        let mut prices: Vec<f64> = payload.bids.iter().map(|o| o.price).collect();
        prices.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for pair in prices.windows(2) {
            assert!(
                (pair[1] - pair[0] - 0.5).abs() < 1e-6,
                "expected a 0.50 step, got {pair:?}"
            );
        }
        assert!(prices.iter().all(|p| (p * 2.0).fract().abs() < 1e-6));
    }

    /// The heatmap draws one row per grid step, so a book whose levels skip steps renders
    /// as black stripes. 6J's levels land ~0.0126 apart on a 0.01 grid; after re-gridding
    /// they must be exactly one step apart.
    #[test]
    fn inverted_books_leave_no_gap_for_the_heatmap_to_stripe() {
        let raw: Vec<String> = (0..10)
            .map(|i| {
                format!(
                    r#"{{"price":{:.7},"size":100.0}}"#,
                    0.0063135 + f64::from(i) * 0.0000005
                )
            })
            .collect();
        let json = format!(
            r#"{{"doms":[{{"timestamp":"0","bids":[{}],"offers":[]}}]}}"#,
            raw.join(",")
        );

        let payload = process_dom(&json, "6J").expect("parses");
        assert_eq!(payload.asks.len(), 10, "all ten levels survive");

        let mut prices: Vec<f64> = payload.asks.iter().map(|o| o.price).collect();
        prices.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for pair in prices.windows(2) {
            assert!(
                (pair[1] - pair[0] - 0.01).abs() < 1e-6,
                "expected a 0.01 step, got {pair:?}"
            );
        }
    }

    #[test]
    fn direct_pairs_keep_every_level_untouched() {
        // On 6E's half-pip grid these are three distinct levels.
        let json = r#"{"doms":[{"timestamp":"0","bids":[
            {"price":1.08420,"size":3.0},
            {"price":1.08415,"size":4.0}
        ],"offers":[{"price":1.08425,"size":5.0}]}]}"#;

        let payload = process_dom(json, "6E").expect("parses");
        assert_eq!(payload.bids.len(), 2);
        assert_eq!(payload.asks.len(), 1);
        assert_eq!(payload.bids.iter().map(|o| o.qty).sum::<f64>(), 7.0);
    }

    /// A price between two grid points is not a level the venue can quote, so it belongs
    /// to its nearest one rather than becoming a row of its own.
    #[test]
    fn off_grid_levels_snap_onto_the_step_and_merge() {
        let json = r#"{"doms":[{"timestamp":"0","bids":[
            {"price":1.08420,"size":3.0},
            {"price":1.08419,"size":4.0}
        ],"offers":[]}]}"#;

        let payload = process_dom(json, "6E").expect("parses");
        assert_eq!(payload.bids.len(), 1);
        assert_eq!(payload.bids[0].qty, 7.0);
    }

    #[test]
    fn process_quote_reads_session_ohl_for_a_direct_pair() {
        let json = r#"{"quotes":[{"entries":{
            "Bid":{"price":1.08420},
            "Offer":{"price":1.08425},
            "Trade":{"price":1.08422},
            "OpeningPrice":{"price":1.08100},
            "HighPrice":{"price":1.08900},
            "LowPrice":{"price":1.08000}
        }}]}"#;

        let quote = process_quote(json, "6E").expect("quote parses");
        assert_eq!(quote.bid, Some(Price::from_f64(1.084_20)));
        assert_eq!(quote.ask, Some(Price::from_f64(1.084_25)));
        assert_eq!(quote.open, Some(Price::from_f64(1.081)));
        assert_eq!(quote.high, Some(Price::from_f64(1.089)));
        assert_eq!(quote.low, Some(Price::from_f64(1.08)));
        assert_eq!(quote.range(), Some(Price::from_f64(0.009)));
    }

    #[test]
    fn process_quote_flips_sides_and_extremes_when_inverted() {
        // JPY/USD raw quotes: the lowest raw price is the highest USD/JPY price.
        let json = r#"{"quotes":[{"entries":{
            "Bid":{"price":0.0066667},
            "Offer":{"price":0.0066677},
            "HighPrice":{"price":0.0066700},
            "LowPrice":{"price":0.0066600}
        }}]}"#;

        let quote = process_quote(json, "6J").expect("quote parses");

        // Raw offer becomes the displayed bid, raw bid becomes the displayed ask.
        assert_eq!(
            quote.bid,
            Some(Price::from_f64(invert_price(0.0066677, "6J")))
        );
        assert_eq!(
            quote.ask,
            Some(Price::from_f64(invert_price(0.0066667, "6J")))
        );

        // Raw low -> displayed high, and the range stays non-negative.
        assert_eq!(
            quote.high,
            Some(Price::from_f64(invert_price(0.00666, "6J")))
        );
        assert_eq!(
            quote.low,
            Some(Price::from_f64(invert_price(0.00667, "6J")))
        );
        assert!(quote.high.unwrap() >= quote.low.unwrap());
        assert!(quote.range().is_some());
    }

    #[test]
    fn process_quote_drops_missing_and_non_positive_entries() {
        let json = r#"{"quotes":[{"entries":{"Bid":{"price":0.0},"Trade":{"price":1.08}}}]}"#;
        let quote = process_quote(json, "6E").expect("quote parses");
        assert_eq!(quote.bid, None);
        assert_eq!(quote.ask, None);
        assert_eq!(quote.last, Some(Price::from_f64(1.08)));
        assert_eq!(quote.range(), None);
    }
}
