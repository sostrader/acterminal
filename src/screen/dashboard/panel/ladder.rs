use super::Message;
use crate::style;
use data::panel::ladder::{ChaseTracker, Config, GroupedDepth, Side, TradeStore};
use exchange::Trade;
use exchange::unit::qty::Qty;
use exchange::unit::{Price, PriceStep};
use exchange::{TickerInfo, UnixMs, depth::Depth};

use iced::widget::canvas::{self, Path, Stroke, Text};
use iced::{Alignment, Event, Point, Rectangle, Renderer, Size, Theme, mouse};

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const ROW_HEIGHT: f32 = 16.0;
/// Minimum row height so very deep books stay readable (and become scrollable).
const MIN_ROW_HEIGHT: f32 = 14.0;
/// Maximum row height, so a thin book never stretches into a handful of giant rows —
/// the leftover space is filled with (empty) price-scale rows instead.
const MAX_ROW_HEIGHT: f32 = 26.0;
/// Text size is derived from the (dynamic) row height so rows never overflow.
const MIN_TEXT_SIZE: f32 = 9.0;
const MAX_TEXT_SIZE: f32 = 22.0;
/// Fixed column-header strip at the top, matching ACTerminal's L2 Depth head row
/// (`.bm-dom-head` / `.dom-l2-head`). The ladder body scrolls underneath it.
const HEADER_HEIGHT: f32 = 15.0;

// Total width ratios must sum to 1.0
/// Uses half of the width for each side of the order quantity columns
const ORDER_QTY_COLS_WIDTH: f32 = 0.60;
/// Uses half of the width for each side of the trade quantity columns
const TRADE_QTY_COLS_WIDTH: f32 = 0.20;

const COL_PADDING: f32 = 4.0;
/// Gap between a cell's text and the edge it is anchored to.
const CELL_TEXT_PAD: f32 = 6.0;
/// Used for calculating layout with texts inside the price column
const MONO_CHAR_ADVANCE: f32 = 0.62;
/// Minimum padding on each side of the price text inside the price column
const PRICE_TEXT_SIDE_PAD_MIN: f32 = 12.0;

/// Border of the hollow marker on the last traded price.
const LAST_PRICE_BORDER_WIDTH: f32 = 1.5;

/// Opacity of the price on rows the book is not quoting, so the scale reads as a scale
/// and the quoted levels still stand out.
const EMPTY_PRICE_ALPHA: f32 = 0.55;

const CHASE_CIRCLE_RADIUS: f32 = 4.0;
/// Maximum interval between chase updates to consider them part of the same chase
const CHASE_MIN_INTERVAL: Duration = Duration::from_millis(200);

impl super::Panel for Ladder {
    fn scroll(&mut self, delta: f32) {
        self.scroll_px += delta;
        Ladder::invalidate(self, Some(Instant::now()));
    }

    fn reset_scroll(&mut self) {
        self.scroll_px = 0.0;
        Ladder::invalidate(self, Some(Instant::now()));
    }

    fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        Ladder::invalidate(self, now)
    }

    fn is_empty(&self) -> bool {
        if self.pending_tick_size.is_some() {
            return true;
        }
        self.grouped_asks().is_empty() && self.grouped_bids().is_empty() && self.trades.is_empty()
    }
}

pub struct Ladder {
    ticker_info: TickerInfo,
    pub config: Config,
    cache: canvas::Cache,
    last_tick: Instant,
    pub step: PriceStep,
    scroll_px: f32,
    last_exchange_ts_ms: Option<UnixMs>,
    orderbook: [GroupedDepth; 2],
    trades: TradeStore,
    pending_tick_size: Option<PriceStep>,
}

impl Ladder {
    pub fn new(config: Option<Config>, ticker_info: TickerInfo, step: PriceStep) -> Self {
        Self {
            trades: TradeStore::new(),
            config: config.unwrap_or_default(),
            ticker_info,
            cache: canvas::Cache::default(),
            last_tick: Instant::now(),
            step,
            scroll_px: 0.0,
            last_exchange_ts_ms: None,
            orderbook: [GroupedDepth::new(), GroupedDepth::new()],
            pending_tick_size: None,
        }
    }

    pub fn insert_trades(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        self.trades.insert_trades(buffer, self.step);
        // Trade-path cleanup: `maybe_cleanup` otherwise only runs on
        // `insert_depth`, so a tape without DOM updates would grow `raw`
        // unboundedly. The call itself is a cheap O(1) front check unless the
        // retention window actually elapsed (then one regroup).
        if let Some(latest) = buffer.last() {
            self.trades
                .maybe_cleanup(latest.time, self.config.trade_retention, self.step);
        }
        // Arrival-driven redraw: the yellow last-trade marker lives on this data,
        // so it must not wait for the 100ms pane tick. Any non-empty buffer changes
        // either the marker or the per-price Buy/Sell columns.
        // `update()` already schedules a `view()` on the next frame, and the tick
        // stays only as fallback (chase fade/cleanup/scroll).
        self.invalidate(Some(Instant::now()));
    }

    pub fn insert_depth(&mut self, depth: &Depth, update_t: UnixMs) {
        let step_changed = self.pending_tick_size.take().is_some_and(|next| {
            let changed = next != self.step;
            self.step = next;
            self.trades.rebuild_grouped(self.step);
            changed
        });

        let raw_best_bid = depth.bids.last_key_value().map(|(p, _)| *p);
        let raw_best_ask = depth.asks.first_key_value().map(|(p, _)| *p);

        // Snapshot before mutating: the DOM topic republishes on a fixed cadence
        // even when nothing moved, so an unconditional redraw would burn a full
        // canvas rebuild per heartbeat. Only redraw when something visible changed.
        let prev_chase_bid = self.chase_tracker(Side::Bid).segment();
        let prev_chase_ask = self.chase_tracker(Side::Ask).segment();
        let prev_bids = self.grouped_bids().clone();
        let prev_asks = self.grouped_asks().clone();

        if self.config.show_chase_tracker {
            let max_int = CHASE_MIN_INTERVAL;
            self.chase_tracker_mut(Side::Bid)
                .update(raw_best_bid, true, update_t, max_int);
            self.chase_tracker_mut(Side::Ask)
                .update(raw_best_ask, false, update_t, max_int);
        } else {
            self.chase_tracker_mut(Side::Bid).reset();
            self.chase_tracker_mut(Side::Ask).reset();
        }

        let mut cleaned = false;
        if self
            .trades
            .maybe_cleanup(update_t, self.config.trade_retention, self.step)
        {
            cleaned = true;
        }

        self.regroup_from_depth(depth);
        self.last_exchange_ts_ms = Some(update_t);

        // Arrival-driven redraw (same rationale as `insert_trades`): when the book
        // or the chase trail actually moved, show it on the next frame instead of
        // waiting for the 100ms pane tick. The tick stays only as fallback.
        let book_changed = step_changed
            || cleaned
            || *self.grouped_bids() != prev_bids
            || *self.grouped_asks() != prev_asks;
        let chase_changed =
            !chase_segment_eq(&prev_chase_bid, &self.chase_tracker(Side::Bid).segment())
                || !chase_segment_eq(&prev_chase_ask, &self.chase_tracker(Side::Ask).segment());
        if book_changed || chase_changed {
            self.invalidate(Some(Instant::now()));
        }
    }

