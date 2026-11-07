//! Letterbox e rotação em quartos — matemática pura, sem GPU.
//!
//! O retângulo do vídeo é sempre **encaixado** (contain) no espaço disponível:
//! barras pretas em cima/embaixo (letterbox) ou nas laterais (pillarbox),
//! centralizado. A rotação em quartos troca os eixos quando é ímpar e gira a
//! amostragem no shader (a textura nunca é regravada).

/// Retângulo em pixels (ou pontos, para a chamada pura).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Aspecto de exibição (largura / altura) com PAR e rotação aplicados.
pub fn display_aspect(width: u32, height: u32, par: (u32, u32), rotation: u8) -> f32 {
    if width == 0 || height == 0 {
        return 1.0;
    }
    let par = if par.0 > 0 && par.1 > 0 { par.0 as f32 / par.1 as f32 } else { 1.0 };
    let (w, h) = if rotation % 2 == 1 { (height as f32, width as f32) } else { (width as f32, height as f32) };
    (w * par) / h
}

/// Tolerância de aspecto para tratar vídeo e janela como "casados": dentro de
/// 1% de diferença, pinta borda-a-borda em vez de sobrar uma moldura de poucos
/// pixels (que a olho nu parece defeito de layout). O esticamento máximo é 1%.
pub const ASPECT_TOLERANCE: f32 = 0.01;

/// Encaixa uma imagem de `src_w × src_h` (pixels de exibição) dentro de `dst`,
/// preservando o aspecto e centralizando. Aspecto praticamente igual ao do
/// destino ([`ASPECT_TOLERANCE`]) devolve `dst` inteiro: barras pretas só
/// quando o aspecto difere de verdade. Entradas inválidas devolvem `dst`.
pub fn letterbox(dst: Rect, src_w: f32, src_h: f32) -> Rect {
    let finite = src_w.is_finite() && src_h.is_finite() && dst.w.is_finite() && dst.h.is_finite();
    if !finite || src_w <= 0.0 || src_h <= 0.0 || dst.w <= 0.0 || dst.h <= 0.0 {
        return dst;
    }
    let quality = (dst.w / dst.h) / (src_w / src_h);
    if (quality - 1.0).abs() <= ASPECT_TOLERANCE {
        return dst;
    }
    let scale = (dst.w / src_w).min(dst.h / src_h);
    let w = (src_w * scale).min(dst.w);
    let h = (src_h * scale).min(dst.h);
    Rect::new(dst.x + (dst.w - w) * 0.5, dst.y + (dst.h - h) * 0.5, w, h)
}

/// Clampa um viewport para dentro da tela: o wgpu exige `0 ≤ x`, `0 ≤ y`,
/// `x + w ≤ largura` e `y + h ≤ altura` (um arredondamento de 1 px estouraria).
pub fn fit_viewport(r: Rect, screen_w: f32, screen_h: f32) -> Rect {
    let x = r.x.clamp(0.0, (screen_w - 1.0).max(0.0));
    let y = r.y.clamp(0.0, (screen_h - 1.0).max(0.0));
    let w = r.w.min(screen_w - x).max(1.0);
    let h = r.h.min(screen_h - y).max(1.0);
    Rect::new(x, y, w, h)
}

/// Transformação de UV de saída → UV de fonte para uma rotação **horária** em
/// quartos: `src = (a·u + c·v + ox, b·u + d·v + oy)`, com UV em 0..1 e origem
/// no canto superior esquerdo.
pub fn uv_transform(rotation: u8) -> ([f32; 4], [f32; 2]) {
    match rotation % 4 {
        // 90° horário: (u,v) → (v, 1-u)
        1 => ([0.0, -1.0, 1.0, 0.0], [0.0, 1.0]),
        // 180°: (u,v) → (1-u, 1-v)
        2 => ([-1.0, 0.0, 0.0, -1.0], [1.0, 1.0]),
        // 270° horário: (u,v) → (1-v, u)
        3 => ([0.0, 1.0, -1.0, 0.0], [1.0, 0.0]),
        _ => ([1.0, 0.0, 0.0, 1.0], [0.0, 0.0]),
    }
}

