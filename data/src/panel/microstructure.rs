//! Rolling order-flow microstructure stats plus the venue's session OHL.
//!
//! Mirrors ACTerminal's two readouts, which live in different places there but describe
//! the same instrument at the same moment: the four 60-second cards under the DOM
//! (`renderMicrostructure` in `acterminal/src/main.ts`) and the OPEN/HIGH/LOW/RANGE strip
//! above it (`updateQuotesBar`). They are one component here.

use std::collections::VecDeque;
use std::time::Duration;

use exchange::unit::{Price, Qty};
use exchange::{Trade, UnixMs};
use serde::{Deserialize, Serialize};

/// ACTerminal fixes its microstructure cards at "60s"; keep the same window so the two
/// terminals read the same numbers side by side.
const DEFAULT_WINDOW_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_window")]
    pub window: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            window: default_window(),
        }
    }
}

fn default_window() -> Duration {
    Duration::from_millis(DEFAULT_WINDOW_MS)
}

/// The four order-flow cards, computed over the trailing window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MicroStats {
    pub trades_per_sec: f64,
    pub avg_size: f64,
    pub buy_pct: f64,
    pub sell_pct: f64,
    pub trade_count: usize,
}

/// The session strip, taken from the venue's `quotes/` topic rather than recomputed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SessionStats {
    pub open: Option<Price>,
    pub high: Option<Price>,
    pub low: Option<Price>,
    pub last: Option<Price>,
}

impl SessionStats {
    /// Signed high - low.
    ///
    /// A healthy session always yields a positive number; the sign is kept rather than
    /// discarded so a feed that reports a high below its low shows up as a negative
    /// range (rendered red) instead of silently blanking the card.
    pub fn range(&self) -> Option<Price> {
        match (self.high, self.low) {
            (Some(high), Some(low)) => Some(high - low),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_none() && self.high.is_none() && self.low.is_none() && self.last.is_none()
    }
}

#[derive(Debug, Clone, Copy)]
struct WindowedTrade {
    time: UnixMs,
    qty: Qty,
    is_sell: bool,
}

/// Trailing-window trade accumulator.
///
/// Sums are carried incrementally and adjusted on eviction rather than recomputed each
/// tick — the pane redraws on every print, and a busy FX session can hold thousands of
/// trades in a 60s window.
#[derive(Debug, Default)]
pub struct TradeWindow {
    trades: VecDeque<WindowedTrade>,
    buy_qty: Qty,
    sell_qty: Qty,
}

impl TradeWindow {
    pub fn insert(&mut self, trades: &[Trade]) {
        for trade in trades {
            if trade.is_sell {
                self.sell_qty += trade.qty;
            } else {
                self.buy_qty += trade.qty;
            }

            self.trades.push_back(WindowedTrade {
                time: trade.time,
                qty: trade.qty,
                is_sell: trade.is_sell,
            });
        }
    }

    /// Drops everything older than `window` relative to `now`.
    pub fn prune(&mut self, now: UnixMs, window: Duration) {
        let cutoff = now
            .as_u64()
            .saturating_sub(window.as_millis().min(u64::MAX as u128) as u64);

        while let Some(front) = self.trades.front() {
            if front.time.as_u64() >= cutoff {
                break;
            }

            let evicted = self.trades.pop_front().expect("front just peeked");
            if evicted.is_sell {
                self.sell_qty = saturating_sub(self.sell_qty, evicted.qty);
            } else {
                self.buy_qty = saturating_sub(self.buy_qty, evicted.qty);
            }
        }
    }

    pub fn clear(&mut self) {
        self.trades.clear();
        self.buy_qty = Qty::ZERO;
        self.sell_qty = Qty::ZERO;
    }

    pub fn is_empty(&self) -> bool {
        self.trades.is_empty()
    }

    /// Computes the four cards over whatever is currently in the window.
    ///
    /// Buy/sell shares are volume-weighted (not trade counts), matching ACTerminal —
    /// one 50-lot sweep should not read the same as fifty 1-lots going the other way.
    pub fn stats(&self, window: Duration) -> MicroStats {
        let count = self.trades.len();
        if count == 0 {
            return MicroStats::default();
        }

        let window_secs = (window.as_secs_f64()).max(f64::EPSILON);
        let buy = self.buy_qty.to_f64();
        let sell = self.sell_qty.to_f64();
        let total = buy + sell;

        let (buy_pct, sell_pct) = if total > 0.0 {
            (buy / total * 100.0, sell / total * 100.0)
        } else {
            (0.0, 0.0)
        };

        MicroStats {
            trades_per_sec: count as f64 / window_secs,
            avg_size: total / count as f64,
            buy_pct,
            sell_pct,
            trade_count: count,
        }
    }
}

fn saturating_sub(lhs: Qty, rhs: Qty) -> Qty {
    if lhs.units >= rhs.units {
        lhs - rhs
    } else {
        Qty::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(time_ms: u64, qty: f64, is_sell: bool) -> Trade {
        Trade {
            time: UnixMs::new(time_ms),
            is_sell,
            price: Price::from_f64(1.08),
            qty: Qty::from_f64(qty),
        }
    }

    #[test]
    fn stats_are_volume_weighted_and_windowed() {
        let mut window = TradeWindow::default();
        window.insert(&[
            trade(1_000, 30.0, false),
            trade(2_000, 10.0, true),
            trade(3_000, 10.0, true),
        ]);

        let stats = window.stats(Duration::from_secs(60));
        assert_eq!(stats.trade_count, 3);
        assert!((stats.trades_per_sec - 3.0 / 60.0).abs() < 1e-9);
        // 50 contracts over 3 prints.
        assert!((stats.avg_size - 50.0 / 3.0).abs() < 1e-9);
        // Volume-weighted: 30 buy vs 20 sell, not 1 buy vs 2 sells.
        assert!((stats.buy_pct - 60.0).abs() < 1e-9);
        assert!((stats.sell_pct - 40.0).abs() < 1e-9);
    }

    #[test]
    fn pruning_evicts_old_trades_and_their_volume() {
        let mut window = TradeWindow::default();
        window.insert(&[trade(1_000, 100.0, false), trade(65_000, 10.0, true)]);

        window.prune(UnixMs::new(70_000), Duration::from_secs(60));

        let stats = window.stats(Duration::from_secs(60));
        assert_eq!(stats.trade_count, 1);
        // The evicted buy volume must not linger in the share split.
        assert!((stats.sell_pct - 100.0).abs() < 1e-9);
        assert!((stats.buy_pct - 0.0).abs() < 1e-9);
        assert!((stats.avg_size - 10.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_window_reports_zeroes_not_nan() {
        let window = TradeWindow::default();
        let stats = window.stats(Duration::from_secs(60));
        assert_eq!(stats, MicroStats::default());
        assert!(stats.avg_size.is_finite());
        assert!(stats.buy_pct.is_finite());
    }

    #[test]
    fn session_range_needs_both_ends() {
        let mut session = SessionStats::default();
        assert!(session.is_empty());
        assert_eq!(session.range(), None);

        session.high = Some(Price::from_f64(1.09));
        assert_eq!(session.range(), None);

        session.low = Some(Price::from_f64(1.08));
        assert_eq!(session.range(), Some(Price::from_f64(0.01)));
    }

    #[test]
    fn an_inverted_high_low_yields_a_negative_range() {
        let session = SessionStats {
            high: Some(Price::from_f64(1.08)),
            low: Some(Price::from_f64(1.09)),
            ..Default::default()
        };

        let range = session.range().expect("both ends present");
        assert!(range.units < 0);
    }
}
