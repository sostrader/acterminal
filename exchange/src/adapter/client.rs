use super::{
    AdapterError, Event, Exchange, MarketKind, StreamConfig, StreamKind, StreamTicksize, Venue,
    hub::acfeed,
};
use crate::{Kline, TickMultiplier, Ticker, TickerInfo, TickerStats, Timeframe, Trade, UnixMs};

use futures::{StreamExt, stream, stream::BoxStream};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

// Keep topics per websocket conservative across venues
// allow up to 100 tickers per websocket stream
pub const MAX_TRADE_TICKERS_PER_STREAM: usize = 100;
pub const MAX_KLINE_STREAMS_PER_STREAM: usize = 100;

#[derive(Clone)]
pub struct AdapterHandles {
    acfeed: Option<acfeed::AcfeedHandle>,
}

impl AdapterHandles {
    pub fn spawn_venue(
        &mut self,
        venue: Venue,
        proxy: Option<&super::proxy::Proxy>,
    ) -> Result<(), AdapterError> {
        match venue {
            Venue::Acfeed => {
                self.acfeed = Some(acfeed::AcfeedHandle::new(proxy)?);
            }
        }

        Ok(())
    }

    pub fn spawn_venues(
        venues: impl IntoIterator<Item = Venue>,
        proxy: Option<&super::proxy::Proxy>,
    ) -> Self {
        let mut out = Self { acfeed: None };

        for venue in venues {
            if let Err(err) = out.spawn_venue(venue, proxy) {
                log::error!("Failed to spawn {venue} adapter: {err}");
            }
        }

        out
    }

