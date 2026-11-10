//! Session-gap compression for time-based x axes.
//!
//! Futures don't trade around the clock: WDO stops on Friday evening and only comes
//! back Monday morning, CME FX comes back Sunday 18:00 CT. Plotting bars at a position
//! proportional to wall-clock time spends two thirds of the canvas on the closed
//! stretch, which is technically honest but reads as an enormous gap in the price
//! action. Every venue-facing charting platform instead squeezes those closed stretches
//! down so bars sit next to each other.
//!
//! [`TimeAxis`] is the mapping that does the squeezing. It's built from the timestamps
//! actually present in a series, so it needs no per-symbol trading-hours calendar: a
//! stretch with no buckets *is* the closed stretch, whatever the venue's schedule
//! happens to be, and each symbol gets its own breaks for free.
//!
//! The mapping is "real ms -> compressed ms": every gap longer than
//! [`MAX_VISIBLE_GAP_SLOTS`] empty slots has its excess milliseconds deducted, so
//! callers keep doing the same `(ts - anchor) / interval * cell_width` arithmetic they
//! did on raw timestamps, only in the compressed domain. Short holes are left alone —
//! a single quiet minute inside a session is real information and stays a real hole.

/// Empty slots a gap is allowed to keep after compression.
///
/// Zero would butt the two sessions directly against each other (TradingView's look);
/// one keeps a visible notch that reads as "the market was closed here" without
/// spending the whole canvas on it.
pub const MAX_VISIBLE_GAP_SLOTS: u64 = 1;

/// Shortest stretch without prints that counts as the market being *closed* rather than
/// merely quiet.
///
/// The two have to be told apart because only one of them should be squeezed: a thin
/// contract routinely goes ten or fifteen minutes without a print mid-session and that
/// flat stretch is real information, while no venue's closure is shorter than its
/// hour-long daily maintenance halt. An hour separates the two cleanly at every
/// timeframe worth charting.
pub const MIN_CLOSURE_MS: u64 = 60 * 60 * 1000;

/// Whether `empty_span_ms` of consecutive slots with no data is the market being closed.
///
/// Used in two places that have to agree: the axis squeezes closures (and only
/// closures), and `TimeSeries` refuses to carry a previous close across one — inventing
/// two days of flat candles for a weekend would both hide the closure from this mapping
/// and put a heap of overlapping dojis on the column it collapses to.
pub fn is_closure(empty_span_ms: u64, interval_ms: u64) -> bool {
    let min_slots = interval_ms.saturating_mul(MAX_VISIBLE_GAP_SLOTS + 1);

    empty_span_ms >= MIN_CLOSURE_MS.max(min_slots)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Break {
    /// First hidden timestamp, i.e. the start of the stretch being deducted.
    start: u64,
    /// Milliseconds deducted by this break.
    removed: u64,
    /// Milliseconds deducted by every earlier break.
    cum_before: u64,
}

impl Break {
    /// Where this break sits once every earlier break has been deducted.
    fn compressed_start(&self) -> u64 {
        self.start - self.cum_before
    }
}

/// Maps real timestamps onto a compressed axis where long market closures take up
/// [`MAX_VISIBLE_GAP_SLOTS`] slots instead of their real duration.
///
/// A default/[`TimeAxis::identity`] instance has no breaks and maps every timestamp to
/// itself, so code paths that don't have data yet (or run on a tick basis) behave
/// exactly as they did before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeAxis {
    interval: u64,
    breaks: Vec<Break>,
}

/// Borrowable no-op mapping, for widgets that take the axis by reference and may be
/// built before any data exists.
pub static IDENTITY: TimeAxis = TimeAxis {
    interval: 0,
    breaks: Vec::new(),
};

impl TimeAxis {
    pub fn identity() -> Self {
        Self::default()
    }

    /// Builds the mapping from a series' bucket timestamps.
    ///
    /// `keys` must be sorted ascending and aligned to `interval` — both hold for the
    /// `BTreeMap` keys of a `TimeSeries` and for the kline timestamps of a comparison
    /// series, which are floored to the timeframe on the way in.
    pub fn from_sorted_keys(keys: impl IntoIterator<Item = u64>, interval: u64) -> Self {
        Self::with_max_gap(keys, interval, MAX_VISIBLE_GAP_SLOTS)
    }

    fn with_max_gap(
        keys: impl IntoIterator<Item = u64>,
        interval: u64,
        max_visible_slots: u64,
    ) -> Self {
        if interval == 0 {
            return Self::identity();
        }

        let mut breaks: Vec<Break> = Vec::new();
        let mut cum_removed = 0u64;
        let mut prev: Option<u64> = None;

        for ts in keys {
            if let Some(prev_ts) = prev
                && ts > prev_ts
            {
                let slots = (ts - prev_ts) / interval;
                let empty_slots = slots.saturating_sub(1);

                if empty_slots > max_visible_slots && is_closure(empty_slots * interval, interval) {
                    let removed = (empty_slots - max_visible_slots) * interval;
                    let start = prev_ts + interval * (1 + max_visible_slots);

                    breaks.push(Break {
                        start,
                        removed,
                        cum_before: cum_removed,
                    });
                    cum_removed += removed;
                }
            }

            // `max` rather than plain assignment so an out-of-order key can't rewind the
            // scan and emit an overlapping break.
            prev = Some(prev.map_or(ts, |p| p.max(ts)));
        }

        Self { interval, breaks }
    }

