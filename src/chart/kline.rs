use super::{
    Action, Basis, Chart, Interaction, Message, PlotConstants, PlotData, TEXT_SIZE, ViewState,
    indicator, request_fetch, scale::linear::PriceInfoLabel,
};
use crate::chart::indicator::kline::KlineIndicatorImpl;
use crate::connector::fetcher::{FetchRange, RequestHandler, is_trade_fetch_enabled};
use crate::{modal::pane::settings::study, style};
use data::aggr::ticks::TickAggr;
use data::aggr::time::TimeSeries;
use data::chart::indicator::{Indicator, KlineIndicator};
use data::chart::kline::{
    CandleVolumeProfile, ClusterKind, ClusterScaling, Config, FootprintStudy, FootprintSummary,
    KlineDataPoint, KlineTrades, NPoc, PointOfControl, VolumeProfileSide,
};
use data::chart::magnets::{MagnetSnapshot, MagnetStore, MagnetsConfig};
use data::chart::masterpice::{AuctionTick, LimiaresClassificacao, MasterpiceStore};
use data::chart::masterpice_profile::{SessionTick, VolumeDayStore, promover_pivo_se_necessario};
use data::chart::masterpice_v2::{AuctionTickV2, MasterpiceStoreV2, V2Config};
use data::chart::premarket::PremarketLevels;
use data::chart::{Autoscale, KlineChartKind, ViewConfig};

use data::config::theme::{darken, mix_color};
use data::util::abbr_large_numbers;

/// Opening row height for footprint clusters, in pixels: tall enough for the
/// buy/sell text, short enough to keep a useful span of the book on screen.
const FOOTPRINT_MIN_ROW_PX: f32 = 14.0;
const FOOTPRINT_MAX_ROW_PX: f32 = 60.0;
use exchange::unit::{Price, PriceStep, Qty};
use exchange::{Kline, TickerInfo, Trade, UnixMs};

use iced::task::Handle;
use iced::theme::palette::Extended;
use iced::widget::canvas::{self, Event, Geometry, LineDash, Path, Stroke};
use iced::{Alignment, Color, Element, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

use enum_map::EnumMap;
use std::time::Instant;

impl Chart for KlineChart {
    type IndicatorKind = KlineIndicator;

    fn state(&self) -> &ViewState {
        &self.chart
    }

    fn mut_state(&mut self) -> &mut ViewState {
        &mut self.chart
    }

    fn invalidate_crosshair(&mut self) {
        self.chart.cache.clear_crosshair();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.clear_crosshair_caches());
    }

    fn invalidate_all(&mut self) {
        self.invalidate(None);
    }

    fn view_indicators(&'_ self, enabled: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>> {
        let chart_state = self.state();
        let visible_region = chart_state.visible_region(chart_state.bounds.size());
        let (earliest, latest) = chart_state.interval_range(&visible_region);
        if earliest > latest {
            return vec![];
        }

        let data_labels_always_visible = self.visual_config.data_labels_always_visible;

        let market = chart_state.ticker_info.market_type();
        let mut elements = vec![];

        for selected_indicator in enabled {
            if !self.kind.allows_indicator(*selected_indicator)
                || !KlineIndicator::for_market(market).contains(selected_indicator)
            {
                continue;
            }
            if let Some(indi) = self.indicators[*selected_indicator].as_ref() {
                elements.push(indi.element(
                    chart_state,
                    data_labels_always_visible,
                    earliest..=latest,
                ));
            }
        }
        elements
    }

    fn visible_timerange(&self) -> Option<(u64, u64)> {
        let chart = self.state();
        let region = chart.visible_region(chart.bounds.size());

        if region.width == 0.0 {
            return None;
        }

        Some(chart.interval_range(&region))
    }

    fn interval_keys(&self) -> Option<Vec<u64>> {
        match &self.data_source {
            PlotData::TimeBased(_) => None,
            PlotData::TickBased(tick_aggr) => Some(
                tick_aggr
                    .datapoints
                    .iter()
                    .map(|dp| dp.kline.time.as_u64())
                    .collect(),
            ),
        }
    }

    fn autoscaled_coords(&self) -> Vector {
        let chart = self.state();
        let x_translation = match &self.kind {
            KlineChartKind::Footprint { .. } => {
                0.5 * (chart.bounds.width / chart.scaling) - (chart.cell_width / chart.scaling)
            }
            KlineChartKind::Candles => {
                0.5 * (chart.bounds.width / chart.scaling)
                    - (8.0 * chart.cell_width / chart.scaling)
            }
        };
        Vector::new(x_translation, chart.translation.y)
    }

    fn supports_fit_autoscaling(&self) -> bool {
        true
    }

    fn is_empty(&self) -> bool {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries.datapoints.is_empty(),
            PlotData::TickBased(tick_aggr) => tick_aggr.datapoints.is_empty(),
        }
    }
}

impl PlotConstants for KlineChart {
    fn min_scaling(&self) -> f32 {
        self.kind.min_scaling()
    }

    fn max_scaling(&self) -> f32 {
        self.kind.max_scaling()
    }

    fn max_cell_width(&self) -> f32 {
        self.kind.max_cell_width()
    }

    fn min_cell_width(&self) -> f32 {
        self.kind.min_cell_width()
    }

    fn max_cell_height(&self) -> f32 {
        self.kind.max_cell_height()
    }

    fn min_cell_height(&self) -> f32 {
        self.kind.min_cell_height()
    }

    fn default_cell_width(&self) -> f32 {
        self.kind.default_cell_width()
    }
}

pub struct KlineChart {
    chart: ViewState,
    data_source: PlotData<KlineDataPoint>,
    raw_trades: Vec<Trade>,
    indicators: EnumMap<KlineIndicator, Option<Box<dyn KlineIndicatorImpl>>>,
    fetching_trades: (bool, Option<Handle>),
    pub(crate) kind: KlineChartKind,
    request_handler: RequestHandler,
    study_configurator: study::Configurator<FootprintStudy>,
    last_tick: Instant,
    visual_config: Config,
    /// Pre-market auction levels for the "Premarket H/L" indicator (off by default).
    /// On by explicit toggle only; see [`PremarketLevels`].
    premarket: PremarketLevels,
    premarket_enabled: bool,
    theoric_enabled: bool,
    /// MAGNETS auction attraction levels (premarket book only, frozen at the
    /// auction close). On by explicit toggle only; see [`MagnetStore`].
    magnets: MagnetStore,
    magnets_enabled: bool,
    /// Masterpice: Nós de Leilão do premarket do dia (N/V/T, score, Alvo1 Pivô,
    /// Alvo2 Extremos, Amplitude Núcleo, Alvo3 Projeção). Estático após o close.
    /// Ver manual `manual_indicador_leilao_volume_profile_v2.md`.
    masterpice: MasterpiceStore,
    masterpice_enabled: bool,
    /// Masterpice Profile: volume tick a tick same-day em tempo real
    /// (POC/VAH/VAL + promoção de pivô sobre os alvos do Masterpice).
    masterpice_profile: VolumeDayStore,
    masterpice_profile_enabled: bool,
    /// Masterpice V2 (experimental): mesmo leilão, V = fila parada ×
    /// mudanças reais, classes por percentis. Convive com a V1 p/ comparação.
    masterpice_v2: MasterpiceStoreV2,
    masterpice_v2_enabled: bool,
    /// Session day (`YYYYMMDD`, UTC) of the last live `premarket/` tick seen.
    /// Set on every live ingest; used to tell "was open and listening during
    /// the auction" apart from "opened after the close" (bootstrap only).
    live_premarket_day: Option<i64>,
    /// Session day whose auction close already fired its one-shot
    /// `hist/premarket` refetch. After the close there is no more data for
    /// the day, so the refetch must happen exactly once per session.
    close_refetch_day: Option<i64>,
    /// Trailing-window buy/sell accumulator for the candlestick volume-profile
    /// overlay. OHLC-independent: fed by fetched history once, then live ticks.
    candle_profile: CandleVolumeProfile,
}

