//! Masterpice V2 (experimental): Nós de Leilão com V = fila parada.
//!
//! A V1 (`[`crate::chart::masterpice`]`) conta snapshots (`V = nº de prints`),
//! o que premia retransmissão do feed (heartbeat) e credita silêncio como
//! dwell. A V2 troca o volumepor dinheiro parado de verdade, sem reescrever
//! a V1 — ambas convivem para comparação.
//!
//! Fórmula única (a mesma do manual §8 — máximo vs soma vs integral foi a
//! ambiguidade que originou o bug; aqui só existe UMA definição):
//!
//! - `size_k` = fila parada exatamente no preço no snapshot k
//!   (`bid_at + ask_at`, contratos; vem de `PremarketPayload.size`).
//! - `fila_parada(R)` = Σ `size_k × (t_{k+1} − t_k)` sobre o run (zero-order
//!   hold: o último print do run é segurado até o início do próximo run; no
//!   run final da sessão, até o próprio último ts — 0 se print único).
//!   Unidade: contratos×segundo. Pico de 1 tick não domina sozinho porque é
//!   ponderado pelo tempo que a fila de fato esperou.
//! - `mudanças(R)` = nº de snapshots do run (após o primeiro) cujo `size`
//!   de fato variou (`|Δ| > 1e-9`). Reenvio idêntico NÃO conta — é por aqui
//!   que o heartbeat deixa de inflar o score.
//! - `confirmações(R)` = `1 + mudanças`. Elegível para Relevante+ só com
//!   `confirmações >= min_confirmacoes` (default 2): 1 print sozinho nunca
//!   passa de TRANSICAO, não importa o T (caso 5100).
//! - `V(R)` = `fila_parada × (1 + mudanças)`.
//! - `T_cap` = `mediana dos gaps entre prints da sessão × t_cap_mult`
//!   (default 4). `T` do run é `min(T_bruto, T_cap)` — defesa contra feed
//!   travado por minutos, sem número mágico por ativo.
//! - `score(R)` = `V × (1 + T_cap)`.
//! - Classes por percentis dos scores elegíveis (p90/p60/p30) + piso mínimo:
//!   PIVO_FORTE exige ainda `peak_size >= piso_min_fila` (contratos de pico
//!   parados no run; default 0 = desligado). Poucos nós? Percentil continua
//!   definido (interpolação linear) e o piso impede promover poeira a pivô.
//! - Pivô = maior score entre elegíveis (fallback: todos, se nenhum elegível).
//!   Alvos (Alvo1/2/3, piso/teto, viés, ativo/dormente) = mesma estrutura da
//!   V1 ([`Alvos`], reutilizada para o desenho ser idêntico).

use serde::{Deserialize, Serialize};

use super::masterpice::{Alvos, Direcao, NodeClass, calcular_vies, day_key};

/// Tick do leilão V2: preço teórico + fila parada exatamente nele.
/// `size` = `bid_at + ask_at` do snapshot (contratos, `PremarketPayload.size`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AuctionTickV2 {
    pub ts: i64,
    pub price: f64,
    pub size: f64,
}

/// Parâmetros da V2 (genéricos por ativo; sem número mágico de score).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct V2Config {
    /// Confirmações mínimas (`1 + mudanças` reais de tamanho) para o nó
    /// poder passar de TRANSICAO. Default 2: 1 print sozinho nunca é
    /// Relevante+, por maior que seja o T.
    pub min_confirmacoes: usize,
    /// Pico mínimo de fila parada (contratos) para PIVO_FORTE. Default 0.0 =
    /// desligado; calibre por ativo (ex: 500 p/ WDO) contra dias ilíquidos.
    pub piso_min_fila: f64,
    /// `T_cap = mediana_gap_sessão × t_cap_mult`. Default 4.0; ≤0 desliga.
    pub t_cap_mult: f64,
}

impl Default for V2Config {
    fn default() -> Self {
        Self {
            min_confirmacoes: 2,
            piso_min_fila: 0.0,
            t_cap_mult: 4.0,
        }
    }
}

/// Nó de leilão V2.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NodeV2 {
    pub n: usize,
    pub price: f64,
    pub n_updates: u32,
    /// Σ size×dt do run (contratos×s, zero-order hold).
    pub fila_integral: f64,
    /// Variações reais de tamanho dentro do run (reenvio idêntico não conta).
    pub mudancas: usize,
    /// `1 + mudanças`.
    pub confirmacoes: usize,
    /// Maior fila vista no run (contratos; base do `piso_min_fila`).
    pub peak_size: f64,
    /// Dwell bruto (s): início do próximo run − início deste.
    pub duration_s: f64,
    /// Dwell após o cap (`min(duration_s, t_cap)`).
    pub duration_cap_s: f64,
    pub score: f64,
    pub elegivel: bool,
    pub class: NodeClass,
}

