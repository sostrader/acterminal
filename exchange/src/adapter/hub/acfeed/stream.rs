//! Zenoh real-time subscriber, ported from ACTerminal's `zenoh_feed.rs` connection/
//! reconnect shape (see `acterminal-tauri/src-tauri/src/zenoh_feed.rs`).
//!
//! Payload parsing is already done in `codec.rs` (`process_ticks`/`process_dom`) — this
//! module only owns the Zenoh session lifecycle, subscription setup, and translating
//! decoded ticks/DOMs into [`Event`]s.
//!
//! ## Design notes / deviations from ACTerminal's shape
//!
//! ACTerminal's `ZenohFeed` is a single long-lived background feed serving a *dynamic*
//! active-symbol set (hence its 500ms reconciliation loop that adds/removes
//! subscriptions as the user changes charts). acterminal's adapter contract is
//! different: each `connect_trade_stream`/`connect_kline_stream`/`connect_depth_stream`
//! call already receives a *fixed* symbol/stream list for the lifetime of that
//! `Stream` — one subscription per `connect_*` call, symbol set fixed at call time.
//! So the dynamic add/remove
//! reconciliation isn't needed here; what's ported is the part that *is* still needed:
//! the outer reconnect-on-failure loop and per-symbol tick-ordering state
//! (`TickState`) reset on every (re)connect.
//!
//! Each `connect_*_stream` call opens its own dedicated `zenoh::Session` (not shared
//! with `fetch.rs` or other stream calls) — simplest to reason about given the
//! reconnect-loop owns the session's lifetime, and mirrors ACTerminal's own choice of a
//! session per long-lived subscription concern.
//!
//! ## Why `Event::KlineReceived` is synthesized here
//!
//! Trades alone can never create or move a candle in this codebase —
//! `data/src/aggr/time.rs::insert_trades_existing_buckets` only updates *existing*
//! buckets and only touches footprint data, never OHLC. So for every active
//! `StreamKind::Kline { ticker_info, timeframe }` subscription this module keeps an
//! in-progress [`Kline`] per symbol/timeframe (see [`KlineBuilder`]), buckets each
//! processed tick's timestamp via [`UnixMs::floor_to`], updates high/low/close/volume
//! on the current bucket, and emits `Event::KlineReceived` on *every* tick (not just on
//! bucket close), so the forming candle updates live.

use crate::{
    Event, Kline, Price, PushFrequency, Qty, TickerInfo, Timeframe, UnixMs, Volume,
    adapter::{StreamKind, StreamTicksize},
    depth::{DepthUpdate, LocalDepthCache},
    unit::PriceStep,
};

use super::codec::{self, TickState};

use crate::metrics;

use rustc_hash::FxHashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use zenoh::Session;
use zenoh::config::Config;

const DEFAULT_ZENOH_ENDPOINT: &str = "tcp/207.244.255.183:7447";
const RECONNECT_DELAY: Duration = Duration::from_secs(3);
const EVENT_CHANNEL_CAPACITY: usize = 512;

fn zenoh_endpoint() -> String {
    std::env::var("ZENOH_ENDPOINT").unwrap_or_else(|_| DEFAULT_ZENOH_ENDPOINT.to_string())
}

fn zenoh_config() -> Config {
    let endpoint = zenoh_endpoint();
    Config::from_json5(&format!(
        r#"{{
            "mode": "client",
            "connect": {{ "endpoints": ["{}"] }}
        }}"#,
        endpoint
    ))
    .expect("invalid Zenoh config")
}

/// Opens a fresh Zenoh client session against `ZENOH_ENDPOINT`. Exposed to `fetch.rs`
/// too (via `super::AcfeedWorker`) so both the subscribe and query paths build their
/// session the same way, even though each keeps its own session lifecycle (see module
/// docs).
pub(super) async fn open_zenoh_session() -> zenoh::Result<Session> {
    let session = zenoh::open(zenoh_config()).await?;
    metrics::session_opened();
    Ok(session)
}