    fn trade_qty_at(&self, price: Price) -> (Qty, Qty) {
        self.trades.trade_qty_at(price)
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    fn grouped_asks(&self) -> &BTreeMap<Price, Qty> {
        &self.orderbook[Side::Ask.idx()].orders
    }

    fn grouped_bids(&self) -> &BTreeMap<Price, Qty> {
        &self.orderbook[Side::Bid.idx()].orders
    }

    fn chase_tracker(&self, side: Side) -> &ChaseTracker {
        &self.orderbook[side.idx()].chase
    }

    fn chase_tracker_mut(&mut self, side: Side) -> &mut ChaseTracker {
        &mut self.orderbook[side.idx()].chase
    }

    pub fn min_tick_size(&self) -> f32 {
        self.ticker_info.min_ticksize.to_f32_lossy()
    }

    pub fn set_tick_size(&mut self, step: PriceStep) {
        self.pending_tick_size = Some(step);
        self.invalidate(Some(Instant::now()));
    }

    pub fn set_show_chase_tracker(&mut self, enabled: bool) {
        if self.config.show_chase_tracker != enabled {
            self.config.show_chase_tracker = enabled;
            if !enabled {
                self.chase_tracker_mut(Side::Bid).reset();
                self.chase_tracker_mut(Side::Ask).reset();
            }

            self.invalidate(Some(Instant::now()));
        }
    }

    fn regroup_from_depth(&mut self, depth: &Depth) {
        let step = self.step;

        self.orderbook[Side::Ask.idx()].regroup_from_raw(&depth.asks, Side::Ask, step);
        self.orderbook[Side::Bid.idx()].regroup_from_raw(&depth.bids, Side::Bid, step);
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        self.cache.clear();
        if let Some(now) = now {
            self.last_tick = now;
        }
        None
    }

    fn format_price(&self, price: Price) -> String {
        price.to_string(self.ticker_info.display_precision())
    }

    fn format_quantity(&self, qty: Qty) -> String {
        data::util::abbr_large_numbers(qty.to_f64())
    }
}

impl canvas::Program<Message> for Ladder {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: &iced::Event,
        bounds: iced::Rectangle,
        cursor: iced_core::mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let _cursor_position = cursor.position_in(bounds)?;

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Middle | mouse::Button::Left | mouse::Button::Right,
            )) => Some(canvas::Action::publish(Message::ResetScroll).and_capture()),
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let scroll_amount = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -(*y) * ROW_HEIGHT,
                    mouse::ScrollDelta::Pixels { y, .. } => -*y,
                };

                Some(canvas::Action::publish(Message::Scrolled(scroll_amount)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: iced_core::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry<Renderer>> {
        let palette = theme.extended_palette();

        let text_color = palette.background.base.text;
        let bid_color = palette.success.base.color;
        let ask_color = palette.danger.base.color;

        let divider_color = style::split_ruler(theme).color;

        let orderbook_visual = self.cache.draw(renderer, bounds.size(), |frame| {
            let full_bounds = bounds;
            // The header owns the top strip; everything below is laid out (and centred)
            // as if the canvas started there, then shifted down to sit under it.
            let bounds = Rectangle {
                height: (bounds.height - HEADER_HEIGHT).max(0.0),
                ..bounds
            };

            let grid = self.build_price_grid();
            let row_height = self.ladder_row_height(bounds.height, grid.as_ref());
            let layout = self.price_layout_for(bounds.width, grid.as_ref(), row_height);
            let cols = self.column_ranges(bounds.width, layout.price_px);

            // The body is clipped to the strip under the header. Canvas text is composited
            // after the shapes whatever the draw order, so a scrolled row overlapping the
            // header would print straight over its labels — clipping is what actually keeps
            // the rows underneath.
            let body_region = Rectangle::new(
                Point::new(0.0, HEADER_HEIGHT),
                Size::new(bounds.width, bounds.height),
            );

            frame.with_clip(body_region, |frame| {
                frame.translate(iced::Vector::new(0.0, HEADER_HEIGHT));

                let (visible_rows, maxima) = match grid.as_ref() {
                    Some(grid) => self.visible_rows(bounds, grid, row_height),
                    None => (Vec::new(), Maxima::default()),
                };

                let mut best_bid_y: Option<f32> = None;
                let mut best_ask_y: Option<f32> = None;

                let best_ask = self.grouped_asks().first_key_value().map(|(p, _)| *p);
                let best_bid = self.grouped_bids().last_key_value().map(|(p, _)| *p);
                let last_trade_price = self.trades.last_trade_price(self.step);

                for visible_row in visible_rows.iter() {
                    match visible_row.row {
                        DomRow::Ask { price, qty } => {
                            let is_best = Some(price) == best_ask;
                            if is_best {
                                best_ask_y = Some(visible_row.y);
                                Self::fill_best_price_cell(
                                    frame,
                                    &cols,
                                    visible_row.y,
                                    row_height,
                                    ask_color,
                                );
                            }

                            self.draw_row(
                                frame,
                                visible_row.y,
                                row_height,
                                price,
                                qty,
                                false,
                                ask_color,
                                text_color,
                                maxima.vis_max_order_qty,
                                visible_row.buy_t,
                                visible_row.sell_t,
                                maxima.vis_max_trade_qty,
                                bid_color,
                                ask_color,
                                &cols,
                            );
                        }
                        DomRow::Bid { price, qty } => {
                            let is_best = Some(price) == best_bid;
                            if is_best {
                                best_bid_y = Some(visible_row.y);
                                Self::fill_best_price_cell(
                                    frame,
                                    &cols,
                                    visible_row.y,
                                    row_height,
                                    bid_color,
                                );
                            }

                            self.draw_row(
                                frame,
                                visible_row.y,
                                row_height,
                                price,
                                qty,
                                true,
                                bid_color,
                                text_color,
                                maxima.vis_max_order_qty,
                                visible_row.buy_t,
                                visible_row.sell_t,
                                maxima.vis_max_trade_qty,
                                bid_color,
                                ask_color,
                                &cols,
                            );
                        }
                    }

                    // Drawn over the row so the fill (a green best-bid cell, a red ask
                    // cell) still reads through the hollow middle of the marker.
                    if Some(visible_row.row.price()) == last_trade_price {
                        Self::outline_price_cell(
                            frame,
                            &cols,
                            visible_row.y,
                            row_height,
                            palette.warning.strong.color,
                        );
                    }
                }

                // The bid/ask frontier: a hairline where the book flips sides, now that
                // no row is spent on the spread.
                let frontier_y = self.row_top_y(0, bounds.height, row_height);
                if frontier_y > 0.0 && frontier_y < bounds.height {
                    frame.fill_rectangle(
                        Point::new(0.0, frontier_y.floor()),
                        Size::new(bounds.width, 1.0),
                        divider_color,
                    );
                }

                if self.config.show_chase_tracker
                    && let Some(grid) = grid.as_ref()
                {
                    // Measured off the price column rather than its neighbour, so the
                    // trail keeps hugging the price whichever columns sit beside it.
                    let half_gap = (layout.inside_pad_px + COL_PADDING) * 0.5;
                    let left_gap_mid_x = cols.price.0 - COL_PADDING + half_gap;
                    let right_gap_mid_x = cols.price.1 + COL_PADDING - half_gap;

                    self.draw_chase_trail(
                        frame,
                        grid,
                        bounds,
                        self.chase_tracker(Side::Bid),
                        right_gap_mid_x,
                        best_ask_y.map(|y| y + row_height / 2.0),
                        palette.success.weak.color,
                        true, // is_bid
                    );
                    self.draw_chase_trail(
                        frame,
                        grid,
                        bounds,
                        self.chase_tracker(Side::Ask),
                        left_gap_mid_x,
                        best_bid_y.map(|y| y + row_height / 2.0),
                        palette.danger.weak.color,
                        false,
                    );
                }

                // Price column vertical dividers
                let mut draw_vsplit = |x: f32| {
                    let x = x.floor() + 0.5;
                    frame.fill_rectangle(
                        Point::new(x, 0.0),
                        Size::new(1.0, bounds.height),
                        divider_color,
                    );
                };
                // The rules bracket the price column itself, not whichever column
                // happens to sit next to it.
                draw_vsplit(cols.price.0 - COL_PADDING);
                draw_vsplit(cols.price.1 + COL_PADDING);
            });

            // Outside the clip and drawn last, so the header always sits on top.
            Self::draw_column_header(frame, full_bounds, &cols, palette, divider_color);
        });

        vec![orderbook_visual]
    }
}

#[derive(Default)]
struct Maxima {
    vis_max_order_qty: f32,
    vis_max_trade_qty: f32,
}

struct VisibleRow {
    row: DomRow,
    y: f32,
    buy_t: Qty,
    sell_t: Qty,
}

struct ColumnRanges {
    bid_order: (f32, f32),
    sell: (f32, f32),
    price: (f32, f32),
    buy: (f32, f32),
    ask_order: (f32, f32),
    /// Whether the columns were laid out inverted. Bars and labels anchor to the *slot*
    /// a column sits in — outer columns against the panel edge, inner ones against the
    /// price — so the ladder keeps its shape and only the contents move.
    inverted: bool,
}

impl ColumnRanges {
    fn bid_order_from_left(&self) -> bool {
        !self.inverted
    }

    fn sell_from_left(&self) -> bool {
        self.inverted
    }

    fn buy_from_left(&self) -> bool {
        !self.inverted
    }

    fn ask_order_from_left(&self) -> bool {
        self.inverted
    }
}

/// Anchor point and alignment for a cell's text, on the same edge its bar grows from.
fn cell_anchor((start, end): (f32, f32), from_left: bool) -> (f32, Alignment) {
    if from_left {
        (start + CELL_TEXT_PAD, Alignment::Start)
    } else {
        (end - CELL_TEXT_PAD, Alignment::End)
    }
}

/// Compares two chase-trail segments for the arrival-driven dirty check. Prices are
/// exact (`Price` is an integer grid); the fade alpha is a float, so it compares
/// with a tight tolerance instead of bitwise equality.
fn chase_segment_eq(a: &Option<(Price, Price, f32)>, b: &Option<(Price, Price, f32)>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some((a_start, a_end, a_alpha)), Some((b_start, b_end, b_alpha))) => {
            a_start == b_start && a_end == b_end && (a_alpha - b_alpha).abs() < 1e-6
        }
        _ => false,
    }
}

