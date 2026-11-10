use crate::widget::chart::heatmap::scene::depth_grid::HeatmapPalette;
use crate::widget::chart::heatmap::scene::pipeline::circle::CircleInstance;
use crate::widget::chart::heatmap::scene::pipeline::rectangle::RectInstance;
use crate::widget::chart::heatmap::scene::pipeline::{DrawItem, DrawLayer, DrawOp};
use crate::widget::chart::heatmap::view::ViewWindow;

use data::aggr::time::TimeSeries;
use data::chart::bookmap::{BubbleScale, Config as BookmapConfig};
use data::chart::heatmap::{GroupedTrade, HeatmapDataPoint, HistoricalDepth, ProfileKind};
use exchange::UnixMs;
use exchange::unit::{Price, PriceStep, Qty};

/// A large print's bubble, kept so the overlay canvas can write its size next to it.
///
/// ACTerminal labels every trade at or above `bubble_large_qty` with its quantity. The
/// bubbles themselves are GPU circle instances and carry no text, so the position is
/// handed to the overlay in the same (bin, world-y) frame the circle shader uses and the
/// label is drawn there.
#[derive(Debug, Clone, Copy)]
pub struct LargeTradeLabel {
    pub x_bin_rel: i32,
    pub y_world: f32,
    pub qty: f64,
    pub is_sell: bool,
}

/// One sample of the best-bid/ask line: a bucket, and the price row it sat at.
/// `y_world` is `None` for a bucket the book never quoted, which breaks the path.
#[derive(Debug, Clone, Copy)]
pub struct BidAskPoint {
    pub x_bin_rel: i32,
    pub y_world: Option<f32>,
}

/// The best-bid/ask traces plus the newest price on each side, for the scale tags.
#[derive(Debug, Clone, Default)]
pub struct BidAskLine {
    pub bid: Vec<BidAskPoint>,
    pub ask: Vec<BidAskPoint>,
    pub newest_bid: Option<Price>,
    pub newest_ask: Option<Price>,
}

#[derive(Debug, Clone)]
pub struct OverlayBuild {
    pub circles: Vec<CircleInstance>,
    pub rects: Vec<RectInstance>,
    pub large_trades: Vec<LargeTradeLabel>,
    /// Prices of the detected walls, so the axis can tag them where their line ends.
    pub wall_prices: Vec<(Price, bool)>,
    /// Best bid/ask traces, stroked by the overlay canvas.
    pub bid_ask_line: BidAskLine,

    // Ranges into `rects` for typed layering.
    pub rect_depth_profile: std::ops::Range<u32>,
    pub rect_walls: std::ops::Range<u32>,
    pub rect_volume: std::ops::Range<u32>,
    pub rect_volume_profile: std::ops::Range<u32>,
}

impl OverlayBuild {
    fn count(r: &std::ops::Range<u32>) -> u32 {
        r.end.saturating_sub(r.start)
    }

    pub fn draw_list(&self) -> Vec<DrawItem> {
        let mut out = Vec::new();

        // Background
        out.push(DrawItem::new(DrawLayer::HEATMAP, DrawOp::Heatmap));

        // Behind circles
        if Self::count(&self.rect_depth_profile) > 0 {
            out.push(DrawItem::new(
                DrawLayer::DEPTH_PROFILE,
                DrawOp::Rects {
                    start: self.rect_depth_profile.start,
                    count: Self::count(&self.rect_depth_profile),
                },
            ));
        }

        // Liquidity walls (drawn under trade bubbles, over the heatmap/depth profile)
        if Self::count(&self.rect_walls) > 0 {
            out.push(DrawItem::new(
                DrawLayer::WALLS,
                DrawOp::Rects {
                    start: self.rect_walls.start,
                    count: Self::count(&self.rect_walls),
                },
            ));
        }

        // Circles
        if !self.circles.is_empty() {
            out.push(DrawItem::new(
                DrawLayer::CIRCLES,
                DrawOp::Circles {
                    start: 0,
                    count: self.circles.len() as u32,
                },
            ));
        }

        // Foreground overlays
        if Self::count(&self.rect_volume) > 0 {
            out.push(DrawItem::new(
                DrawLayer::VOLUME,
                DrawOp::Rects {
                    start: self.rect_volume.start,
                    count: Self::count(&self.rect_volume),
                },
            ));
        }

        if Self::count(&self.rect_volume_profile) > 0 {
            out.push(DrawItem::new(
                DrawLayer::VOLUME_PROFILE,
                DrawOp::Rects {
                    start: self.rect_volume_profile.start,
                    count: Self::count(&self.rect_volume_profile),
                },
            ));
        }

        out
    }
}

