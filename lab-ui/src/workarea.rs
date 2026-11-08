//! Área útil do monitor e encaixe de janela — extraído dos mains do
//! lab-image/lab-player (era o mesmo ~90 linhas duplicado nos dois).
//!
//! `fit_to_work_area` é o caminho único pra "janela do tamanho do
//! conteúdo": px → points, clamp na área útil (taskbar descontada,
//! auto-hide inclusa) e centralização contando o chrome (outer−inner do
//! viewport) pro OUTER inteiro caber.

/// Área útil do monitor em POINTS (tamanho, origem) — o espaço que a
/// taskbar NÃO come. Windows: `SPI_GETWORKAREA` ∪ strip docked da taskbar
/// auto-hide (escondida, o SPI devolve a tela inteira — mas o espaço que
/// ela ocupa ao aparecer é real). Outros: 90% do monitor.
pub fn work_area(ctx: &egui::Context) -> (egui::Vec2, egui::Pos2) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::Shell::{SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETSTATE, ABM_GETTASKBARPOS, APPBARDATA};
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};

        let ppp = ctx.pixels_per_point();
        let monitor = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0)) * ppp; // px

        let mut wa = RECT::default();
        let ok = unsafe {
            SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut RECT as *mut core::ffi::c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
                .is_ok()
        };
        if !ok {
            return (egui::vec2(monitor.x / ppp * 0.9, monitor.y / ppp * 0.9), egui::pos2(0.0, 0.0));
        }

        unsafe {
            let tray = FindWindowW(
                windows::core::PCWSTR::from_raw("Shell_TrayWnd\0".encode_utf16().collect::<Vec<_>>().as_ptr()),
                windows::core::PCWSTR::null(),
            );
            if let Ok(tray) = tray {
                let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, hWnd: tray, ..Default::default() };
                let state = SHAppBarMessage(ABM_GETSTATE, &mut abd);
                if state & 1 != 0 && SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) != 0 {
                    // ABS_AUTOHIDE: desconta o strip docked da taskbar.
                    let strip = &abd.rc;
                    let (w, h) = (monitor.x as i32, monitor.y as i32);
                    let eff = match abd.uEdge {
                        e if e == ABE_LEFT => RECT { left: strip.right, top: 0, right: w, bottom: h },
                        e if e == ABE_TOP => RECT { left: 0, top: strip.bottom, right: w, bottom: h },
                        e if e == ABE_RIGHT => RECT { left: 0, top: 0, right: strip.left, bottom: h },
                        e if e == ABE_BOTTOM => RECT { left: 0, top: 0, right: w, bottom: strip.top },
                        _ => wa,
                    };
                    return (
                        egui::vec2((eff.right - eff.left) as f32 / ppp, (eff.bottom - eff.top) as f32 / ppp),
                        egui::pos2(eff.left as f32 / ppp, eff.top as f32 / ppp),
                    );
                }
            }
        }

        (egui::vec2((wa.right - wa.left) as f32 / ppp, (wa.bottom - wa.top) as f32 / ppp), egui::pos2(wa.left as f32 / ppp, wa.top as f32 / ppp))
    }
    #[cfg(not(windows))]
    {
        let m = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(1920.0, 1080.0));
        (m * 0.9, egui::pos2(m.x * 0.05, m.y * 0.05))
    }
}

/// Tamanho INTERNO (points) para um conteúdo de `w_px`×`h_px` pixels: 1:1
/// (1 ponto = `ppp` pixels) quando cabe, encolhido uniformemente quando não
/// cabe — nunca maior que a área útil menos o chrome da janela. Math pura,
/// testável sem `Context` (o `fit_to_work_area` abaixo é só a leitura do
/// ambiente em volta dela).
pub fn fit_size(avail: egui::Vec2, window_chrome: egui::Vec2, w_px: f32, h_px: f32, ppp: f32) -> egui::Vec2 {
    fit_size_with_fixed_chrome(avail, window_chrome, w_px, h_px, 0.0, ppp)
}

