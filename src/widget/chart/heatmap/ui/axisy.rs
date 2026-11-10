use super::{AxisInteraction, Message};
use crate::widget::chart::heatmap::{scene::camera::Camera, ui::AXIS_TEXT_SIZE};
use exchange::unit::{MinTicksize, Price, PriceStep};
use iced::{Rectangle, Renderer, Theme, widget::canvas};
use iced_core::mouse;

/// Rough vertical spacing (in screen pixels) between Y-axis labels.
const LABEL_TARGET_PX: f32 = 48.0;
const DRAG_ZOOM_SENS: f32 = 0.005;

pub struct AxisYLabelCanvas<'a> {
    pub cache: &'a iced::widget::canvas::Cache,
    pub plot_bounds: Option<Rectangle>,
    pub is_paused: bool,
    pub camera: &'a Camera,
    pub base_price: Option<Price>,
    pub step: PriceStep,
    pub row_h: f32,
    /// The instrument's fixed display precision (see `TickerInfo::display_precision`),
    /// not something inferred from `step` — grouping the heatmap must not change how
    /// many decimals a price is written with.
    pub label_precision: MinTicksize,

    /// Detected liquidity walls, tagged `WALL` on the scale (ACTerminal's
    /// `updateWallPriceLines`, which parks the label on the axis rather than
    /// burying it under the heatmap).
    pub wall_prices: &'a [(Price, bool)],
    /// Best bid/ask, tagged on the scale in their line colors.
    pub best_bid: Option<Price>,
    pub best_ask: Option<Price>,
}

/// Represents a label to be drawn on the Y axis.
enum LabelKind {
    Tick { y_px: f32, label: String },
    Base { y_px: f32, label: String },
    Cursor { y_px: f32, label: String },
}

impl LabelKind {
    fn clip_range(&self, label_height: f32) -> (f32, f32) {
        match self {
            LabelKind::Tick { y_px, .. }
            | LabelKind::Base { y_px, .. }
            | LabelKind::Cursor { y_px, .. } => {
                (*y_px - 0.5 * label_height, *y_px + 0.5 * label_height)
            }
        }
    }
}

/// A colored badge pinned to a specific price on the scale (walls, best bid/ask).
struct PriceTag {
    y_px: f32,
    label: String,
    bg: iced::Color,
    fg: iced::Color,
}

/// Checks if two ranges overlap.
fn ranges_overlap(a: (f32, f32), b: (f32, f32)) -> bool {
    a.1 >= b.0 && a.0 <= b.1
}

fn div_ceil_i64(value: i64, divisor: i64) -> i64 {
    let q = value.div_euclid(divisor);
    if value.rem_euclid(divisor) == 0 {
        q
    } else {
        q.saturating_add(1)
    }
}

fn major_step_for_range(
    lowest_price: Price,
    highest_price: Price,
    labels_can_fit: i32,
    step: PriceStep,
) -> PriceStep {
    let step_units = step.units.max(1);

    let range_units = highest_price
        .units
        .abs_diff(lowest_price.units)
        .max(step_units as u64);
    let target_labels = labels_can_fit.max(2) as f32;
    let raw_major = (range_units as f32) / target_labels;
    let nice_major = crate::widget::chart::nice_step(raw_major);

    // The major step must always be a multiple of the instrument's tick size so
    // that Y-axis labels land on tick-aligned prices.  Floor the multiple at 1
    // so that zoomed-in views still show a label at every tick — giving the
    // best-bid and best-ask tags enough vertical separation to both be visible.
    let major_multiple = (nice_major / step_units as f32).ceil().max(1.0) as i64;
    PriceStep {
        units: step_units.saturating_mul(major_multiple),
    }
}

