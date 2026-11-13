//! Vídeo da fase 3: frame decodificado → tela.
//!
//! Divisão:
//! - [`present`] — escolha do frame que deve estar na tela por PTS (drop/repeat),
//!   matemática pura e testável;
//! - [`color`] — YUV → R'G'B' (BT.601/709/2020 + faixa limitada/completa), pura;
//! - [`tone`] — HDR → SDR (EOTF PQ/HLG + tonemap BT.2390/Hable) em CPU, espelho
//!   do shader;
//! - [`geometry`] — letterbox e transformação de UV da rotação, pura;
//! - [`gpu`] — texturas wgpu + shader WGSL e o paint callback do egui.
//!
//! O frame chega da thread de decode do motor (`engine::video`) já com os planos
//! YUV prontos; aqui nada é decodificado.

pub mod color;
pub mod geometry;
pub mod gpu;
pub mod present;
pub mod tone;

use std::sync::Arc;

use filmcraft_color::ColorInfo;
use filmcraft_frame::Chroma;

/// Maior lado aceito para decodificar/renderizar. 8K (7680) com folga; o device
/// pode baixar ainda mais ([`gpu::VideoGpu`] usa o menor entre este e o limite
/// do wgpu). Frames acima disso viram erro claro, nunca OOM silencioso.
pub const MAX_DIMENSION: u32 = 8192;

/// Tamanho (largura, altura) do plano de croma de um frame.
pub fn chroma_dims(width: u32, height: u32, chroma: Chroma) -> (u32, u32) {
    let (sx, sy) = chroma.shifts();
    let ceil = |v: u32, shift: u32| ((v + (1 << shift) - 1) >> shift).max(1);
    (ceil(width, sx), ceil(height, sy))
}

/// Amostras de um frame prontas para o upload na GPU.
///
/// Os dois casos compartilham a **convenção do shader**: a amostra vale
/// `código de 8 bits / 255` (a matriz de [`color::yuv_to_rgb`] foi montada com
/// os códigos 16–235/0–255). Em 10/12 bits a thread de decode converte o código
/// real para essa escala (ver `engine::video::to_unorm16`), então o WGSL não
/// precisa saber a profundidade de bits — só a textura muda de formato.
#[derive(Clone, Debug)]
pub enum Planes {
    /// 8 bits por amostra, uma amostra por byte (textura `R8Unorm`).
    U8([Arc<Vec<u8>>; 3]),
    /// 16 bits por amostra, little-endian (textura `R16Unorm`): preserva os
    /// ~10 bits reais do HDR até o tonemap, sem o banding do caminho antigo.
    U16([Arc<Vec<u8>>; 3]),
}

impl Planes {
    /// Bytes por amostra (1 ou 2) — o formato da textura em [`gpu`] segue daqui.
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            Self::U8(_) => 1,
            Self::U16(_) => 2,
        }
    }

    /// Bytes crus do plano `plane` (0 = Y, 1 = U, 2 = V).
    pub fn bytes(&self, plane: usize) -> &[u8] {
        match self {
            Self::U8(p) => &p[plane],
            Self::U16(p) => &p[plane],
        }
    }

    /// Amostras do plano `plane` (não bytes).
    pub fn samples(&self, plane: usize) -> usize {
        self.bytes(plane).len() / self.bytes_per_sample()
    }
}

/// Um frame decodificado, com os planos prontos para upload na GPU.
///
/// Os planos de 8 bits compartilham os buffers do cache do decodificador
/// (`Arc`), então copiar o frame entre threads é barato; o `Yuv16` (>8 bits) é
/// convertido na thread de decode, que é o único lugar que paga isso.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    /// Profundidade de bits da FONTE (8, 10, 12…) — vai no overlay da tecla D.
    pub bits: u32,
    pub planes: Planes,
    pub color: ColorInfo,
    /// Tempo de apresentação em segundos (tempo de mídia).
    pub pts: f64,
}

impl Frame {
    /// Origem do frame, no inglês técnico do overlay (`10-bit 4:2:0`).
    pub fn format_label(&self) -> String {
        format!("{}-bit {}", self.bits, self.chroma.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_de_croma() {
        // 4:2:0
        assert_eq!(chroma_dims(1920, 1080, Chroma::C420), (960, 540));
        // 1080 ímpar arredonda para cima (o decodificador entrega o plano cheio).
        assert_eq!(chroma_dims(1920, 1081, Chroma::C420), (960, 541));
        // 4:2:2 e 4:4:4
        assert_eq!(chroma_dims(1920, 1080, Chroma::C422), (960, 1080));
        assert_eq!(chroma_dims(1920, 1080, Chroma::C444), (1920, 1080));
        // nunca 0×0 (textura wgpu não aceita dimensão zero)
        assert_eq!(chroma_dims(1, 1, Chroma::C420), (1, 1));
    }

    /// Amostras são contadas por elemento, não por byte: 16 bits = 2 bytes.
    #[test]
    fn planos_contam_amostras() {
        let u8p = Planes::U8([Arc::new(vec![0; 8]), Arc::new(vec![0; 4]), Arc::new(vec![0; 4])]);
        assert_eq!((u8p.bytes_per_sample(), u8p.samples(0), u8p.samples(1)), (1, 8, 4));
        let u16p = Planes::U16([Arc::new(vec![0; 16]), Arc::new(vec![0; 8]), Arc::new(vec![0; 8])]);
        assert_eq!((u16p.bytes_per_sample(), u16p.samples(0), u16p.samples(1)), (2, 8, 4));
        assert_eq!(u16p.bytes(0).len(), 16);
    }
}