/// Como [`fit_size`], mas `fixed_chrome_px` é o chrome INTERNO do app (barras
/// que não escalam junto com o conteúdo): ele sai da área útil ANTES do fator
/// de encolhimento, então a janela continua "abraçando" o conteúdo em 1:1 na
/// maior escala que cabe.
///
/// A ordem importa: encolher o chrome junto faria a janela ficar mais larga
/// que o conteúdo (letterbox de sobra) quando o cap vem da altura.
pub fn fit_size_with_fixed_chrome(avail: egui::Vec2, window_chrome: egui::Vec2, w_px: f32, h_px: f32, fixed_chrome_px: f32, ppp: f32) -> egui::Vec2 {
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    let w = (w_px / ppp).max(1.0);
    let h = (h_px / ppp).max(1.0);
    let fixed = if fixed_chrome_px.is_finite() { (fixed_chrome_px / ppp).max(0.0) } else { 0.0 };
    // Espaço que sobra para o conteúdo depois do chrome (nunca <= 0: com
    // chrome maior que a tela o resultado é o mínimo de 1 ponto, não negativo).
    let room = egui::vec2((avail.x - window_chrome.x).max(1.0), (avail.y - window_chrome.y).max(1.0));
    let s = (room.x / w).min((room.y - fixed).max(1.0) / h).min(1.0);
    egui::vec2((w * s).max(1.0), (h * s + fixed).max(1.0))
}

/// Converte o tamanho de conteúdo (px) em (`InnerSize`, `OuterPosition`)
/// de viewport: points com clamp na área útil e posição centrada. O chrome
/// da janela (bordas+title bar, outer−inner do viewport real) entra na
/// conta — o OUTER inteiro cabe, title bar não invade a taskbar.
pub fn fit_to_work_area(ctx: &egui::Context, w_px: f32, h_px: f32) -> (egui::Vec2, egui::Pos2) {
    fit_to_work_area_with_chrome(ctx, w_px, h_px, 0.0)
}

/// [`fit_to_work_area`] com o chrome INTERNO do app (topo/controles/lista) em
/// pixels: ele não escala com o conteúdo, então a janela abraça o vídeo em 1:1
/// na maior escala que cabe na tela.
pub fn fit_to_work_area_with_chrome(ctx: &egui::Context, w_px: f32, h_px: f32, fixed_chrome_px: f32) -> (egui::Vec2, egui::Pos2) {
    let ppp = ctx.pixels_per_point();
    let (avail, origin) = work_area(ctx);
    let (cx, cy) = ctx.input(|i| {
        let v = i.viewport();
        match (v.outer_rect, v.inner_rect) {
            (Some(o), Some(n)) => ((o.width() - n.width()).max(0.0), (o.height() - n.height()).max(0.0)),
            _ => (0.0, 40.0),
        }
    });
    let size = fit_size_with_fixed_chrome(avail, egui::vec2(cx, cy), w_px, h_px, fixed_chrome_px, ppp);
    let pos = egui::pos2(origin.x + (avail.x - (size.x + cx)) / 2.0, origin.y + (avail.y - (size.y + cy)) / 2.0);
    (size, pos)
}

#[cfg(test)]
mod tests {
    use super::fit_size;
    use egui::vec2;

    /// 1080p, 1080p de área útil: 1:1 exato, sem encolher.
    #[test]
    fn a_video_that_fits_keeps_its_pixels() {
        let size = fit_size(vec2(2560.0, 1400.0), vec2(0.0, 40.0), 1920.0, 1080.0, 1.0);
        assert_eq!(size, vec2(1920.0, 1080.0));
    }

    /// Escala do Windows a 150%: os pixels do vídeo continuam sendo pixels da
    /// TELA, então o tamanho em points divide por `pixels_per_point`.
    #[test]
    fn scaling_converts_pixels_to_points() {
        let size = fit_size(vec2(2560.0, 1400.0), vec2(0.0, 40.0), 1920.0, 1080.0, 1.5);
        assert_eq!(size, vec2(1280.0, 720.0));
    }

