pub mod comparison;
pub mod heatmap;

use chrono::{TimeZone, Utc};
use data::chart::TimeAxis;
use exchange::TickerInfo;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Zoom(pub usize);

impl Zoom {
    /// "show all data"
    pub fn all() -> Self {
        Self(0)
    }
    pub fn points(n: usize) -> Self {
        Self(n)
    }
    pub fn is_all(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone)]
pub struct Series {
    pub ticker_info: TickerInfo,
    pub name: Option<String>,
    pub points: Vec<(u64, f32)>,
    pub color: iced::Color,
    pub inverted: bool,
}

impl Series {
    pub fn new(ticker_info: TickerInfo, color: iced::Color, name: Option<String>) -> Self {
        Self {
            ticker_info,
            name,
            points: Vec::new(),
            color,
            inverted: false,
        }
    }

    pub fn with_inverted(mut self, inverted: bool) -> Self {
        self.inverted = inverted;
        self
    }

    /// Swap the base/quote halves of a forex pair for display, e.g. "EURUSD" -> "USDEUR".
    /// Returns None when the symbol is not a symmetric pair.
    pub fn inverted_pair_display(symbol: &str) -> Option<String> {
        if symbol.len().is_multiple_of(2) && !symbol.is_empty() {
            let mid = symbol.len() / 2;
            let (base, quote) = symbol.split_at(mid);
            Some(format!("{quote}{base}"))
        } else {
            None
        }
    }
}

pub trait SeriesLike {
    fn name(&self) -> String;
    fn points(&self) -> &[(u64, f32)];
    fn color(&self) -> iced::Color;
    fn ticker_info(&self) -> &TickerInfo;
}

impl SeriesLike for Series {
    fn name(&self) -> String {
        if let Some(name) = &self.name {
            name.clone()
        } else if self.inverted {
            let sym = self.ticker_info.ticker.display_symbol().unwrap_or("");
            Self::inverted_pair_display(sym).unwrap_or_else(|| self.ticker_info.ticker.to_string())
        } else {
            self.ticker_info.ticker.to_string()
        }
    }

    fn points(&self) -> &[(u64, f32)] {
        &self.points
    }

    fn color(&self) -> iced::Color {
        self.color
    }

    fn ticker_info(&self) -> &TickerInfo {
        &self.ticker_info
    }
}

/// Compute a "nice" step close to range/target using 1/2/5*10^k
fn nice_step_multiplier_125(v: f32) -> f32 {
    if v <= 1.0 {
        1.0
    } else if v <= 2.0 {
        2.0
    } else if v <= 5.0 {
        5.0
    } else {
        10.0
    }
}

fn nice_step(rough: f32) -> f32 {
    if !rough.is_finite() || rough <= 0.0 {
        return 1.0;
    }

    let base = 10.0f32.powf(rough.log10().floor());
    let fraction = rough / base;

    nice_step_multiplier_125(fraction) * base
}

fn ticks(min: f32, max: f32, target: usize) -> (Vec<f32>, f32) {
    let span = (max - min).abs().max(1e-6);

    let step = {
        let target = target.max(2) as f32;
        let raw = (span / target).max(f32::EPSILON);
        nice_step(raw)
    };

    let start = (min / step).floor() * step;
    let end = (max / step).ceil() * step;

    let mut v = Vec::new();
    let mut t = start;
    for _ in 0..100 {
        if t > end + step * 0.5 {
            break;
        }
        v.push(t);
        t += step;
    }
    (v, step)
}

fn format_pct(val: f32, step: f32, show_decimals: bool) -> String {
    if show_decimals {
        if step >= 1.0 {
            format!("{:+.1}%", val)
        } else if step >= 0.1 {
            format!("{:+.2}%", val)
        } else {
            format!("{:+.3}%", val)
        }
    } else if step >= 1.0 {
        format!("{:+.0}%", val)
    } else if step >= 0.1 {
        format!("{:+.1}%", val)
    } else {
        format!("{:+.2}%", val)
    }
}

