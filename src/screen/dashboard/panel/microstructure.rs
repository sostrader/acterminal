use super::Message;
use crate::connector::fetcher::{FetchRange, FetchSpec, ReqError, RequestHandler};
use crate::style;
pub use data::panel::microstructure::Config;
use data::panel::microstructure::{MicroStats, SessionStats, TradeWindow};
use exchange::adapter::acfeed::codec::QuotePayload;
use exchange::{TickerInfo, Trade, UnixMs};

use iced::widget::canvas::{self, Text};
use iced::{Alignment, Point, Rectangle, Renderer, Size, Theme, mouse};
use std::time::Instant;

const LABEL_SIZE: iced::Pixels = iced::Pixels(style::text_size::SMALL);
const VALUE_SIZE: iced::Pixels = iced::Pixels(style::text_size::TITLE);
const CARD_GAP: f32 = 4.0;
const CARD_PADDING: f32 = 6.0;
/// Below this width the 4-across grid gets too cramped to read, so it folds to 2.
const NARROW_WIDTH: f32 = 320.0;

/// The eight readouts, in ACTerminal's own order: the four 60s order-flow cards first,
/// then the session strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    TradesPerSec,
    AvgSize,
    BuyPct,
    SellPct,
    Open,
    High,
    Low,
    Range,
}

impl Card {
    const ALL: [Card; 8] = [
        Card::TradesPerSec,
        Card::AvgSize,
        Card::BuyPct,
        Card::SellPct,
        Card::Open,
        Card::High,
        Card::Low,
        Card::Range,
    ];

    fn label(self) -> &'static str {
        match self {
            Card::TradesPerSec => "Trades/s",
            Card::AvgSize => "Avg size",
            Card::BuyPct => "Buy %",
            Card::SellPct => "Sell %",
            Card::Open => "Open",
            Card::High => "High",
            Card::Low => "Low",
            Card::Range => "Range",
        }
    }
}

impl super::Panel for Microstructure {
    fn scroll(&mut self, _delta: f32) {}

    fn reset_scroll(&mut self) {}

    fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        self.invalidate(now)
    }

    fn is_empty(&self) -> bool {
        self.window.is_empty() && self.session.is_empty()
    }
}

pub struct Microstructure {
    window: TradeWindow,
    session: SessionStats,
    ticker_info: TickerInfo,
    pub config: Config,
    cache: canvas::Cache,
    last_tick: Instant,
    /// Newest trade timestamp seen. The rolling window is measured against this rather
    /// than the wall clock, so the cards keep working when the feed runs behind real
    /// time — a delayed feed would otherwise have every arriving trade fall outside a
    /// `now - 60s` cutoff the moment it landed, leaving the cards permanently at zero.
    feed_now: Option<UnixMs>,
    request_handler: RequestHandler,
    /// The rolling 60s cards come from trades; the session strip comes from the venue's
    /// own quote. Two independent sources, so two independent backfills.
    trades_backfill: Backfill,
    quote_backfill: Backfill,
}

/// The one-shot historical backfill's lifecycle. `invalidate` runs several times a
/// second, and the requested range is anchored to "now", so `RequestHandler`'s
/// range-equality dedup can never catch a repeat — this tracks it explicitly instead
/// of letting the pane re-request every tick.
enum Backfill {
    Idle,
    InFlight(Instant),
    Done,
}

/// How long to wait on a backfill before assuming it was dropped and asking again.
const BACKFILL_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

impl Microstructure {
    pub fn new(config: Option<Config>, ticker_info: TickerInfo) -> Self {
        Self {
            window: TradeWindow::default(),
            session: SessionStats::default(),
            ticker_info,
            config: config.unwrap_or_default(),
            cache: canvas::Cache::default(),
            last_tick: Instant::now(),
            feed_now: None,
            request_handler: RequestHandler::default(),
            trades_backfill: Backfill::Idle,
            quote_backfill: Backfill::Idle,
        }
    }