struct PriceLayout {
    price_px: f32,
    inside_pad_px: f32,
}

/// [BidOrderQty][SellQty][ Price ][BuyQty][AskOrderQty]
const NUMBER_OF_COLUMN_GAPS: f32 = 4.0;

impl Ladder {
    fn price_sample_text(&self, grid: Option<&PriceGrid>) -> String {
        let Some(grid) = grid else {
            return self.format_price(Price::from_f64(0.0));
        };

        let a = self.format_price(grid.best_ask());
        let b = self.format_price(grid.best_bid());
        if a.len() >= b.len() { a } else { b }
    }

    fn mono_text_width_px(text_len: usize, size: f32) -> f32 {
        (text_len as f32) * size * MONO_CHAR_ADVANCE
    }

    /// Dynamic row height so the quoted book fills the panel: the rows stretch until the
    /// deepest quoted level on the busier side reaches the panel edge (the bid/ask
    /// frontier stays centred, so both sides need the same room). Rows above/below the book are
    /// still drawn — as bare price-scale rows — so the panel is always filled edge to
    /// edge. `height` is the body height below the column header.
    fn ladder_row_height(&self, height: f32, grid: Option<&PriceGrid>) -> f32 {
        let rows = grid.map_or(1, PriceGrid::rows_to_fit_book).max(1);
        let h = height.max(0.0);
        (h / rows as f32).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT)
    }

    /// Text size tracks the row height so glyphs stay proportioned as the ladder
    /// stretches (and never overlap when the panel gets short).
    fn text_size_for(&self, row_height: f32) -> f32 {
        (row_height * 0.62).clamp(MIN_TEXT_SIZE, MAX_TEXT_SIZE)
    }

    /// Screen y of a row's top edge. Rows are contiguous — no row is spent on the spread,
    /// so the scale steps by exactly one tick from top to bottom. Row `0` (the best bid)
    /// starts at mid height, asks climb above it (`idx < 0`, prices increasing upward)
    /// and bids descend below it (`idx > 0`), which puts the bid/ask frontier in the
    /// middle of the panel. `scroll_px` slides the whole scale, so prices outside the
    /// quoted book can be scrolled into view.
    fn row_top_y(&self, idx: i32, bounds_height: f32, row_height: f32) -> f32 {
        let mid_y = bounds_height * 0.5;
        mid_y + (idx as f32) * row_height - self.scroll_px
    }

    fn price_layout_for(
        &self,
        total_width: f32,
        grid: Option<&PriceGrid>,
        row_height: f32,
    ) -> PriceLayout {
        let sample = self.price_sample_text(grid);
        let text_px = Self::mono_text_width_px(sample.len(), self.text_size_for(row_height));

        let desired_total_gap = CHASE_CIRCLE_RADIUS * 2.0 + 4.0;
        let inside_pad_px = PRICE_TEXT_SIDE_PAD_MIN
            .max(desired_total_gap - COL_PADDING)
            .max(0.0);

        let price_px = (text_px + 2.0 * inside_pad_px).min(total_width.max(0.0));

        PriceLayout {
            price_px,
            inside_pad_px,
        }
    }

    fn column_ranges(&self, width: f32, price_px: f32) -> ColumnRanges {
        column_ranges_for(width, price_px, self.config.invert_layout)
    }
}

