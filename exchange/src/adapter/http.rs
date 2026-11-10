use crate::adapter::AdapterError;
use crate::{Kline, Ticker, TickerInfo, TickerStats, Timeframe, Trade, UnixMs};

use futures::future::BoxFuture;
use tokio::sync::{mpsc, oneshot};

use std::collections::HashMap;
use std::path::PathBuf;

type ResponseTx<T> = oneshot::Sender<Result<T, AdapterError>>;

pub(super) type TickerMetadataMap = HashMap<Ticker, Option<TickerInfo>>;
pub(super) type TickerStatsMap = HashMap<Ticker, TickerStats>;

pub(super) enum FetchCommand<M> {
    TickerMetadata {
        market_scope: M,
        reply: ResponseTx<TickerMetadataMap>,
    },
    TickerStats {
        market_scope: M,
        reply: ResponseTx<TickerStatsMap>,
    },
    Klines {
        ticker: TickerInfo,
        timeframe: Timeframe,
        range: Option<(UnixMs, UnixMs)>,
        reply: ResponseTx<Vec<Kline>>,
    },
    Trades {
        ticker: TickerInfo,
        from_time: UnixMs,
        before_time: UnixMs,
        data_path: Option<PathBuf>,
        reply: ResponseTx<Vec<Trade>>,
    },
    /// Latest stored session quote, for panes that show the venue's own
    /// open/high/low and would otherwise sit blank until the next live quote.
    LatestQuote {
        ticker: TickerInfo,
        reply: ResponseTx<Option<crate::adapter::acfeed::codec::QuotePayload>>,
    },
    /// Persisted pre-open auction series, for bootstrapping the premarket levels
    /// indicator (history first, then live ticks take over).
    Premarket {
        ticker: TickerInfo,
        reply: ResponseTx<Vec<crate::adapter::acfeed::codec::PremarketPayload>>,
    },
}

pub(super) trait FetchCommandHandler<M> {
    fn fetch_ticker_metadata(
        &mut self,
        market_scope: M,
    ) -> BoxFuture<'_, Result<TickerMetadataMap, AdapterError>>;

    fn fetch_ticker_stats(
        &mut self,
        market_scope: M,
    ) -> BoxFuture<'_, Result<TickerStatsMap, AdapterError>>;

    fn fetch_klines(
        &mut self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        range: Option<(UnixMs, UnixMs)>,
    ) -> BoxFuture<'_, Result<Vec<Kline>, AdapterError>> {
        let _ = (ticker_info, timeframe, range);
        Box::pin(async { Err(unsupported_fetch("Kline fetch")) })
    }

    fn fetch_trades(
        &mut self,
        ticker_info: TickerInfo,
        from_time: UnixMs,
        before_time: UnixMs,
        data_path: Option<PathBuf>,
    ) -> BoxFuture<'_, Result<Vec<Trade>, AdapterError>> {
        let _ = (ticker_info, from_time, before_time, data_path);
        Box::pin(async { Err(unsupported_fetch("Trades fetch")) })
    }

    fn fetch_latest_quote(
        &mut self,
        ticker_info: TickerInfo,
    ) -> BoxFuture<'_, Result<Option<crate::adapter::acfeed::codec::QuotePayload>, AdapterError>>
    {
        let _ = ticker_info;
        Box::pin(async { Err(unsupported_fetch("Quote fetch")) })
    }

    fn fetch_premarket(
        &mut self,
        ticker_info: TickerInfo,
    ) -> BoxFuture<'_, Result<Vec<crate::adapter::acfeed::codec::PremarketPayload>, AdapterError>>
    {
        let _ = ticker_info;
        Box::pin(async { Err(unsupported_fetch("Premarket fetch")) })
    }
}

pub(super) fn spawn_fetch_worker<H, M>(mut worker: H) -> RequestPort<FetchCommand<M>>
where
    H: FetchCommandHandler<M> + Send + 'static,
    M: Send + 'static,
{
    const COMMAND_BUFFER_CAPACITY: usize = 128;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(COMMAND_BUFFER_CAPACITY);
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            handle_fetch_command(&mut worker, command).await;
        }
    });
    RequestPort::new(sender)
}

fn unsupported_fetch(feature: &'static str) -> AdapterError {
    AdapterError::InvalidRequest(format!("{feature} is not supported by this worker"))
}

async fn handle_fetch_command<H, M>(handler: &mut H, command: FetchCommand<M>)
where
    H: FetchCommandHandler<M>,
{
    match command {
        FetchCommand::TickerMetadata {
            market_scope,
            reply,
        } => {
            let result = handler.fetch_ticker_metadata(market_scope).await;
            let _ = reply.send(result);
        }
        FetchCommand::TickerStats {
            market_scope,
            reply,
        } => {
            let result = handler.fetch_ticker_stats(market_scope).await;
            let _ = reply.send(result);
        }
        FetchCommand::Klines {
            ticker,
            timeframe,
            range,
            reply,
        } => {
            let result = handler.fetch_klines(ticker, timeframe, range).await;
            let _ = reply.send(result);
        }
        FetchCommand::Trades {
            ticker,
            from_time,
            before_time,
            data_path,
            reply,
        } => {
            let result = handler
                .fetch_trades(ticker, from_time, before_time, data_path)
                .await;
            let _ = reply.send(result);
        }
        FetchCommand::LatestQuote { ticker, reply } => {
            let result = handler.fetch_latest_quote(ticker).await;
            let _ = reply.send(result);
        }
        FetchCommand::Premarket { ticker, reply } => {
            let result = handler.fetch_premarket(ticker).await;
            let _ = reply.send(result);
        }
    }
}

pub(super) struct RequestPort<C> {
    sender: mpsc::Sender<C>,
}

impl<C> Clone for RequestPort<C> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<C> RequestPort<C> {
    pub(super) fn new(sender: mpsc::Sender<C>) -> Self {
        Self { sender }
    }

    pub(super) async fn request<T>(
        &self,
        build: impl FnOnce(ResponseTx<T>) -> C,
    ) -> Result<T, AdapterError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.sender
            .send(build(reply_tx))
            .await
            .map_err(|_| AdapterError::WebsocketError("Request port is closed".to_string()))?;

        reply_rx
            .await
            .map_err(|_| AdapterError::WebsocketError("Response channel dropped".to_string()))?
    }
}