    pub fn insert_buffer(&mut self, trades_buffer: &[Trade]) {
        if let Some(newest) = trades_buffer.iter().map(|t| t.time).max() {
            self.feed_now = Some(self.feed_now.map_or(newest, |seen| seen.max(newest)));
        }

        self.window.insert(trades_buffer);
        self.prune_window();
        self.cache.clear();
    }

    /// Trims the rolling window against the feed's own clock (see [`Self::feed_now`]).
    fn prune_window(&mut self) {
        if let Some(feed_now) = self.feed_now {
            self.window.prune(feed_now, self.config.window);
        }
    }

    /// Feeds the historical backfill in. Trades are ordered oldest -> newest and reach
    /// further back than the rolling window, so `prune` immediately drops the excess —
    /// the point is that the cards read correctly the moment the pane opens instead of
    /// after a full window of live trades has gone by.
    ///
    /// Only the rolling cards are fed from here. The session strip is deliberately left
    /// alone: the first trade in a few minutes of backfill is not the session's open.
    pub fn insert_fetched_trades(&mut self, trades: &[Trade], is_batches_done: bool) {
        self.insert_buffer(trades);

        if is_batches_done {
            self.trades_backfill = Backfill::Done;
        }
    }

    /// Seeds the session strip from the newest quote `persist` has stored, so
    /// open/high/low/range read correctly on open instead of staying blank until the
    /// venue happens to publish its next quote.
    pub fn insert_fetched_quote(&mut self, quote: &QuotePayload) {
        self.insert_quote(quote);
        self.quote_backfill = Backfill::Done;
    }

    /// The venue republishes the full session snapshot on every quote, so a field that
    /// arrives empty means "not published yet", not "reset to unknown" — keep the last
    /// known value rather than blanking a card that was already showing a number.
    pub fn insert_quote(&mut self, quote: &QuotePayload) {
        self.session.open = quote.open.or(self.session.open);
        self.session.high = quote.high.or(self.session.high);
        self.session.low = quote.low.or(self.session.low);
        self.session.last = quote.last.or(self.session.last);
        self.cache.clear();
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        self.prune_window();

        self.cache.clear();
        if let Some(now) = now {
            self.last_tick = now;
        }

        self.fetch_backfill()
    }

    /// Asks for enough recent trades to fill the rolling window (plus margin, since a
    /// quiet product may have none in the last 60s) and to derive the session strip.
    fn fetch_backfill(&mut self) -> Option<super::Action> {
        let mut specs = Vec::new();

        // Enough trades to fill the rolling window with margin, since a quiet product
        // may have printed nothing at all in the last 60s.
        let now = UnixMs::now();
        let window_ms = self.config.window.as_millis().min(u64::MAX as u128) as u64;
        let from = now.as_u64().saturating_sub(window_ms.saturating_mul(4));

        if let Some(spec) = self.request_backfill(
            Self::trades_backfill_slot,
            FetchRange::Trades(UnixMs::new(from), now),
        ) {
            specs.push(spec);
        }

        if let Some(spec) =
            self.request_backfill(Self::quote_backfill_slot, FetchRange::LatestQuote)
        {
            specs.push(spec);
        }

        (!specs.is_empty()).then(|| super::Action::RequestFetch(specs))
    }

    fn trades_backfill_slot(&mut self) -> &mut Backfill {
        &mut self.trades_backfill
    }

    fn quote_backfill_slot(&mut self) -> &mut Backfill {
        &mut self.quote_backfill
    }

