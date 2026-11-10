//! Static table of the CME FX-futures products served over the Zenoh feed.
//!
//! Ported verbatim from ACTerminal (`acterminal-tauri/src-tauri/src/zenoh_feed.rs` +
//! `acterminal-tauri/index.html`'s symbol picker) — this is the sole source of truth,
//! there is no remote discovery/metadata endpoint for these products.

use crate::unit::MinTicksize;
use crate::unit::price::PriceStep;
use serde::{Deserialize, Serialize};

/// Which section of the symbol picker a product is grouped under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Category {
    /// The B3 mini-dollar contract, quoted both via the CME FX proxy (6L/6LNEXT,
    /// displayed as WDO-A/WDO-N) and the MetaTrader/B3 bridge (WDO/WDONEXT,
    /// displayed as WDOB3-A/WDOB3-N).
    B3,
    /// The remaining CME FX-futures majors/micros.
    Fx,
    /// CME equity-index futures (S&P 500).
    Indices,
}

impl Category {
    /// B3 sorts before FX in the symbol picker; indices sort last.
    pub const ALL: [Category; 3] = [Category::B3, Category::Fx, Category::Indices];
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Category::B3 => "B3",
            Category::Fx => "FX",
            Category::Indices => "IND",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SymbolInfo {
    /// CME product code, e.g. "6E".
    pub code: &'static str,
    /// Human-readable pair name, e.g. "EURUSD".
    pub display: &'static str,
    /// Symbol-picker grouping; purely cosmetic, has no bearing on the Zenoh
    /// subscription key (`code`).
    pub category: Category,
    /// Decimal places every price for this product is rendered with — fixed per
    /// instrument, exactly as ACTerminal's `get_decimals` defines it, never inferred
    /// from the tick step or a tick multiplier.
    pub decimals: u32,
    /// Minimum price increment for the *directly quoted* products.
    ///
    /// Ignored for the reciprocal-quoted ones: their displayed grid is whatever
    /// `codec::inverted_display_grid` says `invert_price` snaps to, which is what
    /// [`Self::tick_size`] returns. Read the step through that method, never this field.
    quoted_tick_size: f64,
    /// Spacing of the venue's *raw* ladder, before any inversion.
    ///
    /// Read off the live DOM per product, which is the only place it is observable —
    /// the depth payload carries no tick metadata. Only meaningful for the
    /// reciprocal-quoted products, where it is what sets how far apart two adjacent
    /// levels end up once inverted (see [`Self::inverted_spacing_at`]); for the direct
    /// ones it is simply `quoted_tick_size`.
    raw_tick: f64,
    /// Whether the raw Zenoh feed reports the reciprocal price (e.g. JPY/USD → USD/JPY).
    pub invert: bool,
}

impl SymbolInfo {
    /// Spacing of the price grid the UI actually receives.
    ///
    /// For a reciprocal-quoted product this is the grid `invert_price` rounds onto, not
    /// the underlying CME tick — after inversion no price can ever land between those
    /// grid points, so using the raw tick would leave the ladder and heatmap striped
    /// with rows nothing can fill.
    pub fn tick_size(&self) -> f64 {
        super::codec::inverted_display_grid(self.code).unwrap_or(self.quoted_tick_size)
    }

    /// Exact `PriceStep` for this symbol's price grid, constructed directly from atomic
    /// units rather than routed through the lossy power-of-10 `MinTicksize` conversion.
    pub fn price_step(&self) -> PriceStep {
        PriceStep::from_f64_exact(self.tick_size())
    }

    /// How far apart two adjacent book levels land once inverted, at `inverted_price`.
    ///
    /// `d(k/x) = k·dx/x²`, so this is *not* a constant: it scales with the square of the
    /// price, which is why one fixed display grid fits some inverted products and leaves
    /// others striped with rows no level can occupy. 6L is quoted per 1000, hence the
    /// scale factor.
    ///
    /// Returns `None` for the directly-quoted products, whose spacing is just their tick.
    pub fn inverted_spacing_at(&self, inverted_price: f64) -> Option<f64> {
        if !self.invert || inverted_price <= 0.0 {
            return None;
        }

        let scale = if self.code == "6L" || self.code == "6LNEXT" {
            1000.0
        } else {
            1.0
        };
        // Work back to the raw quote so callers can pass the price they actually display.
        let raw = scale / inverted_price;

        Some(scale * self.raw_tick / (raw * raw))
    }

