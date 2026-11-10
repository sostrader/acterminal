# Manual Genérico — Indicador de Nós de Leilão (+ Volume Profile opcional)

Este manual descreve, de forma **independente de ativo, data ou preço**, a metodologia para reconstruir níveis de suporte/resistência a partir do leilão de abertura e (opcionalmente) cruzá-los com o Volume Profile da sessão real. Ele foi desenhado para ser reaplicado em qualquer papel (WDO, WIN, ações, cripto) e em qualquer dia, sem depender dos números observados numa sessão específica.

Existem **duas versões** do indicador, com capacidades diferentes:

| | Versão A — Só Leilão | Versão B — Leilão + Volume Profile |
|---|---|---|
| Fonte de dados | Apenas ticks do leilão pré-abertura | Ticks do leilão **+** candles OHLCV da sessão em andamento |
| Quando os alvos são calculados | Uma vez, antes da abertura | Uma vez antes da abertura, e **recalculados continuamente** durante o pregão |
| Atualiza durante o dia? | **Não** — os alvos ficam fixos o dia todo | **Sim** — o pivô pode ser "promovido" ou os alvos recalibrados conforme o volume real da sessão se acumula |
| Complexidade de implementação | Baixa | Média (precisa de feed de candles/trades ao vivo) |
| Quando usar | Quando você só tem acesso ao book do leilão, ou quer um indicador simples e leve | Quando você tem um feed de mercado ao vivo e quer o indicador se ajustando durante o pregão |

---

## 1. Conceito Geral

A premissa é a mesma nas duas versões: o **leilão de abertura** (call auction) recalcula continuamente um preço teórico de equilíbrio conforme ofertas de compra e venda entram no book. Olhando **como esse preço se moveu** — quais níveis ele "trabalhou" mais (mais tempo parado, mais atualizações) — conseguimos estimar onde está o "valor justo" do dia e projetar zonas de reação (suporte/resistência) e alvos de continuação (measured move), antes mesmo de o mercado abrir.

A Versão B soma a isso o **Volume Profile**: um histograma de volume real negociado por faixa de preço, construído a partir dos candles da sessão já em andamento. Quando um nível do leilão e um nível de alto volume real coincidem (ou ficam muito próximos), isso reforça a confiança nesse nível — e quando um nível fraco do leilão acaba recebendo muito volume real, ele deve ser promovido a pivô ativo, mesmo que o leilão originalmente não indicasse força ali.

---

## 2. Glossário de Termos (genérico)

### 2.1 Dados de entrada (leilão)

| Termo | Significado |
|---|---|
| **Tick de leilão** | Cada atualização do preço teórico de abertura (PTA) enviada durante o período de leilão, antes da abertura oficial. |
| **PTA (Preço Teórico de Abertura)** | Preço de equilíbrio recalculado continuamente pela bolsa/corretora conforme ofertas entram no leilão. O último PTA antes da abertura normalmente **se torna o preço de abertura oficial**. |
| **Run (execução/permanência)** | Sequência de ticks consecutivos em que o preço não mudou. Cada *run* é um "nó" candidato a suporte/resistência. |

### 2.2 Métricas por nó (N / V / T)

| Sigla | Como é calculado | O que representa |
|---|---|---|
| **N** | Índice sequencial do *run*, na ordem cronológica em que ocorreu | Apenas identifica a ordem temporal — **não mede força** |
| **V** | Número de atualizações de book enquanto o preço ficou parado naquele nível | Quanto "trabalho" o book fez ali — proxy de atividade/negociação |
| **T** | Tempo (segundos) entre o início daquele *run* e o início do próximo *run* com preço diferente | Quanto tempo o mercado "hesitou" ali antes de decidir para onde ir |

### 2.3 Score e Classificação