    fn missing_venue_stream(
        exchange: Exchange,
        streams: Arc<[StreamKind]>,
    ) -> BoxStream<'static, Event> {
        let err = format!("Adapter unavailable for {}", exchange.venue());
        stream::once(async move { Event::Disconnected(streams, err) }).boxed()
    }

    fn missing_venue_error(venue: Venue) -> AdapterError {
        let err = format!("Adapter unavailable for venue {venue}");
        AdapterError::unavailable(venue, err)
    }

    fn depth_scope(config: &StreamConfig<TickerInfo>) -> Arc<[StreamKind]> {
        let ticker_info = config.id;
        let depth_aggr = if config.exchange.is_depth_client_aggr() {
            StreamTicksize::Client
        } else {
            StreamTicksize::ServerSide(config.tick_mltp.unwrap_or(TickMultiplier(1)))
        };

        Arc::from(
            vec![StreamKind::Depth {
                ticker_info,
                depth_aggr,
                push_freq: config.push_freq,
            }]
            .into_boxed_slice(),
        )
    }

    fn trade_scope(config: &StreamConfig<Vec<TickerInfo>>) -> Arc<[StreamKind]> {
        Arc::from(
            config
                .id
                .iter()
                .map(|ticker_info| StreamKind::Trades {
                    ticker_info: *ticker_info,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    fn kline_scope(config: &StreamConfig<Vec<(TickerInfo, Timeframe)>>) -> Arc<[StreamKind]> {
        Arc::from(
            config
                .id
                .iter()
                .map(|(ticker_info, timeframe)| StreamKind::Kline {
                    ticker_info: *ticker_info,
                    timeframe: *timeframe,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    pub fn kline_stream(
        &self,
        config: &StreamConfig<Vec<(TickerInfo, Timeframe)>>,
    ) -> BoxStream<'static, Event> {
        let stream_scope = Self::kline_scope(config);
        let streams = config.id.clone();

        let missing_venue_stream =
            || Self::missing_venue_stream(config.exchange, stream_scope.clone());

        match config.exchange.venue() {
            Venue::Acfeed => self
                .acfeed
                .clone()
                .map_or_else(missing_venue_stream, |handle| {
                    handle.connect_kline_stream(streams).boxed()
                }),
        }
    }

    pub fn trade_stream(
        &self,
        config: &StreamConfig<Vec<TickerInfo>>,
    ) -> BoxStream<'static, Event> {
        let stream_scope = Self::trade_scope(config);
        let streams = config.id.clone();

        let missing_venue_stream =
            || Self::missing_venue_stream(config.exchange, stream_scope.clone());

        match config.exchange.venue() {
            Venue::Acfeed => self
                .acfeed
                .clone()
                .map_or_else(missing_venue_stream, |handle| {
                    handle.connect_trade_stream(streams).boxed()
                }),
        }
    }

    pub fn depth_stream(&self, config: &StreamConfig<TickerInfo>) -> BoxStream<'static, Event> {
        let stream_scope = Self::depth_scope(config);
        let ticker_info = config.id;
        let push_freq = config.push_freq;
        let depth_aggr = if config.exchange.is_depth_client_aggr() {
            StreamTicksize::Client
        } else {
            StreamTicksize::ServerSide(config.tick_mltp.unwrap_or(TickMultiplier(1)))
        };

        let missing_venue_stream =
            || Self::missing_venue_stream(config.exchange, stream_scope.clone());

        match config.exchange.venue() {
            Venue::Acfeed => self
                .acfeed
                .clone()
                .map_or_else(missing_venue_stream, |handle| {
                    handle
                        .connect_depth_stream(ticker_info, depth_aggr, push_freq)
                        .boxed()
                }),
        }
    }

    pub fn quote_stream(&self, config: &StreamConfig<TickerInfo>) -> BoxStream<'static, Event> {
        let ticker_info = config.id;
        let stream_scope: Arc<[StreamKind]> =
            Arc::from(vec![StreamKind::Quote { ticker_info }].into_boxed_slice());

        let missing_venue_stream =
            || Self::missing_venue_stream(config.exchange, stream_scope.clone());

        match config.exchange.venue() {
            Venue::Acfeed => self
                .acfeed
                .clone()
                .map_or_else(missing_venue_stream, |handle| {
                    handle.connect_quote_stream(ticker_info).boxed()
                }),
        }
    }

    /// Returns a map of tickers to their [`TickerInfo`].
    /// If metadata for a ticker can't be fetched/parsed expectedly, it will still be included in the map as `None`.
    pub async fn fetch_ticker_metadata(
        &self,
        venue: Venue,
        _markets: &[MarketKind],
    ) -> Result<HashMap<Ticker, Option<TickerInfo>>, AdapterError> {
        match venue {
            Venue::Acfeed => {
                let Some(handle) = self.acfeed.as_ref() else {
                    return Err(Self::missing_venue_error(venue));
                };
                handle.fetch_ticker_metadata().await
            }
        }
    }

    /// Returns a map of tickers to their [`TickerStats`].
    pub async fn fetch_ticker_stats(
        &self,
        venue: Venue,
        _markets: &[MarketKind],
        _contract_sizes: Option<HashMap<Ticker, crate::unit::ContractSize>>,
    ) -> Result<HashMap<Ticker, TickerStats>, AdapterError> {
        match venue {
            Venue::Acfeed => {
                let Some(handle) = self.acfeed.as_ref() else {
                    return Err(Self::missing_venue_error(venue));
                };
                handle.fetch_ticker_stats().await
            }
        }
    }

    pub async fn fetch_klines(
        &self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        range: Option<(UnixMs, UnixMs)>,
    ) -> Result<Vec<Kline>, AdapterError> {
        let venue = ticker_info.ticker.exchange.venue();

        match venue {
            Venue::Acfeed => {
                let Some(handle) = self.acfeed.as_ref() else {
                    return Err(Self::missing_venue_error(venue));
                };
                handle.fetch_klines(ticker_info, timeframe, range).await
            }
        }
    }

    pub async fn fetch_trades(
        &self,
        ticker_info: TickerInfo,
        from_time: UnixMs,
        before_time: UnixMs,
        data_path: Option<PathBuf>,
    ) -> Result<Vec<Trade>, AdapterError> {
        let exchange = ticker_info.ticker.exchange;

        match exchange.venue() {
            Venue::Acfeed => {
                let Some(handle) = self.acfeed.as_ref() else {
                    return Err(Self::missing_venue_error(exchange.venue()));
                };
                handle
                    .fetch_trades(ticker_info, from_time, before_time, data_path)
                    .await
            }
        }
    }

    /// Latest stored session quote, for bootstrapping the open/high/low readouts.
    pub async fn fetch_latest_quote(
        &self,
        ticker_info: TickerInfo,
    ) -> Result<Option<super::acfeed::codec::QuotePayload>, AdapterError> {
        let exchange = ticker_info.ticker.exchange;

        match exchange.venue() {
            Venue::Acfeed => {
                let Some(handle) = self.acfeed.as_ref() else {
                    return Err(Self::missing_venue_error(exchange.venue()));
                };
                handle.fetch_latest_quote(ticker_info).await
            }
        }
    }

    /// Persisted pre-open auction series, for bootstrapping the premarket levels
    /// indicator on cold start.
    pub async fn fetch_premarket(
        &self,
        ticker_info: TickerInfo,
    ) -> Result<Vec<crate::adapter::hub::acfeed::codec::PremarketPayload>, AdapterError> {
        let exchange = ticker_info.ticker.exchange;

        match exchange.venue() {
            Venue::Acfeed => {
                let Some(handle) = self.acfeed.as_ref() else {
                    return Err(Self::missing_venue_error(exchange.venue()));
                };
                handle.fetch_premarket(ticker_info).await
            }
        }
    }
}

impl Default for AdapterHandles {
    fn default() -> Self {
        Self::spawn_venues(Venue::ALL, None)
    }
}

impl std::hash::Hash for AdapterHandles {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::any::TypeId::of::<Self>().hash(state);
    }
}
