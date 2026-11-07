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

use std::io::Read as _;
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

/// Backend de decodificação de uma faixa.
pub(super) trait AudioSource {
    /// Próximo pedaço de PCM; `None` = fim da faixa.
    fn next_chunk(&mut self) -> Result<Option<Chunk>, String>;
    /// Reposiciona em `secs`. `Err` = formato sem seek.
    fn seek(&mut self, secs: f64) -> Result<(), String>;
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
}

impl SymphoniaSource {
    fn open(path: &str, ext: &str) -> Result<Self, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(ext);
        // Gapless só no MP3 (atraso/padding declarados pelo LAME/Xing); os
        // outros formatos têm o timing no container e são tratados pelo
        // próprio demuxer.
        let opts = FormatOptions { enable_gapless: ext == "mp3", ..FormatOptions::default() };
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
        let duration = params.n_frames.map(|n| n as f64 / f64::from(rate));
        let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
        Ok(Self { format, decoder, track_id, rate, duration })
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

    fn seek(&mut self, secs: f64) -> Result<(), String> {
        let to = SeekTo::Time { time: Time::from(secs.max(0.0)), track_id: Some(self.track_id) };
        self.format.seek(SeekMode::Accurate, to).map_err(|e| e.to_string())?;
        self.decoder.reset();
        Ok(())
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

    fn seek(&mut self, secs: f64) -> Result<(), String> {
        let s = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        self.pos = ((s * f64::from(self.rate)) as i64).min(self.total);
        Ok(())
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
}