async fn open_session(
    stream_scope: &Arc<[StreamKind]>,
    event_tx: &mpsc::Sender<Event>,
) -> Option<Session> {
    match open_zenoh_session().await {
        Ok(session) => Some(session),
        Err(e) => {
            let reason = format!("Zenoh connect to {} failed: {e}", zenoh_endpoint());
            log::warn!("[TRADOVATE] {reason}");
            let _ = event_tx
                .send(Event::Disconnected(Arc::clone(stream_scope), reason))
                .await;
            None
        }
    }
}

/// Closes a Zenoh session explicitly. Letting a `Session` (or its
/// subscribers) just fall out of scope leaks the declare/undeclare cycle —
/// every declare without a matching undeclare/close pins memory — so every
/// reconnect-loop exit path must go through here.
async fn close_session(session: Session) {
    if let Err(e) = session.close().await {
        log::debug!("[TRADOVATE] zenoh session close failed: {e}");
    }
    metrics::session_closed();
}

/// Sends an event without blocking the Zenoh drain loop. A blocking
/// `send().await` parks the per-symbol task when the 512-slot channel fills,
/// which stops `recv_async` draining, fills Zenoh's own FIFO, stalls its
/// dispatch thread, and lets upstream transport buffers grow without bound.
///
/// Dropping is recoverable: depth/quotes are superseded by the next message,
/// klines re-emit on the next tick, and trade gaps are backfilled by the
/// footprint gap-fetch. Returns `false` only when the receiver is gone.
fn try_send_event(
    tx: &mpsc::Sender<Event>,
    event: Event,
    dropped: &mut u64,
    scope: &'static str,
) -> bool {
    match tx.try_send(event) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
        Err(mpsc::error::TrySendError::Full(_)) => {
            *dropped += 1;
            metrics::event_dropped();
            if *dropped == 1 || dropped.is_multiple_of(1024) {
                log::warn!(
                    "[TRADOVATE] {scope} channel full, dropping live events (total dropped: {})",
                    *dropped
                );
            }
            true
        }
    }
}

/// `futures::Stream<Item = Event>` backed by an mpsc receiver fed by a background
/// reconnect-loop task. Aborts the task on drop, mirroring `adapter::ws::ChannelStream`
/// (which is WS-transport-specific and not reachable from here — this is the same
/// shape rebuilt as a small transport-agnostic primitive).
struct EventStream {
    receiver: mpsc::Receiver<Event>,
    task: tokio::task::JoinHandle<()>,
}

impl futures::Stream for EventStream {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        self.receiver.poll_recv(cx)
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Tracks the in-progress candle for one (ticker, timeframe) pair. Trades alone never
/// move OHLC in this codebase (see module docs), so every processed tick is folded into
/// the current bucket and re-emitted as a fresh [`Kline`].
///
/// `pub(super)` so `fetch.rs` can reuse the exact same bucketing logic to build
/// historical klines from raw ticks — live and historical candles must be bucketed
/// identically, so this is shared rather than re-implemented.
#[derive(Debug)]
pub(super) struct KlineBuilder {
    timeframe: Timeframe,
    min_ticksize: PriceStep,
    current: Option<Kline>,
}

impl KlineBuilder {
    pub(super) fn new(timeframe: Timeframe, min_ticksize: PriceStep) -> Self {
        Self {
            timeframe,
            min_ticksize,
            current: None,
        }
    }

