use exchange::SerTicker;
use serde::{Deserialize, Serialize};

/// What a comparison chart's y-axis plots: each series' % change from the baseline at
/// the left edge of the visible window, or each series' raw price. Two comparison-chart
/// panes with the same tickers but different units let similar assets be read either
/// relative (% move together) or absolute (price levels line up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Unit {
    #[default]
    Percent,
    Price,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub colors: Vec<(SerTicker, iced_core::Color)>,
    pub names: Vec<(SerTicker, String)>,
    pub inverted: Vec<(SerTicker, bool)>,
    #[serde(default)]
    pub unit: Unit,
}
