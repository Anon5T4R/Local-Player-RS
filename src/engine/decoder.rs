//! Decodificação de arquivos de áudio.
//!
//! Dois backends, escolhidos por extensão (e um sniff de `OpusHead` em Ogg):
//!
//! - **symphonia** (`mp3`, `mp2`, `flac`, `wav`, `aiff`, `m4a`/mp4 AAC/ALAC,
//!   `ogg`/`oga` Vorbis): probe com `Hint` pela extensão, faixa default,
//!   decode loop por pacote. Pacote corrompido é pulado; stream corrompido
//!   demais vira erro (a UI mostra e segue pra próxima).
//! - **filmcraft** (`opus`, e `ogg` com `OpusHead`): o symphonia 0.5 não tem
//!   decodificador Opus; o `OggSource` do snapshot vendored tem (clean-room,
//!   RFC 6716/7845) e entrega PCM planar por acesso aleatório — seek de graça.
//!
//! O PCM sai **entrelaçado f32** na taxa da fonte; quem converte para a taxa
//! do device/stereo é o motor.

use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use filmcraft_codecs::OggSource;
use filmcraft_media::MediaSource;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

/// Um pedaço de PCM decodificado (entrelaçado, na taxa da fonte).
pub(super) struct Chunk {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

/// Resultado de um seek. A fonte **sempre** fica numa posição tocável.
pub(super) struct Seeked {
    /// Posição efetiva (pode diferir do pedido: fim clampado ou retorno).
    pub position: f64,
    /// Falha não-fatal: alvo recusado e a fonte restaurada em `position`.
    pub warning: Option<String>,
}

/// Backend de decodificação de uma faixa.
pub(super) trait AudioSource {
    /// Próximo pedaço de PCM; `None` = fim da faixa.
    fn next_chunk(&mut self) -> Result<Option<Chunk>, String>;
    /// Reposiciona em `secs`. Se o alvo falhar, a fonte volta ao último ponto
    /// bom (ou ao começo) e o aviso vem em [`Seeked::warning`] — uma busca
    /// recusada nunca deixa o leitor no fim do arquivo.
    fn seek(&mut self, secs: f64) -> Seeked;
    /// Duração declarada pelo container, se houver.
    fn duration(&self) -> Option<f64>;
    fn sample_rate(&self) -> u32;
}

/// Abre `path` no backend adequado.
pub(super) fn open(path: &str) -> Result<Box<dyn AudioSource>, String> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let ogg_opus = matches!(ext.as_str(), "ogg" | "oga") && sniff_opus(path)?;
    if ext == "opus" || ogg_opus {
        Ok(Box::new(OggOpusSource::open(path)?))
    } else {
        Ok(Box::new(SymphoniaSource::open(path, &ext)?))
    }
}

/// `true` se o começo do arquivo Ogg declara uma stream Opus.
fn sniff_opus(path: &str) -> Result<bool, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 4096];
    let mut len = 0;
    while len < buf.len() {
        match file.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(buf[..len].windows(8).any(|w| w == b"OpusHead"))
}

// ── symphonia ────────────────────────────────────────────────────────────

struct SymphoniaSource {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    rate: u32,
    duration: Option<f64>,
    /// Timestamp do último pacote decodificado (o ponto para onde voltar se um
    /// seek falhar).
    last_pos: f64,
}

