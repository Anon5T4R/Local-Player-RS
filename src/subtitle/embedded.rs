//! Legendas embutidas no container: extração (fork `filmcraft-codecs`) e
//! conversão para o mesmo [`Subtitles`] que a legenda externa usa.
//!
//! A leitura do container fica no fork (`libs/filmcraft/crates/codecs/src/subtitles.rs`):
//! ele devolve, por faixa, idioma/codec e as amostras com início/fim em
//! segundos. Aqui só se traduz o payload de cada formato para cues:
//! `S_TEXT/UTF8` e `S_TEXT/WEBVTT` viram um SRT/VTT sintético (os parsers de
//! [`super::srt`] já são tolerantes), `S_TEXT/ASS` vira um script ASS com o
//! `CodecPrivate` como cabeçalho, e `tx3g` tem parser próprio ([`super::tx3g`]).
//!
//! **Externa vence embutida** (é mais explícita): quem chama só extrai daqui
//! quando não achou arquivo ao lado do vídeo. A escolha da faixa embutida é por
//! idioma do app/sistema e, sem correspondência, a primeira.

use std::path::Path;
use std::sync::Arc;

use filmcraft_codecs::subtitles::{SubtitleCodec, SubtitleSample, SubtitleTrack};

use super::srt;
use super::{tx3g, Subtitles};

/// Uma faixa embutida já interpretada, pronta para a UI.
pub struct Embedded {
    /// `Arc` porque a faixa ativa e a lista (tecla C) compartilham as mesmas cues.
    pub subs: Arc<Subtitles>,
    /// Idioma ISO 639-2 declarado pelo container (`und` = não declarado).
    pub language: String,
    /// Nome curto do codec (`subrip`, `ass`, `tx3g`, …).
    pub codec: String,
}

