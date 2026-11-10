//! Leak-hunting counters for the UI/fetch layer: lock-free, always on,
//! negligible cost. `Dashboard::tick` logs one combined snapshot line every
//! 60s (`metrics: ...`), so a leak shows up as a monotonically climbing
//! counter instead of guesswork.

use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! counter {
    ($static_name:ident, $fn_name:ident) => {
        static $static_name: AtomicU64 = AtomicU64::new(0);

        pub fn $fn_name() {
            $static_name.fetch_add(1, Ordering::Relaxed);
        }
    };
}

counter!(FETCH_DISPATCHED_KLINE, fetch_dispatched_kline);
counter!(FETCH_DISPATCHED_TRADES, fetch_dispatched_trades);
counter!(FETCH_DISPATCHED_OTHER, fetch_dispatched_other);
counter!(FETCH_DATA, fetch_data);
counter!(FETCH_ERRORS, fetch_error);
counter!(TOASTS_PUSHED, toast_pushed);
counter!(REQUESTS_ADDED, request_added);
counter!(PANE_INVALIDATES, pane_invalidate);
counter!(RESOLVE_STREAMS, resolve_streams);

/// Snapshot of UI/fetch counters. Healthy idle: all deltas ~0 between lines
/// except `inval` (pane ticks: ~1/s per Kline/Comparison pane, ~10/s per
/// Ladder/T&S/Micro pane, ~60/s per Bookmap pane).
pub fn snapshot() -> String {
    format!(
        "fetch kline={} trades={} other={} data={} err={} req={} | ui inval={} resolves={} toasts={} | {}",
        FETCH_DISPATCHED_KLINE.load(Ordering::Relaxed),
        FETCH_DISPATCHED_TRADES.load(Ordering::Relaxed),
        FETCH_DISPATCHED_OTHER.load(Ordering::Relaxed),
        FETCH_DATA.load(Ordering::Relaxed),
        FETCH_ERRORS.load(Ordering::Relaxed),
        REQUESTS_ADDED.load(Ordering::Relaxed),
        PANE_INVALIDATES.load(Ordering::Relaxed),
        RESOLVE_STREAMS.load(Ordering::Relaxed),
        TOASTS_PUSHED.load(Ordering::Relaxed),
        exchange::metrics::snapshot(),
    )
}