/// Lays the five columns out left to right, with the price column in the middle.
///
/// `inverted` swaps each side's resting-size column with its traded-size one — the tape
/// goes outside and the book sits against the price scale. Widths travel with the
/// column, so inverting moves a column without resizing it.
fn column_ranges_for(width: f32, price_px: f32, inverted: bool) -> ColumnRanges {
    {
        let total_gutter_width = COL_PADDING * NUMBER_OF_COLUMN_GAPS;
        let usable_width = (width - total_gutter_width).max(0.0);

        let price_width = price_px.min(usable_width);

        let rest = (usable_width - price_width).max(0.0);
        let rest_ratio = ORDER_QTY_COLS_WIDTH + TRADE_QTY_COLS_WIDTH; // 0.80

        let order_share = if rest_ratio > 0.0 {
            (ORDER_QTY_COLS_WIDTH / rest_ratio) * rest
        } else {
            0.0
        };
        let trade_share = if rest_ratio > 0.0 {
            (TRADE_QTY_COLS_WIDTH / rest_ratio) * rest
        } else {
            0.0
        };

        let order_width = order_share * 0.5;
        let trade_width = trade_share * 0.5;

        let (outer_left, inner_left) = if inverted {
            (trade_width, order_width)
        } else {
            (order_width, trade_width)
        };
        let (inner_right, outer_right) = if inverted {
            (order_width, trade_width)
        } else {
            (trade_width, order_width)
        };

        let mut cursor_x = 0.0;
        let mut place = |width: f32| {
            let range = (cursor_x, cursor_x + width);
            cursor_x += width + COL_PADDING;
            range
        };

        let outer_left_range = place(outer_left);
        let inner_left_range = place(inner_left);
        let price_range = place(price_width);
        let inner_right_range = place(inner_right);
        let outer_right_range = place(outer_right);

        let (bid_order_range, sell_trades_range) = if inverted {
            (inner_left_range, outer_left_range)
        } else {
            (outer_left_range, inner_left_range)
        };
        let (buy_trades_range, ask_order_range) = if inverted {
            (outer_right_range, inner_right_range)
        } else {
            (inner_right_range, outer_right_range)
        };

        ColumnRanges {
            bid_order: bid_order_range,
            sell: sell_trades_range,
            price: price_range,
            buy: buy_trades_range,
            ask_order: ask_order_range,
            inverted,
        }
    }
}

