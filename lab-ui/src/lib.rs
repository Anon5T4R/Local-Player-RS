//! lab-ui — esqueleto do "padrão egui" da suíte: tema (5 paletas nomeadas,
//! espelhando o padrao-apps), i18n PT/EN/ES e config JSON por app.
//! Piloto de referência; nada aqui vira padrão oficial sem decisão.

pub mod config;
pub mod i18n;
pub mod theme;
pub mod workarea;

use config::Config;
use i18n::{Key, Lang};
use theme::Appearance;

/// Desenha os combos de idioma + tema (canto de configurações comum aos apps).
/// Devolve `(config_changed, forget_quick_key_clicked)`.
pub fn settings_ui(ui: &mut egui::Ui, cfg: &mut Config, show_forget_quick_key: bool) -> (bool, bool) {
    let before = *cfg;
    let mut forget_clicked = false;

    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_label(i18n::t(cfg.lang, Key::Language)).selected_text(cfg.lang.label()).width(80.0).show_ui(ui, |ui| {
            for l in Lang::ALL {
                ui.selectable_value(&mut cfg.lang, l, l.label());
            }
        });

        egui::ComboBox::from_label(i18n::t(cfg.lang, Key::Theme)).selected_text(cfg.theme.label()).width(100.0).show_ui(ui, |ui| {
            for th in Appearance::ALL {
                ui.selectable_value(&mut cfg.theme, th, th.label());
            }
        });

        if show_forget_quick_key && ui.button(i18n::t(cfg.lang, Key::ForgetKey)).clicked() {
            forget_clicked = true;
        }
    });

    (*cfg != before, forget_clicked)
}
