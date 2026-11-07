//! Posicionamento da cue sobre a imagem — matemática pura, sem GPU.
//!
//! O alinhamento vem do próprio arquivo (numeração do ASS, `align:` do VTT) e a
//! caixa nunca sai do retângulo do vídeo: legenda maior que a imagem encosta na
//! borda em vez de vazar para o letterbox.

use super::{Anchor, HAlign, VAlign};
use crate::video::geometry::Rect;

/// Margens em pixels, já convertidas das frações do estilo pelo chamador.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Margins {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

/// Caixa da legenda de tamanho `size` dentro de `video`.
///
/// - `anchor.x`/`anchor.y` (posição explícita em fração) vencem as margens; o
///   ponto da caixa que fica na posição é o do alinhamento declarado.
/// - Sem posição explícita: margens + alinhamento (padrão ASS/VTT: centralizado
///   na horizontal, colado na base com `bottom` de margem).
pub fn place(video: Rect, size: (f32, f32), anchor: &Anchor, margins: Margins) -> Rect {
    let valid = video.w.is_finite() && video.h.is_finite() && video.w > 0.0 && video.h > 0.0;
    if !valid {
        return Rect::new(video.x, video.y, 0.0, 0.0);
    }
    let w = if size.0.is_finite() { size.0.clamp(0.0, video.w) } else { 0.0 };
    let h = if size.1.is_finite() { size.1.clamp(0.0, video.h) } else { 0.0 };
    let margin = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };

    let x = match anchor.x {
        Some(f) => {
            let point = video.x + video.w * f.clamp(0.0, 1.0);
            point
                - match anchor.h {
                    HAlign::Left => 0.0,
                    HAlign::Center => w * 0.5,
                    HAlign::Right => w,
                }
        }
        None => match anchor.h {
            HAlign::Left => video.x + margin(margins.left),
            HAlign::Center => video.x + (video.w - w) * 0.5,
            HAlign::Right => video.x + video.w - margin(margins.right) - w,
        },
    };
    let y = match anchor.y {
        Some(f) => {
            let point = video.y + video.h * f.clamp(0.0, 1.0);
            point
                - match anchor.v {
                    VAlign::Top => 0.0,
                    VAlign::Middle => h * 0.5,
                    VAlign::Bottom => h,
                }
        }
        None => match anchor.v {
            VAlign::Top => video.y + margin(margins.top),
            VAlign::Middle => video.y + (video.h - h) * 0.5,
            VAlign::Bottom => video.y + video.h - margin(margins.bottom) - h,
        },
    };

    // Cue maior que a imagem encosta na borda (nunca sai do quadro).
    let x = if w >= video.w { video.x } else { x.clamp(video.x, video.x + video.w - w) };
    let y = if h >= video.h { video.y } else { y.clamp(video.y, video.y + video.h - h) };
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn base_centralizada_como_antes() {
        let video = Rect::new(100.0, 50.0, 800.0, 450.0);
        let r = place(video, (300.0, 40.0), &Anchor::default(), Margins { bottom: 18.0, ..Default::default() });
        assert!(close(r.x, 100.0 + (800.0 - 300.0) * 0.5), "{r:?}");
        assert!(close(r.y, 50.0 + 450.0 - 18.0 - 40.0), "{r:?}");
        assert!(r.w == 300.0 && r.h == 40.0);
    }

    /// Os nove alinhamentos do ASS (numpad 1–9) sem posição explícita.
    #[test]
    fn nove_alinhamentos_numpad() {
        let v = Rect::new(0.0, 0.0, 400.0, 300.0);
        let size = (100.0, 20.0);
        let m = Margins { left: 10.0, right: 20.0, top: 5.0, bottom: 15.0 };
        let at = |h: HAlign, v_: VAlign| {
            let anchor = Anchor { h, v: v_, x: None, y: None };
            place(v, size, &anchor, m)
        };
        let bl = at(HAlign::Left, VAlign::Bottom);
        assert!(close(bl.x, 10.0) && close(bl.y, 300.0 - 15.0 - 20.0), "{bl:?}");
        let bc = at(HAlign::Center, VAlign::Bottom);
        assert!(close(bc.x, (400.0 - 100.0) * 0.5) && close(bc.y, 265.0), "{bc:?}");
        let br = at(HAlign::Right, VAlign::Bottom);
        assert!(close(br.x, 400.0 - 20.0 - 100.0) && close(br.y, 265.0), "{br:?}");
        let ml = at(HAlign::Left, VAlign::Middle);
        assert!(close(ml.x, 10.0) && close(ml.y, (300.0 - 20.0) * 0.5), "{ml:?}");
        let mc = at(HAlign::Center, VAlign::Middle);
        assert!(close(mc.x, 150.0) && close(mc.y, 140.0), "{mc:?}");
        let mr = at(HAlign::Right, VAlign::Middle);
        assert!(close(mr.x, 280.0) && close(mr.y, 140.0), "{mr:?}");
        let tl = at(HAlign::Left, VAlign::Top);
        assert!(close(tl.x, 10.0) && close(tl.y, 5.0), "{tl:?}");
        let tc = at(HAlign::Center, VAlign::Top);
        assert!(close(tc.x, 150.0) && close(tc.y, 5.0), "{tc:?}");
        let tr = at(HAlign::Right, VAlign::Top);
        assert!(close(tr.x, 280.0) && close(tr.y, 5.0), "{tr:?}");
    }

    /// Posição explícita do VTT: o ponto do alinhamento fica na fração pedida.
    #[test]
    fn posicao_explicita_vence_a_margem() {
        let v = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let size = (200.0, 50.0);
        let left = Anchor { h: HAlign::Left, v: VAlign::Top, x: Some(0.1), y: Some(0.2) };
        let r = place(v, size, &left, Margins::default());
        assert!(close(r.x, 100.0) && close(r.y, 100.0), "{r:?}");
        let center = Anchor { h: HAlign::Center, v: VAlign::Middle, x: Some(0.5), y: Some(0.5) };
        let r = place(v, size, &center, Margins::default());
        assert!(close(r.x, 400.0) && close(r.y, 225.0), "{r:?}");
        let right = Anchor { h: HAlign::Right, v: VAlign::Bottom, x: Some(0.9), y: Some(0.9) };
        let r = place(v, size, &right, Margins::default());
        assert!(close(r.x, 700.0) && close(r.y, 400.0), "{r:?}");
    }

    /// Cue maior que o vídeo é clampada (nada sai do quadro) e retângulo
    /// inválido devolve caixa vazia sem pânico.
    #[test]
    fn clamp_e_entradas_invalidas() {
        let v = Rect::new(10.0, 20.0, 60.0, 30.0);
        let r = place(v, (500.0, 200.0), &Anchor::default(), Margins::default());
        assert_eq!((r.w, r.h), (60.0, 30.0), "{r:?}");
        assert!((r.x - v.x).abs() < 1e-4 && (r.y - v.y).abs() < 1e-4, "{r:?}");
        // Margem negativa/NaN não empurra a caixa para fora.
        let m = Margins { left: f32::NAN, right: -30.0, top: -1.0, bottom: f32::INFINITY };
        let r = place(v, (10.0, 10.0), &Anchor { h: HAlign::Left, v: VAlign::Top, x: None, y: None }, m);
        assert!((r.x - v.x).abs() < 1e-4 && (r.y - v.y).abs() < 1e-4, "{r:?}");
        let empty = place(Rect::new(0.0, 0.0, 0.0, 0.0), (10.0, 10.0), &Anchor::default(), Margins::default());
        assert_eq!((empty.w, empty.h), (0.0, 0.0));
    }
}
