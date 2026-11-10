use std::fmt::{self, Debug, Display};

use enum_map::Enum;
use exchange::adapter::MarketKind;
use serde::{Deserialize, Serialize};

pub trait Indicator: PartialEq + Display + 'static {
    fn for_market(market: MarketKind) -> &'static [Self]
    where
        Self: Sized;
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Eq, Enum)]
pub enum KlineIndicator {
    Volume,
    BarAnalysis,
    CumulativeDelta,
    /// Pre-market auction high/low glowing walls. OFF by default; loaded from
    /// the `hist/premarket` fetch (fetch-only) and drawn while active. See
    /// [`crate::chart::premarket::PremarketLevels`].
    PremarketHighLow,
    /// TheoricScan Targets (T1..T4 + debit). Independent from Premarket H/L;
    /// reads the same auction map as raw input only.
    TheoricTargets,
    /// MAGNETS auction attraction levels (top-K by matchable-volume x dwell,
    /// premarket book only, frozen at the auction close).
    /// See [`crate::chart::magnets::MagnetStore`].
    Magnets,
    /// Masterpice: Nós de Leilão (N/V/T, score, Alvo1 Pivô, Alvo2 Extremos,
    /// Amplitude Núcleo, Alvo3 Projeção). Premarket do dia, estático após o
    /// close. Ver manual `manual_indicador_leilao_volume_profile_v2.md`.
    /// Ver [`crate::chart::masterpice::MasterpiceStore`].
    Masterpice,
    /// Masterpice Profile: Masterpice + Volume Profile same-day tick a tick
    /// em tempo real (POC/VAH/VAL + promoção de pivô). Só Candles.
    /// Ver [`crate::chart::masterpice_profile::VolumeDayStore`].
    MasterpiceProfile,
    /// Masterpice V2 (experimental): mesma leitura de leilão, mas V = fila
    /// parada acumulada no tempo × mudanças reais de tamanho, classes por
    /// percentis da sessão e trava anti-silêncio. Convive com a V1 p/
    /// comparação. Ver [`crate::chart::masterpice_v2::MasterpiceStoreV2`].
    MasterpiceV2,
}

impl Indicator for KlineIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        match market {
            MarketKind::Spot => &Self::FOR_SPOT,
            MarketKind::LinearPerps | MarketKind::InversePerps => &Self::FOR_PERPS,
        }
    }
}

impl KlineIndicator {
    // Indicator togglers on UI menus depend on these arrays.
    // Every variant needs to be in either SPOT, PERPS or both.
    /// Indicators that can be used with spot market tickers
    const FOR_SPOT: [KlineIndicator; 9] = [
        KlineIndicator::Volume,
        KlineIndicator::BarAnalysis,
        KlineIndicator::CumulativeDelta,
        KlineIndicator::PremarketHighLow,
        KlineIndicator::TheoricTargets,
        KlineIndicator::Magnets,
        KlineIndicator::Masterpice,
        KlineIndicator::MasterpiceProfile,
        KlineIndicator::MasterpiceV2,
    ];
    /// Indicators that can be used with perpetual swap market tickers
    const FOR_PERPS: [KlineIndicator; 9] = [
        KlineIndicator::Volume,
        KlineIndicator::BarAnalysis,
        KlineIndicator::CumulativeDelta,
        KlineIndicator::PremarketHighLow,
        KlineIndicator::TheoricTargets,
        KlineIndicator::Magnets,
        KlineIndicator::Masterpice,
        KlineIndicator::MasterpiceProfile,
        KlineIndicator::MasterpiceV2,
    ];
}

impl Display for KlineIndicator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            KlineIndicator::Volume => write!(f, "Volume"),
            KlineIndicator::BarAnalysis => write!(f, "Bar Analysis"),
            KlineIndicator::CumulativeDelta => write!(f, "CVD"),
            KlineIndicator::PremarketHighLow => write!(f, "Premarket H/L"),
            KlineIndicator::TheoricTargets => write!(f, "Theoric Targets"),
            KlineIndicator::Magnets => write!(f, "Magnets"),
            KlineIndicator::Masterpice => write!(f, "Masterpice"),
            KlineIndicator::MasterpiceProfile => write!(f, "Masterpice Profile"),
            KlineIndicator::MasterpiceV2 => write!(f, "Masterpice V2"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Eq, Enum)]
pub enum HeatmapIndicator {
    Volume,
    /// Pre-market auction high/low glowing walls. OFF by default; loaded from
    /// the `hist/premarket` fetch (fetch-only) and drawn while active. See
    /// [`crate::chart::premarket::PremarketLevels`].
    PremarketHighLow,
}

impl Indicator for HeatmapIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        match market {
            MarketKind::Spot => &Self::FOR_SPOT,
            MarketKind::LinearPerps | MarketKind::InversePerps => &Self::FOR_PERPS,
        }
    }
}

impl HeatmapIndicator {
    // Indicator togglers on UI menus depend on these arrays.
    // Every variant needs to be in either SPOT, PERPS or both.
    /// Indicators that can be used with spot market tickers
    const FOR_SPOT: [HeatmapIndicator; 2] =
        [HeatmapIndicator::Volume, HeatmapIndicator::PremarketHighLow];
    /// Indicators that can be used with perpetual swap market tickers
    const FOR_PERPS: [HeatmapIndicator; 2] =
        [HeatmapIndicator::Volume, HeatmapIndicator::PremarketHighLow];
}

impl Display for HeatmapIndicator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HeatmapIndicator::Volume => write!(f, "Volume"),
            HeatmapIndicator::PremarketHighLow => write!(f, "Premarket H/L"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// Temporary workaround,
/// represents any indicator type in the UI
pub enum UiIndicator {
    Heatmap(HeatmapIndicator),
    Kline(KlineIndicator),
}

impl From<KlineIndicator> for UiIndicator {
    fn from(k: KlineIndicator) -> Self {
        UiIndicator::Kline(k)
    }
}

impl From<HeatmapIndicator> for UiIndicator {
    fn from(h: HeatmapIndicator) -> Self {
        UiIndicator::Heatmap(h)
    }
}
