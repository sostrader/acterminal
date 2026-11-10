//! Historical data via the Zenoh queryable `hist/{tick,dom,quote}/<product>`, served by
//! `persist` (see `persist/src/query.rs`). Deliberately NOT `fetch/**` — that's a
//! separate, pre-existing convention owned by the `history` service (on-demand,
//! per-query Acfeed WebSocket fetch, no cache, doesn't scale with consumer count;
//! see `.agents/skills/acfeed/references/fetching.md`). `persist`'s `hist/**` is the
//! opposite: served only from its local DuckDB cache, safe for many concurrent readers.
//!
//! `persist` intentionally does **not** serve pre-bucketed OHLC — it's a thin,
//! horizontally-scalable read path over raw ticks only, so candles must always be
//! formed from ticks. Historical klines are therefore built here by fetching raw ticks
//! (`fetch_klines` below) and folding them into [`Kline`]s via
//! [`aggregate_trades_to_klines`], which reuses `stream.rs`'s
//! [`super::stream::KlineBuilder`] — the exact same bucketing
//! (`UnixMs::floor_to`/`Timeframe`) the live kline stream uses — so historical and live
//! candles land on identical bucket boundaries rather than a second, possibly-diverging
//! aggregation scheme.
//!
//! `persist` only retains [`PERSIST_RETENTION_DAYS`] worth of ticks (see
//! `persist/src/config.rs`'s `RETENTION_DAYS`), so there's no need to compute a precise
//! date range here — every request just asks for the full retention window and takes
//! whatever `persist` actually has.
//!
//! Contract: a Zenoh `get()` against `hist/tick/<product>?days=N` returns **one** JSON
//! reply of the shape `{"product": .., "ticks": [...]}` once the fetch completes (no
//! partial/streaming replies).
//!
//! # Tick reply shape
//!
//! Confirmed field-for-field against `persist/src/query.rs::handle_ticks`:
//! ```json
//! {"product": "6E", "ticks": [
//!   {"timestamp": "2026-08-05T12:00:00.000Z", "price": 1.1000, "size": 2,
//!    "bid": 1.0999, "ask": 1.1001, "bidSize": 3, "askSize": 5, "id": 123}
//! ]}
//! ```
//! `bidSize`/`askSize`/`id` are ignored client-side. There is no explicit aggressor-side
//! field on the wire (persist only ever emits raw ticks) — side is inferred from price
//! vs. `bid`/`ask` (and the previous trade once established), porting the exact decision
//! logic the live feed uses in `codec::process_ticks`/`TickState`, so historical and live
//! trades classify buy/sell identically. See [`SideState`]/[`classify_side`].
//!
//! Historical ticks are just as raw as the real-time feed, so every price is run through
//! `codec`'s `invert_price`/`needs_inversion` — same rationale as the live path (see
//! module docs on `codec.rs`).

use crate::{
    Kline, Price, TickerInfo, Timeframe, Trade, UnixMs,
    adapter::AdapterError,
    unit::{PriceStep, qty::Qty},
};

use super::AcfeedWorker;
use super::codec::{self, invert_price, needs_inversion};

use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use zenoh::Session;

const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
/// Matches `persist`'s default `RETENTION_DAYS` (`persist/src/config.rs`) — requesting
/// more than this is pointless since `persist` never has more than this much history,
/// so every fetch just asks for the full window and takes whatever's actually there.
const PERSIST_RETENTION_DAYS: u64 = 10;
/// Cap on ticks per page for a trade backfill (footprint/time&sales panes), instead of
/// pulling the full retention window in one reply, which is huge for liquid products.
/// The batched fetcher (`fetch_trades_batched` in `src/connector/fetcher.rs`) pages
/// backward with `before` when one page isn't enough to reach `from_time`.
const TRADE_FETCH_LIMIT: usize = 1000;

/// Cap on ticks per page for a kline backfill. `persist` answers `hist/tick`
/// newest-first capped at its own `MAX_REPLY_ROWS`, so a single query only ever
/// covers the most recent slice of the window: on a dense product (WDO ticks on
/// every MT5 quote change) that slice is a couple of hours, and the candle +
/// comparison charts render a truncated day while thinner products (6L) come
/// back whole. Paging backward with `before` (same pattern as the trade
/// backfill in `src/connector/fetcher.rs`) fixes that at the source.
const KLINE_FETCH_PAGE_LIMIT: usize = 15000;
/// Safety cap on pages per kline fetch. A fresh pane pages back to the start of
/// the current UTC day (a B3 session is ~40k ticks, a few pages); this cap keeps
/// a pathological window (multi-day gap walk, tiny `MAX_REPLY_ROWS` server-side)
/// from pulling unbounded history. The chart's priority-1 backfill keeps filling
/// further left on scroll.
const KLINE_FETCH_MAX_PAGES: usize = 12;