/// Saída da reconstrução V2: nós + metadados da sessão (auditoria/comparação).
#[derive(Debug, Clone, PartialEq)]
pub struct V2Build {
    pub nodes: Vec<NodeV2>,
    pub median_gap_s: f64,
    pub t_cap_s: f64,
    pub p90: f64,
    pub p60: f64,
    pub p30: f64,
}

/// Percentil por interpolação linear sobre valores ordenados (crescente).
fn percentil_ordenado(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

fn mediana_ordenada(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Reconstrói os nós V2 a partir dos ticks ordenados por tempo.
/// Entrada deve vir ordenada (`MasterpiceStoreV2` garante no bootstrap/live).
pub fn reconstruir_nos_v2(ticks: &[AuctionTickV2], cfg: &V2Config) -> V2Build {
    let mut gaps: Vec<f64> = ticks
        .windows(2)
        .map(|w| (w[1].ts - w[0].ts).max(0) as f64 / 1000.0)
        .collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_gap_s = mediana_ordenada(&gaps);
    let t_cap_s = if median_gap_s > 0.0 && cfg.t_cap_mult > 0.0 {
        median_gap_s * cfg.t_cap_mult
    } else {
        f64::INFINITY
    };

    // Passada 1: runs com fila integral (zero-order hold) e mudanças.
    struct Bruto {
        price: f64,
        n_updates: u32,
        fila: f64,
        mudancas: usize,
        peak: f64,
        duration_s: f64,
    }
    let mut brutos: Vec<Bruto> = Vec::new();
    let mut i = 0usize;
    while i < ticks.len() {
        let price = ticks[i].price;
        let start = ticks[i].ts;
        let mut j = i;
        while j < ticks.len() && ticks[j].price == price {
            j += 1;
        }
        let mut fila = 0.0;
        let mut mudancas = 0usize;
        let mut peak = 0.0f64;
        for k in i..j {
            let size = ticks[k].size.max(0.0);
            peak = peak.max(size);
            // Último print do run é segurado até o próximo run; no run final
            // da sessão, até o próprio último ts (dt 0 se print único).
            let dt_ms = if k + 1 < ticks.len() {
                (ticks[k + 1].ts - ticks[k].ts).max(0)
            } else {
                (ticks[k].ts - start).max(0).min(0)
            };
            fila += size * dt_ms as f64 / 1000.0;
            if k > i && (ticks[k].size - ticks[k - 1].size).abs() > 1e-9 {
                mudancas += 1;
            }
        }
        let duration_s = if j < ticks.len() {
            (ticks[j].ts - start).max(0) as f64 / 1000.0
        } else {
            (ticks[j - 1].ts - start).max(0) as f64 / 1000.0
        };
        brutos.push(Bruto {
            price,
            n_updates: (j - i) as u32,
            fila,
            mudancas,
            peak,
            duration_s,
        });
        i = j;
    }

    // Passada 2: score + percentis sobre elegíveis + classes.
    let scores_elegiveis: Vec<f64> = brutos
        .iter()
        .filter(|b| b.mudancas + 1 >= cfg.min_confirmacoes)
        .map(|b| b.fila * (1.0 + b.mudancas as f64) * (1.0 + b.duration_s.min(t_cap_s)))
        .collect();
    let mut ordenados = scores_elegiveis.clone();
    ordenados.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (p90, p60, p30) = (
        percentil_ordenado(&ordenados, 0.90),
        percentil_ordenado(&ordenados, 0.60),
        percentil_ordenado(&ordenados, 0.30),
    );

    let mut nodes = Vec::with_capacity(brutos.len());
    for (idx, b) in brutos.into_iter().enumerate() {
        let confirmacoes = b.mudancas + 1;
        let elegivel = confirmacoes >= cfg.min_confirmacoes;
        let duration_cap_s = b.duration_s.min(t_cap_s);
        let v = b.fila * (1.0 + b.mudancas as f64);
        let score = v * (1.0 + duration_cap_s);
        let mut class = if score >= p90 && p90 > 0.0 {
            NodeClass::PivoForte
        } else if score >= p60 && p60 > 0.0 {
            NodeClass::Relevante
        } else if score >= p30 && p30 > 0.0 {
            NodeClass::Transicao
        } else {
            NodeClass::Ruido
        };
        // Piso mínimo: sem fila de pico suficiente, nunca PIVO_FORTE.
        if class == NodeClass::PivoForte && b.peak < cfg.piso_min_fila {
            class = NodeClass::Relevante;
        }
        // Trava anti-silêncio: 1 confirmação nunca passa de TRANSICAO.
        if !elegivel && matches!(class, NodeClass::PivoForte | NodeClass::Relevante) {
            class = NodeClass::Transicao;
        }
        nodes.push(NodeV2 {
            n: idx + 1,
            price: b.price,
            n_updates: b.n_updates,
            fila_integral: b.fila,
            mudancas: b.mudancas,
            confirmacoes,
            peak_size: b.peak,
            duration_s: b.duration_s,
            duration_cap_s,
            score,
            elegivel,
            class,
        });
    }

    V2Build {
        nodes,
        median_gap_s,
        t_cap_s,
        p90,
        p60,
        p30,
    }
}

/// Gera os alvos V2. Pivô = maior score entre elegíveis (fallback: todos).
/// Núcleo, piso/teto, viés e ativo/dormente = mesma semântica da V1.
pub fn gerar_alvos_v2(nodes: &[NodeV2]) -> Option<Alvos> {
    if nodes.is_empty() {
        return None;
    }
    let elegiveis: Vec<&NodeV2> = nodes.iter().filter(|n| n.elegivel).collect();
    let universo: Vec<&NodeV2> = if elegiveis.is_empty() {
        nodes.iter().collect()
    } else {
        elegiveis
    };
    let pivo = universo
        .iter()
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())?;

    let nucleo: Vec<&&NodeV2> = universo
        .iter()
        .filter(|node| matches!(node.class, NodeClass::PivoForte | NodeClass::Relevante))
        .collect();
    let nucleo_ref: Vec<&NodeV2> = if nucleo.is_empty() {
        universo.iter().copied().collect()
    } else {
        nucleo.into_iter().copied().collect()
    };
    let ext_alta = nucleo_ref.iter().map(|n| n.price).fold(f64::MIN, f64::max);
    let ext_baixa = nucleo_ref.iter().map(|n| n.price).fold(f64::MAX, f64::min);
    let amplitude = ext_alta - ext_baixa;
    let abertura_estimada = nodes.iter().max_by_key(|n| n.n)?.price;
    let piso_duro = nodes.iter().map(|n| n.price).fold(f64::MAX, f64::min);
    let teto_duro = nodes.iter().map(|n| n.price).fold(f64::MIN, f64::max);
    let vies = calcular_vies(abertura_estimada, pivo.price, amplitude);
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
        alvo3_alta_ativo: vies.direcao == Direcao::Alta,
        alvo3_baixa_ativo: vies.direcao == Direcao::Baixa,
    })
}