#[derive(Default)]
pub struct InstanceBuilder {
    // Reusable buffers
    volume_acc: Vec<(Qty, Qty)>,
    volume_touched: Vec<usize>,
    depth_profile_bid_acc: Vec<Qty>,
    depth_profile_ask_acc: Vec<Qty>,
    volume_profile_bid_acc: Vec<Qty>,
    volume_profile_ask_acc: Vec<Qty>,

    // Per-bucket best bid/ask scratch for the bid/ask staircase
    bba_bid: Vec<Option<Price>>,
    bba_ask: Vec<Option<Price>>,

    // Scale denominators (for external getters)
    pub depth_profile_scale_max_qty: Option<Qty>,
    pub volume_strip_scale_max_qty: Option<Qty>,
    pub volume_profile_scale_max_qty: Option<Qty>,
}

impl InstanceBuilder {
    /// Guards the per-bucket bid/ask scatter from degenerate windows; past this many
    /// visible buckets the staircase is skipped rather than allocating for it.
    const MAX_BBA_BUCKETS: usize = 8192;

    #[allow(clippy::too_many_arguments)]
    pub fn build_instances(
        &mut self,
        w: &ViewWindow,
        trades: &TimeSeries<HeatmapDataPoint>,
        latest_depth: impl IntoIterator<Item = (Price, Qty, bool)>,
        depth_history: &HistoricalDepth,
        base_price: Price,
        step: PriceStep,
        y_anchor: Option<Price>,
        latest_time: u64,
        scroll_ref_bucket: i64,
        palette: &HeatmapPalette,
        bookmap_cfg: &BookmapConfig,
        profile_kind: Option<&ProfileKind>,
        show_volume_strip: bool,
        depth_denom: f32,
    ) -> OverlayBuild {
        // Reset denoms each rebuild to avoid stale overlay labels
        self.depth_profile_scale_max_qty = None;
        self.volume_strip_scale_max_qty = None;
        self.volume_profile_scale_max_qty = None;

        let (circles, large_trades) = self.build_circles(
            w,
            trades,
            base_price,
            step,
            y_anchor,
            scroll_ref_bucket,
            palette,
            bookmap_cfg,
        );

        let mut rects: Vec<RectInstance> = Vec::new();

        let prof_start = rects.len() as u32;
        self.build_depth_profile_rects(
            w,
            latest_depth,
            base_price,
            step,
            y_anchor,
            bookmap_cfg,
            depth_denom,
            &mut rects,
        );
        let prof_end = rects.len() as u32;

        let walls_start = rects.len() as u32;
        let mut wall_prices = Vec::new();
        self.build_wall_rects(
            w,
            depth_history,
            base_price,
            step,
            y_anchor,
            scroll_ref_bucket,
            bookmap_cfg,
            &mut wall_prices,
        );
        let walls_end = rects.len() as u32;

        let bid_ask_line = self.build_bid_ask_line(
            w,
            depth_history,
            base_price,
            step,
            y_anchor,
            scroll_ref_bucket,
            bookmap_cfg,
        );

        let vol_start = rects.len() as u32;
        if show_volume_strip {
            self.build_volume_strip_rects(w, trades, scroll_ref_bucket, palette, &mut rects);
        }
        let vol_end = rects.len() as u32;

        let tp_start = rects.len() as u32;
        if let Some(kind) = profile_kind {
            self.build_volume_profile_rects(
                w,
                trades,
                base_price,
                step,
                y_anchor,
                latest_time,
                kind,
                palette,
                &mut rects,
            );
        }
        let tp_end = rects.len() as u32;

        OverlayBuild {
            circles,
            rects,
            large_trades,
            wall_prices,
            bid_ask_line,
            rect_depth_profile: prof_start..prof_end,
            rect_walls: walls_start..walls_end,
            rect_volume: vol_start..vol_end,
            rect_volume_profile: tp_start..tp_end,
        }
    }