/// Same precision ladder as `format_pct`, minus the `%`/sign — one more decimal digit
/// once `show_decimals` is set, keyed off the axis tick step rather than a fixed count,
/// so a tightly-clustered price range (e.g. two similar futures) doesn't round every
/// label down to the same value.
fn format_price(val: f32, step: f32, show_decimals: bool) -> String {
    let decimals: usize = if step >= 1.0 {
        if show_decimals { 1 } else { 0 }
    } else if step >= 0.1 {
        if show_decimals { 2 } else { 1 }
    } else if step >= 0.01 {
        if show_decimals { 3 } else { 2 }
    } else if show_decimals {
        4
    } else {
        3
    };
    format!("{val:.decimals$}")
}

fn time_tick_candidates() -> &'static [u64] {
    const S: u64 = 1_000;
    const M: u64 = 60 * S;
    const H: u64 = 60 * M;
    const D: u64 = 24 * H;
    &[
        S,
        2 * S,
        5 * S,
        10 * S,
        15 * S,
        30 * S, //
        M,
        2 * M,
        5 * M,
        10 * M,
        15 * M,
        30 * M, //
        H,
        2 * H,
        4 * H,
        6 * H,
        12 * H, //
        D,
        2 * D,
        7 * D,
        14 * D, //
        30 * D,
        90 * D,
        180 * D,
        365 * D,
    ]
}

fn format_time_label(ts_ms: u64, step_ms: u64) -> String {
    let Some(dt) = Utc.timestamp_millis_opt(ts_ms as i64).single() else {
        return String::new();
    };

    const S: u64 = 1_000;
    const M: u64 = 60 * S;
    const H: u64 = 60 * M;
    const D: u64 = 24 * H;

    if step_ms < M {
        dt.format("%H:%M:%S").to_string()
    } else if step_ms < D {
        dt.format("%H:%M").to_string()
    } else if step_ms < 7 * D {
        dt.format("%b %d").to_string()
    } else if step_ms < 365 * D {
        dt.format("%Y-%m").to_string()
    } else {
        dt.format("%Y").to_string()
    }
}

/// Tick timestamps for the x axis, walked on the *compressed* axis so spacing is
/// picked from the pixels actually on screen, then expanded back to the real timestamps
/// the labels are formatted from.
fn time_ticks(
    time_axis: &TimeAxis,
    compressed_min: u64,
    compressed_max: u64,
    px_per_ms: f32,
    min_px: f32,
) -> (Vec<u64>, u64) {
    let span = compressed_max.saturating_sub(compressed_min).max(1);
    let mut step = *time_tick_candidates().first().unwrap_or(&1_000);
    for &candidate in time_tick_candidates() {
        let px = candidate as f32 * px_per_ms;
        if px >= min_px {
            step = candidate;
            break;
        } else {
            step = candidate;
        }
    }
    // Align first tick to the step boundary >= compressed_min
    let first = if compressed_min.is_multiple_of(step) {
        compressed_min
    } else {
        (compressed_min / step + 1) * step
    };
    let mut out = Vec::new();
    let mut t = first;
    for _ in 0..=2000 {
        if t > compressed_max {
            break;
        }
        out.push(time_axis.expand(t));
        t = t.saturating_add(step);
        if (t - first) > span + step {
            break;
        }
    }
    out.dedup();

    (out, step)
}

pub mod domain {
    pub fn align_floor(ts: u64, dt: u64) -> u64 {
        if dt == 0 {
            return ts;
        }
        (ts / dt) * dt
    }

    pub fn align_ceil(ts: u64, dt: u64) -> u64 {
        if dt == 0 {
            return ts;
        }
        let f = (ts / dt) * dt;
        if f == ts { ts } else { f.saturating_add(dt) }
    }

    pub fn interpolate_y_at(points: &[(u64, f32)], x: u64) -> Option<f32> {
        if points.is_empty() {
            return None;
        }
        let idx_right = points.iter().position(|(px, _)| *px >= x)?;
        Some(match idx_right {
            0 => points[0].1,
            i => {
                let (x0, y0) = points[i - 1];
                let (x1, y1) = points[i];
                let dx = x1.saturating_sub(x0) as f32;
                if dx > 0.0 {
                    let t = (x.saturating_sub(x0)) as f32 / dx;
                    y0 + (y1 - y0) * t.clamp(0.0, 1.0)
                } else {
                    y0
                }
            }
        })
    }

