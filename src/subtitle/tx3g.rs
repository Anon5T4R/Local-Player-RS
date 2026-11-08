//! Legenda embutida `tx3g` / `mov_text` (ISO/IEC 14496-17 e QuickTime `text`).
//!
//! Cada amostra do container é: `u16` tamanho do texto (big-endian) seguido do
//! texto UTF-8 — opcionalmente com caixas de estilo depois (`styl`, `tbox`,
//! `hlit`, `hclr`, `blnk`). O player desenha o texto com o estilo padrão do SRT;
//! o que fica de fora, por limitação documentada:
//!
//! - estilo por trecho (`styl`: fonte, cor, negrito);
//! - caixa de fundo e posição (`tbox`);
//! - realce/karaokê (`hlit`/`hclr`) e `blnk`.
//!
//! Tempos vêm do container (amostra com PTS e duração), não do payload.

use crate::subtitle::{Cue, Segment, Style};

use super::EmbeddedSample;

/// Texto de uma amostra tx3g. Tolera amostras de codificadores que omitem o
/// prefixo de tamanho (o texto é então a amostra inteira) e NULs de
/// preenchimento/quebras antigas.
pub fn text_of(data: &[u8]) -> String {
    if data.len() < 2 {
        return String::new();
    }
    let declared = u16::from_be_bytes([data[0], data[1]]) as usize;
    // Prefixo ausente (codificador fora da norma): os dois primeiros bytes não
    // cabem no resto e o primeiro é texto imprimível — a amostra inteira é o
    // texto. Prefixo truncado (primeiro byte de controle): usa o que existe.
    let body = if declared > data.len().saturating_sub(2) {
        if data[0] >= 0x20 && data[0] != 0x7F {
            data
        } else {
            &data[2..]
        }
    } else {
        &data[2..2 + declared]
    };
    let text = String::from_utf8_lossy(body);
    text.trim_end_matches('\0').replace('\u{0000}', "").to_owned()
}

/// Converte as amostras da faixa em cues. Amostras vazias são puladas; cues que
/// se sobrepõem ficam como estão (a UI mostra a mais recente, como no SRT).
pub fn cues(samples: &[EmbeddedSample]) -> Vec<Cue> {
    let mut cues: Vec<Cue> = samples
        .iter()
        .filter(|s| s.end > s.start && s.start.is_finite() && s.end.is_finite())
        .filter_map(|s| {
            let text = text_of(&s.data);
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            Some(Cue {
                start: s.start,
                end: s.end,
                segments: vec![Segment { text: text.to_owned(), bold: false, italic: false, color: None }],
                style: Style::default(),
                anchor: Default::default(),
            })
        })
        .collect();
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    cues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(start: f64, end: f64, data: &[u8]) -> EmbeddedSample {
        EmbeddedSample { start, end, data: data.to_vec() }
    }

    fn boxed(text: &str) -> Vec<u8> {
        let mut v = (text.len() as u16).to_be_bytes().to_vec();
        v.extend_from_slice(text.as_bytes());
        v
    }

    #[test]
    fn le_tamanho_prefixado() {
        assert_eq!(text_of(&boxed("olá")), "olá");
        // Caixas de estilo depois do texto são ignoradas.
        let mut with_style = boxed("cue");
        with_style.extend_from_slice(&[0, 0, 0, 8, b's', b't', b'y', b'l']);
        assert_eq!(text_of(&with_style), "cue");
    }

    #[test]
    fn tolera_prefixo_ausente_e_nul() {
        assert_eq!(text_of(b"sem prefixo"), "sem prefixo");
        // Amostra truncada com prefixo de controle: devolve o texto que existe.
        assert_eq!(text_of(&[0, 9, b'a', b'b']), "ab");
        assert_eq!(text_of(&[]), "");
        assert_eq!(text_of(&[7]), "");
        let mut nul = boxed("fim\0");
        nul[2 + 3] = 0;
        assert_eq!(text_of(&nul), "fim");
    }

    #[test]
    fn amostras_viram_cues_com_duracao() {
        let cues = cues(&[sample(2.0, 3.5, &boxed("segunda")), sample(0.5, 1.2, &boxed("primeira")), sample(4.0, 5.0, &boxed("   "))]);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].segments[0].text, "primeira");
        assert_eq!((cues[0].start, cues[0].end), (0.5, 1.2));
        assert_eq!(cues[1].segments[0].text, "segunda");
    }
}
