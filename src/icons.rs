//! Ícones da barra de controles, desenhados com o painter do egui.
//!
//! Por que não glyphs de fonte: a UI não liga `monochrome_emoji_fonts`, e
//! nenhuma das fontes embutidas (Ubuntu-Light + o subconjunto `egui-icons`)
//! cobre ⏮ ▶ ⏸ ⏹ ⏭ 🔇 🔊 ⛶ ⟳ — todos caíam no MESMO glifo de "falta" (os quatro
//! botões idênticos do print). Geometria desenhada resolve de vez e independe
//! de fonte; as cores saem sempre do tema (`ui.visuals()`), no mesmo desenho
//! do `IconButton` do craft-ui: estados vêm das `WidgetVisuals`, seleção usa a
//! cor de seleção do tema, nada de cor cravada aqui.

use egui::{Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

/// Ícones usados na barra de controles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Previous,
    Play,
    Pause,
    Stop,
    Next,
    Volume,
    Muted,
    Fullscreen,
    ExitFullscreen,
    Rotate,
    Subtitles,
    Warning,
}

/// Tamanho dos botões de ícone (mesma altura útil do `interact_size` do tema).
pub const BUTTON: Vec2 = Vec2::new(28.0, 24.0);

/// Botão só de ícone. `label` é o nome acessível (independente do desenho);
/// `selected` usa a cor de seleção do tema; `enabled = false` fica flat e
/// apagado, sem clique.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, selected: bool, enabled: bool) -> Response {
    let enabled = enabled && ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(BUTTON, if enabled { Sense::click() } else { Sense::hover() });
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, label));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let visuals = ui.visuals();
    let widget = visuals.widgets.state(response.widget_state());
    let (fill, stroke, tint) = if !enabled {
        (Color32::TRANSPARENT, Stroke::NONE, visuals.weak_text_color())
    } else if selected {
        (visuals.selection.bg_fill, visuals.selection.stroke, visuals.strong_text_color())
    } else {
        (widget.weak_bg_fill, widget.bg_stroke, widget.text_color())
    };
    let painter = ui.painter();
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, widget.corner_radius, fill);
    }
    if stroke.width > 0.0 {
        painter.rect_stroke(rect, widget.corner_radius, stroke, StrokeKind::Inside);
    }
    paint(ui, rect, icon, tint);
    if response.has_focus() && rect.width() > 8.0 {
        ui.painter().rect_stroke(rect.shrink(2.0), widget.corner_radius, visuals.selection.stroke, StrokeKind::Inside);
    }
    response
}

