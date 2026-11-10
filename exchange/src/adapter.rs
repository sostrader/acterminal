mod client;
mod http;
mod hub;
pub mod proxy;

use super::Timeframe;
pub use super::error::AdapterError;
use crate::{Kline, PushFrequency, TickMultiplier, TickerInfo, Trade, UnixMs, depth::Depth};

use enum_map::{Enum, EnumMap};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc};

pub use client::{AdapterHandles, MAX_KLINE_STREAMS_PER_STREAM, MAX_TRADE_TICKERS_PER_STREAM};
pub use hub::acfeed;
pub use proxy::Proxy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum MarketKind {
    Spot,
    LinearPerps,
    InversePerps,
}

impl MarketKind {
    pub const ALL: [MarketKind; 3] = [
        MarketKind::Spot,
        MarketKind::LinearPerps,
        MarketKind::InversePerps,
    ];
}

impl std::fmt::Display for MarketKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                MarketKind::Spot => "Spot",
                MarketKind::LinearPerps => "Linear",
                MarketKind::InversePerps => "Inverse",
            }
        )
    }
}

impl FromStr for MarketKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("spot") {
            Ok(Self::Spot)
        } else if s.eq_ignore_ascii_case("linear") {
            Ok(Self::LinearPerps)
        } else if s.eq_ignore_ascii_case("inverse") {
            Ok(Self::InversePerps)
        } else {
            Err(format!("Invalid market kind: {}", s))
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum StreamKind {
    Kline {
        ticker_info: TickerInfo,
        timeframe: Timeframe,
    },
    Depth {
        ticker_info: TickerInfo,
        #[serde(default = "default_depth_aggr")]
        depth_aggr: StreamTicksize,
        push_freq: PushFrequency,
    },
    Trades {
        ticker_info: TickerInfo,
    },
    /// Session statistics (open/high/low, best bid/ask, last) off the venue's `quotes/`
    /// topic. Separate from `Trades` because these are the exchange's own session
    /// aggregates, not something derivable from the tape.
    Quote {
        ticker_info: TickerInfo,
    },
}

impl StreamKind {
    pub fn ticker_info(&self) -> TickerInfo {
        match self {
            StreamKind::Kline { ticker_info, .. }
            | StreamKind::Depth { ticker_info, .. }
            | StreamKind::Trades { ticker_info, .. }
            | StreamKind::Quote { ticker_info, .. } => *ticker_info,
        }
    }

    pub fn as_depth_stream(&self) -> Option<(TickerInfo, StreamTicksize, PushFrequency)> {
        match self {
            StreamKind::Depth {
                ticker_info,
                depth_aggr,
                push_freq,
            } => Some((*ticker_info, *depth_aggr, *push_freq)),
            _ => None,
        }
    }

    pub fn as_trade_stream(&self) -> Option<TickerInfo> {
        match self {
            StreamKind::Trades { ticker_info } => Some(*ticker_info),
            _ => None,
        }
    }

    pub fn as_quote_stream(&self) -> Option<TickerInfo> {
        match self {
            StreamKind::Quote { ticker_info } => Some(*ticker_info),
            _ => None,
        }
    }

    pub fn as_kline_stream(&self) -> Option<(TickerInfo, Timeframe)> {
        match self {
            StreamKind::Kline {
                ticker_info,
                timeframe,
            } => Some((*ticker_info, *timeframe)),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct UniqueStreams {
    streams: EnumMap<Exchange, Option<FxHashMap<TickerInfo, FxHashSet<StreamKind>>>>,
    specs: EnumMap<Exchange, Option<StreamSpecs>>,
}

impl UniqueStreams {
    pub fn from<'a>(streams: impl Iterator<Item = &'a StreamKind>) -> Self {
        let mut unique_streams = UniqueStreams::default();
        for stream in streams {
            unique_streams.add(*stream);
        }
        unique_streams
    }

    pub fn add(&mut self, stream: StreamKind) {
        let (exchange, ticker_info) = match stream {
            StreamKind::Kline { ticker_info, .. }
            | StreamKind::Depth { ticker_info, .. }
            | StreamKind::Trades { ticker_info, .. }
            | StreamKind::Quote { ticker_info, .. } => (ticker_info.exchange(), ticker_info),
        };

        self.streams[exchange]
            .get_or_insert_with(FxHashMap::default)
            .entry(ticker_info)
            .or_default()
            .insert(stream);

        self.update_specs_for_exchange(exchange);
    }

    pub fn extend<'a>(&mut self, streams: impl IntoIterator<Item = &'a StreamKind>) {
        for stream in streams {
            self.add(*stream);
        }
    }

    fn update_specs_for_exchange(&mut self, exchange: Exchange) {
        let depth_streams = self.depth_streams(Some(exchange));
        let trade_streams = self.trade_streams(Some(exchange));
        let kline_streams = self.kline_streams(Some(exchange));
        let quote_streams = self.quote_streams(Some(exchange));

        self.specs[exchange] = Some(StreamSpecs {
            depth: depth_streams,
            trade: trade_streams,
            kline: kline_streams,
            quote: quote_streams,
        });
    }

    fn streams<T, F>(&self, exchange_filter: Option<Exchange>, stream_extractor: F) -> Vec<T>
    where
        F: Fn(Exchange, &StreamKind) -> Option<T>,
    {
        let f = &stream_extractor;

        let per_exchange = |exchange| {
            // The underlying maps/sets are hash-ordered, so flatten without a
            // sort yields a nondeterministic order on every rebuild. That
            // order feeds subscription chunking (and therefore subscription
            // identity): iced would tear down and recreate live streams on
            // every refresh, leaking a session + task set per churn.
            let mut ordered: Vec<&StreamKind> = self.streams[exchange]
                .as_ref()
                .into_iter()
                .flat_map(|ticker_map| ticker_map.values().flatten())
                .collect();
            ordered.sort_by_cached_key(|stream| format!("{stream:?}"));
            ordered
                .into_iter()
                .filter_map(move |stream| f(exchange, stream))
        };

        match exchange_filter {
            Some(exchange) => per_exchange(exchange).collect(),
            None => Exchange::ALL.into_iter().flat_map(per_exchange).collect(),
        }
    }

    pub fn depth_streams(
        &self,
        exchange_filter: Option<Exchange>,
    ) -> Vec<(TickerInfo, StreamTicksize, PushFrequency)> {
        self.streams(exchange_filter, |_, stream| stream.as_depth_stream())
    }

    pub fn kline_streams(&self, exchange_filter: Option<Exchange>) -> Vec<(TickerInfo, Timeframe)> {
        self.streams(exchange_filter, |_, stream| stream.as_kline_stream())
    }

    pub fn trade_streams(&self, exchange_filter: Option<Exchange>) -> Vec<TickerInfo> {
        self.streams(exchange_filter, |_, stream| stream.as_trade_stream())
    }

    pub fn quote_streams(&self, exchange_filter: Option<Exchange>) -> Vec<TickerInfo> {
        self.streams(exchange_filter, |_, stream| stream.as_quote_stream())
    }

    pub fn combined_used(&self) -> impl Iterator<Item = (Exchange, &StreamSpecs)> {
        self.specs
            .iter()
            .filter_map(|(exchange, specs)| specs.as_ref().map(|stream| (exchange, stream)))
    }

    pub fn combined(&self) -> &EnumMap<Exchange, Option<StreamSpecs>> {
        &self.specs
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum StreamTicksize {
    ServerSide(TickMultiplier),
    #[default]
    Client,
}

fn default_depth_aggr() -> StreamTicksize {
    StreamTicksize::Client
}

#[derive(Debug, Clone, Default)]
pub struct StreamSpecs {
    pub depth: Vec<(TickerInfo, StreamTicksize, PushFrequency)>,
    pub trade: Vec<TickerInfo>,
    pub kline: Vec<(TickerInfo, Timeframe)>,
    pub quote: Vec<TickerInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Venue {
    Acfeed,
}

impl Venue {
    pub const ALL: [Venue; 1] = [Venue::Acfeed];
}

impl std::fmt::Display for Venue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Venue::Acfeed => "Acfeed",
            }
        )
    }
}

impl FromStr for Venue {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("acfeed") {
            Ok(Self::Acfeed)
        } else {
            Err(format!("Invalid venue: {}", s))
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Enum, Ord, PartialOrd,
)]
pub enum Exchange {
    AcfeedFutures,
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.venue(), self.market_type())
    }
}

