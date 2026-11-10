//! TheoricScan Targets: independent, downstream indicator that consumes the
//! premarket auction map (price, N/seq, volume, dwell) as raw input and emits
//! only 4-5 final lines (T1..T4 + optional inherited debit).
//!
//! It never touches [`crate::chart::premarket`] internals: callers feed it with
//! plain [`AuctionNode`]s rebuilt from `PremarketLevels::levels()` (or from
//! OHLC/ticks independently).

use serde::{Deserialize, Serialize};

/// One auction node of the premarket map (raw input).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuctionNode {
    pub price: f64,
    pub seq: usize,
    pub vol: f64,
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub dwell_secs: i64,
}

impl AuctionNode {
    pub fn delta(&self) -> f64 {
        self.buy_vol - self.sell_vol
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeClass {
    Value,
    Intermediate,
    Excess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsConfig {
    pub weight_vol: f64,
    pub weight_time: f64,
    pub value_threshold: f64,
    pub excess_threshold: f64,
    pub cluster_gap: f64,
    pub enable_debit: bool,
    pub enable_delta: bool,
    pub delta_penalty: f64,
    /// Snap target prices to this tick grid (e.g. 0.5 for WDO). 0.0 = no rounding.
    pub round_tick: f64,
}

impl Default for TargetsConfig {
    fn default() -> Self {
        Self {
            weight_vol: 0.6,
            weight_time: 0.4,
            value_threshold: 0.6,
            excess_threshold: 0.3,
            cluster_gap: 2.0,
            enable_debit: true,
            enable_delta: true,
            delta_penalty: 0.8,
            round_tick: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetLine {
    pub label: String,
    pub price: f64,
    pub volume: f64,
    pub debit_origin: Option<String>,
    pub paid: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebitRecord {
    pub price: f64,
    pub origin: String,
    pub direction: Direction,
    pub target_label: String,
    pub paid: bool,
    pub paid_on: Option<String>,
}

fn classify(score: f64, is_edge: bool, cfg: &TargetsConfig) -> NodeClass {
    if score >= cfg.value_threshold {
        NodeClass::Value
    } else if score < cfg.excess_threshold && is_edge {
        NodeClass::Excess
    } else {
        NodeClass::Intermediate
    }
}

/// Score nodes, pick direction vs `current_price`, and build the T1..T4 ladder
/// (plus one compatible pending debit beyond T4, if any).
pub fn build_targets(
    nodes: &[AuctionNode],
    current_price: f64,
    cfg: &TargetsConfig,
    pending_debits: &[DebitRecord],
) -> (Vec<TargetLine>, Direction) {
    if nodes.is_empty() {
        return (vec![], Direction::Up);
    }
    let min_v = nodes.iter().map(|n| n.vol).reduce(f64::min).unwrap_or(0.0);
    let max_v = nodes.iter().map(|n| n.vol).reduce(f64::max).unwrap_or(1.0);
    let min_t = nodes
        .iter()
        .map(|n| n.dwell_secs as f64)
        .reduce(f64::min)
        .unwrap_or(0.0);
    let max_t = nodes
        .iter()
        .map(|n| n.dwell_secs as f64)
        .reduce(f64::max)
        .unwrap_or(1.0);
    let v_span = (max_v - min_v).max(f64::EPSILON);
    let t_span = (max_t - min_t).max(f64::EPSILON);

    let lo = nodes.iter().map(|n| n.price).reduce(f64::min).unwrap();
    let hi = nodes.iter().map(|n| n.price).reduce(f64::max).unwrap_or(lo);

    struct Scored<'a> {
        node: &'a AuctionNode,
        score: f64,
        class: NodeClass,
    }
    let mut scored: Vec<Scored> = nodes
        .iter()
        .map(|n| {
            let vn = (n.vol - min_v) / v_span;
            let tn = (n.dwell_secs as f64 - min_t) / t_span;
            let mut s = cfg.weight_vol * vn + cfg.weight_time * tn;
            // Delta confidence adjustment: penalize neutral/opposing delta.
            if cfg.enable_delta {
                let d = n.delta();
                let expects_buy = n.price < current_price;
                let opposing = (expects_buy && d < 0.0) || (!expects_buy && d > 0.0);
                if d.abs() < f64::EPSILON || opposing {
                    s *= cfg.delta_penalty;
                }
            }
            let edge = (n.price - lo).abs() < f64::EPSILON || (n.price - hi).abs() < f64::EPSILON;
            let class = classify(s, edge, cfg);
            Scored {
                node: n,
                score: s,
                class,
            }
        })
        .collect();

    // Direction: compare current price to the highest-scored value node.
    let anchor = scored
        .iter()
        .filter(|s| s.class == NodeClass::Value)
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
        .or_else(|| {
            scored
                .iter()
                .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
        });
    let dir = match anchor {
        Some(a) if current_price < a.node.price => Direction::Up,
        Some(a) if current_price > a.node.price => Direction::Down,
        _ => {
            if current_price <= (lo + hi) / 2.0 {
                Direction::Up
            } else {
                Direction::Down
            }
        }
    };

    // Order nodes in signal direction, nearest-first.
    scored.sort_by(|a, b| {
        let da = (a.node.price - current_price).abs();
        let db = (b.node.price - current_price).abs();
        let in_dir = |p: f64| match dir {
            Direction::Up => p >= current_price,
            Direction::Down => p <= current_price,
        };
        match (in_dir(a.node.price), in_dir(b.node.price)) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => da.partial_cmp(&db).unwrap(),
        }
    });

    // Cluster nearby nodes (within cluster_gap) summing volume.
    #[derive(Debug, Clone)]
    struct Cluster {
        price: f64,
        vol: f64,
        class: NodeClass,
        score: f64,
    }
    let mut clusters: Vec<Cluster> = vec![];
    for s in scored.iter().filter(|s| match dir {
        Direction::Up => s.node.price >= current_price,
        Direction::Down => s.node.price <= current_price,
    }) {
        if let Some(last) = clusters.last_mut()
            && (s.node.price - last.price).abs() <= cfg.cluster_gap
        {
            last.vol += s.node.vol;
            // volume-weighted price, keep best class/score
            last.price = (last.price + s.node.price) / 2.0;
            if s.score > last.score {
                last.score = s.score;
                last.class = s.class;
            }
            continue;
        }
        clusters.push(Cluster {
            price: s.node.price,
            vol: s.node.vol,
            class: s.class,
            score: s.score,
        });
    }

    // Ladder in strict directional order (nearest-first, never inverted):
    // T1 = nearest Value/Intermediate cluster, T2 = next, T3 = last before
    // the extremity, T4 = extremity itself (any class). The old code took T1
    // as the nearest *Value* (skipping nearer Intermediates) while T2 scanned
    // from the start — landing T2 nearer than T1 (e.g. T1 5164 / T2 5158) —
    // and T4 required an Excess edge, collapsing to 3 lines whenever the edge
    // scored as Value/Intermediate.
    let mut ladder: Vec<Cluster> = vec![];
    for c in &clusters {
        if ladder.len() >= 2 {
            break;
        }
        if (c.class == NodeClass::Value || c.class == NodeClass::Intermediate)
            && !ladder
                .iter()
                .any(|l| (l.price - c.price).abs() <= cfg.cluster_gap)
        {
            ladder.push(c.clone());
        }
    }
    // Extremity in signal direction (session high/low side).
    let edge = match dir {
        Direction::Up => clusters
            .iter()
            .max_by(|a, b| a.price.partial_cmp(&b.price).unwrap()),
        Direction::Down => clusters
            .iter()
            .min_by(|a, b| a.price.partial_cmp(&b.price).unwrap()),
    };
    if let Some(edge) = edge {
        // T3: last value/intermediate strictly before the extremity.
        if let Some(before) = clusters
            .iter()
            .filter(|c| c.class != NodeClass::Excess)
            .filter(|c| (c.price - edge.price).abs() > cfg.cluster_gap)
            .rfind(|_| true)
            && !ladder
                .iter()
                .any(|l| (l.price - before.price).abs() <= cfg.cluster_gap)
        {
            ladder.push(before.clone());
        }
        // T4: extremity itself.
        if !ladder
            .iter()
            .any(|l| (l.price - edge.price).abs() <= cfg.cluster_gap)
        {
            ladder.push(edge.clone());
        }
    }
    ladder.truncate(4);

    // Proximity filter: lines within one tick of each other (e.g. 5158.0 and
    // 5158.5) are the same signal — keep only the biggest volume of the group
    // instead of printing near-duplicate lines. The survivor keeps directional
    // order, so the ladder may legitimately settle at 3 lines.
    let sep = if cfg.round_tick > 0.0 {
        cfg.round_tick
    } else {
        0.5
    };
    let mut thinned: Vec<Cluster> = Vec::with_capacity(ladder.len());
    for c in ladder {
        match thinned.last_mut() {
            Some(last) if (c.price - last.price).abs() <= sep => {
                if c.vol > last.vol {
                    *last = c;
                }
            }
            _ => thinned.push(c),
        }
    }
    ladder = thinned;

    // Snap to the instrument tick grid so targets never print off-tick
    // prices (e.g. 5161.2 on a 0.5-tick WDO) created by cluster averaging.
    let snap = |p: f64| -> f64 {
        if cfg.round_tick > 0.0 {
            (p / cfg.round_tick).round() * cfg.round_tick
        } else {
            p
        }
    };
    let names = ["T1", "T2", "T3", "T4"];
    let mut out: Vec<TargetLine> = ladder
        .iter()
        .enumerate()
        .map(|(i, c)| TargetLine {
            label: names[i.min(3)].to_string(),
            price: snap(c.price),
            volume: c.vol,
            debit_origin: None,
            paid: false,
        })
        .collect();

    // Debit inheritance: pending debit beyond T4 in same direction becomes extra line.
    if cfg.enable_debit {
        let t4 = out.last().map(|t| t.price);
        for d in pending_debits
            .iter()
            .filter(|d| !d.paid && d.direction == dir)
        {
            let beyond = match (t4, dir) {
                (Some(t), Direction::Up) => d.price > t,
                (Some(t), Direction::Down) => d.price < t,
                (None, _) => true,
            };
            if beyond {
                out.push(TargetLine {
                    label: "DÉBITO".to_string(),
                    price: d.price,
                    volume: 0.0,
                    debit_origin: Some(d.origin.clone()),
                    paid: false,
                });
            }
        }
    }

    (out, dir)
}

/// End-of-session settlement: mark targets PAID/DEBIT from session high/low.
pub fn settle_targets(
    targets: &[TargetLine],
    dir: Direction,
    session_high: f64,
    session_low: f64,
    origin: &str,
) -> Vec<DebitRecord> {
    targets
        .iter()
        .filter(|t| t.debit_origin.is_none())
        .filter(|t| {
            let hit = match dir {
                Direction::Up => session_high >= t.price,
                Direction::Down => session_low <= t.price,
            };
            !hit
        })
        .map(|t| DebitRecord {
            price: t.price,
            origin: origin.to_string(),
            direction: dir,
            target_label: t.label.clone(),
            paid: false,
            paid_on: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<AuctionNode> {
        vec![
            AuctionNode {
                price: 5170.0,
                seq: 1,
                vol: 10.0,
                buy_vol: 2.0,
                sell_vol: 8.0,
                dwell_secs: 5,
            },
            AuctionNode {
                price: 5175.0,
                seq: 2,
                vol: 100.0,
                buy_vol: 70.0,
                sell_vol: 30.0,
                dwell_secs: 120,
            },
            AuctionNode {
                price: 5180.0,
                seq: 3,
                vol: 60.0,
                buy_vol: 30.0,
                sell_vol: 30.0,
                dwell_secs: 60,
            },
            AuctionNode {
                price: 5185.0,
                seq: 4,
                vol: 5.0,
                buy_vol: 4.0,
                sell_vol: 1.0,
                dwell_secs: 2,
            },
        ]
    }

    #[test]
    fn ladder_up_from_below_anchor() {
        let cfg = TargetsConfig::default();
        let (targets, dir) = build_targets(&nodes(), 5172.0, &cfg, &[]);
        assert_eq!(dir, Direction::Up);
        assert!(!targets.is_empty());
        assert_eq!(targets[0].label, "T1");
        assert!(targets.iter().any(|t| (t.price - 5185.0).abs() < 3.0));
    }

    #[test]
    fn settle_marks_debit_when_not_hit() {
        let cfg = TargetsConfig::default();
        let (targets, dir) = build_targets(&nodes(), 5172.0, &cfg, &[]);
        let debits = settle_targets(&targets, dir, 5176.0, 5170.0, "2026-09-16");
        // T4 at 5185 not hit by high 5176 -> debit
        assert!(debits.iter().any(|d| (d.price - 5185.0).abs() < 3.0));
    }

    #[test]
    fn tick_spaced_levels_keep_their_own_volume() {
        // Regression: WDO auction levels 1.0 apart with the default gap (2.0)
        // chain-merge into ONE cluster summing the whole session
        // (V:983 shown as Vol 26177). With gap 0 (theoric overlay) each
        // target carries exactly its own premarket level volume.
        let levels: Vec<AuctionNode> = (0..8)
            .map(|i| AuctionNode {
                price: 5163.0 + i as f64,
                seq: i + 1,
                vol: 983.0,
                buy_vol: 500.0,
                sell_vol: 483.0,
                dwell_secs: 5,
            })
            .collect();
        let mut cfg = TargetsConfig::default();
        cfg.cluster_gap = 0.0;
        let (targets, _) = build_targets(&levels, 5163.0, &cfg, &[]);
        assert!(!targets.is_empty());
        for t in &targets {
            if t.debit_origin.is_none() {
                assert_eq!(
                    t.volume, 983.0,
                    "target {} summed levels: {}",
                    t.label, t.volume
                );
            }
        }
        // Prices stay exactly on the auction levels (no cluster averaging).
        for t in targets.iter().filter(|t| t.debit_origin.is_none()) {
            assert!(
                (t.price.fract() - 0.0).abs() < 1e-9,
                "off-level price {}",
                t.price
            );
        }
    }

    /// Nearest level is Intermediate, first Value farther out, edge scores as
    /// Value: the old ladder took T1 as the farther Value while T2 backfilled
    /// the nearer level (T1 5164 / T2 5158, inverted) and dropped T4 (3 lines).
    fn inversion_session() -> Vec<AuctionNode> {
        let n = |price: f64, seq: usize, vol: f64, dwell_secs: i64| AuctionNode {
            price,
            seq,
            vol,
            buy_vol: vol / 2.0,
            sell_vol: vol / 2.0,
            dwell_secs,
        };
        vec![
            n(5155.0, 1, 50.0, 0),     // lo edge, Excess
            n(5158.0, 2, 200.0, 2),    // nearest, Intermediate
            n(5164.0, 3, 2000.0, 120), // first Value
            n(5170.0, 4, 150.0, 10),   // Intermediate
            n(5194.5, 5, 1500.0, 60),  // hi edge, Value (not Excess)
        ]
    }

    #[test]
    fn ladder_up_is_ordered_and_complete() {
        let mut cfg = TargetsConfig::default();
        cfg.cluster_gap = 0.0;
        cfg.enable_delta = false;
        let (targets, dir) = build_targets(&inversion_session(), 5150.0, &cfg, &[]);
        assert_eq!(dir, Direction::Up);
        let prices: Vec<f64> = targets.iter().map(|t| t.price).collect();
        assert_eq!(prices, vec![5158.0, 5164.0, 5170.0, 5194.5]);
        assert_eq!(targets[0].label, "T1");
        assert_eq!(targets[3].label, "T4");
        let vols: Vec<f64> = targets.iter().map(|t| t.volume).collect();
        assert_eq!(vols, vec![200.0, 2000.0, 150.0, 1500.0]);
    }

    #[test]
    fn ladder_down_is_ordered_and_complete() {
        let mut cfg = TargetsConfig::default();
        cfg.cluster_gap = 0.0;
        cfg.enable_delta = false;
        let (targets, dir) = build_targets(&inversion_session(), 5200.0, &cfg, &[]);
        assert_eq!(dir, Direction::Down);
        let prices: Vec<f64> = targets.iter().map(|t| t.price).collect();
        assert!(
            prices.windows(2).all(|w| w[0] > w[1]),
            "inverted: {prices:?}"
        );
        assert_eq!(prices.len(), 4);
        assert_eq!(prices[0], 5194.5);
        assert_eq!(prices[3], 5155.0);
    }

    #[test]
    fn near_duplicate_levels_keep_biggest_volume() {
        // 5158.0 (2050) and 5158.5 (1333) are one tick apart: same signal,
        // only the bigger stays, so the ladder settles at 3 lines.
        let n = |price: f64, seq: usize, vol: f64, dwell_secs: i64| AuctionNode {
            price,
            seq,
            vol,
            buy_vol: vol / 2.0,
            sell_vol: vol / 2.0,
            dwell_secs,
        };
        let levels = vec![
            n(5158.0, 1, 2050.0, 5),
            n(5158.5, 2, 1333.0, 2),
            n(5171.0, 3, 1026.0, 21),
            n(5194.5, 4, 1536.0, 8),
        ];
        let mut cfg = TargetsConfig::default();
        cfg.cluster_gap = 0.0;
        cfg.enable_delta = false;
        let (targets, dir) = build_targets(&levels, 5150.0, &cfg, &[]);
        assert_eq!(dir, Direction::Up);
        let prices: Vec<f64> = targets.iter().map(|t| t.price).collect();
        assert_eq!(prices, vec![5158.0, 5171.0, 5194.5]);
        let vols: Vec<f64> = targets.iter().map(|t| t.volume).collect();
        assert_eq!(vols, vec![2050.0, 1026.0, 1536.0]);
        assert_eq!(targets[0].label, "T1");
        assert_eq!(targets[2].label, "T3");
    }

    #[test]
    fn debit_inherited_beyond_t4() {
        let cfg = TargetsConfig::default();
        let pending = vec![DebitRecord {
            price: 5190.0,
            origin: "2026-09-16".into(),
            direction: Direction::Up,
            target_label: "T4".into(),
            paid: false,
            paid_on: None,
        }];
        let (targets, _) = build_targets(&nodes(), 5172.0, &cfg, &pending);
        assert!(
            targets
                .iter()
                .any(|t| t.label == "DÉBITO" && (t.price - 5190.0).abs() < 1e-9)
        );
    }
}
