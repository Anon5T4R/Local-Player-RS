//! HDR → SDR: EOTF + tone mapping, com espelho CPU da conta do shader.
//!
//! O shader (`yuv.wgsl`) faz a conta por pixel; este módulo existe para (a)
//! decidir **se** e **com que pico** tonemapar a partir do [`ColorInfo`] do
//! container, (b) escolher o **operador** e (c) ter a mesma matemática em CPU,
//! testável com os valores da spec — o WGSL só é validado pelo `naga`, não
//! executa em teste.
//!
//! ## Operadores
//!
//! Dois estão disponíveis; o default é o **BT.2390**.
//!
//! ### BT.2390 (EETF, ITU-R BT.2390 — default)
//!
//! O EETF é aplicado no domínio PQ sobre **max(R,G,B)** e vira um **ganho
//! único** para os três canais (`k = E2(max)/max`):
//! - **Por quê é o default:** é o operador da própria recomendação (o mesmo que
//!   mpv/libplacebo/VLC usam por padrão), preserva matiz por construção (o
//!   ganho é o mesmo nos três canais) e a curva tem joelho: abaixo de `KS`
//!   (≈ 1,5·maxLum − 0,5 no domínio PQ) o sinal passa **intacto** — tons médios
//!   e o branco de referência não escurecem, só os realces comprimem. Com o
//!   pipeline de 10 bits (textura `R16Unorm`) a compressão suave não vira
//!   banding como virava em 8 bits.
//! - **Trade-off:** o ganho por max(R,G,B) dessatura realces muito saturados
//!   (comportamento esperado de EETF; é o preço de não desviar matiz).
//! - A faixa 0–1 do uniform é o branco de referência SDR (203 nits, BT.2408):
//!   o pico do conteúdo mapeia para 1,0 e o resto fica abaixo.
//!
//! ### Hable (filmic, Uncharted 2) — opcional
//!
//! `y = hable(x) / hable(white)`, canal a canal, com `x` em luz de display
//! (1.0 = 203 nits) e `white` = pico do conteúdo na mesma unidade.
//! - Barato (um racional + uma divisão por canal), monotônico, `0 → 0`, pico →
//!   1 e ombro suave nos realces. Comparado a Reinhard com white point,
//!   preserva mais contraste nos tons médios — o sintoma que o PQ/HLG tratado
//!   como SDR produzia (imagem "lavada").
//! - **Custo:** o branco de referência (203 nits) cai em ~0,39 de luz linear
//!   (~0,66 em sRGB) para deixar ombro até o pico, **escurecendo a imagem
//!   inteira**; e como o operador é aplicado por canal, matiz desvia em cores
//!   muito saturadas. Ficou como opção de comparação via
//!   `LOCALPLAYER_HDR_TONEMAP=hable` (overlay D mostra o operador ativo).
//!
//! A conversão de gamut BT.2020 → BT.709 segue fora (o shader usa a matriz do
//! container sem converter primárias).
//!
//! ## PQ (SMPTE ST 2084)
//!
//! `pq_eotf_nits` devolve nits absolutos (0–10 000). O pico default é
//! [`DEFAULT_PEAK_NITS`] (1000 nits), sobrescrevível por
//! `LOCALPLAYER_HDR_PEAK` e, quando o arquivo sinaliza `MaxCLL`/mastering
//! display, pelo pico do próprio conteúdo.
//!
//! ## HLG (ARIB STD-B67)
//!
//! `hlg_inverse_oetf` devolve luz de cena 0–1; o OOTF (gamma de sistema 1,2
//! para display de 1000 nits) leva a display linear. HLG é **relativo**: o
//! pico é sempre [`HLG_PEAK_NITS`], não o do env.

use std::sync::OnceLock;

use filmcraft_color::{ColorInfo, Transfer};

