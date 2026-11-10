//! CME FX-futures data source backed by Zenoh (mirrors ACTerminal's `ZenohProvider`).
//!
//! Real-time ticks/doms are consumed via Zenoh subscription (`stream.rs`); historical
//! bars/ticks are fetched via a Zenoh queryable (`fetch.rs`). Both share the raw-payload
//! parsing + per-symbol price formatting (inversion/decimals) in `codec.rs`, which must
//! stay in lockstep with ACTerminal's `zenoh_feed.rs` since they consume the same feed.
//!
//! This module intentionally does **not** touch `Venue`/`Exchange`/`AdapterHandles` —
//! that collapse (`Venue::Acfeed`, `Exchange::AcfeedFutures`, wiring
//! [`AcfeedHandle`] into `AdapterHandles`) is left for a later checkpoint. Everything
//! here is additive and usable standalone via [`AcfeedHandle`].

pub mod codec;
pub mod fetch;
pub mod stream;
pub mod symbols;

use crate::{
    Event, Kline, PushFrequency, TickerInfo, Timeframe, Trade, UnixMs, adapter::StreamTicksize,
};

use super::{AdapterError, RequestPort};
use std::path::PathBuf;

type AcfeedCommand = super::FetchCommand<()>;

/// Facade combining real-time Zenoh subscriptions ([`stream`]) and historical Zenoh
/// queries ([`fetch`]) into one handle.
#[derive(Clone)]
pub struct AcfeedHandle {
    request_port: RequestPort<AcfeedCommand>,
}

impl AcfeedHandle {
    /// `proxy_cfg` is accepted for signature symmetry with the other `*Handle::new`
    /// constructors (Zenoh doesn't need an `Option<&Proxy>` the way the REST-based
    /// hubs do) but is currently unused.
    pub fn new(_proxy_cfg: Option<&crate::proxy::Proxy>) -> Result<Self, AdapterError> {
        let worker = AcfeedWorker::new();
        let request_port = super::spawn_fetch_worker(worker);

        Ok(Self { request_port })
    }

    /// Acfeed's forex-futures metadata is a static table (see `symbols.rs`) rather
    /// than something fetched over the network. Real UI wiring (mapping `symbols::SYMBOLS`
    /// into `Ticker`s) needs an `Exchange::AcfeedFutures` variant that doesn't exist
    /// yet (that's the next checkpoint's job), so this returns an empty-but-successful
    /// map for now rather than panicking or erroring.
    pub async fn fetch_ticker_metadata(&self) -> Result<super::TickerMetadataMap, AdapterError> {
        self.request_port
            .request(|reply| AcfeedCommand::TickerMetadata {
                market_scope: (),
                reply,
            })
            .await
    }

    /// See [`Self::fetch_ticker_metadata`] — same placeholder rationale.
    pub async fn fetch_ticker_stats(&self) -> Result<super::TickerStatsMap, AdapterError> {
        self.request_port
            .request(|reply| AcfeedCommand::TickerStats {
                market_scope: (),
                reply,
            })
            .await
    }

    pub async fn fetch_klines(
        &self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        range: Option<(UnixMs, UnixMs)>,
    ) -> Result<Vec<Kline>, AdapterError> {
        self.request_port
            .request(move |reply| AcfeedCommand::Klines {
                ticker: ticker_info,
                timeframe,
                range,
                reply,
            })
            .await
    }

    /// `data_path` is accepted for signature symmetry with `AdapterHandles::fetch_trades`
    /// but is unused — Acfeed's fetch has no local cache, it queries `persist` directly.
    pub async fn fetch_trades(
        &self,
        ticker_info: TickerInfo,
        from_time: UnixMs,
        before_time: UnixMs,
        data_path: Option<PathBuf>,
    ) -> Result<Vec<Trade>, AdapterError> {
        self.request_port
            .request(move |reply| AcfeedCommand::Trades {
                ticker: ticker_info,
                from_time,
                before_time,
                data_path,
                reply,
            })
            .await
    }

    /// Takes no `MarketKind`: `codec::process_ticks` already returns fully-formed
    /// `Trade`s, since FX futures need no notional conversion off their raw tick sizes.
    pub fn connect_trade_stream(
        self,
        tickers: Vec<TickerInfo>,
    ) -> impl futures::Stream<Item = Event> {
        stream::connect_trade_stream(tickers)
    }

    /// Latest stored session quote, used to bootstrap panes that show the venue's
    /// own open/high/low before the next live quote lands.
    pub async fn fetch_latest_quote(
        &self,
        ticker_info: TickerInfo,
    ) -> Result<Option<codec::QuotePayload>, AdapterError> {
        self.request_port
            .request(move |reply| AcfeedCommand::LatestQuote {
                ticker: ticker_info,
                reply,
            })
            .await
    }

