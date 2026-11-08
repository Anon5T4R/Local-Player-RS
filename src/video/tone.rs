//! HDR → SDR: EOTF + tone mapping, com espelho CPU da conta do shader.
//!
//! O shader (`yuv.wgsl`) faz a conta por pixel; este módulo existe para (a)
//! decidir **se** e **com que pico** tonemapar a partir do [`ColorInfo`] do
//! container e (b) ter a mesma matemática em CPU, testável com os valores da
//! spec — o WGSL só é validado pelo `naga`, não executa em teste.
//!
//! ## Operador escolhido: Hable (filmic, Uncharted 2)
//!
//! `y = hable(x) / hable(white)`, com `x` em luz de display (1.0 = branco de
//! referência SDR, 203 nits) e `white` = pico do conteúdo na mesma unidade.
//! - **Por quê:** é barato (um racional + uma divisão por canal), monotônico,
//!   `0 → 0`, pico → 1 e tem ombro suave nos realces. Comparado a Reinhard com
//!   white point, preserva mais contraste nos tons médios — exatamente o
//!   sintoma que o PQ/HLG tratado como SDR produzia (imagem "lavada").
//! - **Trade-off:** o branco de referência (203 nits) cai em ~0,39 de luz
//!   linear (~0,66 em sRGB) para deixar ombro até o pico; cenas HDR escuras
//!   ficam com aparência mais cinematográfica (e mais escuras) que no SDR
//!   nativo. Matiz pode desviar em cores muito saturadas porque o operador é
//!   aplicado por canal; o `ToneMap` BT.2390 do `filmcraft_color` (que usa
//!   max(R,G,B) e dessatura) é a alternativa mais "neutra" para uma iteração
//!   futura. A conversão de gamut BT.2020 → BT.709 também fica para depois: o
//!   shader já usava a matriz do container sem converter primárias.
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

/// Parâmetros resolvidos para um frame: o que o uniform e o overlay precisam.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneMapParams {
    pub mode: ToneMapMode,
    /// Pico do conteúdo em nits (0 quando `Off`).
    pub peak_nits: f32,
}

impl ToneMapParams {
    /// Pico do PQ: o valor já resolvido pelo chamador (sinalizado pelo arquivo >
    /// env > default), sempre na faixa aceita. HLG ignora o valor e usa
    /// [`HLG_PEAK_NITS`] (relativo).
    pub fn for_color(color: &ColorInfo, peak_nits: f32) -> Self {
        let mode = ToneMapMode::from_transfer(color.transfer);
        let peak = match mode {
            ToneMapMode::Off => 0.0,
            ToneMapMode::Hlg => HLG_PEAK_NITS,
            ToneMapMode::Pq => {
                let p = if peak_nits.is_finite() && peak_nits > 0.0 { peak_nits } else { DEFAULT_PEAK_NITS };
                p.clamp(MIN_PEAK_NITS, MAX_PEAK_NITS)
            }
        };
        Self { mode, peak_nits: peak }
    }