/// Desenha `icon` centrado em `rect`, tingido de `tint`.
pub fn paint(ui: &Ui, rect: Rect, icon: Icon, tint: Color32) {
    let painter = ui.painter();
    let c = rect.center();
    // Meia altura do glifo (o glifo inteiro tem 2s): sobra ~4,5 pts de folga
    // em cima/embaixo num botão de 24 pts.
    let s = (rect.height().min(rect.width()) * 0.5 * 0.62).max(2.0);
    let at = |x: f32, y: f32| c + Vec2::new(x * s, y * s);
    let stroke = Stroke::new((s * 0.2).clamp(1.1, 1.8), tint);

    match icon {
        Icon::Play => {
            painter.add(Shape::convex_polygon(vec![at(-0.62, -1.0), at(-0.62, 1.0), at(0.9, 0.0)], tint, Stroke::NONE));
        }
        Icon::Pause => {
            for x in [-0.36, 0.36] {
                painter.rect_filled(Rect::from_center_size(at(x, 0.0), Vec2::new(0.34 * s, 2.0 * s)), CornerRadius::same(1), tint);
            }
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(1.5 * s)), CornerRadius::same(1), tint);
        }
        Icon::Previous => {
            painter.rect_filled(Rect::from_center_size(at(-0.8, 0.0), Vec2::new(0.26 * s, 2.0 * s)), CornerRadius::same(1), tint);
            painter.add(Shape::convex_polygon(vec![at(0.8, -1.0), at(0.8, 1.0), at(-0.55, 0.0)], tint, Stroke::NONE));
        }
        Icon::Next => {
            painter.rect_filled(Rect::from_center_size(at(0.8, 0.0), Vec2::new(0.26 * s, 2.0 * s)), CornerRadius::same(1), tint);
            painter.add(Shape::convex_polygon(vec![at(-0.8, -1.0), at(-0.8, 1.0), at(0.55, 0.0)], tint, Stroke::NONE));
        }
        Icon::Volume | Icon::Muted => {
            // Boca do alto-falante + cone (duas peças convexas, sem polígono côncavo).
            painter.rect_filled(Rect::from_center_size(at(-0.62, 0.0), Vec2::new(0.5 * s, 0.84 * s)), CornerRadius::same(1), tint);
            painter.add(Shape::convex_polygon(vec![at(-0.38, -0.42), at(0.1, -1.0), at(0.1, 1.0), at(-0.38, 0.42)], tint, Stroke::NONE));
            let waves = at(0.15, 0.0);
            if icon == Icon::Muted {
                let r = 0.34 * s;
                let x = at(0.6, 0.0);
                painter.line_segment([x + Vec2::new(-r, -r), x + Vec2::new(r, r)], stroke);
                painter.line_segment([x + Vec2::new(-r, r), x + Vec2::new(r, -r)], stroke);
            } else {
                for radius in [0.5, 0.85] {
                    painter.add(Shape::line(arc(waves, radius * s, -50.0, 50.0, 10), stroke));
                }
            }
        }
        Icon::Fullscreen => brackets(painter, c, s, false, stroke),
        Icon::ExitFullscreen => brackets(painter, c, s, true, stroke),
        Icon::Rotate => {
            let radius = 0.8 * s;
            painter.add(Shape::line(arc(c, radius, -60.0, 240.0, 20), stroke));
            // Seta na ponta do arco: o ícone "gira" no sentido anti-horário.
            let angle = (-60.0f32).to_radians();
            let tip = c + Vec2::new(angle.cos(), angle.sin()) * radius;
            let dir = Vec2::new(angle.sin(), -angle.cos());
            painter.add(arrow_head(tip, dir, 0.62 * s, tint));
        }
        Icon::Subtitles => {
            let box_rect = Rect::from_center_size(c, Vec2::new(1.9 * s, 1.3 * s));
            painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, StrokeKind::Inside);
            let y = box_rect.center().y + 0.24 * s;
            painter.line_segment([egui::pos2(box_rect.left() + 0.3 * s, y), egui::pos2(box_rect.left() + 0.85 * s, y)], stroke);
            painter.line_segment([egui::pos2(box_rect.left() + 1.0 * s, y), egui::pos2(box_rect.right() - 0.3 * s, y)], stroke);
        }
        Icon::Warning => {
            let points = vec![at(0.0, -1.0), at(1.0, 0.85), at(-1.0, 0.85)];
            painter.add(Shape::closed_line(points, stroke));
            painter.line_segment([at(0.0, -0.4), at(0.0, 0.25)], stroke);
            painter.circle_filled(at(0.0, 0.62), (0.12 * s).max(0.8), tint);
        }
    }
}

/// Quatro cantos de "tela cheia" (cantos na borda, braços para dentro) ou de
/// "sair da tela cheia" (cantos de dentro, braços para a borda).
fn brackets(painter: &egui::Painter, center: Pos2, s: f32, inward: bool, stroke: Stroke) {
    let half = 0.85 * s;
    let arm = 0.45 * s;
    let (left, right) = (center.x - half, center.x + half);
    let (top, bottom) = (center.y - half, center.y + half);
    // (canto, direção horizontal do braço, direção vertical do braço)
    let corners = if inward {
        [
            (egui::pos2(left + arm, top + arm), -1.0, -1.0),
            (egui::pos2(right - arm, top + arm), 1.0, -1.0),
            (egui::pos2(left + arm, bottom - arm), -1.0, 1.0),
            (egui::pos2(right - arm, bottom - arm), 1.0, 1.0),
        ]
    } else {
        [
            (egui::pos2(left, top), 1.0, 1.0),
            (egui::pos2(right, top), -1.0, 1.0),
            (egui::pos2(left, bottom), 1.0, -1.0),
            (egui::pos2(right, bottom), -1.0, -1.0),
        ]
    };
    for (corner, dx, dy) in corners {
        painter.line_segment([corner, corner + Vec2::new(dx * arm, 0.0)], stroke);
        painter.line_segment([corner, corner + Vec2::new(0.0, dy * arm)], stroke);
    }
}

