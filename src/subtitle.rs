//! Legendas externas (SRT e o subconjunto simples de VTT).
//!
//! O arquivo é procurado ao lado do vídeo com o mesmo nome-base
//! (`filme.srt`, `filme.sub.srt`, `filme.pt-br.srt`, `filme.en.srt`, ...) e o
//! parser é tolerante: BOM UTF-8/UTF-16, CRLF/CR solto, índice numérico
//! opcional, timestamps com vírgula (SRT) ou ponto (VTT), tags `<i>/<b>/<u>` e
//! entradas quebradas (puladas, nunca abortam o arquivo).
//!
//! A seleção é pelo relógio do motor: quem chama passa o tempo de mídia e
//! recebe o texto da cue ativa. Posição/tamanho da legenda sobre o vídeo são
//! matemática pura ([`place_bottom`]), testável sem GPU.

use std::path::{Path, PathBuf};

use crate::video::geometry::Rect;

/// Uma entrada de legenda — tempos em segundos de mídia (`start < end`).
#[derive(Clone, Debug, PartialEq)]
struct Cue {
    start: f64,
    end: f64,
    text: String,
}

/// Legenda carregada de um arquivo externo.
pub struct Subtitles {
    /// Ordenadas por `start` (a busca para na primeira cue futura).
    cues: Vec<Cue>,
    /// Arquivo de origem (nome mostrado no hover do botão de legenda).
    pub path: String,
}

impl Subtitles {
    /// Lê e interpreta um `.srt`/`.vtt`. Entradas malformadas são puladas; erro
    /// só quando o arquivo não lê ou não sobra nenhuma entrada válida.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cues = parse(&decode(&bytes));
        if cues.is_empty() {
            return Err(format!("{}: nenhuma legenda válida", path.display()));
        }
        Ok(Self { cues, path: path.display().to_string() })
    }

    /// Texto da legenda ativa em `time` (segundos de mídia). Cues que se
    /// sobrepõem: vence a que começou por último.
    pub fn text_at(&self, time: f64) -> Option<&str> {
        if !time.is_finite() {
            return None;
        }
        let mut active = None;
        for cue in &self.cues {
            if cue.start > time {
                break;
            }
            if time < cue.end {
                active = Some(cue.text.as_str());
            }
        }
        active
    }
}

/// Primeiro arquivo de legenda ao lado de `media`, na ordem de preferência.
pub fn find_for(media: &Path) -> Option<PathBuf> {
    candidates(media).into_iter().find(|p| p.is_file())
}

/// Candidatos de legenda para um arquivo de vídeo, na ordem de preferência
/// (`filme.srt` — o SRT genérico ao lado do vídeo costuma ser a legenda
/// escolhida — antes de `filme.pt-br.srt` e variantes de idioma).
fn candidates(media: &Path) -> Vec<PathBuf> {
    let Some(stem) = media.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let dir = media.parent().unwrap_or(Path::new(""));
    let mut out = Vec::new();
    for tag in ["", ".sub", ".pt-br", ".pt", ".en", ".es"] {
        for ext in ["srt", "vtt"] {
            out.push(dir.join(format!("{stem}{tag}.{ext}")));
        }
    }
    out
}

/// Bytes do arquivo → texto: BOM UTF-8/UTF-16 (LE/BE); UTF-8 normal; sem
/// UTF-8 válido assume Latin-1 (SRT antigo gravado em cp1252/latin-1).
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

/// Parser tolerante de SRT (e VTT simples): blocos separados por linha em
/// branco, índice opcional e timestamp `HH:MM:SS,mmm` (ou `.mmm`).
fn parse(text: &str) -> Vec<Cue> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    // Blocos por linha em branco — inclusive linha "vazia" com espaços.
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in normalized.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !block.is_empty() {
                blocks.push(std::mem::take(&mut block));
            }
        } else {
            block.push(line);
        }
    }
    if !block.is_empty() {
        blocks.push(block);
    }

    let mut cues = Vec::new();
    for lines in blocks {
        let Some(header) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let Some((start, end)) = parse_range(lines[header]) else {
            continue;
        };
        // Timestamp invertido/instável não vira cue (a entrada é pulada).
        if !(start.is_finite() && end.is_finite()) || end <= start {
            continue;
        }
        let mut text_lines = Vec::new();
        for line in &lines[header + 1..] {
            let stripped = strip_tags(line);
            let stripped = stripped.trim();
            if !stripped.is_empty() {
                text_lines.push(stripped.to_owned());
            }
        }
        if text_lines.is_empty() {
            continue;
        }
        cues.push(Cue { start, end, text: text_lines.join("\n") });
    }
    // Ordem de exibição: a busca da UI pode parar na primeira cue futura.
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    cues
}