    /// Traces the best bid and best ask across the visible buckets, mirroring
    /// ACTerminal `bookmap.ts`'s `bidSeries`/`askSeries`. Returns polyline points
    /// (drawn on the overlay canvas, so the line is genuinely continuous rather than
    /// a run of disconnected horizontal bars) plus the newest visible best bid/ask
    /// for the price-scale tags.
    ///
    /// Runs are scattered per bucket in one pass over the same window the wall
    /// detector already walks; because `iter_time_filtered` yields price levels in
    /// ascending order, the first ask run to claim a bucket is that bucket's best
    /// ask, while bids keep overwriting so the last one wins.
    #[allow(clippy::too_many_arguments)]
    fn build_bid_ask_line(
        &mut self,
        w: &ViewWindow,
        depth_history: &HistoricalDepth,
        base_price: Price,
        step: PriceStep,
        y_anchor: Option<Price>,
        ref_bucket: i64,
        bookmap_cfg: &BookmapConfig,
    ) -> BidAskLine {
        let empty = BidAskLine::default();

        if !bookmap_cfg.show_bid_ask_lines || w.lowest > w.highest {
            return empty;
        }

        let aggr = w.aggr_time.max(1);
        let start_bucket = (w.earliest / aggr) as i64;
        let end_bucket = (w.latest_vis / aggr) as i64;
        if end_bucket < start_bucket {
            return empty;
        }

        let Some(len) = usize::try_from(end_bucket - start_bucket + 1)
            .ok()
            .filter(|n| *n <= Self::MAX_BBA_BUCKETS)
        else {
            return empty;
        };

        self.bba_bid.clear();
        self.bba_ask.clear();
        self.bba_bid.resize(len, None);
        self.bba_ask.resize(len, None);

        for (price, runs) in depth_history.iter_time_filtered(
            UnixMs::new(w.earliest),
            UnixMs::new(w.latest_vis),
            w.highest,
            w.lowest,
        ) {
            for run in runs {
                let run_start = run.start_time.as_u64().max(w.earliest);
                // `until_time` is exclusive, so step back one ms before bucketing.
                let run_end = run.until_time.as_u64().min(w.latest_vis.saturating_add(1));
                if run_end <= run_start {
                    continue;
                }

                let b0 = ((run_start / aggr) as i64).max(start_bucket);
                let b1 = (((run_end - 1) / aggr) as i64).min(end_bucket);

                for b in b0..=b1 {
                    let i = (b - start_bucket) as usize;
                    if run.is_bid {
                        self.bba_bid[i] = Some(*price);
                    } else if self.bba_ask[i].is_none() {
                        self.bba_ask[i] = Some(*price);
                    }
                }
            }
        }

        let to_points = |series: &[Option<Price>]| -> Vec<BidAskPoint> {
            series
                .iter()
                .enumerate()
                .map(|(i, price)| {
                    let x_bin_rel = (start_bucket + i as i64 - ref_bucket)
                        .clamp(i32::MIN as i64, i32::MAX as i64)
                        as i32;

                    // `None` marks a bucket the book never quoted; the overlay breaks
                    // the path there instead of drawing a line across dead air.
                    let y_world = price.map(|p| {
                        let rel_y_bin =
                            w.y_bin_for_price_texture_aligned(p, base_price, step, y_anchor);
                        RectInstance::y_center_for_bin(rel_y_bin, w)
                    });

                    BidAskPoint { x_bin_rel, y_world }
                })
                .collect()
        };

        BidAskLine {
            bid: to_points(&self.bba_bid),
            ask: to_points(&self.bba_ask),
            newest_bid: self.bba_bid.iter().rev().find_map(|p| *p),
            newest_ask: self.bba_ask.iter().rev().find_map(|p| *p),
        }
    }

