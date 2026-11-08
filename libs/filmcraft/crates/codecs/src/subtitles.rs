//! fork local-player-rs: faixas de legenda embutidas no container.
//!
//! O hub expõe vídeo e áudio em [`filmcraft_media::MediaInfo`]; legenda nunca
//! entrou porque o editor do FilmCraft não a consome. O player precisa (o João
//! compara com o mpv: "tem nesses arquivos, o mpv pegava"), então este módulo
//! abre o arquivo de novo com os demuxers já vendorizados e devolve:
//!
//! - [`probe`]: as faixas de legenda do container (idioma, codec/formato, nome);
//! - [`extract`]: as amostras de cada faixa com início/fim em **segundos** e o
//!   payload cru. Quem interpreta SRT/WebVTT/ASS/tx3g é o app (`src/subtitle/`),
//!   onde estão os parsers e o render.
//!
//! O `MediaSource`/`MediaInfo` do upstream ficam intocados (o fork não muda o
//! craft-media vendido; a superfície nova fica aqui). Divergência registrada em
//! `docs/upstream/fork-patches.md` (seção 4).

use std::sync::Arc;

use filmcraft_isobmff::{CodecConfig, TrackKind as Mp4Kind};
use filmcraft_matroska::{Codec as MkvCodec, OpenOptions, TrackKind as MkvKind};
use filmcraft_media::SharedReader;

use crate::Src;
use crate::{CodecError, Result};

/// Teto de amostras lidas por faixa: legenda real tem milhares, não milhões; o
/// teto evita que um arquivo hostil monte uma lista gigante de cues.
const MAX_SAMPLES_PER_TRACK: usize = 100_000;

/// Codec/formato de uma faixa de legenda, como o app precisa para escolher o
/// parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubtitleCodec {
    /// `S_TEXT/UTF8` (Matroska): payload é o texto SubRip da cue.
    SubRip,
    /// `S_TEXT/WEBVTT` / `D_WEBVTT/*`: payload é o texto da cue WebVTT.
    WebVtt,
    /// `S_TEXT/ASS` / `S_TEXT/SSA`: `header` é o script (`CodecPrivate`) com
    /// estilos; cada amostra é o texto de um `Dialogue:`.
    Ass { header: Vec<u8>, ssa: bool },
    /// `tx3g` (ISO/IEC 14496-17) ou QuickTime `text` (`mov_text`): amostra com
    /// tamanho u16 + texto.
    Tx3g,
    /// Qualquer outro codec de legenda (bitmap etc.): listado, não suportado.
    Other(String),
}

impl SubtitleCodec {
    /// Nome curto para log/caixa de seleção (o mesmo de FFmpeg onde existe).
    pub fn name(&self) -> &str {
        match self {
            SubtitleCodec::SubRip => "subrip",
            SubtitleCodec::WebVtt => "webvtt",
            SubtitleCodec::Ass { ssa, .. } => {
                if *ssa {
                    "ssa"
                } else {
                    "ass"
                }
            }
            SubtitleCodec::Tx3g => "tx3g",
            SubtitleCodec::Other(name) => name,
        }
    }

    /// True se o app sabe transformar as amostras em cues de texto.
    pub fn is_supported(&self) -> bool {
        !matches!(self, SubtitleCodec::Other(_))
    }
}

/// Uma faixa de legenda do container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleTrack {
    /// Índice da faixa no demuxer do container (para ler as amostras).
    pub index: usize,
    /// ID da faixa no container.
    pub id: u64,
    /// Idioma ISO 639-2 (`und` quando o container não declara).
    pub language: String,
    /// Nome/título da faixa, se houver.
    pub name: String,
    pub codec: SubtitleCodec,
}

/// Uma amostra de legenda: tempos em segundos de mídia e o payload cru.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleSample {
    pub start: f64,
    pub end: f64,
    pub data: Vec<u8>,
}

/// Faixa pronta para interpretar: metadados + amostras em ordem de tempo.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddedSubtitle {
    pub track: SubtitleTrack,
    pub samples: Vec<SubtitleSample>,
}

/// Container reconhecido pelo extrator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Mp4,
    Matroska,
}