| Termo | Definição |
|---|---|
| **Score** | `score = V × (1 + T)`. Combina intensidade (V) e persistência (T) num único número comparável entre nós. |
| **Classes de nó** | Faixas de score usadas para ranquear a confiabilidade de cada nível. Os nomes sugeridos são: `PIVO_FORTE` (score muito alto), `RELEVANTE` (score médio), `TRANSICAO` (score baixo, mas não nulo) e `RUIDO` (score mínimo — ticks isolados e fugazes). |
| **Limiares de classificação** | **Não são universais.** Devem ser calibrados por ativo/volatilidade, idealmente usando estatísticas do próprio histórico de scores (ex: percentis p90/p60/p30), não números fixos copiados de outro papel ou de outro dia. |

### 2.4 Sequência de Alvos (válida nas duas versões)

| Termo | Definição | Papel |
|---|---|---|
| **Alvo 1 (Pivô)** | Preço do nó com maior *score* entre todos os nós do leilão | "Valor justo" do leilão — referência de retorno à média |
| **Alvo 2 (Extremo Alta / Extremo Baixa)** | Máximo e mínimo preço entre os nós classificados como `PIVO_FORTE` ou `RELEVANTE` | Delimitam o "range núcleo" — a faixa validada com convicção pelo leilão |
| **Amplitude Núcleo** | `Alvo2_alta − Alvo2_baixa` | Régua usada para projetar o Alvo 3 |
| **Alvo 3 (Projeção measured-move)** | `Alvo2_alta + Amplitude` (rompimento de alta) ou `Alvo2_baixa − Amplitude` (rompimento de baixa) | Alvo de continuação — só deve ser considerado ativo depois de o Alvo 2 correspondente ser **rompido com fechamento**, não apenas tocado por pavio |
| **Piso Duro / Teto Duro** | Mínimo e máximo absolutos de **todos** os ticks do leilão, **inclusive** os classificados como `RUIDO` | Limites de invalidação do viés — **nunca são alvos**. Distintos do Alvo 2, que usa só os nós fortes (`PIVO_FORTE`/`RELEVANTE`) |
| **Viés Direcional** | `(fechamento_leilão − pivô) / amplitude_núcleo`. Sinal `+` = o leilão fechou se afastando do valor justo para cima; `−` = para baixo; `0`/amplitude zero = neutro | Diz qual Alvo 3 é o **alvo de trabalho desde a abertura**, sem precisar adivinhar a direção |
| **Alvo Ativo vs. Alvo Dormente** | O Alvo 3 na direção do viés nasce ativo; o oposto nasce dormente | O dormente só "acorda" com **fechamento de candle além do duro correspondente** (abaixo do piso → acorda Alvo3 Baixa e invalida a alta; acima do teto → o inverso). Pavio não ativa |

### 2.5 Volume Profile (exclusivo da Versão B)

| Termo | Definição |
|---|---|
| **Volume Profile** | Histograma de volume negociado por faixa de preço (não por tempo) |
| **POC (Point of Control)** | Nível de preço com o maior volume acumulado até o momento — atualiza-se a cada novo candle |
| **Value Area (VA)** | Faixa de preços que concentra uma fração definida do volume total (convenção comum: 70%, mas é um parâmetro configurável) |
| **VAH / VAL** | Topo e base da Value Area |
| **Aceitação fora da área de valor** | Vários candles consecutivos fecham fora do range VAH–VAL — sinaliza formação de uma nova área de valor (viés de continuação) |
| **Rejeição da área de valor** | Preço toca fora do VAH/VAL mas retorna rapidamente para dentro — viés de reversão em direção ao POC |
| **Promoção de pivô** | Regra dinâmica: se o POC real ficar próximo (dentro de uma tolerância) de um Alvo 2 originalmente fraco no leilão, esse nível assume o papel de Alvo 1 ativo, substituindo o pivô estático do leilão |

> **Por que a Versão A não "atualiza"**: sem candles/volume real da sessão, não há como saber se um nível está recebendo mais ou menos negociação do que o leilão sugeriu. Os alvos calculados antes da abertura permanecem fixos até o fim do pregão. A Versão B resolve isso recebendo novos candles continuamente e recalculando POC/VA a cada atualização.

