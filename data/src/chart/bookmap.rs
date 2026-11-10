//! Configuration for the "Bookmap" chart kind: a liquidity heatmap rendered
//! with a continuous intensity/gamma colormap (à la Bookmap/ACTerminal),
//! trade "bubbles" and persistent liquidity "walls" — ported from the
//! ACTerminal TypeScript `BookMapApp` (see `src/bookmap.ts` in the sibling
//! Tauri frontend repo).
//!
//! This is a second, independent chart kind: it does not modify or replace
//! the existing discrete-cell `Heatmap` chart kind.

use serde::{Deserialize, Serialize};

/// Named color ramps used to map a normalized liquidity intensity (0..1) to
/// a color, separately for the bid and ask side. Mirrors the `CMAPS` table
/// in the TS source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum Colormap {
    #[default]
    Bookmap,
    Mono,
    Inferno,
    Viridis,
}

impl Colormap {
    pub const ALL: [Colormap; 4] = [
        Colormap::Bookmap,
        Colormap::Mono,
        Colormap::Inferno,
        Colormap::Viridis,
    ];

    /// Index matching the `cmap_color()` selector in `heatmap_tex.wgsl`.
    pub fn shader_index(self) -> u32 {
        match self {
            Colormap::Bookmap => 0,
            Colormap::Mono => 1,
            Colormap::Inferno => 2,
            Colormap::Viridis => 3,
        }
    }
}

impl std::fmt::Display for Colormap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Colormap::Bookmap => "Bookmap",
            Colormap::Mono => "Mono",
            Colormap::Inferno => "Inferno",
            Colormap::Viridis => "Viridis",
        };
        write!(f, "{s}")
    }
}

/// How a trade's quantity maps to its bubble radius. Mirrors
/// `BookmapConfig.bubble_scale` in the TS source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
pub enum BubbleScale {
    Linear,
    #[default]
    Sqrt,
    Log,
}

impl BubbleScale {
    pub const ALL: [BubbleScale; 3] = [BubbleScale::Linear, BubbleScale::Sqrt, BubbleScale::Log];
}

impl std::fmt::Display for BubbleScale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BubbleScale::Linear => "Linear",
            BubbleScale::Sqrt => "Sqrt",
            BubbleScale::Log => "Log",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct Config {
    /// Overall opacity/strength multiplier applied before gamma correction.
    pub heatmap_intensity: f32,
    /// Gamma-correction exponent applied to the normalized liquidity value;
    /// values < 1 boost mid-tones, values > 1 suppress them.
    pub heatmap_gamma: f32,
    /// Minimum resting order quantity for a price level to be drawn at all.
    pub heatmap_min_qty: f32,
    pub heatmap_colormap: Colormap,

    /// Bubble radius range, in logical pixels.
    pub bubble_min_px: f32,
    pub bubble_max_px: f32,
    pub bubble_scale: BubbleScale,
    /// Trades at/above this quantity are drawn as "large" bubbles with a halo.
    pub bubble_large_qty: f32,
    /// Trades below this quantity are not drawn at all.
    pub bubble_min_trade: f32,
    /// Trades below `bubble_large_qty` are drawn hollow (stroked) instead of
    /// filled with a radial gradient.
    pub bubble_hollow_small: bool,
    pub show_bubbles: bool,

    /// Draws dashed lines + labels at persistent, outsized resting liquidity.
    pub show_walls: bool,
    /// A price level qualifies as a wall once its quantity reaches this
    /// multiple of the book's average level size.
    pub wall_threshold_mult: f32,

    /// Traces the best bid and best ask over time as two thin lines, plus
    /// matching tags on the price scale. Mirrors ACTerminal's `show_bba`
    /// ("Best Bid/Ask lines") toggle.
    pub show_bid_ask_lines: bool,
}

impl Default for Config {
    /// Values are ACTerminal's own `BookmapConfig::default()` (see
    /// `acterminal-tauri/src-tauri/src/dto.rs`) so a fresh bookmap pane here reads the
    /// same as the reference terminal side by side. Note the gamma is below 1, which
    /// boosts mid-tones rather than suppressing them — the opposite of what a
    /// crypto-depth default wants, but what makes a thin CME FX book legible.
    ///
    /// `wall_threshold_mult` is the one deliberate departure: 1.5x the average level
    /// size flags the resting size that actually matters on these books, where
    /// ACTerminal's 2.2x only ever tags the extremes.
    fn default() -> Self {
        Config {
            heatmap_intensity: 1.2,
            heatmap_gamma: 0.65,
            heatmap_min_qty: 0.0,
            heatmap_colormap: Colormap::Bookmap,

            bubble_min_px: 4.0,
            bubble_max_px: 12.0,
            bubble_scale: BubbleScale::Sqrt,
            bubble_large_qty: 10.0,
            bubble_min_trade: 0.0,
            bubble_hollow_small: true,
            show_bubbles: true,

            show_walls: true,
            wall_threshold_mult: 1.5,

            show_bid_ask_lines: true,
        }
    }
}
