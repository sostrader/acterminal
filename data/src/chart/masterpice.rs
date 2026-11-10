//! Masterpice: indicador de Nós de Leilão (Versão A do manual).
//!
//! Núcleo compartilhado pelo Masterpice (estático) e pelo Masterpice Profile
//! (dinâmico). Fonte exclusiva: ticks do leilão pré-abertura
//! (`premarket/` live + bootstrap `hist/premarket`).
//!
//! NÃO importa nenhum cálculo de outros indicadores (Magnets, Premarket H/L,
//! TheoricTargets, CandleVolumeProfile). Apenas o plumbing de extração
//! (tipos de payload/fetch) é reaproveitado nas camadas acima.
//!
//! Nomenclatura conforme o manual `manual_indicador_leilao_volume_profile_v2.md`:
//! - Tick de leilão = (ts, price) do preço teórico de abertura (PTA)
//! - Run = sequência de ticks consecutivos com mesmo preço (nó candidato)
//! - N = índice sequencial do run (ordem cronológica, NÃO mede força)
//! - V = nº de atualizações de book com preço parado naquele nível
//! - T = tempo (segundos) entre início deste run e início do próximo run
//! - score = V × (1 + T)
//! - Classes: PivoForte / Relevante / Transicao / Ruido
//! - Alvo1 (Pivô) = preço do nó de maior score
//! - Alvo2 (Extremo Alta/Baixa) = max/min entre nós PivoForte|Relevante
//! - Amplitude Núcleo = Alvo2_alta − Alvo2_baixa
//! - Alvo3 (Projeção) = Alvo2 ± Amplitude (só ativo após rompimento c/ fechamento)
//! - Abertura Estimada = preço do último nó (último PTA vira abertura oficial)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Um tick bruto do leilão: preço teórico de abertura num instante (ms).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AuctionTick {
    pub ts: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeClass {
    PivoForte,
    Relevante,
    Transicao,
    Ruido,
}

/// Limiares de classificação — parâmetros por ativo, NÃO constantes universais.
/// Calibrar com percentis do histórico de scores (ex: p90/p60/p30).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LimiaresClassificacao {
    pub pivo_forte_min: f64,
    pub relevante_min: f64,
    pub transicao_min: f64,
}

impl Default for LimiaresClassificacao {
    /// Ponto de partida — ajustar por ativo antes de produção.
    fn default() -> Self {
        Self {
            pivo_forte_min: 100.0,
            relevante_min: 20.0,
            transicao_min: 5.0,
        }
    }
}

impl NodeClass {
    pub fn from_score(score: f64, limiares: &LimiaresClassificacao) -> Self {
        if score >= limiares.pivo_forte_min {
            NodeClass::PivoForte
        } else if score >= limiares.relevante_min {
            NodeClass::Relevante
        } else if score >= limiares.transicao_min {
            NodeClass::Transicao
        } else {
            NodeClass::Ruido
        }
    }
}

/// Um nó de leilão processado: preço + métricas N/V/T + score + classe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AuctionNode {
    pub n: usize,
    pub price: f64,
    pub n_updates: u32,
    pub duration_s: f64,
    pub score: f64,
    pub class: NodeClass,
}

/// Reconstrói os nós (runs) a partir dos ticks do leilão ordenados por tempo.
pub fn reconstruir_nos(
    ticks: &[AuctionTick],
    limiares: &LimiaresClassificacao,
) -> Vec<AuctionNode> {
    let mut nodes = Vec::new();
    if ticks.is_empty() {
        return nodes;
    }
    let mut i = 0usize;
    let mut n = 0usize;
    while i < ticks.len() {
        let price = ticks[i].price;
        let start = ticks[i].ts;
        let mut j = i;
        while j < ticks.len() && ticks[j].price == price {
            j += 1;
        }
        let n_updates = (j - i) as u32;
        let duration_s = if j < ticks.len() {
            (ticks[j].ts - start).max(0) as f64 / 1000.0
        } else {
            // Último run: sem próximo início; T fica aberto (0 se tick único).
            (ticks[j - 1].ts - start).max(0) as f64 / 1000.0
        };
        let score = n_updates as f64 * (1.0 + duration_s);
        let class = NodeClass::from_score(score, limiares);
        n += 1;
        nodes.push(AuctionNode {
            n,
            price,
            n_updates,
            duration_s,
            score,
            class,
        });
        i = j;
    }
    nodes
}