---

## 3. Fluxo do Algoritmo

### 3.1 Versão A — Somente Leilão (estática)

```text
ENTRADA: lista de ticks do leilão (datetime, price)

1. Ordenar ticks por tempo.
2. Agrupar em "runs" (sequências de mesmo preço).
3. Para cada run: calcular N, V, T, score, classe.
4. Alvo1 = preço do nó de maior score.
5. núcleo = nós com classe PIVO_FORTE ou RELEVANTE.
6. Alvo2_alta = max(preço em núcleo); Alvo2_baixa = min(preço em núcleo).
7. amplitude = Alvo2_alta - Alvo2_baixa.
8. Alvo3_alta = Alvo2_alta + amplitude; Alvo3_baixa = Alvo2_baixa - amplitude.
9. piso_duro = min(preço em TODOS os ticks, inclusive RUIDO);
   teto_duro = max(preço em TODOS os ticks, inclusive RUIDO).
10. vies = (fechamento_leilão − Alvo1) / amplitude  (+ = alta, − = baixa, 0 = neutro).
11. Hierarquia (não simetria): o Alvo3 na direção do viés nasce ATIVO;
    o oposto nasce DORMENTE e só acorda com fechamento de candle além do
    duro correspondente (abaixo do piso → acorda Alvo3 Baixa; acima do
    teto → acorda Alvo3 Alta). Pavio nunca ativa.

SAÍDA: Alvo1, Alvo2_alta, Alvo2_baixa, Alvo3_alta (+estado ativo/dormente),
        Alvo3_baixa (+estado ativo/dormente), piso_duro, teto_duro, vies
        (fixos até o fim do pregão — não há passo de atualização)
```

### 3.2 Versão B — Leilão + Volume Profile (dinâmica)

```text
ENTRADA 1: ticks do leilão -> gerar Alvo1..Alvo3 (idêntico à Versão A)
ENTRADA 2: stream de candles OHLCV da sessão em andamento

REPETIR a cada novo candle:
  1. Atualizar o histograma de volume por preço (distribuir o volume
     do candle entre os níveis de preço do seu range [low, high]).
  2. Recalcular POC = preço com maior volume acumulado.
  3. Recalcular Value Area (VAH/VAL) somando os preços de maior volume
     até atingir o percentual configurado (ex: 70%).
  4. Se |POC - Alvo2_alta| <= tolerância OU |POC - Alvo2_baixa| <= tolerância:
        promover Alvo1 = POC   (o nível fraco do leilão virou pivô real)
  5. (Opcional) Recalcular Alvo3 usando a amplitude real da Value Area
     em vez da amplitude do leilão, se a sessão já tiver volume
     suficiente para ser mais confiável que a pré-abertura.

SAÍDA: Alvo1 (pode mudar durante o dia), Alvo2, Alvo3, POC, VAH, VAL
        (todos atualizados a cada novo candle)
```

---

## 4. Implementação em Rust

> `Cargo.toml`:
> ```toml
> [dependencies]
> csv = "1.3"
> serde = { version = "1", features = ["derive"] }
> chrono = { version = "0.4", features = ["serde"] }
> ```

O código abaixo é dividido em um **núcleo compartilhado** (nós de leilão + alvos, usado pelas duas versões) e um **módulo opcional** de Volume Profile (só necessário na Versão B). Nenhum preço, ticker ou data real está fixado no código — tudo é parametrizado.

### 4.1 Núcleo compartilhado (Versão A e B)