fn parse_range(line: &str) -> Option<(f64, f64)> {
    let (left, right) = line.split_once("-->")?;
    let start = parse_timestamp(left)?;
    // O fim pode vir seguido de configuração da cue (`align:start position:10%`).
    let end_token = right.split_whitespace().next()?;
    let end = parse_timestamp(end_token)?;
    Some((start, end))
}

/// `HH:MM:SS,mmm` / `HH:MM:SS.mmm` / `MM:SS,mmm` → segundos.
fn parse_timestamp(raw: &str) -> Option<f64> {
    let mut parts = raw.trim().split(':');
    let (hours, minutes, seconds) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(m), Some(s), None) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?, s),
        (Some(m), Some(s), None, None) => (0, m.parse::<u32>().ok()?, s),
        _ => return None,
    };
    if minutes >= 60 {
        return None;
    }
    let seconds = seconds.replace(',', ".").parse::<f64>().ok()?;
    if !seconds.is_finite() || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some(f64::from(hours) * 3600.0 + f64::from(minutes) * 60.0 + seconds)
}

/// Remove as tags básicas de estilo (`<i>`, `<b>`, `<u>`, `<s>` e os fechamentos)
/// e decodifica as entidades comuns. Tag desconhecida fica literal.
fn strip_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        let Some(rel_close) = rest[open..].find('>') else {
            break;
        };
        let close = open + rel_close;
        out.push_str(&rest[..open]);
        let tag = rest[open + 1..close].trim();
        let name = tag.trim_start_matches('/').trim_end_matches('/').trim().to_ascii_lowercase();
        if !matches!(name.as_str(), "i" | "b" | "u" | "s") {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    decode_entities(&out)
}

fn decode_entities(text: &str) -> String {
    // `&amp;` por último: senão `&amp;lt;` viraria `<` (dupla decodificação).
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&nbsp;", " ").replace("&amp;", "&")
}