impl Ladder {
    fn draw_row(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        y: f32,
        row_height: f32,
        price: Price,
        order_qty: Qty,
        is_bid: bool,
        side_color: iced::Color,
        text_color: iced::Color,
        max_order_qty: f32,
        trade_buy_qty: Qty,
        trade_sell_qty: Qty,
        max_trade_qty: f32,
        trade_buy_color: iced::Color,
        trade_sell_color: iced::Color,
        cols: &ColumnRanges,
    ) {
        let order_qty_f32 = order_qty.to_f32_lossy();
        let trade_buy_qty_f32 = trade_buy_qty.to_f32_lossy();
        let trade_sell_qty_f32 = trade_sell_qty.to_f32_lossy();

        // A price the book is not quoting is still a row on the scale, but it carries no
        // size: it shows the price (dimmed) and nothing else — never a `0`.
        let has_depth = order_qty_f32 > 0.0;
        let qty_txt = if has_depth {
            self.format_quantity(order_qty)
        } else {
            String::new()
        };

        let (order_col, order_from_left) = if is_bid {
            (cols.bid_order, cols.bid_order_from_left())
        } else {
            (cols.ask_order, cols.ask_order_from_left())
        };

        Self::fill_bar(
            frame,
            order_col,
            y,
            row_height,
            order_qty_f32,
            max_order_qty,
            side_color,
            order_from_left,
            0.20,
        );
        let (x_text, align) = cell_anchor(order_col, order_from_left);
        Self::draw_cell_text(frame, &qty_txt, x_text, y, row_height, text_color, align);

        // Sell trades
        let sell_from_left = cols.sell_from_left();
        Self::fill_bar(
            frame,
            cols.sell,
            y,
            row_height,
            trade_sell_qty_f32,
            max_trade_qty,
            trade_sell_color,
            sell_from_left,
            0.30,
        );
        let sell_txt = if trade_sell_qty_f32 > 0.0 {
            self.format_quantity(trade_sell_qty)
        } else {
            "".into()
        };
        let (x_text, align) = cell_anchor(cols.sell, sell_from_left);
        Self::draw_cell_text(frame, &sell_txt, x_text, y, row_height, text_color, align);

        // Buy trades
        let buy_from_left = cols.buy_from_left();
        Self::fill_bar(
            frame,
            cols.buy,
            y,
            row_height,
            trade_buy_qty_f32,
            max_trade_qty,
            trade_buy_color,
            buy_from_left,
            0.30,
        );
        let buy_txt = if trade_buy_qty_f32 > 0.0 {
            self.format_quantity(trade_buy_qty)
        } else {
            "".into()
        };
        let (x_text, align) = cell_anchor(cols.buy, buy_from_left);
        Self::draw_cell_text(frame, &buy_txt, x_text, y, row_height, text_color, align);

        // Price
        let price_text = self.format_price(price);
        let price_x_center = (cols.price.0 + cols.price.1) * 0.5;
        let price_color = if has_depth {
            side_color
        } else {
            text_color.scale_alpha(EMPTY_PRICE_ALPHA)
        };
        Self::draw_cell_text(
            frame,
            &price_text,
            price_x_center,
            y,
            row_height,
            price_color,
            Alignment::Center,
        );
    }

    /// ACTerminal labels the L2 columns above the ladder ("Bid Qty / Price / Ask Qty");
    /// this ladder carries two extra per-price trade columns, so those get labelled too.
    fn draw_column_header(
        frame: &mut iced::widget::canvas::Frame,
        bounds: Rectangle,
        cols: &ColumnRanges,
        palette: &iced::theme::palette::Extended,
        divider_color: iced::Color,
    ) {
        frame.fill_rectangle(
            Point::new(0.0, 0.0),
            Size::new(bounds.width, HEADER_HEIGHT),
            palette.background.weak.color,
        );

        frame.fill_rectangle(
            Point::new(0.0, HEADER_HEIGHT - 1.0),
            Size::new(bounds.width, 1.0),
            divider_color,
        );

        let label_color = palette.background.weak.text;
        let mut label = |content: &str, x: f32, align: Alignment| {
            frame.fill_text(Text {
                content: content.to_string(),
                position: Point::new(x, HEADER_HEIGHT * 0.5),
                color: label_color,
                size: style::text_size::TINY.into(),
                font: style::AZERET_MONO,
                align_x: align.into(),
                align_y: Alignment::Center.into(),
                ..Default::default()
            });
        };

        let mut labelled = |content: &str, range: (f32, f32), from_left: bool| {
            let (x, align) = cell_anchor(range, from_left);
            label(content, x, align);
        };

        labelled("Bid Qty", cols.bid_order, cols.bid_order_from_left());
        labelled("Sell", cols.sell, cols.sell_from_left());
        labelled("Buy", cols.buy, cols.buy_from_left());
        labelled("Ask Qty", cols.ask_order, cols.ask_order_from_left());
        label(
            "Price",
            (cols.price.0 + cols.price.1) * 0.5,
            Alignment::Center,
        );
    }

    /// Tints the price cell of the top-of-book row, matching ACTerminal's
    /// `.best-bid`/`.best-ask` styling.
    fn fill_best_price_cell(
        frame: &mut iced::widget::canvas::Frame,
        cols: &ColumnRanges,
        y: f32,
        row_height: f32,
        side_color: iced::Color,
    ) {
        frame.fill_rectangle(
            Point::new(cols.price.0, y),
            Size::new(cols.price.1 - cols.price.0, row_height),
            iced::Color {
                a: 0.22,
                ..side_color
            },
        );
    }

    /// Marks the price the market last traded at: same cell as the best bid/ask tint but
    /// hollow, so the row's own colour still shows through the middle.
    fn outline_price_cell(
        frame: &mut iced::widget::canvas::Frame,
        cols: &ColumnRanges,
        y: f32,
        row_height: f32,
        color: iced::Color,
    ) {
        let width = cols.price.1 - cols.price.0;
        if width <= 0.0 || row_height <= 0.0 {
            return;
        }

        let inset = LAST_PRICE_BORDER_WIDTH * 0.5;
        let rect = Path::rectangle(
            Point::new(cols.price.0 + inset, y + inset),
            Size::new(
                (width - LAST_PRICE_BORDER_WIDTH).max(0.0),
                (row_height - LAST_PRICE_BORDER_WIDTH).max(0.0),
            ),
        );

        frame.stroke(
            &rect,
            Stroke::default()
                .with_color(color)
                .with_width(LAST_PRICE_BORDER_WIDTH),
        );
    }

    fn fill_bar(
        frame: &mut iced::widget::canvas::Frame,
        (x_start, x_end): (f32, f32),
        y: f32,
        height: f32,
        value: f32,
        scale_value_max: f32,
        color: iced::Color,
        from_left: bool,
        alpha: f32,
    ) {
        if scale_value_max <= 0.0 || value <= 0.0 {
            return;
        }
        let col_width = x_end - x_start;

        let mut bar_width = (value / scale_value_max) * col_width.max(1.0);
        bar_width = bar_width.min(col_width);
        let bar_x = if from_left {
            x_start
        } else {
            x_end - bar_width
        };

        frame.fill_rectangle(
            Point::new(bar_x, y),
            Size::new(bar_width, height),
            iced::Color { a: alpha, ..color },
        );
    }