/// Direção prioritária do dia, lida no próprio leilão (não adivinhada).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direcao {
    Alta,
    Baixa,
    Neutra,
}

/// Viés direcional do leilão: `(fechamento − pivô) / amplitude_núcleo`.
/// Sinal + → o leilão fechou se afastando do valor justo para cima (prioridade
/// compradora desde a abertura); sinal − → o inverso. `Neutra` quando a
/// amplitude é zero ou o fechamento coincide com o pivô.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vies {
    pub direcao: Direcao,
    pub valor: f64,
}

pub fn calcular_vies(fechamento: f64, pivo: f64, amplitude_nucleo: f64) -> Vies {
    if amplitude_nucleo <= f64::EPSILON {
        return Vies {
            direcao: Direcao::Neutra,
            valor: 0.0,
        };
    }
    let valor = (fechamento - pivo) / amplitude_nucleo;
    let direcao = if valor > 0.0 {
        Direcao::Alta
    } else if valor < 0.0 {
        Direcao::Baixa
    } else {
        Direcao::Neutra
    };
    Vies { direcao, valor }
}

/// Sequência de alvos derivada apenas do leilão (nomes do manual).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Alvos {
    pub abertura_estimada: f64,
    pub alvo1_pivo: f64,
    pub alvo2_extremo_alta: f64,
    pub alvo2_extremo_baixa: f64,
    pub amplitude_nucleo: f64,
    pub alvo3_projecao_alta: f64,
    pub alvo3_projecao_baixa: f64,
    /// Piso duro: mínima absoluta de TODOS os ticks do leilão (inclusive
    /// Ruído). Limite de invalidação do viés de alta — NÃO é alvo. Distinto do
    /// Alvo2 Baixa, que usa só os nós fortes.
    pub piso_duro: f64,
    /// Teto duro: máxima absoluta de TODOS os ticks do leilão (inclusive
    /// Ruído). Limite de invalidação do viés de baixa.
    pub teto_duro: f64,
    /// Viés lido no fechamento do leilão (hierarquia, não simetria).
    pub vies: Vies,
    /// Alvo3 de alta é o alvo de trabalho desde a abertura quando o viés é de
    /// alta. Dormente caso contrário.
    pub alvo3_alta_ativo: bool,
    /// Alvo3 de baixa é o alvo de trabalho desde a abertura quando o viés é de
    /// baixa. Dormente caso contrário.
    pub alvo3_baixa_ativo: bool,
}

pub fn gerar_alvos(nodes: &[AuctionNode]) -> Option<Alvos> {
    if nodes.is_empty() {
        return None;
    }
    let pivo = nodes
        .iter()
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())?;
    let nucleo: Vec<&AuctionNode> = nodes
        .iter()
        .filter(|node| matches!(node.class, NodeClass::PivoForte | NodeClass::Relevante))
        .collect();
    let nucleo_ref: Vec<&AuctionNode> = if nucleo.is_empty() {
        nodes.iter().collect()
    } else {
        nucleo
    };
    let ext_alta = nucleo_ref.iter().map(|n| n.price).fold(f64::MIN, f64::max);
    let ext_baixa = nucleo_ref.iter().map(|n| n.price).fold(f64::MAX, f64::min);
    let amplitude = ext_alta - ext_baixa;
    let abertura_estimada = nodes.iter().max_by_key(|n| n.n)?.price;
    // Piso/teto sobre TODOS os nós (os runs cobrem todos os ticks, inclusive
    // os classificados como Ruído) — nunca só sobre o núcleo.
    let piso_duro = nodes.iter().map(|n| n.price).fold(f64::MAX, f64::min);
    let teto_duro = nodes.iter().map(|n| n.price).fold(f64::MIN, f64::max);
    let vies = calcular_vies(abertura_estimada, pivo.price, amplitude);
    let alvo3_alta_ativo = vies.direcao == Direcao::Alta;
    let alvo3_baixa_ativo = vies.direcao == Direcao::Baixa;
    Some(Alvos {
        abertura_estimada,
        alvo1_pivo: pivo.price,
        alvo2_extremo_alta: ext_alta,
        alvo2_extremo_baixa: ext_baixa,
        amplitude_nucleo: amplitude,
        alvo3_projecao_alta: ext_alta + amplitude,
        alvo3_projecao_baixa: ext_baixa - amplitude,
        piso_duro,
        teto_duro,
        vies,
        alvo3_alta_ativo,
        alvo3_baixa_ativo,
    })
}