    /// Detects persistent, outsized resting liquidity ("walls") over the last ~30
    /// aggregation buckets and emits a thin trail segment per qualifying price level.
    /// Mirrors ACTerminal `bookmap.ts`'s `detectWalls()`: a level qualifies once it's
    /// present for >=60% of the window and its duration-weighted average quantity is
    /// >= `wall_threshold_mult` times the book-wide duration-weighted average.
    #[allow(clippy::too_many_arguments)]
    fn build_wall_rects(
        &mut self,
        w: &ViewWindow,
        depth_history: &HistoricalDepth,
        _base_price: Price,
        _step: PriceStep,
        _y_anchor: Option<Price>,
        _ref_bucket: i64,
        bookmap_cfg: &BookmapConfig,
        wall_prices: &mut Vec<(Price, bool)>,
    ) {
        if !bookmap_cfg.show_walls || w.lowest > w.highest {
            return;
        }

        const PRESENCE_FRAC_MIN: f64 = 0.6;
        const WALL_WINDOW_BUCKETS: u64 = 30;

        let aggr = w.aggr_time.max(1);
        let window_ms = WALL_WINDOW_BUCKETS.saturating_mul(aggr);
        let latest = w.latest_vis;
        let earliest = latest.saturating_sub(window_ms);
        let window_len = (latest - earliest).max(1) as f64;

        struct LevelStat {
            qty_dur_sum: f64,
            dur_sum: f64,
            is_bid: bool,
        }

        let mut book_qty_dur_sum: f64 = 0.0;
        let mut book_dur_sum: f64 = 0.0;
        let mut per_level: Vec<(Price, LevelStat)> = Vec::new();

        for (price, runs) in depth_history.iter_time_filtered(
            UnixMs::new(earliest),
            UnixMs::new(latest),
            w.highest,
            w.lowest,
        ) {
            let mut qty_dur_sum = 0.0;
            let mut dur_sum = 0.0;
            let mut last_is_bid = false;

            for run in runs {
                let run_start = run.start_time.as_u64().max(earliest);
                let run_end = run.until_time.as_u64().min(latest);
                if run_end <= run_start {
                    continue;
                }

                let dur = (run_end - run_start) as f64;
                qty_dur_sum += run.qty.to_f64() * dur;
                dur_sum += dur;
                last_is_bid = run.is_bid;
            }

            if dur_sum <= 0.0 {
                continue;
            }

            book_qty_dur_sum += qty_dur_sum;
            book_dur_sum += dur_sum;

            per_level.push((
                *price,
                LevelStat {
                    qty_dur_sum,
                    dur_sum,
                    is_bid: last_is_bid,
                },
            ));
        }

        if book_dur_sum <= 0.0 {
            return;
        }

        let book_avg_qty = book_qty_dur_sum / book_dur_sum;
        if book_avg_qty <= 0.0 {
            return;
        }

        let threshold = book_avg_qty * bookmap_cfg.wall_threshold_mult.max(0.0) as f64;

        for (price, stat) in per_level {
            let presence_frac = stat.dur_sum / window_len;
            if presence_frac < PRESENCE_FRAC_MIN {
                continue;
            }

            let level_avg_qty = stat.qty_dur_sum / stat.dur_sum;
            if level_avg_qty < threshold {
                continue;
            }

            wall_prices.push((price, stat.is_bid));
        }
    }