    fn draw_cell_text(
        frame: &mut iced::widget::canvas::Frame,
        text: &str,
        x_anchor: f32,
        y: f32,
        row_height: f32,
        color: iced::Color,
        align: Alignment,
    ) {
        frame.fill_text(Text {
            content: text.to_string(),
            position: Point::new(x_anchor, y + row_height / 2.0),
            color,
            size: (row_height * 0.62)
                .clamp(MIN_TEXT_SIZE, MAX_TEXT_SIZE)
                .into(),
            font: style::AZERET_MONO,
            align_x: align.into(),
            align_y: Alignment::Center.into(),
            ..Default::default()
        });
    }

    fn draw_chase_trail(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        grid: &PriceGrid,
        bounds: Rectangle,
        tracker: &ChaseTracker,
        pos_x: f32,
        best_offer_y: Option<f32>,
        color: iced::Color,
        is_bid: bool,
    ) {
        let radius = CHASE_CIRCLE_RADIUS;
        if let Some((start_p_raw, end_p_raw, alpha)) = tracker.segment() {
            let start_p = start_p_raw.round_to_side_step(is_bid, self.step);
            let end_p = end_p_raw.round_to_side_step(is_bid, self.step);

            let color = color.scale_alpha(alpha);
            let stroke_w = 2.0;
            let pad_to_circle = radius + stroke_w * 0.5;

            let start_y = self.price_to_screen_y(start_p, grid, bounds.height);
            let end_y = self
                .price_to_screen_y(end_p, grid, bounds.height)
                .or(best_offer_y);

            if let Some(end_y) = end_y {
                if let Some(start_y) = start_y {
                    let dy = end_y - start_y;
                    if dy.abs() > pad_to_circle {
                        let line_end_y = end_y - dy.signum() * pad_to_circle;
                        let line_path =
                            Path::line(Point::new(pos_x, start_y), Point::new(pos_x, line_end_y));
                        frame.stroke(
                            &line_path,
                            Stroke::default().with_color(color).with_width(stroke_w),
                        );
                    }
                }

                let circle = &Path::circle(Point::new(pos_x, end_y), radius);
                frame.fill(circle, color);
            }
        }
    }

    /// Anchors the price scale on the current top of book.
    ///
    /// The scale itself is unbounded — it is generated on the fly from the anchor, so the
    /// ladder always shows a continuous run of prices (one row per `step`), not just the
    /// prices that happen to be quoted right now.
    fn build_price_grid(&self) -> Option<PriceGrid> {
        let step = self.step;

        let best_bid = self.grouped_bids().last_key_value().map(|(p, _)| *p);
        let best_ask = self.grouped_asks().first_key_value().map(|(p, _)| *p);

        // Row 0 is the best bid; when the book is one-sided (or gone) fall back to the
        // ask, then to the traded range, so the scale still has somewhere to sit.
        let anchor_bid = match (best_bid, best_ask) {
            (Some(bb), _) => bb,
            (None, Some(ba)) => ba.add_steps(-1, step),
            (None, None) => {
                let (min_t, max_t) = self.trades.price_range()?;
                let steps = Price::steps_between_inclusive(min_t, max_t, step).unwrap_or(1);
                max_t.add_steps(-(steps as i64 / 2), step)
            }
        };

        // How far the quoted book reaches from the spread, in rows — only used to size
        // the rows so the whole book fits the panel.
        let ask_rows = self
            .grouped_asks()
            .last_key_value()
            .and_then(|(p, _)| Price::steps_between_inclusive(anchor_bid, *p, step))
            .unwrap_or(1);
        let bid_rows = self
            .grouped_bids()
            .first_key_value()
            .and_then(|(p, _)| Price::steps_between_inclusive(*p, anchor_bid, step))
            .unwrap_or(1);

        Some(PriceGrid {
            anchor_bid,
            step,
            ask_rows,
            bid_rows,
        })
    }

    fn visible_rows(
        &self,
        bounds: Rectangle,
        grid: &PriceGrid,
        row_height: f32,
    ) -> (Vec<VisibleRow>, Maxima) {
        let asks_grouped = self.grouped_asks();
        let bids_grouped = self.grouped_bids();

        let mut visible: Vec<VisibleRow> = Vec::new();
        let mut maxima = Maxima::default();

        if row_height <= 0.0 {
            return (visible, maxima);
        }

        // Only the rows that actually intersect the viewport are built, so the scale can
        // run as far as the user scrolls without any cost per hidden row.
        let y0 = self.row_top_y(0, bounds.height, row_height);
        let idx_top = ((0.0 - y0) / row_height).floor() as i32;
        let rows_needed = (bounds.height / row_height).ceil() as i32 + 1;
        let idx_bottom = idx_top.saturating_add(rows_needed);

        for idx in idx_top..=idx_bottom {
            let top_y_screen = self.row_top_y(idx, bounds.height, row_height);
            if top_y_screen >= bounds.height || top_y_screen + row_height <= 0.0 {
                continue;
            }

            let Some(price) = grid.index_to_price(idx) else {
                continue;
            };

            let is_bid = PriceGrid::is_bid_row(idx);
            // Empty on every price the book is not quoting — those rows are drawn as
            // bare price-scale rows, with whatever trades happened there.
            let order_qty = if is_bid {
                bids_grouped.get(&price).copied().unwrap_or_default()
            } else {
                asks_grouped.get(&price).copied().unwrap_or_default()
            };

            maxima.vis_max_order_qty = maxima.vis_max_order_qty.max(order_qty.to_f32_lossy());
            let (buy_t, sell_t) = self.trade_qty_at(price);
            maxima.vis_max_trade_qty = maxima
                .vis_max_trade_qty
                .max(buy_t.to_f32_lossy().max(sell_t.to_f32_lossy()));

            let row = if is_bid {
                DomRow::Bid {
                    price,
                    qty: order_qty,
                }
            } else {
                DomRow::Ask {
                    price,
                    qty: order_qty,
                }
            };

            visible.push(VisibleRow {
                row,
                y: top_y_screen,
                buy_t,
                sell_t,
            });
        }

        visible.sort_by(|a, b| a.y.total_cmp(&b.y));
        (visible, maxima)
    }

