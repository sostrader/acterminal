use crate::{Price, PriceStep, UnixMs, serde_util, unit::qty::Qty};

use serde::Deserializer;
use serde::de::Error as SerdeError;
use serde_json::Value;

use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy)]
pub struct DeOrder {
    pub price: f64,
    pub qty: f64,
}

impl<'de> serde::Deserialize<'de> for DeOrder {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // can be either an array like ["price","qty", ...] or an object with keys "0" and "1"
        let value = Value::deserialize(deserializer)?;

        let price = match &value {
            Value::Array(arr) => arr.first().and_then(serde_util::value_as_f64),
            Value::Object(map) => map.get("0").and_then(serde_util::value_as_f64),
            _ => None,
        }
        .ok_or_else(|| SerdeError::custom("Order price not found or invalid"))?;

        let qty = match &value {
            Value::Array(arr) => arr.get(1).and_then(serde_util::value_as_f64),
            Value::Object(map) => map.get("1").and_then(serde_util::value_as_f64),
            _ => None,
        }
        .ok_or_else(|| SerdeError::custom("Order qty not found or invalid"))?;

        Ok(DeOrder { price, qty })
    }
}

pub struct DepthPayload {
    pub last_update_id: u64,
    pub time: UnixMs,
    pub bids: Vec<DeOrder>,
    pub asks: Vec<DeOrder>,
}

pub enum DepthUpdate {
    Snapshot(DepthPayload),
    Diff(DepthPayload),
}

#[derive(Clone, Default)]
pub struct Depth {
    pub bids: BTreeMap<Price, Qty>,
    pub asks: BTreeMap<Price, Qty>,
}

impl std::fmt::Debug for Depth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Depth")
            .field("bids", &self.bids.len())
            .field("asks", &self.asks.len())
            .finish()
    }
}

impl Depth {
    fn apply_diff(&mut self, diff: &DepthPayload, min_ticksize: PriceStep) {
        Self::diff_price_levels(&mut self.bids, &diff.bids, min_ticksize);
        Self::diff_price_levels(&mut self.asks, &diff.asks, min_ticksize);
    }

    fn diff_price_levels(
        price_map: &mut BTreeMap<Price, Qty>,
        orders: &[DeOrder],
        min_ticksize: PriceStep,
    ) {
        orders.iter().for_each(|order| {
            let price = Price::from_f64(order.price).round_to_step(min_ticksize);
            let qty = Qty::from_f64(order.qty);

            if qty.is_zero() {
                price_map.remove(&price);
            } else {
                price_map.insert(price, qty);
            }
        });
    }

    fn replace_all(&mut self, snapshot: &DepthPayload, min_ticksize: PriceStep) {
        let level = |de_order: &DeOrder| {
            (
                Price::from_f64(de_order.price).round_to_step(min_ticksize),
                Qty::from_f64(de_order.qty),
            )
        };

        self.bids = snapshot.bids.iter().map(level).collect();
        self.asks = snapshot.asks.iter().map(level).collect();
    }

    pub fn mid_price(&self) -> Option<Price> {
        match (self.asks.first_key_value(), self.bids.last_key_value()) {
            (Some((ask_price, _)), Some((bid_price, _))) => Some((*ask_price + *bid_price) / 2),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct LocalDepthCache {
    pub last_update_id: u64,
    pub time: UnixMs,
    pub depth: Arc<Depth>,
}

impl LocalDepthCache {
    pub fn update(&mut self, new_depth: DepthUpdate, min_ticksize: PriceStep) {
        match new_depth {
            DepthUpdate::Snapshot(snapshot) => {
                self.last_update_id = snapshot.last_update_id;
                self.time = snapshot.time;

                let depth = Arc::make_mut(&mut self.depth);
                depth.replace_all(&snapshot, min_ticksize);
            }
            DepthUpdate::Diff(diff) => {
                self.last_update_id = diff.last_update_id;
                self.time = diff.time;

                let depth = Arc::make_mut(&mut self.depth);
                depth.apply_diff(&diff, min_ticksize);
            }
        }
    }
}
