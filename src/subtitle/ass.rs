//! ASS/SSA (SubStation Alpha) — parser do formato com estilo básico.
//!
//! Coberto (o que 80% das legendas reais usam):
//!
//! - `[Script Info]`: `PlayResX`/`PlayResY` (escalam fonte e margens);
//! - `[V4+ Styles]` e `[V4 Styles]`: `Format:` real do arquivo + `Style:`
//!   (fonte, tamanho, negrito, itálico, cores, alinhamento, `BorderStyle`,
//!   margens);
//! - `[Events]`: `Format:` + `Dialogue:` (o texto é o último campo e pode ter
//!   vírgulas);
//! - overrides inline: `\an` (os nove alinhamentos), `\i`, `\b`, `\fs`,
//!   `\c`/`\1c`, `\r`; `\N` (quebra dura), `\n` (quebra) e `\h` (espaço duro).
//!
//! Fora do alcance — removido do texto, nunca aparece cru na tela:
//! `\pos`/`\move` (posicionamento absoluto; a cue segue o `\an` e as margens),
//! `\fad`/`\fade`, karaokê (`\k`, `\K`, `\kf`), desenho vetorial (`\p`),
//! animação (`\t`), escala/rotação (`\fscx`, `\fscy`, `\frz`), `\alpha`,
//! `\bord`/`\shad`, `\q` (wrap style), `\clip`. Também ficam fora: colisão de
//! linhas entre cues do libass, fontes embutidas e o `[Aegisub Project Garbage]`
//! (ignorado). Negrito é sintetizado (a egui só tem as fontes normal/negrito
//! registradas pelo tema): o renderer engrossa o texto desenhando-o duas vezes.

use std::collections::BTreeMap;

use super::srt::Parsed;
use super::{Anchor, Backdrop, Cue, Family, HAlign, Segment, Style, VAlign};

/// Resolução de referência do SSA quando o arquivo não declara `PlayRes`.
const DEFAULT_PLAY_RES: (f32, f32) = (384.0, 288.0);

/// Um `Style:` do script, nos valores originais (pixels do `PlayRes`).
#[derive(Clone, Debug)]
struct AssStyle {
    family: Family,
    size: f32,
    bold: bool,
    italic: bool,
    color: [u8; 3],
    outline: [u8; 3],
    backdrop: Backdrop,
    align: u8,
    margin_l: f32,
    margin_r: f32,
    margin_v: f32,
}

impl AssStyle {
    /// Estilo embutido, já convertido para fração do `PlayRes`.
    fn builtin(play_res: (f32, f32)) -> Self {
        let mut style = Self::default();
        style.size /= play_res.1;
        style.margin_l /= play_res.0;
        style.margin_r /= play_res.0;
        style.margin_v /= play_res.1;
        style
    }
}

impl Default for AssStyle {
    fn default() -> Self {
        Self {
            family: Family::Proportional,
            size: 20.0,
            bold: false,
            italic: false,
            color: [255, 255, 255],
            outline: [0, 0, 0],
            backdrop: Backdrop::Outline,
            align: 2,
            margin_l: 10.0,
            margin_r: 10.0,
            margin_v: 10.0,
        }
    }
}