/// Reavalia os Alvo3 dormentes diante de um fechamento de candle da sessão
/// real (regra de ativação, não simetria):
/// - fechamento abaixo do piso duro → viés de alta invalidado, Alvo3 Baixa acorda;
/// - fechamento acima do teto duro → viés de baixa invalidado, Alvo3 Alta acorda.
/// Toque com pavio NÃO ativa (exige fechamento). Função pura para o robô
/// chamar tick a tick / candle a candle; o store estático não muda sozinho.
pub fn avaliar_ativacao_por_fechamento(alvos: &Alvos, fechamento_sessao: f64) -> (bool, bool) {
    let mut alta = alvos.alvo3_alta_ativo;
    let mut baixa = alvos.alvo3_baixa_ativo;
    if fechamento_sessao < alvos.piso_duro {
        alta = false;
        baixa = true;
    } else if fechamento_sessao > alvos.teto_duro {
        baixa = false;
        alta = true;
    }
    (alta, baixa)
}

/// Store incremental do Masterpice (Versão A — estática, congela no close).
///
/// Alimentado por `premarket/` live + bootstrap `hist/premarket`, um ponto por
/// snapshot `(ts, price)`. Sessão day-keyed (YYYYMMDD UTC, source-driven com
/// prorrogação incluída), watermark `loaded_ts` anti-duplo, rollover p/ dia novo.
#[derive(Debug, Clone, Default)]
pub struct MasterpiceStore {
    raw: Vec<AuctionTick>,
    nodes: Vec<AuctionNode>,
    alvos: Option<Alvos>,
    session_day: Option<i64>,
    loaded_ts: Option<i64>,
    limiares: Option<LimiaresClassificacao>,
    version: u64,
    /// Preços distintos em ordem de primeiro toque (p/ day_key reverso/debug).
    order: Vec<f64>,
    seen: HashMap<i64, usize>,
}

impl MasterpiceStore {
    pub fn new(limiares: LimiaresClassificacao) -> Self {
        Self {
            limiares: Some(limiares),
            ..Self::default()
        }
    }

    pub fn nodes(&self) -> &[AuctionNode] {
        &self.nodes
    }