```rust
// src/leilao.rs
//
// Núcleo do indicador: reconstrução de nós do leilão e geração de alvos.
// Não depende de nenhum dado de sessão real — funciona isoladamente (Versão A).

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Um tick bruto do leilão: preço teórico de abertura em um instante.
#[derive(Debug, Deserialize, Clone)]
pub struct AuctionTick {
    pub datetime: DateTime<Utc>,
    pub price: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeClass {
    PivoForte,
    Relevante,
    Transicao,
    Ruido,
}

/// Limiares de classificação — parâmetros, não constantes fixas.
/// Calibre estes valores com o histórico de scores do seu próprio ativo
/// (ex: percentis da distribuição de scores observada em N sessões).
#[derive(Debug, Clone, Copy)]
pub struct LimiaresClassificacao {
    pub pivo_forte_min: f64,
    pub relevante_min: f64,
    pub transicao_min: f64,
}

impl Default for LimiaresClassificacao {
    /// Valores de partida razoáveis — ajuste antes de usar em produção.
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
#[derive(Debug, Clone)]
pub struct AuctionNode {
    pub n: usize,
    pub price: f64,
    pub n_updates: u32,   // V
    pub duration_s: f64,  // T
    pub score: f64,
    pub class: NodeClass,
}

/// Reconstrói os nós (runs) a partir da lista de ticks do leilão.
pub fn reconstruir_nos(ticks: &[AuctionTick], limiares: &LimiaresClassificacao) -> Vec<AuctionNode> {
    let mut nodes = Vec::new();
    if ticks.is_empty() {
        return nodes;
    }

    let mut i = 0usize;
    let mut n = 0usize;

    while i < ticks.len() {
        let price = ticks[i].price;
        let start = ticks[i].datetime;

        let mut j = i;
        while j < ticks.len() && ticks[j].price == price {
            j += 1;
        }
        let n_updates = (j - i) as u32;

        let duration_s = if j < ticks.len() {
            (ticks[j].datetime - start).num_milliseconds() as f64 / 1000.0
        } else {
            (ticks[j - 1].datetime - start).num_milliseconds() as f64 / 1000.0
        };

        let score = n_updates as f64 * (1.0 + duration_s);
        let class = NodeClass::from_score(score, limiares);

        n += 1;
        nodes.push(AuctionNode { n, price, n_updates, duration_s, score, class });

        i = j;
    }

    nodes
}

/// Sequência de alvos derivada apenas do leilão.
#[derive(Debug, Clone)]
pub struct Alvos {
    pub abertura_estimada: f64,
    pub alvo1_pivo: f64,
    pub alvo2_extremo_alta: f64,
    pub alvo2_extremo_baixa: f64,
    pub amplitude_nucleo: f64,
    pub alvo3_projecao_alta: f64,
    pub alvo3_projecao_baixa: f64,
    /// Mínimo absoluto de TODOS os ticks (inclusive ruído): invalida a alta.
    pub piso_duro: f64,
    /// Máximo absoluto de TODOS os ticks (inclusive ruído): invalida a baixa.
    pub teto_duro: f64,
    /// (fechamento − pivô) / amplitude: + = alta, − = baixa, 0 = neutro.
    pub vies: Vies,
    /// Hierarquia: só o Alvo3 na direção do viés nasce ativo.
    pub alvo3_alta_ativo: bool,
    pub alvo3_baixa_ativo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direcao { Alta, Baixa, Neutra }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vies { pub direcao: Direcao, pub valor: f64 }

pub fn calcular_vies(fechamento: f64, pivo: f64, amplitude: f64) -> Vies {
    if amplitude <= f64::EPSILON {
        return Vies { direcao: Direcao::Neutra, valor: 0.0 };
    }
    let valor = (fechamento - pivo) / amplitude;
    let direcao = if valor > 0.0 { Direcao::Alta }
        else if valor < 0.0 { Direcao::Baixa }
        else { Direcao::Neutra };
    Vies { direcao, valor }
}

/// Reavalia os dormentes diante de um FECHAMENTO da sessão (pavio não ativa).
pub fn avaliar_ativacao_por_fechamento(alvos: &Alvos, fechamento_sessao: f64) -> (bool, bool) {
    let mut alta = alvos.alvo3_alta_ativo;
    let mut baixa = alvos.alvo3_baixa_ativo;
    if fechamento_sessao < alvos.piso_duro { alta = false; baixa = true; }
    else if fechamento_sessao > alvos.teto_duro { baixa = false; alta = true; }
    (alta, baixa)
}

pub fn gerar_alvos(nodes: &[AuctionNode]) -> Option<Alvos> {
    if nodes.is_empty() {
        return None;
    }

    let pivo = nodes.iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())?;

    let nucleo: Vec<&AuctionNode> = nodes
        .iter()
        .filter(|node| matches!(node.class, NodeClass::PivoForte | NodeClass::Relevante))
        .collect();
    let nucleo_ref: Vec<&AuctionNode> = if nucleo.is_empty() { nodes.iter().collect() } else { nucleo };

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
```

