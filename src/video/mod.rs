//! Vídeo da fase 3: frame decodificado → tela.
//!
//! Divisão:
//! - [`present`] — escolha do frame que deve estar na tela por PTS (drop/repeat),
//!   matemática pura e testável;
//! - [`color`] — YUV → R'G'B' (BT.601/709/2020 + faixa limitada/completa), pura;
//! - [`tone`] — HDR → SDR (EOTF PQ/HLG + Hable) em CPU, espelho do shader;
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

/// Um frame decodificado, com os planos prontos para upload na GPU.
///
/// Os planos compartilham os buffers do cache do decodificador (`Arc`), então
/// copiar o frame entre threads é barato; só o `Yuv16` (>8 bits) vira `u8` na
/// thread de decode.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    pub planes: [Arc<Vec<u8>>; 3],
    pub color: ColorInfo,
    /// Tempo de apresentação em segundos (tempo de mídia).
    pub pts: f64,
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
}