impl AxisYLabelCanvas<'_> {
    fn world_to_px_y(&self, y_world: f32, bounds: Rectangle) -> f32 {
        self.camera
            .world_to_screen_y(y_world, bounds.width, bounds.height)
    }

    fn px_to_world_y(&self, y_px: f32, bounds: Rectangle) -> f32 {
        self.camera
            .screen_to_world_y(y_px, bounds.width, bounds.height)
    }

    pub fn width(base_price: Option<Price>, label_precision: MinTicksize) -> Option<iced::Length> {
        let value = base_price?.to_string(label_precision);
        let width = (value.len() as f32 * AXIS_TEXT_SIZE * 0.8).max(72.0);

        Some(iced::Length::Fixed(width.ceil()))
    }

    /// Screen-space y for an absolute price, snapped to the same row grid the tick
    /// labels use so tags line up with what the shader drew.
    fn price_to_px_y(&self, price: Price, base_price: Price, bounds: Rectangle) -> f32 {
        let step_units = self.step.units.max(1);
        let rel_units = price.units.saturating_sub(base_price.units);
        let s = (rel_units as f64 / step_units as f64).round() as i64;

        let y_world = super::world_y_for_step_center(s, self.row_h);
        self.world_to_px_y(y_world, bounds)
    }

    /// Wall + best-bid/ask badges. Best bid/ask are offered first so that when the
    /// scale is too zoomed out to separate them (they sit a tick apart, and walls
    /// often sit right on top of them) the live quote wins over a wall marker
    /// rather than the badges stacking into an unreadable pile.
    ///
    /// When bid and ask are so close that their natural pixel positions overlap,
    /// the ask tag is nudged upward (toward the offer side) so both remain
    /// visible — mirroring how bookmap-style terminals keep the two live quotes
    /// readable at any zoom level.
    fn price_tags(
        &self,
        base_price: Price,
        bounds: Rectangle,
        palette: &iced::theme::palette::Extended,
        tag_height: f32,
    ) -> Vec<PriceTag> {
        let mut tags: Vec<PriceTag> = Vec::new();

        let push = |tags: &mut Vec<PriceTag>,
                    price: Price,
                    label: String,
                    pair: iced::theme::palette::Pair| {
            let y_px = self.price_to_px_y(price, base_price, bounds);
            if !(0.0..=bounds.height).contains(&y_px) {
                return;
            }

            let clip = (y_px - 0.5 * tag_height, y_px + 0.5 * tag_height);
            if tags.iter().any(|t: &PriceTag| {
                ranges_overlap(clip, (t.y_px - 0.5 * tag_height, t.y_px + 0.5 * tag_height))
            }) {
                return;
            }

            tags.push(PriceTag {
                y_px,
                label,
                bg: iced::Color {
                    a: 1.0,
                    ..pair.color
                },
                fg: pair.text,
            });
        };

        if let Some(bid) = self.best_bid {
            push(
                &mut tags,
                bid,
                bid.to_string(self.label_precision),
                palette.success.strong,
            );
        }
        if let Some(ask) = self.best_ask {
            push(
                &mut tags,
                ask,
                ask.to_string(self.label_precision),
                palette.danger.strong,
            );
        }

        // When best bid and best ask are only one tick apart their tags will
        // overlap at most zoom levels.  Nudge the ask tag upward (toward the
        // offer side) by exactly one tag height so both remain readable.
        // This is done before pushing wall tags so walls are not affected.
        if tags.len() == 2 {
            let dy = tags[1].y_px - tags[0].y_px;
            if dy.abs() < tag_height {
                let nudge = (tag_height - dy.abs()) * 0.5 + 1.0;
                // Ask is tags[1]; nudge it upward (negative y = up on screen)
                // when ask sits below bid, or downward when above.
                if dy >= 0.0 {
                    tags[1].y_px += nudge;
                } else {
                    tags[1].y_px -= nudge;
                }
                // A collapsed pane can report a height smaller than the tag
                // (`clamp` panics on min > max).
                let lo = 0.5 * tag_height;
                let hi = bounds.height - 0.5 * tag_height;
                if hi > lo && tags[1].y_px.is_finite() {
                    tags[1].y_px = tags[1].y_px.clamp(lo, hi);
                }
            }
        }

        for (price, is_bid) in self.wall_prices {
            let pair = if *is_bid {
                palette.success.weak
            } else {
                palette.danger.weak
            };
            push(
                &mut tags,
                *price,
                price.to_string(self.label_precision),
                pair,
            );
        }

        tags
    }
}