/// Start of the current UTC day, in epoch ms. A fresh pane (no `from` bound) has
/// no visible window yet, so "the day" is the window it pages back to: the full
/// session for B3 products, the evening + day sessions for CME FX. Pure half,
/// split out so it can be tested without the clock.
fn utc_day_start_ms(now_ms: u64) -> u64 {
    now_ms - (now_ms % DAY_MS)
}

/// Selector for one kline-fetch page. Zenoh separates selector parameters with
/// `;`, not `&` (see `fetch_trades`); `before` is omitted on the first page so
/// `persist` answers with its newest ticks.
fn kline_page_selector(symbol: &str, days: u64, before_ms: Option<u64>) -> String {
    match before_ms {
        Some(before) => {
            format!("hist/tick/{symbol}?days={days};limit={KLINE_FETCH_PAGE_LIMIT};before={before}")
        }
        None => format!("hist/tick/{symbol}?days={days};limit={KLINE_FETCH_PAGE_LIMIT}"),
    }
}

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Whole days of history to ask `persist` for in order to cover back to `from`.
///
/// `persist` only exposes a coarse `?days=N` filter, so this rounds up and clamps to the
/// retention window. Asking for the full window on every request (which is what this
/// used to do) means transferring days of raw ticks just to render a few hours of
/// candles — slow enough on a liquid product to leave a visible hole between the
/// backfill and the live feed while the reply is still in flight.
fn days_to_cover(from: UnixMs) -> u64 {
    let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
    days_for_span(now.saturating_sub(from.as_u64()))
}

/// Pure half of [`days_to_cover`], split out so it can be tested without the clock.
fn days_for_span(span_ms: u64) -> u64 {
    span_ms.div_ceil(DAY_MS).clamp(1, PERSIST_RETENTION_DAYS)
}

fn deserialize_num_or_str<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
    }
    match NumOrStr::deserialize(deserializer)? {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

fn deserialize_num_or_str_opt<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStrOpt {
        Num(f64),
        Str(String),
        None,
    }
    match Option::<NumOrStrOpt>::deserialize(deserializer)? {
        Some(NumOrStrOpt::Num(n)) => Ok(Some(n)),
        Some(NumOrStrOpt::Str(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
        Some(NumOrStrOpt::None) | None => Ok(None),
    }
}

/// Epoch time as either a number (assumed ms, falling back to seconds-heuristic in
/// [`to_unix_ms`]) or a string (numeric-in-string, or RFC3339).
fn deserialize_time<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum TimeOrStr {
        Num(i64),
        Str(String),
    }
    match TimeOrStr::deserialize(deserializer)? {
        TimeOrStr::Num(n) => Ok(n),
        TimeOrStr::Str(s) => {
            if let Ok(n) = s.parse::<i64>() {
                return Ok(n);
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s) {
                return Ok(dt.timestamp_millis());
            }
            // The live `history` service emits minute-precision timestamps without
            // seconds (e.g. "2026-07-30T22:00Z"), which isn't valid RFC3339 (seconds
            // are mandatory there) — handle that shape explicitly, plus a couple of
            // nearby variants defensively in case bar_size/tick timestamps differ.
            const FORMATS: &[&str] = &[
                "%Y-%m-%dT%H:%MZ",
                "%Y-%m-%dT%H:%M:%SZ",
                "%Y-%m-%dT%H:%M:%S%.fZ",
            ];
            for fmt in FORMATS {
                if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&s, fmt) {
                    return Ok(dt.and_utc().timestamp_millis());
                }
            }
            Err(serde::de::Error::custom(format!(
                "unparseable historical timestamp: {s}"
            )))
        }
    }
}

/// Heuristic ms/seconds disambiguation: epoch-seconds for "now" is ~1.7e9, epoch-ms is
/// ~1.7e12 — anything below 10^12 is assumed to be seconds.
fn to_unix_ms(raw: i64) -> UnixMs {
    const MS_THRESHOLD: i64 = 10_000_000_000; // 10^10
    if raw.unsigned_abs() < MS_THRESHOLD as u64 {
        UnixMs::from_seconds_saturating(raw.max(0) as u64)
    } else {
        UnixMs::new(raw.max(0) as u64)
    }
}

/// One historical tick, matching `persist/src/query.rs::handle_ticks`'s reply
/// field-for-field. `bid`/`ask` are nullable (a tick may lack a book snapshot) and feed
/// [`classify_side`]; `bidSize`/`askSize`/`id` are sent by persist but unused here.
#[derive(Debug, Deserialize)]
struct RawHistTick {
    #[serde(
        alias = "t",
        alias = "timestamp",
        deserialize_with = "deserialize_time"
    )]
    time: i64,
    #[serde(alias = "p", deserialize_with = "deserialize_num_or_str")]
    price: f64,
    #[serde(
        alias = "s",
        alias = "qty",
        alias = "quantity",
        default,
        deserialize_with = "deserialize_num_or_str_opt"
    )]
    size: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_num_or_str_opt")]
    bid: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_num_or_str_opt")]
    ask: Option<f64>,
}

