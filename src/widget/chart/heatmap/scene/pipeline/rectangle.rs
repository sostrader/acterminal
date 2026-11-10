use crate::widget::chart::heatmap::scene::depth_grid::HeatmapPalette;
use crate::widget::chart::heatmap::view::ViewWindow;
use bytemuck::{Pod, Zeroable};
use exchange::unit::Qty;

pub const RECT_VERTICES: &[[f32; 2]] = &[[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
pub const RECT_INDICES: &[u16] = &[0, 1, 2, 2, 3, 0];

pub const MIN_BAR_PX: f32 = 1.0;

/// How the rect derives its horizontal extent. Must stay in sync with `rect.wgsl`.
pub mod x_mode {
    /// Positioned/sized directly in world space via `position`/`size`.
    pub const WORLD: u32 = 0;
    /// Spans `[x0_bin, x1_bin_excl)`, inset by the volume-strip gap and shifted half
    /// a bucket so discrete bars line up with the trade circles.
    pub const BINS_GAPPED: u32 = 1;
    /// Spans `[x0_bin, x1_bin_excl)` exactly, with no gap or shift, so consecutive
    /// segments butt up against each other and sit on the heatmap's column grid.
    /// Used by the continuous overlays (walls, best bid/ask lines).
    pub const BINS_CONTIGUOUS: u32 = 2;
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct RectInstance {
    pub position: [f32; 2],
    pub size: [f32; 2],
    pub color: [f32; 4],
    pub x0_bin: i32,
    pub x1_bin_excl: i32,
    /// One of [`x_mode`].
    pub x_mode: u32,
    pub fade_mode: u32, // 0=fade, 1=skip
    pub subpx_alpha: f32,
}

impl RectInstance {
    const DEPTH_PROFILE_ALPHA: f32 = 1.0;

    const VOLUME_TOTAL_ALPHA: f32 = 0.65;
    const VOLUME_DELTA_ALPHA: f32 = 1.0;
    const VOLUME_DELTA_TINT_TO_WHITE: f32 = 0.12;

    const VOLUME_PROFILE_ALPHA: f32 = 1.0;

    const WALL_LINE_PX: f32 = 1.5;
    const WALL_ALPHA: f32 = 0.55;

    fn extent_and_subpx_alpha(raw_world: f32, cam_scale: f32) -> (f32, f32) {
        let raw_world = raw_world.max(0.0);
        if raw_world <= 0.0 {
            return (0.0, 0.0);
        }

        let sx = cam_scale.max(1e-6);
        let raw_px = raw_world * sx;

        if raw_px >= MIN_BAR_PX {
            return (raw_world, 1.0);
        }

        let draw_world = MIN_BAR_PX / sx;
        let subpx_alpha = (raw_px / MIN_BAR_PX).clamp(0.0, 1.0);
        (draw_world, subpx_alpha)
    }

    /// `colormap`/`gamma` come from the bookmap style so the profile is tinted off the
    /// same ramp the heatmap behind it is painted with — it reads as part of the plot
    /// rather than an overlay pasted on top. The volume profile deliberately keeps the
    /// theme's own colours, so the two stay tellable apart.
    /// Bar length is normalized against the profile's own largest level, so the profile
    /// stays readable whatever the book's absolute size. Its *colour* is normalized
    /// against `depth_denom` — the very scale `heatmap_tex.wgsl` shades the plot with,
    /// already divided by the bookmap's `heatmap_intensity` — so a level of a given size
    /// is the same shade in the profile as in the heatmap row beside it, and both react
    /// to the intensity and gamma sliders together.
    #[allow(clippy::too_many_arguments)]
    pub fn depth_profile_bar(
        y_world: f32,
        qty: Qty,
        max_qty: Qty,
        is_bid: bool,
        w: &ViewWindow,
        colormap: data::chart::bookmap::Colormap,
        gamma: f32,
        depth_denom: f32,
    ) -> Self {
        let t = (qty.to_f64() / max_qty.to_scale_or_one()).clamp(0.0, 1.0) as f32;
        let raw_w_world = t * w.depth_profile_max_width;
        let (w_world, subpx_alpha) = Self::extent_and_subpx_alpha(raw_w_world, w.cam_scale);
        let center_x = 0.5 * w_world;

        let denom = if depth_denom.is_finite() && depth_denom > 0.0 {
            depth_denom
        } else {
            max_qty.to_scale_or_one() as f32
        };
        let shade_t = ((qty.to_f64() as f32) / denom).clamp(0.0, 1.0);
        let shaded = shade_t.powf(gamma.max(0.01));
        let rgb = crate::widget::chart::heatmap::colormap::sample(colormap, is_bid, shaded);

        Self {
            position: [center_x, y_world],
            size: [w_world, w.y_bin_h_world],
            color: [rgb[0], rgb[1], rgb[2], Self::DEPTH_PROFILE_ALPHA],
            x0_bin: 0,
            x1_bin_excl: 0,
            x_mode: x_mode::WORLD,
            fade_mode: 0,
            subpx_alpha,
        }
    }

    pub fn volume_total_bar(
        total_qty: Qty,
        max_qty: Qty,
        buy_qty: Qty,
        sell_qty: Qty,
        x0_bin: i32,
        x1_bin_excl: i32,
        w: &ViewWindow,
        palette: &HeatmapPalette,
    ) -> Self {
        let denom = max_qty.to_scale_or_one();

        let (base_rgb, _is_tie) = if buy_qty > sell_qty {
            (palette.buy_rgb, false)
        } else if sell_qty > buy_qty {
            (palette.sell_rgb, false)
        } else {
            (palette.secondary_rgb, true)
        };

        let raw_total_h = ((total_qty.to_f64() / denom) * w.volume_area_max_height as f64) as f32;
        let (total_h, subpx_alpha) = Self::extent_and_subpx_alpha(raw_total_h, w.cam_scale);
        let total_center_y = w.volume_area_bottom_y - 0.5 * total_h;

        Self {
            position: [0.0, total_center_y],
            size: [0.0, total_h],
            color: [
                base_rgb[0],
                base_rgb[1],
                base_rgb[2],
                Self::VOLUME_TOTAL_ALPHA,
            ],
            x0_bin,
            x1_bin_excl,
            x_mode: x_mode::BINS_GAPPED,
            fade_mode: 0,
            subpx_alpha,
        }
    }

    pub fn volume_delta_bar(
        diff_qty: Qty,
        total_h: f32,
        max_qty: Qty,
        base_rgb: [f32; 3],
        x0_bin: i32,
        x1_bin_excl: i32,
        w: &ViewWindow,
    ) -> Self {
        let denom = max_qty.to_scale_or_one();
        let raw_overlay_h = ((diff_qty.to_f64() / denom) * w.volume_area_max_height as f64)
            .min(total_h.max(0.0_f32) as f64) as f32;
        let (overlay_h, subpx_alpha) = Self::extent_and_subpx_alpha(raw_overlay_h, w.cam_scale);
        let overlay_h = overlay_h.min(total_h.max(0.0));

        let t = Self::VOLUME_DELTA_TINT_TO_WHITE;
        let overlay_rgb = [
            base_rgb[0] + (1.0 - base_rgb[0]) * t,
            base_rgb[1] + (1.0 - base_rgb[1]) * t,
            base_rgb[2] + (1.0 - base_rgb[2]) * t,
        ];

        let overlay_center_y = w.volume_area_bottom_y - 0.5 * overlay_h;

        Self {
            position: [0.0, overlay_center_y],
            size: [0.0, overlay_h],
            color: [
                overlay_rgb[0],
                overlay_rgb[1],
                overlay_rgb[2],
                Self::VOLUME_DELTA_ALPHA,
            ],
            x0_bin,
            x1_bin_excl,
            x_mode: x_mode::BINS_GAPPED,
            fade_mode: 0,
            subpx_alpha,
        }
    }

    pub fn volume_profile_split_bar(
        y_world: f32,
        width_world: f32,
        left_edge_world: f32,
        w: &ViewWindow,
        rgb: [f32; 3],
    ) -> Self {
        let raw_width_world = width_world.max(0.0);
        let (width_world, subpx_alpha) = Self::extent_and_subpx_alpha(raw_width_world, w.cam_scale);
        let center_x = left_edge_world + 0.5 * width_world;

        Self {
            position: [center_x, y_world],
            size: [width_world, w.y_bin_h_world],
            color: [rgb[0], rgb[1], rgb[2], Self::VOLUME_PROFILE_ALPHA],
            x0_bin: 0,
            x1_bin_excl: 0,
            x_mode: x_mode::WORLD,
            fade_mode: 1,
            subpx_alpha,
        }
    }

    /// A thin horizontal "liquidity wall" trail segment at a given price row,
    /// spanning `[x0_bin, x1_bin_excl)` columns. Mirrors the ACTerminal
    /// `bookmap.ts` wall-trail rendering (persistent, outsized resting liquidity).
    pub fn wall_bar(
        y_world: f32,
        x0_bin: i32,
        x1_bin_excl: i32,
        is_bid: bool,
        w: &ViewWindow,
        palette: &HeatmapPalette,
    ) -> Self {
        let raw_h = Self::WALL_LINE_PX / w.cam_scale.max(1e-6);
        let (h, subpx_alpha) = Self::extent_and_subpx_alpha(raw_h, w.cam_scale);

        let rgb = if is_bid {
            palette.bid_rgb
        } else {
            palette.ask_rgb
        };

        Self {
            position: [0.0, y_world],
            size: [0.0, h],
            color: [rgb[0], rgb[1], rgb[2], Self::WALL_ALPHA],
            x0_bin,
            x1_bin_excl,
            x_mode: x_mode::BINS_CONTIGUOUS,
            fade_mode: 0,
            subpx_alpha,
        }
    }

    #[inline]
    pub fn y_center_for_bin(y_bin: i64, w: &ViewWindow) -> f32 {
        -((y_bin as f32 + 0.5) * w.y_bin_h_world)
    }
}
