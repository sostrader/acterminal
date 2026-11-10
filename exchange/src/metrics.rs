//! Leak-hunting counters for the transport layer: lock-free, always on,
//! negligible cost. The UI layer logs a combined snapshot every 60s (see
//! `crate::metrics`), so a leak shows up as a monotonically climbing counter
//! instead of guesswork.

use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! counter {
    ($static_name:ident, $fn_name:ident) => {
        static $static_name: AtomicU64 = AtomicU64::new(0);

        pub fn $fn_name() {
            $static_name.fetch_add(1, Ordering::Relaxed);
        }
    };
}

counter!(SESSIONS_OPENED, session_opened);
counter!(SESSIONS_CLOSED, session_closed);
counter!(SUBS_DECLARED, sub_declared);
counter!(EVENTS_DROPPED, event_dropped);

/// Snapshot of transport counters: `open=X closed=Y subs=Z dropped=E`.
/// Healthy steady state: `opened == closed` over time and `dropped == 0`.
/// A leak shows up as `opened - closed` (or `subs`) climbing without bound.
pub fn snapshot() -> String {
    format!(
        "sess open={} closed={} subs={} dropped={}",
        SESSIONS_OPENED.load(Ordering::Relaxed),
        SESSIONS_CLOSED.load(Ordering::Relaxed),
        SUBS_DECLARED.load(Ordering::Relaxed),
        EVENTS_DROPPED.load(Ordering::Relaxed),
    )
}
