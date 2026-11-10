//! CPU-side twin of `heatmap_tex.wgsl`'s `cmap_color`.
//!
//! The liquidity heatmap is coloured in the fragment shader, but the DOM depth profile
//! that sits in front of it is a plain rectangle instance with a flat RGBA. Colouring it
//! from the theme's bid/ask swatches made it read as a separate object pasted over the
//! plot; sampling the same ramp here makes it read as part of it.
//!
//! Stops are the same values as the shader, which is the same table as ACTerminal
//! `bookmap.ts`'s `CMAPS`. The two must stay in step — the test at the bottom pins the
//! endpoints, but a stop edited in one place still needs editing in the other.

use data::chart::bookmap::Colormap;

type Rgb = [f32; 3];

const fn rgb(r: f32, g: f32, b: f32) -> Rgb {
    [r / 255.0, g / 255.0, b / 255.0]
}

const BOOKMAP_BID: [Rgb; 5] = [
    rgb(6.0, 10.0, 16.0),
    rgb(6.0, 55.0, 65.0),
    rgb(20.0, 140.0, 135.0),
    rgb(125.0, 225.0, 210.0),
    rgb(235.0, 255.0, 250.0),
];

const BOOKMAP_ASK: [Rgb; 5] = [
    rgb(6.0, 10.0, 16.0),
    rgb(60.0, 18.0, 18.0),
    rgb(170.0, 40.0, 40.0),
    rgb(245.0, 140.0, 90.0),
    rgb(255.0, 235.0, 200.0),
];

const MONO: [Rgb; 5] = [
    rgb(8.0, 10.0, 14.0),
    rgb(55.0, 60.0, 72.0),
    rgb(135.0, 145.0, 160.0),
    rgb(220.0, 225.0, 235.0),
    rgb(255.0, 255.0, 255.0),
];

const INFERNO: [Rgb; 6] = [
    rgb(0.0, 0.0, 4.0),
    rgb(40.0, 11.0, 84.0),
    rgb(120.0, 28.0, 109.0),
    rgb(200.0, 55.0, 85.0),
    rgb(252.0, 135.0, 40.0),
    rgb(252.0, 255.0, 164.0),
];

const VIRIDIS: [Rgb; 5] = [
    rgb(68.0, 1.0, 84.0),
    rgb(59.0, 82.0, 139.0),
    rgb(33.0, 144.0, 141.0),
    rgb(93.0, 201.0, 99.0),
    rgb(253.0, 231.0, 37.0),
];

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn ramp(stops: &[Rgb], t: f32) -> Rgb {
    debug_assert!(stops.len() >= 2);

    let segments = (stops.len() - 1) as f32;
    let scaled = t.clamp(0.0, 1.0) * segments;
    let i = (scaled.floor()).clamp(0.0, segments - 1.0);
    let frac = scaled - i;

    let i = i as usize;
    mix(stops[i], stops[i + 1], frac)
}

/// Colour for a normalized liquidity intensity `t`, matching the shader exactly.
pub fn sample(colormap: Colormap, is_bid: bool, t: f32) -> Rgb {
    match colormap {
        Colormap::Bookmap => {
            if is_bid {
                ramp(&BOOKMAP_BID, t)
            } else {
                ramp(&BOOKMAP_ASK, t)
            }
        }
        Colormap::Mono => ramp(&MONO, t),
        Colormap::Inferno => ramp(&INFERNO, t),
        Colormap::Viridis => ramp(&VIRIDIS, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_hits_its_endpoints_exactly() {
        let low = sample(Colormap::Bookmap, true, 0.0);
        assert_eq!(low, BOOKMAP_BID[0]);

        let high = sample(Colormap::Bookmap, true, 1.0);
        assert_eq!(high, BOOKMAP_BID[4]);

        // Out-of-range input clamps rather than extrapolating off the ramp.
        assert_eq!(sample(Colormap::Bookmap, true, -1.0), BOOKMAP_BID[0]);
        assert_eq!(sample(Colormap::Bookmap, true, 5.0), BOOKMAP_BID[4]);
    }

    #[test]
    fn bid_and_ask_diverge_only_on_the_bookmap_ramp() {
        // The bookmap ramp is two-sided (teal vs red); the others are single ramps used
        // for both sides, exactly as the shader does it.
        assert_ne!(
            sample(Colormap::Bookmap, true, 0.8),
            sample(Colormap::Bookmap, false, 0.8)
        );
        assert_eq!(
            sample(Colormap::Viridis, true, 0.8),
            sample(Colormap::Viridis, false, 0.8)
        );
    }

    #[test]
    fn interior_stops_land_on_segment_boundaries() {
        // 5 stops => 4 segments, so t = 0.25 is exactly stop 1.
        assert_eq!(sample(Colormap::Bookmap, true, 0.25), BOOKMAP_BID[1]);
        // 6 stops => 5 segments, so t = 0.4 is exactly stop 2.
        assert_eq!(sample(Colormap::Inferno, true, 0.4), INFERNO[2]);
    }
}