/// Parser completo do script. Entradas quebradas são puladas; os avisos
/// (capítulos, overrides ignorados) vão para o chamador.
pub fn parse(text: &str) -> Parsed {
    let mut play_res = DEFAULT_PLAY_RES;
    let mut styles: BTreeMap<String, AssStyle> = BTreeMap::new();
    let mut cues = Vec::new();
    let mut warnings = Vec::new();
    let mut section = String::new();
    let mut style_columns: Vec<String> = Vec::new();
    let mut event_columns: Vec<String> = Vec::new();
    let mut saw_unsupported = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('!') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        match section.as_str() {
            "script info" => {
                if let Some((key, value)) = line.split_once(':') {
                    match key.trim().to_ascii_lowercase().as_str() {
                        "playresx" => {
                            if let Some(v) = parse_positive(value) {
                                play_res.0 = v;
                            }
                        }
                        "playresy" => {
                            if let Some(v) = parse_positive(value) {
                                play_res.1 = v;
                            }
                        }
                        _ => {}
                    }
                }
            }
            "v4+ styles" | "v4 styles" => {
                if let Some(rest) = line.strip_prefix("Format:") {
                    style_columns = split_columns(rest);
                } else if let Some(rest) = line.strip_prefix("Style:") {
                    // Sem `Format:` o arquivo é ilegal; o padrão do SSA serve.
                    if style_columns.is_empty() {
                        style_columns = default_style_columns();
                    }
                    if let Some(style) = parse_style(rest, &style_columns, play_res) {
                        styles.insert(style.0.to_ascii_lowercase(), style.1);
                    }
                }
            }
            "events" => {
                if let Some(rest) = line.strip_prefix("Format:") {
                    event_columns = split_columns(rest);
                } else if line.starts_with("Dialogue:") {
                    if event_columns.is_empty() {
                        event_columns = default_event_columns();
                    }
                    let rest = line.trim_start_matches("Dialogue:").trim_start();
                    if let Some((cue, used_pos)) = parse_dialogue(rest, &event_columns, &styles, play_res) {
                        saw_unsupported |= used_pos;
                        cues.push(cue);
                    }
                }
            }
            _ => {}
        }
    }

    if saw_unsupported {
        warnings.push("override \\pos/\\move ignorado (posicionamento absoluto fora do render básico)".into());
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    Parsed { cues, warnings }
}