    /// Issues one backfill request, guarding against the re-request storm that would
    /// otherwise follow from `invalidate` running several times a second against a
    /// range anchored to "now" (which `RequestHandler`'s range-equality dedup can
    /// never match).
    fn request_backfill(
        &mut self,
        slot: fn(&mut Self) -> &mut Backfill,
        range: FetchRange,
    ) -> Option<FetchSpec> {
        match *slot(self) {
            Backfill::Done => return None,
            Backfill::InFlight(since) if since.elapsed() < BACKFILL_RETRY_AFTER => return None,
            _ => {}
        }

        match self.request_handler.add_request(range) {
            Ok(Some(req_id)) => {
                *slot(self) = Backfill::InFlight(Instant::now());
                Some(FetchSpec {
                    req_id,
                    fetch: range,
                    stream: None,
                })
            }
            Ok(None) => None,
            Err(ReqError::Overlaps) => None,
            Err(reason) => {
                log::error!("Microstructure backfill request failed: {reason}");
                None
            }
        }
    }

    fn value_for(&self, card: Card, stats: &MicroStats) -> String {
        let price = |value: Option<exchange::unit::Price>| {
            value.map_or_else(
                || "--".to_string(),
                |p| p.to_string(self.ticker_info.display_precision()),
            )
        };

        match card {
            Card::TradesPerSec => format!("{:.1}", stats.trades_per_sec),
            // Contract counts, so no decimals — same reasoning as `abbr_large_numbers`.
            Card::AvgSize => format!("{:.0}", stats.avg_size),
            Card::BuyPct => format!("{:.0}%", stats.buy_pct),
            Card::SellPct => format!("{:.0}%", stats.sell_pct),
            Card::Open => price(self.session.open),
            Card::High => price(self.session.high),
            Card::Low => price(self.session.low),
            Card::Range => price(self.session.range()),
        }
    }
}

impl canvas::Program<Message> for Microstructure {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let palette = theme.extended_palette();
        let stats = self.window.stats(self.config.window);

        let content = self.cache.draw(renderer, bounds.size(), |frame| {
            let columns = if bounds.width < NARROW_WIDTH { 2 } else { 4 };
            let rows = Card::ALL.len().div_ceil(columns);

            let cell_width = (bounds.width - CARD_GAP * (columns as f32 + 1.0)) / columns as f32;
            let cell_height = (bounds.height - CARD_GAP * (rows as f32 + 1.0)) / rows as f32;

            if cell_width <= 0.0 || cell_height <= 0.0 {
                return;
            }

            for (i, card) in Card::ALL.iter().enumerate() {
                let col = i % columns;
                let row = i / columns;

                let x = CARD_GAP + col as f32 * (cell_width + CARD_GAP);
                let y = CARD_GAP + row as f32 * (cell_height + CARD_GAP);

                frame.fill_rectangle(
                    Point { x, y },
                    Size {
                        width: cell_width,
                        height: cell_height,
                    },
                    palette.background.weak.color,
                );

                let up = palette.success.base.color;
                let down = palette.danger.base.color;

                let value_color = match card {
                    // High/Low share the Buy%/Sell% colours so the two halves of the
                    // grid read as one scale.
                    Card::BuyPct | Card::High => up,
                    Card::SellPct | Card::Low => down,
                    Card::Range => match self.session.range() {
                        Some(range) if range.units > 0 => up,
                        Some(range) if range.units < 0 => down,
                        _ => palette.background.base.text,
                    },
                    _ => palette.background.base.text,
                };

                frame.fill_text(Text {
                    content: card.label().to_string(),
                    position: Point {
                        x: x + CARD_PADDING,
                        y: y + CARD_PADDING,
                    },
                    size: LABEL_SIZE,
                    font: style::AZERET_MONO,
                    color: palette.background.weak.text,
                    align_x: Alignment::Start.into(),
                    align_y: Alignment::Start.into(),
                    ..Default::default()
                });

                frame.fill_text(Text {
                    content: self.value_for(*card, &stats),
                    position: Point {
                        x: x + CARD_PADDING,
                        y: y + cell_height - CARD_PADDING,
                    },
                    size: VALUE_SIZE,
                    font: style::AZERET_MONO,
                    color: value_color,
                    align_x: Alignment::Start.into(),
                    align_y: Alignment::End.into(),
                    ..Default::default()
                });
            }
        });

        vec![content]
    }
}
