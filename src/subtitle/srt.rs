//! SubRip (`.srt`) e WebVTT (`.vtt`): parser tolerante dos dois.
//!
//! Aceita BOM, CRLF/CR solto, índice numérico opcional, timestamps com vírgula
//! (SRT) ou ponto (VTT), `MM:SS` sem hora e `HH:MM:SS.mmm`. No VTT, além do
//! texto: cabeçalho `WEBVTT`, configuração da cue (`align:`, `position:` e
//! `line:` em %), blocos `NOTE`/`STYLE`/`REGION` ignorados, `<c.classe>`,
//! `<v Nome>`, `<lang>` e `<ruby>` removidos como marcação (sobra o texto) e
//! capítulos (`chapter-*`) ignorados com aviso.
//!
//! Fora do alcance (documentado): `line:<número>` (posição em linhas), `size:`,
//! `vertical:` e `region:` são lidos e descartados; o texto sai com um estilo só
//! (negrito/itálico inline `<b>/<i>` é marcação, não estilo).

use super::{Anchor, Cue, HAlign, Segment, Style, VAlign};

/// Resultado de um parser: as cues válidas e os avisos (vão para o stderr).
pub struct Parsed {
    pub cues: Vec<Cue>,
    pub warnings: Vec<String>,
}

/// Parser de SRT/VTT. `vtt` liga as regras próprias do WebVTT (blocos NOTE e
/// capítulos); o formato do timestamp é aceito nos dois.
pub fn parse(text: &str, vtt: bool) -> Parsed {
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
    let mut warnings = Vec::new();
    for lines in blocks {
        let first = lines.first().copied().unwrap_or("");
        if vtt && is_vtt_meta(first) {
            continue;
        }
        // Capítulo do WebVTT: o bloco começa com um id `chapter-...`.
        if vtt && first.to_ascii_lowercase().starts_with("chapter-") {
            warnings.push(format!("capítulo WebVTT ignorado: {first}"));
            continue;
        }
        let Some(header) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let Some((start, end, settings)) = parse_range(lines[header]) else {
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
        cues.push(Cue {
            start,
            end,
            segments: vec![Segment { text: text_lines.join("\n"), bold: false, italic: false, color: None }],
            style: Style::default(),
            anchor: parse_settings(settings),
        });
    }
    // Ordem de exibição: a busca da UI pode parar na primeira cue futura.
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    Parsed { cues, warnings }
}

/// Blocos de serviço do WebVTT: cabeçalho, notas, estilo e regiões.
fn is_vtt_meta(first: &str) -> bool {
    let upper = first.to_ascii_uppercase();
    upper.starts_with("WEBVTT") || upper == "NOTE" || upper.starts_with("NOTE ") || upper == "STYLE" || upper == "REGION"
}

/// `00:00:01.000 --> 00:00:02.000 align:start position:10%` → (início, fim,
/// configuração crua). O fim é só o primeiro token da direita.
fn parse_range(line: &str) -> Option<(f64, f64, &str)> {
    let (left, right) = line.split_once("-->")?;
    let start = parse_timestamp(left)?;
    let right = right.trim_start();
    let end_token = right.split_whitespace().next()?;
    let end = parse_timestamp(end_token)?;
    let settings = right[end_token.len().min(right.len())..].trim();
    Some((start, end, settings))
}

/// Configuração da cue do VTT → âncora. Desconhecido é ignorado.
fn parse_settings(settings: &str) -> Anchor {
    let mut anchor = Anchor::default();
    for token in settings.split_whitespace() {
        let Some((key, value)) = token.split_once(':') else { continue };
        match key {
            "align" => {
                anchor.h = match value {
                    "start" | "left" => HAlign::Left,
                    "end" | "right" => HAlign::Right,
                    _ => HAlign::Center,
                };
            }
            // `position:10%` ou `position:10%,line-left`: só a fração interessa.
            "position" => {
                if let Some(p) = percent(value.split(',').next().unwrap_or("")) {
                    anchor.x = Some(p);
                }
            }
            "line" => {
                if let Some(p) = percent(value) {
                    anchor.y = Some(p);
                    anchor.v = VAlign::Top;
                }
            }
            _ => {}
        }
    }
    anchor
}

fn percent(raw: &str) -> Option<f32> {
    let value = raw.trim().strip_suffix('%')?.parse::<f32>().ok()?;
    value.is_finite().then(|| (value / 100.0).clamp(0.0, 1.0))
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

/// Remove a marcação do texto e decodifica as entidades comuns. No VTT,
/// `<c.classe>`, `<v Nome>`, `<lang xx>`, `<ruby>`/`<rt>` e as tags de estilo
/// `<i>/<b>/<u>/<s>` saem; tag desconhecida fica literal.
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
        let name = tag.trim_start_matches('/').trim_end_matches('/').trim();
        let name = name.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if !is_markup(&name) {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    decode_entities(&out)
}

fn is_markup(name: &str) -> bool {
    if matches!(name, "i" | "b" | "u" | "s" | "c" | "v" | "lang" | "ruby" | "rt") || name.starts_with("c.") {
        return true;
    }
    // Etiqueta de timestamp do WebVTT (`<00:00:05.000>`, usada em karaokê).
    name.contains(':') && name.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.')
}

fn decode_entities(text: &str) -> String {
    // `&amp;` por último: senão `&amp;lt;` viraria `<` (dupla decodificação).
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&nbsp;", " ").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(cues: &[Cue]) -> Vec<String> {
        cues.iter().map(|c| c.segments.iter().map(|s| s.text.as_str()).collect::<String>()).collect()
    }

    #[test]
    fn parser_basico() {
        let src = "1\n00:00:01,000 --> 00:00:03,500\nOlá\n\n2\n00:00:04,000 --> 00:00:06,000\nSegunda\nlinha\n";
        let parsed = parse(src, false);
        assert_eq!(parsed.cues.len(), 2);
        assert_eq!((parsed.cues[0].start, parsed.cues[0].end), (1.0, 3.5));
        assert_eq!(texts(&parsed.cues), vec!["Olá", "Segunda\nlinha"]);
        assert!(parsed.cues[0].anchor == Anchor::default());
    }

    /// BOM UTF-8, CRLF, bloco sem índice e timestamp com ponto (VTT).
    #[test]
    fn bom_crlf_e_timestamp_com_ponto() {
        let src = "\u{feff}00:00:00,500 --> 00:00:01,000\r\nSem índice\r\n\r\n2\r\n00:00:02.000 --> 00:00:03.250\r\nPonto\r\n";
        let cues = parse(src, false).cues;
        assert_eq!(cues.len(), 2);
        assert!((cues[0].start - 0.5).abs() < 1e-9, "{cues:?}");
        assert!((cues[1].start - 2.0).abs() < 1e-9, "{cues:?}");
        assert!((cues[1].end - 3.25).abs() < 1e-9, "{cues:?}");
    }

    #[test]
    fn tags_e_entidades_saem() {
        let src = "1\n00:00:01,000 --> 00:00:02,000\n<i>itálico</i> e <b>negrito</b> &amp; <u>sublinhado</u>\n";
        assert_eq!(texts(&parse(src, false).cues), vec!["itálico e negrito & sublinhado"]);
    }

    /// VTT de verdade: `<c.classe>` e `<v Nome>` saem do texto, `WEBVTT` não
    /// vira bloco e a configuração da cue é lida.
    #[test]
    fn vtt_com_marcacao_e_settings() {
        let src = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000 align:start position:10%\n<c.yellow>amarelo</c> <v João>fala</v>\n";
        let parsed = parse(src, true);
        assert_eq!(texts(&parsed.cues), vec!["amarelo fala"]);
        let anchor = parsed.cues[0].anchor;
        assert_eq!(anchor.h, HAlign::Left);
        assert!((anchor.x.unwrap() - 0.1).abs() < 1e-6, "{anchor:?}");
        assert_eq!(anchor.v, VAlign::Bottom, "sem line, a cue fica na base");
    }

    #[test]
    fn vtt_settings_de_linha_e_align_direita() {
        let src = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000 align:end line:85%\nFim\n";
        let anchor = parse(src, true).cues[0].anchor;
        assert_eq!(anchor.h, HAlign::Right);
        assert_eq!(anchor.v, VAlign::Top, "line:% ancora pelo topo");
        assert!((anchor.y.unwrap() - 0.85).abs() < 1e-6, "{anchor:?}");
        // `line:2` (número de linhas) e `size:` não são posição: descartados.
        let anchor = parse("WEBVTT\n\n00:00:01.000 --> 00:00:02.000 line:2 size:50%\nx\n", true).cues[0].anchor;
        assert_eq!((anchor.y, anchor.x), (None, None), "{anchor:?}");
    }

    /// Blocos NOTE/STYLE/REGION não viram legenda; capítulo vira aviso.
    #[test]
    fn note_e_capitulos_do_vtt() {
        let src = concat!(
            "WEBVTT\n\n",
            "NOTE isto é um comentário\n\n",
            "STYLE\n::cue { color: yellow }\n\n",
            "REGION\nid:bottom width:40%\n\n",
            "chapter-1\n00:00:01.000 --> 00:00:02.000\nCapítulo um\n\n",
            "00:00:03.000 --> 00:00:04.000\nFala\n",
        );
        let parsed = parse(src, true);
        assert_eq!(texts(&parsed.cues), vec!["Fala"]);
        assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
        assert!(parsed.warnings[0].contains("chapter-1"));
        // Em SRT, o "NOTE" é texto de cue como outro qualquer (sem aviso).
        let parsed = parse("1\n00:00:01,000 --> 00:00:02,000\nNOTE\n", false);
        assert_eq!(texts(&parsed.cues), vec!["NOTE"]);
        assert!(parsed.warnings.is_empty());
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
        let cues = parse(src, false).cues;
        assert_eq!(cues.len(), 1, "{cues:?}");
        assert_eq!(texts(&cues), vec!["boa"]);
    }

    #[test]
    fn timestamp_aceita_duas_partes_e_rejeita_lixo() {
        assert!((parse_timestamp("01:02:03,004").unwrap() - 3723.004).abs() < 1e-9);
        assert!((parse_timestamp("02:03.500").unwrap() - 123.5).abs() < 1e-9);
        for bad in ["", "--", "00:00:60,000", "00:61:00,000", "a:b:c", "1:2:3:4"] {
            assert!(parse_timestamp(bad).is_none(), "{bad}");
        }
    }

    /// Linha "em branco" com espaços também fecha o bloco.
    #[test]
    fn linha_em_branco_com_espacos_separa_blocos() {
        let cues = parse("1\n00:00:01,000 --> 00:00:02,000\nA\n \n2\n00:00:03,000 --> 00:00:04,000\nB\n", false).cues;
        assert_eq!(cues.len(), 2, "{cues:?}");
        assert_eq!(texts(&cues)[1], "B");
    }

    /// Percentual malformado não vira posição.
    #[test]
    fn settings_invalidos_sao_ignorados() {
        let anchor = parse("WEBVTT\n\n00:00:01.000 --> 00:00:02.000 position:abc align:sideways\nx\n", true).cues[0].anchor;
        assert_eq!((anchor.x, anchor.h), (None, HAlign::Center));
    }

    /// Etiqueta de timestamp do VTT (karaokê) sai do texto como as demais tags.
    #[test]
    fn timestamp_tag_do_vtt_sai() {
        let cues = parse("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<00:00:01.500>uma<00:00:01.800>palavra\n", true).cues;
        assert_eq!(texts(&cues), vec!["umapalavra"]);
    }
}