impl FromStr for Exchange {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split_whitespace();
        let Some(venue_part) = parts.next() else {
            return Err(format!("Invalid exchange: {}", s));
        };
        let Some(market_part) = parts.next() else {
            return Err(format!("Invalid exchange: {}", s));
        };

        if parts.next().is_some() {
            return Err(format!("Invalid exchange: {}", s));
        }

        let venue = Venue::from_str(venue_part).map_err(|_| format!("Invalid exchange: {}", s))?;
        let market =
            MarketKind::from_str(market_part).map_err(|_| format!("Invalid exchange: {}", s))?;

        Self::from_venue_and_market(venue, market).ok_or_else(|| format!("Invalid exchange: {}", s))
    }
}

impl Exchange {
    pub const ALL: [Exchange; 1] = [Exchange::AcfeedFutures];

    pub fn from_venue_and_market(venue: Venue, market: MarketKind) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|exchange| exchange.venue() == venue && exchange.market_type() == market)
    }

    pub fn market_type(&self) -> MarketKind {
        match self {
            // CME FX-futures notional math matches the linear-perps qty-normalization
            // branch (contract_size * price), so this doesn't need its own MarketKind.
            Exchange::AcfeedFutures => MarketKind::LinearPerps,
        }
    }

    pub fn venue(&self) -> Venue {
        match self {
            Exchange::AcfeedFutures => Venue::Acfeed,
        }
    }

    pub fn is_depth_client_aggr(&self) -> bool {
        match self {
            // No server-side depth-level aggregation protocol exists for the Zenoh feed.
            Exchange::AcfeedFutures => true,
        }
    }

    pub fn is_custom_push_freq(&self) -> bool {
        match self {
            Exchange::AcfeedFutures => false,
        }
    }

    pub fn supports_heatmap_timeframe(&self, _tf: Timeframe) -> bool {
        match self {
            Exchange::AcfeedFutures => true,
        }
    }

    pub fn supports_kline_timeframe(&self, tf: Timeframe) -> bool {
        match self.venue() {
            Venue::Acfeed => Timeframe::KLINE.contains(&tf),
        }
    }

    pub fn is_perps(&self) -> bool {
        match self {
            Exchange::AcfeedFutures => true,
        }
    }

    pub fn stream_ticksize(
        &self,
        multiplier: Option<TickMultiplier>,
        server_fallback: TickMultiplier,
    ) -> StreamTicksize {
        if self.is_depth_client_aggr() {
            StreamTicksize::Client
        } else {
            StreamTicksize::ServerSide(multiplier.unwrap_or(server_fallback))
        }
    }

    pub fn allowed_tick_multipliers(
        &self,
        min_ticksize: Option<super::unit::PriceStep>,
    ) -> Vec<TickMultiplier> {
        if self.is_depth_client_aggr() {
            return TickMultiplier::ALL.to_vec();
        }

        let Some(_min_tick) = min_ticksize else {
            return vec![];
        };

        TickMultiplier::ALL.to_vec()
    }

    pub fn is_symbol_supported(&self, symbol: &str, log: bool) -> bool {
        let valid_symbol = symbol
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-');

        if valid_symbol {
            return true;
        } else if log {
            log::warn!("Unsupported ticker: '{}': {:?}", self, symbol,);
        }
        false
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected(Arc<[StreamKind]>),
    Disconnected(Arc<[StreamKind]>, String),
    DepthReceived(StreamKind, UnixMs, Arc<Depth>),
    TradesReceived(StreamKind, UnixMs, Box<[Trade]>),
    KlineReceived(StreamKind, Kline),
    QuoteReceived(StreamKind, hub::acfeed::codec::QuotePayload),
}

#[derive(Debug, Clone, Hash)]
pub struct StreamConfig<I> {
    pub id: I,
    pub exchange: Exchange,
    pub tick_mltp: Option<TickMultiplier>,
    pub push_freq: PushFrequency,
}

impl<I> StreamConfig<I> {
    pub fn new(
        id: I,
        exchange: Exchange,
        tick_mltp: Option<TickMultiplier>,
        push_freq: PushFrequency,
    ) -> Self {
        Self {
            id,
            exchange,
            tick_mltp,
            push_freq,
        }
    }
}