    /// Linha `tone` do uniform: `(mode, peak_nits, 0, 0)`.
    pub fn uniform_row(self) -> [f32; 4] {
        [self.mode.code(), self.peak_nits, 0.0, 0.0]
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

/// Um canal do sinal → luz de display (1.0 = branco de referência SDR).
#[cfg(test)]
pub fn decode_to_display(v: f32, mode: ToneMapMode) -> f32 {
    match mode {
        ToneMapMode::Off => v,
        ToneMapMode::Pq => pq_eotf_nits(v) / REFERENCE_WHITE_NITS,
        ToneMapMode::Hlg => hlg_ootf(hlg_inverse_oetf(v)) * (HLG_PEAK_NITS / REFERENCE_WHITE_NITS),
    }
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
pub fn tonemap_linear(rgb: [f32; 3], mode: ToneMapMode, peak_nits: f32) -> [f32; 3] {
    if !mode.is_active() {
        return rgb;
    }
    let white = (if peak_nits.is_finite() { peak_nits } else { DEFAULT_PEAK_NITS }).max(1.0) / REFERENCE_WHITE_NITS;
    let scale = 1.0 / hable(white).max(1e-6);
    rgb.map(|v| (hable(decode_to_display(v, mode)) * scale).clamp(0.0, 1.0))
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

    /// Tonemap PQ 1000 nits: preto fica preto, o pico do conteúdo vira branco
    /// e a curva é monotônica e finita em toda a faixa (inclusive acima do pico,
    /// que satura em 1).
    #[test]
    fn tonemap_pq_monotonico_e_limites() {
        let mode = ToneMapMode::Pq;
        let peak = 1000.0;
        for c in 0..3 {
            assert!(tonemap_linear([0.0; 3], mode, peak)[c].abs() < 1e-7);
        }
        let at_peak = tonemap_linear([0.751_827_1; 3], mode, peak);
        for v in at_peak {
            assert!((v - 1.0).abs() < 1e-3, "pico PQ -> {v}");
        }
        let mut last = -1.0;
        for i in 0..=1000 {
            let code = i as f32 / 1000.0;
            let out = tonemap_linear([code; 3], mode, peak);
            for v in out {
                assert!(v.is_finite(), "code {code}");
                assert!((0.0..=1.0).contains(&v), "code {code} -> {v}");
                assert!(v >= last - 1e-6, "monotonicidade em {code}: {last} -> {v}");
            }
            last = out[0];
        }
        // acima do pico satura em 1 em vez de estourar
        let over = tonemap_linear([1.0; 3], mode, peak);
        assert!(over.iter().all(|v| (*v - 1.0).abs() < 1e-6));
    }

    /// HLG usa pico relativo fixo de 1000 nits; o topo do sinal chega ao branco
    /// e o env não muda o resultado.
    #[test]
    fn tonemap_hlg_usa_1000_relativo() {
        let hlg = ToneMapParams::for_color(&ColorInfo { transfer: Transfer::Hlg, ..ColorInfo::REC709 }, 4000.0);
        assert_eq!(hlg.peak_nits, HLG_PEAK_NITS);
        let out = tonemap_linear([1.0; 3], hlg.mode, hlg.peak_nits);
        assert!(out.iter().all(|v| (0.0..=1.0).contains(v) && (*v - 1.0).abs() < 2e-3), "{out:?}");
        // tons médios não estouram nem viram NaN
        for code in [0.25, 0.5, 0.75] {
            let out = tonemap_linear([code; 3], hlg.mode, hlg.peak_nits);
            assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{code} -> {out:?}");
        }
    }

    /// SDR não passa por tonemap nenhum.
    #[test]
    fn sdr_passa_direto() {
        let c = [0.2, 0.5, 0.9];
        assert_eq!(tonemap_linear(c, ToneMapMode::Off, 1000.0), c);
        let p = ToneMapParams::for_color(&ColorInfo::REC709, 1000.0);
        assert_eq!(p.mode, ToneMapMode::Off);
        assert_eq!(p.peak_nits, 0.0);
    }

    /// Pico: sinalizado pelo arquivo, senão env/default, sempre na faixa.
    #[test]
    fn pico_do_pq_respeita_limites() {
        let pq = ColorInfo { transfer: Transfer::Pq, ..ColorInfo::REC709 };
        assert_eq!(ToneMapParams::for_color(&pq, 1000.0).peak_nits, 1000.0);
        assert_eq!(ToneMapParams::for_color(&pq, 4000.0).peak_nits, 4000.0);
        assert_eq!(ToneMapParams::for_color(&pq, 50_000.0).peak_nits, MAX_PEAK_NITS);
        assert_eq!(ToneMapParams::for_color(&pq, 1.0).peak_nits, MIN_PEAK_NITS);
        assert_eq!(ToneMapParams::for_color(&pq, f32::NAN).peak_nits, DEFAULT_PEAK_NITS);
        assert_eq!(ToneMapParams::for_color(&pq, 0.0).peak_nits, DEFAULT_PEAK_NITS);
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

    /// A linha do uniform tem modo e pico nas duas primeiras posições.
    #[test]
    fn linha_do_uniform() {
        let pq = ToneMapParams::for_color(&ColorInfo { transfer: Transfer::Pq, ..ColorInfo::REC709 }, 800.0);
        assert_eq!(pq.uniform_row(), [1.0, 800.0, 0.0, 0.0]);
        let hlg = ToneMapParams::for_color(&ColorInfo { transfer: Transfer::Hlg, ..ColorInfo::REC709 }, 800.0);
        assert_eq!(hlg.uniform_row(), [2.0, 1000.0, 0.0, 0.0]);
        let sdr = ToneMapParams::for_color(&ColorInfo::REC709, 800.0);
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
        assert!(!ToneMapParams::for_color(&info, 1000.0).mode.is_active());
    }

    /// O encode sRGB fecha o round-trip com a curva de decodificação do shader.
    #[test]
    fn encode_srgb_roundtrip() {
        for v in [0.0, 0.01, 0.18, 0.5, 1.0] {
            assert!((filmcraft_color::srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-5, "{v}");
        }
    }
}