#[derive(Debug, Deserialize, Default)]
struct TickReply {
    #[serde(default)]
    #[allow(dead_code)]
    product: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    symbol: Option<String>,
    #[serde(default)]
    ticks: Vec<RawHistTick>,
}

/// Running state for [`classify_side`], one instance per (chronologically-ordered)
/// batch of historical ticks for a single product. Ports `codec::TickState`/
/// `process_ticks`'s live aggressor-classification exactly (see that module's doc
/// comment), just without the atomics since historical ticks are folded sequentially
/// in a single pass rather than concurrently streamed.
#[derive(Default)]
struct SideState {
    last_price: f64,
    last_dir: i8,
}

/// Classifies one tick's aggressor side (`true` = sell) the same way the live feed does:
/// compare against the previous trade price first (established once a trade has
/// printed), falling back to price-vs-bid/ask for the very first tick in the batch.
/// Returns `(is_sell, is_market)`; `is_market` distinguishes an aggressor print from a
/// passive one, which is what decides the side of the book it snaps to.
fn classify_side(
    price: f64,
    bid: Option<f64>,
    ask: Option<f64>,
    state: &mut SideState,
) -> (bool, bool) {
    let (is_sell, dir): (bool, i8) = if state.last_price > 0.0 {
        if price > state.last_price {
            (false, 1)
        } else if price < state.last_price {
            (true, -1)
        } else {
            match state.last_dir {
                1 | 2 => (false, 2),
                -1 | -2 => (true, -2),
                _ => (false, 0),
            }
        }
    } else if let Some(ask) = ask.filter(|a| *a > 0.0)
        && price >= ask
    {
        (false, 1)
    } else if let Some(bid) = bid.filter(|b| *b > 0.0)
        && price <= bid
    {
        (true, -1)
    } else {
        (false, 0)
    };

    state.last_price = price;
    state.last_dir = dir;

    (is_sell, matches!(dir, 1 | -1))
}

/// Historical twin of `codec::snap_to_bbo`, on the raw floats persist sends rather than
/// integer wire ticks. Same rule, and it has to stay the same rule: a backfilled tape
/// that sits between grid points while the live one sits on the BBO makes the footprint
/// disagree with itself across the join.
fn snap_to_bbo(
    is_sell: bool,
    is_market: bool,
    price: f64,
    bid: Option<f64>,
    ask: Option<f64>,
) -> f64 {
    let use_ask = is_sell != is_market;

    let target = if use_ask { ask } else { bid };
    target.filter(|p| *p > 0.0).unwrap_or(price)
}

/// Converts a chronologically-ordered (oldest -> newest) slice of raw ticks into
/// [`Trade`]s, running [`classify_side`] sequentially so aggressor-side inference sees
/// each tick's real predecessor.
fn ticks_to_trades(ticks: &[RawHistTick], symbol: &str) -> Vec<Trade> {
    let invert = needs_inversion(symbol);
    let mut state = SideState::default();

    ticks
        .iter()
        .map(|tick| {
            // Side classification runs on raw (pre-inversion) price/bid/ask, matching
            // the live feed's order of operations in `codec::process_ticks`.
            let (is_sell_raw, is_market) =
                classify_side(tick.price, tick.bid, tick.ask, &mut state);

            // Snap on the raw grid, before inversion — see `codec::snap_to_bbo`.
            let snapped = snap_to_bbo(is_sell_raw, is_market, tick.price, tick.bid, tick.ask);
            let price = if invert {
                invert_price(snapped, symbol)
            } else {
                snapped
            };
            // Inversion flips aggressor side (`codec::invert_side`) since the
            // reciprocal price transform reverses "up"/"down" — same rule the live
            // path applies.
            let is_sell = is_sell_raw ^ invert;

            Trade {
                time: to_unix_ms(tick.time),
                is_sell,
                price: Price::from_f64(price),
                qty: Qty::from_f64(tick.size.unwrap_or(0.0)),
            }
        })
        .collect()
}

/// Folds already-ordered (oldest -> newest) trades into OHLCV klines using
/// [`super::stream::KlineBuilder`] — the exact bucketing (`UnixMs::floor_to`) the live
/// kline stream uses — so a historical backfill and the live feed's in-progress candle
/// always land on identical bucket boundaries.
fn aggregate_trades_to_klines(
    trades: &[Trade],
    timeframe: Timeframe,
    min_ticksize: PriceStep,
) -> Vec<Kline> {
    let mut builder = super::stream::KlineBuilder::new(timeframe, min_ticksize);
    let mut klines: Vec<Kline> = Vec::new();

    for trade in trades {
        let kline = builder.update(trade.time, trade.price, trade.qty);
        match klines.last_mut() {
            Some(last) if last.time == kline.time => *last = kline,
            _ => klines.push(kline),
        }
    }

    klines
}