    /// Fixed display precision for this product, for `Price::to_string`/`fmt_into`.
    pub fn display_precision(&self) -> MinTicksize {
        MinTicksize::new(-(self.decimals as i8))
    }
}

pub const SYMBOLS: [SymbolInfo; 16] = [
    SymbolInfo {
        // 6L, the CME FX proxy for the mini-dollar — displayed under the B3 category
        // alongside WDO/WDONEXT since both track the same underlying (USD/BRL).
        code: "6L",
        display: "WDO-A",
        category: Category::B3,
        decimals: 2,
        // Reciprocal-quoted: the effective step comes from
        // `codec::inverted_display_grid`, so this is never read.
        quoted_tick_size: 0.0,
        raw_tick: 0.00005,
        invert: true,
    },
    SymbolInfo {
        // 6L's next contract month, streamed on its own `ticks/6LNEXT` /
        // `doms/6LNEXT` / `quotes/6LNEXT` topics (see stream/src/main.rs
        // get_symbol_for_product("6LNEXT")) — same instrument/feed convention as
        // 6L, just a different expiry, so every parameter mirrors it exactly.
        code: "6LNEXT",
        display: "WDO-N",
        category: Category::B3,
        decimals: 2,
        quoted_tick_size: 0.0,
        raw_tick: 0.00005,
        invert: true,
    },
    SymbolInfo {
        // B3 Mini Dollar futures, fed via the MetaTrader bridge (metatrader/) rather
        // than Acfeed — direct BRL-points quote, not reciprocal like 6L, since
        // the bridge already publishes absolute prices (see
        // metatrader/EA_WDO_Premarket_Zenoh.mq5 / metatrader/src/main.rs).
        code: "WDO",
        display: "WDOB3-A",
        category: Category::B3,
        // Same 0.5 grid as WDO-A (6L above), so it must show both decimals for the
        // same reason: a whole-point level like 5146.00 needs the trailing zero, or a
        // half-point level like 5145.50 collapses to "5145.5" and looks like a
        // different (and finer) grid than WDO-A's ".50"/".00" ticks.
        decimals: 2,
        quoted_tick_size: 0.5,
        raw_tick: 0.5,
        invert: false,
    },
    SymbolInfo {
        // WDO's next contract month, mirrors WDO — see metatrader/EA_WDO_Premarket_Zenoh.mq5.
        code: "WDONEXT",
        display: "WDOB3-N",
        category: Category::B3,
        decimals: 2,
        quoted_tick_size: 0.5,
        raw_tick: 0.5,
        invert: false,
    },
    SymbolInfo {
        code: "6E",
        display: "EURUSD",
        category: Category::Fx,
        decimals: 5,
        quoted_tick_size: 0.00005,
        raw_tick: 0.00005,
        invert: false,
    },
    SymbolInfo {
        // Unlike the other direct majors, sterling quotes a full pip, not a half one —
        // the venue's own ladder steps 1.3493 -> 1.3494, and the depth confirms it: every
        // resting level lands on a 1e-4 multiple. On a half-pip grid the ladder and the
        // heatmap come out striped, with every second row empty by construction.
        code: "6B",
        display: "GBPUSD",
        category: Category::Fx,
        decimals: 4,
        quoted_tick_size: 0.0001,
        raw_tick: 0.0001,
        invert: false,
    },
    SymbolInfo {
        code: "6A",
        display: "AUDUSD",
        category: Category::Fx,
        decimals: 5,
        quoted_tick_size: 0.00005,
        raw_tick: 0.00005,
        invert: false,
    },
    SymbolInfo {
        code: "6N",
        display: "NZDUSD",
        category: Category::Fx,
        decimals: 5,
        quoted_tick_size: 0.00005,
        raw_tick: 0.00005,
        invert: false,
    },
    SymbolInfo {
        code: "6J",
        display: "USDJPY",
        category: Category::Fx,
        decimals: 2,
        // Reciprocal-quoted: the effective step comes from
        // `codec::inverted_display_grid`, so this is never read.
        quoted_tick_size: 0.0,
        raw_tick: 0.0000005,
        invert: true,
    },
    SymbolInfo {
        code: "6C",
        display: "USDCAD",
        category: Category::Fx,
        decimals: 4,
        // Reciprocal-quoted: the effective step comes from
        // `codec::inverted_display_grid`, so this is never read.
        quoted_tick_size: 0.0,
        raw_tick: 0.00005,
        invert: true,
    },
    SymbolInfo {
        code: "6S",
        display: "USDCHF",
        category: Category::Fx,
        decimals: 5,
        // Reciprocal-quoted: the effective step comes from
        // `codec::inverted_display_grid`, so this is never read.
        quoted_tick_size: 0.0,
        raw_tick: 0.00005,
        invert: true,
    },
    SymbolInfo {
        code: "6M",
        display: "USDMXN",
        category: Category::Fx,
        decimals: 4,
        // Reciprocal-quoted: the effective step comes from
        // `codec::inverted_display_grid`, so this is never read.
        quoted_tick_size: 0.0,
        raw_tick: 0.00001,
        invert: true,
    },
    SymbolInfo {
        // Unlike the full-size 6E, the micro trades a whole pip, not a half one — the
        // venue's ladder steps 0.72110 -> 0.72120, every level landing on a 1e-4
        // multiple (measured off a live DOM). Same grid as M6B/6B, just displayed
        // with one extra (always-zero) decimal.
        code: "M6E",
        display: "Micro EURUSD",
        category: Category::Fx,
        decimals: 5,
        quoted_tick_size: 0.0001,
        raw_tick: 0.0001,
        invert: false,
    },
    SymbolInfo {
        // Same whole-pip correction as M6E — see that comment.
        code: "M6A",
        display: "Micro AUDUSD",
        category: Category::Fx,
        decimals: 5,
        quoted_tick_size: 0.0001,
        raw_tick: 0.0001,
        invert: false,
    },
    SymbolInfo {
        code: "M6B",
        display: "Micro GBPUSD",
        category: Category::Fx,
        decimals: 4,
        quoted_tick_size: 0.0001,
        raw_tick: 0.0001,
        invert: false,
    },
    SymbolInfo {
        // CME S&P 500 futures, streamed on `ticks/ES` / `doms/ES` / `quotes/ES`
        // (continuous front contract, see stream/src/main.rs CONTINUOUS_PRODUCTS).
        // Direct quote on a 0.25 grid (.00/.25/.50/.75) with 2 decimals, so a
        // level like 7740.25 renders whole — never 7740.2 or 7740.250.
        code: "ES",
        display: "S&P 500",
        category: Category::Indices,
        decimals: 2,
        quoted_tick_size: 0.25,
        raw_tick: 0.25,
        invert: false,
    },
];

pub fn find(code: &str) -> Option<&'static SymbolInfo> {
    SYMBOLS.iter().find(|s| s.code == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_codes_findable() {
        for s in &SYMBOLS {
            assert_eq!(find(s.code), Some(s));
        }
        assert_eq!(find("BTCUSDT"), None);
    }

    #[test]
    fn gold_and_rub_are_removed() {
        assert_eq!(find("GC"), None);
        assert_eq!(find("6R"), None);
    }

    #[test]
    fn b3_category_sorts_first_in_declared_order() {
        let categories: Vec<Category> = SYMBOLS.iter().map(|s| s.category).collect();
        let first_fx = categories
            .iter()
            .position(|c| *c == Category::Fx)
            .expect("at least one FX symbol");
        assert!(
            categories[..first_fx].iter().all(|c| *c == Category::B3),
            "every B3 symbol must precede every FX symbol"
        );

        let b3_displays: Vec<&str> = SYMBOLS
            .iter()
            .filter(|s| s.category == Category::B3)
            .map(|s| s.display)
            .collect();
        assert_eq!(
            b3_displays,
            ["WDO-A", "WDO-N", "WDOB3-A", "WDOB3-N"],
            "B3 group must list the CME-fed WDO pair before the MetaTrader-fed WDOB3 pair"
        );
    }

    #[test]
    fn es_sp500_subscription_grid_and_display() {
        let info = find("ES").expect("ES registered");
        assert_eq!(info.display, "S&P 500");
        assert!(!info.invert, "ES is quoted directly");
        assert_eq!(info.category, Category::Indices);
        // 0.25 -> 25 * 10^9 atomic units.
        assert_eq!(info.price_step().units, 25 * 10i64.pow(9));
        assert_eq!(info.decimals, 2);
        assert_eq!(
            crate::unit::price::Price::from_f64(7740.25).to_string(info.display_precision()),
            "7740.25",
        );
        assert_eq!(super::super::codec::get_decimals("ES"), 2);
        assert_eq!(super::super::codec::get_tick_size_for_symbol("ES"), 0.25);
    }

    #[test]
    fn price_steps_match_the_post_inversion_grid() {
        // 0.5 -> 5 * 10^10 atomic units (not rounded up to a power of ten)
        assert_eq!(find("6L").unwrap().price_step().units, 5 * 10i64.pow(10));
        // 6J is inverted onto a cent grid: 0.01 -> 10^9
        assert_eq!(find("6J").unwrap().price_step().units, 10i64.pow(9));
        // 6C/6M invert onto a 1e-4 grid -> 10^7
        assert_eq!(find("6C").unwrap().price_step().units, 10i64.pow(7));
        assert_eq!(find("6M").unwrap().price_step().units, 10i64.pow(7));
        assert_eq!(find("6S").unwrap().price_step().units, 10i64.pow(6));
        // 6E quotes on a half-pip grid: 0.00005 -> 5 * 10^6
        assert_eq!(find("6E").unwrap().price_step().units, 5 * 10i64.pow(6));
        // 6B quotes on a whole pip: 0.0001 -> 10^7
        assert_eq!(find("6B").unwrap().price_step().units, 10i64.pow(7));
    }

    /// The step must be the grid prices actually land on, or the ladder and heatmap end
    /// up striped with rows nothing can ever fill.
    #[test]
    fn every_inverted_price_is_a_whole_multiple_of_its_step() {
        for info in SYMBOLS.iter().filter(|s| s.invert) {
            let step = info.price_step();
            assert!(step.units > 0, "{} has no step", info.code);

            // Sweep a spread of raw reciprocal quotes through the real inversion.
            for i in 1..200 {
                let raw = 0.005 + f64::from(i) * 0.000_137;
                let inverted = super::super::codec::invert_price(raw, info.code);
                let units = crate::unit::price::Price::from_f64(inverted).units;

                assert_eq!(
                    units % step.units,
                    0,
                    "{} inverted {raw} -> {inverted}, off the {:?} grid",
                    info.code,
                    step.units
                );
            }
        }
    }

    /// The directly-quoted products get their step straight from the table, so it has to
    /// be the grid their book actually rests on. These are levels read off the venue's
    /// own ladder: sterling steps a whole pip while the euro steps half of one, and
    /// configuring sterling as a half-pip product is what left every second ladder row —
    /// and every second heatmap bin — empty by construction.
    #[test]
    fn direct_products_step_the_grid_their_book_rests_on() {
        // (code, levels seen on the venue's ladder)
        let measured: [(&str, &[f64]); 3] = [
            ("6B", &[1.3493, 1.3494, 1.3495, 1.3496, 1.3500, 1.3503]),
            ("6E", &[1.084_25, 1.084_30, 1.084_35, 1.084_40]),
            ("ES", &[7740.25, 7740.50, 7740.75, 7741.00, 7745.00]),
        ];

        for (code, levels) in measured {
            let info = find(code).unwrap();
            assert!(!info.invert, "{code} is quoted directly");

            let step = info.price_step();
            for level in levels {
                let units = crate::unit::price::Price::from_f64(*level).units;
                assert_eq!(
                    units % step.units,
                    0,
                    "{code}: {level} is off the configured grid"
                );
            }

            // A grid finer than the book's would still divide every level — it also has
            // to be coarse enough that consecutive levels are adjacent rows.
            let adjacent = crate::unit::price::Price::from_f64(levels[1]).units
                - crate::unit::price::Price::from_f64(levels[0]).units;
            assert_eq!(
                adjacent, step.units,
                "{code}: consecutive book levels must be one row apart"
            );
        }
    }

    /// Reproduces the spacing measured off live DOM screenshots, one product at a time.
    ///
    /// This is the number the display grid has to match: where it doesn't, the ladder
    /// shows rows that no level can ever occupy. 6C is the only inverted product whose
    /// configured grid happens to line up, which is why it alone looked correct.
    #[test]
    fn inverted_spacing_matches_the_measured_book() {
        // (code, inverted price seen on screen, spacing measured from the raw ladder)
        let measured = [
            ("6J", 158.390_75, 0.012_544),
            ("6S", 0.796_40, 0.000_032),
            ("6L", 5_204.267_5, 1.354_220),
            ("6M", 17.003_91, 0.002_891),
            ("6C", 1.401_62, 0.000_098),
        ];

        for (code, price, expected) in measured {
            let info = find(code).unwrap();
            let spacing = info
                .inverted_spacing_at(price)
                .expect("an inverted product has a spacing");

            let rel_err = (spacing - expected).abs() / expected;
            assert!(
                rel_err < 0.01,
                "{code}: computed {spacing}, measured {expected}"
            );
        }
    }

    #[test]
    fn directly_quoted_products_have_no_inversion_spacing() {
        for info in SYMBOLS.iter().filter(|s| !s.invert) {
            assert_eq!(info.inverted_spacing_at(1.08), None, "{}", info.code);
        }
    }

    /// The products whose fixed decimals disagree with what the step alone implies are
    /// exactly the ones that used to render wrong, so pin them explicitly.
    #[test]
    fn display_precision_comes_from_the_table_not_the_step() {
        let cases = [
            ("6E", 5, "1.08425"),
            // Whole-pip grid, so the fifth decimal is always a zero — 4 is what the
            // venue's ladder shows.
            ("6B", 4, "1.0843"),
            // 0.5 step would imply 1 decimal; ACTerminal shows 2.
            ("6L", 2, "1.08"),
            // 0.01 step implies 2 — matches.
            ("6J", 2, "1.08"),
            // 1e-4 step implies 4 — matches.
            ("6C", 4, "1.0843"),
            ("6M", 4, "1.0843"),
            ("6S", 5, "1.08425"),
        ];

        for (code, decimals, expected) in cases {
            let info = find(code).unwrap();
            assert_eq!(info.decimals, decimals, "{code} decimals");
            assert_eq!(
                crate::unit::price::Price::from_f64(1.084_25).to_string(info.display_precision()),
                expected,
                "{code} rendering"
            );
        }
    }
}
