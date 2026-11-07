//! Legendas externas: SRT, WebVTT (com cue settings) e ASS/SSA (estilo básico).
//!
//! O arquivo é procurado ao lado do vídeo com o mesmo nome-base (`filme.srt`,
//! `filme.pt-br.ass`, `filme.en.vtt`, ...) — a lista de candidatos e a ordem de
//! preferência ficam em [`candidates`]. O parser de cada formato é tolerante:
//! BOM UTF-8/UTF-16, CRLF/CR solto, marcação desconhecida e entradas quebradas
//! (puladas, nunca abortam o arquivo).
//!
//! A seleção é pelo relógio do motor: quem chama passa o tempo de mídia e
//! recebe a cue ativa ([`Subtitles::cue_at`]). Posição/tamanho da legenda sobre
//! o vídeo são matemática pura ([`layout::place`]), testável sem GPU; o estilo
//! (fonte, cores, alinhamento, margens) já sai resolvido dos parsers.
//!
//! Limitações do ASS (posicionamento absoluto, karaokê, desenho, `\t`...) estão
//! documentadas em [`ass`].

mod ass;
mod layout;
mod srt;

use std::path::{Path, PathBuf};

pub use layout::{place, Margins};

/// Alinhamento horizontal da cue dentro da área do vídeo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Alinhamento vertical da cue dentro da área do vídeo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// Onde a cue é ancorada. `x`/`y` só existem quando o arquivo pede uma posição
/// explícita (`position:`/`line:` em % no WebVTT); frações de 0 a 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub h: HAlign,
    pub v: VAlign,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

impl Default for Anchor {
    fn default() -> Self {
        Self { h: HAlign::Center, v: VAlign::Bottom, x: None, y: None }
    }
}

/// Família de fonte: a egui só tem estas duas (o ASS pede pelo nome e a gente
/// classifica por ele).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Proportional,
    Monospace,
}

/// Como o texto se destaca do vídeo: caixa semitransparente (SRT/VTT) ou
/// contorno (ASS com `BorderStyle` 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    Box,
    Outline,
}

/// Estilo efetivo de uma cue (no ASS, já resolvido contra o `Style` do script).
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Tamanho da fonte como fração da altura da imagem.
    pub size: f32,
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    /// Cor do texto (RGB).
    pub color: [u8; 3],
    /// Cor do contorno (ASS `OutlineColour`/`BackColour`).
    pub outline: [u8; 3],
    pub backdrop: Backdrop,
    /// Margens como fração da largura (l/r) e da altura (v) da imagem.
    pub margin_l: f32,
    pub margin_r: f32,
    pub margin_v: f32,
}

impl Default for Style {
    /// Aquilo que um SRT/VTT sem estilo declara: texto claro, contorno escuro,
    /// caixa semitransparente e margens proporcionais à imagem.
    fn default() -> Self {
        Self {
            size: 0.05,
            family: Family::Proportional,
            bold: false,
            italic: false,
            color: [255, 255, 255],
            outline: [0, 0, 0],
            backdrop: Backdrop::Box,
            margin_l: 0.02,
            margin_r: 0.02,
            margin_v: 0.035,
        }
    }
}

/// Um trecho de texto com estilo próprio (o ASS muda negrito/itálico/cor no
/// meio da linha; SRT e VTT produzem um segmento só).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Cor só deste trecho (`\c&H...&`); `None` = a cor do estilo.
    pub color: Option<[u8; 3]>,
}

/// Uma entrada de legenda — tempos em segundos de mídia (`start < end`).
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub segments: Vec<Segment>,
    pub style: Style,
    pub anchor: Anchor,
}

/// Legenda carregada de um arquivo externo.
pub struct Subtitles {
    /// Ordenadas por `start` (a busca para na primeira cue futura).
    cues: Vec<Cue>,
    /// Arquivo de origem (nome mostrado no hover do botão de legenda).
    pub path: String,
}