impl SymphoniaSource {
    fn open(path: &str, ext: &str) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(ext);
        // Gapless só no MP3 (atraso/padding declarados pelo LAME/Xing); os
        // outros formatos têm o timing no container e são tratados pelo
        // próprio demuxer. Um Xing/Info com contagem ZERADA (o que um ffmpeg
        // escrevendo em pipe/corte deixa) faz o symphonia calcular
        // `0 - delay - padding` e estourar: nesse caso o gapless fica de fora.
        let gapless = ext == "mp3" && mp3_gapless_ok(path);
        let opts = FormatOptions { enable_gapless: gapless, ..FormatOptions::default() };
        let probed = symphonia::default::get_probe().format(&hint, stream, &opts, &MetadataOptions::default()).map_err(|e| e.to_string())?;
        let format = probed.format;
        // Primeira faixa que declara taxa de amostragem é a faixa de áudio: o
        // "default track" de um mp4 é o vídeo, e a fase 2 toca só o áudio.
        // (`or_else(default_track)` cobre containers onde o reader ainda não
        // preencheu os params — aí o erro "unknown sample rate" é honesto.)
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.sample_rate.is_some())
            .or_else(|| format.default_track())
            .ok_or_else(|| "no audio track".to_string())?;
        let track_id = track.id;
        let params = track.codec_params.clone();
        let rate = params.sample_rate.ok_or_else(|| "unknown sample rate".to_string())?;
        let mpeg = is_mpeg_audio_ext(ext);
        let declared = params.n_frames.map(|n| n as f64 / f64::from(rate));
        let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let declared_ok = declared.is_some_and(|d| plausible_duration(d, file_len));
        // Duração não confiável em MPEG de áudio: conta os cabeçalhos de frame
        // (barato e sem decode). É o que salva o MP3 cortado, cujo Xing declara
        // zero frames — sem isso a barra de seek vira ~4·10^14 s e qualquer
        // arrasto pra frente vira um seek para o infinito. Os outros formatos
        // mantêm a duração do container (não têm o defeito do Xing).
        let duration = if mpeg {
            if declared_ok {
                declared
            } else {
                scan_mpeg_audio_duration(path)
            }
        } else {
            declared
        };
        let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
        Ok(Self { format, decoder, track_id, rate, duration, last_pos: 0.0 })
    }

    /// Alvo efetivo: finito, ≥ 0 e com folga do fim conhecido (pedir a
    /// duração exata cai no EOF do último frame em MP3).
    fn clamp_target(&self, secs: f64) -> f64 {
        let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        match self.duration {
            Some(d) if d > 0.0 => secs.min((d - 0.1).max(0.0)),
            _ => secs,
        }
    }

    fn try_seek(&mut self, secs: f64) -> Result<f64, String> {
        let to = SeekTo::Time { time: Time::from(secs), track_id: Some(self.track_id) };
        self.format.seek(SeekMode::Accurate, to).map_err(|e| e.to_string())?;
        self.decoder.reset();
        self.last_pos = secs;
        Ok(secs)
    }
}