/// Store incremental V2 (congela no close, como a V1).
#[derive(Debug, Clone, Default)]
pub struct MasterpiceStoreV2 {
    raw: Vec<AuctionTickV2>,
    build: Option<V2Build>,
    alvos: Option<Alvos>,
    session_day: Option<i64>,
    loaded_ts: Option<i64>,
    cfg: Option<V2Config>,
    version: u64,
}

impl MasterpiceStoreV2 {
    pub fn new(cfg: V2Config) -> Self {
        Self {
            cfg: Some(cfg),
            ..Self::default()
        }
    }

    pub fn nodes(&self) -> &[NodeV2] {
        self.build.as_ref().map_or(&[], |b| &b.nodes)
    }

    pub fn build(&self) -> Option<&V2Build> {
        self.build.as_ref()
    }

    pub fn alvos(&self) -> Option<&Alvos> {
        self.alvos.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.build.as_ref().map_or(true, |b| b.nodes.is_empty())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn session_day(&self) -> Option<i64> {
        self.session_day
    }

    pub fn set_config(&mut self, cfg: V2Config) {
        self.cfg = Some(cfg);
        self.rebuild();
        self.version += 1;
    }

    pub fn bootstrap<I>(&mut self, points: I)
    where
        I: IntoIterator<Item = AuctionTickV2>,
    {
        let mut raw: Vec<AuctionTickV2> = points.into_iter().collect();
        raw.sort_by_key(|t| t.ts);
        raw.dedup_by_key(|t| t.ts);
        let max_ts = raw.iter().map(|t| t.ts).max();
        let old_nodes = self.nodes().to_vec();
        let old_alvos = self.alvos;
        self.raw = raw;
        self.session_day = max_ts.map(day_key);
        self.loaded_ts = max_ts;
        self.rebuild();
        if self.nodes() != old_nodes.as_slice() || self.alvos != old_alvos {
            self.version += 1;
        }
    }

    pub fn add_point(&mut self, tick: AuctionTickV2) {
        let day = day_key(tick.ts);
        match self.session_day {
            None => self.session_day = Some(day),
            Some(cur) if cur != day => {
                if day < cur {
                    return;
                }
                self.raw.clear();
                self.build = None;
                self.alvos = None;
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
        match self.raw.binary_search_by_key(&tick.ts, |t| t.ts) {
            Ok(_) => return,
            Err(pos) => self.raw.insert(pos, tick),
        }
        self.rebuild();
        self.version += 1;
    }

    fn rebuild(&mut self) {
        let cfg = self.cfg.unwrap_or_default();
        let day = self.session_day;
        let ticks: Vec<AuctionTickV2> = match day {
            Some(d) => self
                .raw
                .iter()
                .copied()
                .filter(|t| day_key(t.ts) == d)
                .collect(),
            None => self.raw.clone(),
        };
        if ticks.is_empty() {
            self.build = None;
            self.alvos = None;
            return;
        }
        let build = reconstruir_nos_v2(&ticks, &cfg);
        self.alvos = gerar_alvos_v2(&build.nodes);
        self.build = Some(build);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sessão real de 24/09 (28 snapshots do `premarket_data.csv`).
    /// (ts, preço, fila parada = bid_at + ask_at do snapshot).
    fn sessao_24_09() -> Vec<AuctionTickV2> {
        let t = |ts: i64, price: f64, size: f64| AuctionTickV2 { ts, price, size };
        vec![
            t(1790250900098, 5163.0, 223.0),
            t(1790250920985, 5100.0, 776.0),
            t(1790250946129, 5111.0, 938.0),
            t(1790250952390, 5105.5, 983.0),
            t(1790250953198, 5111.0, 981.0),
            t(1790250960200, 5111.0, 988.0),
            t(1790250962543, 5149.0, 996.0),
            t(1790250990793, 5150.5, 976.0),
            t(1790250992409, 5151.0, 1036.0),
            t(1790251020871, 5151.0, 1119.0),
            t(1790251023093, 5219.5, 1964.0),
            t(1790251036020, 5219.0, 2001.0),
            t(1790251043902, 5200.0, 2004.0),
            t(1790251066730, 5199.0, 2189.0),
            t(1790251079700, 5199.0, 2210.0),
            t(1790251093015, 5163.0, 2364.0),
            t(1790251131117, 5169.0, 3.0),
            t(1790251132329, 5170.0, 2853.0),
            t(1790251140173, 5170.0, 2854.0),
            t(1790251140374, 5171.0, 2898.0),
            t(1790251153711, 5171.5, 2976.0),
            t(1790251156741, 5174.5, 3103.0),
            t(1790251157245, 5177.5, 3210.0),
            t(1790251175827, 5178.0, 3289.0),
            t(1790251182392, 5180.0, 4586.0),
            t(1790251197045, 5181.0, 4092.0),
            t(1790251197852, 5182.5, 4027.0),
            t(1790251199856, 5182.5, 3.0),
        ]
    }

    #[test]
    fn replay_24_09_pivo_vai_para_o_dinheiro() {
        let ticks = sessao_24_09();
        let build = reconstruir_nos_v2(&ticks, &V2Config::default());
        let alvos = gerar_alvos_v2(&build.nodes).unwrap();
        // V1 marcava 5151; V2 deve marcar a zona de maior fila (5199).
        assert_eq!(alvos.alvo1_pivo, 5199.0);
        assert_eq!(alvos.alvo2_extremo_alta, 5199.0);
        // 5100 (1 print, 25s de silêncio) não pode ser Relevante+.
        let n5100 = build.nodes.iter().find(|n| n.price == 5100.0).unwrap();
        assert!(!n5100.elegivel);
        assert_eq!(n5100.class, NodeClass::Transicao);
        // 5199 é PIVO_FORTE com 1 mudança real (2189→2210).
        let n5199 = build.nodes.iter().find(|n| n.price == 5199.0).unwrap();
        assert_eq!(n5199.class, NodeClass::PivoForte);
        assert_eq!(n5199.mudancas, 1);
        // Abertura abaixo do pivô → viés de baixa (preço subiu AO pivô).
        assert_eq!(alvos.vies.direcao, Direcao::Baixa);
        assert!(alvos.alvo3_baixa_ativo && !alvos.alvo3_alta_ativo);
    }

    #[test]
    fn heartbeat_identico_nao_conta_mudanca() {
        // 3 prints idênticos = 0 mudanças → 1 confirmação → capped.
        let ticks = vec![
            AuctionTickV2 {
                ts: 0,
                price: 100.0,
                size: 500.0,
            },
            AuctionTickV2 {
                ts: 10_000,
                price: 100.0,
                size: 500.0,
            },
            AuctionTickV2 {
                ts: 20_000,
                price: 100.0,
                size: 500.0,
            },
            AuctionTickV2 {
                ts: 21_000,
                price: 101.0,
                size: 10.0,
            },
        ];
        let build = reconstruir_nos_v2(&ticks, &V2Config::default());
        let n100 = build.nodes.iter().find(|n| n.price == 100.0).unwrap();
        assert_eq!(n100.mudancas, 0);
        assert_eq!(n100.confirmacoes, 1);
        assert!(!n100.elegivel);
        // Sem confirmação real nunca passa de TRANSICAO (Ruído também vale).
        assert!(matches!(
            n100.class,
            NodeClass::Transicao | NodeClass::Ruido
        ));
    }

    #[test]
    fn silencio_longo_com_1_print_nao_vira_relevante() {
        // Caso 5100: 1 print + 60s de silêncio.
        let ticks = vec![
            AuctionTickV2 {
                ts: 0,
                price: 100.0,
                size: 300.0,
            },
            AuctionTickV2 {
                ts: 60_000,
                price: 101.0,
                size: 300.0,
            },
            AuctionTickV2 {
                ts: 61_000,
                price: 101.0,
                size: 400.0,
            },
        ];
        let build = reconstruir_nos_v2(&ticks, &V2Config::default());
        let n100 = build.nodes.iter().find(|n| n.price == 100.0).unwrap();
        assert!((n100.duration_s - 60.0).abs() < 1e-9);
        assert_eq!(n100.class, NodeClass::Transicao);
    }

    #[test]
    fn t_cap_segura_feed_travado() {
        // Gaps normais de 1s, um salto de 600s no meio.
        let mut ticks = vec![];
        for k in 0..4 {
            ticks.push(AuctionTickV2 {
                ts: k * 1000,
                price: 100.0,
                size: 100.0 + k as f64,
            });
        }
        ticks.push(AuctionTickV2 {
            ts: 603_000,
            price: 101.0,
            size: 50.0,
        });
        let build = reconstruir_nos_v2(&ticks, &V2Config::default());
        assert!((build.median_gap_s - 1.0).abs() < 1e-9);
        assert!((build.t_cap_s - 4.0).abs() < 1e-9);
        let n100 = build.nodes.iter().find(|n| n.price == 100.0).unwrap();
        assert!(n100.duration_cap_s <= 4.0 + 1e-9);
        assert!(n100.duration_s > 600.0);
    }

    #[test]
    fn piso_minimo_barra_pivo_fraco() {
        let ticks = vec![
            AuctionTickV2 {
                ts: 0,
                price: 100.0,
                size: 10.0,
            },
            AuctionTickV2 {
                ts: 5_000,
                price: 100.0,
                size: 12.0,
            },
            AuctionTickV2 {
                ts: 10_000,
                price: 101.0,
                size: 5.0,
            },
        ];
        let cfg = V2Config {
            piso_min_fila: 500.0,
            ..V2Config::default()
        };
        let build = reconstruir_nos_v2(&ticks, &cfg);
        // Mesmo sendo top percentil, sem pico de fila não é PIVO_FORTE.
        assert!(build.nodes.iter().all(|n| n.class != NodeClass::PivoForte));
    }

    #[test]
    fn store_v2_bootstrap_e_rollover() {
        let mut st = MasterpiceStoreV2::new(V2Config::default());
        let ticks = sessao_24_09();
        st.bootstrap(ticks);
        assert!(!st.is_empty());
        assert_eq!(st.alvos().unwrap().alvo1_pivo, 5199.0);
        let v = st.version();
        st.add_point(AuctionTickV2 {
            ts: 1790250900098,
            price: 5163.0,
            size: 223.0,
        });
        assert_eq!(st.version(), v); // replay ignorado pelo watermark
    }
}