impl KlineChart {
    pub fn new(
        layout: ViewConfig,
        basis: Basis,
        step: PriceStep,
        klines_raw: &[Kline],
        raw_trades: Vec<Trade>,
        enabled_indicators: &[KlineIndicator],
        ticker_info: TickerInfo,
        kind: &KlineChartKind,
        visual_config: Option<Config>,
    ) -> Self {
        let visual_config = visual_config.unwrap_or_default();
        let kind = kind.clone();

        match basis {
            Basis::Time(interval) => {
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, klines_raw)
                    .with_trades(&raw_trades);

                let base_price_y = timeseries.base_price();
                let latest_x = timeseries
                    .latest_timestamp()
                    .map_or(0, |timestamp| timestamp.as_u64());
                let (scale_high, scale_low) = timeseries.price_scale({
                    match &kind {
                        KlineChartKind::Footprint { .. } => 12,
                        KlineChartKind::Candles => 60,
                    }
                });

                let low_rounded = scale_low.round_to_side_step(true, step);
                let high_rounded = scale_high.round_to_side_step(false, step);

                let y_ticks = Price::steps_between_inclusive(low_rounded, high_rounded, step)
                    .map(|n| n.saturating_sub(1))
                    .unwrap_or(1)
                    .max(1) as f32;

                let cell_width = match &kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match &kind {
                    // One row per price step, opened at a height the cluster text
                    // actually fits in. Fitting a fixed 800px over however many steps the
                    // initial range happens to span makes the opening zoom depend on the
                    // instrument's tick: a 0.00005-step pair and a 0.5-step one produce
                    // wildly different row heights from the same expression, and the
                    // finer one opens unreadably squashed. Clamping pins the opening zoom
                    // to the asset's step instead; the user can still zoom freely.
                    KlineChartKind::Footprint { .. } => {
                        (800.0 / y_ticks).clamp(FOOTPRINT_MIN_ROW_PX, FOOTPRINT_MAX_ROW_PX)
                    }
                    KlineChartKind::Candles => 200.0 / y_ticks,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits.clone(),
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );
                chart.base_price_y = base_price_y;
                chart.latest_x = latest_x;

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (chart.cell_width / chart.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (8.0 * chart.cell_width / chart.scaling)
                    }
                };
                chart.translation.x = x_translation;

                let data_source = PlotData::TimeBased(timeseries);

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    // Roof-level overlay, never a sub-panel: keep its EnumMap slot
                    // empty so `view_indicators` does not render a stray blank panel.
                    if i == KlineIndicator::PremarketHighLow
                        || i == KlineIndicator::TheoricTargets
                        || i == KlineIndicator::Magnets
                        || i == KlineIndicator::Masterpice
                        || i == KlineIndicator::MasterpiceProfile
                        || i == KlineIndicator::MasterpiceV2
                    {
                        continue;
                    }
                    if !kind.allows_indicator(i) {
                        continue;
                    }
                    let mut indi = indicator::kline::make_empty(i);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                let mut kline_chart = KlineChart {
                    chart,
                    visual_config,
                    data_source,
                    raw_trades,
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::default(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                    premarket: PremarketLevels::new(),
                    premarket_enabled: enabled_indicators
                        .contains(&KlineIndicator::PremarketHighLow),
                    theoric_enabled: enabled_indicators.contains(&KlineIndicator::TheoricTargets),
                    magnets: MagnetStore::new(MagnetsConfig::default()),
                    magnets_enabled: enabled_indicators.contains(&KlineIndicator::Magnets),
                    masterpice: MasterpiceStore::new(LimiaresClassificacao::default()),
                    masterpice_enabled: enabled_indicators.contains(&KlineIndicator::Masterpice),
                    masterpice_profile: VolumeDayStore::new(0.5, 0.70),
                    masterpice_profile_enabled: enabled_indicators
                        .contains(&KlineIndicator::MasterpiceProfile),
                    masterpice_v2: MasterpiceStoreV2::new(V2Config::default()),
                    masterpice_v2_enabled: enabled_indicators
                        .contains(&KlineIndicator::MasterpiceV2),
                    live_premarket_day: None,
                    close_refetch_day: None,
                    candle_profile: CandleVolumeProfile::default(),
                };
                prune_tape(&mut kline_chart.raw_trades);
                kline_chart.chart.sync_time_axis(&kline_chart.data_source);

                kline_chart
            }
            Basis::Tick(interval) => {
                let cell_width = match &kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match &kind {
                    KlineChartKind::Footprint { .. } => 90.0,
                    KlineChartKind::Candles => 8.0,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits.clone(),
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (chart.cell_width / chart.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (8.0 * chart.cell_width / chart.scaling)
                    }
                };
                chart.translation.x = x_translation;

                let data_source = PlotData::TickBased(TickAggr::new(interval, step, &[]));

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    // Roof-level overlay, never a sub-panel: keep its EnumMap slot
                    // empty so `view_indicators` does not render a stray blank panel.
                    if i == KlineIndicator::PremarketHighLow
                        || i == KlineIndicator::TheoricTargets
                        || i == KlineIndicator::Magnets
                        || i == KlineIndicator::Masterpice
                        || i == KlineIndicator::MasterpiceProfile
                        || i == KlineIndicator::MasterpiceV2
                    {
                        continue;
                    }
                    if !kind.allows_indicator(i) {
                        continue;
                    }
                    let mut indi = indicator::kline::make_empty(i);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    visual_config,
                    data_source,
                    raw_trades,
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::default(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                    premarket: PremarketLevels::new(),
                    premarket_enabled: enabled_indicators
                        .contains(&KlineIndicator::PremarketHighLow),
                    theoric_enabled: enabled_indicators.contains(&KlineIndicator::TheoricTargets),
                    magnets: MagnetStore::new(MagnetsConfig::default()),
                    magnets_enabled: enabled_indicators.contains(&KlineIndicator::Magnets),
                    masterpice: MasterpiceStore::new(LimiaresClassificacao::default()),
                    masterpice_enabled: enabled_indicators.contains(&KlineIndicator::Masterpice),
                    masterpice_profile: VolumeDayStore::new(0.5, 0.70),
                    masterpice_profile_enabled: enabled_indicators
                        .contains(&KlineIndicator::MasterpiceProfile),
                    masterpice_v2: MasterpiceStoreV2::new(V2Config::default()),
                    masterpice_v2_enabled: enabled_indicators
                        .contains(&KlineIndicator::MasterpiceV2),
                    live_premarket_day: None,
                    close_refetch_day: None,
                    candle_profile: CandleVolumeProfile::default(),
                }
            }
        }
    }

    pub fn update_latest_kline(&mut self, kline: &Kline) {
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_klines(&[*kline]);

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(&[*kline], &self.data_source));

                let chart = self.mut_state();

                if kline.time.as_u64() > chart.latest_x {
                    chart.latest_x = kline.time.as_u64();
                }

                chart.last_price = Some(PriceInfoLabel::new(kline.close, kline.open));
            }
            PlotData::TickBased(_) => {}
        }
    }

    pub fn kind(&self) -> &KlineChartKind {
        &self.kind
    }

    fn fetch_missing_data(&mut self) -> Option<Action> {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                if timeseries.datapoints.is_empty() {
                    // No start bound: we don't know yet where real data begins (the
                    // market may be closed right now), so let the fetch fall back to
                    // persist's retention window instead of guessing `now() -
                    // 450*timeframe_ms`, which lands entirely inside a weekend/holiday
                    // gap whenever "now" itself sits past the last close.
                    //
                    // `latest` is floored to the timeframe grid: without this every
                    // 1s tick mints a distinct `(None, now)` range that defeats the
                    // request dedup and spawns a fresh fetch task per second while
                    // the series stays empty.
                    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                    let step = timeseries.interval.to_milliseconds().max(1);
                    let latest = now_ms - now_ms % step;

                    let range = FetchRange::Kline(None, UnixMs::new(latest));
                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        return Some(action);
                    }
                }

                let (visible_earliest, visible_latest) = self.visible_timerange()?;
                let (kline_earliest, _) = timeseries.timerange();
                let visible_earliest_ms = UnixMs::new(visible_earliest);
                let visible_latest_ms = UnixMs::new(visible_latest);
                let visible_span = visible_latest.saturating_sub(visible_earliest);
                let prefetch_earliest = visible_earliest.saturating_sub(visible_span);

                // priority 1, initial klines for visible range
                if visible_earliest_ms < kline_earliest {
                    let range =
                        FetchRange::Kline(Some(UnixMs::new(prefetch_earliest)), kline_earliest);

                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        return Some(action);
                    }
                }

                // priority 2, trades: footprint fills per-bucket gaps; the candle
                // volume-profile overlay bootstraps its trailing time window once
                // (full window on enable, backwards delta when hours grow) and
                // then lives on realtime ticks like the footprint does.
                // Masterpice Profile também precisa de trades (same-day, tick a
                // tick) para o POC/VAH/VAL em tempo real.
                let wants_trades = match self.kind {
                    KlineChartKind::Footprint { .. } => true,
                    KlineChartKind::Candles => {
                        self.candle_profile_enabled() || self.masterpice_profile_enabled
                    }
                };
                if wants_trades
                    && !self.fetching_trades.0
                    && is_trade_fetch_enabled()
                    && let Some((fetch_from, fetch_to)) =
                        self.trade_fetch_range(timeseries, visible_earliest_ms, visible_latest_ms)
                {
                    let range = FetchRange::Trades(fetch_from, fetch_to);
                    if let Some(action) = request_fetch(&mut self.request_handler, range) {
                        self.fetching_trades = (true, None);
                        return Some(action);
                    }
                }
            }
            PlotData::TickBased(_) => {
                // TODO: implement trade fetch
            }
        }

        None
    }

    pub fn reset_request_handler(&mut self) {
        self.request_handler = RequestHandler::default();
        self.fetching_trades = (false, None);
    }

    /// Whether the candle volume-profile overlay is active (Candles kind + toggle on).
    fn candle_profile_enabled(&self) -> bool {
        matches!(self.kind, KlineChartKind::Candles) && self.visual_config.show_volume_profile
    }

    fn profile_window_ms(&self) -> u64 {
        u64::from(self.visual_config.volume_profile_history_hours).saturating_mul(3_600_000)
    }

    /// Trade range for footprint gaps; for the candle volume-profile overlay,
    /// cold start bootstraps the full trailing window once, a larger window
    /// fetches only the backwards delta, and afterwards live ticks take over
    /// (a smaller window just prunes locally — no fetch).
    fn trade_fetch_range(
        &self,
        timeseries: &TimeSeries<KlineDataPoint>,
        visible_earliest: UnixMs,
        visible_latest: UnixMs,
    ) -> Option<(UnixMs, UnixMs)> {
        if matches!(self.kind, KlineChartKind::Footprint { .. }) {
            return timeseries.suggest_trade_fetch_range(visible_earliest, visible_latest);
        }
        // Masterpice Profile: bootstrap same-day uma vez (início do dia UTC
        // até agora); depois vive de ticks live. Independente do trailing
        // window do candle volume-profile.
        if self.masterpice_profile_enabled && self.masterpice_profile.is_empty() {
            let now = chrono::Utc::now().timestamp_millis().max(0) as u64;
            let day_start = now - (now % 86_400_000);
            return Some((UnixMs::new(day_start), UnixMs::new(now)));
        }
        if !self.candle_profile_enabled() {
            return None;
        }
        let window_ms = self.profile_window_ms();
        if window_ms == 0 {
            return None;
        }
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let window_from = now.saturating_sub(window_ms);
        match self.candle_profile.fetched_from() {
            // First load: full trailing window, then realtime.
            None => Some((UnixMs::new(window_from), UnixMs::new(now))),
            // Window grew past what was fetched: backwards delta only.
            Some(fetched_from) if window_from < fetched_from.as_u64() => Some((
                UnixMs::new(window_from),
                UnixMs::new(fetched_from.as_u64().saturating_sub(1)),
            )),
            _ => None,
        }
    }

    pub fn raw_trades(&self) -> Vec<Trade> {
        self.raw_trades.clone()
    }

    pub fn set_handle(&mut self, handle: Handle) {
        self.fetching_trades.1 = Some(handle);
    }

    pub fn tick_size(&self) -> PriceStep {
        self.chart.tick_size
    }

    pub fn study_configurator(&self) -> &study::Configurator<FootprintStudy> {
        &self.study_configurator
    }

    pub fn update_study_configurator(&mut self, message: study::Message<FootprintStudy>) {
        let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        else {
            return;
        };

        match self.study_configurator.update(message) {
            Some(study::Action::ToggleStudy(study, is_selected)) => {
                if is_selected {
                    let already_exists = studies.iter().any(|s| s.is_same_type(&study));
                    if !already_exists {
                        studies.push(study);
                    }
                } else {
                    studies.retain(|s| !s.is_same_type(&study));
                }
            }
            Some(study::Action::ConfigureStudy(study)) => {
                if let Some(existing_study) = studies.iter_mut().find(|s| s.is_same_type(&study)) {
                    *existing_study = study;
                }
            }
            None => {}
        }

        self.invalidate(None);
    }

    pub fn chart_layout(&self) -> ViewConfig {
        self.chart.layout()
    }

    pub fn visual_config(&self) -> Config {
        self.visual_config
    }

    pub fn set_visual_config(&mut self, visual_config: Config) {
        let was_enabled =
            matches!(self.kind, KlineChartKind::Candles) && self.visual_config.show_volume_profile;
        let will_enable =
            matches!(self.kind, KlineChartKind::Candles) && visual_config.show_volume_profile;
        let hours_changed = visual_config.volume_profile_history_hours
            != self.visual_config.volume_profile_history_hours;
        self.visual_config = visual_config;
        if will_enable && (!was_enabled || hours_changed) {
            // Enabled or window changed: allow a fresh bootstrap fetch (full
            // window on enable, backwards delta when hours grow); a smaller
            // window just prunes locally. Live ticks continue afterwards.
            let step = self.chart.tick_size;
            self.candle_profile
                .prune(step, self.visual_config.volume_profile_history_hours);
            self.fetching_trades = (false, None);
        } else if !will_enable && was_enabled {
            self.candle_profile.clear();
            self.fetching_trades = (false, None);
        }
        self.chart.cache.clear_all();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.clear_all_caches());
    }

    pub fn set_cluster_kind(&mut self, new_kind: ClusterKind) {
        if let KlineChartKind::Footprint {
            ref mut clusters, ..
        } = self.kind
        {
            *clusters = new_kind;
        }

        self.invalidate(None);
    }

    pub fn set_cluster_scaling(&mut self, new_scaling: ClusterScaling) {
        if let KlineChartKind::Footprint {
            ref mut scaling, ..
        } = self.kind
        {
            *scaling = new_scaling;
        }

        self.invalidate(None);
    }

    pub fn basis(&self) -> Basis {
        self.chart.basis
    }

    pub fn change_tick_size(&mut self, new_step: PriceStep) {
        let chart = self.mut_state();

        chart.cell_height *= (new_step.units as f32) / (chart.tick_size.units as f32);
        chart.tick_size = new_step;

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                tick_aggr.change_tick_size(new_step, &self.raw_trades);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.change_tick_size(new_step, &self.raw_trades);
            }
        }

        if self.candle_profile_enabled() {
            let hours = self.visual_config.volume_profile_history_hours;
            self.candle_profile.rebuild(new_step, hours);
        }

        // Masterpice Profile re-bina o dia corrente na nova grade.
        if self.masterpice_profile_enabled {
            let ticks: Vec<SessionTick> = self
                .raw_trades
                .iter()
                .map(|t| SessionTick {
                    time_ms: t.time.as_u64() as i64,
                    price: t.price.to_f64(),
                    qty: t.qty.to_f64(),
                })
                .collect();
            self.masterpice_profile
                .set_tick_size(new_step.to_f64_lossy(), &ticks);
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_ticksize_change(&self.data_source));

        self.invalidate(None);
    }

    pub fn set_basis(&mut self, new_basis: Basis) -> Option<Action> {
        let previous_basis = self.chart.basis;

        self.chart.last_price = None;
        self.chart.basis = new_basis;

        match new_basis {
            Basis::Time(interval) => {
                if matches!(previous_basis, Basis::Tick(_)) {
                    self.raw_trades.clear();
                };

                let step = self.chart.tick_size;
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, &[]);
                self.data_source = PlotData::TimeBased(timeseries);
            }
            Basis::Tick(tick_count) => {
                let trades = if matches!(previous_basis, Basis::Tick(_)) {
                    &self.raw_trades
                } else {
                    self.raw_trades.clear();
                    &vec![]
                };

                let step = self.chart.tick_size;
                let tick_aggr = TickAggr::new(tick_count, step, trades);
                self.data_source = PlotData::TickBased(tick_aggr);
            }
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_basis_change(&self.data_source));

        self.reset_request_handler();
        self.invalidate(Some(Instant::now()))
    }

    pub fn studies(&self) -> Option<Vec<FootprintStudy>> {
        match &self.kind {
            KlineChartKind::Footprint { studies, .. } => Some(studies.clone()),
            _ => None,
        }
    }

    pub fn set_studies(&mut self, new_studies: Vec<FootprintStudy>) {
        if let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        {
            *studies = new_studies;
        }

        self.invalidate(None);
    }

    pub fn insert_trades(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        // Every print builds candles/footprint — auction/premarket included, never dropped.
        self.raw_trades.extend_from_slice(buffer);
        prune_tape(&mut self.raw_trades);

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                let old_dp_len = tick_aggr.datapoints.len();
                tick_aggr.insert_trades(buffer);

                if let Some(last_dp) = tick_aggr.datapoints.last() {
                    self.chart.last_price =
                        Some(PriceInfoLabel::new(last_dp.kline.close, last_dp.kline.open));
                } else {
                    self.chart.last_price = None;
                }

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_trades(buffer, old_dp_len, &self.data_source));

                self.invalidate(None);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_trades_existing_buckets(buffer);

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_trades(buffer, 0, &self.data_source));

                self.invalidate(None);
            }
        }

        // Live leg of the candle volume-profile overlay: every realtime tick
        // accumulates, the trailing window prunes — same pattern as footprint's
        // fetch-then-realtime, but OHLC-independent.
        if self.candle_profile_enabled() {
            let step = self.chart.tick_size;
            let hours = self.visual_config.volume_profile_history_hours;
            self.candle_profile.insert(buffer, step);
            self.candle_profile.prune(step, hours);
        }
        // Live leg do Masterpice Profile: tick a tick same-day (POC/VAH/VAL).
        if self.masterpice_profile_enabled && !buffer.is_empty() {
            let ticks: Vec<SessionTick> = buffer
                .iter()
                .map(|t| SessionTick {
                    time_ms: t.time.as_u64() as i64,
                    price: t.price.to_f64(),
                    qty: t.qty.to_f64(),
                })
                .collect();
            self.update_masterpice_profile(&ticks);
        }
    }

    /// Folds a backfilled batch into the tape, then rebuilds the aggregation from it.
    ///
    /// A backfill routinely returns prints the chart already holds: the source serves the
    /// newest window it has rather than exactly the window asked for, and the request
    /// handler retries a completed range after its cooldown. Adding those prints on top
    /// of the ones already folded in inflates every bucket they land in — a 5-minute
    /// candle that really traded ~2k contracts creeps towards six figures over a session
    /// of retries, which is what "the footprint is counting every tick ever seen" looks
    /// like on screen. Folding through the tape makes a re-delivered batch a no-op.
    pub fn insert_raw_trades(&mut self, raw_trades: Vec<Trade>, is_batches_done: bool) {
        let old_dp_len = match &self.data_source {
            PlotData::TickBased(tick_aggr) => tick_aggr.datapoints.len(),
            PlotData::TimeBased(_) => 0,
        };

        merge_fetched_into_tape(&mut self.raw_trades, &raw_trades);
        self.refold_raw_trades();

        // History leg of the candle volume-profile overlay: backfilled batches
        // (full window on enable, backwards delta when hours grow). Redelivery
        // is a no-op via `insert_batch`; live ticks continue afterwards.
        if self.candle_profile_enabled() && !raw_trades.is_empty() {
            let step = self.chart.tick_size;
            let hours = self.visual_config.volume_profile_history_hours;
            self.candle_profile.insert_batch(&raw_trades, step);
            self.candle_profile.prune(step, hours);
        }
        // History leg do Masterpice Profile: backfill same-day sem duplicar.
        if self.masterpice_profile_enabled && !raw_trades.is_empty() {
            let ticks: Vec<SessionTick> = raw_trades
                .iter()
                .map(|t| SessionTick {
                    time_ms: t.time.as_u64() as i64,
                    price: t.price.to_f64(),
                    qty: t.qty.to_f64(),
                })
                .collect();
            if self.masterpice_profile.insert_batch(&ticks) {
                self.chart.cache.main.clear();
                self.chart.cache.clear_crosshair();
            }
        }

        if let PlotData::TickBased(tick_aggr) = &self.data_source {
            self.chart.last_price = tick_aggr
                .datapoints
                .last()
                .map(|last_dp| PriceInfoLabel::new(last_dp.kline.close, last_dp.kline.open));
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_insert_trades(&raw_trades, old_dp_len, &self.data_source));

        if is_batches_done {
            self.fetching_trades = (false, None);
        }

        self.invalidate(None);
    }

    /// Rebuilds the whole aggregation from the tape. Every path that mutates the tape
    /// outside the live feed goes through here, so folding is idempotent: the buckets
    /// hold exactly the trades the tape holds, however many times a range was fetched.
    fn refold_raw_trades(&mut self) {
        let tick_size = self.chart.tick_size;

        match &mut self.data_source {
            PlotData::TickBased(tick_aggr) => {
                tick_aggr.change_tick_size(tick_size, &self.raw_trades);
            }
            PlotData::TimeBased(timeseries) => {
                timeseries.clear_trades();
                timeseries.insert_trades_existing_buckets(&self.raw_trades);
            }
        }
    }

    pub fn insert_hist_klines(&mut self, req_id: uuid::Uuid, klines_raw: &[Kline]) {
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_klines(klines_raw);
                // Backfilled klines can open buckets the tape already has trades for, so
                // the tape is replayed — but only after clearing, or every bucket already
                // folded would count its trades again on every kline fetch.
                timeseries.clear_trades();
                timeseries.insert_trades_existing_buckets(&self.raw_trades);

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(klines_raw, &self.data_source));

                if klines_raw.is_empty() {
                    self.request_handler
                        .mark_failed(req_id, "No data received".to_string());
                } else {
                    self.request_handler.mark_completed(req_id);
                }
                self.invalidate(None);
            }
            PlotData::TickBased(_) => {}
        }
    }

    fn calc_qty_scales(
        &self,
        earliest: u64,
        latest: u64,
        highest: Price,
        lowest: Price,
        step: PriceStep,
        cluster_kind: ClusterKind,
    ) -> f64 {
        let rounded_highest = highest.round_to_side_step(false, step).add_steps(1, step);
        let rounded_lowest = lowest.round_to_side_step(true, step).add_steps(-1, step);

        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries
                .max_qty_ts_range(
                    cluster_kind,
                    UnixMs::new(earliest),
                    UnixMs::new(latest),
                    rounded_highest,
                    rounded_lowest,
                )
                .to_f64(),
            PlotData::TickBased(tick_aggr) => {
                let earliest = earliest as usize;
                let latest = latest as usize;

                tick_aggr
                    .max_qty_idx_range(
                        cluster_kind,
                        earliest,
                        latest,
                        rounded_highest,
                        rounded_lowest,
                    )
                    .to_f64()
            }
        }
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
        // Before anything reads a coordinate: bars may have been appended or backfilled
        // since the last frame, which moves where the market's closures sit.
        self.chart.sync_time_axis(&self.data_source);

        let chart = &mut self.chart;

        if let Some(autoscale) = chart.layout.autoscale {
            match autoscale {
                super::Autoscale::CenterLatest => {
                    let x_translation = match &self.kind {
                        KlineChartKind::Footprint { .. } => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (chart.cell_width / chart.scaling)
                        }
                        KlineChartKind::Candles => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (8.0 * chart.cell_width / chart.scaling)
                        }
                    };
                    chart.translation.x = x_translation;

                    let calculate_target_y = |kline: exchange::Kline| -> f32 {
                        let y_low = chart.price_to_y(kline.low);
                        let y_high = chart.price_to_y(kline.high);
                        let y_close = chart.price_to_y(kline.close);

                        let mut target_y_translation = -(y_low + y_high) / 2.0;

                        if chart.bounds.height > f32::EPSILON && chart.scaling > f32::EPSILON {
                            let visible_half_height = (chart.bounds.height / chart.scaling) / 2.0;

                            let view_center_y_centered = -target_y_translation;

                            let visible_y_top = view_center_y_centered - visible_half_height;
                            let visible_y_bottom = view_center_y_centered + visible_half_height;

                            let padding = chart.cell_height;

                            if y_close < visible_y_top {
                                target_y_translation = -(y_close - padding + visible_half_height);
                            } else if y_close > visible_y_bottom {
                                target_y_translation = -(y_close + padding - visible_half_height);
                            }
                        }
                        target_y_translation
                    };

                    chart.translation.y = self.data_source.latest_y_midpoint(calculate_target_y);
                }
                super::Autoscale::FitToVisible => {
                    let visible_region = chart.visible_region(chart.bounds.size());
                    let (start_interval, end_interval) = chart.interval_range(&visible_region);

                    if let Some((lowest, highest)) = self
                        .data_source
                        .visible_price_range(start_interval, end_interval)
                    {
                        let chart_height = chart.bounds.height;
                        let tick_size = chart.tick_size.to_f32_lossy();

                        if chart_height > f32::EPSILON && tick_size > 0.0 {
                            let (fit_lowest, fit_highest) =
                                if let KlineChartKind::Footprint { .. } = self.kind {
                                    if let Some((footprint_low, footprint_high)) = self
                                        .data_source
                                        .visible_footprint_price_range(start_interval, end_interval)
                                    {
                                        let half_tick = tick_size * 0.5;
                                        (
                                            footprint_low.to_f32_lossy() - half_tick,
                                            footprint_high.to_f32_lossy() + half_tick,
                                        )
                                    } else {
                                        (lowest, highest)
                                    }
                                } else {
                                    (lowest, highest)
                                };

                            let visible_span = (fit_highest - fit_lowest).max(tick_size);
                            let base_padding = visible_span * 0.05; // 5% padding on top and bottom

                            let mut top_padding = base_padding;
                            let mut bottom_padding = base_padding;

                            if let KlineChartKind::Footprint { clusters, .. } = self.kind {
                                let provisional_span = visible_span + top_padding + bottom_padding;
                                if provisional_span > 0.0 {
                                    let provisional_cell_height =
                                        (chart_height * tick_size) / provisional_span;

                                    let outer_padding = price_padding_from_pixels(
                                        provisional_cell_height,
                                        tick_size,
                                    );

                                    top_padding += outer_padding;
                                    bottom_padding += outer_padding;

                                    if self.visual_config.show_footprint_summary {
                                        bottom_padding =
                                            bottom_padding.max(footprint_summary_padding(
                                                provisional_cell_height,
                                                chart.scaling,
                                                chart.cell_width,
                                                tick_size,
                                                clusters,
                                            ));
                                    }
                                }
                            }

                            let padded_span = visible_span + top_padding + bottom_padding;
                            if padded_span > 0.0 {
                                chart.cell_height = (chart_height * tick_size) / padded_span;
                                chart.base_price_y = Price::from_f32(fit_highest + top_padding);
                                chart.translation.y = -chart_height / 2.0;
                            }
                        }
                    }
                }
            }
        }

        chart.cache.clear_all();
        for indi in self.indicators.values_mut().filter_map(Option::as_mut) {
            indi.clear_all_caches();
        }

        if let Some(t) = now {
            self.last_tick = t;
            self.fetch_missing_data()
        } else {
            None
        }
    }

    pub fn toggle_indicator(&mut self, indicator: KlineIndicator) {
        if indicator == KlineIndicator::TheoricTargets {
            if !matches!(self.kind, KlineChartKind::Candles) {
                return;
            }
            self.theoric_enabled = !self.theoric_enabled;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
            return;
        }
        if indicator == KlineIndicator::Magnets {
            // Roof-level magnet lines overlay, candle charts only.
            if !matches!(self.kind, KlineChartKind::Candles) {
                return;
            }
            self.magnets_enabled = !self.magnets_enabled;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
            return;
        }
        if indicator == KlineIndicator::Masterpice {
            // Roof-level Masterpice overlay (Nós de Leilão), candle charts only.
            if !matches!(self.kind, KlineChartKind::Candles) {
                return;
            }
            self.masterpice_enabled = !self.masterpice_enabled;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
            return;
        }
        if indicator == KlineIndicator::MasterpiceProfile {
            // Roof-level Masterpice Profile overlay, candle charts only.
            if !matches!(self.kind, KlineChartKind::Candles) {
                return;
            }
            let will_enable = !self.masterpice_profile_enabled;
            self.masterpice_profile_enabled = will_enable;
            if !will_enable {
                self.masterpice_profile.clear();
            } else {
                // Bootstrap frio a partir da fita atual (só trades do dia).
                let ticks: Vec<SessionTick> = self
                    .raw_trades
                    .iter()
                    .map(|t| SessionTick {
                        time_ms: t.time.as_u64() as i64,
                        price: t.price.to_f64(),
                        qty: t.qty.to_f64(),
                    })
                    .collect();
                self.masterpice_profile.insert_batch(&ticks);
            }
            self.fetching_trades = (false, None);
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
            return;
        }
        if indicator == KlineIndicator::MasterpiceV2 {
            // Roof-level Masterpice V2 overlay, candle charts only.
            if !matches!(self.kind, KlineChartKind::Candles) {
                return;
            }
            self.masterpice_v2_enabled = !self.masterpice_v2_enabled;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
            return;
        }
        if indicator == KlineIndicator::PremarketHighLow {
            // Roof-level wall overlay, candle charts only — never on footprint.
            if !matches!(self.kind, KlineChartKind::Candles) {
                return;
            }
            self.premarket_enabled = !self.premarket_enabled;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
            return;
        }

        if !self.kind.allows_indicator(indicator) {
            return;
        }

        let prev_indi_count = self.indicators.values().filter(|v| v.is_some()).count();

        if self.indicators[indicator].is_some() {
            self.indicators[indicator] = None;
        } else {
            let mut box_indi = indicator::kline::make_empty(indicator);
            box_indi.rebuild_from_source(&self.data_source);
            self.indicators[indicator] = Some(box_indi);
        }

        if let Some(main_split) = self.chart.layout.splits.first() {
            let current_indi_count = self.indicators.values().filter(|v| v.is_some()).count();
            self.chart.layout.splits = data::util::calc_panel_splits(
                *main_split,
                current_indi_count,
                Some(prev_indi_count),
            );
        }
    }

    pub fn premarket_enabled(&self) -> bool {
        self.premarket_enabled
    }

    /// Take the current premarket levels out of the chart (leaving it empty).
    /// Used when the chart object itself is replaced (initial kline fetch) so
    /// the bootstrap isn't lost in the swap — the one-shot fetch won't re-fire.
    pub fn take_premarket_levels(&mut self) -> PremarketLevels {
        std::mem::take(&mut self.premarket)
    }

    /// Restore premarket levels taken from a replaced chart.
    pub fn restore_premarket_levels(&mut self, levels: PremarketLevels) {
        if !levels.is_empty() {
            self.premarket = levels;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Take the current magnets store out of the chart (leaving it empty).
    pub fn take_magnet_store(&mut self) -> MagnetStore {
        std::mem::take(&mut self.magnets)
    }

    /// Restore a magnets store taken from a replaced chart.
    pub fn restore_magnet_store(&mut self, store: MagnetStore) {
        if !store.is_empty() {
            self.magnets = store;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Bootstrap the premarket levels from persisted history (one-shot on pane open
    /// / indicator enable).
    pub fn bootstrap_premarket<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = (i64, f64, f64, f64, f64, f64, f64)>,
    {
        let before = self.premarket.len();
        self.premarket.bootstrap(points);
        if self.premarket.len() != before {
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Whether the premarket levels have been bootstrapped yet, so the one-shot
    /// `hist/premarket` bootstrap fetch is fired only before the live stream takes over.
    pub fn is_premarket_bootstrapped(&self) -> bool {
        !self.premarket.is_empty()
    }

    /// Feed a live premarket tick; re-renders when it moves levels or stats.
    /// Every accepted live tick replots (cache cleared on version change);
    /// the tick's day is also recorded so the auction-close refetch can tell
    /// "was open and listening during the auction" from "opened after it".
    pub fn update_premarket(
        &mut self,
        ts: i64,
        price: f64,
        size: f64,
        bid_vol: f64,
        ask_vol: f64,
        bid_at: f64,
        ask_at: f64,
    ) {
        let before = self.premarket.version();
        self.premarket
            .add_point(ts, price, size, bid_vol, ask_vol, bid_at, ask_at);
        if self.premarket.version() != before {
            self.live_premarket_day = Some(PremarketLevels::day_key(ts));
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Bootstrap the MAGNETS store from persisted premarket book history
    /// (one-shot on pane open / indicator enable).
    pub fn bootstrap_magnets<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = MagnetSnapshot>,
    {
        let before = self.magnets.version();
        self.magnets.bootstrap(points);
        if self.magnets.version() != before {
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Feed a live premarket book snapshot; re-renders when magnets move.
    /// Same live-day bookkeeping as [`Self::update_premarket`].
    pub fn update_magnets(&mut self, snap: MagnetSnapshot) {
        let ts = snap.ts;
        let before = self.magnets.version();
        self.magnets.add_point(snap);
        if self.magnets.version() != before {
            self.live_premarket_day = Some(PremarketLevels::day_key(ts));
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Whether the magnets store has been bootstrapped yet.
    pub fn is_magnets_bootstrapped(&self) -> bool {
        !self.magnets.is_empty()
    }

    /// Bootstrap do Masterpice a partir do histórico persistido do leilão
    /// (one-shot ao abrir o pane / habilitar o indicador). Mesma fonte do
    /// Magnets (`hist/premarket`), cálculo próprio (N/V/T do manual).
    pub fn bootstrap_masterpice<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = AuctionTick>,
    {
        let before = self.masterpice.version();
        self.masterpice.bootstrap(points);
        if self.masterpice.version() != before {
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Alimenta o Masterpice com um tick live do `premarket/`.
    pub fn update_masterpice(&mut self, tick: AuctionTick) {
        let before = self.masterpice.version();
        self.masterpice.add_point(tick);
        if self.masterpice.version() != before {
            self.live_premarket_day = Some(data::chart::masterpice::day_key(tick.ts));
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    pub fn is_masterpice_bootstrapped(&self) -> bool {
        !self.masterpice.is_empty()
    }

    pub fn take_masterpice_store(&mut self) -> MasterpiceStore {
        std::mem::take(&mut self.masterpice)
    }

    pub fn restore_masterpice_store(&mut self, store: MasterpiceStore) {
        if !store.is_empty() {
            self.masterpice = store;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Alimenta o VolumeDayStore do Masterpice Profile tick a tick (live).
    /// Só trades do dia corrente entram (rollover interno ao store).
    pub fn update_masterpice_profile(&mut self, ticks: &[SessionTick]) {
        if !self.masterpice_profile_enabled || ticks.is_empty() {
            return;
        }
        if self.masterpice_profile.insert_ticks(ticks) {
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    pub fn take_masterpice_profile_store(&mut self) -> VolumeDayStore {
        std::mem::take(&mut self.masterpice_profile)
    }

    pub fn restore_masterpice_profile_store(&mut self, store: VolumeDayStore) {
        if !store.is_empty() {
            self.masterpice_profile = store;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Bootstrap do Masterpice V2 a partir do histórico persistido do leilão,
    /// COM a fila parada de cada snapshot (base do V da V2).
    pub fn bootstrap_masterpice_v2<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = AuctionTickV2>,
    {
        let before = self.masterpice_v2.version();
        self.masterpice_v2.bootstrap(points);
        if self.masterpice_v2.version() != before {
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Alimenta a V2 com um tick live do `premarket/` (com fila parada).
    pub fn update_masterpice_v2(&mut self, tick: AuctionTickV2) {
        let before = self.masterpice_v2.version();
        self.masterpice_v2.add_point(tick);
        if self.masterpice_v2.version() != before {
            self.live_premarket_day = Some(data::chart::masterpice::day_key(tick.ts));
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    pub fn is_masterpice_v2_bootstrapped(&self) -> bool {
        !self.masterpice_v2.is_empty()
    }

    pub fn take_masterpice_v2_store(&mut self) -> MasterpiceStoreV2 {
        std::mem::take(&mut self.masterpice_v2)
    }

    pub fn restore_masterpice_v2_store(&mut self, store: MasterpiceStoreV2) {
        if !store.is_empty() {
            self.masterpice_v2 = store;
            self.chart.cache.main.clear();
            self.chart.cache.clear_crosshair();
        }
    }

    /// Currently plotted premarket session day (`YYYYMMDD`, UTC), if any.
    pub fn premarket_session_day(&self) -> Option<i64> {
        self.premarket.session_day()
    }

    /// Auction close: timestamp of the last accepted premarket print, if any.
    pub fn premarket_close_ts(&self) -> Option<i64> {
        self.premarket.close_ts()
    }

    /// Whether any premarket-fed roof overlay is toggled on for this chart.
    pub fn premarket_overlay_on(&self) -> bool {
        self.premarket_enabled
            || self.theoric_enabled
            || self.magnets_enabled
            || self.masterpice_enabled
            || self.masterpice_profile_enabled
            || self.masterpice_v2_enabled
    }

    /// Session day of the last accepted live `premarket/` tick, if this
    /// session was open and listening while the auction ran.
    pub fn live_premarket_day(&self) -> Option<i64> {
        self.live_premarket_day
    }

    /// Session day whose close already fired its one-shot refetch, if any.
    pub fn close_refetch_day(&self) -> Option<i64> {
        self.close_refetch_day
    }

    /// Mark the close refetch as fired for `day` (set synchronously when the
    /// fetch is dispatched, so a second regular print cannot double-fire).
    pub fn set_close_refetch_day(&mut self, day: i64) {
        self.close_refetch_day = Some(day);
    }

    /// Carry the live/close bookkeeping across a chart replacement (initial
    /// kline fetch rebuilds the chart object); mirrors the levels/store
    /// take/restore pair so the one-shot refetch is neither lost nor repeated.
    pub fn take_premarket_aux_days(&mut self) -> (Option<i64>, Option<i64>) {
        (
            std::mem::take(&mut self.live_premarket_day),
            std::mem::take(&mut self.close_refetch_day),
        )
    }

    /// Restore live/close bookkeeping taken from a replaced chart.
    pub fn restore_premarket_aux_days(&mut self, live_day: Option<i64>, close_day: Option<i64>) {
        self.live_premarket_day = live_day;
        self.close_refetch_day = close_day;
    }

    /// Plot the premarket levels of a specific session day (`YYYYMMDD`, UTC) —
    /// backing for a date picker, or for scroll-follow by passing the day under
    /// the chart's visible center. Rebuilds from the stored snapshot, no fetch.
    pub fn set_premarket_session_day(&mut self, day: i64) {
        self.premarket.set_session_day(day);
        self.chart.cache.main.clear();
        self.chart.cache.clear_crosshair();
    }
}

impl canvas::Program<Message> for KlineChart {
    type State = Interaction;

    fn update(
        &self,
        interaction: &mut Interaction,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        super::canvas_interaction(self, interaction, event, bounds, cursor)
    }

    fn draw(
        &self,
        interaction: &Interaction,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let chart = self.state();

        if chart.bounds.width == 0.0 {
            return vec![];
        }

        let bounds_size = bounds.size();
        let palette = theme.extended_palette();

        let klines = chart.cache.main.draw(renderer, bounds_size, |frame| {
            let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);

            frame.translate(center);
            frame.scale(chart.scaling);
            frame.translate(chart.translation);

            let region = chart.visible_region(frame.size());
            let (earliest, latest) = chart.interval_range(&region);

            let price_to_y = |price| chart.price_to_y(price);
            let interval_to_x = |interval| chart.interval_to_x(interval);
            // Último preço (close) para o papel dinâmico da confluência
            // TG1/TG2 (suporte vs resistência).
            let last_px = chart
                .last_price
                .map(|l| l.get_with_color(palette).0.to_f64());

            match &self.kind {
                KlineChartKind::Footprint {
                    clusters,
                    scaling,
                    studies,
                } => {
                    let (highest, lowest) = chart.price_range(&region);

                    let max_cluster_qty = self.calc_qty_scales(
                        earliest,
                        latest,
                        highest,
                        lowest,
                        chart.tick_size,
                        *clusters,
                    );

                    let cell_height_unscaled = chart.cell_height * chart.scaling;
                    let cell_width_unscaled = chart.cell_width * chart.scaling;

                    let text_size =
                        footprint_cluster_text_size(cell_height_unscaled, cell_width_unscaled);

                    let candle_width = 0.1 * chart.cell_width;
                    let content_spacing = ContentGaps::from_view(candle_width, chart.scaling);

                    let imbalance = studies.iter().find_map(|study| {
                        if let FootprintStudy::Imbalance {
                            threshold,
                            color_scale,
                            ignore_zeros,
                        } = study
                        {
                            Some((*threshold, *color_scale, *ignore_zeros))
                        } else {
                            None
                        }
                    });

                    let show_text = should_show_text(
                        cell_height_unscaled,
                        cell_width_unscaled,
                        footprint_cluster_min_width(*clusters),
                    );
                    let cell_layout = FootprintCellLayout {
                        cell_w: chart.cell_width,
                        cell_h: chart.cell_height,
                        candle_w: candle_width,
                        pal: palette,
                        cluster: *clusters,
                        gaps: content_spacing,
                    };

                    draw_all_npocs(
                        &self.data_source,
                        frame,
                        price_to_y,
                        interval_to_x,
                        &cell_layout,
                        studies,
                        earliest,
                        latest,
                        imbalance.is_some(),
                    );

                    render_data_source(
                        &self.data_source,
                        frame,
                        earliest,
                        latest,
                        interval_to_x,
                        |frame, x_position, kline, trades| {
                            let cluster_scaling = effective_cluster_qty(
                                *scaling,
                                max_cluster_qty,
                                trades,
                                cell_layout.cluster,
                            );

                            draw_clusters(
                                frame,
                                price_to_y,
                                x_position,
                                &cell_layout,
                                chart.scaling,
                                cluster_scaling,
                                text_size,
                                self.tick_size(),
                                show_text,
                                self.visual_config.show_footprint_summary,
                                imbalance,
                                kline,
                                trades,
                            );
                        },
                    );
                }
                KlineChartKind::Candles => {
                    let candle_width = chart.cell_width * 0.62;

                    render_data_source(
                        &self.data_source,
                        frame,
                        earliest,
                        latest,
                        interval_to_x,
                        |frame, x_position, kline, _| {
                            draw_candle_dp(
                                frame,
                                price_to_y,
                                candle_width,
                                palette,
                                x_position,
                                kline,
                            );
                        },
                    );

                    if self.visual_config.show_volume_profile {
                        draw_candle_volume_profile(
                            frame,
                            &self.candle_profile,
                            &price_to_y,
                            region,
                            palette,
                            self.chart.tick_size,
                            self.visual_config.volume_profile_side,
                        );
                    }
                }
            }

            chart.draw_last_price_line(frame, palette, region);

            if self.premarket_enabled && !self.premarket.is_empty() {
                draw_premarket_levels(
                    frame,
                    &self.premarket,
                    price_to_y,
                    region,
                    chart.scaling,
                    palette,
                );
            }
            if self.theoric_enabled && !self.premarket.is_empty() {
                draw_theoric_targets(
                    frame,
                    &self.premarket,
                    price_to_y,
                    region,
                    chart.scaling,
                    palette,
                    self.chart.tick_size.to_f64_lossy(),
                );
            }
            if self.magnets_enabled && !self.magnets.is_empty() {
                draw_magnets(
                    frame,
                    &self.magnets,
                    price_to_y,
                    region,
                    chart.scaling,
                    palette,
                    self.chart.tick_size.to_f64_lossy(),
                );
            }
            if self.masterpice_enabled
                && let Some(alvos) = self.masterpice.alvos()
            {
                draw_masterpice(
                    frame,
                    alvos,
                    price_to_y,
                    &interval_to_x,
                    latest,
                    region,
                    chart.scaling,
                    palette,
                    self.chart.tick_size.to_f64_lossy(),
                    last_px,
                    false,
                );
            }
            if self.masterpice_profile_enabled && !self.masterpice.is_empty() {
                draw_masterpice_profile(
                    frame,
                    &self.masterpice,
                    &self.masterpice_profile,
                    price_to_y,
                    &interval_to_x,
                    latest,
                    region,
                    chart.scaling,
                    palette,
                    self.chart.tick_size.to_f64_lossy(),
                    last_px,
                );
            }
            if self.masterpice_v2_enabled
                && let Some(alvos_v2) = self.masterpice_v2.alvos()
            {
                draw_masterpice(
                    frame,
                    alvos_v2,
                    price_to_y,
                    &interval_to_x,
                    latest,
                    region,
                    chart.scaling,
                    palette,
                    self.chart.tick_size.to_f64_lossy(),
                    last_px,
                    true,
                );
            }
        });

        let crosshair = chart.cache.crosshair.draw(renderer, bounds_size, |frame| {
            let visible_region = chart.visible_region(bounds_size);
            let visible_range = chart.interval_range(&visible_region);

            if let Some(cursor_position) = cursor.position_in(bounds) {
                let (_, rounded_aggregation) =
                    chart.draw_crosshair(frame, theme, bounds_size, cursor_position, interaction);

                draw_crosshair_tooltip(
                    &self.data_source,
                    &chart.ticker_info,
                    frame,
                    palette,
                    chart.basis,
                    Some(rounded_aggregation),
                    visible_range,
                );
            } else if self.visual_config.data_labels_always_visible {
                draw_crosshair_tooltip(
                    &self.data_source,
                    &chart.ticker_info,
                    frame,
                    palette,
                    chart.basis,
                    None,
                    visible_range,
                );
            }
        });

        vec![klines, crosshair]
    }

    fn mouse_interaction(
        &self,
        interaction: &Interaction,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match interaction {
            Interaction::Panning { .. } => mouse::Interaction::Grabbing,
            Interaction::Zoomin { .. } => mouse::Interaction::ZoomIn,
            Interaction::None | Interaction::Ruler { .. } => {
                if cursor.is_over(bounds) {
                    mouse::Interaction::Crosshair
                } else {
                    mouse::Interaction::default()
                }
            }
        }
    }
}

/// Upper bound for the live trade tape (`KlineChart::raw_trades`). Each `Trade`
/// is ~32 bytes, so the cap pins the tape to roughly 16 MB per pane. Without
/// it the tape grows for the whole session — one entry per print, never
/// pruned — and saturates RAM in minutes at high tick rates.
///
/// Trade-off: a later full refold (backfill, tick-size change) replays only
/// the retained tail. OHLC buckets are unaffected, but footprint detail in the
/// pruned region is not restored by the refold.
const MAX_RAW_TRADES: usize = 500_000;
/// Hysteresis for tape pruning: only drain once the tape overshoots the cap
/// by this much, so the O(n) front-drain stays amortized across inserts.
const TAPE_PRUNE_BATCH: usize = 50_000;

/// Drops the oldest prints once the tape exceeds [`MAX_RAW_TRADES`].
/// The tape stays time-ordered: live inserts append in arrival order and
/// [`merge_fetched_into_tape`] re-sorts after every merge.
fn prune_tape(tape: &mut Vec<Trade>) {
    if tape.len() > MAX_RAW_TRADES + TAPE_PRUNE_BATCH {
        let drop = tape.len() - MAX_RAW_TRADES;
        tape.drain(0..drop);
    }
}

/// Merges a fetched batch into `tape`, treating the batch as authoritative for its own
/// time span: whatever the tape held in `[first, last]` is dropped before the batch goes
/// in, so re-fetching a window replaces it instead of stacking on it.
///
/// The tape stays sorted by time, which is what makes replaying it reproduce the same
/// buckets — tick-based aggregation in particular is order-dependent.
fn merge_fetched_into_tape(tape: &mut Vec<Trade>, batch: &[Trade]) {
    let (Some(first), Some(last)) = (batch.first(), batch.last()) else {
        return;
    };

    let (from, to) = if first.time <= last.time {
        (first.time, last.time)
    } else {
        (last.time, first.time)
    };

    tape.retain(|trade| trade.time < from || trade.time > to);
    tape.extend_from_slice(batch);
    tape.sort_by_key(|trade| trade.time);
    prune_tape(tape);
}

fn draw_footprint_kline(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    x_position: f32,
    candle_width: f32,
    kline: &Kline,
    palette: &Extended,
) {
    let y_open = price_to_y(kline.open);
    let y_high = price_to_y(kline.high);
    let y_low = price_to_y(kline.low);
    let y_close = price_to_y(kline.close);

    let body_color = if kline.close >= kline.open {
        palette.success.weak.color
    } else {
        palette.danger.weak.color
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 8.0), y_open.min(y_close)),
        Size::new(candle_width / 4.0, (y_open - y_close).abs()),
        body_color,
    );

    let wick_color = if kline.close >= kline.open {
        palette.success.weak.color
    } else {
        palette.danger.weak.color
    };
    let marker_line = Stroke::with_color(
        Stroke {
            width: 1.0,
            ..Default::default()
        },
        wick_color.scale_alpha(0.6),
    );
    frame.stroke(
        &Path::line(
            Point::new(x_position, y_high),
            Point::new(x_position, y_low),
        ),
        marker_line,
    );
}

fn draw_candle_dp(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    candle_width: f32,
    palette: &Extended,
    x_position: f32,
    kline: &Kline,
) {
    let y_open = price_to_y(kline.open);
    let y_high = price_to_y(kline.high);
    let y_low = price_to_y(kline.low);
    let y_close = price_to_y(kline.close);

    let body_color = if kline.close >= kline.open {
        palette.success.base.color
    } else {
        palette.danger.base.color
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 2.0), y_open.min(y_close)),
        Size::new(candle_width, (y_open - y_close).abs()),
        body_color,
    );

    let wick_color = if kline.close >= kline.open {
        palette.success.base.color
    } else {
        palette.danger.base.color
    };
    // A hairline stroke, not a filled bar: the previous `candle_width / 4.0` rectangle
    // made the wick nearly as wide as the body, so bearish/bullish shapes read as thick
    // blocks with no visible taper — a proper candlestick wick stays thin regardless of
    // how wide the body is.
    frame.stroke(
        &Path::line(
            Point::new(x_position, y_high),
            Point::new(x_position, y_low),
        ),
        Stroke::with_color(
            Stroke {
                width: 1.5,
                ..Default::default()
            },
            wick_color,
        ),
    );
}

/// Volume-profile overlay for the candlestick chart: trailing-window buy/sell
/// ticks grouped by price level (default 1h, 0-24h), anchored to the configured
/// edge (right = bars point back/left, left = bars point forward/right) with
/// the candle theme colors (success=buy, danger=sell). OHLC-independent — the
/// source is the window accumulator (history bootstrap + realtime), not the
/// per-bucket footprint and not the visible range.
///
/// Every bar gets a thin outline for edge precision: the PoC level (largest
/// total volume over the whole window — singular, unlike footprint's per-candle
/// PoC) is outlined yellow; other bars get a discreet darker shade of their
/// own fill.
fn draw_candle_volume_profile(
    frame: &mut canvas::Frame,
    profile: &CandleVolumeProfile,
    price_to_y: &impl Fn(Price) -> f32,
    region: Rectangle,
    palette: &Extended,
    step: PriceStep,
    side: VolumeProfileSide,
) {
    if profile.is_empty() {
        return;
    }
    let max_qty = profile.max_total();
    if max_qty <= 0.0 {
        return;
    }
    // One row per tick-step so bars align with price levels; fall back when the
    // step maps to sub-pixel height.
    let probe = Price::from_f64(0.0);
    let row_h = (price_to_y(probe) - price_to_y(probe.add_steps(1, step)))
        .abs()
        .clamp(2.0, 28.0)
        .max(1.0);
    let profile_width = (region.width * 0.28).min(220.0);
    let poc = profile.poc_price();
    let buy_fill = palette.success.base.color.scale_alpha(0.55);
    let sell_fill = palette.danger.base.color.scale_alpha(0.55);
    let buy_edge = darken(palette.success.base.color, 0.22);
    let sell_edge = darken(palette.danger.base.color, 0.22);
    let poc_edge = palette.warning.strong.color;
    let mut sorted: Vec<_> = profile.levels().collect();
    sorted.sort_by_key(|(p, _)| **p);
    for (price, (buy, sell)) in sorted {
        let (buy_f, sell_f) = (buy.to_f64(), sell.to_f64());
        let total = buy_f + sell_f;
        if total <= 0.0 {
            continue;
        }
        let y = price_to_y(*price);
        if !y.is_finite() {
            continue;
        }
        let bar_w = (total / max_qty) as f32 * profile_width;
        let sell_w = ((sell_f / total) as f32) * bar_w;
        let buy_w = bar_w - sell_w;
        let top = y - row_h / 2.0;
        let is_poc = poc == Some(*price);
        // Right-anchored bars extend left (point back); left-anchored extend
        // right (point forward). Sell sits at the outer edge, buy at the anchor.
        let (sell_x, buy_x) = match side {
            VolumeProfileSide::Right => {
                let right = region.x + region.width;
                (right - bar_w, right - buy_w)
            }
            VolumeProfileSide::Left => {
                let left = region.x;
                (left, left + sell_w)
            }
        };
        let stroke_bar = |frame: &mut canvas::Frame,
                          x: f32,
                          top: f32,
                          w: f32,
                          h: f32,
                          edge: Color,
                          width: f32| {
            if w <= 0.0 {
                return;
            }
            frame.stroke(
                &Path::rectangle(Point::new(x, top), Size::new(w, h)),
                Stroke::with_color(
                    Stroke {
                        width,
                        ..Default::default()
                    },
                    edge,
                ),
            );
        };
        if sell_w > 0.0 {
            frame.fill_rectangle(Point::new(sell_x, top), Size::new(sell_w, row_h), sell_fill);
            let (edge, width) = if is_poc {
                (poc_edge, 1.5)
            } else {
                (sell_edge, 1.0)
            };
            stroke_bar(frame, sell_x, top, sell_w, row_h, edge, width);
        }
        if buy_w > 0.0 {
            frame.fill_rectangle(Point::new(buy_x, top), Size::new(buy_w, row_h), buy_fill);
            let (edge, width) = if is_poc {
                (poc_edge, 1.5)
            } else {
                (buy_edge, 1.0)
            };
            stroke_bar(frame, buy_x, top, buy_w, row_h, edge, width);
        }
    }
}

/// Strokes the premarket levels for the "Premarket H/L" indicator the way wall
/// lines look: thin horizontal traces across the visible area, with the session
/// extremes (high/low) in amber and every intermediate level in white. Each level
/// ends in a right-edge tag carrying "N:<seq> V:<vol> T:<secs>s".
/// `1/scaling` keeps line and tag constant on screen as the chart zooms.
fn draw_premarket_levels(
    frame: &mut canvas::Frame,
    premarket: &PremarketLevels,
    price_to_y: impl Fn(Price) -> f32,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
) {
    let inv = 1.0 / scaling.max(0.05);
    let text_size = style::text_size::TINY * inv;
    let pad_x = 4.0 * inv;
    let pad_y = 2.0 * inv;
    let gap = 3.0 * inv;
    let amber = palette.warning.strong.color;
    let line_w = (1.0 * inv).min(1.5);

    let high = premarket.high();
    let low = premarket.low();
    let right = region.x + region.width;

    for level in premarket.levels() {
        let y = price_to_y(Price::from_f64(level.price));
        if !y.is_finite() {
            continue;
        }

        let is_extreme = Some(level.price) == high || Some(level.price) == low;
        let color = if is_extreme { amber } else { Color::WHITE };

        let content = format!(
            "N:{} V:{} T:{}s",
            level.seq,
            level.vol as i64,
            level.dwell_secs()
        );
        let tag_w = content.len() as f32 * text_size * 0.62 + pad_x * 2.0;
        let tag_h = text_size + pad_y * 2.0;
        let line_end = (right - tag_w - gap).max(region.x);

        frame.stroke(
            &Path::line(Point::new(region.x, y), Point::new(line_end, y)),
            Stroke::with_color(
                Stroke {
                    width: line_w,
                    ..Default::default()
                },
                color.scale_alpha(0.6),
            ),
        );

        frame.fill_rectangle(
            Point::new(line_end, y - tag_h / 2.0),
            Size::new(tag_w, tag_h),
            palette.background.base.color,
        );
        frame.fill_text(canvas::Text {
            content,
            position: Point::new(right - gap, y),
            size: iced::Pixels(text_size),
            color,
            font: style::AZERET_MONO,
            align_x: iced::alignment::Horizontal::Right.into(),
            align_y: iced::alignment::Vertical::Center,
            ..canvas::Text::default()
        });
    }
}

/// TheoricScan Targets overlay: rebuilds AuctionNodes from PremarketLevels
/// (read-only, no mutation), scores them via data::chart::targets, and draws
/// only T1..T4 (+ debit).
///
/// Frozen by construction: nodes (vol/dwell/seq) AND the reference price all
/// come from the premarket session only — vol = same-day `premarket/` queue
/// at price, reference = auction close (last theoretical print, prorrogação
/// included). No candle close, tick, DOM or footprint value enters here, so
/// the lines stop moving once the auction ends.
fn draw_theoric_targets(
    frame: &mut canvas::Frame,
    premarket: &PremarketLevels,
    price_to_y: impl Fn(Price) -> f32,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
    tick: f64,
) {
    use data::chart::targets::{AuctionNode, TargetsConfig, build_targets};
    // Premarket-book input per node — the SAME basis as the premarket
    // indicator's "V" (queue resting exactly at the node price, split by side):
    // sell = bid queue at price, buy = ask queue at price, vol = their sum.
    // Dwell = time the theoretical price stood at the node. No tick/footprint
    // volume enters here.
    let nodes: Vec<AuctionNode> = premarket
        .levels()
        .map(|l| AuctionNode {
            price: l.price,
            seq: l.seq,
            vol: l.bid_at + l.ask_at,
            buy_vol: l.ask_at,
            sell_vol: l.bid_at,
            dwell_secs: l.dwell_secs(),
        })
        .collect();
    if nodes.is_empty() {
        return;
    }
    // Auction-close reference: frozen once the feed stops publishing
    // `premarket/` for the day (prorrogação included).
    let current = premarket
        .close()
        .or_else(|| premarket.open())
        .unwrap_or(nodes[0].price);
    let mut cfg = TargetsConfig::default();
    // No cluster merging: auction levels sit on the tick grid (WDO = 0.5/1.0
    // apart) and the default gap (2.0) chains them ALL into a single cluster,
    // summing the whole session into one bogus "Vol" (e.g. V:983 shown as
    // Vol 26177). PremarketLevels already dedups same-price prints, so each
    // level keeps exactly its own premarket queue volume.
    cfg.cluster_gap = 0.0;
    // Snap targets to the instrument tick grid (WDO = 0.5): cluster averaging
    // must never print off-tick prices like 5161.2.
    cfg.round_tick = if tick > 0.0 { tick } else { 0.5 };
    let (targets, _dir) = build_targets(&nodes, current, &cfg, &[]);
    // Tick-aware price formatting (0.5 -> 1 decimal, 1.0 -> 0 decimals).
    let decimals = (-cfg.round_tick.log10().floor()) as usize;
    let fmt_p = |p: f64| format!("{:.prec$}", p, prec = decimals);
    let inv = 1.0 / scaling.max(0.05);
    let text_size = style::text_size::TINY * inv;
    let pad_x = 4.0 * inv;
    let pad_y = 2.0 * inv;
    let gap = 3.0 * inv;
    let right = region.x + region.width;
    let colors = [
        Color::from_rgb8(0xAD, 0xFF, 0x2F),
        Color::from_rgb8(0xFF, 0xEB, 0x3B),
        Color::from_rgb8(0xFF, 0x98, 0x00),
        Color::from_rgb8(0xE0, 0x40, 0xFB),
        Color::WHITE,
    ];
    for (i, t) in targets.iter().take(5).enumerate() {
        let y = price_to_y(Price::from_f64(t.price));
        if !y.is_finite() {
            continue;
        }
        let color = colors[i.min(4)];
        let content = if t.label == "DÉBITO" {
            format!("DÉBITO {}", fmt_p(t.price))
        } else {
            format!("{} · {} · Vol {:.0}", t.label, fmt_p(t.price), t.volume)
        };
        let tag_w = content.len() as f32 * text_size * 0.62 + pad_x * 2.0;
        let tag_h = text_size + pad_y * 2.0;
        let line_end = (right - tag_w - gap).max(region.x);
        let width = if i == 0 {
            (2.5 * inv).min(3.0)
        } else {
            (1.2 * inv).min(1.5)
        };
        frame.stroke(
            &Path::line(Point::new(region.x, y), Point::new(line_end, y)),
            Stroke::with_color(
                Stroke {
                    width,
                    ..Default::default()
                },
                color.scale_alpha(0.9),
            ),
        );
        frame.fill_rectangle(
            Point::new(line_end, y - tag_h / 2.0),
            Size::new(tag_w, tag_h),
            palette.background.base.color,
        );
        frame.fill_text(canvas::Text {
            content,
            position: Point::new(right - gap, y),
            size: iced::Pixels(text_size),
            color,
            font: style::AZERET_MONO,
            align_x: iced::alignment::Horizontal::Right.into(),
            align_y: iced::alignment::Vertical::Center,
            ..canvas::Text::default()
        });
    }
}

/// MAGNETS overlay: one frozen horizontal line per top-K auction attraction
/// level (premarket book only). Rank 1 is the thickest line; all lines are
/// pending-gray (no monitoring/status phase). Right-edge tags read
/// "M{rank} | {price} | S:{score} | V:{volume} | T:{secs}s".
fn draw_magnets(
    frame: &mut canvas::Frame,
    magnets: &MagnetStore,
    price_to_y: impl Fn(Price) -> f32,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
    tick: f64,
) {
    let levels = magnets.levels();
    if levels.is_empty() {
        return;
    }
    let grid = if tick > 0.0 { tick } else { 0.5 };
    let decimals = (-grid.log10().floor()) as usize;
    let fmt_p = |p: f64| format!("{:.prec$}", p, prec = decimals);
    let inv = 1.0 / scaling.max(0.05);
    let text_size = style::text_size::TINY * inv;
    let pad_x = 4.0 * inv;
    let pad_y = 2.0 * inv;
    let gap = 3.0 * inv;
    let right = region.x + region.width;
    // Pending gray, brighter for the top rank.
    let colors = [
        Color::from_rgb8(0xE0, 0xE0, 0xE0),
        Color::from_rgb8(0xB0, 0xB0, 0xB0),
        Color::from_rgb8(0x90, 0x90, 0x90),
        Color::from_rgb8(0x70, 0x70, 0x70),
        Color::from_rgb8(0x60, 0x60, 0x60),
    ];
    for (i, m) in levels.iter().enumerate() {
        let y = price_to_y(Price::from_f64(m.price));
        if !y.is_finite() {
            continue;
        }
        let color = colors[i.min(4)];
        // Normalized score (0..1); `+ 0.0` keeps a zero min_exec from
        // printing as "-0".
        let content = format!(
            "M{} | {} | S:{:.2} | V:{:.0} | T:{:.1}s | X:{:.0}",
            m.rank,
            fmt_p(m.price),
            m.score,
            m.volume,
            m.duration_ms as f64 / 1000.0,
            m.min_exec + 0.0,
        );
        let tag_w = content.len() as f32 * text_size * 0.62 + pad_x * 2.0;
        let tag_h = text_size + pad_y * 2.0;
        let line_end = (right - tag_w - gap).max(region.x);
        let width = if i == 0 {
            (2.5 * inv).min(3.0)
        } else {
            (1.2 * inv).min(1.5)
        };
        frame.stroke(
            &Path::line(Point::new(region.x, y), Point::new(line_end, y)),
            Stroke::with_color(
                Stroke {
                    width,
                    ..Default::default()
                },
                color.scale_alpha(0.9),
            ),
        );
        frame.fill_rectangle(
            Point::new(line_end, y - tag_h / 2.0),
            Size::new(tag_w, tag_h),
            palette.background.base.color,
        );
        frame.fill_text(canvas::Text {
            content,
            position: Point::new(right - gap, y),
            size: iced::Pixels(text_size),
            color,
            font: style::AZERET_MONO,
            align_x: iced::alignment::Horizontal::Right.into(),
            align_y: iced::alignment::Vertical::Center,
            ..canvas::Text::default()
        });
    }
}

/// Métricas compartilhadas do estilo Masterpice: linha pontilhada (igual
/// bid/ask) limitada ao último candle + tag de nome no fim da linha + tag de
/// preço na borda da escala (só preço, sem timer).
struct MpMetrics {
    text_size: f32,
    pad_x: f32,
    pad_y: f32,
    gap: f32,
    left: f32,
    right: f32,
    last_x: f32,
    name_bg: Color,
    scale_bg: Color,
}

impl MpMetrics {
    fn new(region: Rectangle, scaling: f32, palette: &Extended, last_x: f32) -> Self {
        let inv = 1.0 / scaling.max(0.05);
        let right = region.x + region.width;
        Self {
            // Títulos no mesmo tamanho da fonte da escala de preços.
            text_size: style::text_size::BODY * inv,
            pad_x: 4.0 * inv,
            pad_y: 2.0 * inv,
            gap: 3.0 * inv,
            left: region.x,
            right,
            last_x: last_x.clamp(region.x, right),
            name_bg: palette.background.base.color,
            // Cinza escuro um pouco mais claro que o fundo do gráfico.
            scale_bg: mix_color(Color::WHITE, palette.background.base.color, 0.10),
        }
    }

    fn dotted(width: f32) -> Stroke<'static> {
        const DASH: &[f32] = &[2.0, 2.0];
        Stroke {
            width,
            line_dash: LineDash {
                segments: DASH,
                offset: 4,
            },
            ..Default::default()
        }
    }
}

/// Um nível Masterpice: linha pontilhada de `region.x` até o último candle,
/// tag de NOME no fim da linha (sem preço: TG1, TG2, OPEN, HIGH, LOW...) e
/// tag na escala com NOME + PREÇO (ex "TG1 5132") para diferenciar os níveis
/// e saber o preço de fato. Dormente desenha "TGn STANDBY" + preço, apagado.
#[allow(clippy::too_many_arguments)]
fn mp_level(
    frame: &mut canvas::Frame,
    price_to_y: &impl Fn(Price) -> f32,
    m: &MpMetrics,
    price: f64,
    name: &str,
    color: Color,
    width: f32,
    standby: bool,
    fmt_price: &impl Fn(f64) -> String,
) {
    let y = price_to_y(Price::from_f64(price));
    if !y.is_finite() {
        return;
    }
    let line_color = if standby {
        color.scale_alpha(0.35)
    } else {
        color.scale_alpha(0.85)
    };
    frame.stroke(
        &Path::line(Point::new(m.left, y), Point::new(m.last_x, y)),
        Stroke::with_color(MpMetrics::dotted(width), line_color),
    );
    // Tag de nome no fim da linha.
    let tag_w = name.len() as f32 * m.text_size * 0.62 + m.pad_x * 2.0;
    let tag_h = m.text_size + m.pad_y * 2.0;
    let tag_x = (m.last_x + m.gap).min((m.right - tag_w).max(m.last_x));
    let text_color = if standby {
        color.scale_alpha(0.5)
    } else {
        color
    };
    frame.fill_rectangle(
        Point::new(tag_x, y - tag_h / 2.0),
        Size::new(tag_w, tag_h),
        m.name_bg,
    );
    frame.fill_text(canvas::Text {
        content: name.to_string(),
        position: Point::new(tag_x + m.pad_x, y),
        size: iced::Pixels(m.text_size),
        color: text_color,
        font: style::AZERET_MONO,
        align_x: iced::alignment::Horizontal::Left.into(),
        align_y: iced::alignment::Vertical::Center,
        ..canvas::Text::default()
    });
    // Tag na escala: NOME + PREÇO (inclusive dormente, apagado).
    let ptext = format!("{name} {}", fmt_price(price));
    let ptag_w = ptext.len() as f32 * m.text_size * 0.62 + m.pad_x * 2.0;
    frame.fill_rectangle(
        Point::new(m.right - ptag_w, y - tag_h / 2.0),
        Size::new(ptag_w, tag_h),
        m.scale_bg,
    );
    frame.fill_text(canvas::Text {
        content: ptext,
        position: Point::new(m.right - m.pad_x, y),
        size: iced::Pixels(m.text_size),
        color: text_color,
        font: style::AZERET_MONO,
        align_x: iced::alignment::Horizontal::Right.into(),
        align_y: iced::alignment::Vertical::Center,
        ..canvas::Text::default()
    });
}

/// Masterpice overlay: Nós de Leilão do premarket.
/// Nomes funcionais: TG1 PIVOT = Alvo1; TG2/TG3 = Alvo2 alta/baixa;
/// TG4/TG5 = Alvo3 alta/baixa; HIGH = teto duro; LOW = piso duro;
/// OPEN = abertura estimada. Cor pela direção como o candle: alta = azul
/// (bull), baixa = vermelho (bear); HIGH/LOW = amarelo. Alvo3 fora do viés
/// vira "TGn STANDBY" + preço, apagado. Recebe `&Alvos` para servir V1 e V2.
///
/// Confluência: se |TG1 − TG2| (ou |TG1 − TG3|) ≤ 1 tick, os dois papéis
/// colidem no mesmo preço — desenha UM rótulo combinado ("TG1/TG2 PIVO-…")
/// com papel dinâmico pelo último preço: abaixo e sem romper = resistência
/// (vermelho); acima/após romper = suporte (azul). Sem último preço, o
/// padrão é resistência em cima, suporte embaixo.
///
/// `apenas_area_leilao` (V2): suprime Alvo3 fora de [LOW, HIGH] — projeção
/// fora da área do leilão não se desenha.
#[allow(clippy::too_many_arguments)]
fn draw_masterpice(
    frame: &mut canvas::Frame,
    alvos: &data::chart::masterpice::Alvos,
    price_to_y: impl Fn(Price) -> f32,
    interval_to_x: &impl Fn(u64) -> f32,
    latest: u64,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
    tick: f64,
    last_price: Option<f64>,
    apenas_area_leilao: bool,
) {
    use data::chart::masterpice::Direcao;
    let grid = if tick > 0.0 { tick } else { 0.5 };
    let tol = grid.max(0.01);
    let decimals = (-grid.log10().floor().max(0.0)) as usize;
    let fmt_p = |p: f64| format!("{:.prec$}", p, prec = decimals);
    let m = MpMetrics::new(region, scaling, palette, interval_to_x(latest));
    let inv = 1.0 / scaling.max(0.05);
    let w_main = (1.5 * inv).min(2.0);
    let w_aux = (1.0 * inv).min(1.5);
    let alta = palette.success.base.color;
    let baixa = palette.danger.base.color;
    let amarelo = palette.warning.strong.color;
    // Papel dinâmico da confluência pelo contexto do preço.
    let papel_alto = match last_price {
        Some(px) if px >= alvos.alvo1_pivo => ("SUP", alta),
        Some(_) => ("RESIST", baixa),
        None => ("RESIST", baixa),
    };
    let papel_baixo = match last_price {
        Some(px) if px >= alvos.alvo1_pivo => ("SUP", alta),
        Some(_) => ("RESIST", baixa),
        None => ("SUP", alta),
    };
    let conf_alta = (alvos.alvo2_extremo_alta - alvos.alvo1_pivo).abs() <= tol;
    let conf_baixa = (alvos.alvo1_pivo - alvos.alvo2_extremo_baixa).abs() <= tol;
    // TG1 isolado só quando não há confluência; senão o rótulo combinado
    // o representa (evita dois sinais opostos no mesmo preço).
    if conf_alta && conf_baixa {
        let (papel, cor) = papel_alto;
        mp_level(
            frame,
            &price_to_y,
            &m,
            alvos.alvo1_pivo,
            &format!("TG1/TG2/TG3 PIVO-{papel}"),
            cor,
            w_main,
            false,
            &fmt_p,
        );
    } else {
        if conf_alta {
            let (papel, cor) = papel_alto;
            mp_level(
                frame,
                &price_to_y,
                &m,
                alvos.alvo1_pivo,
                &format!("TG1/TG2 PIVO-{papel}"),
                cor,
                w_main,
                false,
                &fmt_p,
            );
        } else {
            // TG1 segue a cor do viés (a direção prioritária do dia).
            let pivo_cor = match alvos.vies.direcao {
                Direcao::Alta => alta,
                Direcao::Baixa => baixa,
                Direcao::Neutra => Color::WHITE,
            };
            mp_level(
                frame,
                &price_to_y,
                &m,
                alvos.alvo1_pivo,
                "TG1 PIVOT",
                pivo_cor,
                w_main,
                false,
                &fmt_p,
            );
            mp_level(
                frame,
                &price_to_y,
                &m,
                alvos.alvo2_extremo_alta,
                "TG2",
                alta,
                w_aux,
                false,
                &fmt_p,
            );
        }
        if conf_baixa {
            let (papel, cor) = papel_baixo;
            mp_level(
                frame,
                &price_to_y,
                &m,
                alvos.alvo1_pivo,
                &format!("TG1/TG3 PIVO-{papel}"),
                cor,
                w_main,
                false,
                &fmt_p,
            );
        } else {
            mp_level(
                frame,
                &price_to_y,
                &m,
                alvos.alvo2_extremo_baixa,
                "TG3",
                baixa,
                w_aux,
                false,
                &fmt_p,
            );
        }
    }
    // V2: Alvo3 fora da área do leilão [LOW, HIGH] não se desenha.
    let tg4_fora = apenas_area_leilao
        && (alvos.alvo3_projecao_alta < alvos.piso_duro
            || alvos.alvo3_projecao_alta > alvos.teto_duro);
    let tg5_fora = apenas_area_leilao
        && (alvos.alvo3_projecao_baixa < alvos.piso_duro
            || alvos.alvo3_projecao_baixa > alvos.teto_duro);
    if !tg4_fora {
        mp_level(
            frame,
            &price_to_y,
            &m,
            alvos.alvo3_projecao_alta,
            if alvos.alvo3_alta_ativo {
                "TG4"
            } else {
                "TG4 STANDBY"
            },
            alta,
            w_aux,
            !alvos.alvo3_alta_ativo,
            &fmt_p,
        );
    }
    if !tg5_fora {
        mp_level(
            frame,
            &price_to_y,
            &m,
            alvos.alvo3_projecao_baixa,
            if alvos.alvo3_baixa_ativo {
                "TG5"
            } else {
                "TG5 STANDBY"
            },
            baixa,
            w_aux,
            !alvos.alvo3_baixa_ativo,
            &fmt_p,
        );
    }
    mp_level(
        frame,
        &price_to_y,
        &m,
        alvos.abertura_estimada,
        "OPEN",
        Color::WHITE.scale_alpha(0.6),
        (1.0 * inv).min(1.2),
        false,
        &fmt_p,
    );
    let duro_w = (1.0 * inv).min(1.2);
    mp_level(
        frame,
        &price_to_y,
        &m,
        alvos.teto_duro,
        "HIGH",
        amarelo,
        duro_w,
        false,
        &fmt_p,
    );
    mp_level(
        frame,
        &price_to_y,
        &m,
        alvos.piso_duro,
        "LOW",
        amarelo,
        duro_w,
        false,
        &fmt_p,
    );
}

/// Masterpice Profile overlay: base TG1..TG5 + HIGH/LOW + POC/VAH/VAL same-day
/// em tempo real, com promoção de pivô. Se o POC promovido diferir do Alvo1,
/// desenha "TG1 ATIVO" no POC.
fn draw_masterpice_profile(
    frame: &mut canvas::Frame,
    store: &MasterpiceStore,
    profile: &VolumeDayStore,
    price_to_y: impl Fn(Price) -> f32,
    interval_to_x: &impl Fn(u64) -> f32,
    latest: u64,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
    tick: f64,
    last_price: Option<f64>,
) {
    // Base: mesmos níveis do Masterpice.
    let Some(alvos_base) = store.alvos() else {
        return;
    };
    draw_masterpice(
        frame,
        alvos_base,
        &price_to_y,
        interval_to_x,
        latest,
        region,
        scaling,
        palette,
        tick,
        last_price,
        false,
    );
    let Some(vp) = profile.profile() else { return };
    let Some(alvos) = store.alvos() else { return };
    let grid = if tick > 0.0 { tick } else { 0.5 };
    let decimals = (-grid.log10().floor().max(0.0)) as usize;
    let fmt_p = |p: f64| format!("{:.prec$}", p, prec = decimals);
    let m = MpMetrics::new(region, scaling, palette, interval_to_x(latest));
    let inv = 1.0 / scaling.max(0.05);
    let w = (1.2 * inv).min(1.5);
    mp_level(
        frame,
        &price_to_y,
        &m,
        vp.poc,
        "POC",
        Color::from_rgb8(0x00, 0xE5, 0xFF),
        w,
        false,
        &fmt_p,
    );
    mp_level(
        frame,
        &price_to_y,
        &m,
        vp.vah,
        "VAH",
        Color::from_rgb8(0x7C, 0x4D, 0xFF),
        (1.0 * inv).min(1.2),
        false,
        &fmt_p,
    );
    mp_level(
        frame,
        &price_to_y,
        &m,
        vp.val,
        "VAL",
        Color::from_rgb8(0x7C, 0x4D, 0xFF),
        (1.0 * inv).min(1.2),
        false,
        &fmt_p,
    );
    let tolerancia = 2.0 * grid;
    let alvo1_ativo = promover_pivo_se_necessario(
        alvos.alvo1_pivo,
        alvos.alvo2_extremo_alta,
        alvos.alvo2_extremo_baixa,
        &vp,
        tolerancia,
    );
    if (alvo1_ativo - alvos.alvo1_pivo).abs() > f64::EPSILON {
        mp_level(
            frame,
            &price_to_y,
            &m,
            alvo1_ativo,
            "TG1 PIVOT ATIVO",
            palette.success.base.color,
            (1.5 * inv).min(2.0),
            false,
            &fmt_p,
        );
    }
}

fn render_data_source<F>(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    earliest: u64,
    latest: u64,
    interval_to_x: impl Fn(u64) -> f32,
    draw_fn: F,
) where
    F: Fn(&mut canvas::Frame, f32, &Kline, &KlineTrades),
{
    match data_source {
        PlotData::TickBased(tick_aggr) => {
            let earliest = earliest as usize;
            let latest = latest as usize;

            tick_aggr
                .datapoints
                .iter()
                .rev()
                .enumerate()
                .filter(|(index, _)| *index <= latest && *index >= earliest)
                .for_each(|(index, tick_aggr)| {
                    let x_position = interval_to_x(index as u64);

                    draw_fn(frame, x_position, &tick_aggr.kline, &tick_aggr.footprint);
                });
        }
        PlotData::TimeBased(timeseries) => {
            if latest < earliest {
                return;
            }

            timeseries
                .datapoints
                .range(UnixMs::new(earliest)..=UnixMs::new(latest))
                .for_each(|(timestamp, dp)| {
                    let x_position = interval_to_x(timestamp.as_u64());

                    draw_fn(frame, x_position, &dp.kline, &dp.footprint);
                });
        }
    }
}

fn draw_all_npocs(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    interval_to_x: impl Fn(u64) -> f32,
    layout: &FootprintCellLayout<'_>,
    studies: &[FootprintStudy],
    visible_earliest: u64,
    visible_latest: u64,
    imb_study_on: bool,
) {
    let Some(lookback) = studies.iter().find_map(|study| {
        if let FootprintStudy::NPoC { lookback } = study {
            Some(*lookback)
        } else {
            None
        }
    }) else {
        return;
    };

    let (filled_color, naked_color) = (
        layout.pal.background.strong.color,
        if layout.pal.is_dark {
            layout.pal.warning.weak.color.scale_alpha(0.5)
        } else {
            layout.pal.warning.strong.color
        },
    );

    let line_height = layout.cell_h.min(1.0);

    let bar_width_factor: f32 = 0.9;
    let inset = (layout.cell_w * (1.0 - bar_width_factor)) / 2.0;

    let candle_lane_factor: f32 = match layout.cluster {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => 0.25,
        ClusterKind::BidAsk | ClusterKind::Table => 1.0,
    };

    let start_x_for = |cell_center_x: f32| -> f32 {
        match layout.cluster {
            ClusterKind::Table => cell_center_x + (layout.cell_w / 2.0) - inset,
            ClusterKind::BidAsk => {
                cell_center_x + (layout.candle_w / 2.0) + layout.gaps.candle_to_cluster
            }
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
                let content_left = (cell_center_x - (layout.cell_w / 2.0)) + inset;
                let candle_lane_left = content_left
                    + if imb_study_on {
                        layout.candle_w + layout.gaps.marker_to_candle
                    } else {
                        0.0
                    };
                candle_lane_left
                    + layout.candle_w * candle_lane_factor
                    + layout.gaps.candle_to_cluster
            }
        }
    };

    let wick_x_for = |cell_center_x: f32| -> f32 {
        match layout.cluster {
            ClusterKind::BidAsk | ClusterKind::Table => cell_center_x,
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
                let content_left = (cell_center_x - (layout.cell_w / 2.0)) + inset;
                let candle_lane_left = content_left
                    + if imb_study_on {
                        layout.candle_w + layout.gaps.marker_to_candle
                    } else {
                        0.0
                    };
                candle_lane_left + (layout.candle_w * candle_lane_factor) / 2.0
                    - (layout.gaps.candle_to_cluster * 0.5)
            }
        }
    };

    let end_x_for = |cell_center_x: f32| -> f32 {
        match layout.cluster {
            ClusterKind::BidAsk | ClusterKind::Table => {
                cell_center_x - (layout.candle_w / 2.0) - layout.gaps.candle_to_cluster
            }
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => wick_x_for(cell_center_x),
        }
    };

    let rightmost_cell_center_x = {
        let earliest_x = interval_to_x(visible_earliest);
        let latest_x = interval_to_x(visible_latest);
        if earliest_x > latest_x {
            earliest_x
        } else {
            latest_x
        }
    };

    let mut draw_the_line = |interval: u64, poc: &PointOfControl| {
        let start_x = start_x_for(interval_to_x(interval));

        let (line_width, color) = match poc.status {
            NPoc::Naked => {
                let end_x = end_x_for(rightmost_cell_center_x);
                let line_width = end_x - start_x;
                if line_width.abs() <= layout.cell_w {
                    return;
                }
                (line_width, naked_color)
            }
            NPoc::Filled { at } => {
                let end_x = end_x_for(interval_to_x(at));
                let line_width = end_x - start_x;
                if line_width.abs() <= layout.cell_w {
                    return;
                }
                (line_width, filled_color)
            }
            _ => return,
        };

        frame.fill_rectangle(
            Point::new(start_x, price_to_y(poc.price) - line_height / 2.0),
            Size::new(line_width, line_height),
            color,
        );
    };

    match data_source {
        PlotData::TickBased(tick_aggr) => {
            tick_aggr
                .datapoints
                .iter()
                .rev()
                .enumerate()
                .take(lookback)
                .filter_map(|(index, dp)| dp.footprint.poc.as_ref().map(|poc| (index as u64, poc)))
                .for_each(|(interval, poc)| draw_the_line(interval, poc));
        }
        PlotData::TimeBased(timeseries) => {
            timeseries
                .datapoints
                .iter()
                .rev()
                .take(lookback)
                .filter_map(|(timestamp, dp)| {
                    dp.footprint
                        .poc
                        .as_ref()
                        .map(|poc| (timestamp.as_u64(), poc))
                })
                .for_each(|(interval, poc)| draw_the_line(interval, poc));
        }
    }
}

fn effective_cluster_qty(
    scaling: ClusterScaling,
    visible_max: f64,
    footprint: &KlineTrades,
    cluster_kind: ClusterKind,
) -> f64 {
    let individual_max = match cluster_kind {
        ClusterKind::BidAsk | ClusterKind::Table => footprint
            .trades
            .values()
            .map(|group| group.buy_qty.max(group.sell_qty))
            .max()
            .unwrap_or_default(),
        ClusterKind::DeltaProfile => footprint
            .trades
            .values()
            .map(|group| group.buy_qty.abs_diff(group.sell_qty))
            .max()
            .unwrap_or_default(),
        ClusterKind::VolumeProfile => footprint
            .trades
            .values()
            .map(|group| group.buy_qty + group.sell_qty)
            .max()
            .unwrap_or_default(),
    };

    match scaling {
        ClusterScaling::VisibleRange => Qty::scale_or_one(visible_max),
        ClusterScaling::Datapoint => individual_max.to_scale_or_one(),
        ClusterScaling::Hybrid { weight } => {
            let w = weight.clamp(0.0, 1.0) as f64;
            Qty::scale_or_one(visible_max * w + individual_max.to_f64() * (1.0 - w))
        }
    }
}

fn draw_clusters(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    x_position: f32,
    layout: &FootprintCellLayout<'_>,
    scaling: f32,
    max_cluster_qty: f64,
    text_size: f32,
    step: PriceStep,
    show_text: bool,
    show_summary: bool,
    imbalance: Option<(usize, Option<usize>, bool)>,
    kline: &Kline,
    footprint: &KlineTrades,
) {
    let text_color = layout.pal.background.weakest.text;

    let bar_width_factor: f32 = 0.9;
    let inset = (layout.cell_w * (1.0 - bar_width_factor)) / 2.0;

    let cell_left = x_position - (layout.cell_w / 2.0);
    let content_left = cell_left + inset;
    let content_right = x_position + (layout.cell_w / 2.0) - inset;

    match layout.cluster {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
            let area = ProfileArea::new(
                content_left,
                content_right,
                layout.candle_w,
                layout.gaps,
                imbalance.is_some(),
            );
            let bar_alpha = if show_text { 0.25 } else { 1.0 };

            for (price, group) in &footprint.trades {
                let buy_qty = group.buy_qty.to_f64();
                let sell_qty = group.sell_qty.to_f64();
                let y = price_to_y(*price);

                match layout.cluster {
                    ClusterKind::VolumeProfile => {
                        super::draw_volume_bar(
                            frame,
                            area.bars_left,
                            y,
                            buy_qty,
                            sell_qty,
                            max_cluster_qty,
                            area.bars_width,
                            layout.cell_h,
                            layout.pal.success.base.color,
                            layout.pal.danger.base.color,
                            bar_alpha,
                            true,
                        );

                        if show_text {
                            draw_cluster_text(
                                frame,
                                &abbr_large_numbers(f64::from(group.total_qty())),
                                Point::new(area.bars_left, y),
                                text_size,
                                text_color,
                                Alignment::Start,
                                Alignment::Center,
                            );
                        }
                    }
                    ClusterKind::DeltaProfile => {
                        let delta = group.delta_qty().to_f64();
                        if show_text {
                            draw_cluster_text(
                                frame,
                                &abbr_large_numbers(delta),
                                Point::new(area.bars_left, y),
                                text_size,
                                text_color,
                                Alignment::Start,
                                Alignment::Center,
                            );
                        }

                        let bar_width = (delta.abs() / max_cluster_qty) as f32 * area.bars_width;
                        if bar_width > 0.0 {
                            let color = if delta >= 0.0 {
                                layout.pal.success.base.color.scale_alpha(bar_alpha)
                            } else {
                                layout.pal.danger.base.color.scale_alpha(bar_alpha)
                            };
                            frame.fill_rectangle(
                                Point::new(area.bars_left, y - (layout.cell_h / 2.0)),
                                Size::new(bar_width, layout.cell_h),
                                color,
                            );
                        }
                    }
                    _ => {}
                }

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance {
                    let higher_price = price.add_steps(1, step);

                    let rect_w = ((area.imb_marker_width - 1.0) / 2.0).max(1.0);
                    let buyside_x = area.imb_marker_left + area.imb_marker_width - rect_w;
                    let sellside_x =
                        area.imb_marker_left + area.imb_marker_width - (2.0 * rect_w) - 1.0;

                    draw_imbalance_markers(
                        frame,
                        &price_to_y,
                        footprint,
                        *price,
                        sell_qty,
                        higher_price,
                        threshold,
                        color_scale,
                        ignore_zeros,
                        layout.cell_h,
                        layout.pal,
                        buyside_x,
                        sellside_x,
                        rect_w,
                    );
                }
            }

            draw_footprint_kline(
                frame,
                &price_to_y,
                area.candle_center_x,
                layout.candle_w,
                kline,
                layout.pal,
            );
        }
        ClusterKind::Table => {
            let area = TableArea::new(
                frame,
                &price_to_y,
                content_left,
                content_right,
                layout.candle_w,
                kline,
                layout.pal,
                layout.gaps,
            );
            let table_width = area.width();
            let half_width = table_width / 2.0;
            let cell_border = 1.0;
            let grid_color = layout.pal.background.weakest.text.scale_alpha(0.32);
            for (price, group) in &footprint.trades {
                let buy_qty = group.buy_qty.to_f64();
                let sell_qty = group.sell_qty.to_f64();
                let y = price_to_y(*price);
                let row_top = y - (layout.cell_h / 2.0);

                frame.fill_rectangle(
                    Point::new(area.table_left, row_top),
                    Size::new(half_width, layout.cell_h),
                    volume_cell_background(
                        layout.pal,
                        ImbalanceSide::Sell,
                        sell_qty,
                        max_cluster_qty,
                    ),
                );
                frame.fill_rectangle(
                    Point::new(area.table_left + half_width, row_top),
                    Size::new(half_width, layout.cell_h),
                    volume_cell_background(
                        layout.pal,
                        ImbalanceSide::Buy,
                        buy_qty,
                        max_cluster_qty,
                    ),
                );
                let sell_text_color = volume_cell_text_color(
                    layout.pal,
                    ImbalanceSide::Sell,
                    sell_qty,
                    max_cluster_qty,
                    text_color,
                );
                let buy_text_color = volume_cell_text_color(
                    layout.pal,
                    ImbalanceSide::Buy,
                    buy_qty,
                    max_cluster_qty,
                    text_color,
                );

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance {
                    if let Some(alpha) = sell_imbalance_alpha(
                        footprint,
                        *price,
                        sell_qty,
                        step,
                        threshold,
                        color_scale,
                        ignore_zeros,
                    ) {
                        draw_table_imbalance_marker(
                            frame,
                            layout.pal,
                            ImbalanceSide::Sell,
                            alpha,
                            sell_qty,
                            max_cluster_qty,
                            Rectangle::new(
                                Point::new(area.table_left, row_top),
                                Size::new(half_width, layout.cell_h),
                            ),
                        );
                    }

                    if let Some(alpha) = buy_imbalance_alpha(
                        footprint,
                        *price,
                        buy_qty,
                        step,
                        threshold,
                        color_scale,
                        ignore_zeros,
                    ) {
                        draw_table_imbalance_marker(
                            frame,
                            layout.pal,
                            ImbalanceSide::Buy,
                            alpha,
                            buy_qty,
                            max_cluster_qty,
                            Rectangle::new(
                                Point::new(area.table_left + half_width, row_top),
                                Size::new(half_width, layout.cell_h),
                            ),
                        );
                    }
                }

                frame.fill_rectangle(
                    Point::new(area.table_left, row_top),
                    Size::new(table_width, cell_border),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_left, row_top + layout.cell_h - cell_border),
                    Size::new(table_width, cell_border),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_left, row_top),
                    Size::new(cell_border, layout.cell_h),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_left + half_width, row_top),
                    Size::new(cell_border, layout.cell_h),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_right - cell_border, row_top),
                    Size::new(cell_border, layout.cell_h),
                    grid_color,
                );

                if show_text {
                    draw_cluster_text(
                        frame,
                        &abbr_large_numbers(sell_qty),
                        Point::new(area.table_left + half_width - 3.0, y),
                        text_size,
                        sell_text_color,
                        Alignment::End,
                        Alignment::Center,
                    );
                    draw_cluster_text(
                        frame,
                        &abbr_large_numbers(buy_qty),
                        Point::new(area.table_left + half_width + 3.0, y),
                        text_size,
                        buy_text_color,
                        Alignment::Start,
                        Alignment::Center,
                    );
                }
            }
        }
        ClusterKind::BidAsk => {
            let area = BidAskArea::new(
                x_position,
                content_left,
                content_right,
                layout.candle_w,
                layout.gaps,
            );

            let bar_alpha = if show_text { 0.25 } else { 1.0 };

            let imb_marker_reserve = if imbalance.is_some() {
                ((area.imb_marker_width - 1.0) / 2.0).max(1.0)
            } else {
                0.0
            };

            let right_max_x =
                area.bid_area_right - imb_marker_reserve - (2.0 * layout.gaps.marker_to_bars);
            let right_area_width = (right_max_x - area.bid_area_left).max(0.0);

            let left_min_x =
                area.ask_area_left + imb_marker_reserve + (2.0 * layout.gaps.marker_to_bars);
            let left_area_width = (area.ask_area_right - left_min_x).max(0.0);

            for (price, group) in &footprint.trades {
                let buy_qty = group.buy_qty.to_f64();
                let sell_qty = group.sell_qty.to_f64();
                let y = price_to_y(*price);

                if buy_qty > 0.0 && right_area_width > 0.0 {
                    if show_text {
                        draw_cluster_text(
                            frame,
                            &abbr_large_numbers(buy_qty),
                            Point::new(area.bid_area_left, y),
                            text_size,
                            text_color,
                            Alignment::Start,
                            Alignment::Center,
                        );
                    }

                    let bar_width = (buy_qty / max_cluster_qty) as f32 * right_area_width;
                    if bar_width > 0.0 {
                        frame.fill_rectangle(
                            Point::new(area.bid_area_left, y - (layout.cell_h / 2.0)),
                            Size::new(bar_width, layout.cell_h),
                            layout.pal.success.base.color.scale_alpha(bar_alpha),
                        );
                    }
                }
                if sell_qty > 0.0 && left_area_width > 0.0 {
                    if show_text {
                        draw_cluster_text(
                            frame,
                            &abbr_large_numbers(sell_qty),
                            Point::new(area.ask_area_right, y),
                            text_size,
                            text_color,
                            Alignment::End,
                            Alignment::Center,
                        );
                    }

                    let bar_width = (sell_qty / max_cluster_qty) as f32 * left_area_width;
                    if bar_width > 0.0 {
                        frame.fill_rectangle(
                            Point::new(area.ask_area_right, y - (layout.cell_h / 2.0)),
                            Size::new(-bar_width, layout.cell_h),
                            layout.pal.danger.base.color.scale_alpha(bar_alpha),
                        );
                    }
                }

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance
                    && area.imb_marker_width > 0.0
                {
                    let higher_price = price.add_steps(1, step);

                    let rect_width = ((area.imb_marker_width - 1.0) / 2.0).max(1.0);

                    let buyside_x = area.bid_area_right - rect_width - layout.gaps.marker_to_bars;
                    let sellside_x = area.ask_area_left + layout.gaps.marker_to_bars;

                    draw_imbalance_markers(
                        frame,
                        &price_to_y,
                        footprint,
                        *price,
                        sell_qty,
                        higher_price,
                        threshold,
                        color_scale,
                        ignore_zeros,
                        layout.cell_h,
                        layout.pal,
                        buyside_x,
                        sellside_x,
                        rect_width,
                    );
                }
            }

            draw_footprint_kline(
                frame,
                &price_to_y,
                area.candle_center_x,
                layout.candle_w,
                kline,
                layout.pal,
            );
        }
    }

    if show_text && show_summary {
        let Some(summary) = FootprintSummary::from_trades(footprint) else {
            return;
        };

        let text_size = style::text_size::TINY;
        let lowest_trade_price = footprint.trades.keys().min();

        let line_spacing = (text_size * 1.2) / scaling;
        let line_height = text_size / scaling;
        let summary_padding = line_height + line_spacing + line_height;

        let summary_y = match lowest_trade_price {
            Some(p) => price_to_y(*p) + layout.cell_h / 2.0 + summary_padding,
            None => price_to_y(kline.low) + layout.cell_h / 2.0 + summary_padding,
        };

        draw_cluster_text(
            frame,
            &format!("V: {}", abbr_large_numbers(summary.total.to_f64())),
            Point::new(x_position, summary_y),
            text_size,
            layout.pal.background.weakest.text,
            Alignment::Center,
            Alignment::Start,
        );

        let delta_color = if summary.delta >= Qty::ZERO {
            layout.pal.success.base.color
        } else {
            layout.pal.danger.base.color
        };

        draw_cluster_text(
            frame,
            &format!("Δ: {}", abbr_large_numbers(summary.delta.to_f64())),
            Point::new(x_position, summary_y + line_spacing),
            text_size,
            delta_color,
            Alignment::Center,
            Alignment::Start,
        );
    }
}

fn draw_imbalance_markers(
    frame: &mut canvas::Frame,
    price_to_y: &impl Fn(Price) -> f32,
    footprint: &KlineTrades,
    price: Price,
    sell_qty: f64,
    higher_price: Price,
    threshold: usize,
    color_scale: Option<usize>,
    ignore_zeros: bool,
    cell_height: f32,
    palette: &Extended,
    buyside_x: f32,
    sellside_x: f32,
    rect_width: f32,
) {
    if ignore_zeros && sell_qty <= 0.0 {
        return;
    }

    if let Some(group) = footprint.trades.get(&higher_price) {
        let diagonal_buy_qty = group.buy_qty.to_f64();

        if ignore_zeros && diagonal_buy_qty <= 0.0 {
            return;
        }

        let rect_height = cell_height / 2.0;

        let alpha_from_ratio = |ratio: f64| -> f32 {
            if let Some(scale) = color_scale {
                let divisor = (scale as f64 / 10.0) - 1.0;
                (0.2 + 0.8 * ((ratio - 1.0) / divisor).min(1.0)).min(1.0) as f32
            } else {
                1.0
            }
        };

        if diagonal_buy_qty >= sell_qty {
            let required_qty = sell_qty * (100 + threshold) as f64 / 100.0;
            if diagonal_buy_qty > required_qty {
                let ratio = diagonal_buy_qty / required_qty;
                let alpha = alpha_from_ratio(ratio);

                let y = price_to_y(higher_price);
                frame.fill_rectangle(
                    Point::new(buyside_x, y - (rect_height / 2.0)),
                    Size::new(rect_width, rect_height),
                    imbalance_background(palette, ImbalanceSide::Buy, alpha),
                );
            }
        } else {
            let required_qty = diagonal_buy_qty * (100 + threshold) as f64 / 100.0;
            if sell_qty > required_qty {
                let ratio = sell_qty / required_qty;
                let alpha = alpha_from_ratio(ratio);

                let y = price_to_y(price);
                frame.fill_rectangle(
                    Point::new(sellside_x, y - (rect_height / 2.0)),
                    Size::new(rect_width, rect_height),
                    imbalance_background(palette, ImbalanceSide::Sell, alpha),
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ImbalanceSide {
    Buy,
    Sell,
}

fn volume_cell_background(
    palette: &Extended,
    side: ImbalanceSide,
    qty: f64,
    max_qty: f64,
) -> Color {
    const MIN_ALPHA: f32 = 0.04;

    let intensity = if max_qty > 0.0 {
        (qty / max_qty).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let alpha = MIN_ALPHA + intensity * (1.0 - MIN_ALPHA);

    match side {
        ImbalanceSide::Buy => palette.success.base.color.scale_alpha(alpha),
        ImbalanceSide::Sell => palette.danger.base.color.scale_alpha(alpha),
    }
}

fn volume_cell_text_color(
    palette: &Extended,
    _side: ImbalanceSide,
    _qty: f64,
    _max_qty: f64,
    _default_color: Color,
) -> Color {
    // DOM bid/ask: quadrados mantêm as cores (success/danger),
    // mas o texto do preço/volume é sempre branco para legibilidade.
    palette.background.base.text
}

fn draw_table_imbalance_marker(
    frame: &mut canvas::Frame,
    palette: &Extended,
    side: ImbalanceSide,
    alpha: f32,
    qty: f64,
    max_qty: f64,
    cell: Rectangle,
) {
    if cell.height <= 0.0 {
        return;
    }

    let bar_width = 2.5;
    let gap = 1.5;

    let volume_intensity = if max_qty > 0.0 {
        (qty / max_qty).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let imbalance_strength = alpha.clamp(0.0, 1.0);
    let marker_alpha = 0.38 + (volume_intensity * 0.24) + (imbalance_strength * 0.38);
    let marker_alpha = marker_alpha.clamp(0.38, 1.0);

    let color = palette.warning.strong.color.scale_alpha(marker_alpha);

    let (x, bar_w) = match side {
        ImbalanceSide::Sell => (cell.x - bar_width - gap, bar_width),
        ImbalanceSide::Buy => (cell.x + cell.width + gap, bar_width),
    };

    frame.fill_rectangle(Point::new(x, cell.y), Size::new(bar_w, cell.height), color);
}

fn imbalance_background(palette: &Extended, side: ImbalanceSide, alpha: f32) -> Color {
    let accent = match side {
        ImbalanceSide::Buy => palette.success.strong.color,
        ImbalanceSide::Sell => palette.danger.strong.color,
    };
    let alpha = alpha.clamp(0.0, 1.0);

    if palette.is_dark {
        let tint = 0.28 + (alpha * 0.32);
        mix_color(accent, palette.background.strongest.color, tint)
    } else {
        let tint = 0.18 + (alpha * 0.24);
        mix_color(accent, palette.background.weak.color, tint)
    }
}

fn buy_imbalance_alpha(
    footprint: &KlineTrades,
    price: Price,
    buy_qty: f64,
    step: PriceStep,
    threshold: usize,
    color_scale: Option<usize>,
    ignore_zeros: bool,
) -> Option<f32> {
    let lower_price = price.add_steps(-1, step);
    let diagonal_sell_qty = footprint
        .trades
        .get(&lower_price)
        .map(|group| group.sell_qty.to_f64())
        .unwrap_or_default();

    if ignore_zeros && (buy_qty <= 0.0 || diagonal_sell_qty <= 0.0) {
        return None;
    }

    imbalance_alpha(buy_qty, diagonal_sell_qty, threshold, color_scale)
}

fn sell_imbalance_alpha(
    footprint: &KlineTrades,
    price: Price,
    sell_qty: f64,
    step: PriceStep,
    threshold: usize,
    color_scale: Option<usize>,
    ignore_zeros: bool,
) -> Option<f32> {
    let higher_price = price.add_steps(1, step);
    let diagonal_buy_qty = footprint
        .trades
        .get(&higher_price)
        .map(|group| group.buy_qty.to_f64())
        .unwrap_or_default();

    if ignore_zeros && (sell_qty <= 0.0 || diagonal_buy_qty <= 0.0) {
        return None;
    }

    imbalance_alpha(sell_qty, diagonal_buy_qty, threshold, color_scale)
}

fn imbalance_alpha(
    dominant_qty: f64,
    opposite_qty: f64,
    threshold: usize,
    color_scale: Option<usize>,
) -> Option<f32> {
    let required_qty = opposite_qty * (100 + threshold) as f64 / 100.0;

    if required_qty <= 0.0 {
        return (dominant_qty > 0.0).then_some(1.0);
    }

    if dominant_qty <= required_qty {
        return None;
    }

    let ratio = dominant_qty / required_qty;
    Some(if let Some(scale) = color_scale {
        let divisor = (scale as f64 / 10.0) - 1.0;
        (0.2 + 0.8 * ((ratio - 1.0) / divisor).min(1.0)).min(1.0) as f32
    } else {
        1.0
    })
}

impl ContentGaps {
    fn from_view(candle_width: f32, scaling: f32) -> Self {
        let px = |p: f32| p / scaling;
        let base = (candle_width * 0.2).max(px(2.0));
        Self {
            marker_to_candle: base,
            candle_to_cluster: base,
            marker_to_bars: px(2.0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ContentGaps {
    /// Space between imb. markers candle body
    marker_to_candle: f32,
    /// Space between candle body and clusters
    candle_to_cluster: f32,
    /// Inner space reserved between imb. markers and clusters (used for BidAsk)
    marker_to_bars: f32,
}

/// Layout and style parameters shared across footprint cell draw functions.
struct FootprintCellLayout<'a> {
    cell_w: f32,
    cell_h: f32,
    candle_w: f32,
    pal: &'a Extended,
    cluster: ClusterKind,
    gaps: ContentGaps,
}

fn draw_cluster_text(
    frame: &mut canvas::Frame,
    text: &str,
    position: Point,
    text_size: f32,
    color: iced::Color,
    align_x: Alignment,
    align_y: Alignment,
) {
    frame.fill_text(canvas::Text {
        content: text.to_string(),
        position,
        size: iced::Pixels(text_size),
        color,
        align_x: align_x.into(),
        align_y: align_y.into(),
        font: style::AZERET_MONO,
        ..canvas::Text::default()
    });
}

fn draw_crosshair_tooltip(
    data: &PlotData<KlineDataPoint>,
    ticker_info: &TickerInfo,
    frame: &mut canvas::Frame,
    palette: &Extended,
    basis: Basis,
    at_interval: Option<u64>,
    visible_range: (u64, u64),
) {
    let (visible_earliest, visible_latest) = visible_range;

    let kline_opt = match (data, at_interval) {
        (PlotData::TimeBased(timeseries), Some(at_interval)) => {
            let in_visible = at_interval >= visible_earliest && at_interval <= visible_latest;

            timeseries
                .datapoints
                .get(&UnixMs::new(at_interval))
                .map(|dp| &dp.kline)
                .or_else(|| {
                    if in_visible {
                        let search_end = at_interval.min(visible_latest);
                        timeseries
                            .datapoints
                            .range(UnixMs::new(visible_earliest)..=UnixMs::new(search_end))
                            .next_back()
                            .map(|(_, dp)| &dp.kline)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let right_of_latest = match basis {
                        Basis::Time(_) => at_interval > visible_latest,
                        Basis::Tick(_) => at_interval < visible_earliest,
                    };

                    if right_of_latest {
                        timeseries
                            .datapoints
                            .range(UnixMs::new(visible_earliest)..=UnixMs::new(visible_latest))
                            .next_back()
                            .map(|(_, dp)| &dp.kline)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let (last_time, dp) = timeseries.datapoints.last_key_value()?;
                    (at_interval > last_time.as_u64()).then_some(&dp.kline)
                })
        }
        (PlotData::TickBased(tick_aggr), Some(at_interval)) => {
            let kline_at = |interval: u64| {
                let index = (interval / u64::from(tick_aggr.interval.0)) as usize;
                (index < tick_aggr.datapoints.len())
                    .then(|| &tick_aggr.datapoints[tick_aggr.datapoints.len() - 1 - index].kline)
            };

            let in_visible = at_interval >= visible_earliest && at_interval <= visible_latest;

            kline_at(at_interval).or_else(|| {
                let right_of_latest = match basis {
                    Basis::Time(_) => at_interval > visible_latest,
                    Basis::Tick(_) => at_interval < visible_earliest,
                };

                if in_visible || right_of_latest {
                    kline_at(visible_earliest)
                } else {
                    None
                }
            })
        }
        (PlotData::TimeBased(timeseries), None) => timeseries
            .datapoints
            .last_key_value()
            .map(|(_, dp)| &dp.kline),
        (PlotData::TickBased(tick_aggr), None) => tick_aggr.datapoints.last().map(|dp| &dp.kline),
    };

    if let Some(kline) = kline_opt {
        let change_pct = ((kline.close - kline.open) / kline.open * 100.0) as f32;
        let change_color = if change_pct >= 0.0 {
            palette.success.base.color
        } else {
            palette.danger.base.color
        };

        let base_color = palette.background.base.text;
        let precision = ticker_info.display_precision();

        let segments = [
            ("O", base_color, false),
            (&kline.open.to_string(precision), change_color, true),
            ("H", base_color, false),
            (&kline.high.to_string(precision), change_color, true),
            ("L", base_color, false),
            (&kline.low.to_string(precision), change_color, true),
            ("C", base_color, false),
            (&kline.close.to_string(precision), change_color, true),
            (&format!("{change_pct:+.2}%"), change_color, true),
        ];

        let total_width: f32 = segments
            .iter()
            .map(|(s, _, _)| s.len() as f32 * (TEXT_SIZE * 0.8))
            .sum();

        let position = Point::new(8.0, 8.0);

        let tooltip_rect = Rectangle {
            x: position.x,
            y: position.y,
            width: total_width,
            height: 16.0,
        };

        frame.fill_rectangle(
            tooltip_rect.position(),
            tooltip_rect.size(),
            palette.background.weakest.color.scale_alpha(0.9),
        );

        let mut x = position.x;
        for (text, seg_color, is_value) in segments {
            frame.fill_text(canvas::Text {
                content: text.to_string(),
                position: Point::new(x, position.y),
                size: iced::Pixels(crate::style::text_size::BODY),
                color: seg_color,
                font: style::AZERET_MONO,
                ..canvas::Text::default()
            });
            x += text.len() as f32 * 8.0;
            x += if is_value { 6.0 } else { 2.0 };
        }
    }
}

struct ProfileArea {
    imb_marker_left: f32,
    imb_marker_width: f32,
    bars_left: f32,
    bars_width: f32,
    candle_center_x: f32,
}

impl ProfileArea {
    fn new(
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        gaps: ContentGaps,
        has_imbalance: bool,
    ) -> Self {
        let candle_lane_left = if has_imbalance {
            content_left + candle_width + gaps.marker_to_candle
        } else {
            content_left
        };
        let candle_lane_width = candle_width * 0.25;

        let bars_left = candle_lane_left + candle_lane_width + gaps.candle_to_cluster;
        let bars_width = (content_right - bars_left).max(0.0);

        let candle_center_x = candle_lane_left + (candle_lane_width / 2.0);

        Self {
            imb_marker_left: content_left,
            imb_marker_width: if has_imbalance { candle_width } else { 0.0 },
            bars_left,
            bars_width,
            candle_center_x,
        }
    }
}

struct BidAskArea {
    bid_area_left: f32,
    bid_area_right: f32,
    ask_area_left: f32,
    ask_area_right: f32,
    candle_center_x: f32,
    imb_marker_width: f32,
}

impl BidAskArea {
    fn new(
        x_position: f32,
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        spacing: ContentGaps,
    ) -> Self {
        let candle_body_width = candle_width * 0.25;

        let candle_left = x_position - (candle_body_width / 2.0);
        let candle_right = x_position + (candle_body_width / 2.0);

        let ask_area_right = candle_left - spacing.candle_to_cluster;
        let bid_area_left = candle_right + spacing.candle_to_cluster;

        Self {
            bid_area_left,
            bid_area_right: content_right,
            ask_area_left: content_left,
            ask_area_right,
            candle_center_x: x_position,
            imb_marker_width: candle_width,
        }
    }
}

struct TableArea {
    table_left: f32,
    table_right: f32,
}

impl TableArea {
    fn new(
        frame: &mut canvas::Frame,
        price_to_y: &impl Fn(Price) -> f32,
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        kline: &Kline,
        palette: &Extended,
        spacing: ContentGaps,
    ) -> Self {
        let candle_center_x = content_left + candle_width / 2.0;
        draw_footprint_kline(
            frame,
            price_to_y,
            candle_center_x,
            candle_width,
            kline,
            palette,
        );

        Self {
            table_left: (content_left + candle_width + spacing.candle_to_cluster)
                .min(content_right),
            table_right: content_right,
        }
    }

    fn width(&self) -> f32 {
        (self.table_right - self.table_left).max(0.0)
    }
}

#[inline]
fn footprint_cluster_min_width(cluster_kind: ClusterKind) -> f32 {
    match cluster_kind {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => 80.0,
        ClusterKind::BidAsk => 120.0,
        ClusterKind::Table => 100.0,
    }
}

#[inline]
fn footprint_cluster_text_size(cell_height_unscaled: f32, cell_width_unscaled: f32) -> f32 {
    let text_size_from_height = cell_height_unscaled.round().min(16.0) - 3.0;
    let text_size_from_width = (cell_width_unscaled * 0.1).round().min(16.0) - 3.0;

    text_size_from_height.min(text_size_from_width)
}

#[inline]
fn price_padding_from_pixels(cell_height: f32, tick_size: f32) -> f32 {
    const OUTER_BOUND_PADDING_PX: f32 = 4.0;

    if cell_height <= f32::EPSILON {
        return 0.0;
    }

    (OUTER_BOUND_PADDING_PX / cell_height) * tick_size
}

fn footprint_summary_padding(
    cell_height: f32,
    scaling: f32,
    cell_width: f32,
    tick_size: f32,
    cluster_kind: ClusterKind,
) -> f32 {
    if cell_height <= f32::EPSILON {
        return 0.0;
    }

    let cell_height_unscaled = cell_height * scaling;
    let cell_width_unscaled = cell_width * scaling;

    if !should_show_text(
        cell_height_unscaled,
        cell_width_unscaled,
        footprint_cluster_min_width(cluster_kind),
    ) {
        return 0.0;
    }

    let text_size = style::text_size::TINY;

    let lowest_cell_bottom = cell_height / 2.0;

    let line_spacing = (text_size * 1.2) / scaling;
    let line_height = text_size / scaling;

    let summary_padding = line_height + line_spacing + line_height;
    let summary_y_start = lowest_cell_bottom + summary_padding;

    let second_line_y_start = summary_y_start + line_spacing;
    let summary_y_end = second_line_y_start + line_height;

    let extra_bottom_padding = line_height;
    let summary_y_end_with_padding = summary_y_end + extra_bottom_padding;
    let summary_ticks = summary_y_end_with_padding / cell_height;

    summary_ticks * tick_size
}

#[inline]
fn should_show_text(cell_height_unscaled: f32, cell_width_unscaled: f32, min_w: f32) -> bool {
    cell_height_unscaled > 8.0 && cell_width_unscaled > min_w
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::unit::qty::Qty;

    fn trade(time_ms: u64, price: f64, qty: f64) -> Trade {
        Trade {
            time: UnixMs::new(time_ms),
            is_sell: false,
            price: Price::from_f64(price),
            qty: Qty::from_f64(qty),
        }
    }

    fn total_qty(tape: &[Trade]) -> f64 {
        tape.iter().map(|t| t.qty.to_f64()).sum()
    }

    /// The backfill source serves the newest window it has rather than the exact window
    /// asked for, and a completed request is retried after its cooldown — so the same
    /// batch arrives over and over. Re-folding it must not add a single contract, or the
    /// footprint accumulates the whole session into its latest candles.
    #[test]
    fn refetching_the_same_batch_leaves_the_tape_untouched() {
        let batch = vec![
            trade(1_000, 158.77, 3.0),
            trade(2_000, 158.78, 1.0),
            trade(3_000, 158.77, 2.0),
        ];

        let fingerprint = |tape: &[Trade]| -> Vec<(u64, i64, f64)> {
            tape.iter()
                .map(|t| (t.time.as_u64(), t.price.units, t.qty.to_f64()))
                .collect()
        };

        let mut tape = Vec::new();
        merge_fetched_into_tape(&mut tape, &batch);
        let after_first = fingerprint(&tape);

        for _ in 0..10 {
            merge_fetched_into_tape(&mut tape, &batch);
        }

        assert_eq!(fingerprint(&tape), after_first);
        assert!((total_qty(&tape) - 6.0).abs() < 1e-9);
    }

    /// A batch overlapping trades the live feed already delivered replaces them over its
    /// own span — the overlap is the same prints from the same source, not new volume.
    #[test]
    fn an_overlapping_batch_replaces_its_window_instead_of_stacking() {
        let mut tape = vec![
            trade(1_000, 158.70, 5.0),
            trade(4_000, 158.77, 2.0),
            trade(5_000, 158.78, 1.0),
        ];

        merge_fetched_into_tape(
            &mut tape,
            &[trade(4_000, 158.77, 2.0), trade(5_000, 158.78, 1.0)],
        );

        assert_eq!(tape.len(), 3, "the overlap was replaced, not appended");
        assert!((total_qty(&tape) - 8.0).abs() < 1e-9);
    }

    /// Trades outside the batch's span are the caller's own — backfilling an older window
    /// must not drop the live prints that arrived after it.
    #[test]
    fn merging_keeps_trades_outside_the_batch_span_and_stays_sorted() {
        let mut tape = vec![trade(9_000, 158.90, 4.0)];

        merge_fetched_into_tape(
            &mut tape,
            &[trade(1_000, 158.70, 1.0), trade(2_000, 158.71, 1.0)],
        );

        let times: Vec<u64> = tape.iter().map(|t| t.time.as_u64()).collect();
        assert_eq!(times, vec![1_000, 2_000, 9_000]);
        assert!((total_qty(&tape) - 6.0).abs() < 1e-9);
    }

    /// The tape must not grow past its cap no matter how many prints arrive:
    /// oldest prints are dropped first and time order is preserved.
    #[test]
    fn tape_pruning_caps_live_growth_and_keeps_order() {
        let mut tape: Vec<Trade> = (0..MAX_RAW_TRADES as u64 + TAPE_PRUNE_BATCH as u64 + 10)
            .map(|t| trade(t, 158.70, 1.0))
            .collect();

        prune_tape(&mut tape);

        assert_eq!(tape.len(), MAX_RAW_TRADES);
        let times: Vec<u64> = tape.iter().map(|t| t.time.as_u64()).collect();
        assert!(times.windows(2).all(|w| w[0] < w[1]), "stays sorted");
        // First retained trade is the one right after the dropped prefix.
        let dropped = (MAX_RAW_TRADES + TAPE_PRUNE_BATCH + 10) - MAX_RAW_TRADES;
        assert_eq!(times[0] as usize, dropped);
        assert_eq!(
            times[times.len() - 1] as usize,
            dropped + MAX_RAW_TRADES - 1
        );
    }

    /// Below the cap (+ hysteresis batch) pruning is a no-op.
    #[test]
    fn tape_pruning_keeps_small_tapes_untouched() {
        let mut tape: Vec<Trade> = (0..1_000u64).map(|t| trade(t, 158.70, 1.0)).collect();
        prune_tape(&mut tape);
        assert_eq!(tape.len(), 1_000);
        assert_eq!(tape[0].time.as_u64(), 0);
    }
}