    fn build_volume_profile_rects(
        &mut self,
        w: &ViewWindow,
        trades: &TimeSeries<HeatmapDataPoint>,
        base_price: Price,
        step: PriceStep,
        y_anchor: Option<Price>,
        latest_time: u64,
        profile_kind: &ProfileKind,
        palette: &HeatmapPalette,
        rects: &mut Vec<RectInstance>,
    ) {
        if w.volume_profile_max_width <= 0.0 {
            return;
        }

        let min_rel_y_bin = w.y_bin_for_price_texture_aligned(w.lowest, base_price, step, y_anchor);
        let max_rel_y_bin =
            w.y_bin_for_price_texture_aligned(w.highest, base_price, step, y_anchor);
        if max_rel_y_bin < min_rel_y_bin {
            return;
        }

        let len = (max_rel_y_bin - min_rel_y_bin + 1) as usize;

        self.volume_profile_bid_acc.resize(len, Qty::ZERO);
        self.volume_profile_ask_acc.resize(len, Qty::ZERO);
        self.volume_profile_bid_acc[..].fill(Qty::ZERO);
        self.volume_profile_ask_acc[..].fill(Qty::ZERO);

        let mut max_total = Qty::ZERO;

        let latest_profile_time = latest_time.min(w.latest_vis);

        let (earliest_profile_time, latest_profile_time) = match profile_kind {
            ProfileKind::VisibleRange => (w.earliest, w.latest_vis),
            ProfileKind::FixedWindow(datapoints) => {
                let aggr = w.aggr_time.max(1);
                let window_ms = (*datapoints as u64).saturating_mul(aggr);
                (
                    latest_profile_time.saturating_sub(window_ms),
                    latest_profile_time,
                )
            }
        };

        if latest_profile_time < earliest_profile_time {
            return;
        }

        for (_time, dp) in trades
            .datapoints
            .range(UnixMs::new(earliest_profile_time)..=UnixMs::new(latest_profile_time))
        {
            for t in dp.grouped_trades.iter() {
                let rel_y_bin =
                    w.y_bin_for_price_texture_aligned(t.price, base_price, step, y_anchor);
                let idx = rel_y_bin - min_rel_y_bin;
                if idx < 0 || idx >= len as i64 {
                    continue;
                }

                let i = idx as usize;
                if t.is_sell {
                    self.volume_profile_ask_acc[i] += t.qty;
                } else {
                    self.volume_profile_bid_acc[i] += t.qty;
                }

                let total = self.volume_profile_bid_acc[i] + self.volume_profile_ask_acc[i];
                max_total = max_total.max(total);
            }
        }

        if max_total.is_zero() {
            return;
        }

        let max_total_f32 = max_total.to_f32_lossy();
        self.volume_profile_scale_max_qty = Some(max_total);

        for i in 0..len {
            let rel_y_bin = min_rel_y_bin + i as i64;
            let y_world = w.y_center_for_bin(rel_y_bin);

            let buy_qty = self.volume_profile_bid_acc[i];
            let sell_qty = self.volume_profile_ask_acc[i];
            let total = buy_qty + sell_qty;

            if total.is_zero() {
                continue;
            }

            let buy_qty_f32 = buy_qty.to_f32_lossy();
            let sell_qty_f32 = sell_qty.to_f32_lossy();
            let total_f32 = total.to_f32_lossy();

            let total_w = (total_f32 / max_total_f32) * w.volume_profile_max_width;
            if total_w <= 0.0 {
                continue;
            }
            let buy_w = total_w * (buy_qty_f32 / total_f32);
            let sell_w = total_w * (sell_qty_f32 / total_f32);

            let mut x = w.left_edge_world;

            if !sell_qty.is_zero() && sell_w > 0.0 {
                rects.push(RectInstance::volume_profile_split_bar(
                    y_world,
                    sell_w,
                    x,
                    w,
                    palette.sell_rgb,
                ));
                x += sell_w;
            }

            if !buy_qty.is_zero() && buy_w > 0.0 {
                rects.push(RectInstance::volume_profile_split_bar(
                    y_world,
                    buy_w,
                    x,
                    w,
                    palette.buy_rgb,
                ));
            }
        }
    }