    /// Visible window as a pair of *compressed* axis positions (see
    /// [`data::chart::TimeAxis`]) — zoom is counted in bars, and on the compressed axis
    /// a bar is a fixed width whether or not the market was open on either side of it.
    /// Callers expand the pair when they need real timestamps to filter data with.
    pub fn window(
        series: &[&[(u64, f32)]],
        zoom: super::Zoom,
        pan_points: f32,
        dt: u64,
        time_axis: &super::TimeAxis,
    ) -> Option<(u64, u64)> {
        if series.is_empty() {
            return None;
        }

        let mut any = false;
        let mut data_min_x = u64::MAX;
        let mut data_max_x = u64::MIN;
        for pts in series {
            for (x, _) in *pts {
                any = true;
                let x = time_axis.compress(*x);
                if x < data_min_x {
                    data_min_x = x;
                }
                if x > data_max_x {
                    data_max_x = x;
                }
            }
        }
        if !any {
            return None;
        }
        if data_max_x == data_min_x {
            data_max_x = data_max_x.saturating_add(1);
        }

        let add_signed = |v: u64, d: i64| -> u64 {
            if d >= 0 {
                v.saturating_add(d as u64)
            } else {
                v.saturating_sub((-d) as u64)
            }
        };

        let span = if zoom.is_all() {
            data_max_x.saturating_sub(data_min_x).max(1)
        } else {
            let n = zoom.0;
            let mut s = ((n.saturating_sub(1)) as u64).saturating_mul(dt);
            if s == 0 {
                s = 1;
            }
            s
        };

        let pad_ms = (pan_points * dt as f32).round() as i64;
        let mut right = add_signed(data_max_x, pad_ms);
        let right_cap = data_max_x.saturating_add(span);
        if right > right_cap {
            right = right_cap;
        }
        let left = right.saturating_sub(span);

        let left = align_floor(left, dt);
        let right = align_ceil(right, dt);

        Some((left, right))
    }

    pub fn pct_domain(series: &[&[(u64, f32)]], min_x: u64, max_x: u64) -> Option<(f32, f32)> {
        let mut min_pct = f32::INFINITY;
        let mut max_pct = f32::NEG_INFINITY;
        let mut any = false;

        for pts in series {
            if pts.is_empty() {
                continue;
            }

            let y0 = interpolate_y_at(pts, min_x).unwrap_or(0.0);
            if y0 == 0.0 {
                continue;
            }

            let mut has_visible = false;
            for (_x, y) in pts.iter().filter(|(x, _)| *x >= min_x && *x <= max_x) {
                has_visible = true;
                let pct = ((*y / y0) - 1.0) * 100.0;
                if pct < min_pct {
                    min_pct = pct;
                }
                if pct > max_pct {
                    max_pct = pct;
                }
            }

            if has_visible {
                any = true;
                if 0.0 < min_pct {
                    min_pct = 0.0;
                }
                if 0.0 > max_pct {
                    max_pct = 0.0;
                }
            }
        }

        if !any {
            return None;
        }

        if (max_pct - min_pct).abs() < f32::EPSILON {
            min_pct -= 1.0;
            max_pct += 1.0;
        }

        let span = (max_pct - min_pct).max(1e-6);
        let pad = span * 0.05;
        Some((min_pct - pad, max_pct + pad))
    }

    /// Same visible-window scan as `pct_domain`, but for raw price: no baseline, no
    /// forced zero crossing — just the min/max of every series' points in range, padded
    /// 5% for breathing room.
    pub fn price_domain(series: &[&[(u64, f32)]], min_x: u64, max_x: u64) -> Option<(f32, f32)> {
        let mut min_p = f32::INFINITY;
        let mut max_p = f32::NEG_INFINITY;
        let mut any = false;

        for pts in series {
            for (_x, y) in pts.iter().filter(|(x, _)| *x >= min_x && *x <= max_x) {
                any = true;
                if *y < min_p {
                    min_p = *y;
                }
                if *y > max_p {
                    max_p = *y;
                }
            }
        }

        if !any {
            return None;
        }

        if (max_p - min_p).abs() < f32::EPSILON {
            let pad = (min_p.abs() * 0.01).max(1e-6);
            min_p -= pad;
            max_p += pad;
        }

        let span = (max_p - min_p).max(1e-6);
        let pad = span * 0.05;
        Some((min_p - pad, max_p + pad))
    }
}