    fn price_to_screen_y(&self, price: Price, grid: &PriceGrid, bounds_height: f32) -> Option<f32> {
        let row_height = self.ladder_row_height(bounds_height, Some(grid));
        let idx = grid.price_to_index(price)?;

        Some(self.row_top_y(idx, bounds_height, row_height) + row_height / 2.0)
    }
}

enum DomRow {
    Ask { price: Price, qty: Qty },
    Bid { price: Price, qty: Qty },
}

impl DomRow {
    fn price(&self) -> Price {
        match self {
            DomRow::Ask { price, .. } | DomRow::Bid { price, .. } => *price,
        }
    }
}

/// The ladder's price scale: an unbounded run of consecutive prices, one row per `step`,
/// anchored on the top of book.
///
/// Row `0` is the best bid and rows count outward from there — `1` is one step below it,
/// `-1` one step above (the first ask row), and so on. Every row in between exists
/// whether or not the book quotes it, so a price the book has walked away from keeps its
/// place on the scale (and keeps showing the trades that happened there) instead of
/// vanishing from the ladder.
struct PriceGrid {
    /// Grouped best bid — row `0`, the anchor the rest of the scale is derived from.
    anchor_bid: Price,
    /// Grid spacing between consecutive rows (e.g. 0.50 for WDO).
    step: PriceStep,
    /// Rows from the anchor out to the highest quoted ask / lowest quoted bid. Only used
    /// to pick a row height that fits the quoted book in the panel.
    ask_rows: usize,
    bid_rows: usize,
}

impl PriceGrid {
    fn best_bid(&self) -> Price {
        self.anchor_bid
    }

    fn best_ask(&self) -> Price {
        self.anchor_bid.add_steps(1, self.step)
    }

    /// Rows the panel needs to show the whole quoted book with the frontier centred.
    fn rows_to_fit_book(&self) -> usize {
        2 * self.ask_rows.max(self.bid_rows)
    }

    /// Price on row `idx`, or `None` for rows that would run off the bottom of the price
    /// axis (e.g. scrolling far below zero).
    fn index_to_price(&self, idx: i32) -> Option<Price> {
        let units = i64::from(idx)
            .checked_neg()
            .and_then(|offset| offset.checked_mul(self.step.units))
            .and_then(|delta| self.anchor_bid.units.checked_add(delta))?;

        (units > 0).then(|| Price::from_units(units))
    }

    /// Row a price sits on — the inverse of [`Self::index_to_price`].
    fn price_to_index(&self, price: Price) -> Option<i32> {
        let step_units = self.step.units;
        if step_units <= 0 {
            return None;
        }

        let steps = price
            .units
            .checked_sub(self.anchor_bid.units)?
            .div_euclid(step_units);

        i32::try_from(-steps).ok()
    }