impl AudioSource for SymphoniaSource {
    fn next_chunk(&mut self) -> Result<Option<Chunk>, String> {
        let mut misses = 0u32;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(SymphoniaError::DecodeError(_)) => {
                    misses += 1;
                    if misses > 64 {
                        return Err("danificado demais para continuar".into());
                    }
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => return Err("stream pediu reset (codec mudou no meio)".into()),
                Err(e) => return Err(e.to_string()),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let packet_ts = packet.ts();
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let spec = *buf.spec();
                    let frames = buf.capacity();
                    if frames == 0 {
                        continue;
                    }
                    let mut sb = SampleBuffer::<f32>::new(frames as u64, spec);
                    sb.copy_interleaved_ref(buf);
                    let samples = sb.samples().to_vec();
                    self.rate = spec.rate;
                    if spec.rate > 0 {
                        // Ponto de retorno se um seek futuro for recusado.
                        self.last_pos = packet_ts as f64 / f64::from(spec.rate);
                    }
                    return Ok(Some(Chunk { rate: spec.rate, channels: spec.channels.count().max(1), samples }));
                }
                Err(SymphoniaError::DecodeError(_)) => {
                    misses += 1;
                    if misses > 64 {
                        return Err("pacotes corrompidos demais".into());
                    }
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    fn seek(&mut self, secs: f64) -> Seeked {
        let target = self.clamp_target(secs);
        match self.try_seek(target) {
            Ok(at) => Seeked { position: at, warning: None },
            Err(detail) => {
                // O alvo passou do último frame: o demuxer varreu até o EOF.
                // Volta para o último pacote decodificado — a faixa continua.
                let restore = if self.last_pos.is_finite() && self.last_pos > 0.0 { self.last_pos } else { 0.0 };
                match self.try_seek(restore) {
                    Ok(at) => Seeked { position: at, warning: Some(detail) },
                    Err(_) => match self.try_seek(0.0) {
                        Ok(at) => Seeked { position: at, warning: Some(detail) },
                        Err(again) => Seeked { position: 0.0, warning: Some(format!("{detail}; restaurar também falhou: {again}")) },
                    },
                }
            }
        }
    }

    fn duration(&self) -> Option<f64> {
        self.duration
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

// ── Ogg Opus (filmcraft) ─────────────────────────────────────────────────

struct OggOpusSource {
    src: OggSource,
    rate: u32,
    total: i64,
    pos: i64,
}

/// 100 ms por chamada a 48 kHz (tamanho de buffer confortável pro ring).
const OPUS_CHUNK_FRAMES: usize = 4_800;

impl OggOpusSource {
    fn open(path: &str) -> Result<Self, String> {
        let bytes: Arc<[u8]> = std::fs::read(path).map_err(|e| e.to_string())?.into();
        let src = OggSource::open(path, bytes).map_err(|e| e.to_string())?;
        let info = src.info();
        let stream = info.audio_streams.first().ok_or_else(|| "no audio stream".to_string())?;
        let rate = if stream.sample_rate > 0 { stream.sample_rate } else { 48_000 };
        let total = info.duration.to_units_floor(i64::from(rate)).max(0);
        Ok(Self { src, rate, total, pos: 0 })
    }
}

impl AudioSource for OggOpusSource {
    fn next_chunk(&mut self) -> Result<Option<Chunk>, String> {
        if self.pos >= self.total {
            return Ok(None);
        }
        let frames = (self.total - self.pos).min(OPUS_CHUNK_FRAMES as i64) as usize;
        let buf = self.src.audio(self.pos, frames, self.rate).map_err(|e| e.to_string())?;
        self.pos += frames as i64;
        let ch = buf.channels.len();
        let n = buf.frames();
        if ch == 0 || n == 0 {
            return Ok(None);
        }
        let mut samples = Vec::with_capacity(n * ch);
        for i in 0..n {
            for chan in &buf.channels {
                samples.push(chan.get(i).copied().unwrap_or(0.0));
            }
        }
        Ok(Some(Chunk { rate: self.rate, channels: ch, samples }))
    }

    fn seek(&mut self, secs: f64) -> Seeked {
        let s = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        self.pos = ((s * f64::from(self.rate)) as i64).min(self.total);
        let position = if self.rate == 0 { 0.0 } else { self.pos as f64 / f64::from(self.rate) };
        Seeked { position, warning: None }
    }

    fn duration(&self) -> Option<f64> {
        if self.rate == 0 {
            None
        } else {
            Some(self.total as f64 / f64::from(self.rate))
        }
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

// ── MP3/MP2/MP1: duração quando o container mente ────────────────────────

/// Extensões que o symphonia abre pelo demuxer MPEG de áudio.
fn is_mpeg_audio_ext(ext: &str) -> bool {
    matches!(ext, "mp3" | "mp2" | "mp1")
}

/// Uma duração declarada só vale se couber nos bytes do arquivo. Um áudio real
/// nunca fica abaixo de 8 kbps; acima disso a conta `bytes·8/8000` é um teto
/// generoso (e pega o `n_frames` que estourou a subtração do gapless).
fn plausible_duration(secs: f64, file_len: u64) -> bool {
    if !secs.is_finite() || secs <= 0.0 {
        return false;
    }
    if file_len == 0 {
        return true;
    }
    let max = file_len as f64 * 8.0 / 8_000.0;
    secs <= max * 1.05 + 0.05
}

/// Cabeçalho MPEG de áudio (só o que o scan de duração usa).
#[derive(Clone, Copy)]
struct MpegFrame {
    /// 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5 (1 é reservado).
    version: u8,
    /// 3 = Layer I, 2 = Layer II, 1 = Layer III (0 é reservado).
    layer: u8,
    sample_rate: u32,
    frame_size: usize,
    samples: u32,
    mono: bool,
}

const BR_MPEG1_L1: [u32; 15] = [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448];
const BR_MPEG1_L2: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
const BR_MPEG1_L3: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BR_MPEG2_L1: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256];
const BR_MPEG2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
const SR_MPEG1: [u32; 3] = [44_100, 48_000, 32_000];
const SR_MPEG2: [u32; 3] = [22_050, 24_000, 16_000];
const SR_MPEG25: [u32; 3] = [11_025, 12_000, 8_000];

impl MpegFrame {
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 4 || b[0] != 0xFF || (b[1] & 0xE0) != 0xE0 {
            return None;
        }
        let version = (b[1] >> 3) & 0x03;
        let layer = (b[1] >> 1) & 0x03;
        if version == 1 || layer == 0 {
            return None;
        }
        let br_idx = usize::from((b[2] >> 4) & 0x0F);
        let sr_idx = usize::from((b[2] >> 2) & 0x03);
        if br_idx == 0 || br_idx == 15 || sr_idx == 3 {
            return None;
        }
        let padding = usize::from((b[2] >> 1) & 0x01);
        let mono = (b[3] >> 6) == 0x03;
        let mpeg1 = version == 3;
        let bitrates = match (mpeg1, layer) {
            (true, 3) => &BR_MPEG1_L1,
            (true, 2) => &BR_MPEG1_L2,
            (true, 1) => &BR_MPEG1_L3,
            (false, 3) => &BR_MPEG2_L1,
            _ => &BR_MPEG2_L23,
        };
        let bitrate = bitrates[br_idx].checked_mul(1_000)?;
        let sample_rate = match version {
            3 => SR_MPEG1[sr_idx],
            2 => SR_MPEG2[sr_idx],
            _ => SR_MPEG25[sr_idx],
        };
        if bitrate == 0 || sample_rate == 0 {
            return None;
        }
        let samples = match layer {
            3 => 384,
            2 => 1_152,
            _ if mpeg1 => 1_152,
            _ => 576,
        };
        // Layer I conta em slots de 4 bytes; Layer II/III em slots de 1 byte
        // (72 slots por frame no Layer III de MPEG-2/2.5).
        let coefficient = if layer == 3 {
            12u64
        } else if layer == 1 && !mpeg1 {
            72
        } else {
            144
        };
        let mut frame_size = (coefficient * u64::from(bitrate) / u64::from(sample_rate)) as usize;
        if layer == 3 {
            // Layer I: o padding é um slot de 4 bytes.
            frame_size = frame_size.saturating_add(padding).saturating_mul(4);
        } else {
            frame_size = frame_size.saturating_add(padding);
        }
        if frame_size < 4 {
            return None;
        }
        Some(Self { version, layer, sample_rate, frame_size, samples, mono })
    }

    /// Bytes de side info entre o header e um eventual Xing/Info/VBRI.
    fn side_info_len(&self) -> usize {
        if self.layer != 1 {
            return 0;
        }
        match (self.version == 3, self.mono) {
            (true, false) => 32,
            (true, true) => 17,
            (false, false) => 17,
            (false, true) => 9,
        }
    }
}

/// Primeiro frame MPEG válido do arquivo (offset + header), pulando ID3/capa.
fn find_first_mpeg_frame(path: &str) -> Option<(u64, MpegFrame)> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut base = 0u64;
    let mut keep = 0usize;
    loop {
        let n = file.read(&mut buf[keep..]).ok()?;
        if n == 0 {
            return None;
        }
        let filled = keep + n;
        let mut i = 0;
        while i + 3 < filled {
            let Some(frame) = MpegFrame::parse(&buf[i..i + 4]) else {
                i += 1;
                continue;
            };
            // Um candidato só vale se o frame seguinte (quando já está no
            // buffer) também parece cabeçalho — filtro contra bytes de capa.
            let next = i + frame.frame_size;
            if next + 3 < filled && MpegFrame::parse(&buf[next..next + 4]).is_none() {
                i += 1;
                continue;
            }
            return Some((base + i as u64, frame));
        }
        keep = filled.min(3);
        buf.copy_within(filled - keep..filled, 0);
        base += (filled - keep) as u64;
    }
}

/// `false` quando o Xing/Info declara 0 frames: o gapless do symphonia
/// subtrai delay/padding desse número e estoura (4,18·10^14 s no release,
/// panic no debug). Sem gapless, `n_frames` fica 0 e a duração do scan vale.
fn mp3_gapless_ok(path: &str) -> bool {
    let Some((offset, frame)) = find_first_mpeg_frame(path) else { return true };
    if frame.layer != 1 {
        return true;
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };
    if file.seek(SeekFrom::Start(offset + 4 + frame.side_info_len() as u64)).is_err() {
        return true;
    }
    let mut tag = [0u8; 8];
    if file.read_exact(&mut tag).is_err() || (&tag[..4] != b"Xing" && &tag[..4] != b"Info") {
        return true;
    }
    let flags = u32::from_be_bytes([tag[4], tag[5], tag[6], tag[7]]);
    if flags & 0x1 == 0 {
        return true;
    }
    let mut frames = [0u8; 4];
    if file.read_exact(&mut frames).is_err() {
        return true;
    }
    u32::from_be_bytes(frames) != 0
}

/// Duração de MPEG de áudio pela contagem de cabeçalhos de frame (sem decode).
fn scan_mpeg_audio_duration(path: &str) -> Option<f64> {
    let (start, _) = find_first_mpeg_frame(path)?;
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut seconds = 0.0f64;
    let mut frames = 0u64;
    loop {
        let mut header = [0u8; 4];
        if reader.read_exact(&mut header).is_err() {
            break;
        }
        let Some(frame) = MpegFrame::parse(&header) else { break };
        let skip = match frame.frame_size.checked_sub(4) {
            Some(skip) => skip,
            None => break,
        };
        if skip > 0 && reader.seek_relative(skip as i64).is_err() {
            break;
        }
        seconds += f64::from(frame.samples) / f64::from(frame.sample_rate);
        frames += 1;
        if frames >= 5_000_000 {
            break;
        }
    }
    (frames > 0 && seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// Downmix/upmix para stereo entrelaçado.
///
/// 1 canal vira L=R; 2 canais passam direto; mais de 2 usa os dois primeiros
/// (front L/R em WAV/SMPTE order) — não há matriz de surround na fase 2.
pub(super) fn to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => {
            let mut out = Vec::with_capacity(samples.len() * 2);
            for s in samples {
                out.push(*s);
                out.push(*s);
            }
            out
        }
        2 => samples.to_vec(),
        n => {
            let frames = samples.len() / n;
            let mut out = Vec::with_capacity(frames * 2);
            for i in 0..frames {
                out.push(samples.get(i * n).copied().unwrap_or(0.0));
                out.push(samples.get(i * n + 1).copied().unwrap_or(0.0));
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WAV PCM 16 bits mono 8 kHz com 4 amostras (0, +0,5, -0,5, 0).
    fn wav_pcm16() -> Vec<u8> {
        let samples: [i16; 4] = [0, 16_384, -16_384, 0];
        let data_len = (samples.len() * 2) as u32;
        let mut v = Vec::with_capacity(44 + data_len as usize);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&8_000u32.to_le_bytes());
        v.extend_from_slice(&16_000u32.to_le_bytes()); // bytes/s
        v.extend_from_slice(&2u16.to_le_bytes()); // block align
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    /// Decodifica a faixa inteira (taxa declarada, canais, samples, duração).
    fn decode_all(path: &str) -> Result<(u32, usize, Vec<f32>, Option<f64>), String> {
        let mut src = open(path)?;
        let rate = src.sample_rate();
        let duration = src.duration();
        let mut channels = 0;
        let mut samples = Vec::new();
        while let Some(chunk) = src.next_chunk()? {
            channels = chunk.channels;
            samples.extend(chunk.samples);
        }
        Ok((rate, channels, samples, duration))
    }

    /// Exercita o caminho real probe → default audio track → decode loop com
    /// um arquivo pequeno: pega quebra de fiação no symphonia (extensão,
    /// params, conversão PCM) sem depender de cpal.
    #[test]
    fn wav_pcm16_decodifica_pelo_symphonia() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("lp-rs-decode-{}-{nanos}.wav", std::process::id()));
        assert!(std::fs::write(&path, wav_pcm16()).is_ok(), "gravar WAV de teste");
        let decoded = decode_all(path.to_str().unwrap_or_default());
        let _ = std::fs::remove_file(&path);

        assert!(decoded.is_ok(), "WAV de teste não decodificou: {:?}", decoded.as_ref().err());
        let Ok((rate, channels, samples, duration)) = decoded else { return };
        assert_eq!(rate, 8_000, "taxa declarada");
        assert_eq!(channels, 1, "canais");
        assert!(duration.is_none_or(|d| (d - 0.0005).abs() < 1e-9), "duração: {duration:?}");
        assert_eq!(samples.len(), 4);
        assert!((samples[1] - 0.5).abs() < 1e-3, "{samples:?}");
        assert!((samples[2] + 0.5).abs() < 1e-3, "{samples:?}");
    }

    #[test]
    fn downmix_mono_duplica() {
        assert_eq!(to_stereo(&[0.5, -0.5], 1), vec![0.5, 0.5, -0.5, -0.5]);
    }

    #[test]
    fn stereo_passa_direto() {
        assert_eq!(to_stereo(&[1.0, 2.0, 3.0, 4.0], 2), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn surround_usa_frontais() {
        // 5.1 de mentira: frame = [L, R, C, LFE, Ls, Rs]
        let input = [1.0, 2.0, 9.0, 8.0, 7.0, 6.0, 11.0, 12.0, 90.0, 80.0, 70.0, 60.0];
        assert_eq!(to_stereo(&input, 6), vec![1.0, 2.0, 11.0, 12.0]);
    }

    #[test]
    fn sem_canais_nao_estoura() {
        assert!(to_stereo(&[], 0).is_empty());
    }

    /// Grava `bytes` num arquivo temporário com a extensão dada.
    fn temp_media(ext: &str, bytes: &[u8]) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("lp-rs-{}-{nanos}.{ext}", std::process::id()));
        assert!(std::fs::write(&path, bytes).is_ok(), "gravar arquivo de teste");
        path
    }

    /// Frame MPEG-1 Layer III 128 kbps/44,1 kHz estéreo (417 bytes). O payload
    /// de zeros decodifica como silêncio; com `tag`, carrega um Xing com a
    /// contagem ZERADA + LAME — exatamente o que um ffmpeg escrevendo em pipe
    /// deixa num corte, e a origem do `n_frames - delay` que estoura.
    fn mp3_frame(tag: bool) -> Vec<u8> {
        let mut frame = vec![0u8; 417];
        frame[0..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        if tag {
            frame[36..40].copy_from_slice(b"Xing");
            frame[40..44].copy_from_slice(&0x0000_000F_u32.to_be_bytes());
            // frames/bytes/TOC/quality continuam zerados (o defeito).
            frame[156..165].copy_from_slice(b"Lavc62.28");
        }
        frame
    }

    fn mp3_sintetico(audio_frames: usize) -> Vec<u8> {
        let mut bytes = mp3_frame(true);
        for _ in 0..audio_frames {
            bytes.extend_from_slice(&mp3_frame(false));
        }
        bytes
    }

    /// Regressão do bug do João: MP3 "cortado" com Xing zerado declarava
    /// 4,18·10^14 s (o gapless subtraía delay de 0 frames). A duração tem que
    /// vir do scan de cabeçalhos, não do container.
    #[test]
    fn mp3_com_xing_zerado_nao_vira_duracao_absurda() {
        let path = temp_media("mp3", &mp3_sintetico(40));
        let Some(path_str) = path.to_str() else { return };
        assert!(!mp3_gapless_ok(path_str), "Xing com 0 frames não pode ligar o gapless");
        let opened = SymphoniaSource::open(path_str, "mp3");
        let duration = match &opened {
            Ok(src) => src.duration(),
            Err(_) => None,
        };
        let open_error = opened.err();
        let _ = std::fs::remove_file(&path);

        assert!(duration.is_some(), "MP3 sintético abriu sem duração: {open_error:?}");
        let Some(duration) = duration else { return };
        // 41 frames (Xing + 40 de áudio) × 1152 amostras a 44,1 kHz.
        let expected = 41.0 * 1152.0 / 44_100.0;
        assert!((duration - expected).abs() < 1e-6, "duração {duration} != {expected}");
        assert!(duration < 10.0, "duração absurda sobreviveu: {duration}");
    }

    /// Um seek para além do fim (a barra com duração quebrada pedia isso)
    /// recusa o alvo, volta para o último ponto decodificado e — crucial — o
    /// próximo pacote continua vindo (EOF aqui virava `TrackEnded` e pulava a
    /// faixa).
    #[test]
    fn seek_alem_do_fim_restaura_e_a_faixa_continua() {
        let path = temp_media("mp3", &mp3_sintetico(80));
        let Some(path_str) = path.to_str() else { return };
        let opened = SymphoniaSource::open(path_str, "mp3");
        assert!(opened.is_ok(), "MP3 sintético não abriu: {:?}", opened.err());
        let Ok(mut src) = opened else {
            let _ = std::fs::remove_file(&path);
            return;
        };
        let first_ok = matches!(src.next_chunk(), Ok(Some(_)));
        assert!(first_ok, "o frame silencioso precisa decodificar");

        // Sem duração conhecida o clamp não mascara o alvo fora do arquivo.
        src.duration = None;
        let outcome = AudioSource::seek(&mut src, 1.0e9);
        let continues = matches!(src.next_chunk(), Ok(Some(_)));
        let _ = std::fs::remove_file(&path);

        assert!(outcome.warning.is_some(), "seek fora do arquivo tinha que avisar");
        assert!(outcome.position < 10.0, "posição restaurada absurda: {}", outcome.position);
        assert!(continues, "o decoder ficou no EOF: a UI trataria como fim de faixa e pularia");
    }

    /// Seek normal dentro da faixa continua funcionando (e o alvo é clampado
    /// com folga do fim para não cair no EOF do último frame).
    #[test]
    fn seek_dentro_da_faixa_e_clamp_do_fim() {
        let path = temp_media("mp3", &mp3_sintetico(80));
        let Some(path_str) = path.to_str() else { return };
        let opened = SymphoniaSource::open(path_str, "mp3");
        assert!(opened.is_ok(), "MP3 sintético não abriu: {:?}", opened.err());
        let Ok(mut src) = opened else {
            let _ = std::fs::remove_file(&path);
            return;
        };
        let inside = AudioSource::seek(&mut src, 1.0);
        let near_end = AudioSource::seek(&mut src, 1.0e9);
        let duration = src.duration();
        let after_ok = matches!(src.next_chunk(), Ok(Some(_)));
        let _ = std::fs::remove_file(&path);

        assert!(inside.warning.is_none(), "seek de 1 s recusado: {:?}", inside.warning);
        assert!((inside.position - 1.0).abs() < 1e-9, "posição {}", inside.position);
        assert!(near_end.warning.is_none(), "seek para o fim recusado: {:?}", near_end.warning);
        if let Some(d) = duration {
            assert!((near_end.position - (d - 0.1).max(0.0)).abs() < 1e-9, "clamp do fim: {}", near_end.position);
        }
        assert!(after_ok, "decoder parou no seek do fim");
    }

    /// O WAV (PCM) continua abrindo e aceitando seek pelo mesmo trait.
    #[test]
    fn wav_aceita_seek() {
        let path = temp_media("wav", &wav_pcm16());
        let Some(path_str) = path.to_str() else { return };
        let opened = SymphoniaSource::open(path_str, "wav");
        assert!(opened.is_ok(), "WAV de teste não abriu: {:?}", opened.err());
        let Ok(mut src) = opened else {
            let _ = std::fs::remove_file(&path);
            return;
        };
        let outcome = AudioSource::seek(&mut src, 0.0);
        let chunk_ok = matches!(src.next_chunk(), Ok(Some(_)));
        let _ = std::fs::remove_file(&path);
        assert!(outcome.warning.is_none(), "seek do WAV: {:?}", outcome.warning);
        assert_eq!(outcome.position, 0.0);
        assert!(chunk_ok, "WAV não decodificou depois do seek");
    }
}