fn default_style_columns() -> Vec<String> {
    [
        "name",
        "fontname",
        "fontsize",
        "primarycolour",
        "secondarycolour",
        "outlinecolour",
        "backcolour",
        "bold",
        "italic",
        "underline",
        "strikeout",
        "scalex",
        "scaley",
        "spacing",
        "angle",
        "borderstyle",
        "outline",
        "shadow",
        "alignment",
        "marginl",
        "marginr",
        "marginv",
        "encoding",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn default_event_columns() -> Vec<String> {
    ["layer", "start", "end", "style", "name", "marginl", "marginr", "marginv", "effect", "text"].iter().map(|s| (*s).to_string()).collect()
}

fn split_columns(raw: &str) -> Vec<String> {
    raw.split(',').map(|c| c.trim().to_ascii_lowercase()).collect()
}

/// `Style: Default,Arial,20,&H00FFFFFF,...` → (nome, estilo resolvido).
fn parse_style(raw: &str, columns: &[String], play_res: (f32, f32)) -> Option<(String, AssStyle)> {
    let fields: Vec<&str> = raw.split(',').map(str::trim).collect();
    let field = |name: &str| -> Option<&str> {
        let i = columns.iter().position(|c| c == name)?;
        fields.get(i).copied()
    };
    let name = field("name")?;
    if name.is_empty() {
        return None;
    }
    let default = AssStyle::default();
    let mut style = AssStyle {
        family: font_family(field("fontname").unwrap_or("")),
        size: field("fontsize").and_then(parse_positive).unwrap_or(default.size),
        bold: field("bold").is_some_and(flag),
        italic: field("italic").is_some_and(flag),
        color: field("primarycolour").and_then(parse_color).unwrap_or(default.color),
        outline: field("outlinecolour").and_then(parse_color).unwrap_or(default.outline),
        backdrop: match field("borderstyle") {
            Some("3") => Backdrop::Box,
            _ => Backdrop::Outline,
        },
        align: field("alignment").and_then(|v| v.trim().parse::<u8>().ok()).filter(|n| (1..=9).contains(n)).unwrap_or(default.align),
        margin_l: field("marginl").and_then(parse_positive).unwrap_or(default.margin_l),
        margin_r: field("marginr").and_then(parse_positive).unwrap_or(default.margin_r),
        margin_v: field("marginv").and_then(parse_positive).unwrap_or(default.margin_v),
    };
    // Fonte/margens em fração do PlayRes (o estilo final independe da resolução).
    style.size /= play_res.1;
    style.margin_l /= play_res.0;
    style.margin_r /= play_res.0;
    style.margin_v /= play_res.1;
    Some((name.to_owned(), style))
}

/// `0:00:01.50,0:00:04.00,Default,,0,0,0,,texto` → cue.
fn parse_dialogue(raw: &str, columns: &[String], styles: &BTreeMap<String, AssStyle>, play_res: (f32, f32)) -> Option<(Cue, bool)> {
    let text_index = columns.iter().position(|c| c == "text").unwrap_or(columns.len().saturating_sub(1));
    // O texto é o último campo e pode conter vírgulas: separa só até ele.
    let mut fields: Vec<&str> = raw.splitn(text_index + 1, ',').map(str::trim).collect();
    if fields.len() <= text_index {
        return None;
    }
    let text = fields.pop().unwrap_or("");
    let column = |name: &str| -> Option<&str> { fields.get(columns.iter().position(|c| c == name)?).copied() };
    let start = column("start").and_then(parse_time)?;
    let end = column("end").and_then(parse_time)?;
    if !(start.is_finite() && end.is_finite()) || end <= start || text.trim().is_empty() {
        return None;
    }
    let mut ass = match column("style").and_then(|s| styles.get(&s.to_ascii_lowercase())) {
        Some(style) => style.clone(),
        // Estilo desconhecido: o primeiro do script; script sem estilo: o padrão.
        None => styles.values().next().cloned().unwrap_or_else(|| AssStyle::builtin(play_res)),
    };
    // Margens do evento (0 = herda o estilo); em pixels do PlayRes.
    let margin = |name: &str| column(name).and_then(parse_positive).filter(|v| *v > 0.0);
    if let Some(v) = margin("marginl") {
        ass.margin_l = v / play_res.0;
    }
    if let Some(v) = margin("marginr") {
        ass.margin_r = v / play_res.0;
    }
    if let Some(v) = margin("marginv") {
        ass.margin_v = v / play_res.1;
    }

    let mut style = Style {
        size: ass.size,
        family: ass.family,
        bold: ass.bold,
        italic: ass.italic,
        color: ass.color,
        outline: ass.outline,
        backdrop: ass.backdrop,
        margin_l: ass.margin_l,
        margin_r: ass.margin_r,
        margin_v: ass.margin_v,
    };
    let mut anchor = anchor_of(ass.align);
    let mut unsupported = false;
    let segments = segments(text, &mut style, &mut anchor, play_res, &mut unsupported);
    if segments.is_empty() {
        return None;
    }
    Some((Cue { start, end, segments, style, anchor }, unsupported))
}

/// Alinhamento numpad do ASS → âncora (1 = baixo/esquerda ... 9 = topo/direita).
fn anchor_of(align: u8) -> Anchor {
    let n = align.clamp(1, 9);
    let h = match (n - 1) % 3 {
        0 => HAlign::Left,
        2 => HAlign::Right,
        _ => HAlign::Center,
    };
    let v = match (n - 1) / 3 {
        0 => VAlign::Bottom,
        1 => VAlign::Middle,
        _ => VAlign::Top,
    };
    Anchor { h, v, x: None, y: None }
}

/// Estado do texto corrente (o ASS muda estilo no meio da linha).
struct TextState {
    bold: bool,
    italic: bool,
    color: Option<[u8; 3]>,
}

/// Texto cru do `Dialogue` → segmentos, aplicando os overrides suportados.
fn segments(raw: &str, style: &mut Style, anchor: &mut Anchor, play_res: (f32, f32), unsupported: &mut bool) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut current = String::new();
    let mut state = TextState { bold: style.bold, italic: style.italic, color: None };
    let flush = |out: &mut Vec<Segment>, current: &mut String, state: &TextState| {
        if current.is_empty() {
            return;
        }
        out.push(Segment { text: std::mem::take(current), bold: state.bold, italic: state.italic, color: state.color });
    };

    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                flush(&mut out, &mut current, &state);
                let mut block = String::new();
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == '}' {
                        closed = true;
                        break;
                    }
                    block.push(c2);
                }
                if closed {
                    apply_overrides(&block, style, anchor, play_res, &mut state, unsupported);
                }
            }
            '\\' => match chars.next() {
                Some('N') | Some('n') => current.push('\n'),
                Some('h') => current.push('\u{a0}'),
                // Barra solta no texto: mantém como está.
                Some(other) => {
                    current.push('\\');
                    current.push(other);
                }
                None => current.push('\\'),
            },
            _ => current.push(c),
        }
    }
    flush(&mut out, &mut current, &state);
    out
}