/// Detecta MP4/MOV vs Matroska/WebM pelos primeiros bytes (o nome do arquivo não
/// é confiável: um `.mkv` pode ser MP4 renomeado).
fn sniff(reader: &SharedReader) -> Result<Container> {
    let mut head = [0u8; 12];
    let n = reader.len().min(head.len() as u64) as usize;
    reader.read_at(0, &mut head[..n]).map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
    if n >= 8 && (&head[4..8] == b"ftyp" || &head[4..8] == b"moov" || &head[4..8] == b"mdat" || &head[4..8] == b"free") {
        return Ok(Container::Mp4);
    }
    if n >= 4 && head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Ok(Container::Matroska);
    }
    Err(CodecError::Unsupported("legendas: container não reconhecido".into()))
}

/// Lista as faixas de legenda do arquivo. Matroska é aberto **sem** indexar os
/// clusters (só o bloco `Tracks` interessa aqui).
pub fn probe(reader: &SharedReader) -> Result<Vec<SubtitleTrack>> {
    match sniff(reader)? {
        Container::Mp4 => {
            let file = filmcraft_isobmff::open(Src(reader.clone())).map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
            Ok(file
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.kind == Mp4Kind::Subtitle)
                .map(|(index, t)| SubtitleTrack {
                    index,
                    id: u64::from(t.id),
                    language: t.language.clone(),
                    name: t.handler_name.clone(),
                    codec: mp4_codec(t),
                })
                .collect())
        }
        Container::Matroska => {
            let file = filmcraft_matroska::open_with(Src(reader.clone()), &OpenOptions { index: false, verify_crc: false })
                .map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
            Ok(file
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.kind == MkvKind::Subtitle)
                .map(|(index, t)| SubtitleTrack {
                    index,
                    id: t.number,
                    language: t.language.clone(),
                    name: t.name.clone(),
                    codec: mkv_codec(&t.codec),
                })
                .collect())
        }
    }
}

/// Lê as amostras de todas as faixas de legenda do arquivo.
pub fn extract(reader: &SharedReader) -> Result<Vec<EmbeddedSubtitle>> {
    match sniff(reader)? {
        Container::Mp4 => extract_mp4(reader),
        Container::Matroska => extract_matroska(reader),
    }
}

fn extract_mp4(reader: &SharedReader) -> Result<Vec<EmbeddedSubtitle>> {
    let src = Src(reader.clone());
    let file = filmcraft_isobmff::open(src.clone()).map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
    let mut out = Vec::new();
    for (index, t) in file.tracks.iter().enumerate() {
        if t.kind != Mp4Kind::Subtitle {
            continue;
        }
        let codec = mp4_codec(t);
        let timescale = f64::from(t.timescale.max(1));
        let mut samples = Vec::new();
        for i in 0..t.samples.len().min(MAX_SAMPLES_PER_TRACK) {
            let Some(presentation) = t.presentation_pts(i) else { continue };
            let duration = t.samples[i].duration;
            let start = presentation as f64 / timescale;
            let end = (presentation + i64::from(duration)) as f64 / timescale;
            let data = file.read_sample(&src, index, i).map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
            samples.push(SubtitleSample { start, end, data });
        }
        out.push(EmbeddedSubtitle {
            track: SubtitleTrack { index, id: u64::from(t.id), language: t.language.clone(), name: t.handler_name.clone(), codec },
            samples,
        });
    }
    Ok(out)
}

fn extract_matroska(reader: &SharedReader) -> Result<Vec<EmbeddedSubtitle>> {
    let src = Src(reader.clone());
    // Indexar lê só cabeçalhos de cluster/bloco (nunca o payload do vídeo) e dá
    // as tabelas de amostras por faixa; é o mesmo trabalho que o motor já faz.
    let file = filmcraft_matroska::open_with(src.clone(), &OpenOptions { index: true, verify_crc: false })
        .map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
    let mut out = Vec::new();
    for (index, t) in file.tracks.iter().enumerate() {
        if t.kind != MkvKind::Subtitle {
            continue;
        }
        let codec = mkv_codec(&t.codec);
        let (num, den) = t.timebase;
        let to_secs = |ticks: u64| ticks as f64 * num as f64 / den as f64;
        let mut samples = Vec::new();
        for i in 0..t.samples.len().min(MAX_SAMPLES_PER_TRACK) {
            let s = t.samples[i];
            let start = s.pts as f64 * num as f64 / den as f64;
            // Bloco sem `BlockDuration`: a cue vale até a próxima (fallback) ou 3 s.
            let end = if s.duration > 0 {
                start + to_secs(s.duration)
            } else {
                t.samples.get(i + 1).map(|next| next.pts as f64 * num as f64 / den as f64).unwrap_or(start + 3.0)
            };
            let data = file.read_sample(&src, index, i).map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
            samples.push(SubtitleSample { start, end, data });
        }
        out.push(EmbeddedSubtitle {
            track: SubtitleTrack { index, id: t.number, language: t.language.clone(), name: t.name.clone(), codec },
            samples,
        });
    }
    Ok(out)
}