/// Pontos de um arco em graus (0° = direita, y para baixo como no egui).
fn arc(center: Pos2, radius: f32, from_deg: f32, to_deg: f32, steps: usize) -> Vec<Pos2> {
    let steps = steps.max(1);
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let angle = (from_deg + (to_deg - from_deg) * t).to_radians();
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

/// Triângulo de ponta de seta em `tip`, apontando para `dir`.
fn arrow_head(tip: Pos2, dir: Vec2, size: f32, color: Color32) -> Shape {
    let len = dir.length();
    let unit = if len > f32::EPSILON { dir / len } else { Vec2::new(1.0, 0.0) };
    let normal = Vec2::new(-unit.y, unit.x);
    let base = tip - unit * size;
    Shape::convex_polygon(vec![tip, base + normal * (size * 0.55), base - normal * (size * 0.55)], color, Stroke::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Todo ícone desenha alguma coisa e nenhum par de ícones de transporte
    /// sai com o mesmo conjunto de formas (o bug do print: quatro botões
    /// idênticos por glifo de fonte ausente).
    #[test]
    fn cada_icone_desenha_formas_proprias() {
        let ctx = egui::Context::default();
        let signatures: Vec<(Icon, String)> = [
            Icon::Previous,
            Icon::Play,
            Icon::Pause,
            Icon::Stop,
            Icon::Next,
            Icon::Volume,
            Icon::Muted,
            Icon::Fullscreen,
            Icon::ExitFullscreen,
            Icon::Rotate,
            Icon::Subtitles,
            Icon::Warning,
        ]
        .into_iter()
        .map(|icon| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint(ui, Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(24.0)), icon, Color32::WHITE);
            });
            // O fork cobra o consumidor das texturas do frame (mesma regra dos
            // testes do egui).
            output.textures_delta.clear();
            let signature = format!("{:?}", output.shapes.iter().map(|s| format!("{:?}", s.shape)).collect::<Vec<_>>());
            (icon, signature)
        })
        .collect();
        for (icon, signature) in &signatures {
            assert_ne!(signature, "[]", "{icon:?} não pintou nada");
        }
        for (i, (a, sig_a)) in signatures.iter().enumerate() {
            for (b, sig_b) in signatures.iter().skip(i + 1) {
                assert_ne!(sig_a, sig_b, "{a:?} e {b:?} desenham o mesmo ícone");
            }
        }
    }

    /// Desabilitado não recebe clique (o `Sense` é só hover) e o botão ocupa o
    /// tamanho padrão da barra.
    #[test]
    fn botao_desabilitado_nao_clica() {
        let ctx = egui::Context::default();
        let mut enabled = None;
        let mut disabled = None;
        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            enabled = Some(icon_button(ui, Icon::Play, "Play", false, true));
        });
        first.textures_delta.clear();
        let mut second = ctx.run_ui(egui::RawInput::default(), |ui| {
            disabled = Some(icon_button(ui, Icon::Play, "Play", false, false));
        });
        second.textures_delta.clear();
        let enabled = enabled.expect("botão habilitado responde");
        let disabled = disabled.expect("botão desabilitado responde");
        assert!(enabled.sense.senses_click(), "habilitado aceita clique");
        assert!(!disabled.sense.senses_click(), "desabilitado não aceita clique");
        assert_eq!(enabled.rect.size(), BUTTON);
    }
}
