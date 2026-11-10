use exchange::unit::{Price, PriceStep};

use crate::widget::chart::heatmap::view::ViewWindow;

pub const CIRCLE_VERTICES: &[[f32; 2]] = &[[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
pub const CIRCLE_INDICES: &[u16] = &[0, 1, 2, 2, 3, 0];

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CircleInstance {
    pub y_world: f32,
    pub x_bin_rel: i32,
    pub x_frac: f32,
    pub radius_px: f32,
    /// 0.0 = filled disc; >0.0 = hollow ring of this stroke width in px
    /// (mirrors ACTerminal `bookmap.ts`'s `bubble_hollow_small`).
    pub ring_width_px: f32,
    pub color: [f32; 4],
}

impl CircleInstance {
    pub const R_MIN_PX: f32 = 1.5;

    /// Builds a bubble instance centered on `trade_price` at the given time bucket.
    /// Radius, color and hollow/fill state are decided by the caller (the Bookmap
    /// trade-bubble builder), so this stays a pure geometry/placement factory.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trade_price: Price,
        bucket: i64,
        ref_bucket: i64,
        base_price: Price,
        step: PriceStep,
        y_anchor: Option<Price>,
        w: &ViewWindow,
        radius_px: f32,
        ring_width_px: f32,
        color: [f32; 4],
    ) -> Self {
        let x_bin_rel = (bucket - ref_bucket).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let y_world = w.y_center_for_price_texture_aligned(trade_price, base_price, step, y_anchor);

        Self {
            y_world,
            x_bin_rel,
            x_frac: 0.0,
            radius_px: radius_px.max(Self::R_MIN_PX),
            ring_width_px,
            color,
        }
    }
}