/// Lê todas as faixas de legenda embutidas do arquivo. Faixas com codec que o
/// player não interpreta (bitmap etc.) são registradas no stderr e puladas;
/// falha de container é erro (o chamador decide se vira status/aviso).
pub fn load(path: &Path) -> Result<Vec<Embedded>, String> {
    let tracks = filmcraft_codecs::subtitles::extract_path(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for embedded in tracks {
        let track = &embedded.track;
        if !track.codec.is_supported() {
            eprintln!("engine: legenda embutida {} ({}): codec {} não suportado", track.language, track.name, track.codec.name());
            continue;
        }
        match from_payloads(track, &embedded.samples) {
            Ok(mut subs) => {
                eprintln!(
                    "engine: legenda embutida {} ({}){}: {} cues",
                    track.language,
                    track.codec.name(),
                    if track.name.is_empty() { String::new() } else { format!(" \"{}\"", track.name) },
                    subs.cues.len()
                );
                subs.path = path.display().to_string();
                out.push(Embedded { subs: Arc::new(subs), language: track.language.clone(), codec: track.codec.name().to_owned() });
            }
            Err(e) => eprintln!("engine: legenda embutida {} ({}): {e}", track.language, track.name),
        }
    }
    Ok(out)
}

/// Converte as amostras de uma faixa para cues (sem tocar em I/O — é o que os
/// testes cobrem amostra a amostra).
pub fn from_payloads(track: &SubtitleTrack, samples: &[SubtitleSample]) -> Result<Subtitles, String> {
    let cues = match &track.codec {
        SubtitleCodec::SubRip => srt_cues(samples),
        SubtitleCodec::WebVtt => vtt_cues(samples),
        SubtitleCodec::Ass { header, .. } => ass_cues(header, samples)?,
        SubtitleCodec::Tx3g => tx3g::cues(samples),
        SubtitleCodec::Other(name) => return Err(format!("codec de legenda não suportado: {name}")),
    };
    if cues.is_empty() {
        return Err("nenhuma cue válida".into());
    }
    Ok(Subtitles { cues, path: String::new() })
}

/// Texto de uma amostra de legenda textual, sem NULs de preenchimento.
fn body(sample: &SubtitleSample) -> String {
    let text = String::from_utf8_lossy(&sample.data);
    text.trim_end_matches('\0').replace('\u{0000}', "")
}

/// Tempo utilizável: finito e com fim depois do início (amostra quebrada não
/// vira cue).
fn valid_range(sample: &SubtitleSample) -> bool {
    sample.start.is_finite() && sample.end.is_finite() && sample.end > sample.start
}

fn srt_cues(samples: &[SubtitleSample]) -> Vec<super::Cue> {
    let mut text = String::new();
    let mut n = 0;
    for sample in samples {
        let body = body(sample);
        let body = body.trim();
        if body.is_empty() || !valid_range(sample) {
            continue;
        }
        n += 1;
        text.push_str(&format!("{n}\n{} --> {}\n{body}\n\n", stamp_srt(sample.start), stamp_srt(sample.end)));
    }
    srt::parse(&text, false).cues
}

fn vtt_cues(samples: &[SubtitleSample]) -> Vec<super::Cue> {
    let mut text = String::from("WEBVTT\n\n");
    for sample in samples {
        let body = body(sample);
        let body = body.trim();
        if body.is_empty() || !valid_range(sample) {
            continue;
        }
        text.push_str(&format!("{} --> {}\n{body}\n\n", stamp_vtt(sample.start), stamp_vtt(sample.end)));
    }
    srt::parse(&text, true).cues
}

/// Script ASS: `CodecPrivate` (estilos) + um `Dialogue:` por amostra.
fn ass_cues(header: &[u8], samples: &[SubtitleSample]) -> Result<Vec<super::Cue>, String> {
    if samples.iter().all(|s| body(s).trim().is_empty()) {
        return Ok(Vec::new());
    }
    let header = String::from_utf8_lossy(header).trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut script = String::new();
    if !header.contains("[Script Info]") {
        script.push_str("[Script Info]\nScriptType: v4.00+\nPlayResX: 1280\nPlayResY: 720\n");
    }
    script.push_str(&header);
    if !script.ends_with('\n') {
        script.push('\n');
    }
    if !script.contains("[Events]") {
        script.push_str("[Events]\n");
    }
    // `[Events]` sem `Format:` (CodecPrivate só com estilos): o parser tem o
    // formato padrão, mas o próprio script fica legível com a linha explícita.
    let events = script.rfind("[Events]").map(|at| &script[at..]).unwrap_or("");
    if !events.contains("Format:") {
        if let Some(at) = script.rfind("[Events]") {
            let after = at + "[Events]".len();
            script.insert_str(after, "\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text");
        }
    }
    for sample in samples {
        let text = body(sample).trim().replace('\n', "\\N");
        if text.is_empty() || !valid_range(sample) {
            continue;
        }
        script.push_str(&format!("Dialogue: 0,{},{},Default,,0,0,0,,{text}\n", stamp_ass(sample.start), stamp_ass(sample.end)));
    }
    Ok(super::ass::parse(&script).cues)
}

/// `HH:MM:SS,mmm` (SRT).
fn stamp_srt(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

/// `HH:MM:SS.mmm` (WebVTT).
fn stamp_vtt(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

/// `H:MM:SS.cc` (ASS; centésimos).
fn stamp_ass(seconds: f64) -> String {
    let cs = (seconds.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, (cs / 6_000) % 60, (cs / 100) % 60, cs % 100)
}

/// Faixa a exibir: a primeira com o idioma preferido; sem correspondência, a
/// primeira da lista (a ordem do container).
pub fn pick(tracks: &[Embedded], preferred: &str) -> Option<usize> {
    if tracks.is_empty() {
        return None;
    }
    tracks.iter().position(|t| language_matches(preferred, &t.language)).or(Some(0))
}

/// Idioma preferido a partir do idioma da UI (o app nasce com o do sistema; é o
/// mesmo idioma que o usuário escolheu para a interface). Códigos ISO 639-2.
pub fn preferred_language(lang: lab_ui::i18n::Lang) -> &'static str {
    match lang {
        lab_ui::i18n::Lang::Pt => "por",
        lab_ui::i18n::Lang::En => "eng",
        lab_ui::i18n::Lang::Es => "spa",
    }
}

/// Compara códigos de idioma tolerando a forma curta (`pt`, `pt-BR`), a longa
/// (`por`) e as variantes legadas do ISO 639-2/B (`fre`, `ger`, `chi`…).
pub fn language_matches(preferred: &str, track: &str) -> bool {
    let p = language_key(preferred);
    let t = language_key(track);
    !p.is_empty() && !t.is_empty() && t != "und" && p == t
}

fn language_key(code: &str) -> String {
    let base = code.split(['-', '_']).next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "pt" => "por".into(),
        "en" => "eng".into(),
        "es" => "spa".into(),
        "fr" => "fra".into(),
        "de" => "deu".into(),
        "it" => "ita".into(),
        "ja" => "jpn".into(),
        "ko" => "kor".into(),
        "zh" => "zho".into(),
        "ru" => "rus".into(),
        // bibliográficos (B) -> terminológicos (T)
        "fre" => "fra".into(),
        "ger" => "deu".into(),
        "chi" => "zho".into(),
        "dut" => "nld".into(),
        "rum" => "ron".into(),
        "slo" => "slk".into(),
        "per" => "fas".into(),
        "alb" => "sqi".into(),
        "arm" => "hye".into(),
        "baq" => "eus".into(),
        "bur" => "mya".into(),
        "geo" => "kat".into(),
        "ice" => "isl".into(),
        "mac" => "mkd".into(),
        "tib" => "bod".into(),
        "wel" => "cym".into(),
        other => other.into(),
    }
}

/// De onde vem a legenda ativa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Arquivo ao lado do vídeo (mais explícito que a faixa do container).
    External,
    /// Faixa embutida no container, pelo índice em `embedded`.
    Embedded(usize),
}

