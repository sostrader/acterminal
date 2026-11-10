use exchange::adapter::{AdapterError, AdapterHandles, StreamKind};
use exchange::{Kline, TickerInfo, Trade, UnixMs};
use iced::{
    Task,
    task::{Handle, Straw, sipper},
};
use rustc_hash::FxHashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

// Defaults to enabled: Acfeed's `hist/tick` queryable covers all 9 products, so
// footprint/time&sales panes should backfill trade history automatically rather than
// requiring a manual toggle every session.
static TRADE_FETCH_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn toggle_trade_fetch(value: bool) {
    TRADE_FETCH_ENABLED.store(value, Ordering::Relaxed);
}

pub fn is_trade_fetch_enabled() -> bool {
    TRADE_FETCH_ENABLED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub enum FetchedData {
    Trades {
        batch: Vec<Trade>,
        until_time: UnixMs,
    },
    Klines {
        data: Vec<Kline>,
        req_id: Option<uuid::Uuid>,
    },
    LatestQuote(Box<exchange::adapter::acfeed::codec::QuotePayload>),
    /// Persisted pre-open auction series, for bootstrapping the premarket
    /// levels indicator (summed sizes per snapshot, the Theoric Targets input)
    /// and MAGNETS (full book per snapshot).
    Premarket(Vec<exchange::adapter::acfeed::codec::PremarketPayload>),
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ReqError {
    #[error("Request is already failed: {0}")]
    Failed(String),
    #[error("Request overlaps with an existing request")]
    Overlaps,
}

// Retry cooldown for completed/failed requests (avoids hammering the source on flaky
// data). `PENDING_TIMEOUT_MS` is a safety net for requests stuck `Pending` forever —
// e.g. a fetch whose result never reaches `mark_completed`/`mark_failed` (a dropped
// task, or an adapter error path that doesn't route back to this handler) would
// otherwise block that exact range from ever being retried.
const RETRY_COOLDOWN_MS: u64 = 30_000;
const PENDING_TIMEOUT_MS: u64 = 20_000;
/// Terminal (completed/failed) request entries older than this are evicted.
/// Well past the retry cooldown, so no in-flight or retryable slot is lost.
const REQUEST_GC_AFTER_MS: u64 = 300_000;
/// Hard cap for the request map; without it every distinct fetch range leaks
/// one entry for the pane's lifetime.
const MAX_REQUESTS: usize = 256;

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis() as u64
}

#[derive(PartialEq, Debug, Clone)]
enum RequestStatus {
    Pending(u64),
    Completed(u64),
    Failed(u64, String),
}

#[derive(Default)]
pub struct RequestHandler {
    requests: FxHashMap<Uuid, FetchRequest>,
}

impl RequestHandler {
    /// Evicts stale terminal entries and enforces [`MAX_REQUESTS`].
    /// In-flight (`Pending`) slots are never dropped.
    fn gc_requests(&mut self, now: u64) {
        self.requests.retain(|_, req| match &req.status {
            RequestStatus::Pending(_) => true,
            RequestStatus::Completed(ts) | RequestStatus::Failed(ts, _) => {
                now.saturating_sub(*ts) < REQUEST_GC_AFTER_MS
            }
        });
        if self.requests.len() > MAX_REQUESTS {
            let mut terminal: Vec<(Uuid, u64)> = self
                .requests
                .iter()
                .filter_map(|(id, req)| match &req.status {
                    RequestStatus::Completed(ts) | RequestStatus::Failed(ts, _) => Some((*id, *ts)),
                    RequestStatus::Pending(_) => None,
                })
                .collect();
            terminal.sort_by_key(|(_, ts)| *ts);
            let excess = self.requests.len().saturating_sub(MAX_REQUESTS);
            for (id, _) in terminal.into_iter().take(excess) {
                self.requests.remove(&id);
            }
        }
    }

    pub fn add_request(&mut self, fetch: FetchRange) -> Result<Option<Uuid>, ReqError> {
        let request = FetchRequest::new(fetch);
        let id = Uuid::new_v4();

        self.gc_requests(now_ms());

        let existing = self
            .requests
            .iter()
            .find(|(_, v)| v.same_with(&request))
            .map(|(k, v)| (*k, v.status.clone()));

        if let Some((existing_id, status)) = existing {
            let now = now_ms();

            let decision = match status {
                RequestStatus::Failed(ts, error_msg) => {
                    if now - ts > RETRY_COOLDOWN_MS {
                        Ok(Some(existing_id))
                    } else {
                        Err(ReqError::Failed(error_msg))
                    }
                }
                RequestStatus::Completed(ts) => {
                    // retry completed requests after a cooldown
                    // to handle data source failures or outdated results gracefully
                    if now - ts > RETRY_COOLDOWN_MS {
                        Ok(Some(existing_id))
                    } else {
                        Ok(None)
                    }
                }
                RequestStatus::Pending(started_at) => {
                    if now - started_at > PENDING_TIMEOUT_MS {
                        Ok(Some(existing_id))
                    } else {
                        Err(ReqError::Overlaps)
                    }
                }
            };

            // Re-arm the reused slot as freshly pending so a concurrent lookup
            // doesn't also see it as expired before the retry resolves.
            if let Ok(Some(retry_id)) = decision
                && let Some(req) = self.requests.get_mut(&retry_id)
            {
                req.status = RequestStatus::Pending(now);
            }

            return decision;
        }

        self.requests.insert(id, request);
        crate::metrics::request_added();
        Ok(Some(id))
    }

    pub fn mark_completed(&mut self, id: Uuid) {
        if let Some(request) = self.requests.get_mut(&id) {
            request.status = RequestStatus::Completed(now_ms());
        } else {
            log::warn!("Request not found: {:?}", id);
        }
    }

    pub fn mark_failed(&mut self, id: Uuid, error: String) {
        if let Some(request) = self.requests.get_mut(&id) {
            request.status = RequestStatus::Failed(now_ms(), error);
        } else {
            log::warn!("Request not found: {:?}", id);
        }
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum FetchRange {
    /// `None` for the start means "don't know where real data starts yet" (a freshly
    /// opened pane/series with nothing loaded): the fetch falls back to persist's full
    /// retention window and lets its `ORDER BY ts DESC LIMIT` surface whatever the most
    /// recent real data actually is. A concrete start is for backfills bounded by data
    /// already on screen (e.g. "fill the gap before the earliest kline we have"), where
    /// guessing `now() - span` would be wrong the moment `now()` itself falls in a
    /// weekend/holiday gap with no data — the fetch would never reach the last real
    /// session.
    Kline(Option<UnixMs>, UnixMs),
    Trades(UnixMs, UnixMs),
    /// Latest stored session quote. Carries no range: only the newest one describes
    /// the session as it stands.
    LatestQuote,
    /// Persisted pre-open auction series, the only source for the premarket levels
    /// indicator (fetch-only, no live stream).
    Premarket,
}

#[derive(PartialEq, Debug)]
struct FetchRequest {
    fetch_type: FetchRange,
    status: RequestStatus,
}

impl FetchRequest {
    fn new(fetch_type: FetchRange) -> Self {
        FetchRequest {
            fetch_type,
            status: RequestStatus::Pending(now_ms()),
        }
    }

    fn same_with(&self, other: &FetchRequest) -> bool {
        // `FetchRange` is `PartialEq`: identical ranges of any kind (Kline,
        // Trades, LatestQuote, Premarket) share one slot. Previously only
        // `Kline` deduped, so every trade/quote/premarket fetch leaked a fresh
        // entry even for the same range.
        self.fetch_type == other.fetch_type
    }
}

pub struct FetchSpec {
    pub req_id: uuid::Uuid,
    pub fetch: FetchRange,
    pub stream: Option<StreamKind>,
}

impl From<(uuid::Uuid, FetchRange, Option<StreamKind>)> for FetchSpec {
    fn from(t: (uuid::Uuid, FetchRange, Option<StreamKind>)) -> Self {
        FetchSpec {
            req_id: t.0,
            fetch: t.1,
            stream: t.2,
        }
    }
}

impl std::fmt::Debug for FetchSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchSpec")
            .field("req_id", &self.req_id)
            .field("fetch", &self.fetch)
            .field("stream", &self.stream)
            .finish()
    }
}

impl Clone for FetchSpec {
    fn clone(&self) -> Self {
        FetchSpec {
            req_id: self.req_id,
            fetch: self.fetch,
            stream: self.stream,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InfoKind {
    FetchingKlines,
    FetchingTrades(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchTaskStatus {
    Loading(InfoKind),
    Completed,
}

#[derive(Debug, Clone)]
pub enum FetchUpdate {
    Status {
        pane_id: Uuid,
        status: FetchTaskStatus,
    },
    Data {
        layout_id: Uuid,
        pane_id: Uuid,
        stream: StreamKind,
        data: FetchedData,
    },
    Error {
        pane_id: Uuid,
        error: String,
    },
}

pub fn request_fetch(
    handles: AdapterHandles,
    pane_id: Uuid,
    ready_streams: &[StreamKind],
    layout_id: Uuid,
    req_id: Uuid,
    fetch: FetchRange,
    stream: Option<StreamKind>,
    on_trade_handle: &mut impl FnMut(Handle),
) -> Task<FetchUpdate> {
    match fetch {
        FetchRange::Kline(from, to) => {
            let kline_stream = if let Some(s) = stream {
                Some((s, pane_id))
            } else {
                ready_streams.iter().find_map(|stream| {
                    if let StreamKind::Kline { .. } = stream {
                        Some((*stream, pane_id))
                    } else {
                        None
                    }
                })
            };

            if let Some((stream, pane_uid)) = kline_stream {
                return kline_fetch_task(
                    handles.clone(),
                    layout_id,
                    pane_uid,
                    stream,
                    Some(req_id),
                    from.map(|from| (from, to)),
                );
            }
        }
        FetchRange::Trades(from_time, to_time) => {
            let trade_info = ready_streams.iter().find_map(|stream| {
                if let StreamKind::Trades { ticker_info } = stream {
                    Some((*ticker_info, pane_id, *stream))
                } else {
                    None
                }
            });

            if let Some((ticker_info, pane_id, stream)) = trade_info {
                crate::metrics::fetch_dispatched_trades();
                let data_path = data::data_path(Some("market_data/acfeed/"));

                let (task, handle) = Task::sip(
                    fetch_trades_batched(
                        handles.clone(),
                        ticker_info,
                        from_time,
                        to_time,
                        data_path,
                    ),
                    move |batch| {
                        let until_time = batch.last().map_or(to_time, |trade| trade.time);

                        let data = FetchedData::Trades { batch, until_time };

                        FetchUpdate::Data {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }
                    },
                    move |result| match result {
                        Ok(()) => FetchUpdate::Status {
                            pane_id,
                            status: FetchTaskStatus::Completed,
                        },
                        Err(err) => {
                            log::error!("Trade fetch failed: {err}");
                            FetchUpdate::Error {
                                pane_id,
                                error: err.ui_message(),
                            }
                        }
                    },
                )
                .abortable();

                on_trade_handle(handle.abort_on_drop());

                return task;
            }
        }
        FetchRange::LatestQuote => {
            let quote_stream = ready_streams
                .iter()
                .find(|stream| matches!(stream, StreamKind::Quote { .. }));

            if let Some(stream) = quote_stream.copied() {
                crate::metrics::fetch_dispatched_other();
                let ticker_info = stream.ticker_info();

                return Task::perform(
                    async move { handles.fetch_latest_quote(ticker_info).await },
                    move |result| match result {
                        Ok(Some(quote)) => FetchUpdate::Data {
                            layout_id,
                            pane_id,
                            data: FetchedData::LatestQuote(Box::new(quote)),
                            stream,
                        },
                        // Nothing stored yet is normal on a cold `persist`; the pane
                        // just keeps waiting for the live quote as it did before.
                        Ok(None) => FetchUpdate::Status {
                            pane_id,
                            status: FetchTaskStatus::Completed,
                        },
                        Err(err) => {
                            log::error!("Quote fetch failed: {err}");
                            FetchUpdate::Error {
                                pane_id,
                                error: err.ui_message(),
                            }
                        }
                    },
                );
            }
        }
        FetchRange::Premarket => {
            // Fetch-only (`hist/premarket`): ticker comes from the pane's own
            // streams (Kline first, else whatever is ready), never from a live
            // `premarket/` subscription.
            let resolved = stream.map(|s| (s.ticker_info(), s)).or_else(|| {
                ready_streams
                    .iter()
                    .find(|s| matches!(s, StreamKind::Kline { .. }))
                    .or_else(|| ready_streams.first())
                    .map(|s| (s.ticker_info(), *s))
            });

            if let Some((ticker_info, stream)) = resolved {
                crate::metrics::fetch_dispatched_other();
                let task = Task::perform(
                    async move { handles.fetch_premarket(ticker_info).await },
                    move |result| match result {
                        Ok(points) => FetchUpdate::Data {
                            layout_id,
                            pane_id,
                            data: FetchedData::Premarket(points),
                            stream,
                        },
                        Err(err) => {
                            log::error!("Premarket fetch failed: {err}");
                            FetchUpdate::Error {
                                pane_id,
                                error: err.ui_message(),
                            }
                        }
                    },
                );

                return task;
            }
        }
    }

    Task::none()
}

pub fn request_fetch_many(
    handles: AdapterHandles,
    pane_id: Uuid,
    ready_streams: &[StreamKind],
    layout_id: Uuid,
    reqs: impl IntoIterator<Item = (Uuid, FetchRange, Option<StreamKind>)>,
    mut on_trade_handle: impl FnMut(Handle),
) -> Task<FetchUpdate> {
    let mut tasks = Vec::new();

    for (req_id, fetch, stream) in reqs {
        tasks.push(request_fetch(
            handles.clone(),
            pane_id,
            ready_streams,
            layout_id,
            req_id,
            fetch,
            stream,
            &mut on_trade_handle,
        ));
    }

    Task::batch(tasks)
}

pub fn kline_fetch_task(
    handles: AdapterHandles,
    layout_id: Uuid,
    pane_id: Uuid,
    stream: StreamKind,
    req_id: Option<Uuid>,
    range: Option<(UnixMs, UnixMs)>,
) -> Task<FetchUpdate> {
    crate::metrics::fetch_dispatched_kline();
    let update_status = Task::done(FetchUpdate::Status {
        pane_id,
        status: FetchTaskStatus::Loading(InfoKind::FetchingKlines),
    });

    let fetch_task = match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,
        } => {
            let fetch = async move { handles.fetch_klines(ticker_info, timeframe, range).await };

            Task::perform(
                iced::futures::TryFutureExt::map_err(fetch, |err| {
                    log::error!("Kline fetch failed: {err}");
                    err.ui_message()
                }),
                move |result| match result {
                    Ok(klines) => {
                        let data = FetchedData::Klines {
                            data: klines,
                            req_id,
                        };
                        FetchUpdate::Data {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }
                    }
                    Err(err) => FetchUpdate::Error {
                        pane_id,
                        error: err,
                    },
                },
            )
        }
        _ => Task::none(),
    };

    update_status.chain(fetch_task)
}

/// Backfills `[from_time, to_time)`, paging backward from `to_time`.
///
/// `persist`'s `hist/tick` queryable only ever answers "the newest `limit` ticks
/// strictly before `before`" (see `persist/src/query.rs`); it has no way to ask for
/// "the oldest ticks after `from_time`". Walking forward from `from_time` — which this
/// used to do, advancing past whatever the last batch's newest timestamp was — assumes
/// the source delivers chronologically from that point, which isn't how `persist`
/// behaves: a forward walk lands on whatever is most recent in the requested `days`
/// window and then immediately looks "done" (its newest timestamp already exceeds
/// `to_time`), even when the actual target window (say, Friday's close, backfilling
/// across a weekend) was never fetched at all. Paging backward from `to_time` instead
/// matches what the server can actually serve: each page's oldest timestamp becomes the
/// next page's `before`, until the walk reaches `from_time` or the source runs dry.
pub fn fetch_trades_batched(
    handles: AdapterHandles,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    to_time: UnixMs,
    data_path: PathBuf,
) -> impl Straw<(), Vec<Trade>, AdapterError> {
    sipper(async move |mut progress| {
        let mut before = to_time;

        loop {
            match handles
                .fetch_trades(ticker_info, from_time, before, Some(data_path.clone()))
                .await
            {
                Ok(batch) => {
                    if batch.is_empty() {
                        break;
                    }

                    let oldest = batch.first().map_or(from_time, |trade| trade.time);
                    let reached_start = oldest <= from_time;
                    let made_progress = oldest < before;

                    let () = progress.send(batch).await;

                    if reached_start || !made_progress {
                        // Reached the requested start, or the source ignored `before`
                        // (no backward progress) — stop instead of re-fetching the same
                        // page forever.
                        break;
                    }

                    before = oldest;
                }
                Err(err) => return Err(err),
            }
        }

        Ok(())
    })
}