    /// Builds trade "bubbles" per ACTerminal `bookmap.ts`'s `attachBubblesPrimitive`:
    /// radius follows `bubble_scale` (log/linear/sqrt) between `bubble_min_px` and
    /// `bubble_max_px` relative to the largest visible trade, trades below
    /// `bubble_min_trade` are dropped, trades at/above `bubble_large_qty` get an
    /// extra "halo" instance behind them, and trades below that threshold are drawn
    /// hollow (stroked) when `bubble_hollow_small` is set.
    fn build_circles(
        &self,
        w: &ViewWindow,
        trades: &TimeSeries<HeatmapDataPoint>,
        base_price: Price,
        step: PriceStep,
        y_anchor: Option<Price>,
        ref_bucket: i64,
        palette: &HeatmapPalette,
        bookmap_cfg: &BookmapConfig,
    ) -> (Vec<CircleInstance>, Vec<LargeTradeLabel>) {
        const HOLLOW_RING_PX: f32 = 1.5;
        const HALO_SCALE: f32 = 1.6;
        const HALO_ALPHA: f32 = 0.22;
        const MAIN_ALPHA: f32 = 0.85;
        const HOLLOW_ALPHA: f32 = 0.9;

        if !bookmap_cfg.show_bubbles {
            return (vec![], vec![]);
        }

        let min_trade = bookmap_cfg.bubble_min_trade.max(0.0) as f64;
        let large_qty = bookmap_cfg.bubble_large_qty.max(0.0) as f64;

        let mut visible: Vec<(i64, &GroupedTrade)> = Vec::new();
        let mut g_max: f64 = 0.0;

        for (bucket_time, dp) in trades
            .datapoints
            .range(UnixMs::new(w.earliest)..=UnixMs::new(w.latest_vis))
        {
            let bucket = (bucket_time.as_u64() / w.aggr_time) as i64;

            for trade in dp.grouped_trades.iter() {
                if trade.price < w.lowest || trade.price > w.highest {
                    continue;
                }

                let q = trade.qty.to_f64();
                if q < min_trade {
                    continue;
                }

                g_max = g_max.max(q);
                visible.push((bucket, trade));
            }
        }

        if visible.is_empty() || g_max <= 0.0 {
            return (vec![], vec![]);
        }

        let r_min = bookmap_cfg.bubble_min_px.max(0.5);
        let r_max = bookmap_cfg.bubble_max_px.max(r_min);
        let hollow_small = bookmap_cfg.bubble_hollow_small;

        let mut out = Vec::with_capacity(visible.len() * 2);
        let mut large_trades = Vec::new();
        for (bucket, trade) in visible {
            let q = trade.qty.to_f64().max(0.0);
            let t = (match bookmap_cfg.bubble_scale {
                BubbleScale::Linear => (q / g_max).clamp(0.0, 1.0),
                BubbleScale::Log => ((1.0 + q).ln() / (1.0 + g_max).ln()).clamp(0.0, 1.0),
                BubbleScale::Sqrt => (q / g_max).clamp(0.0, 1.0).sqrt(),
            }) as f32;
            let radius_px = r_min + t * (r_max - r_min);

            let base_rgb = if trade.is_sell {
                palette.sell_rgb
            } else {
                palette.buy_rgb
            };

            if large_qty > 0.0 && q >= large_qty {
                large_trades.push(LargeTradeLabel {
                    x_bin_rel: (bucket - ref_bucket).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                    y_world: w.y_center_for_price_texture_aligned(
                        trade.price,
                        base_price,
                        step,
                        y_anchor,
                    ),
                    qty: q,
                    is_sell: trade.is_sell,
                });

                let halo_rgb = [
                    base_rgb[0] + (1.0 - base_rgb[0]) * 0.35,
                    base_rgb[1] + (1.0 - base_rgb[1]) * 0.35,
                    base_rgb[2] + (1.0 - base_rgb[2]) * 0.35,
                ];

                out.push(CircleInstance::new(
                    trade.price,
                    bucket,
                    ref_bucket,
                    base_price,
                    step,
                    y_anchor,
                    w,
                    radius_px * HALO_SCALE,
                    0.0,
                    [halo_rgb[0], halo_rgb[1], halo_rgb[2], HALO_ALPHA],
                ));
                out.push(CircleInstance::new(
                    trade.price,
                    bucket,
                    ref_bucket,
                    base_price,
                    step,
                    y_anchor,
                    w,
                    radius_px,
                    0.0,
                    [base_rgb[0], base_rgb[1], base_rgb[2], MAIN_ALPHA],
                ));
            } else {
                let ring_width_px = if hollow_small { HOLLOW_RING_PX } else { 0.0 };
                let alpha = if hollow_small {
                    HOLLOW_ALPHA
                } else {
                    MAIN_ALPHA
                };

                out.push(CircleInstance::new(
                    trade.price,
                    bucket,
                    ref_bucket,
                    base_price,
                    step,
                    y_anchor,
                    w,
                    radius_px,
                    ring_width_px,
                    [base_rgb[0], base_rgb[1], base_rgb[2], alpha],
                ));
            }
        }

        (out, large_trades)
    }