/// Comandos de override reconhecidos, do mais longo para o mais curto (o nome
/// tem que casar antes do argumento: `\an7`, `\fs48`, `\1c&H...&`).
const COMMANDS: &[&str] = &[
    "alpha", "fscx", "fscy", "fade", "clip", "iclip", "move", "frz", "frx", "fry", "bord", "shad", "blur", "pbo", "1c", "2c", "3c", "4c", "an", "fs",
    "fad", "ko", "kf", "pos", "k", "q", "p", "r", "b", "i", "u", "s", "c",
];

/// `{\an8\i1\fs28\c&H0000FF&}` — aplica o que dá e descarta o resto.
fn apply_overrides(block: &str, style: &mut Style, anchor: &mut Anchor, play_res: (f32, f32), state: &mut TextState, unsupported: &mut bool) {
    for command in block.split('\\').skip(1) {
        let command = command.trim();
        let (name, arg) = split_command(command);
        match name {
            "an" => {
                if let Ok(n) = arg.trim().parse::<u8>() {
                    if (1..=9).contains(&n) {
                        *anchor = anchor_of(n);
                    }
                }
            }
            "i" => state.italic = flag(arg),
            "b" => state.bold = flag(arg),
            // `\fs` inline vale para a cue inteira (aparece no começo na prática).
            "fs" => {
                if let Some(size) = parse_positive(arg) {
                    style.size = size / play_res.1;
                }
            }
            "c" | "1c" => {
                if let Some(rgb) = parse_color(arg) {
                    state.color = Some(rgb);
                }
            }
            // `\r` volta ao estilo do script.
            "r" => {
                state.bold = style.bold;
                state.italic = style.italic;
                state.color = None;
            }
            "pos" | "move" => *unsupported = true,
            _ => {}
        }
    }
}

/// Separa o nome (minúsculo, estável) do argumento cru do comando.
fn split_command(command: &str) -> (&'static str, &str) {
    let lower = command.to_ascii_lowercase();
    for &name in COMMANDS {
        if lower.starts_with(name) {
            return (name, &command[name.len()..]);
        }
    }
    ("", command)
}

/// Valor booleano do ASS: `0` desliga; `-1`/`1`/`700` etc. ligam.
fn flag(raw: &str) -> bool {
    let raw = raw.trim();
    !(raw.is_empty() || raw == "0" || raw == "-0")
}

/// `&HAABBGGRR` (alfa primeiro, 00 = opaco) ou `&HBBGGRR` → RGB.
fn parse_color(raw: &str) -> Option<[u8; 3]> {
    let hex = raw.trim().trim_start_matches('&').trim_start_matches(['H', 'h']).trim_end_matches('&').trim();
    if hex.is_empty() || hex.len() > 8 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some([(value & 0xFF) as u8, ((value >> 8) & 0xFF) as u8, ((value >> 16) & 0xFF) as u8])
}

/// Nome da fonte → família disponível (só proporcional/mono no app).
fn font_family(name: &str) -> Family {
    let name = name.to_ascii_lowercase();
    if ["mono", "courier", "consolas", "fixed", "terminal"].iter().any(|m| name.contains(m)) {
        Family::Monospace
    } else {
        Family::Proportional
    }
}