**Uso mínimo (Versão A, sem Volume Profile):**

```rust
// src/main_versao_a.rs
mod leilao;
use leilao::{reconstruir_nos, gerar_alvos, LimiaresClassificacao, AuctionTick};
use std::error::Error;

fn carregar_ticks(path: &str) -> Result<Vec<AuctionTick>, Box<dyn Error>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut ticks: Vec<AuctionTick> = rdr.deserialize().collect::<Result<_, _>>()?;
    ticks.sort_by_key(|t| t.datetime);
    Ok(ticks)
}

fn main() -> Result<(), Box<dyn Error>> {
    let ticks = carregar_ticks("premarket_data.csv")?;
    let limiares = LimiaresClassificacao::default();
    let nodes = reconstruir_nos(&ticks, &limiares);
    let alvos = gerar_alvos(&nodes).expect("sem nós suficientes");

    println!("=== NÓS DO LEILÃO ===");
    for node in &nodes {
        println!(
            "N:{:<3} preço:{:<10.2} V:{:<3} T:{:<8.3} score:{:<10.2} classe:{:?}",
            node.n, node.price, node.n_updates, node.duration_s, node.score, node.class
        );
    }
    println!("\n=== ALVOS (estáticos, válidos para o pregão inteiro) ===\n{:#?}", alvos);
    Ok(())
}
```

### 4.2 Módulo opcional de Volume Profile (só na Versão B)

```rust
// src/volume_profile.rs
//
// Extensão opcional: só é usada se você tiver um feed de candles da
// sessão em andamento. Sem este módulo, o indicador continua
// funcionando normalmente (Versão A), só não terá atualização dinâmica.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Clone)]
pub struct Candle {
    pub datetime: DateTime<Utc>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

pub struct VolumeProfile {
    pub poc: f64,
    pub vah: f64,
    pub val: f64,
}

/// Constrói o perfil de volume distribuindo o volume de cada candle
/// igualmente entre os ticks de preço contidos no seu range [low, high].
/// `tick_size` deve refletir o tick mínimo do ativo (ex.: 0.5, 0.01, 1.0).
pub fn construir_volume_profile(
    candles: &[Candle],
    tick_size: f64,
    value_area_pct: f64,
) -> VolumeProfile {
    let mut hist: BTreeMap<i64, f64> = BTreeMap::new();
    let key_of = |p: f64| -> i64 { (p / tick_size).round() as i64 };

    for c in candles {
        let low_key = key_of(c.low);
        let high_key = key_of(c.high);
        let n_ticks = (high_key - low_key + 1).max(1) as f64;
        let vol_each = c.volume / n_ticks;
        for k in low_key..=high_key {
            *hist.entry(k).or_insert(0.0) += vol_each;
        }
    }

    let (poc_key, _) = hist
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .expect("histograma vazio — verifique se `candles` não está vazio");
    let poc = *poc_key as f64 * tick_size;

    let total_vol: f64 = hist.values().sum();
    let mut ordenado: Vec<(&i64, &f64)> = hist.iter().collect();
    ordenado.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());

    let mut acumulado = 0.0;
    let mut va_keys: Vec<i64> = Vec::new();
    for (k, v) in ordenado {
        acumulado += v;
        va_keys.push(*k);
        if acumulado >= value_area_pct * total_vol {
            break;
        }
    }

    let vah = *va_keys.iter().max().unwrap() as f64 * tick_size;
    let val = *va_keys.iter().min().unwrap() as f64 * tick_size;

    VolumeProfile { poc, vah, val }
}

/// Promove dinamicamente o Alvo 1 caso o POC real coincida com um dos
/// extremos do "núcleo" do leilão (Alvo 2), dentro de uma tolerância.
/// Deve ser chamada a cada novo candle, durante o pregão.
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
```