/// Branco de referência do SDR (BT.2408), em cd/m².
#[cfg(test)]
pub const REFERENCE_WHITE_NITS: f32 = 203.0;
/// Pico default do conteúdo PQ quando nada melhor é conhecido.
pub const DEFAULT_PEAK_NITS: f32 = 1000.0;
/// Pico relativo do HLG (não vem do env).
pub const HLG_PEAK_NITS: f32 = 1000.0;
/// Faixa aceita para o pico configurado (abaixo disso o tonemap só escurece).
pub const MIN_PEAK_NITS: f32 = 100.0;
/// Teto absoluto do PQ.
pub const MAX_PEAK_NITS: f32 = 10_000.0;
/// Variável de ambiente que sobrescreve o pico default do PQ.
pub const PEAK_ENV: &str = "LOCALPLAYER_HDR_PEAK";
/// Variável de ambiente que escolhe o operador (`bt2390` default, `hable`).
pub const OPERATOR_ENV: &str = "LOCALPLAYER_HDR_TONEMAP";

/// Qual EOTF/tonemap o shader deve aplicar neste frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneMapMode {
    /// SDR: caminho antigo, sem tonemap.
    Off,
    /// SMPTE ST 2084.
    Pq,
    /// ARIB STD-B67.
    Hlg,
}

impl ToneMapMode {
    /// Modo a partir da transferência declarada pelo container.
    pub fn from_transfer(transfer: Transfer) -> Self {
        match transfer {
            Transfer::Pq => Self::Pq,
            Transfer::Hlg => Self::Hlg,
            Transfer::Bt709 | Transfer::Srgb | Transfer::Linear => Self::Off,
        }
    }

    /// Código que vai no uniform (`tone.x`) e que o WGSL compara.
    pub fn code(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Pq => 1.0,
            Self::Hlg => 2.0,
        }
    }

    /// Rótulo curto para o overlay (inglês técnico, como o resto da tecla D).
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "sdr",
            Self::Pq => "pq",
            Self::Hlg => "hlg",
        }
    }

    pub fn is_active(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// Operador de tonemapping aplicado por cima da EOTF.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operator {
    /// EETF da ITU-R BT.2390 (default): razão única por max(R,G,B), joelho.
    #[default]
    Bt2390,
    /// Curva filmic de Hable (Uncharted 2), canal a canal.
    Hable,
}

impl Operator {
    /// Código que vai no uniform (`tone.z`) e que o WGSL compara.
    pub fn code(self) -> f32 {
        match self {
            Self::Bt2390 => 0.0,
            Self::Hable => 1.0,
        }
    }

    /// Rótulo curto para o overlay (inglês técnico, como o resto da tecla D).
    pub fn label(self) -> &'static str {
        match self {
            Self::Bt2390 => "bt2390",
            Self::Hable => "hable",
        }
    }

    /// Nome aceito no env (case-insensitive); `None` para texto desconhecido.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bt2390" | "2390" => Some(Self::Bt2390),
            "hable" => Some(Self::Hable),
            _ => None,
        }
    }
}

/// Operador configurado por `LOCALPLAYER_HDR_TONEMAP` (lido uma vez por
/// processo; sem a env, BT.2390).
pub fn env_operator() -> Operator {
    static OP: OnceLock<Operator> = OnceLock::new();
    *OP.get_or_init(|| std::env::var(OPERATOR_ENV).ok().and_then(|raw| Operator::parse(&raw)).unwrap_or_default())
}

/// Parâmetros resolvidos para um frame: o que o uniform e o overlay precisam.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneMapParams {
    pub mode: ToneMapMode,
    /// Pico do conteúdo em nits (0 quando `Off`).
    pub peak_nits: f32,
    pub operator: Operator,
}

impl ToneMapParams {
    /// Pico do PQ: o valor já resolvido pelo chamador (sinalizado pelo arquivo >
    /// env > default), sempre na faixa aceita. HLG ignora o valor e usa
    /// [`HLG_PEAK_NITS`] (relativo).
    pub fn for_color(color: &ColorInfo, peak_nits: f32, operator: Operator) -> Self {
        let mode = ToneMapMode::from_transfer(color.transfer);
        let peak = match mode {
            ToneMapMode::Off => 0.0,
            ToneMapMode::Hlg => HLG_PEAK_NITS,
            ToneMapMode::Pq => {
                let p = if peak_nits.is_finite() && peak_nits > 0.0 { peak_nits } else { DEFAULT_PEAK_NITS };
                p.clamp(MIN_PEAK_NITS, MAX_PEAK_NITS)
            }
        };
        Self { mode, peak_nits: peak, operator }
    }