    /// Anything above the anchor is quoted by the ask side of the book.
    fn is_bid_row(idx: i32) -> bool {
        idx >= 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(best_bid: f64, step: f64) -> PriceGrid {
        PriceGrid {
            anchor_bid: Price::from_f64(best_bid),
            step: PriceStep::from_f64_exact(step),
            ask_rows: 1,
            bid_rows: 1,
        }
    }

    fn test_ladder() -> Ladder {
        let ticker = exchange::Ticker::new("6E", exchange::adapter::Exchange::AcfeedFutures);
        let info = exchange::TickerInfo::new(ticker, 0.00005, 1.0, None);
        Ladder::new(None, info, PriceStep::from_f64_exact(0.00005))
    }

    fn test_depth(bid_qty: f64, ask_qty: f64) -> Depth {
        let mut depth = Depth::default();
        depth
            .bids
            .insert(Price::from_f64(1.08420), Qty::from_f64(bid_qty));
        depth
            .asks
            .insert(Price::from_f64(1.08425), Qty::from_f64(ask_qty));
        depth
    }

    fn test_trade(price: f64) -> Trade {
        Trade {
            time: UnixMs::new(1_772_000_000_000),
            is_sell: false,
            price: Price::from_f64(price),
            qty: Qty::from_f64(1.0),
        }
    }

    /// Arrival-driven redraw: a print must mark the ladder dirty immediately instead
    /// of waiting for the 100ms pane tick (the yellow last-trade marker lives here).
    /// An empty buffer changes nothing visible, so it must not touch `last_tick`
    /// (otherwise the tick fallback window would restart for no reason).
    #[test]
    fn trades_arrive_dirty_immediately_but_empty_buffers_stay_quiet() {
        let mut ladder = test_ladder();
        let before = ladder.last_update();
        std::thread::sleep(std::time::Duration::from_millis(2));

        ladder.insert_trades(&[]);
        assert_eq!(
            ladder.last_update(),
            before,
            "empty trade buffer must not invalidate"
        );

        ladder.insert_trades(&[test_trade(1.08420)]);
        assert!(
            ladder.last_update() > before,
            "non-empty trade buffer must invalidate immediately"
        );
    }

    /// The DOM topic republishes on a fixed cadence even when nothing moved: an
    /// identical snapshot must not clear the canvas, while a real size change must
    /// invalidate on arrival (same rationale as the trades test above).
    #[test]
    fn identical_depth_snapshot_stays_quiet_but_a_real_move_redraws() {
        let mut ladder = test_ladder();
        let t0 = UnixMs::new(1_772_000_000_000);
        // Disable the chase trail so only the book itself drives the dirty check.
        ladder.set_show_chase_tracker(false);
        ladder.insert_depth(&test_depth(3.0, 5.0), t0);
        let after_first = ladder.last_update();
        std::thread::sleep(std::time::Duration::from_millis(2));

        ladder.insert_depth(&test_depth(3.0, 5.0), UnixMs::new(t0.as_u64() + 50));
        assert_eq!(
            ladder.last_update(),
            after_first,
            "identical DOM snapshot must not invalidate"
        );

        ladder.insert_depth(&test_depth(4.0, 5.0), UnixMs::new(t0.as_u64() + 100));
        assert!(
            ladder.last_update() > after_first,
            "changed DOM size must invalidate immediately"
        );
    }

    fn price_at(g: &PriceGrid, idx: i32) -> f64 {
        g.index_to_price(idx).unwrap().to_f64()
    }

    fn width_of((start, end): (f32, f32)) -> f32 {
        end - start
    }

    /// Order left to right, as the columns are actually drawn.
    fn slots(cols: &ColumnRanges) -> Vec<(&'static str, (f32, f32))> {
        let mut slots = vec![
            ("bid", cols.bid_order),
            ("sell", cols.sell),
            ("price", cols.price),
            ("buy", cols.buy),
            ("ask", cols.ask_order),
        ];
        slots.sort_by(|a, b| a.1.0.total_cmp(&b.1.0));
        slots
    }

    /// Inverting swaps each side's book column with its tape column and leaves everything
    /// else alone: the price stays in the middle, and a column keeps the width it had.
    #[test]
    fn inverting_the_layout_swaps_the_columns_without_resizing_them() {
        let normal = column_ranges_for(400.0, 80.0, false);
        let inverted = column_ranges_for(400.0, 80.0, true);

        let order: Vec<&str> = slots(&normal).iter().map(|(name, _)| *name).collect();
        assert_eq!(order, ["bid", "sell", "price", "buy", "ask"]);

        let order: Vec<&str> = slots(&inverted).iter().map(|(name, _)| *name).collect();
        assert_eq!(order, ["sell", "bid", "price", "ask", "buy"]);

        for (name, range, inv_range) in [
            ("bid", normal.bid_order, inverted.bid_order),
            ("sell", normal.sell, inverted.sell),
            ("price", normal.price, inverted.price),
            ("buy", normal.buy, inverted.buy),
            ("ask", normal.ask_order, inverted.ask_order),
        ] {
            assert!(
                (width_of(range) - width_of(inv_range)).abs() < 1e-4,
                "{name} changed width when the layout was inverted"
            );
        }

        assert_eq!(
            normal.price, inverted.price,
            "the price column does not move"
        );
    }

    /// Bars and labels anchor to the slot, not to the column — an outer column grows from
    /// the panel edge and an inner one from the price, so the ladder reads the same and
    /// only the contents move.
    #[test]
    fn cells_anchor_to_their_slot_in_both_layouts() {
        let normal = column_ranges_for(400.0, 80.0, false);
        assert!(
            normal.bid_order_from_left(),
            "outer-left grows from the edge"
        );
        assert!(!normal.sell_from_left(), "inner-left grows from the price");
        assert!(normal.buy_from_left(), "inner-right grows from the price");
        assert!(
            !normal.ask_order_from_left(),
            "outer-right grows from the edge"
        );

        let inverted = column_ranges_for(400.0, 80.0, true);
        assert!(
            !inverted.bid_order_from_left(),
            "bid is now the inner column"
        );
        assert!(inverted.sell_from_left(), "sell is now the outer column");
        assert!(!inverted.buy_from_left());
        assert!(inverted.ask_order_from_left());

        let (x, align) = cell_anchor((10.0, 50.0), true);
        assert!((x - (10.0 + CELL_TEXT_PAD)).abs() < 1e-6);
        assert_eq!(align, Alignment::Start);

        let (x, align) = cell_anchor((10.0, 50.0), false);
        assert!((x - (50.0 - CELL_TEXT_PAD)).abs() < 1e-6);
        assert_eq!(align, Alignment::End);
    }

    /// The scale runs one step per row in both directions, whatever the book is quoting,
    /// and no row is spent on the spread: row 0 is the best bid, row -1 the first ask
    /// row, so the bid and the ask sit one tick apart like every other pair of rows.
    #[test]
    fn scale_is_continuous_across_the_frontier() {
        let g = grid(5136.00, 0.5);

        assert!((price_at(&g, 0) - 5136.00).abs() < 1e-9);
        assert!((price_at(&g, -1) - 5136.50).abs() < 1e-9);
        assert!((price_at(&g, 1) - 5135.50).abs() < 1e-9);
        assert!((price_at(&g, 5) - 5133.50).abs() < 1e-9);
        assert!((price_at(&g, -4) - 5138.00).abs() < 1e-9);

        for idx in -6..6 {
            let gap = price_at(&g, idx) - price_at(&g, idx + 1);
            assert!((gap - 0.5).abs() < 1e-9, "row {idx} is not one step wide");
        }
    }

    #[test]
    fn rows_are_split_into_sides_at_the_best_bid() {
        assert!(PriceGrid::is_bid_row(0), "row 0 is the best bid");
        assert!(PriceGrid::is_bid_row(3));
        assert!(!PriceGrid::is_bid_row(-1), "row -1 is the first ask row");
    }

    /// Scrolling walks the same scale far past the quoted book, so the rows have to keep
    /// resolving to prices (and stop at the bottom of the price axis, never going negative).
    #[test]
    fn scale_extends_far_beyond_the_quoted_book() {
        let g = grid(10.0, 0.5);

        assert!((price_at(&g, -200) - 110.0).abs() < 1e-9);
        assert!((price_at(&g, 19) - 0.5).abs() < 1e-9);
        assert!(
            g.index_to_price(20).is_none(),
            "the scale stops at zero, it does not wrap into negative prices"
        );
    }

    #[test]
    fn price_to_index_inverts_index_to_price() {
        let g = grid(158.43, 0.01);

        for idx in [-30, -3, -1, 0, 1, 2, 40] {
            let price = g.index_to_price(idx).unwrap();
            assert_eq!(g.price_to_index(price), Some(idx), "row {idx}");
        }
    }

    /// Rows are sized off the deepest side so the whole quoted book fits with the bid/ask
    /// frontier centred, then clamped so a thin book does not stretch into giant rows.
    #[test]
    fn row_height_fits_the_book_then_fills_with_scale_rows() {
        let mut g = grid(100.0, 0.5);
        g.ask_rows = 10;
        g.bid_rows = 4;

        assert_eq!(g.rows_to_fit_book(), 20, "the deepest side, both ways");

        let ladder = |height: f32, g: &PriceGrid| -> f32 {
            let rows = g.rows_to_fit_book();
            (height / rows as f32).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT)
        };

        assert!((ladder(400.0, &g) - 20.0).abs() < 1e-6);
        assert_eq!(ladder(2000.0, &g), MAX_ROW_HEIGHT, "thin book stays sane");
        assert_eq!(
            ladder(100.0, &g),
            MIN_ROW_HEIGHT,
            "deep book stays readable"
        );
    }
}