**Uso completo (Versão B, com atualização dinâmica):**

```rust
// src/main_versao_b.rs
mod leilao;
mod volume_profile;

use leilao::{reconstruir_nos, gerar_alvos, LimiaresClassificacao, AuctionTick};
use volume_profile::{construir_volume_profile, promover_pivo_se_necessario, Candle};
use std::error::Error;

fn carregar_ticks(path: &str) -> Result<Vec<AuctionTick>, Box<dyn Error>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut ticks: Vec<AuctionTick> = rdr.deserialize().collect::<Result<_, _>>()?;
    ticks.sort_by_key(|t| t.datetime);
    Ok(ticks)
}

fn carregar_candles(path: &str) -> Result<Vec<Candle>, Box<dyn Error>> {
    let mut rdr = csv::Reader::from_path(path)?;
    let mut candles: Vec<Candle> = rdr.deserialize().collect::<Result<_, _>>()?;
    candles.sort_by_key(|c| c.datetime);
    Ok(candles)
}

fn main() -> Result<(), Box<dyn Error>> {
    // Etapa 1 — igual à Versão A: gera os alvos estáticos do leilão.
    let ticks = carregar_ticks("premarket_data.csv")?;
    let limiares = LimiaresClassificacao::default();
    let nodes = reconstruir_nos(&ticks, &limiares);
    let mut alvos = gerar_alvos(&nodes).expect("sem nós suficientes");
    println!("=== ALVOS INICIAIS (do leilão) ===\n{:#?}", alvos);

    // Etapa 2 — específica da Versão B: recalcula com dados da sessão real.
    // Num robô ao vivo, isto rodaria dentro de um loop que recebe candles
    // um a um; aqui simulamos lendo um arquivo já fechado, como exemplo.
    let candles = carregar_candles("ohlc_sessao.csv")?;
    let tick_size = 0.5; // ajuste para o tick do seu ativo
    let value_area_pct = 0.70; // convenção de mercado, mas é configurável

    let vp = construir_volume_profile(&candles, tick_size, value_area_pct);
    println!(
        "\n=== VOLUME PROFILE (sessão em andamento) ===\nPOC: {:.2}  VAH: {:.2}  VAL: {:.2}",
        vp.poc, vp.vah, vp.val
    );

    let tolerancia = 2.0 * tick_size; // parâmetro — ajuste por ativo
    alvos.alvo1_pivo = promover_pivo_se_necessario(
        alvos.alvo1_pivo,
        alvos.alvo2_extremo_alta,
        alvos.alvo2_extremo_baixa,
        &vp,
        tolerancia,
    );

    println!("\n=== ALVOS ATUALIZADOS (após checar o Volume Profile) ===\n{:#?}", alvos);
    Ok(())
}
```

---

## 5. Como ler cada saída (genérico)