    /// Linha `tone` do uniform: `(mode, peak_nits, operator, 0)`.
    pub fn uniform_row(self) -> [f32; 4] {
        [self.mode.code(), self.peak_nits, self.operator.code(), 0.0]
    }
}

/// Pico configurado por `LOCALPLAYER_HDR_PEAK` (lido uma vez por processo).
pub fn env_peak_nits() -> f32 {
    static PEAK: OnceLock<f32> = OnceLock::new();
    *PEAK.get_or_init(|| std::env::var(PEAK_ENV).ok().and_then(|raw| parse_peak_nits(&raw)).unwrap_or(DEFAULT_PEAK_NITS))
}

/// Aceita só número finito e positivo; satura na faixa aceita. Texto inválido
/// vira `None` (o chamador cai no default).
pub fn parse_peak_nits(raw: &str) -> Option<f32> {
    raw.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0).map(|v| v.clamp(MIN_PEAK_NITS, MAX_PEAK_NITS))
}

/// SMPTE ST 2084 EOTF: código 0–1 → nits absolutos (0–10 000).
#[cfg(test)]
pub fn pq_eotf_nits(e: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.687_5;
    let p = e.max(0.0).powf(1.0 / M2);
    ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1) * MAX_PEAK_NITS
}

/// SMPTE ST 2084 inverse EOTF: nits → código 0–1 (espelho do shader).
#[cfg(test)]
pub fn pq_inverse_eotf(nits: f32) -> f32 {
    const M1: f32 = 0.159_301_76;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_563;
    const C3: f32 = 18.687_5;
    let y = (nits.max(0.0) / MAX_PEAK_NITS).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

/// ARIB STD-B67 inverse OETF: código 0–1 → luz de cena 0–~1 (sem OOTF).
#[cfg(test)]
pub fn hlg_inverse_oetf(e: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_7;
    let e = e.max(0.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    }
}

/// OOTF do HLG (gamma de sistema 1,2) aplicado à luz de cena.
#[cfg(test)]
pub fn hlg_ootf(scene: f32) -> f32 {
    scene.max(0.0).powf(1.2)
}

/// Um canal do sinal → nits absolutos de display (PQ/HLG).
#[cfg(test)]
pub fn display_nits(v: f32, mode: ToneMapMode) -> f32 {
    match mode {
        ToneMapMode::Off => v * REFERENCE_WHITE_NITS,
        ToneMapMode::Pq => pq_eotf_nits(v),
        ToneMapMode::Hlg => hlg_ootf(hlg_inverse_oetf(v)) * HLG_PEAK_NITS,
    }
}

/// Um canal do sinal → luz de display (1.0 = branco de referência SDR).
#[cfg(test)]
pub fn decode_to_display(v: f32, mode: ToneMapMode) -> f32 {
    display_nits(v, mode) / REFERENCE_WHITE_NITS
}

/// ITU-R BT.2390 EETF: `nits` (pico de origem `src_peak`) → pico de destino
/// `dst_peak`, no domínio PQ e com joelho. Identidade abaixo do joelho e quando
/// o destino é maior que a origem. Espelho exato do `bt2390_eetf` do WGSL.
#[cfg(test)]
pub fn bt2390_eetf(nits: f32, src_peak: f32, dst_peak: f32) -> f32 {
    if dst_peak >= src_peak || nits <= 0.0 {
        return nits.max(0.0).min(src_peak.max(dst_peak));
    }
    let smax = pq_inverse_eotf(src_peak);
    let e1 = (pq_inverse_eotf(nits) / smax).min(1.0);
    let max_lum = pq_inverse_eotf(dst_peak) / smax;
    let ks = 1.5 * max_lum - 0.5;
    let e2 = if e1 < ks {
        e1
    } else {
        let t = (e1 - ks) / (1.0 - ks);
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * ks + (t3 - 2.0 * t2 + t) * (1.0 - ks) + (-2.0 * t3 + 3.0 * t2) * max_lum
    };
    pq_eotf_nits(e2 * smax)
}

/// Curva filmic de Hable (Uncharted 2), sem normalização do white point.
#[cfg(test)]
pub fn hable(x: f32) -> f32 {
    const A: f32 = 0.15;
    const B: f32 = 0.50;
    const C: f32 = 0.10;
    const D: f32 = 0.20;
    const E: f32 = 0.02;
    const F: f32 = 0.30;
    (x * (A * x + C * B) + D * E) / (x * (A * x + B) + D * F) - E / F
}

/// Tonemap de um RGB já em código de transferência (R'G'B') para luz de
/// display linear `[0,1]`. Espelho exato do `tonemap` do WGSL.
#[cfg(test)]
pub fn tonemap_linear(rgb: [f32; 3], mode: ToneMapMode, peak_nits: f32, operator: Operator) -> [f32; 3] {
    if !mode.is_active() {
        return rgb;
    }
    match operator {
        Operator::Hable => {
            let white = (if peak_nits.is_finite() { peak_nits } else { DEFAULT_PEAK_NITS }).max(1.0) / REFERENCE_WHITE_NITS;
            let scale = 1.0 / hable(white).max(1e-6);
            rgb.map(|v| (hable(decode_to_display(v, mode)) * scale).clamp(0.0, 1.0))
        }
        Operator::Bt2390 => {
            let nits = rgb.map(|v| display_nits(v, mode));
            let m = nits[0].max(nits[1]).max(nits[2]);
            let out = bt2390_eetf(m, peak_nits.max(1.0), REFERENCE_WHITE_NITS);
            // Ganho único nos três canais: matiz preservado (o preto não divide).
            let k = if m > 1e-6 { out / m } else { 0.0 };
            nits.map(|n| (n * k / REFERENCE_WHITE_NITS).clamp(0.0, 1.0))
        }
    }
}

/// Encode sRGB (luz linear → codificado), para alvos que não são `*Srgb`.
#[cfg(test)]
pub fn linear_to_srgb(v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_color::Matrix;

    /// Códigos PQ da spec (SMPTE ST 2084): a EOTF tem que devolver os nits
    /// conhecidos nesses pontos, não só "algo monotônico".
    #[test]
    fn pq_conhecidos_da_spec() {
        let cases: [(f32, f32); 5] = [(0.0, 0.0), (0.508_078_4, 100.0), (0.580_688_9, 203.0), (0.751_827_1, 1000.0), (1.0, 10_000.0)];
        for (code, nits) in cases {
            let got = pq_eotf_nits(code);
            let tol = (nits * 0.005).max(0.05);
            assert!((got - nits).abs() <= tol, "PQ {code} -> {got} nits (esperado {nits})");
        }
        assert_eq!(pq_eotf_nits(0.0), 0.0);
    }

    /// A EOTF local concorda com a do `filmcraft_color` (implementação
    /// independente, já usada pelo resto do hub).
    #[test]
    fn pq_concorda_com_filmcraft_color() {
        for i in 0..=100 {
            let code = i as f32 / 100.0;
            let ours = pq_eotf_nits(code);
            let theirs = filmcraft_color::pq_eotf(code) * 10_000.0;
            assert!((ours - theirs).abs() <= 0.5, "code {code}: {ours} vs {theirs}");
        }
    }

    /// A inverse EOTF fecha o round-trip com a EOTF nos pontos da spec.
    #[test]
    fn pq_inverse_roundtrip() {
        for nits in [0.0, 1.0, 100.0, 203.0, 1000.0, 4000.0, 10_000.0] {
            let back = pq_eotf_nits(pq_inverse_eotf(nits));
            assert!((back - nits).abs() <= (nits * 1e-3).max(0.01), "{nits} -> {back}");
        }
        assert!((pq_inverse_eotf(10_000.0) - 1.0).abs() < 1e-4);
    }

    /// Inverse OETF do HLG: 0,5 é o joelho conhecido (1/12 de luz de cena) e
    /// 1,0 chega a ~1,0; a OETF desfaz a conta.
    #[test]
    fn hlg_inverse_oetf_joelho_e_roundtrip() {
        assert!((hlg_inverse_oetf(0.0) - 0.0).abs() < 1e-7);
        assert!((hlg_inverse_oetf(0.5) - 1.0 / 12.0).abs() < 1e-6);
        assert!((hlg_inverse_oetf(1.0) - 1.0).abs() < 1e-3);
        for i in 0..=100 {
            let code = i as f32 / 100.0;
            let back = filmcraft_color::hlg_oetf(hlg_inverse_oetf(code));
            assert!((back - code).abs() < 1e-5, "code {code} -> {back}");
        }
        // concorda com a implementação do hub
        for i in 0..=100 {
            let code = i as f32 / 100.0;
            assert!((hlg_inverse_oetf(code) - filmcraft_color::hlg_inverse_oetf(code)).abs() < 1e-6);
        }
    }

    /// Hable: 0 → 0 e o pico (white) → 1; nada de NaN.
    #[test]
    fn hable_limites() {
        assert!(hable(0.0).abs() < 1e-7);
        let white = DEFAULT_PEAK_NITS / REFERENCE_WHITE_NITS;
        assert!((hable(white) / hable(white) - 1.0).abs() < 1e-6);
        for x in [1e-4, 0.5, 1.0, 10.0, 1000.0] {
            assert!(hable(x).is_finite(), "hable({x})");
        }
    }

    /// Tonemap PQ 1000 nits (Hable): preto fica preto, o pico do conteúdo vira
    /// branco e a curva é monotônica e finita em toda a faixa (inclusive acima
    /// do pico, que satura em 1).
    #[test]
    fn tonemap_pq_hable_monotonico_e_limites() {
        let (mode, peak, op) = (ToneMapMode::Pq, 1000.0, Operator::Hable);
        for c in 0..3 {
            assert!(tonemap_linear([0.0; 3], mode, peak, op)[c].abs() < 1e-7);
        }
        let at_peak = tonemap_linear([0.751_827_1; 3], mode, peak, op);
        for v in at_peak {
            assert!((v - 1.0).abs() < 1e-3, "pico PQ -> {v}");
        }
        let mut last = -1.0;
        for i in 0..=1000 {
            let code = i as f32 / 1000.0;
            let out = tonemap_linear([code; 3], mode, peak, op);
            for v in out {
                assert!(v.is_finite(), "code {code}");
                assert!((0.0..=1.0).contains(&v), "code {code} -> {v}");
                assert!(v >= last - 1e-6, "monotonicidade em {code}: {last} -> {v}");
            }
            last = out[0];
        }
        // acima do pico satura em 1 em vez de estourar
        let over = tonemap_linear([1.0; 3], mode, peak, op);
        assert!(over.iter().all(|v| (*v - 1.0).abs() < 1e-6));
    }

    /// BT.2390: identidade no joelho, monotônico, pico do conteúdo e acima dele
    /// viram branco SDR, nada de NaN e o preto continua preto.
    #[test]
    fn tonemap_pq_bt2390_limites_e_monotonicidade() {
        let (mode, peak, op) = (ToneMapMode::Pq, 1000.0, Operator::Bt2390);
        let black = tonemap_linear([0.0; 3], mode, peak, op);
        assert!(black.iter().all(|v| v.abs() < 1e-7), "{black:?}");
        let at_peak = tonemap_linear([pq_inverse_eotf(peak); 3], mode, peak, op);
        assert!(at_peak.iter().all(|v| (*v - 1.0).abs() < 5e-3), "{at_peak:?}");
        let over = tonemap_linear([1.0; 3], mode, peak, op);
        assert!(over.iter().all(|v| (*v - 1.0).abs() < 1e-3), "{over:?}");
        let mut last = -1.0;
        for i in 0..=2000 {
            let nits = 0.2 + i as f32;
            let out = tonemap_linear([pq_inverse_eotf(nits); 3], mode, peak, op);
            for v in out {
                assert!(v.is_finite(), "{nits} nits");
                assert!((0.0..=1.0).contains(&v), "{nits} nits -> {v}");
                assert!(v >= last - 1e-6, "monotonicidade em {nits} nits: {last} -> {v}");
            }
            last = out[0];
        }
    }

    /// BT.2390 preserva matiz: o ganho é único para os três canais (Hable, por
    /// canal, não garante isso). Tom de 1000/500/250 nits continua 4:2:1.
    #[test]
    fn bt2390_preserva_matiz() {
        let mode = ToneMapMode::Pq;
        let rgb = [pq_inverse_eotf(1000.0), pq_inverse_eotf(500.0), pq_inverse_eotf(250.0)];
        let out = tonemap_linear(rgb, mode, 1000.0, Operator::Bt2390);
        assert!((out[0] / out[1] - 2.0).abs() < 5e-3, "{out:?}");
        assert!((out[1] / out[2] - 2.0).abs() < 5e-3, "{out:?}");
        // ... e o valor abaixo do joelho passa intacto (diferente do Hable).
        let dark = tonemap_linear([pq_inverse_eotf(20.0); 3], mode, 1000.0, Operator::Bt2390);
        let want = 20.0 / REFERENCE_WHITE_NITS;
        assert!((dark[0] - want).abs() < 0.01, "{dark:?} (esperado ~{want})");
    }

    /// A EETF local concorda com a do `filmcraft_color` (f64, usada pelo
    /// pipeline de cor do hub) nos picos e nits típicos de HDR.
    #[test]
    fn bt2390_concorda_com_filmcraft_color() {
        for (src, dst) in [(1000.0f32, 203.0f32), (4000.0, 203.0), (1000.0, 100.0)] {
            for nits in [1.0f32, 50.0, 100.0, 203.0, 400.0, 700.0, 1000.0, 1500.0, 4000.0] {
                let ours = bt2390_eetf(nits, src, dst);
                let theirs = filmcraft_color::transform::bt2390_eetf(f64::from(nits), f64::from(src), f64::from(dst)) as f32;
                let tol = (nits.max(dst) * 2e-3).max(0.5);
                assert!((ours - theirs).abs() <= tol, "src {src} nits {nits}: {ours} vs {theirs}");
            }
        }
    }

    /// A escolha do default: no mesmo PQ (pico 1000), BT.2390 entrega os tons
    /// médios bem mais claros que o Hable — é o motivo de ele ser o operador
    /// padrão (o Hable escurece a imagem inteira para abrir ombro até o pico).
    #[test]
    fn bt2390_preserva_mais_os_meios_tons_que_hable() {
        for nits in [50.0f32, 100.0, 150.0] {
            let code = pq_inverse_eotf(nits);
            let b = tonemap_linear([code; 3], ToneMapMode::Pq, 1000.0, Operator::Bt2390)[0];
            let h = tonemap_linear([code; 3], ToneMapMode::Pq, 1000.0, Operator::Hable)[0];
            assert!(b > h * 1.3, "{nits} nits: bt2390 {b} vs hable {h}");
        }
        // Os dois continuam mapeando o pico do conteúdo para o branco SDR.
        let peak = pq_inverse_eotf(1000.0);
        for op in [Operator::Bt2390, Operator::Hable] {
            let v = tonemap_linear([peak; 3], ToneMapMode::Pq, 1000.0, op)[0];
            assert!((v - 1.0).abs() < 5e-3, "{op:?}: {v}");
        }
    }

    /// HLG usa pico relativo fixo de 1000 nits; o topo do sinal chega ao branco
    /// e o env não muda o resultado (nos dois operadores).
    #[test]
    fn tonemap_hlg_usa_1000_relativo() {
        for op in [Operator::Bt2390, Operator::Hable] {
            let hlg = ToneMapParams::for_color(&ColorInfo { transfer: Transfer::Hlg, ..ColorInfo::REC709 }, 4000.0, op);
            assert_eq!(hlg.peak_nits, HLG_PEAK_NITS);
            let out = tonemap_linear([1.0; 3], hlg.mode, hlg.peak_nits, hlg.operator);
            assert!(out.iter().all(|v| (0.0..=1.0).contains(v) && (*v - 1.0).abs() < 2e-3), "{op:?}: {out:?}");
            // tons médios não estouram nem viram NaN
            for code in [0.25, 0.5, 0.75] {
                let out = tonemap_linear([code; 3], hlg.mode, hlg.peak_nits, hlg.operator);
                assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{op:?} {code} -> {out:?}");
            }
        }
    }

    /// SDR não passa por tonemap nenhum.
    #[test]
    fn sdr_passa_direto() {
        let c = [0.2, 0.5, 0.9];
        for op in [Operator::Bt2390, Operator::Hable] {
            assert_eq!(tonemap_linear(c, ToneMapMode::Off, 1000.0, op), c);
        }
        let p = ToneMapParams::for_color(&ColorInfo::REC709, 1000.0, Operator::Bt2390);
        assert_eq!(p.mode, ToneMapMode::Off);
        assert_eq!(p.peak_nits, 0.0);
    }

    /// Pico: sinalizado pelo arquivo, senão env/default, sempre na faixa.
    #[test]
    fn pico_do_pq_respeita_limites() {
        let pq = ColorInfo { transfer: Transfer::Pq, ..ColorInfo::REC709 };
        let op = Operator::Bt2390;
        assert_eq!(ToneMapParams::for_color(&pq, 1000.0, op).peak_nits, 1000.0);
        assert_eq!(ToneMapParams::for_color(&pq, 4000.0, op).peak_nits, 4000.0);
        assert_eq!(ToneMapParams::for_color(&pq, 50_000.0, op).peak_nits, MAX_PEAK_NITS);
        assert_eq!(ToneMapParams::for_color(&pq, 1.0, op).peak_nits, MIN_PEAK_NITS);
        assert_eq!(ToneMapParams::for_color(&pq, f32::NAN, op).peak_nits, DEFAULT_PEAK_NITS);
        assert_eq!(ToneMapParams::for_color(&pq, 0.0, op).peak_nits, DEFAULT_PEAK_NITS);
    }

    /// O parse do env é estrito: lixo não vira pico.
    #[test]
    fn parse_do_env() {
        assert_eq!(parse_peak_nits("1000"), Some(1000.0));
        assert_eq!(parse_peak_nits(" 800 "), Some(800.0));
        assert_eq!(parse_peak_nits("999999"), Some(MAX_PEAK_NITS));
        assert_eq!(parse_peak_nits("1"), Some(MIN_PEAK_NITS));
        for bad in ["", "abc", "0", "-100", "NaN", "inf"] {
            assert_eq!(parse_peak_nits(bad), None, "{bad}");
        }
    }

    /// Operador: BT.2390 é o default e o env aceita os dois nomes.
    #[test]
    fn operador_default_e_parse() {
        assert_eq!(Operator::default(), Operator::Bt2390);
        assert_eq!(Operator::parse("BT2390"), Some(Operator::Bt2390));
        assert_eq!(Operator::parse(" bt2390 "), Some(Operator::Bt2390));
        assert_eq!(Operator::parse("Hable"), Some(Operator::Hable));
        for bad in ["", "reinhard", "pq"] {
            assert_eq!(Operator::parse(bad), None, "{bad}");
        }
    }

    /// A linha do uniform tem modo, pico e operador nas três primeiras posições.
    #[test]
    fn linha_do_uniform() {
        let pq = ToneMapParams::for_color(&ColorInfo { transfer: Transfer::Pq, ..ColorInfo::REC709 }, 800.0, Operator::Hable);
        assert_eq!(pq.uniform_row(), [1.0, 800.0, 1.0, 0.0]);
        let hlg = ToneMapParams::for_color(&ColorInfo { transfer: Transfer::Hlg, ..ColorInfo::REC709 }, 800.0, Operator::Bt2390);
        assert_eq!(hlg.uniform_row(), [2.0, 1000.0, 0.0, 0.0]);
        let sdr = ToneMapParams::for_color(&ColorInfo::REC709, 800.0, Operator::Bt2390);
        assert_eq!(sdr.uniform_row(), [0.0, 0.0, 0.0, 0.0]);
    }

    /// Transferências não-HDR (inclusive a default do `ColorInfo`) não tonemapam.
    #[test]
    fn somente_pq_e_hlg_ativam() {
        for t in [Transfer::Bt709, Transfer::Srgb, Transfer::Linear] {
            assert_eq!(ToneMapMode::from_transfer(t), ToneMapMode::Off, "{t:?}");
        }
        assert_eq!(ToneMapMode::from_transfer(Transfer::Pq), ToneMapMode::Pq);
        assert_eq!(ToneMapMode::from_transfer(Transfer::Hlg), ToneMapMode::Hlg);
        // matriz 2020 com transferência SDR continua SDR
        let mut info = ColorInfo::REC709;
        info.matrix = Matrix::Bt2020Ncl;
        assert!(!ToneMapParams::for_color(&info, 1000.0, Operator::Bt2390).mode.is_active());
    }

    /// O encode sRGB fecha o round-trip com a curva de decodificação do shader.
    #[test]
    fn encode_srgb_roundtrip() {
        for v in [0.0, 0.01, 0.18, 0.5, 1.0] {
            assert!((filmcraft_color::srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-5, "{v}");
        }
    }
}