impl canvas::Program<Message> for AxisYLabelCanvas<'_> {
    type State = super::AxisState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_over(bounds)?;

                // Double-click detection needs global cursor position + previous click state.
                if let Some(global_pos) = cursor.position() {
                    let new_click =
                        mouse::Click::new(global_pos, mouse::Button::Left, state.previous_click);

                    let is_double = new_click.kind() == mouse::click::Kind::Double;

                    state.previous_click = Some(new_click);

                    if is_double {
                        state.interaction = AxisInteraction::None;
                        return Some(canvas::Action::publish(Message::AxisYDoubleClicked));
                    }
                } else {
                    state.previous_click = None;
                }

                state.interaction = AxisInteraction::Panning {
                    last_position: p,
                    zoom_anchor: None,
                };
                None
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.interaction = AxisInteraction::None;
                None
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let AxisInteraction::Panning { last_position, .. } = &mut state.interaction {
                    let delta_px = *position - *last_position;
                    *last_position = *position;

                    let factor = (-delta_px.y * DRAG_ZOOM_SENS).exp().clamp(0.01, 100.0);

                    Some(canvas::Action::publish(Message::ScrolledAxisY {
                        factor,
                        cursor_y: bounds.height * 0.5, // anchor at viewport center
                        viewport_h: bounds.height,
                    }))
                } else {
                    None
                }
            }
            iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let p = cursor.position_in(bounds)?;
                let scroll_amount = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y * 0.1,
                    mouse::ScrollDelta::Pixels { y, .. } => *y * 0.01,
                };

                let factor = (1.0 + scroll_amount).clamp(0.01, 100.0);

                Some(canvas::Action::publish(Message::ScrolledAxisY {
                    factor,
                    cursor_y: p.y,
                    viewport_h: bounds.height,
                }))
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
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let cam_sy = self.camera.scale();
        if !self.row_h.is_finite() || self.row_h <= 0.0 || !cam_sy.is_finite() || cam_sy <= 0.0 {
            return vec![];
        }

        let Some(base_price) = self.base_price else {
            return vec![];
        };

        let palette = theme.extended_palette();

        let tick_labels = self.cache.draw(renderer, bounds.size(), |frame| {
            let row_h = self.row_h;

            let y_world_top = self.px_to_world_y(0.0, bounds);
            let y_world_bottom = self.px_to_world_y(bounds.height, bounds);

            let step = PriceStep {
                units: self.step.units.max(1),
            };

            let step_at_top = super::step_center_pos_from_world_y(y_world_top, row_h);
            let step_at_bottom = super::step_center_pos_from_world_y(y_world_bottom, row_h);

            let lowest_step = step_at_top.min(step_at_bottom).floor() as i64;
            let highest_step = step_at_top.max(step_at_bottom).ceil() as i64;

            let lowest_price = base_price.add_steps(lowest_step, step);
            let highest_price = base_price.add_steps(highest_step, step);

            let labels_can_fit = (bounds.height / LABEL_TARGET_PX).floor() as i32;
            let major_step =
                major_step_for_range(lowest_price, highest_price, labels_can_fit, step);
            let major_step_units = major_step.units.max(step.units);

            let x = bounds.width / 2.0;
            let cursor_label_padding = 6.0f32;
            let cursor_label_height = AXIS_TEXT_SIZE + 2.0 * cursor_label_padding;
            let size_cursor_label = iced::Size {
                width: bounds.width,
                height: cursor_label_height,
            };

            // --- Compute label positions and values ---
            let mut labels: Vec<LabelKind> = Vec::new();

            let suppress_cursor_label =
                super::paused_control_hovered(self.is_paused, self.plot_bounds, cursor);

            // Cursor label (highest priority)
            let cursor_label_snapped = if suppress_cursor_label {
                None
            } else {
                self.plot_bounds
                    .and_then(|pb| cursor.position_in(pb))
                    .map(|p| {
                        let y_world_cursor = self.px_to_world_y(p.y, bounds);
                        let step_at_cursor =
                            super::step_center_pos_from_world_y(y_world_cursor, row_h).round()
                                as i64;
                        let y_world_for_step =
                            super::world_y_for_step_center(step_at_cursor, row_h);
                        let y_px = self.world_to_px_y(y_world_for_step, bounds);

                        let price_at_cursor = base_price.add_steps(step_at_cursor, step);
                        let label_at_cursor = price_at_cursor.to_string(self.label_precision);

                        LabelKind::Cursor {
                            y_px,
                            label: label_at_cursor,
                        }
                    })
            };

            // Base price label (secondary priority)
            let base_step: i64 = 0;
            let y_world_base = super::world_y_for_step_center(base_step, row_h);
            let y_px_base = self.world_to_px_y(y_world_base, bounds);

            let price_base = base_price.add_steps(base_step, step);
            let label_base = price_base.to_string(self.label_precision);

            let base_label = LabelKind::Base {
                y_px: y_px_base,
                label: label_base,
            };

            // Tick labels (absolute price-domain majors, then projected to row grid)
            let mut tick_units =
                div_ceil_i64(lowest_price.units, major_step_units).saturating_mul(major_step_units);
            let mut safety_counter = 0usize;
            let tick_limit = highest_price.units.saturating_add(major_step_units / 2);

            while tick_units <= tick_limit && safety_counter < 2_048 {
                let rel_units = tick_units.saturating_sub(base_price.units);

                if rel_units % step.units == 0 {
                    let s = rel_units / step.units;
                    let y_world = super::world_y_for_step_center(s, row_h);
                    let y_px = self.world_to_px_y(y_world, bounds);

                    if (0.0..=bounds.height).contains(&y_px) {
                        let price = Price::from_units(tick_units);
                        let label = price.to_string(self.label_precision);
                        labels.push(LabelKind::Tick { y_px, label });
                    }
                }

                let next_tick = tick_units.saturating_add(major_step_units);
                if next_tick == tick_units {
                    break;
                }
                tick_units = next_tick;
                safety_counter += 1;
            }

            // --- Render labels with overlap filtering ---
            let text_color = palette.background.base.text;

            // Compute clip ranges for cursor and base labels
            let cursor_clip_range = cursor_label_snapped
                .as_ref()
                .map(|l| l.clip_range(size_cursor_label.height));
            let base_clip_range = base_label.clip_range(size_cursor_label.height);

            // Wall / best-bid-ask badges: drawn last (on top) but reserved here so
            // plain tick labels never collide with them.
            let tags = self.price_tags(base_price, bounds, palette, cursor_label_height);
            let tag_clip_ranges: Vec<(f32, f32)> = tags
                .iter()
                .map(|t| {
                    (
                        t.y_px - 0.5 * cursor_label_height,
                        t.y_px + 0.5 * cursor_label_height,
                    )
                })
                .collect();

            // Draw tick labels, skipping overlaps
            for label in &labels {
                let tick_clip = label.clip_range(AXIS_TEXT_SIZE * 0.7 * 2.0);
                // Skip if overlaps cursor or base label
                if cursor_clip_range.is_some_and(|c| ranges_overlap(tick_clip, c)) {
                    continue;
                }
                if ranges_overlap(tick_clip, base_clip_range) {
                    continue;
                }
                if tag_clip_ranges
                    .iter()
                    .any(|t| ranges_overlap(tick_clip, *t))
                {
                    continue;
                }

                if let LabelKind::Tick { y_px, label } = label {
                    frame.fill_text(canvas::Text {
                        content: label.clone(),
                        position: iced::Point::new(x, *y_px),
                        color: text_color,
                        size: AXIS_TEXT_SIZE.into(),
                        font: crate::style::AZERET_MONO,
                        align_x: iced::Alignment::Center.into(),
                        align_y: iced::Alignment::Center.into(),
                        ..Default::default()
                    });
                }
            }

            // Draw base label if not overlapped by the cursor or a price tag. The
            // mid price sits between best bid and best ask, so those tags almost
            // always cover it — they win, being the more specific readout.
            let base_y_in_view = (0.0..=bounds.height).contains(&y_px_base);
            let overlaps_cursor = cursor_clip_range
                .map(|c| ranges_overlap(base_clip_range, c))
                .unwrap_or(false);
            let overlaps_tag = tag_clip_ranges
                .iter()
                .any(|t| ranges_overlap(base_clip_range, *t));
            if base_y_in_view && !overlaps_cursor && !overlaps_tag {
                let mut bg = palette.secondary.strong.color;
                bg = iced::Color { a: 1.0, ..bg };

                frame.fill_rectangle(
                    iced::Point::new(0.0, y_px_base - 0.5 * size_cursor_label.height),
                    size_cursor_label,
                    bg,
                );

                if let LabelKind::Base { label, .. } = &base_label {
                    frame.fill_text(canvas::Text {
                        content: label.clone(),
                        position: iced::Point::new(x, y_px_base),
                        color: palette.primary.strong.text,
                        size: AXIS_TEXT_SIZE.into(),
                        font: crate::style::AZERET_MONO,
                        align_x: iced::Alignment::Center.into(),
                        align_y: iced::Alignment::Center.into(),
                        ..Default::default()
                    });
                }
            }

            // Draw wall / best-bid-ask badges, yielding to the cursor label
            for tag in &tags {
                let tag_clip = (
                    tag.y_px - 0.5 * cursor_label_height,
                    tag.y_px + 0.5 * cursor_label_height,
                );
                if cursor_clip_range.is_some_and(|c| ranges_overlap(tag_clip, c)) {
                    continue;
                }

                frame.fill_rectangle(
                    iced::Point::new(0.0, tag.y_px - 0.5 * size_cursor_label.height),
                    size_cursor_label,
                    tag.bg,
                );

                frame.fill_text(canvas::Text {
                    content: tag.label.clone(),
                    position: iced::Point::new(x, tag.y_px),
                    color: tag.fg,
                    size: AXIS_TEXT_SIZE.into(),
                    font: crate::style::AZERET_MONO,
                    align_x: iced::Alignment::Center.into(),
                    align_y: iced::Alignment::Center.into(),
                    ..Default::default()
                });
            }

            // Draw cursor label (highest priority)
            if let Some(LabelKind::Cursor { y_px, label }) = cursor_label_snapped {
                let mut bg = palette.secondary.base.color;
                bg = iced::Color { a: 1.0, ..bg };

                frame.fill_rectangle(
                    iced::Point::new(0.0, y_px - 0.5 * size_cursor_label.height),
                    size_cursor_label,
                    bg,
                );

                frame.fill_text(canvas::Text {
                    content: label,
                    position: iced::Point::new(x, y_px),
                    color: palette.secondary.base.text,
                    size: AXIS_TEXT_SIZE.into(),
                    font: crate::style::AZERET_MONO,
                    align_x: iced::Alignment::Center.into(),
                    align_y: iced::Alignment::Center.into(),
                    ..Default::default()
                });
            }
        });

        vec![tick_labels]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.position_over(bounds).is_some() {
            match state.interaction {
                AxisInteraction::Panning { .. } => mouse::Interaction::Grabbing,
                _ => mouse::Interaction::ResizingVertically,
            }
        } else {
            mouse::Interaction::default()
        }
    }
}