async fn query_once(session: &Session, selector: &str) -> Result<String, AdapterError> {
    let replies = session
        .get(selector)
        .timeout(QUERY_TIMEOUT)
        .await
        .map_err(|e| {
            AdapterError::WebsocketError(format!("Zenoh query failed for {selector}: {e}"))
        })?;

    // The `hist/*` queryable contract always replies with exactly one JSON payload
    // (no partial/streaming replies), so only the first reply is ever meaningful.
    match replies.recv_async().await {
        Ok(reply) => match reply.result() {
            Ok(sample) => {
                let bytes = sample.payload().to_bytes();
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            Err(e) => Err(AdapterError::WebsocketError(format!(
                "Zenoh reply error for {selector}: {e}"
            ))),
        },
        Err(_) => Err(AdapterError::WebsocketError(format!(
            "No Zenoh reply received for {selector}"
        ))),
    }
}

/// Runs a query, reconnecting the worker's session once and retrying on failure (the
/// session may have gone stale between fetches — Zenoh sessions aren't as
/// self-healing as a subscription loop that's constantly polling).
async fn query_with_retry(
    worker: &mut AcfeedWorker,
    selector: &str,
) -> Result<String, AdapterError> {
    worker.ensure_session().await?;
    match query_once(worker.session()?, selector).await {
        Ok(body) => Ok(body),
        Err(first_err) => {
            log::warn!(
                "[TRADOVATE-FETCH] query failed ({first_err}), reconnecting and retrying once"
            );
            worker.invalidate_session();
            worker.ensure_session().await?;
            query_once(worker.session()?, selector).await
        }
    }
}

/// Historical candles no longer come from a server-side `fetch/ohlc` endpoint — persist
/// only ever serves raw ticks (see module docs) — so this fetches ticks over `persist`
/// and aggregates them client-side via [`aggregate_trades_to_klines`].
///
/// Ticks are pulled in pages, newest first, walking `before` backward: a single
/// `hist/tick` reply is capped at `persist`'s `MAX_REPLY_ROWS` newest ticks, which on
/// a dense product covers only the tail of the window (WDO's afternoon instead of
/// its whole session). Paging continues until the window is covered:
/// - bounded backfill (`Some((from, _))`, e.g. scroll-left fill): until the oldest
///   tick reaches `from`;
/// - fresh pane (`None`, nothing on screen yet): until the oldest tick reaches the
///   start of the current UTC day, so candle + comparison charts open on the full
///   session instead of the last couple of hours.
///
/// Pages are concatenated oldest -> newest and aggregated in one pass, so bucket
/// boundaries match the live feed exactly as before. The end bound is never used:
/// `persist` naturally has nothing newer than "now" to ask for.
pub(super) async fn fetch_klines(
    worker: &mut AcfeedWorker,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    range: Option<(UnixMs, UnixMs)>,
) -> Result<Vec<Kline>, AdapterError> {
    let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();

    let days = range.map_or(PERSIST_RETENTION_DAYS, |(from, _)| days_to_cover(from));
    // Bounded backfills stop at `from`; a fresh pane stops at today's open (UTC),
    // which still resolves to one page when the market is closed (the newest ticks
    // are all older than that, e.g. Friday's session on a weekend).
    let stop_at_ms = match range {
        Some((from, _)) => from.as_u64(),
        None => utc_day_start_ms(chrono::Utc::now().timestamp_millis().max(0) as u64),
    };

    let mut all_trades: Vec<Trade> = Vec::new();
    let mut before_ms: Option<u64> = None;

    for _ in 0..KLINE_FETCH_MAX_PAGES {
        let selector = kline_page_selector(&symbol, days, before_ms);
        let body = query_with_retry(worker, &selector).await?;

        let reply: TickReply = serde_json::from_str(&body).map_err(|e| {
            AdapterError::ParseError(format!("Acfeed tick reply parse failed for {symbol}: {e}"))
        })?;

        let mut page = ticks_to_trades(&reply.ticks, &symbol);
        if page.is_empty() {
            break;
        }

        // `persist` hands ticks oldest -> newest within a page; pages arrive newest
        // first, so each page is prepended. `before` is exclusive (`ts < before`),
        // so stepping it to the page's oldest timestamp never re-fetches a row.
        let oldest_ms = page.first().map_or(u64::MAX, |t| t.time.as_u64());
        let page_len = page.len();
        page.append(&mut all_trades);
        all_trades = page;

        if oldest_ms <= stop_at_ms {
            break;
        }
        if page_len < KLINE_FETCH_PAGE_LIMIT {
            // Short page: the source ran dry inside the window, nothing older to get.
            // (A server-side `MAX_REPLY_ROWS` smaller than the page limit just means
            // more pages, not a stop: a full page always advances `before`.)
            break;
        }
        if oldest_ms >= before_ms.unwrap_or(u64::MAX) {
            // No backward progress; stop instead of re-fetching the same page forever.
            break;
        }
        before_ms = Some(oldest_ms);
    }

    if let Some((from, _)) = range {
        all_trades.retain(|t| t.time >= from);
    }

    // Every tick builds a candle — premarket/auction prints included, never dropped.
    Ok(aggregate_trades_to_klines(
        &all_trades,
        timeframe,
        ticker_info.min_ticksize,
    ))
}

/// Fetches one page of trades, the newest available strictly before
/// `before_time`, trimmed to `from_time`. This is a single Zenoh round trip
/// with no pagination of its own — the backward-paging loop that walks
/// `before_time` down toward `from_time` across multiple calls lives in the
/// main crate, `fetch_trades_batched` (`src/connector/fetcher.rs`).
///
/// `persist` orders `ts DESC` before applying `limit` (see
/// `persist/src/query.rs`), i.e. it only ever answers "the newest `limit`
/// ticks before `before`", never "the oldest ticks after `from_time`". A
/// caller that only ever passed `from_time` (as this used to) would always
/// land on whatever is most recent in the `days` window: fine when that
/// happens to be the window actually wanted, wrong the moment it isn't —
/// which is exactly what happens once a session prints more than
/// `TRADE_FETCH_LIMIT` ticks since `from_time`. A weekend gap makes this
/// visible reliably: Monday's ticks alone fill the cap, so a request meant
/// to backfill Friday's session (an older, non-live window) came back with
/// Monday's data instead and Friday's footprint stayed empty, even though
/// its candle (aggregated server-side from a much smaller number of bars)
/// still rendered fine. Passing `before_time` explicitly, and having the
/// caller page it backward, fixes that at the source.
pub(super) async fn fetch_trades(
    worker: &mut AcfeedWorker,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    before_time: UnixMs,
    _data_path: Option<PathBuf>,
) -> Result<Vec<Trade>, AdapterError> {
    let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();

    // `days` is the coarsest filter persist offers, so it can only round the window out
    // to whole days; `limit` then takes the newest ticks below `before` (persist orders
    // `ts DESC` before capping). Asking for the cap up front rather than trimming the
    // reply client-side keeps days of ticks off the wire for a liquid product.
    // Zenoh separates selector parameters with `;`, not `&`. With `&` the whole tail
    // lands inside the first parameter's value ("days" becomes "3&limit=1000"), persist
    // fails to parse it and silently falls back to its default of one day — which over a
    // weekend covers only closed-market hours and comes back empty, which is exactly what
    // an unpopulated footprint looks like.
    let days = days_to_cover(from_time);
    let selector = format!(
        "hist/tick/{symbol}?days={days};limit={TRADE_FETCH_LIMIT};before={}",
        before_time.as_u64()
    );
    let body = query_with_retry(worker, &selector).await?;

    let reply: TickReply = serde_json::from_str(&body).map_err(|e| {
        AdapterError::ParseError(format!("Acfeed tick reply parse failed for {symbol}: {e}"))
    })?;

    Ok(trades_from(&reply.ticks, &symbol, from_time))
}

/// Converts a tick reply and keeps only the trades the caller actually asked for.
///
/// The day-granular window persist serves always reaches further back than `from_time`,
/// and handing those extra prints back would have every backfill re-deliver trades the
/// caller already folded in — the caller asked for a window and gets that window.
fn trades_from(ticks: &[RawHistTick], symbol: &str, from_time: UnixMs) -> Vec<Trade> {
    ticks_to_trades(ticks, symbol)
        .into_iter()
        .filter(|trade| trade.time >= from_time)
        .collect()
}

/// Latest stored session quote for `ticker`, so a pane showing the venue's own
/// open/high/low can render immediately instead of waiting out the next live quote.
///
/// Unlike the other backfills, this deliberately asks `persist` for *every* stored
/// quote (wide `days` window) and takes the newest row — the session strip only ever
/// cares about the most recent quote on record, regardless of when it was written. A
/// product that hasn't printed a quote in days (closed market, holiday, weekend) must
/// still show its last known open/high/low rather than sit blank until the venue
/// publishes again. `persist` retains quotes permanently and `handle_quotes` replies
/// oldest -> newest, so `.last()` is always the newest persisted quote.
///
/// `persist` stores each quote's `entries` object verbatim, so the reply has the same
/// shape the live topic publishes and is handed straight to [`codec::process_quote`] —
/// which is what applies the per-symbol inversion (an inverted product's session high
/// is its raw feed's low) and the price rounding grid. Re-deriving any of that here
/// would be a second implementation free to drift from the live path.
pub(super) async fn fetch_latest_quote(
    worker: &mut AcfeedWorker,
    ticker_info: TickerInfo,
) -> Result<Option<codec::QuotePayload>, AdapterError> {
    let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();

    // Persist keeps quotes indefinitely, so a wide window (roughly a decade) is enough
    // to always surface the newest stored quote without ever clipping it.
    const QUOTE_LOOKBACK_DAYS: u64 = 5;
    let selector = format!("hist/quote/{symbol}?days={QUOTE_LOOKBACK_DAYS}");
    let body = query_with_retry(worker, &selector).await?;

    let reply: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        AdapterError::ParseError(format!("Acfeed quote reply parse failed for {symbol}: {e}"))
    })?;

    // Replies run oldest -> newest, and only the newest describes the session now.
    let Some(latest) = reply
        .get("quotes")
        .and_then(|q| q.as_array())
        .and_then(|q| q.last())
    else {
        return Ok(None);
    };

    let single = serde_json::json!({ "quotes": [latest] }).to_string();

    Ok(codec::process_quote(&single, &symbol))
}

