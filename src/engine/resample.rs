//! Resample linear stereo — a conversão de taxa que a fase 2 usa quando a
//! taxa da faixa difere da taxa do device.
//!
//! **Limitação explícita:** interpolação linear é barata e sem dependência,
//! mas tem roll-off nos agudos e algum aliasing na decimação. Para música
//! (44.1/48 kHz) a diferença é inaudível na prática; um sinc/polyphase fica
//! para quando o motor de vídeo exigir precisão de relógio.

/// Resampler linear com estado entre chunks (posição fracionária e última
/// amostra sobrevivem à fronteira — sem cliques a cada pacote).
#[derive(Debug, Clone)]
pub struct LinearResampler {
    /// Passo em amostras de entrada por amostra de saída (`src_rate / dst_rate`).
    step: f64,
    /// Posição fracionária da próxima saída entre `prev` e a amostra atual.
    frac: f64,
    /// Última amostra de entrada processada (L, R).
    prev: [f32; 2],
    primed: bool,
}

impl LinearResampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        let step = if dst_rate == 0 { 1.0 } else { f64::from(src_rate) / f64::from(dst_rate) };
        Self { step, frac: 0.0, prev: [0.0; 2], primed: false }
    }

    /// Zera o estado (seek/troca de faixa): não interpola através do corte.
    pub fn reset(&mut self) {
        self.frac = 0.0;
        self.prev = [0.0; 2];
        self.primed = false;
    }

    /// Converte `input` (entrelaçado stereo) para `out` (entrelaçado stereo),
    /// na taxa de destino. `out` é **append**: o chamador limpa antes.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let frame = |i: usize| -> [f32; 2] { [input[2 * i], input[2 * i + 1]] };
        let mut i = 0;
        if !self.primed {
            self.prev = frame(0);
            self.primed = true;
            // A primeira saída é a própria amostra inicial (posição 0).
            out.push(self.prev[0]);
            out.push(self.prev[1]);
            self.frac += self.step;
            i = 1;
        }
        while i < frames {
            let cur = frame(i);
            while self.frac < 1.0 {
                let f = self.frac as f32;
                out.push(self.prev[0] + (cur[0] - self.prev[0]) * f);
                out.push(self.prev[1] + (cur[1] - self.prev[1]) * f);
                self.frac += self.step;
            }
            self.frac -= 1.0;
            self.prev = cur;
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(data: &[f32]) -> usize {
        data.len() / 2
    }

    #[test]
    fn taxa_igual_reproduz_a_sequencia_sem_o_ultimo_frame() {
        let mut r = LinearResampler::new(48_000, 48_000);
        let input: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        // O último frame fica retido até o próximo chunk (não há "próximo"
        // frame para interpolar); no fim do stream isso é 1 amostra só.
        assert_eq!(out, input[..6]);
    }

    #[test]
    fn metade_da_taxa_pega_amostras_alternadas() {
        let mut r = LinearResampler::new(48_000, 24_000);
        // Amostras L=i, R=-i.
        let input: Vec<f32> = (0..10).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(frames(&out), 5);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[2], 2.0);
        assert_eq!(out[4], 4.0);
        assert_eq!(out[8], 8.0);
    }

    #[test]
    fn dobro_da_taxa_interpola_meio() {
        let mut r = LinearResampler::new(24_000, 48_000);
        let input = [0.0, 0.0, 2.0, -2.0, 4.0, -4.0];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(frames(&out), 4);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[2], 1.0);
        assert_eq!(out[3], -1.0);
        assert_eq!(out[4], 2.0);
        assert_eq!(out[5], -2.0);
        assert_eq!(out[6], 3.0);
    }

    #[test]
    fn chunk_a_chunk_equivale_a_um_bloco_so() {
        let input: Vec<f32> = (0..200).flat_map(|i| [i as f32, (i * 2) as f32]).collect();
        let mut whole = Vec::new();
        LinearResampler::new(44_100, 48_000).process(&input, &mut whole);
        let mut split = Vec::new();
        let mut r = LinearResampler::new(44_100, 48_000);
        for chunk in input.chunks(14) {
            r.process(chunk, &mut split);
        }
        assert_eq!(frames(&whole), frames(&split));
        assert!(whole.iter().zip(&split).all(|(a, b)| (a - b).abs() < 1e-4));
    }

    #[test]
    fn sinal_constante_fica_constante() {
        let mut r = LinearResampler::new(44_100, 48_000);
        let input = vec![0.5f32; 2 * 1000];
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }
}