fn parse_positive(raw: &str) -> Option<f32> {
    let value = raw.trim().trim_end_matches("px").trim().parse::<f32>().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

/// `H:MM:SS.cc` (centésimos; também aceita vírgula e fração livre).
fn parse_time(raw: &str) -> Option<f64> {
    let mut parts = raw.trim().split(':');
    let (hours, minutes, seconds) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(m), Some(s), None) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?, s),
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

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = concat!(
        "[Script Info]\n",
        "Title: teste\n",
        "PlayResX: 1920\n",
        "PlayResY: 1080\n",
        "\n",
        "[V4+ Styles]\n",
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
        "Style: Default,Arial,60,&H00FFFFFF,&H000000FF,&H00101010,&H80000000,0,0,0,0,100,100,0,0,1,2,1,2,40,40,50,1\n",
        "Style: Topo,Comic Sans MS,72,&H0000FFFF,&H000000FF,&H0000FF00,&H80000000,-1,-1,0,0,100,100,0,0,3,2,1,8,10,10,20,1\n",
        "\n",
        "[Events]\n",
        "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        "Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,Primeira fala\n",
        "Dialogue: 0,0:00:04.00,0:00:06.00,Topo,,0,0,0,,No topo\\Ne com quebra\n",
    );

    #[test]
    fn script_info_styles_e_events() {
        let parsed = parse(SCRIPT);
        assert_eq!(parsed.cues.len(), 2, "{:?}", parsed.warnings);
        let first = &parsed.cues[0];
        assert!((first.start - 1.0).abs() < 1e-9 && (first.end - 3.5).abs() < 1e-9);
        assert_eq!(first.segments[0].text, "Primeira fala");
        // 60 px num PlayResY de 1080 = 5,5% da altura da imagem.
        assert!((first.style.size - 60.0 / 1080.0).abs() < 1e-6, "{:?}", first.style);
        assert_eq!(first.style.color, [255, 255, 255]);
        assert_eq!(first.style.outline, [0x10, 0x10, 0x10]);
        assert_eq!(first.style.backdrop, Backdrop::Outline);
        assert_eq!(first.anchor.v, VAlign::Bottom);
        assert_eq!(first.anchor.h, HAlign::Center);
        assert!((first.style.margin_v - 50.0 / 1080.0).abs() < 1e-6);
        assert!((first.style.margin_l - 40.0 / 1920.0).abs() < 1e-6);
    }

    /// `\N` vira quebra de linha, negrito/itálico vêm do estilo e o estilo
    /// "Topo" muda fonte, cor, caixa e alinhamento (8 = topo/centro).
    #[test]
    fn estilo_nomeado_e_quebra_dura() {
        let parsed = parse(SCRIPT);
        let second = &parsed.cues[1];
        assert_eq!(second.segments[0].text, "No topo\ne com quebra");
        assert!(second.style.bold && second.style.italic);
        assert_eq!(second.style.color, [255, 255, 0], "&H0000FFFF = amarelo");
        assert_eq!(second.style.backdrop, Backdrop::Box, "BorderStyle 3");
        assert_eq!(second.anchor.v, VAlign::Top);
        assert_eq!(second.anchor.h, HAlign::Center);
        assert_eq!(second.style.family, Family::Proportional);
    }

    /// Cores em `&HBBGGRR&` e `&HAABBGGRR`: BGR na ordem do formato.
    #[test]
    fn cores_hex_bgr() {
        assert_eq!(parse_color("&H0000FF&"), Some([255, 0, 0]));
        assert_eq!(parse_color("&HFF0000&"), Some([0, 0, 255]));
        assert_eq!(parse_color("&H00FFFFFF&"), Some([255, 255, 255]));
        assert_eq!(parse_color("&H8020C040"), Some([0x40, 0xC0, 0x20]));
        assert_eq!(parse_color("&H123456&"), Some([0x56, 0x34, 0x12]), "&HBBGGRR");
        assert_eq!(parse_color("&HFF&"), Some([0xFF, 0x00, 0x00]), "dígito solto lê o que der");
        assert_eq!(parse_color("&H&"), None);
        assert_eq!(parse_color("&HGGGGGG&"), None);
    }

    /// `\an` muda o alinhamento, `\i1\b1` o estilo, `\fs` o tamanho e `\c` a cor
    /// do trecho; o resto dos overrides sai do texto.
    #[test]
    fn overrides_inline() {
        let script = concat!(
            "[Script Info]\n",
            "PlayResX: 640\n",
            "PlayResY: 480\n",
            "[V4+ Styles]\n",
            "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n",
            "Style: Default,Consolas,24,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n",
            "[Events]\n",
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\an7\\fs48\\c&H0000FF&}Alerta{\\r} normal\n",
            "Dialogue: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,{\\pos(320,240)}com pos\n",
        );
        let parsed = parse(script);
        assert_eq!(parsed.cues.len(), 2);
        let cue = &parsed.cues[0];
        assert_eq!(cue.anchor.h, HAlign::Left);
        assert_eq!(cue.anchor.v, VAlign::Top);
        assert!((cue.style.size - 48.0 / 480.0).abs() < 1e-6, "{:?}", cue.style.size);
        assert_eq!(cue.style.family, Family::Monospace);
        assert_eq!(cue.segments.len(), 2, "{:?}", cue.segments);
        assert_eq!(cue.segments[0].text, "Alerta");
        assert_eq!(cue.segments[0].color, Some([255, 0, 0]));
        assert_eq!(cue.segments[1].text, " normal");
        assert_eq!(cue.segments[1].color, None, "\\r volta à cor do estilo");
        // `\pos` não posiciona (fora do render básico) e vira aviso.
        assert_eq!(parsed.cues[1].anchor, Anchor::default());
        assert!(parsed.warnings.iter().any(|w| w.contains("pos")), "{:?}", parsed.warnings);
    }

    /// Sem `Format:` no bloco o padrão do SSA vale; estilo desconhecido cai no
    /// primeiro do script; tempos inválidos pulam a entrada.
    #[test]
    fn tolerante_a_arquivos_torto() {
        let script = concat!(
            "[Script Info]\n",
            "[V4 Styles]\n",
            "Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n",
            "[Events]\n",
            "Dialogue: Marked=0,0:00:01.00,0:00:02.00,Sumiu,,0,0,0,,sem estilo\n",
            "Dialogue: Marked=0,0:00:03.00,0:00:02.00,Default,,0,0,0,,invertida\n",
            "Dialogue: Marked=0,xx,yy,Default,,0,0,0,,tempo ruim\n",
            "Comment: 0,0:00:05.00,0:00:06.00,Default,,0,0,0,,comentário\n",
        );
        let parsed = parse(script);
        assert_eq!(parsed.cues.len(), 1, "{:?}", parsed.cues.len());
        assert_eq!(parsed.cues[0].segments[0].text, "sem estilo");
        assert!((parsed.cues[0].style.size - 20.0 / DEFAULT_PLAY_RES.1).abs() < 1e-6);
    }

    /// O texto pode conter vírgulas sem quebrar as colunas do `Dialogue`.
    #[test]
    fn texto_com_virgulas() {
        let script = concat!(
            "[Events]\n",
            "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
            "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,um, dois, três\n",
        );
        assert_eq!(parse(script).cues[0].segments[0].text, "um, dois, três");
    }

    #[test]
    fn tempo_aceita_centesimos_e_rejeita_lixo() {
        assert!((parse_time("1:02:03.50").unwrap() - 3723.5).abs() < 1e-9);
        assert!((parse_time("0:00:01,25").unwrap() - 1.25).abs() < 1e-9);
        for bad in ["", "1:2", "0:61:00.00", "0:00:60.00", "a:b:c"] {
            assert!(parse_time(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn alinhamento_numpad() {
        let expect = [
            (HAlign::Left, VAlign::Bottom),
            (HAlign::Center, VAlign::Bottom),
            (HAlign::Right, VAlign::Bottom),
            (HAlign::Left, VAlign::Middle),
            (HAlign::Center, VAlign::Middle),
            (HAlign::Right, VAlign::Middle),
            (HAlign::Left, VAlign::Top),
            (HAlign::Center, VAlign::Top),
            (HAlign::Right, VAlign::Top),
        ];
        for (i, (h, v)) in expect.into_iter().enumerate() {
            let anchor = anchor_of(i as u8 + 1);
            assert_eq!((anchor.h, anchor.v), (h, v), "\\an{}", i + 1);
        }
        // Fora de 1..=9 cai no 9/1 sem pânico.
        assert_eq!(anchor_of(0).h, HAlign::Left);
        assert_eq!(anchor_of(200).v, VAlign::Top);
    }
}