impl Subtitles {
    /// Lê e interpreta um `.srt`/`.vtt`/`.ass`/`.ssa`. Entradas malformadas são
    /// puladas; erro só quando o arquivo não lê ou não sobra nenhuma entrada
    /// válida.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = decode(&bytes);
        let parsed = match format_of(path, &text) {
            Format::Ass => ass::parse(&text),
            Format::Vtt => srt::parse(&text, true),
            Format::Srt => srt::parse(&text, false),
        };
        for warning in &parsed.warnings {
            eprintln!("engine: legenda: {}: {warning}", path.display());
        }
        if parsed.cues.is_empty() {
            return Err(format!("{}: nenhuma legenda válida", path.display()));
        }
        Ok(Self { cues: parsed.cues, path: path.display().to_string() })
    }

    /// Cue ativa em `time` (segundos de mídia). Cues que se sobrepõem: vence a
    /// que começou por último.
    pub fn cue_at(&self, time: f64) -> Option<&Cue> {
        if !time.is_finite() {
            return None;
        }
        let mut active = None;
        for cue in &self.cues {
            if cue.start > time {
                break;
            }
            if time < cue.end {
                active = Some(cue);
            }
        }
        active
    }
}

/// Formato efetivo do arquivo: a extensão manda; sem extensão conhecida, um
/// sniff no conteúdo (VTT tem cabeçalho, ASS tem seções).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Srt,
    Vtt,
    Ass,
}

fn format_of(path: &Path, text: &str) -> Format {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "ass" | "ssa" => Format::Ass,
        "vtt" => Format::Vtt,
        "srt" | "sub" => Format::Srt,
        _ => {
            let head = text.trim_start_matches('\u{feff}').trim_start();
            if head.starts_with("WEBVTT") {
                Format::Vtt
            } else if text.contains("[Script Info]") || text.contains("[Events]") || text.lines().any(|l| l.trim_start().starts_with("Dialogue:")) {
                Format::Ass
            } else {
                Format::Srt
            }
        }
    }
}

/// Primeiro arquivo de legenda ao lado de `media`, na ordem de preferência.
pub fn find_for(media: &Path) -> Option<PathBuf> {
    candidates(media).into_iter().find(|p| p.is_file())
}

/// Candidatos de legenda para um arquivo de vídeo, na ordem de preferência:
/// `filme.srt` — o SRT genérico ao lado do vídeo costuma ser a legenda
/// escolhida — antes de `filme.vtt`, `.ass`/`.ssa` e das variantes de idioma.
fn candidates(media: &Path) -> Vec<PathBuf> {
    let Some(stem) = media.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let dir = media.parent().unwrap_or(Path::new(""));
    let mut out = Vec::new();
    for tag in ["", ".sub", ".pt-br", ".pt", ".en", ".es"] {
        for ext in ["srt", "vtt", "ass", "ssa"] {
            out.push(dir.join(format!("{stem}{tag}.{ext}")));
        }
    }
    out
}