/// Caixa da legenda sobre a imagem: centralizada na horizontal e com `margin`
/// acima da base do vídeo. Dimensões inválidas não explodem (caixa vazia).
pub fn place_bottom(video: Rect, size: (f32, f32), margin: f32) -> Rect {
    if !video.w.is_finite() || !video.h.is_finite() || video.w <= 0.0 || video.h <= 0.0 {
        return Rect::new(video.x, video.y, 0.0, 0.0);
    }
    let margin = if margin.is_finite() { margin.max(0.0) } else { 0.0 };
    let w = if size.0.is_finite() { size.0.clamp(0.0, video.w) } else { 0.0 };
    let h = if size.1.is_finite() { size.1.clamp(0.0, video.h) } else { 0.0 };
    let x = video.x + (video.w - w) * 0.5;
    // Vídeo mais baixo que a legenda + margem: encosta no topo em vez de sair.
    let y = (video.y + video.h - margin - h).max(video.y);
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_basico() {
        let src = "1\n00:00:01,000 --> 00:00:03,500\nOlá\n\n2\n00:00:04,000 --> 00:00:06,000\nSegunda\nlinha\n";
        let cues = parse(src);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], Cue { start: 1.0, end: 3.5, text: "Olá".into() });
        assert_eq!(cues[1].text, "Segunda\nlinha");
    }

    /// BOM UTF-8, CRLF, bloco sem índice e timestamp com ponto (VTT).
    #[test]
    fn bom_crlf_e_timestamp_com_ponto() {
        let src = "\u{feff}00:00:00,500 --> 00:00:01,000\r\nSem índice\r\n\r\n2\r\n00:00:02.000 --> 00:00:03.250\r\nPonto\r\n";
        let cues = parse(src);
        assert_eq!(cues.len(), 2);
        assert!((cues[0].start - 0.5).abs() < 1e-9, "{cues:?}");
        assert!((cues[1].start - 2.0).abs() < 1e-9, "{cues:?}");
        assert!((cues[1].end - 3.25).abs() < 1e-9, "{cues:?}");
    }

    #[test]
    fn tags_e_entidades_saem() {
        let src = "1\n00:00:01,000 --> 00:00:02,000\n<i>itálico</i> e <b>negrito</b> &amp; <u>sublinhado</u>\n";
        let cues = parse(src);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "itálico e negrito & sublinhado");
    }

    /// Entrada sem timestamp, timestamp ilegível, invertido e texto vazio são
    /// pulados sem derrubar o resto do arquivo.
    #[test]
    fn entradas_quebradas_sao_puladas() {
        let src = concat!(
            "sem timestamp\n\n",
            "1\n00:00:05,000 --> 00:00:04,000\ninvertida\n\n",
            "2\nxx --> yy\ntempo ruim\n\n",
            "3\n00:00:03,000 --> 00:00:04,000\n\n",
            "4\n00:00:07,000 --> 00:00:08,000\nboa\n",
        );
        let cues = parse(src);
        assert_eq!(cues.len(), 1, "{cues:?}");
        assert_eq!(cues[0].text, "boa");
    }

    #[test]
    fn timestamp_aceita_duas_partes_e_rejeita_lixo() {
        assert!((parse_timestamp("01:02:03,004").unwrap() - 3723.004).abs() < 1e-9);
        assert!((parse_timestamp("02:03.500").unwrap() - 123.5).abs() < 1e-9);
        for bad in ["", "--", "00:00:60,000", "00:61:00,000", "a:b:c", "1:2:3:4"] {
            assert!(parse_timestamp(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn utf16_le_com_bom_decodifica() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "1\n00:00:01,000 --> 00:00:02,000\nção\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let cues = parse(&decode(&bytes));
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "ção");
    }

    /// Sem UTF-8 válido, o fallback Latin-1 mantém os acentos de SRT antigo.
    #[test]
    fn latim1_de_fallback() {
        assert_eq!(decode(&[0x4F, 0x6C, 0xE1]), "Olá");
    }

    /// VTT de verdade: cabeçalho WEBVTT (bloco sem timestamp) e configuração
    /// da cue depois do fim.
    #[test]
    fn vtt_simples() {
        let src = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000 align:start position:10%\nVTT ok\n";
        let cues = parse(src);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "VTT ok");
    }

    /// Linha "em branco" com espaços também fecha o bloco.
    #[test]
    fn linha_em_branco_com_espacos_separa_blocos() {
        let cues = parse("1\n00:00:01,000 --> 00:00:02,000\nA\n \n2\n00:00:03,000 --> 00:00:04,000\nB\n");
        assert_eq!(cues.len(), 2, "{cues:?}");
        assert_eq!(cues[1].text, "B");
    }

    #[test]
    fn texto_ativo_pelo_relogio() {
        let cues = parse("1\n00:00:01,000 --> 00:00:02,000\nA\n\n2\n00:00:02,000 --> 00:00:03,000\nB\n");
        let subs = Subtitles { cues, path: String::new() };
        assert_eq!(subs.text_at(0.5), None);
        assert_eq!(subs.text_at(1.0), Some("A"));
        assert_eq!(subs.text_at(1.999), Some("A"));
        assert_eq!(subs.text_at(2.0), Some("B"), "fim exclusivo/começo do próximo");
        assert_eq!(subs.text_at(3.0), None);
        assert_eq!(subs.text_at(f64::NAN), None);
        assert_eq!(subs.text_at(f64::INFINITY), None);
    }

    /// Cue sobreposta: vence a que começou por último.
    #[test]
    fn cue_sobreposta_mais_recente_vence() {
        let cues = parse("1\n00:00:01,000 --> 00:00:05,000\nlonga\n\n2\n00:00:02,000 --> 00:00:03,000\ncurta\n");
        let subs = Subtitles { cues, path: String::new() };
        assert_eq!(subs.text_at(2.5), Some("curta"));
        assert_eq!(subs.text_at(4.0), Some("longa"));
    }

    #[test]
    fn candidatos_ordenados_por_preferencia() {
        let c = candidates(Path::new("videos/filme.mkv"));
        assert!(c[0].ends_with("filme.srt"), "{c:?}");
        assert!(c.iter().any(|p| p.ends_with("filme.pt-br.srt")), "{c:?}");
        assert!(c.iter().any(|p| p.ends_with("filme.en.vtt")), "{c:?}");
        assert!(candidates(Path::new("")).is_empty());
    }

    #[test]
    fn legenda_fica_centralizada_acima_da_base() {
        let video = Rect::new(100.0, 50.0, 800.0, 450.0);
        let r = place_bottom(video, (300.0, 40.0), 18.0);
        assert!((r.x - (100.0 + (800.0 - 300.0) * 0.5)).abs() < 1e-6, "{r:?}");
        assert!((r.y - (50.0 + 450.0 - 18.0 - 40.0)).abs() < 1e-6, "{r:?}");
        assert!(r.w == 300.0 && r.h == 40.0);
    }

    #[test]
    fn legenda_maior_que_o_video_e_clampada() {
        let video = Rect::new(10.0, 20.0, 60.0, 30.0);
        let r = place_bottom(video, (500.0, 200.0), 10.0);
        assert_eq!((r.w, r.h), (60.0, 30.0), "{r:?}");
        assert!((r.x - video.x).abs() < 1e-6 && (r.y - video.y).abs() < 1e-6, "{r:?}");
        // Retângulo inválido devolve caixa vazia sem pânico.
        let empty = place_bottom(Rect::new(0.0, 0.0, 0.0, 0.0), (10.0, 10.0), 2.0);
        assert_eq!((empty.w, empty.h), (0.0, 0.0));
    }
}
