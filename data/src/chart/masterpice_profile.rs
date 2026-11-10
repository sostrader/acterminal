//! Masterpice Profile: Leilão + Volume Profile same-day em tempo real.
//!
//! Versão B do manual (`manual_indicador_leilao_volume_profile_v2.md` §3.2/§4.2).
//! Núcleo do leilão vive em [`crate::chart::masterpice`] (Alvo1/Alvo2/Alvo3);
//! este módulo adiciona APENAS o perfil de volume do dia corrente, tick a tick.
//!
//! Fonte de volume: trades reais da sessão em andamento (live `TradesReceived`
//! + backfill de trades do mesmo dia). Nomes do manual:
//! - POC (Point of Control) = preço com maior volume acumulado do dia
//! - Value Area (VA) = faixa que concentra `value_area_pct` do volume (padrão 70%)
//! - VAH / VAL = topo e base da Value Area
//! - Promoção de pivô = se POC ficar dentro da tolerância de um Alvo2 fraco,
//!   esse nível vira o Alvo1 ativo
//!
//! Disciplina de fonte: volume SEMPRE same-day (`day_key` UTC, rollover no dia
//! novo). Nenhum cálculo de outros indicadores é importado aqui — o
//! `CandleVolumeProfile` existente serviu só de inspiração de plumbing
//! (acumular por bin de `tick_size`, incremental + prune), mas a janela dele
//! (trailing N horas) NÃO é usada: aqui a janela é o pregão corrente.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Tick real da sessão (trade): tempo em ms, preço e quantidade.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SessionTick {
    pub time_ms: i64,
    pub price: f64,
    pub qty: f64,
}

/// Perfil de volume do dia até o momento (nomes do manual).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolumeProfile {
    pub poc: f64,
    pub vah: f64,
    pub val: f64,
    pub total_volume: f64,
}

/// Constrói o perfil distribuindo cada trade integralmente no seu bin de preço
/// (`tick_size` do ativo, ex: 0.5 p/ WDO). Batch puro — usado em backfill/testes.
/// O store incremental abaixo usa a mesma regra tick a tick.
pub fn construir_volume_profile(
    ticks: &[SessionTick],
    tick_size: f64,
    value_area_pct: f64,
) -> Option<VolumeProfile> {
    if ticks.is_empty() || tick_size <= 0.0 {
        return None;
    }
    let mut hist: BTreeMap<i64, f64> = BTreeMap::new();
    let key_of = |p: f64| (p / tick_size).round() as i64;
    for t in ticks {
        if t.qty <= 0.0 {
            continue;
        }
        *hist.entry(key_of(t.price)).or_insert(0.0) += t.qty;
    }
    if hist.is_empty() {
        return None;
    }
    let (&poc_key, _) = hist.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
    let total: f64 = hist.values().sum();
    let mut ordenado: Vec<(&i64, &f64)> = hist.iter().collect();
    ordenado.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
    let mut acumulado = 0.0;
    let mut va_keys: Vec<i64> = Vec::new();
    let alvo = value_area_pct.clamp(0.0, 1.0) * total;
    for (k, v) in ordenado {
        acumulado += *v;
        va_keys.push(*k);
        if acumulado >= alvo {
            break;
        }
    }
    let vah = *va_keys.iter().max()? as f64 * tick_size;
    let val = *va_keys.iter().min()? as f64 * tick_size;
    Some(VolumeProfile {
        poc: poc_key as f64 * tick_size,
        vah,
        val,
        total_volume: total,
    })
}

/// Promoção dinâmica do Alvo1 (nome do manual): se o POC real ficar dentro da
/// tolerância de um dos extremos do núcleo (Alvo2), o pivô ativo passa a ser o
/// POC. Chamar a cada novo trade/candle durante o pregão.
pub fn promover_pivo_se_necessario(
    alvo1_atual: f64,
    alvo2_extremo_alta: f64,
    alvo2_extremo_baixa: f64,
    vp: &VolumeProfile,
    tolerancia: f64,
) -> f64 {
    if (vp.poc - alvo2_extremo_alta).abs() <= tolerancia
        || (vp.poc - alvo2_extremo_baixa).abs() <= tolerancia
    {
        vp.poc
    } else {
        alvo1_atual
    }
}

