//! Sessão do player: playlist, posição por faixa (retomada EXPLÍCITA — o play
//! abre sempre do começo; a posição salva só alimenta o botão "Retomar") e
//! preferências (volume/mute). Tudo na pasta de config do app
//! (`%APPDATA%\local-player-rs` no Windows, `~/.config/local-player-rs` fora).
//!
//! Os três arquivos são derivados e minúsculos: falha de leitura/gravação não
//! derruba nada (volta ao default). O resume tem cap de 200 entradas — remover
//! a "mais antiga" é um `HashMap::keys().next()` porque a ordem exata não
//! importa (o banco é descartável).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Playlist da sessão.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Playlist {
    pub files: Vec<String>,
    /// Altura do painel `Itens` depois de um resize manual; `None` = altura do
    /// conteúdo (o painel acompanha a lista). A UI valida a faixa ao desenhar.
    #[serde(default)]
    pub panel_height: Option<f32>,
}

/// Posição por arquivo (caminho absoluto → segundos em que parou).
#[derive(Serialize, Deserialize, Default)]
pub struct Resume {
    #[serde(default)]
    pub positions: HashMap<String, f64>,
}

/// Preferências do player.
#[derive(Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default = "default_volume")]
    pub volume: f64,
    #[serde(default)]
    pub muted: bool,
}

fn default_volume() -> f64 {
    100.0
}

impl Default for Prefs {
    fn default() -> Self {
        Self { volume: default_volume(), muted: false }
    }
}

/// Cap do resume (LRU aproximado — o banco é derivado, não crítico).
const MAX_ENTRIES: usize = 200;

fn dir() -> std::path::PathBuf {
    lab_ui::config::config_dir("local-player-rs")
}

fn read_json<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    std::fs::read_to_string(dir().join(name)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn write_json<T: Serialize>(name: &str, value: &T) {
    let path = dir().join(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(value) {
        let _ = std::fs::write(path, json);
    }
}

pub fn load_playlist() -> Playlist {
    let mut pl: Playlist = read_json("playlist.json").unwrap_or_default();
    // json editado à mão: altura inválida volta para "automática".
    pl.panel_height = pl.panel_height.filter(|h| h.is_finite() && *h > 0.0);
    pl
}

pub fn save_playlist(pl: &Playlist) {
    write_json("playlist.json", pl);
}

pub fn load() -> Resume {
    read_json("resume.json").unwrap_or_default()
}

pub fn save(r: &Resume) {
    write_json("resume.json", r);
}

pub fn load_prefs() -> Prefs {
    let mut prefs: Prefs = read_json("player.json").unwrap_or_default();
    prefs.volume = prefs.volume.clamp(0.0, 100.0);
    prefs
}

pub fn save_prefs(p: &Prefs) {
    write_json("player.json", p);
}

/// Grava a posição de `media` com cap de entradas. Só MEMÓRIA — persistir é
/// com [`save`] (a UI decide quando tocar o disco; write a cada tick de
/// posição era stutter na thread da UI).
pub fn remember(r: &mut Resume, media: &str, secs: f64) {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    r.positions.insert(media.to_string(), secs);
    while r.positions.len() > MAX_ENTRIES {
        if let Some(oldest) = r.positions.keys().next().cloned() {
            r.positions.remove(&oldest);
        }
    }
}

pub fn position_of(r: &Resume, media: &str) -> Option<f64> {
    r.positions.get(media).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lembra_posicao_e_limita_o_banco() {
        let mut r = Resume::default();
        remember(&mut r, "/a.mkv", 12.5);
        assert_eq!(position_of(&r, "/a.mkv"), Some(12.5));
        // Entrada inválida vira 0, nunca NaN.
        remember(&mut r, "/nan.mkv", f64::NAN);
        assert_eq!(position_of(&r, "/nan.mkv"), Some(0.0));
        for i in 0..250 {
            remember(&mut r, &format!("/m{i}.mkv"), i as f64);
        }
        assert!(r.positions.len() <= MAX_ENTRIES);
    }

    #[test]
    fn prefs_tem_defaults() {
        let p = Prefs::default();
        assert_eq!(p.volume, 100.0);
        assert!(!p.muted);
    }
}