/// Fetches the persisted pre-open auction series for `ticker` from `hist/premarket` —
/// the bootstrap source for the premarket levels indicator (plus the full book
/// per snapshot for MAGNETS). Each point carries the sums (size = book size
/// resting at that point's price; bid_vol/ask_vol = summed sizes over ALL book
/// levels; bid_at/ask_at = the at-price queue split by side — the Theoric
/// Targets input, same basis as the premarket indicator's "V") and the raw
/// `book_bids`/`book_offers` levels; indicators recompute their state from it
/// and merge live ticks on top, so an old snapshot tail cannot double-count
/// (see `data::chart::premarket::PremarketLevels`).
pub(super) async fn fetch_premarket(
    worker: &mut AcfeedWorker,
    ticker_info: TickerInfo,
) -> Result<Vec<codec::PremarketPayload>, AdapterError> {
    let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();

    // Premarket points are sparse (seconds apart even mid-auction), and the
    // indicator plots a single session day by default, so fetch just the newest
    // day; older sessions are selectable from the stored snapshot only when the
    // caller asks for a wider window.
    const PREMARKET_LOOKBACK_DAYS: u64 = 1;
    let selector = format!("hist/premarket/{symbol}?days={PREMARKET_LOOKBACK_DAYS}");
    let body = query_with_retry(worker, &selector).await?;

    let reply: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        AdapterError::ParseError(format!(
            "Acfeed premarket reply parse failed for {symbol}: {e}"
        ))
    })?;

    let Some(points) = reply.get("premarket").and_then(|p| p.as_array()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(points.len());
    for point in points {
        let (Some(ts), Some(price)) = (
            point.get("ts").and_then(|t| t.as_i64()),
            point.get("price").and_then(|p| p.as_f64()),
        ) else {
            continue;
        };
        // Volume do nível ("V" na tag): soma dos sizes do livro (bids+offers)
        // cujos preços batem com o preço teórico deste snapshot — não o livro
        // todo, só a fila parada exatamente no preço da linha.
        let side_sum = |side: &str| -> f64 {
            point
                .get(side)
                .and_then(|v| v.as_array())
                .map(|lvls| {
                    lvls.iter()
                        .filter_map(|lvl| lvl.get("size").and_then(|s| s.as_f64()))
                        .map(|s| s.max(0.0))
                        .sum()
                })
                .unwrap_or(0.0)
        };
        let size: f64 = ["bids", "offers"]
            .iter()
            .filter_map(|side| point.get(*side).and_then(|v| v.as_array()))
            .flatten()
            .filter(|lvl| {
                lvl.get("price")
                    .and_then(|p| p.as_f64())
                    .map(|lp| (lp - price).abs() < 1e-9)
                    .unwrap_or(false)
            })
            .filter_map(|lvl| lvl.get("size").and_then(|s| s.as_f64()))
            .map(|s| s.max(0.0))
            .sum();
        // Full-book totals per side.
        let bid_vol = side_sum("bids");
        let ask_vol = side_sum("offers");
        // At-price queue split by side (Theoric Targets input — same basis as
        // the premarket indicator's "V", which is bid_at + ask_at).
        let at_price = |side: &str| -> f64 {
            point
                .get(side)
                .and_then(|v| v.as_array())
                .map(|lvls| {
                    lvls.iter()
                        .filter(|lvl| {
                            lvl.get("price")
                                .and_then(|p| p.as_f64())
                                .map(|lp| (lp - price).abs() < 1e-9)
                                .unwrap_or(false)
                        })
                        .filter_map(|lvl| lvl.get("size").and_then(|s| s.as_f64()))
                        .map(|s| s.max(0.0))
                        .sum()
                })
                .unwrap_or(0.0)
        };
        let bid_at = at_price("bids");
        let ask_at = at_price("offers");
        // Full book, venue-space (same space as `price` here — WDO has no
        // inversion; MAGNETS matching input).
        let book = |side: &str| -> Vec<codec::BookLevel> {
            point
                .get(side)
                .and_then(|v| v.as_array())
                .map(|lvls| {
                    lvls.iter()
                        .filter_map(|lvl| {
                            let p = lvl.get("price").and_then(|p| p.as_f64())?;
                            let s = lvl.get("size").and_then(|s| s.as_f64())?;
                            Some(codec::BookLevel { price: p, size: s })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        out.push(codec::PremarketPayload {
            time: UnixMs::new(ts.max(0) as u64),
            price: Price::from_f64(price),
            size,
            bid_vol,
            ask_vol,
            bid_at,
            ask_at,
            book_bids: book("bids"),
            book_offers: book("offers"),
        });
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `persist` filters by whole days and then hands back the newest slice of that
    /// window, so a request for "trades since 10:00" comes back reaching hours further
    /// back. Returning that surplus makes every backfill re-deliver prints the caller
    /// already folded into its candles.
    #[test]
    fn a_backfill_returns_only_the_window_that_was_asked_for() {
        let json = r#"{"ticks": [
            {"timestamp": "2026-07-30T09:00:00Z", "price": 1.1000, "size": 5},
            {"timestamp": "2026-07-30T09:59:59Z", "price": 1.1001, "size": 2},
            {"timestamp": "2026-07-30T10:00:00Z", "price": 1.1002, "size": 3},
            {"timestamp": "2026-07-30T10:30:00Z", "price": 1.1003, "size": 4}
        ]}"#;
        let reply: TickReply = serde_json::from_str(json).expect("parses tick reply");

        let from = UnixMs::new(
            chrono::DateTime::parse_from_rfc3339("2026-07-30T10:00:00Z")
                .unwrap()
                .timestamp_millis() as u64,
        );

        let trades = trades_from(&reply.ticks, "6E", from);

        assert_eq!(
            trades.len(),
            2,
            "the two ticks before the window are dropped"
        );
        assert!(trades.iter().all(|trade| trade.time >= from));
        assert!((trades.iter().map(|t| t.qty.to_f64()).sum::<f64>() - 7.0).abs() < 1e-9);
    }

    /// Side inference walks the batch in order, so the trim has to happen after the
    /// conversion — a trade kept at the window's edge must classify against its real
    /// predecessor, not against whatever tick happens to be first in the reply.
    #[test]
    fn trimming_the_window_keeps_the_side_inferred_from_the_full_batch() {
        let json = r#"{"ticks": [
            {"timestamp": "2026-07-30T09:59:58Z", "price": 1.1005, "size": 1},
            {"timestamp": "2026-07-30T10:00:00Z", "price": 1.1002, "size": 1}
        ]}"#;
        let reply: TickReply = serde_json::from_str(json).expect("parses tick reply");

        let from = UnixMs::new(
            chrono::DateTime::parse_from_rfc3339("2026-07-30T10:00:00Z")
                .unwrap()
                .timestamp_millis() as u64,
        );

        let trades = trades_from(&reply.ticks, "6E", from);

        assert_eq!(trades.len(), 1);
        assert!(
            trades[0].is_sell,
            "a downtick off the previous print is a sell, and that predecessor is only \
             visible if the batch is classified before it is trimmed"
        );
    }

    #[test]
    fn days_for_span_rounds_up_and_clamps_to_retention() {
        // A span inside the last day still needs one day of history, never zero.
        assert_eq!(days_for_span(0), 1);
        assert_eq!(days_for_span(60_000), 1);
        assert_eq!(days_for_span(DAY_MS), 1);

        // Partial days round up, so the window always reaches past `from`.
        assert_eq!(days_for_span(DAY_MS + 1), 2);
        assert_eq!(days_for_span(DAY_MS * 2), 2);

        // `persist` has nothing older than the retention window.
        assert_eq!(
            days_for_span(DAY_MS * 400),
            PERSIST_RETENTION_DAYS,
            "requests past retention must not exceed what persist keeps"
        );
    }

    #[test]
    fn days_to_cover_handles_a_future_timestamp() {
        // Clock skew must not underflow into a span of ~584 million years.
        let ahead = UnixMs::new(chrono::Utc::now().timestamp_millis() as u64 + DAY_MS);
        assert_eq!(days_to_cover(ahead), 1);
    }

    #[test]
    fn utc_day_start_floors_to_midnight() {
        // 2026-09-14T18:10:37Z -> 2026-09-14T00:00:00Z.
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-14T18:10:37Z")
            .unwrap()
            .timestamp_millis() as u64;
        let expected = chrono::DateTime::parse_from_rfc3339("2026-09-14T00:00:00Z")
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(utc_day_start_ms(now), expected);
        assert_eq!(utc_day_start_ms(expected), expected);
    }

    #[test]
    fn kline_page_selector_uses_semicolon_params_and_skips_before_on_first_page() {
        assert_eq!(
            kline_page_selector("WDO", 10, None),
            format!("hist/tick/WDO?days=10;limit={KLINE_FETCH_PAGE_LIMIT}")
        );
        assert_eq!(
            kline_page_selector("6L", 2, Some(1_700_000_000_000)),
            format!("hist/tick/6L?days=2;limit={KLINE_FETCH_PAGE_LIMIT};before=1700000000000")
        );
    }

    #[test]
    fn deserialize_time_handles_minute_precision_timestamps() {
        // The historical service may emit minute-precision timestamps without seconds,
        // which `DateTime::parse_from_rfc3339` rejects on its own.
        let json = r#"{"ticks": [
            {"timestamp": "2026-07-30T22:00Z", "price": 1.1, "size": 5}
        ]}"#;
        let reply: TickReply =
            serde_json::from_str(json).expect("parses minute-precision timestamp");
        assert_eq!(
            reply.ticks[0].time,
            chrono::DateTime::parse_from_rfc3339("2026-07-30T22:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn tick_reply_parses_persist_shape() {
        let json = r#"{
            "product": "6E",
            "ticks": [
                {"timestamp": "1700000000000", "price": "1.10050", "size": 3, "bid": 1.1004, "ask": 1.1005, "bidSize": 2, "askSize": 4, "id": 7}
            ]
        }"#;

        let reply: TickReply = serde_json::from_str(json).expect("parses");
        assert_eq!(reply.ticks.len(), 1);

        let trades = ticks_to_trades(&reply.ticks, "6E");
        assert_eq!(trades.len(), 1);
        assert!((trades[0].price.to_f64() - 1.1005).abs() < 1e-6);
        assert_eq!(trades[0].qty.to_f64(), 3.0);
        assert_eq!(trades[0].time, UnixMs::new(1_700_000_000_000));
    }

    #[test]
    fn classify_side_hits_ask_is_buy_hits_bid_is_sell() {
        // First tick in a batch: no previous trade price yet, so classification falls
        // back to price vs. bid/ask, exactly like `codec::process_ticks`'s cold-start path.
        let ticks: Vec<RawHistTick> = serde_json::from_str(
            r#"[
                {"time": 1700000000000, "price": 1.1005, "size": 1, "bid": 1.1004, "ask": 1.1005},
                {"time": 1700000001000, "price": 1.1004, "size": 1, "bid": 1.1003, "ask": 1.1006}
            ]"#,
        )
        .unwrap();

        let trades = ticks_to_trades(&ticks, "6E");
        assert!(!trades[0].is_sell, "price at ask should classify as buy");

        // Second tick prints lower than the first trade's price -> classified against
        // the previous trade, not bid/ask, matching the live feed's "established
        // last_price" branch.
        assert!(trades[1].is_sell, "price fell vs. previous trade -> sell");
    }

    #[test]
    fn tick_reply_inverts_price_and_flips_side() {
        let ticks: Vec<RawHistTick> = serde_json::from_str(
            r#"[{"time": 1700000000000, "price": 0.15, "size": 5, "bid": 0.1499, "ask": 0.15}]"#,
        )
        .unwrap();

        let trades = ticks_to_trades(&ticks, "6M");
        let expected = invert_price(0.15, "6M");
        assert!((trades[0].price.to_f64() - expected).abs() < 1e-6);
        // raw tick hits the ask (buy); inversion flips it to a sell, same rule the live
        // path applies via `codec::invert_side`.
        assert!(trades[0].is_sell);
    }

    #[test]
    fn aggregate_trades_to_klines_buckets_like_kline_builder() {
        let ticksize = PriceStep::from_f64_exact(0.00001);
        let trades = vec![
            Trade {
                time: UnixMs::new(0),
                is_sell: false,
                price: Price::from_f64(1.1000),
                qty: Qty::from_f64(2.0),
            },
            Trade {
                time: UnixMs::new(30_000),
                is_sell: true,
                price: Price::from_f64(1.1010),
                qty: Qty::from_f64(1.0),
            },
            Trade {
                time: UnixMs::new(65_000),
                is_sell: false,
                price: Price::from_f64(1.0995),
                qty: Qty::from_f64(5.0),
            },
        ];

        let klines = aggregate_trades_to_klines(&trades, Timeframe::M1, ticksize);
        assert_eq!(klines.len(), 2, "two 1m buckets: [0,60s) and [60s,120s)");
        assert_eq!(klines[0].open.to_f64(), 1.1000);
        assert!((klines[0].high.to_f64() - 1.1010).abs() < 1e-9);
        assert!((klines[0].close.to_f64() - 1.1010).abs() < 1e-9);
        assert_eq!(klines[0].volume.total().to_f64(), 3.0);
        assert_eq!(klines[1].open.to_f64(), 1.0995);
        assert_eq!(klines[1].volume.total().to_f64(), 5.0);
    }
}