/// Store incremental tick-a-tick, same-day, do Masterpice Profile.
///
/// - `tick_size`: grade do ativo (WDO = 0.5). Trocar re-bina o dia.
/// - `value_area_pct`: fração da VA (padrão 0.70).
/// - Rollover: primeiro trade de um dia novo limpa o dia anterior.
/// - Deduplicação de backfill: `loaded_ts` por dia; batch antigo não duplica.
#[derive(Debug, Clone)]
pub struct VolumeDayStore {
    hist: BTreeMap<i64, f64>,
    session_day: Option<i64>,
    tick_size: f64,
    value_area_pct: f64,
    total: f64,
    seen_ts: std::collections::HashSet<i64>,
    version: u64,
}

impl Default for VolumeDayStore {
    fn default() -> Self {
        Self {
            hist: BTreeMap::new(),
            session_day: None,
            tick_size: 0.5,
            value_area_pct: 0.70,
            total: 0.0,
            seen_ts: std::collections::HashSet::new(),
            version: 0,
        }
    }
}

impl VolumeDayStore {
    pub fn new(tick_size: f64, value_area_pct: f64) -> Self {
        Self {
            tick_size: if tick_size > 0.0 { tick_size } else { 0.5 },
            value_area_pct: value_area_pct.clamp(0.0, 1.0),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hist.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn session_day(&self) -> Option<i64> {
        self.session_day
    }

    pub fn set_tick_size(&mut self, tick_size: f64, ticks_do_dia: &[SessionTick]) {
        if tick_size <= 0.0 || (tick_size - self.tick_size).abs() < f64::EPSILON {
            return;
        }
        self.tick_size = tick_size;
        // Re-bina o dia corrente com os ticks guardados pelo chamador.
        self.hist.clear();
        self.total = 0.0;
        self.seen_ts.clear();
        let day = self.session_day;
        for t in ticks_do_dia {
            if day.is_none_or(|d| crate::chart::masterpice::day_key(t.time_ms) == d) {
                self.insert_one(*t);
            }
        }
        self.version += 1;
    }

    fn key_of(&self, price: f64) -> i64 {
        (price / self.tick_size).round() as i64
    }

    fn insert_one(&mut self, t: SessionTick) {
        if t.qty <= 0.0 {
            return;
        }
        let k = self.key_of(t.price);
        *self.hist.entry(k).or_insert(0.0) += t.qty;
        self.total += t.qty;
    }

    /// Ingere trades live tick a tick. Retorna true se o perfil mudou.
    pub fn insert_ticks(&mut self, ticks: &[SessionTick]) -> bool {
        let mut changed = false;
        for t in ticks {
            if t.qty <= 0.0 {
                continue;
            }
            let day = crate::chart::masterpice::day_key(t.time_ms);
            match self.session_day {
                None => self.session_day = Some(day),
                Some(cur) if cur != day => {
                    if day < cur {
                        continue;
                    }
                    self.hist.clear();
                    self.total = 0.0;
                    self.seen_ts.clear();
                    self.session_day = Some(day);
                }
                Some(_) => {}
            }
            // Backfill redeliverado (mesmo ms + mesmo preço/qty): ignora.
            // Chave simples por (time_ms); trades distintos no mesmo ms são
            // raros no feed agregado e o batch dedicado abaixo trata o resto.
            if !self.seen_ts.insert(t.time_ms) {
                // Permite múltiplos trades no mesmo ms vindos do live:
                // acumula mesmo assim, mas o bootstrap usa insert_batch.
                self.insert_one(*t);
                changed = true;
                continue;
            }
            self.insert_one(*t);
            changed = true;
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Ingere batch de backfill sem duplicar o que o live já viu:
    /// só soma ticks com `time_ms` ainda não registrado no dia corrente.
    pub fn insert_batch(&mut self, ticks: &[SessionTick]) -> bool {
        let mut changed = false;
        for t in ticks {
            if t.qty <= 0.0 {
                continue;
            }
            let day = crate::chart::masterpice::day_key(t.time_ms);
            match self.session_day {
                None => self.session_day = Some(day),
                Some(cur) if cur != day => {
                    if day < cur {
                        continue;
                    }
                    // Batch de dia novo: assume o dia (bootstrap frio).
                    self.hist.clear();
                    self.total = 0.0;
                    self.seen_ts.clear();
                    self.session_day = Some(day);
                }
                Some(_) => {}
            }
            if self.seen_ts.contains(&t.time_ms) {
                continue;
            }
            self.seen_ts.insert(t.time_ms);
            self.insert_one(*t);
            changed = true;
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    pub fn profile(&self) -> Option<VolumeProfile> {
        if self.hist.is_empty() {
            return None;
        }
        let (&poc_key, _) = self
            .hist
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
        let mut ordenado: Vec<(&i64, &f64)> = self.hist.iter().collect();
        ordenado.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
        let alvo = self.value_area_pct.clamp(0.0, 1.0) * self.total;
        let mut acumulado = 0.0;
        let mut va_keys: Vec<i64> = Vec::new();
        for (k, v) in ordenado {
            acumulado += *v;
            va_keys.push(*k);
            if acumulado >= alvo {
                break;
            }
        }
        Some(VolumeProfile {
            poc: poc_key as f64 * self.tick_size,
            vah: *va_keys.iter().max()? as f64 * self.tick_size,
            val: *va_keys.iter().min()? as f64 * self.tick_size,
            total_volume: self.total,
        })
    }

    pub fn clear(&mut self) {
        self.hist.clear();
        self.total = 0.0;
        self.seen_ts.clear();
        self.session_day = None;
        self.version += 1;
    }

    /// Níveis ordenados (preço, volume) p/ desenho do histograma lateral.
    pub fn levels(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.hist
            .iter()
            .map(|(k, v)| (*k as f64 * self.tick_size, *v))
    }

    pub fn max_volume(&self) -> f64 {
        self.hist.values().copied().fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: i64, price: f64, qty: f64) -> SessionTick {
        SessionTick {
            time_ms: ms,
            price,
            qty,
        }
    }

    #[test]
    fn poc_vah_val_same_day() {
        let ticks = vec![
            t(1000, 100.0, 10.0),
            t(2000, 100.5, 5.0),
            t(3000, 100.0, 7.0),
            t(4000, 101.0, 2.0),
        ];
        let vp = construir_volume_profile(&ticks, 0.5, 0.70).unwrap();
        assert_eq!(vp.poc, 100.0);
        assert_eq!(vp.total_volume, 24.0);
        // 70% de 24 = 16.8: 17 (100.0) sozinho já cobre.
        assert_eq!((vp.vah, vp.val), (100.0, 100.0));
        let vp50 = construir_volume_profile(&ticks, 0.5, 0.50).unwrap();
        assert_eq!(vp50.poc, 100.0);
    }

    #[test]
    fn store_tick_a_tick_rollover_e_batch_sem_duplo() {
        let day1 = chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day2 = day1 + 86_400_000;
        let mut st = VolumeDayStore::new(0.5, 0.70);
        assert!(st.insert_ticks(&[t(day1, 100.0, 10.0), t(day1 + 1000, 100.5, 5.0)]));
        assert_eq!(st.profile().unwrap().poc, 100.0);
        let v = st.version();
        // Batch redeliverado com mesmos ms: no-op.
        assert!(!st.insert_batch(&[t(day1, 100.0, 10.0)]));
        assert_eq!(st.version(), v);
        // Dia novo: limpa e recomeça.
        assert!(st.insert_ticks(&[t(day2, 200.0, 3.0)]));
        let vp = st.profile().unwrap();
        assert_eq!(vp.poc, 200.0);
        assert_eq!(vp.total_volume, 3.0);
    }

    #[test]
    fn promove_pivo_dentro_e_fora_da_tolerancia() {
        let vp = VolumeProfile {
            poc: 103.0,
            vah: 103.0,
            val: 102.0,
            total_volume: 100.0,
        };
        // Dentro da tolerância do Alvo2 alta -> promove.
        assert_eq!(
            promover_pivo_se_necessario(100.0, 103.5, 99.0, &vp, 1.0),
            103.0
        );
        // Longe de ambos -> mantém.
        assert_eq!(
            promover_pivo_se_necessario(100.0, 110.0, 90.0, &vp, 1.0),
            100.0
        );
    }
}