/// Aplica [`uv_transform`] a um UV (o mesmo cálculo do shader; usado nos testes).
#[cfg(test)]
pub fn rotate_uv(rotation: u8, u: f32, v: f32) -> (f32, f32) {
    let (m, o) = uv_transform(rotation);
    (m[0] * u + m[2] * v + o[0], m[1] * u + m[3] * v + o[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    /// 16:9 dentro de um quadrado 100×100: barras em cima/embaixo, centralizado.
    #[test]
    fn letterbox_16_9_em_quadrado() {
        let r = letterbox(Rect::new(0.0, 0.0, 100.0, 100.0), 1920.0, 1080.0);
        assert!(close(r.w, 100.0) && close(r.h, 56.25), "{r:?}");
        assert!(close(r.x, 0.0) && close(r.y, 21.875), "{r:?}");
    }

    /// Retrato dentro de paisagem: barras nas laterais (pillarbox).
    #[test]
    fn pillarbox_de_retrato() {
        let r = letterbox(Rect::new(10.0, 20.0, 200.0, 100.0), 1080.0, 1920.0);
        assert!(close(r.h, 100.0), "{r:?}");
        assert!(close(r.w, 56.25), "{r:?}");
        assert!(close(r.x, 10.0 + (200.0 - 56.25) / 2.0), "{r:?}");
        assert!(close(r.y, 20.0), "{r:?}");
    }

    /// Aspecto praticamente igual ao da janela (dentro de 1%): borda-a-borda,
    /// sem a moldura de poucos pixels que sobrava do arredondamento.
    #[test]
    fn aspecto_quase_igual_pinta_borda_a_borda() {
        // 1920×1080 numa janela 1920×1072: 0,7% de diferença, antes sobrava
        // ~14 px de moldura lateral.
        let dst = Rect::new(0.0, 0.0, 1920.0, 1072.0);
        assert_eq!(letterbox(dst, 1920.0, 1080.0), dst);
        // 4:3 numa janela 1,34:1 (0,7%) também casa.
        let dst = Rect::new(5.0, 5.0, 800.0, 600.0);
        assert_eq!(letterbox(dst, 1600.0, 1200.0), dst);
        // Fora da tolerância (4:3 em 16:9) continua letterbox de verdade.
        let r = letterbox(Rect::new(0.0, 0.0, 1600.0, 900.0), 640.0, 480.0);
        assert!(close(r.h, 900.0) && close(r.w, 1200.0) && close(r.x, 200.0), "{r:?}");
        // Exatamente no limite (1%) ainda casa; um fio acima, não.
        let dst = Rect::new(0.0, 0.0, 1010.0, 1000.0);
        assert_eq!(letterbox(dst, 1000.0, 1000.0), dst);
        let dst = Rect::new(0.0, 0.0, 1011.0, 1000.0);
        assert!(letterbox(dst, 1000.0, 1000.0).w < dst.w);
    }

    /// Rotação ímpar troca o aspecto: 16:9 girado 90° vira 9:16 (pillarbox).
    #[test]
    fn rotacao_troca_o_aspecto() {
        assert!(close(display_aspect(1920, 1080, (1, 1), 0), 16.0 / 9.0));
        assert!(close(display_aspect(1920, 1080, (1, 1), 1), 9.0 / 16.0));
        assert!(close(display_aspect(1920, 1080, (1, 1), 2), 16.0 / 9.0));
    }

    /// PAR anamórfico entra no aspecto de exibição.
    #[test]
    fn par_entra_no_aspecto() {
        assert!(close(display_aspect(720, 480, (32, 27), 0), (720.0 * 32.0 / 27.0) / 480.0));
        // PAR inválido = quadrado.
        assert!(close(display_aspect(720, 480, (0, 0), 0), 720.0 / 480.0));
    }

    /// Os quatro cantos imageados pelas quatro rotações.
    #[test]
    fn rotacoes_mapeiam_os_cantos() {
        // identidade
        assert_eq!(rotate_uv(0, 0.0, 0.0), (0.0, 0.0));
        // 90° horário: canto sup. esq. da tela = canto inf. esq. da fonte
        let (u, v) = rotate_uv(1, 0.0, 0.0);
        assert!(close(u, 0.0) && close(v, 1.0));
        let (u, v) = rotate_uv(1, 1.0, 0.0);
        assert!(close(u, 0.0) && close(v, 0.0));
        // 180°
        let (u, v) = rotate_uv(2, 0.0, 0.0);
        assert!(close(u, 1.0) && close(v, 1.0));
        // 270° horário: canto sup. esq. = canto sup. dir. da fonte
        let (u, v) = rotate_uv(3, 0.0, 0.0);
        assert!(close(u, 1.0) && close(v, 0.0));
    }

    /// Dimensões inválidas não explodem: devolve o destino.
    #[test]
    fn dimensoes_invalidas_devolvem_o_destino() {
        let dst = Rect::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(letterbox(dst, 0.0, 10.0), dst);
        assert_eq!(letterbox(dst, f32::NAN, 10.0), dst);
        assert_eq!(letterbox(Rect::new(0.0, 0.0, 0.0, 0.0), 16.0, 9.0), Rect::new(0.0, 0.0, 0.0, 0.0));
    }

    /// O viewport do wgpu nunca sai da tela (nem com 1 px de sobra).
    #[test]
    fn viewport_fica_dentro_da_tela() {
        let r = fit_viewport(Rect::new(-0.4, 5.0, 200.0, 100.0), 100.0, 80.0);
        assert_eq!(r.x, 0.0);
        assert!(r.x + r.w <= 100.0 && r.y + r.h <= 80.0, "{r:?}");
        let inside = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(fit_viewport(inside, 100.0, 100.0), inside);
    }
}