    /// Vídeo 4K num monitor 1080p: cai no maior que cabe (uniforme, nunca
    /// maior que a área útil) em vez de abrir uma janela fora da tela.
    #[test]
    fn a_video_larger_than_the_screen_is_capped() {
        let avail = vec2(1920.0, 1040.0);
        let chrome = vec2(0.0, 40.0);
        let size = fit_size(avail, chrome, 3840.0, 2160.0, 1.0);
        assert!(size.x <= avail.x, "{size:?}");
        assert!(size.y <= avail.y - chrome.y, "{size:?}");
        // Mesma proporção 16:9 do vídeo.
        assert!((size.x / size.y - 16.0 / 9.0).abs() < 0.01, "{size:?}");
        // E o maior possível: encostado na altura útil.
        assert!((size.y - (avail.y - chrome.y)).abs() < 0.5, "{size:?}");
    }

    /// A largura também limita (tela retrato / janela estreita).
    #[test]
    fn the_narrow_side_caps_the_size() {
        let size = fit_size(vec2(1000.0, 2000.0), vec2(0.0, 40.0), 1920.0, 1080.0, 1.0);
        assert!((size.x - 1000.0).abs() < 0.5, "{size:?}");
        assert!(size.y < 2000.0, "{size:?}");
    }

    /// Chrome maior que a própria área útil (taskbar gigante, tela minúscula):
    /// nunca negativo nem zero (o egui não aceita tamanho inválido).
    #[test]
    fn a_huge_chrome_never_yields_a_negative_size() {
        let size = fit_size(vec2(100.0, 100.0), vec2(200.0, 200.0), 1920.0, 1080.0, 1.0);
        assert!(size.x >= 1.0 && size.y >= 1.0, "{size:?}");
    }

    /// Conteúdo inválido (0 px, NaN ou ppp 0) não vira tamanho inválido.
    #[test]
    fn degenerate_input_is_kept_usable() {
        for (w, h, ppp) in [(0.0, 0.0, 1.0), (f32::NAN, f32::NAN, 1.0), (100.0, 100.0, 0.0)] {
            let size = fit_size(vec2(800.0, 600.0), vec2(0.0, 40.0), w, h, ppp);
            assert!(size.x.is_finite() && size.x >= 1.0, "{size:?}");
            assert!(size.y.is_finite() && size.y >= 1.0, "{size:?}");
        }
    }

    /// O chrome interno do app NÃO encolhe com o vídeo: num monitor 1080p um
    /// vídeo 1080p + 200 px de barras fica na maior escala que cabe, e a janela
    /// abraça o vídeo (sem letterbox de sobra nas laterais).
    #[test]
    fn the_app_chrome_is_not_scaled_away() {
        use super::fit_size_with_fixed_chrome;
        let chrome = 200.0;
        let avail = vec2(1920.0, 1080.0);
        let win_chrome = vec2(16.0, 40.0);
        let size = fit_size_with_fixed_chrome(avail, win_chrome, 1920.0, 1080.0, chrome, 1.0);
        // Vídeo + barras preenche exatamente a altura útil.
        assert!((size.y + win_chrome.y - avail.y).abs() < 0.5, "{size:?}");
        // A área do vídeo continua 16:9 (a janela não sobra nas laterais).
        let video_h = size.y - chrome;
        assert!((size.x / video_h - 1920.0 / 1080.0).abs() < 0.01, "{size:?}");
        // E a largura não passa do espaço lateral.
        assert!(size.x + win_chrome.x <= avail.x + 0.5, "{size:?}");
    }

    /// Sem cap (vídeo pequeno) o chrome só SOMA na altura.
    #[test]
    fn a_small_video_keeps_its_pixels_and_adds_the_chrome() {
        use super::fit_size_with_fixed_chrome;
        let size = fit_size_with_fixed_chrome(vec2(1920.0, 1080.0), vec2(16.0, 40.0), 800.0, 450.0, 120.0, 1.0);
        assert_eq!(size, vec2(800.0, 570.0));
    }

    /// Chrome interno maior que a própria tela: ainda devolve tamanho válido.
    #[test]
    fn a_chrome_bigger_than_the_screen_is_not_negative() {
        use super::fit_size_with_fixed_chrome;
        let size = fit_size_with_fixed_chrome(vec2(800.0, 600.0), vec2(0.0, 40.0), 1920.0, 1080.0, 900.0, 1.0);
        assert!(size.x >= 1.0 && size.y >= 1.0 && size.x.is_finite() && size.y.is_finite(), "{size:?}");
    }
}