| Campo | Significado |
|---|---|
| `N` | Posição do nível na sequência cronológica do leilão — não indica força. |
| `V` (n_updates) | Quantas vezes o book atualizou naquele preço antes de mudar — mais alto = mais "disputado". |
| `T` (duration_s) | Quanto tempo (segundos) aquele preço ficou como referência — mais alto = mais "aceito". |
| `score` | Combinação V × (1+T); só serve para comparar nós entre si, sem unidade física. |
| `classe` | Rótulo de confiabilidade do nó, derivado do score e dos limiares calibrados. |
| `alvo1_pivo` | Nível de retorno à média mais provável — na Versão B, pode mudar durante o dia. |
| `alvo2_extremo_alta/baixa` | Bordas do range validado pelo leilão — usadas como gatilho para o Alvo 3. |
| `alvo3_projecao_*` | Alvo de continuação — só o da direção do viés nasce ativo; o dormente só acorda com fechamento além do duro correspondente (nunca por pavio). |
| `piso_duro` / `teto_duro` | Mín/máx de todos os ticks do leilão (inclusive ruído) — limites de invalidação, nunca alvos. |
| `vies` | `(fechamento − pivô) / amplitude` — a direção prioritária do dia, lida no leilão. |
| `poc` | (Só Versão B) Preço mais negociado da sessão até o momento — dinâmico. |
| `vah` / `val` | (Só Versão B) Bordas da área de consenso do dia até o momento — dinâmico. |

---

## 6. Diferenças Práticas Entre as Versões (resumo)

- **Versão A** é determinística e "congela" no momento em que o leilão termina. Ideal para quem só tem acesso ao book de leilão, ou quer algo simples para rodar uma vez por dia sem manter estado durante o pregão.
- **Versão B** exige manter um histograma de volume vivo, atualizado a cada candle/trade, e por isso é capaz de **corrigir** os alvos do leilão à luz do que realmente aconteceu na negociação — inclusive promovendo um nível que o leilão via como fraco a pivô real, se o volume da sessão discordar do leilão.
- Você pode implementar só a Versão A primeiro (menos partes móveis, mais fácil de testar) e adicionar o módulo de Volume Profile depois, sem reescrever o núcleo — foi por isso que o código foi dividido em `leilao.rs` (núcleo, reutilizável nas duas versões) e `volume_profile.rs` (extensão opcional).

---

## 7. Roteiro para Virar um Robô de Verdade
1. **Streaming em tempo real**: troque a leitura de CSV por um feed ao vivo (WebSocket/FIX da corretora), alimentando `reconstruir_nos` e (se usar a Versão B) `construir_volume_profile` de forma incremental — não recalcule tudo do zero a cada tick, mantenha estado.
2. **Calibração dos limiares por ativo**: colete o histórico de scores de várias sessões e defina `LimiaresClassificacao` por percentis, não por números fixos.
3. **Confirmação por fechamento, não por pavio**: antes de tratar um Alvo 2 como "rompido" (o que ativa o Alvo 3), exija 1–2 candles fechando além do nível.
4. **Gestão de risco obrigatória**: stop-loss como fração da amplitude núcleo, e limite de perda diária — o indicador aponta probabilidades, não certezas.
5. **Registro contínuo para backtest**: salve, para cada pregão, os nós do leilão, os alvos gerados (Versão A) e a evolução do POC/VA (Versão B) junto com o que de fato ocorreu — isso constrói seu dataset de validação para recalibrar os parâmetros ao longo do tempo, ativo por ativo.
6. **Testes unitários com dados sintéticos**: gere séries de ticks artificiais (sem relação com nenhum pregão real) para testar `reconstruir_nos`, `gerar_alvos` e `construir_volume_profile` isoladamente antes de conectar a um feed real ou a dinheiro real.

---

## 8. Versão V2 (experimental) — V = fila parada, não contagem de prints

A V1 conta snapshots (`V = nº de prints`), o que premia retransmissão do feed (heartbeat) e credita silêncio como dwell, ignorando o dinheiro parado no book. A V2 troca o volume por fila parada real. A V1 continua existindo sem alterações — ambas convivem para comparação. Nomes de alvos, viés, piso/teto, ativo/dormente e desenho são idênticos; só o score e as classes mudam.

### 8.1 Fórmula única (sem ambiguidade máximo × soma × integral)

Para cada snapshot `k`, seja `size_k` a fila parada exatamente no preço (`bid_at + ask_at`, contratos) e `t_k` seu timestamp. Para um run `R` (prints `i..j-1` no mesmo preço):