/// Decisão da fonte: externa carregada vence as faixas do container; sem
/// externa, a embutida escolhida pelo idioma do app/sistema.
pub fn choose_source(has_external: bool, embedded: &[Embedded], preferred: &str) -> Option<Source> {
    if has_external {
        return Some(Source::External);
    }
    pick(embedded, preferred).map(Source::Embedded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(codec: SubtitleCodec, language: &str) -> SubtitleTrack {
        SubtitleTrack { index: 0, id: 1, language: language.into(), name: String::new(), codec }
    }

    fn sample(start: f64, end: f64, data: &[u8]) -> SubtitleSample {
        SubtitleSample { start, end, data: data.to_vec() }
    }

    fn embedded(language: &str) -> Embedded {
        Embedded { subs: Arc::new(Subtitles { cues: Vec::new(), path: String::new() }), language: language.into(), codec: "subrip".into() }
    }

    #[test]
    fn subrip_embutido_vira_cues_com_tempo_do_container() {
        let subs = from_payloads(&track(SubtitleCodec::SubRip, "eng"), &[sample(1.0, 2.5, b"hello\n"), sample(3.0, 4.0, b"world")]).expect("subrip");
        assert_eq!(subs.cue_at(1.5).map(|c| c.segments[0].text.as_str()), Some("hello"));
        let cue = subs.cue_at(3.2).expect("cue");
        assert_eq!((cue.start, cue.end), (3.0, 4.0));
    }

    #[test]
    fn webvtt_embutido_usa_o_parser_vtt() {
        let subs = from_payloads(&track(SubtitleCodec::WebVtt, "por"), &[sample(0.5, 1.0, b"<v Ana>ol\xc3\xa1")]).expect("vtt");
        assert_eq!(subs.cue_at(0.7).map(|c| c.segments[0].text.as_str()), Some("olá"));
    }

    #[test]
    fn ass_embutido_reconstroi_o_script_do_codec_private() {
        let header = b"[Script Info]\nPlayResX: 1920\nPlayResY: 1080\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
        let subs = from_payloads(
            &track(SubtitleCodec::Ass { header: header.to_vec(), ssa: false }, "eng"),
            &[sample(1.0, 2.0, b"{\\an8}topo"), sample(3.0, 4.0, b"linha1\nlinha2")],
        )
        .expect("ass");
        assert_eq!(subs.cue_at(1.5).map(|c| c.segments[0].text.as_str()), Some("topo"));
        assert!(subs.cue_at(1.5).expect("cue").style.size < 0.05);
        assert_eq!(subs.cue_at(3.5).map(|c| c.segments[0].text.as_str()), Some("linha1\nlinha2"));
    }

    #[test]
    fn tx3g_embutido_usa_o_parser_proprio() {
        let mut data = 5u16.to_be_bytes().to_vec();
        data.extend_from_slice(b"ol\xc3\xa1");
        let subs = from_payloads(&track(SubtitleCodec::Tx3g, "por"), &[sample(2.0, 3.0, &data)]).expect("tx3g");
        assert_eq!(subs.cue_at(2.5).map(|c| c.segments[0].text.as_str()), Some("olá"));
    }

    #[test]
    fn codec_nao_suportado_falha_sem_panico() {
        assert!(from_payloads(&track(SubtitleCodec::Other("S_HDMV/PGS".into()), "eng"), &[]).is_err());
    }

    #[test]
    fn escolha_prefere_o_idioma_e_cai_na_primeira() {
        let tracks = vec![embedded("eng"), embedded("por"), embedded("spa")];
        assert_eq!(pick(&tracks, "por"), Some(1));
        assert_eq!(pick(&tracks, "pt-BR"), Some(1));
        assert_eq!(pick(&tracks, "fre"), Some(0), "sem correspondência: primeira");
        assert_eq!(pick(&[], "por"), None);
    }

    #[test]
    fn externa_vence_embutida() {
        let tracks = vec![embedded("por")];
        assert_eq!(choose_source(true, &tracks, "por"), Some(Source::External), "externa sempre ganha");
        assert_eq!(choose_source(false, &tracks, "por"), Some(Source::Embedded(0)));
        assert_eq!(choose_source(false, &[], "por"), None);
    }

    #[test]
    fn comparacao_de_idioma_tolera_formas() {
        assert!(language_matches("pt-BR", "por"));
        assert!(language_matches("por", "pt"));
        assert!(language_matches("eng", "en"));
        assert!(language_matches("deu", "ger"));
        assert!(!language_matches("por", "und"));
        assert!(!language_matches("", "por"));
    }
}