    fn build_depth_profile_rects(
        &mut self,
        w: &ViewWindow,
        latest_depth: impl IntoIterator<Item = (Price, Qty, bool)>,
        base_price: Price,
        step: PriceStep,
        y_anchor: Option<Price>,
        bookmap_cfg: &BookmapConfig,
        depth_denom: f32,
        rects: &mut Vec<RectInstance>,
    ) {
        if w.depth_profile_max_width <= 0.0 {
            return;
        }

        let min_rel_y_bin = w.y_bin_for_price_texture_aligned(w.lowest, base_price, step, y_anchor);
        let max_rel_y_bin =
            w.y_bin_for_price_texture_aligned(w.highest, base_price, step, y_anchor);
        if max_rel_y_bin < min_rel_y_bin {
            return;
        }

        let len = (max_rel_y_bin - min_rel_y_bin + 1) as usize;

        self.depth_profile_bid_acc.resize(len, Qty::ZERO);
        self.depth_profile_ask_acc.resize(len, Qty::ZERO);
        self.depth_profile_bid_acc[..].fill(Qty::ZERO);
        self.depth_profile_ask_acc[..].fill(Qty::ZERO);

        let mut max_qty = Qty::ZERO;

        for (price, qty, is_bid) in latest_depth {
            if price < w.lowest || price > w.highest {
                continue;
            }

            let rel_y_bin = w.y_bin_for_price_texture_aligned(price, base_price, step, y_anchor);
            let idx = rel_y_bin - min_rel_y_bin;
            if idx < 0 || idx >= len as i64 {
                continue;
            }

            let i = idx as usize;
            let v = if is_bid {
                &mut self.depth_profile_bid_acc[i]
            } else {
                &mut self.depth_profile_ask_acc[i]
            };

            *v += qty;
            max_qty = max_qty.max(*v);
        }

        if max_qty.is_zero() {
            return;
        }

        self.depth_profile_scale_max_qty = Some(max_qty);

        for i in 0..len {
            let rel_y_bin = min_rel_y_bin + i as i64;
            let y = w.y_center_for_bin(rel_y_bin);

            for (is_bid, qty) in [
                (true, self.depth_profile_bid_acc[i]),
                (false, self.depth_profile_ask_acc[i]),
            ] {
                if !qty.is_zero() {
                    rects.push(RectInstance::depth_profile_bar(
                        y,
                        qty,
                        max_qty,
                        is_bid,
                        w,
                        bookmap_cfg.heatmap_colormap,
                        bookmap_cfg.heatmap_gamma,
                        depth_denom,
                    ));
                }
            }
        }
    }