/// Codec de uma faixa de legenda MP4 (`stsd`). `tx3g`/`text` são os formatos de
/// texto que o app interpreta; `wvtt` (WebVTT em MP4) ainda fica de fora.
fn mp4_codec(t: &filmcraft_isobmff::Track) -> SubtitleCodec {
    match t.codec() {
        Some(CodecConfig::Unknown { fourcc, .. }) => match &fourcc.0 {
            b"tx3g" | b"text" => SubtitleCodec::Tx3g,
            other => SubtitleCodec::Other(String::from_utf8_lossy(other).trim().to_ascii_lowercase()),
        },
        Some(other) => SubtitleCodec::Other(other.name().to_string()),
        None => SubtitleCodec::Other("desconhecido".into()),
    }
}

/// Codec de uma faixa de legenda Matroska (CodecID + CodecPrivate).
fn mkv_codec(codec: &MkvCodec) -> SubtitleCodec {
    match codec {
        MkvCodec::SubRip => SubtitleCodec::SubRip,
        MkvCodec::WebVtt => SubtitleCodec::WebVtt,
        MkvCodec::Ass { header, ssa } => SubtitleCodec::Ass { header: header.clone(), ssa: *ssa },
        other => SubtitleCodec::Other(other.name()),
    }
}

/// Abre o arquivo por caminho e lê todas as faixas (ponto de entrada do app).
pub fn extract_path(path: &std::path::Path) -> Result<Vec<EmbeddedSubtitle>> {
    let file = filmcraft_media::reader::FileReader::open(path).map_err(|e| CodecError::Container(format!("legendas: {e}")))?;
    let reader: SharedReader = Arc::new(file);
    extract(&reader)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture real de MKV: o muxer do vendor grava uma faixa S_TEXT/UTF8 com
    /// duração por bloco, e o extrator devolve as amostras em segundos.
    #[test]
    fn extrai_legenda_subrip_de_mkv() {
        use filmcraft_matroska::{MkvWriter, MuxOptions, TrackSpec};
        use std::io::Cursor;

        let mut sub = TrackSpec::new(MkvKind::Subtitle, "S_TEXT/UTF8");
        sub.language = Some("por".into());
        let mut w =
            MkvWriter::new(Cursor::new(Vec::new()), vec![sub], MuxOptions { timestamp_scale: 1_000_000, ..Default::default() }).expect("muxer");
        w.write_frame(0, 1_000_000_000, true, b"primeira", Some(2_000_000_000)).expect("cue 1");
        w.write_frame(0, 4_000_000_000, true, b"segunda", Some(1_500_000_000)).expect("cue 2");
        let bytes = w.finish().expect("finish").into_inner();
        let dir = std::env::temp_dir().join(format!("filmcraft-subs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("fixture.mkv");
        std::fs::write(&path, &bytes).expect("write fixture");

        let reader: SharedReader = Arc::new(filmcraft_media::reader::FileReader::open(&path).expect("reader"));
        let tracks = probe(&reader).expect("probe");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].language, "por");
        assert_eq!(tracks[0].codec, SubtitleCodec::SubRip);

        let got = extract_path(&path).expect("extract");
        assert_eq!(got.len(), 1);
        let s = &got[0].samples;
        assert_eq!(s.len(), 2);
        assert!((s[0].start - 1.0).abs() < 1e-6 && (s[0].end - 3.0).abs() < 1e-6, "{s:?}");
        assert_eq!(s[0].data, b"primeira");
        assert!((s[1].start - 4.0).abs() < 1e-6 && (s[1].end - 5.5).abs() < 1e-6, "{s:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mapeia_codecs_de_legenda() {
        assert_eq!(mkv_codec(&MkvCodec::SubRip).name(), "subrip");
        assert_eq!(mkv_codec(&MkvCodec::WebVtt).name(), "webvtt");
        assert_eq!(mkv_codec(&MkvCodec::Ass { header: vec![1], ssa: true }).name(), "ssa");
        assert!(!mkv_codec(&MkvCodec::Other("S_HDMV/PGS".into())).is_supported());
        assert!(SubtitleCodec::Tx3g.is_supported());
    }
}