    /// True when nothing is being compressed, so callers can skip the mapping entirely.
    pub fn is_identity(&self) -> bool {
        self.breaks.is_empty()
    }

    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// Total milliseconds this mapping deducts.
    pub fn removed_total(&self) -> u64 {
        self.breaks
            .last()
            .map_or(0, |b| b.cum_before.saturating_add(b.removed))
    }

    /// Real timestamp -> compressed axis position. Monotonic non-decreasing: every
    /// timestamp inside a closed stretch collapses onto that stretch's single position.
    pub fn compress(&self, ts: u64) -> u64 {
        ts - self.removed_before(ts)
    }

    /// Compressed axis position -> real timestamp. Inverse of [`Self::compress`] on
    /// visible timestamps; positions that land exactly on a break resolve to the first
    /// bucket after the closure, which is the bucket actually drawn there.
    pub fn expand(&self, compressed: u64) -> u64 {
        let idx = self
            .breaks
            .partition_point(|b| b.compressed_start() <= compressed);

        if idx == 0 {
            compressed
        } else {
            let b = &self.breaks[idx - 1];
            compressed
                .saturating_add(b.cum_before)
                .saturating_add(b.removed)
        }
    }

    /// Signed distance between two timestamps on the compressed axis.
    pub fn compressed_delta(&self, ts: u64, anchor: u64) -> i64 {
        let a = self.compress(ts) as i128;
        let b = self.compress(anchor) as i128;

        (a - b).clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// True when `ts` falls inside a compressed-away stretch, so it has no position of
    /// its own on screen and must not get an axis label.
    pub fn is_hidden(&self, ts: u64) -> bool {
        let idx = self.breaks.partition_point(|b| b.start <= ts);

        idx != 0 && {
            let b = &self.breaks[idx - 1];
            ts >= b.start && ts < b.start.saturating_add(b.removed)
        }
    }

    /// Resolves a horizontal position — given as a `ratio` across the visible range
    /// `earliest..=latest` — onto the bucket grid, returning the bucket's real timestamp
    /// and where it sits as a ratio of the same range.
    ///
    /// The interpolation happens on the compressed axis, since that's the one the cursor
    /// travels along, while the rounding happens in real time, where the `interval` grid
    /// is defined. Every crosshair and ruler on a time basis goes through this.
    pub fn snap_to_bucket(
        &self,
        earliest: u64,
        latest: u64,
        ratio: f64,
        interval: u64,
    ) -> (u64, f32) {
        let compressed_earliest = self.compress(earliest) as f64;
        let compressed_latest = self.compress(latest) as f64;

        let at = (compressed_earliest + ratio * (compressed_latest - compressed_earliest)).max(0.0);

        // Rounding has to happen here, on the compressed axis, not after expanding:
        // compressed positions sit on the same `interval` grid (every deduction is a
        // whole number of slots), whereas a real timestamp rounded to the grid can land
        // inside a closure, on a column that isn't drawn anywhere.
        let rounded = if interval == 0 {
            at as u64
        } else {
            (at / interval as f64).round() as u64 * interval
        };
        let rounded = self.expand(rounded);

        let span = compressed_latest - compressed_earliest;
        let snap_ratio = if span > 0.0 {
            ((self.compress(rounded) as f64 - compressed_earliest) / span) as f32
        } else {
            0.5
        };

        (rounded, snap_ratio)
    }

    fn removed_before(&self, ts: u64) -> u64 {
        let idx = self.breaks.partition_point(|b| b.start <= ts);

        if idx == 0 {
            0
        } else {
            let b = &self.breaks[idx - 1];
            b.cum_before + (ts - b.start).min(b.removed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M15: u64 = 15 * 60 * 1000;

    /// Three consecutive buckets, then a two-day closure, then three more.
    fn weekend_axis() -> (TimeAxis, Vec<u64>) {
        let close = 3 * M15;
        let reopen = close + 2 * 24 * 60 * 60 * 1000;
        let keys = vec![0, M15, 2 * M15, reopen, reopen + M15, reopen + 2 * M15];

        (TimeAxis::from_sorted_keys(keys.clone(), M15), keys)
    }

    #[test]
    fn identity_axis_maps_every_timestamp_to_itself() {
        let axis = TimeAxis::identity();

        assert!(axis.is_identity());
        assert_eq!(axis.compress(1_700_000_000_000), 1_700_000_000_000);
        assert_eq!(axis.expand(1_700_000_000_000), 1_700_000_000_000);
        assert!(!axis.is_hidden(1_700_000_000_000));
    }

    #[test]
    fn contiguous_series_has_no_breaks() {
        let keys: Vec<u64> = (0..10).map(|i| i * M15).collect();

        assert!(TimeAxis::from_sorted_keys(keys, M15).is_identity());
    }

    #[test]
    fn a_quiet_stretch_shorter_than_a_closure_is_left_alone() {
        // Two hours of 1m buckets, half an hour of which never printed: thin, not shut.
        let m1 = 60 * 1000;
        let keys = vec![0, m1, 2 * m1, 32 * m1, 33 * m1];

        assert!(TimeAxis::from_sorted_keys(keys, m1).is_identity());
    }

    #[test]
    fn an_hour_without_prints_counts_as_closed() {
        let m1 = 60 * 1000;
        let keys = vec![0, m1, 2 * m1, 63 * m1, 64 * m1];
        let axis = TimeAxis::from_sorted_keys(keys, m1);

        assert!(!axis.is_identity());
        assert_eq!(axis.compress(63 * m1) - axis.compress(2 * m1), 2 * m1);
    }

    #[test]
    fn single_empty_slot_is_left_alone() {
        // One quiet bucket inside a session is real information, not a closure.
        let keys = vec![0, M15, 3 * M15, 4 * M15];

        assert!(TimeAxis::from_sorted_keys(keys, M15).is_identity());
    }

    #[test]
    fn closure_collapses_to_one_empty_slot() {
        let (axis, keys) = weekend_axis();
        let reopen = keys[3];

        // The session either side of the break keeps its natural spacing...
        assert_eq!(axis.compress(M15) - axis.compress(0), M15);
        // ...and the closure itself is worth exactly two slots of travel: the last bar
        // before it, one blank, then the reopen bar.
        assert_eq!(axis.compress(reopen) - axis.compress(2 * M15), 2 * M15);
    }

    #[test]
    fn expand_inverts_compress_on_visible_buckets() {
        let (axis, keys) = weekend_axis();

        for key in keys {
            assert_eq!(axis.expand(axis.compress(key)), key);
        }
    }

    #[test]
    fn timestamps_inside_the_closure_are_hidden() {
        let (axis, keys) = weekend_axis();
        let reopen = keys[3];

        assert!(!axis.is_hidden(2 * M15), "last bucket before the closure");
        assert!(!axis.is_hidden(3 * M15), "the blank slot that is kept");
        assert!(axis.is_hidden(4 * M15), "first deducted bucket");
        assert!(axis.is_hidden(reopen - M15));
        assert!(!axis.is_hidden(reopen), "first bucket after the closure");
    }

    #[test]
    fn compression_is_monotonic_across_a_closure() {
        let (axis, keys) = weekend_axis();
        let reopen = keys[3];

        let mut prev = axis.compress(0);
        let mut ts = 0;
        while ts <= reopen + 2 * M15 {
            let compressed = axis.compress(ts);
            assert!(compressed >= prev, "compress went backwards at {ts}");
            prev = compressed;
            ts += M15;
        }
    }

    #[test]
    fn repeated_closures_accumulate() {
        let day = 24 * 60 * 60 * 1000;
        let keys = vec![0, day, 2 * day, 5 * day, 6 * day, 12 * day];
        let axis = TimeAxis::from_sorted_keys(keys.clone(), day);

        // Daily bars: each weekend-sized hole is worth two days of travel regardless of
        // how long it really was.
        assert_eq!(axis.compress(5 * day) - axis.compress(2 * day), 2 * day);
        assert_eq!(axis.compress(12 * day) - axis.compress(6 * day), 2 * day);

        for key in keys {
            assert_eq!(axis.expand(axis.compress(key)), key);
        }
    }

    #[test]
    fn snapping_lands_on_buckets_either_side_of_a_closure() {
        let (axis, keys) = weekend_axis();
        let (first, last) = (keys[0], keys[5]);

        // Five columns are on screen (3 bars, 1 blank, then the reopen bar and the two
        // after it): sweeping the ratio must only ever produce bars that exist.
        for step in 0..=20 {
            let ratio = f64::from(step) / 20.0;
            let (bucket, snap) = axis.snap_to_bucket(first, last, ratio, M15);

            assert!(
                !axis.is_hidden(bucket),
                "snapped into the closure at {ratio}"
            );
            assert!(
                (-0.01..=1.01).contains(&snap),
                "snap ratio {snap} outside the visible range at {ratio}"
            );
        }

        // Both edges resolve to the bar sitting there.
        assert_eq!(axis.snap_to_bucket(first, last, 0.0, M15).0, first);
        assert_eq!(axis.snap_to_bucket(first, last, 1.0, M15).0, last);
    }

    #[test]
    fn timestamps_outside_the_data_are_not_compressed() {
        let (axis, keys) = weekend_axis();
        let last = keys[5];

        // Panning past either edge extrapolates on the raw timeframe grid, so the fetch
        // bounds derived from it keep moving outward.
        assert_eq!(axis.compressed_delta(0, M15), -(M15 as i64));
        assert_eq!(axis.expand(axis.compress(last) + M15), last + M15);
    }
}