```text
fila_parada(R)  = Σ size_k × (t_{k+1} − t_k), em contratos×segundo
                  (zero-order hold: o último print do run é segurado até o
                  início do próximo run; no run final da sessão, até o
                  próprio último ts — 0 se print único)
mudanças(R)     = nº de prints do run (após o primeiro) com
                  |size_k − size_{k−1}| > ε  (reenvio idêntico NÃO conta)
confirmações(R) = 1 + mudanças(R)
V(R)            = fila_parada(R) × (1 + mudanças(R))
T_cap           = mediana(gaps entre prints da sessão) × t_cap_mult (default 4)
score(R)        = V(R) × (1 + min(T(R), T_cap))
```

Pesar pelo tempo (integral) em vez do máximo impede que um pico de fila de 1 tick (possível iceberg/spoofing) domine sozinho; contar só mudanças reais impede que fila estática re-snapshotada infle por frequência do feed.

### 8.2 Classes: percentis + piso mínimo + trava anti-silêncio

```text
elegível(R) = confirmações(R) >= min_confirmacoes  (default 2)
classes por percentis dos scores ELEGÍVEIS (interpolação linear):
  score >= p90 → PIVO_FORTE  (exige ainda peak_size >= piso_min_fila,
                              senão cai para RELEVANTE)
  score >= p60 → RELEVANTE
  score >= p30 → TRANSICAO
  abaixo      → RUIDO
trava: nó com 1 confirmação nunca passa de TRANSICAO, qualquer que seja o T.
pivô = maior score entre elegíveis (fallback: todos, se nenhum elegível).
```

- Percentis dispensam número mágico de score e funcionam entre ativos; o `piso_min_fila` (contratos de pico, default 0 = desligado, ex: 500 p/ WDO) impede promover poeira a pivô em dias ilíquidos ou com poucos nós.
- O cap de `T` (`mediana × k`, por sessão) é defesa em profundidade contra feed travado por minutos — não substitui a trava de confirmações.

### 8.3 Parâmetros (`V2Config`)

| Parâmetro | Default | Papel |
|---|---|---|
| `min_confirmacoes` | 2 | `1 + mudanças` mínimas p/ Relevante+ (subir p/ 3 = exigir ≥2 mudanças literais; em leilões curtos pode zerar os elegíveis — use com o fallback ciente) |
| `piso_min_fila` | 0.0 (desligado) | Pico mínimo de contratos parados p/ PIVO_FORTE; calibre por ativo |
| `t_cap_mult` | 4.0 | Multiplicador da mediana dos gaps p/ o cap de T; ≤0 desliga |

### 8.4 Trava de área (só V2): nada fora de [LOW, HIGH]

Na V2, Alvo3 (TG4/TG5) fora do intervalo `[piso_duro, teto_duro]` **não se desenha** — projeção ou standby fora da área do leilão não faz sentido. Vale para ativo e dormente.

---

## 9. Regra de confluência no desenho (V1 e V2)

Quando o pivô coincide com um extremo do núcleo (`|TG1 − TG2| ≤ 1 tick` ou `|TG1 − TG3| ≤ 1 tick`), imprimir os dois rótulos com sinais opostos no mesmo preço é instrução conflitante. A regra (só desenho — os dados não mudam):

```text
SE |TG1 − TG2| <= tick: rótulo único "TG1/TG2 PIVO-<papel>"
SE |TG1 − TG3| <= tick: rótulo único "TG1/TG3 PIVO-<papel>"
<papel> pelo último preço: abaixo do nível = RESIST (vermelho, venda na
rejeição); acima/após romper com fechamento = SUP (azul, compra na defesa).
Sem último preço: RESIST em cima, SUP embaixo.
```

A decisão direcional continua vindo do contexto (de onde o preço veio, se houve rompimento com fechamento — nunca pavio), na mesma linha do viés direcional e do piso/teto duro.