    /// Feed one trade's (time, price, qty) into the builder. Returns the updated
    /// (possibly newly-opened) in-progress candle.
    pub(super) fn update(&mut self, time: UnixMs, price: Price, qty: Qty) -> Kline {
        let bucket_time = time.floor_to(self.timeframe);
        let p = price.round_to_step(self.min_ticksize);

        let kline = match self.current {
            Some(mut k) if k.time == bucket_time => {
                if p.units > k.high.units {
                    k.high = p;
                }
                if p.units < k.low.units {
                    k.low = p;
                }
                k.close = p;
                k.volume = Volume::TotalOnly(k.volume.total() + qty);
                k
            }
            _ => Kline {
                time: bucket_time,
                open: p,
                high: p,
                low: p,
                close: p,
                volume: Volume::TotalOnly(qty),
            },
        };

        self.current = Some(kline);
        kline
    }
}

// ── Trade stream ────────────────────────────────────────────────────────────

pub fn connect_trade_stream(tickers: Vec<TickerInfo>) -> impl futures::Stream<Item = Event> {
    let stream_scope: Arc<[StreamKind]> = Arc::from(
        tickers
            .iter()
            .map(|ticker_info| StreamKind::Trades {
                ticker_info: *ticker_info,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );

    let mut by_symbol: FxHashMap<String, (TickerInfo, Arc<TickState>)> = FxHashMap::default();
    for ticker_info in &tickers {
        let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();
        by_symbol.insert(symbol, (*ticker_info, Arc::new(TickState::default())));
    }

    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let task = tokio::spawn(run_trade_loop(stream_scope, by_symbol, event_tx));

    EventStream {
        receiver: event_rx,
        task,
    }
}

async fn run_trade_loop(
    stream_scope: Arc<[StreamKind]>,
    by_symbol: FxHashMap<String, (TickerInfo, Arc<TickState>)>,
    event_tx: mpsc::Sender<Event>,
) {
    if by_symbol.is_empty() {
        let _ = event_tx
            .send(Event::Disconnected(
                stream_scope,
                "Empty stream payload".to_string(),
            ))
            .await;
        return;
    }

    loop {
        let Some(session) = open_session(&stream_scope, &event_tx).await else {
            if event_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        };

        for (_, state) in by_symbol.values() {
            state.reset();
        }

        let mut join_set = JoinSet::new();
        for (symbol, (ticker_info, state)) in &by_symbol {
            let key_expr = format!("ticks/{symbol}");
            let sub = match session.declare_subscriber(&key_expr).await {
                Ok(sub) => {
                    metrics::sub_declared();
                    sub
                }
                Err(e) => {
                    log::warn!("[TRADOVATE] subscribe failed {key_expr}: {e}");
                    continue;
                }
            };

            let symbol = symbol.clone();
            let ticker_info = *ticker_info;
            let state = Arc::clone(state);
            let tx = event_tx.clone();

            join_set.spawn(async move {
                let mut dropped = 0u64;
                loop {
                    match sub.recv_async().await {
                        Ok(sample) => {
                            let bytes = sample.payload().to_bytes();
                            let payload = String::from_utf8_lossy(&bytes);
                            let trades = codec::process_ticks(&payload, &symbol, &state);

                            if !trades.is_empty() {
                                let time =
                                    trades.last().map(|t| t.time).unwrap_or_else(UnixMs::now);
                                let event = Event::TradesReceived(
                                    StreamKind::Trades { ticker_info },
                                    time,
                                    trades.into_boxed_slice(),
                                );
                                if !try_send_event(&tx, event, &mut dropped, "trade") {
                                    return;
                                }
                            }
                        }
                        Err(_) => return,
                    }
                }
            });
        }

        if join_set.is_empty() {
            let _ = event_tx
                .send(Event::Disconnected(
                    Arc::clone(&stream_scope),
                    "No subscriptions established".to_string(),
                ))
                .await;
            close_session(session).await;
            if event_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        }

        let _ = event_tx
            .send(Event::Connected(Arc::clone(&stream_scope)))
            .await;

        join_set.join_next().await;
        join_set.abort_all();
        // Reap the aborted tasks before the set is dropped: dropping a
        // `JoinSet` detaches (does not abort) its tasks, leaking the
        // subscriber + payload tasks on every reconnect.
        while join_set.join_next().await.is_some() {}
        close_session(session).await;

        if event_tx.is_closed() {
            return;
        }

        let _ = event_tx
            .send(Event::Disconnected(
                Arc::clone(&stream_scope),
                "Zenoh subscription ended".to_string(),
            ))
            .await;
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

// ── Kline stream (synthesized from ticks) ──────────────────────────────────

/// The kline subscriptions grouped by feed symbol: one shared [`TickState`] per symbol
/// (aggressor classification is per-instrument, not per-timeframe) alongside every
/// timeframe being built from that symbol's ticks.
type KlineStreamsBySymbol = FxHashMap<String, (Arc<TickState>, Vec<(TickerInfo, Timeframe)>)>;

pub fn connect_kline_stream(
    streams: Vec<(TickerInfo, Timeframe)>,
) -> impl futures::Stream<Item = Event> {
    let stream_scope: Arc<[StreamKind]> = Arc::from(
        streams
            .iter()
            .map(|(ticker_info, timeframe)| StreamKind::Kline {
                ticker_info: *ticker_info,
                timeframe: *timeframe,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );

    let mut by_symbol: KlineStreamsBySymbol = FxHashMap::default();
    for (ticker_info, timeframe) in &streams {
        let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();
        by_symbol
            .entry(symbol)
            .or_insert_with(|| (Arc::new(TickState::default()), Vec::new()))
            .1
            .push((*ticker_info, *timeframe));
    }

    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let task = tokio::spawn(run_kline_loop(stream_scope, by_symbol, event_tx));

    EventStream {
        receiver: event_rx,
        task,
    }
}

async fn run_kline_loop(
    stream_scope: Arc<[StreamKind]>,
    by_symbol: KlineStreamsBySymbol,
    event_tx: mpsc::Sender<Event>,
) {
    if by_symbol.is_empty() {
        let _ = event_tx
            .send(Event::Disconnected(
                stream_scope,
                "Empty stream payload".to_string(),
            ))
            .await;
        return;
    }

    loop {
        let Some(session) = open_session(&stream_scope, &event_tx).await else {
            if event_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        };

        for (state, _) in by_symbol.values() {
            state.reset();
        }

        let mut join_set = JoinSet::new();
        for (symbol, (state, targets)) in &by_symbol {
            let key_expr = format!("ticks/{symbol}");
            let sub = match session.declare_subscriber(&key_expr).await {
                Ok(sub) => {
                    metrics::sub_declared();
                    sub
                }
                Err(e) => {
                    log::warn!("[TRADOVATE] subscribe failed {key_expr}: {e}");
                    continue;
                }
            };

            let symbol = symbol.clone();
            let state = Arc::clone(state);
            let targets = targets.clone();
            let tx = event_tx.clone();

            join_set.spawn(async move {
                let mut dropped = 0u64;
                let mut builders: Vec<(TickerInfo, Timeframe, KlineBuilder)> = targets
                    .into_iter()
                    .map(|(ticker_info, timeframe)| {
                        (
                            ticker_info,
                            timeframe,
                            KlineBuilder::new(timeframe, ticker_info.min_ticksize),
                        )
                    })
                    .collect();

                loop {
                    match sub.recv_async().await {
                        Ok(sample) => {
                            let bytes = sample.payload().to_bytes();
                            let payload = String::from_utf8_lossy(&bytes);
                            let trades = codec::process_ticks(&payload, &symbol, &state);

                            for trade in &trades {
                                for (ticker_info, timeframe, builder) in &mut builders {
                                    let kline = builder.update(trade.time, trade.price, trade.qty);
                                    let event = Event::KlineReceived(
                                        StreamKind::Kline {
                                            ticker_info: *ticker_info,
                                            timeframe: *timeframe,
                                        },
                                        kline,
                                    );
                                    if !try_send_event(&tx, event, &mut dropped, "kline") {
                                        return;
                                    }
                                }
                            }
                        }
                        Err(_) => return,
                    }
                }
            });
        }

        if join_set.is_empty() {
            let _ = event_tx
                .send(Event::Disconnected(
                    Arc::clone(&stream_scope),
                    "No subscriptions established".to_string(),
                ))
                .await;
            close_session(session).await;
            if event_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        }

        let _ = event_tx
            .send(Event::Connected(Arc::clone(&stream_scope)))
            .await;

        join_set.join_next().await;
        join_set.abort_all();
        // Reap the aborted tasks before the set is dropped: dropping a
        // `JoinSet` detaches (does not abort) its tasks, leaking the
        // subscriber + payload tasks on every reconnect.
        while join_set.join_next().await.is_some() {}
        close_session(session).await;

        if event_tx.is_closed() {
            return;
        }

        let _ = event_tx
            .send(Event::Disconnected(
                Arc::clone(&stream_scope),
                "Zenoh subscription ended".to_string(),
            ))
            .await;
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

// ── Depth stream ────────────────────────────────────────────────────────────

pub fn connect_depth_stream(
    ticker_info: TickerInfo,
    depth_aggr: StreamTicksize,
    push_freq: PushFrequency,
) -> impl futures::Stream<Item = Event> {
    let stream = StreamKind::Depth {
        ticker_info,
        depth_aggr,
        push_freq,
    };
    let stream_scope: Arc<[StreamKind]> = Arc::from(vec![stream].into_boxed_slice());
    let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();

    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let task = tokio::spawn(run_depth_loop(
        stream,
        stream_scope,
        symbol,
        ticker_info,
        event_tx,
    ));

    EventStream {
        receiver: event_rx,
        task,
    }
}

/// Subscribes to the venue's `quotes/` topic for session statistics (open/high/low,
/// best bid/ask, last).
///
/// Deliberately separate from the depth loop even though both are per-symbol
/// subscriptions: quotes carry no book state to cache and no aggregation, so folding
/// them together would only couple two independent reconnect lifetimes.
pub fn connect_quote_stream(ticker_info: TickerInfo) -> impl futures::Stream<Item = Event> {
    let stream = StreamKind::Quote { ticker_info };
    let stream_scope: Arc<[StreamKind]> = Arc::from(vec![stream].into_boxed_slice());
    let (symbol, _) = ticker_info.ticker.to_full_symbol_and_type();

    let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let task = tokio::spawn(run_quote_loop(stream, stream_scope, symbol, event_tx));

    EventStream {
        receiver: event_rx,
        task,
    }
}

async fn run_quote_loop(
    stream: StreamKind,
    stream_scope: Arc<[StreamKind]>,
    symbol: String,
    event_tx: mpsc::Sender<Event>,
) {
    loop {
        let Some(session) = open_session(&stream_scope, &event_tx).await else {
            if event_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        };

        let key_expr = format!("quotes/{symbol}");
        let sub = match session.declare_subscriber(&key_expr).await {
            Ok(sub) => sub,
            Err(e) => {
                let reason = format!("subscribe failed {key_expr}: {e}");
                log::warn!("[TRADOVATE] {reason}");
                let _ = event_tx
                    .send(Event::Disconnected(Arc::clone(&stream_scope), reason))
                    .await;
                close_session(session).await;
                if event_tx.is_closed() {
                    return;
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };

        let _ = event_tx
            .send(Event::Connected(Arc::clone(&stream_scope)))
            .await;

        let mut dropped = 0u64;
        while let Ok(sample) = sub.recv_async().await {
            let bytes = sample.payload().to_bytes();
            let payload = String::from_utf8_lossy(&bytes);

            if let Some(quote) = codec::process_quote(&payload, &symbol) {
                let event = Event::QuoteReceived(stream, quote);
                if !try_send_event(&event_tx, event, &mut dropped, "quote") {
                    close_session(session).await;
                    return;
                }
            }
        }

        close_session(session).await;
        if event_tx.is_closed() {
            return;
        }

        let _ = event_tx
            .send(Event::Disconnected(
                Arc::clone(&stream_scope),
                "Zenoh subscription ended".to_string(),
            ))
            .await;
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn run_depth_loop(
    stream: StreamKind,
    stream_scope: Arc<[StreamKind]>,
    symbol: String,
    ticker_info: TickerInfo,
    event_tx: mpsc::Sender<Event>,
) {
    loop {
        let Some(session) = open_session(&stream_scope, &event_tx).await else {
            if event_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
            continue;
        };

        let key_expr = format!("doms/{symbol}");
        let sub = match session.declare_subscriber(&key_expr).await {
            Ok(sub) => sub,
            Err(e) => {
                let reason = format!("subscribe failed {key_expr}: {e}");
                log::warn!("[TRADOVATE] {reason}");
                let _ = event_tx
                    .send(Event::Disconnected(Arc::clone(&stream_scope), reason))
                    .await;
                close_session(session).await;
                if event_tx.is_closed() {
                    return;
                }
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };

        let _ = event_tx
            .send(Event::Connected(Arc::clone(&stream_scope)))
            .await;

        let mut cache = LocalDepthCache::default();
        let mut dropped = 0u64;

        while let Ok(sample) = sub.recv_async().await {
            let bytes = sample.payload().to_bytes();
            let payload = String::from_utf8_lossy(&bytes);

            if let Some(depth_payload) = codec::process_dom(&payload, &symbol) {
                let time = depth_payload.time;
                cache.update(
                    DepthUpdate::Snapshot(depth_payload),
                    ticker_info.min_ticksize,
                );

                let event = Event::DepthReceived(stream, time, cache.depth.clone());
                if !try_send_event(&event_tx, event, &mut dropped, "depth") {
                    close_session(session).await;
                    return;
                }
            }
        }

        close_session(session).await;
        if event_tx.is_closed() {
            return;
        }

        let _ = event_tx
            .send(Event::Disconnected(
                Arc::clone(&stream_scope),
                "Zenoh subscription ended".to_string(),
            ))
            .await;
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn min_ticksize(step: f32) -> PriceStep {
        PriceStep::from_f64_exact(step as f64)
    }

    #[test]
    fn kline_builder_tracks_ohlc_within_bucket() {
        let mut builder = KlineBuilder::new(Timeframe::M1, min_ticksize(0.00001));

        let k1 = builder.update(UnixMs::new(0), Price::from_f64(1.1000), Qty::from_f64(2.0));
        assert_eq!(k1.open.to_f64(), 1.1000);
        assert_eq!(k1.high.to_f64(), 1.1000);
        assert_eq!(k1.low.to_f64(), 1.1000);
        assert_eq!(k1.close.to_f64(), 1.1000);
        assert_eq!(k1.volume.total().to_f64(), 2.0);

        // still within the same 1m bucket (30s later)
        let k2 = builder.update(
            UnixMs::new(30_000),
            Price::from_f64(1.1010),
            Qty::from_f64(1.0),
        );
        assert_eq!(k2.time, k1.time);
        assert_eq!(k2.open.to_f64(), 1.1000, "open must not move mid-bucket");
        assert!((k2.high.to_f64() - 1.1010).abs() < 1e-9);
        assert!((k2.low.to_f64() - 1.1000).abs() < 1e-9);
        assert!((k2.close.to_f64() - 1.1010).abs() < 1e-9);
        assert_eq!(
            k2.volume.total().to_f64(),
            3.0,
            "volume accumulates within bucket"
        );

        // a lower print, still same bucket -> low should move, high stays
        let k2b = builder.update(
            UnixMs::new(45_000),
            Price::from_f64(1.0990),
            Qty::from_f64(4.0),
        );
        assert!(
            (k2b.high.to_f64() - 1.1010).abs() < 1e-9,
            "high must not regress"
        );
        assert!((k2b.low.to_f64() - 1.0990).abs() < 1e-9);
        assert!((k2b.close.to_f64() - 1.0990).abs() < 1e-9);
        assert_eq!(k2b.volume.total().to_f64(), 7.0);

        // crosses into the next 1m bucket (65s)
        let k3 = builder.update(
            UnixMs::new(65_000),
            Price::from_f64(1.0995),
            Qty::from_f64(5.0),
        );
        assert!(
            k3.time > k2b.time,
            "bucket boundary must roll the candle over"
        );
        assert_eq!(
            k3.open.to_f64(),
            1.0995,
            "new bucket opens at the fresh price"
        );
        assert_eq!(k3.high.to_f64(), 1.0995);
        assert_eq!(k3.low.to_f64(), 1.0995);
        assert_eq!(
            k3.volume.total().to_f64(),
            5.0,
            "volume resets for a fresh bucket"
        );
    }

    #[test]
    fn kline_builder_first_update_opens_flat_candle() {
        let mut builder = KlineBuilder::new(Timeframe::M5, min_ticksize(0.005));
        let k = builder.update(
            UnixMs::new(123_456_789),
            Price::from_f64(150.0),
            Qty::from_f64(1.0),
        );
        assert_eq!(k.open, k.high);
        assert_eq!(k.open, k.low);
        assert_eq!(k.open, k.close);
    }
}