    fn build_volume_strip_rects(
        &mut self,
        w: &ViewWindow,
        trades: &TimeSeries<HeatmapDataPoint>,
        ref_bucket: i64,
        palette: &HeatmapPalette,
        rects: &mut Vec<RectInstance>,
    ) {
        const BUCKET_GAP_FRAC: f32 = 0.10;
        const MIN_BAR_W_PX: f32 = 2.0;
        const MAX_COLS_PER_X_BIN: i64 = 4096;
        if w.volume_area_max_height <= 0.0 {
            return;
        }

        // Compute X binning
        let px_per_col = w.cam_scale;
        let px_per_drawn_col = px_per_col * (1.0 - BUCKET_GAP_FRAC);
        let mut cols_per_x_bin = 1i64;
        if px_per_drawn_col.is_finite() && px_per_drawn_col > 0.0 {
            cols_per_x_bin = (MIN_BAR_W_PX / px_per_drawn_col).ceil() as i64;
            cols_per_x_bin = cols_per_x_bin.clamp(1, MAX_COLS_PER_X_BIN);
        }

        let start_bucket = (w.earliest / w.aggr_time) as i64;
        let latest_bucket = (w.latest_vis / w.aggr_time) as i64;

        let min_x_bin = start_bucket.div_euclid(cols_per_x_bin);
        let max_x_bin = latest_bucket.div_euclid(cols_per_x_bin);
        if max_x_bin < min_x_bin {
            return;
        }

        // Accumulate buy/sell volumes into bins
        let bins_len = (max_x_bin - min_x_bin + 1) as usize;
        self.volume_acc.resize(bins_len, (Qty::ZERO, Qty::ZERO));
        self.volume_acc
            .iter_mut()
            .for_each(|e| *e = (Qty::ZERO, Qty::ZERO));
        self.volume_touched.clear();

        for (time, dp) in trades
            .datapoints
            .range(UnixMs::new(w.earliest)..=UnixMs::new(w.latest_vis))
        {
            let bucket = (time.as_u64() / w.aggr_time) as i64;
            let x_bin = bucket.div_euclid(cols_per_x_bin);
            let idx = (x_bin - min_x_bin) as usize;

            if idx >= bins_len {
                continue;
            }

            let (buy, sell) = dp.buy_sell;
            if buy.is_zero() && sell.is_zero() {
                continue;
            }

            let e = &mut self.volume_acc[idx];
            let was_zero = e.0.is_zero() && e.1.is_zero();
            e.0 += buy;
            e.1 += sell;
            if was_zero {
                self.volume_touched.push(idx);
            }
        }

        if self.volume_touched.is_empty() {
            return;
        }

        self.volume_touched.sort_unstable();
        self.volume_touched.dedup();

        // Find max total volume
        let mut max_total = Qty::ZERO;
        for &idx in &self.volume_touched {
            let (buy, sell) = self.volume_acc[idx];
            max_total = max_total.max(buy + sell);
        }
        if max_total.is_zero() {
            return;
        }

        self.volume_strip_scale_max_qty = Some(max_total);

        // Build rectangle instances
        for &idx in &self.volume_touched {
            let (buy, sell) = self.volume_acc[idx];
            let total_qty = buy + sell;
            if total_qty.is_zero() {
                continue;
            }

            let x_bin = min_x_bin + idx as i64;
            let start_bucket = x_bin * cols_per_x_bin;
            let end_bucket_excl = (start_bucket + cols_per_x_bin).min(latest_bucket + 1);
            if end_bucket_excl <= start_bucket {
                continue;
            }

            let x0_bin = (start_bucket - ref_bucket).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            let x1_bin =
                (end_bucket_excl - ref_bucket).clamp(i32::MIN as i64, i32::MAX as i64) as i32;

            // Total volume bar
            let total_bar = RectInstance::volume_total_bar(
                total_qty, max_total, buy, sell, x0_bin, x1_bin, w, palette,
            );
            let total_h = total_bar.size[1];
            let base_rgb = [total_bar.color[0], total_bar.color[1], total_bar.color[2]];
            rects.push(total_bar);

            // Delta overlay (if not tied)
            let diff = buy.abs_diff(sell);
            if !diff.is_zero() {
                rects.push(RectInstance::volume_delta_bar(
                    diff, total_h, max_total, base_rgb, x0_bin, x1_bin, w,
                ));
            }
        }
    }
}