    /// Persisted pre-open auction series, the only source for the premarket levels
    /// indicator (fetch-only, no live stream).
    pub async fn fetch_premarket(
        &self,
        ticker_info: TickerInfo,
    ) -> Result<Vec<codec::PremarketPayload>, AdapterError> {
        self.request_port
            .request(move |reply| AcfeedCommand::Premarket {
                ticker: ticker_info,
                reply,
            })
            .await
    }

    /// See [`Self::connect_trade_stream`] re: no `MarketKind` parameter.
    pub fn connect_kline_stream(
        self,
        streams: Vec<(TickerInfo, Timeframe)>,
    ) -> impl futures::Stream<Item = Event> {
        stream::connect_kline_stream(streams)
    }

    pub fn connect_depth_stream(
        self,
        ticker_info: TickerInfo,
        depth_aggr: StreamTicksize,
        push_freq: PushFrequency,
    ) -> impl futures::Stream<Item = Event> {
        stream::connect_depth_stream(ticker_info, depth_aggr, push_freq)
    }

    pub fn connect_quote_stream(
        self,
        ticker_info: TickerInfo,
    ) -> impl futures::Stream<Item = Event> {
        stream::connect_quote_stream(ticker_info)
    }
}

/// Backs [`AcfeedHandle`]'s fetch commands. Holds a lazily-opened `zenoh::Session`
/// reused across queries (separate from `stream.rs`'s own per-subscription sessions —
/// see `stream.rs`'s module docs for why a session-per-concern split was chosen over one
/// shared session).
struct AcfeedWorker {
    session: Option<zenoh::Session>,
}

impl AcfeedWorker {
    fn new() -> Self {
        Self { session: None }
    }

    async fn ensure_session(&mut self) -> Result<(), AdapterError> {
        if self.session.is_none() {
            let session = stream::open_zenoh_session()
                .await
                .map_err(|e| AdapterError::WebsocketError(format!("Zenoh connect failed: {e}")))?;
            self.session = Some(session);
        }
        Ok(())
    }

    fn session(&self) -> Result<&zenoh::Session, AdapterError> {
        self.session.as_ref().ok_or_else(|| {
            AdapterError::WebsocketError("Zenoh session not initialized".to_string())
        })
    }

    fn invalidate_session(&mut self) {
        // Close, don't just drop: an unclosed session leaks its
        // declare/undeclare cycle. Fire-and-forget is fine here — the next
        // `ensure_session` opens a fresh one regardless of when this lands.
        if let Some(session) = self.session.take() {
            crate::metrics::session_closed();
            tokio::spawn(async move {
                let _ = session.close().await;
            });
        }
    }
}

impl super::FetchCommandHandler<()> for AcfeedWorker {
    fn fetch_ticker_metadata(
        &mut self,
        _market_scope: (),
    ) -> futures::future::BoxFuture<'_, Result<super::TickerMetadataMap, AdapterError>> {
        Box::pin(async { Ok(std::collections::HashMap::new()) })
    }

    fn fetch_ticker_stats(
        &mut self,
        _market_scope: (),
    ) -> futures::future::BoxFuture<'_, Result<super::TickerStatsMap, AdapterError>> {
        Box::pin(async { Ok(std::collections::HashMap::new()) })
    }

    fn fetch_klines(
        &mut self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        range: Option<(UnixMs, UnixMs)>,
    ) -> futures::future::BoxFuture<'_, Result<Vec<Kline>, AdapterError>> {
        Box::pin(async move { fetch::fetch_klines(self, ticker_info, timeframe, range).await })
    }

    fn fetch_trades(
        &mut self,
        ticker_info: TickerInfo,
        from_time: UnixMs,
        before_time: UnixMs,
        data_path: Option<PathBuf>,
    ) -> futures::future::BoxFuture<'_, Result<Vec<Trade>, AdapterError>> {
        Box::pin(async move {
            fetch::fetch_trades(self, ticker_info, from_time, before_time, data_path).await
        })
    }

    fn fetch_latest_quote(
        &mut self,
        ticker_info: TickerInfo,
    ) -> futures::future::BoxFuture<'_, Result<Option<codec::QuotePayload>, AdapterError>> {
        Box::pin(async move { fetch::fetch_latest_quote(self, ticker_info).await })
    }

    fn fetch_premarket(
        &mut self,
        ticker_info: TickerInfo,
    ) -> futures::future::BoxFuture<'_, Result<Vec<codec::PremarketPayload>, AdapterError>> {
        Box::pin(async move { fetch::fetch_premarket(self, ticker_info).await })
    }
}