/// Bytes do arquivo → texto: BOM UTF-8/UTF-16 (LE/BE); UTF-8 normal; sem
/// UTF-8 válido assume Latin-1 (legenda antiga gravada em cp1252/latin-1).
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        return decode_utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        return decode_utf16(rest, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().copied().map(char::from).collect(),
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> =
        bytes.as_chunks::<2>().0.iter().map(|pair| if little_endian { u16::from_le_bytes(*pair) } else { u16::from_be_bytes(*pair) }).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cues_of(src: &str, vtt: bool) -> Vec<Cue> {
        srt::parse(src, vtt).cues
    }

    #[test]
    fn utf16_le_com_bom_decodifica() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "1\n00:00:01,000 --> 00:00:02,000\nção\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let cues = cues_of(&decode(&bytes), false);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].segments[0].text, "ção");
    }

    /// Sem UTF-8 válido, o fallback Latin-1 mantém os acentos de SRT antigo.
    #[test]
    fn latim1_de_fallback() {
        assert_eq!(decode(&[0x4F, 0x6C, 0xE1]), "Olá");
    }

    #[test]
    fn texto_ativo_pelo_relogio() {
        let cues = cues_of("1\n00:00:01,000 --> 00:00:02,000\nA\n\n2\n00:00:02,000 --> 00:00:03,000\nB\n", false);
        let subs = Subtitles { cues, path: String::new() };
        assert_eq!(subs.cue_at(0.5), None);
        assert_eq!(subs.cue_at(1.0).map(|c| c.segments[0].text.as_str()), Some("A"));
        assert_eq!(subs.cue_at(1.999).map(|c| c.segments[0].text.as_str()), Some("A"));
        assert_eq!(subs.cue_at(2.0).map(|c| c.segments[0].text.as_str()), Some("B"), "fim exclusivo/começo do próximo");
        assert_eq!(subs.cue_at(3.0), None);
        assert_eq!(subs.cue_at(f64::NAN), None);
        assert_eq!(subs.cue_at(f64::INFINITY), None);
    }

    /// Cue sobreposta: vence a que começou por último.
    #[test]
    fn cue_sobreposta_mais_recente_vence() {
        let cues = cues_of("1\n00:00:01,000 --> 00:00:05,000\nlonga\n\n2\n00:00:02,000 --> 00:00:03,000\ncurta\n", false);
        let subs = Subtitles { cues, path: String::new() };
        assert_eq!(subs.cue_at(2.5).map(|c| c.segments[0].text.as_str()), Some("curta"));
        assert_eq!(subs.cue_at(4.0).map(|c| c.segments[0].text.as_str()), Some("longa"));
    }

    #[test]
    fn candidatos_ordenados_por_preferencia() {
        let c = candidates(Path::new("videos/filme.mkv"));
        assert!(c[0].ends_with("filme.srt"), "{c:?}");
        assert!(c.iter().any(|p| p.ends_with("filme.pt-br.srt")), "{c:?}");
        assert!(c.iter().any(|p| p.ends_with("filme.en.vtt")), "{c:?}");
        // ASS/SSA entram na mesma lista, depois de SRT/VTT.
        let ass = c.iter().position(|p| p.ends_with("filme.ass")).expect("ass");
        let srt = c.iter().position(|p| p.ends_with("filme.srt")).expect("srt");
        assert!(srt < ass, "{c:?}");
        assert!(c.iter().any(|p| p.ends_with("filme.pt-br.ssa")), "{c:?}");
        assert!(candidates(Path::new("")).is_empty());
    }

    /// A extensão vence; sem extensão conhecida o conteúdo decide.
    #[test]
    fn formato_pelo_nome_ou_pelo_conteudo() {
        assert_eq!(format_of(Path::new("a.ass"), "irrelevante"), Format::Ass);
        assert_eq!(format_of(Path::new("a.SSA"), ""), Format::Ass);
        assert_eq!(format_of(Path::new("a.vtt"), ""), Format::Vtt);
        assert_eq!(format_of(Path::new("a.srt"), "[Script Info]"), Format::Srt);
        assert_eq!(format_of(Path::new("a.txt"), "WEBVTT\n\n00:00.000 --> 00:01.000\nx\n"), Format::Vtt);
        assert_eq!(format_of(Path::new("a.txt"), "[Script Info]\n[Events]\n"), Format::Ass);
        assert_eq!(format_of(Path::new("a.txt"), "1\n00:00:01,000 --> 00:00:02,000\nx\n"), Format::Srt);
    }

    /// `Subtitles::load` de ponta a ponta: escolhe o parser pela extensão, avisa
    /// o que ignorou e erra só quando não sobra cue.
    #[test]
    fn load_escolhe_o_parser_e_avisa() {
        let dir = std::env::temp_dir().join(format!("local-player-rs-subs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ass = dir.join("filme.ass");
        std::fs::write(
            &ass,
            concat!(
                "[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n",
                "[Events]\n",
                "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
                "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an8}olá\n",
            ),
        )
        .expect("write ass");
        let subs = Subtitles::load(&ass).expect("ass válido");
        assert_eq!(subs.cue_at(1.5).map(|c| c.segments[0].text.as_str()), Some("olá"));
        assert!((subs.cue_at(1.5).map(|c| c.style.size).unwrap_or(0.0) - 20.0 / 1080.0).abs() < 1e-6);

        let vtt = dir.join("filme.vtt");
        std::fs::write(&vtt, "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nvtt ok\n").expect("write vtt");
        assert_eq!(Subtitles::load(&vtt).expect("vtt válido").cue_at(1.5).map(|c| c.segments[0].text.as_str()), Some("vtt ok"));

        let empty = dir.join("vazio.srt");
        std::fs::write(&empty, "sem nada aqui\n").expect("write vazio");
        assert!(Subtitles::load(&empty).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