    pub fn alvos(&self) -> Option<&Alvos> {
        self.alvos.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn session_day(&self) -> Option<i64> {
        self.session_day
    }

    pub fn set_limiares(&mut self, limiares: LimiaresClassificacao) {
        self.limiares = Some(limiares);
        self.refresh();
    }

    /// Substitui a sessão pelo snapshot do bootstrap (histórico persistido).
    pub fn bootstrap<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = AuctionTick>,
    {
        let mut raw: Vec<AuctionTick> = points.into_iter().collect();
        raw.sort_by_key(|t| t.ts);
        raw.dedup_by_key(|t| t.ts);
        let max_ts = raw.iter().map(|t| t.ts).max();
        let before = (self.nodes.clone(), self.alvos);
        self.raw = raw;
        self.session_day = max_ts.map(day_key);
        self.loaded_ts = max_ts;
        self.rebuild();
        if self.nodes != before.0 || self.alvos != before.1 {
            self.version += 1;
        }
    }

    /// Ingere um tick live do `premarket/`. Apenas mesmo dia, mais novo que o
    /// watermark; dia novo faz rollover (pane aberta antes do leilão de hoje).
    pub fn add_point(&mut self, tick: AuctionTick) {
        let day = day_key(tick.ts);
        match self.session_day {
            None => self.session_day = Some(day),
            Some(cur) if cur != day => {
                if day < cur {
                    return;
                }
                self.raw.clear();
                self.nodes.clear();
                self.alvos = None;
                self.order.clear();
                self.seen.clear();
                self.loaded_ts = None;
                self.session_day = Some(day);
            }
            Some(_) => {}
        }
        if let Some(wm) = self.loaded_ts
            && tick.ts <= wm
        {
            return;
        }
        self.loaded_ts = Some(self.loaded_ts.map_or(tick.ts, |w| w.max(tick.ts)));
        // Insere ordenado (live chega ordenado; bootstrap já ordena).
        match self.raw.binary_search_by_key(&tick.ts, |t| t.ts) {
            Ok(_) => return, // ts duplicado: ignora
            Err(pos) => self.raw.insert(pos, tick),
        }
        self.rebuild();
        self.version += 1;
    }

    fn rebuild(&mut self) {
        let lim = self.limiares.unwrap_or_default();
        let day = self.session_day;
        let ticks: Vec<AuctionTick> = match day {
            Some(d) => self
                .raw
                .iter()
                .copied()
                .filter(|t| day_key(t.ts) == d)
                .collect(),
            None => self.raw.clone(),
        };
        self.nodes = reconstruir_nos(&ticks, &lim);
        self.alvos = gerar_alvos(&self.nodes);
        self.order.clear();
        self.seen.clear();
        for node in &self.nodes {
            let k = price_key(node.price);
            if !self.seen.contains_key(&k) {
                self.seen.insert(k, self.order.len());
                self.order.push(node.price);
            }
        }
    }

    fn refresh(&mut self) {
        self.rebuild();
        self.version += 1;
    }
}

fn price_key(price: f64) -> i64 {
    (price * 1e11).round() as i64
}

/// YYYYMMDD (UTC) p/ timestamp em ms (Howard Hinnant).
pub fn day_key(ts_ms: i64) -> i64 {
    let secs = ts_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y += i64::from(m <= 2);
    y * 10_000 + m * 100 + d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(ts: i64, price: f64) -> AuctionTick {
        AuctionTick { ts, price }
    }

    #[test]
    fn nos_n_v_t_score_classe() {
        // Preço 100 parado 2 updates (0s..10s), depois 101 (1 update), depois 100.
        let lim = LimiaresClassificacao {
            pivo_forte_min: 100.0,
            relevante_min: 20.0,
            transicao_min: 5.0,
        };
        let ticks = vec![
            tick(0, 100.0),
            tick(5_000, 100.0),
            tick(10_000, 101.0),
            tick(12_000, 100.0),
        ];
        let nodes = reconstruir_nos(&ticks, &lim);
        assert_eq!(nodes.len(), 3);
        assert_eq!((nodes[0].n, nodes[0].n_updates), (1, 2));
        assert!((nodes[0].duration_s - 10.0).abs() < 1e-9);
        assert!((nodes[0].score - 2.0 * 11.0).abs() < 1e-9);
        assert_eq!(nodes[0].class, NodeClass::Relevante);
        assert_eq!(nodes[1].price, 101.0);
        assert!((nodes[1].duration_s - 2.0).abs() < 1e-9);
        // Último run sem próximo: T=0, score=V.
        assert_eq!(nodes[2].duration_s, 0.0);
        assert_eq!(nodes[2].score, 1.0);
        assert_eq!(nodes[2].class, NodeClass::Ruido);
    }

    #[test]
    fn alvos_pivo_extremos_amplitude_projecao() {
        let lim = LimiaresClassificacao::default();
        let ticks = vec![
            tick(0, 100.0),
            tick(1_000, 100.0),
            tick(30_000, 100.0), // V=3, T=30s+ -> score alto
            tick(31_000, 103.0),
            tick(32_000, 103.0), // V=2
            tick(60_000, 103.0), // dwell adicional
            tick(61_000, 101.0), // último = abertura estimada
        ];
        let nodes = reconstruir_nos(&ticks, &lim);
        let alvos = gerar_alvos(&nodes).unwrap();
        assert_eq!(alvos.abertura_estimada, 101.0);
        assert_eq!(alvos.alvo1_pivo, 100.0);
        assert!(alvos.alvo2_extremo_alta >= alvos.alvo2_extremo_baixa);
        assert!(
            (alvos.amplitude_nucleo - (alvos.alvo2_extremo_alta - alvos.alvo2_extremo_baixa)).abs()
                < 1e-9
        );
        assert!(
            (alvos.alvo3_projecao_alta - (alvos.alvo2_extremo_alta + alvos.amplitude_nucleo)).abs()
                < 1e-9
        );
        assert!(
            (alvos.alvo3_projecao_baixa - (alvos.alvo2_extremo_baixa - alvos.amplitude_nucleo))
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn store_bootstrap_live_watermark_rollover() {
        let mut st = MasterpiceStore::new(LimiaresClassificacao::default());
        let day1 = chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(11, 55, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let day2 = day1 + 86_400_000;
        st.bootstrap([tick(day1, 100.0), tick(day1 + 10_000, 101.0)]);
        assert!(!st.is_empty());
        let v = st.version();
        // Replay do tail: watermark ignora.
        st.add_point(tick(day1 + 5_000, 100.0));
        assert_eq!(st.version(), v);
        // Dia novo: rollover constrói do zero.
        st.add_point(tick(day2, 200.0));
        st.add_point(tick(day2 + 5_000, 200.0));
        assert_eq!(st.nodes().len(), 1);
        assert_eq!(st.alvos().unwrap().alvo1_pivo, 200.0);
    }

    #[test]
    fn vies_piso_teto_e_alvo_ativo() {
        // Sessão do screenshot: pivô 5131.5, fechamento 5135, núcleo 5130–5150.
        let lim = LimiaresClassificacao::default();
        let ticks = vec![
            tick(0, 5150.0),
            tick(1_000, 5150.0),
            tick(30_000, 5150.0), // score alto -> núcleo/alta
            tick(31_000, 5131.5),
            tick(32_000, 5131.5),
            tick(33_000, 5131.5),
            tick(34_000, 5131.5),
            tick(35_000, 5131.5),  // V=5, pivô dominante
            tick(140_000, 5130.0), // núcleo/baixa
            tick(141_000, 5130.0),
            tick(170_000, 5130.0),
            tick(200_000, 5128.0), // piso duro em nó de Ruído
            tick(210_000, 5199.0), // teto duro em nó de Ruído
            tick(220_000, 5135.0), // último = fechamento/abertura estimada
        ];
        let nodes = reconstruir_nos(&ticks, &lim);
        let alvos = gerar_alvos(&nodes).unwrap();
        assert_eq!(alvos.alvo1_pivo, 5131.5);
        // Piso/teto sobre TODOS os ticks, não só o núcleo.
        assert_eq!(alvos.piso_duro, 5128.0);
        assert_eq!(alvos.teto_duro, 5199.0);
        assert!(alvos.piso_duro < alvos.alvo2_extremo_baixa);
        // Viés: (5135 − 5131.5)/20 = +0.175 → Alta, só Alvo3 Alta ativo.
        assert_eq!(alvos.vies.direcao, Direcao::Alta);
        assert!((alvos.vies.valor - 0.175).abs() < 1e-9);
        assert!(alvos.alvo3_alta_ativo);
        assert!(!alvos.alvo3_baixa_ativo);
    }

    #[test]
    fn alvo_dormente_acorda_com_fechamento_alem_do_duro() {
        let alvos = Alvos {
            abertura_estimada: 5135.0,
            alvo1_pivo: 5131.5,
            alvo2_extremo_alta: 5150.0,
            alvo2_extremo_baixa: 5130.0,
            amplitude_nucleo: 20.0,
            alvo3_projecao_alta: 5170.0,
            alvo3_projecao_baixa: 5110.0,
            piso_duro: 5128.0,
            teto_duro: 5199.0,
            vies: Vies {
                direcao: Direcao::Alta,
                valor: 0.175,
            },
            alvo3_alta_ativo: true,
            alvo3_baixa_ativo: false,
        };
        // Dentro dos duros: hierarquia mantida.
        assert_eq!(
            avaliar_ativacao_por_fechamento(&alvos, 5140.0),
            (true, false)
        );
        // Pavio não ativa — mas esta função só recebe FECHAMENTO; quem chama
        // deve passar o close do candle, nunca a mínima do pavio.
        assert_eq!(
            avaliar_ativacao_por_fechamento(&alvos, 5128.0),
            (true, false)
        );
        // Fechamento abaixo do piso: invalida a alta, acorda a baixa.
        assert_eq!(
            avaliar_ativacao_por_fechamento(&alvos, 5127.5),
            (false, true)
        );
        // Fechamento acima do teto com viés de baixa: o inverso.
        let mut baixa = alvos;
        baixa.vies.direcao = Direcao::Baixa;
        baixa.alvo3_alta_ativo = false;
        baixa.alvo3_baixa_ativo = true;
        assert_eq!(
            avaliar_ativacao_por_fechamento(&baixa, 5200.0),
            (true, false)
        );
    }

    #[test]
    fn vies_neutro_com_amplitude_zero() {
        let v = calcular_vies(100.0, 100.0, 0.0);
        assert_eq!(v.direcao, Direcao::Neutra);
        // Um único nível: amplitude 0 → ninguém ativo desde a abertura.
        let nodes = reconstruir_nos(
            &[tick(0, 100.0), tick(1_000, 100.0)],
            &LimiaresClassificacao::default(),
        );
        let alvos = gerar_alvos(&nodes).unwrap();
        assert_eq!(alvos.vies.direcao, Direcao::Neutra);
        assert!(!alvos.alvo3_alta_ativo && !alvos.alvo3_baixa_ativo);
    }
}
