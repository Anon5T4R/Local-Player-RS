//! Diagnóstico de um arquivo sem abrir janela: `--probe` e `--bench`.
//!
//! Os dois passam pelo **mesmo caminho de abertura** do player
//! ([`super::video::open_media`], o hub do FilmCraft), para que o resultado
//! valha para o que a UI faz. `--bench` é decode puro: sem áudio, sem GPU, sem
//! egui — é o que separa "o decode não acompanha o tempo real" (H1) de "a UI
//! engargala no upload/repaint" (H3).
//!
//! Nenhum dos dois é caminho de produto: são ferramentas de diagnóstico
//! permanentes (o João roda `--bench` no MKV que travou e manda a saída).

use std::sync::Arc;
use std::time::Instant;

use filmcraft_media::FrameRequest;
use filmcraft_time::Tick;

use super::error::EngineError;
use super::video::{self, VideoInfo};
use crate::metrics;

/// Metadados de uma faixa de vídeo.
#[derive(Clone, Debug)]
pub struct ProbeVideo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub pixel_format: String,
    pub bitrate: Option<u64>,
    pub has_alpha: bool,
}

/// Metadados de uma faixa de áudio.
#[derive(Clone, Debug)]
pub struct ProbeAudio {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: Option<u32>,
}

/// O que o `--probe` imprime.
#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub path: String,
    pub container: String,
    pub duration: f64,
    pub file_size: Option<u64>,
    /// bits/s (declarado pelo container ou estimado por arquivo/duração).
    pub bitrate: Option<u64>,
    pub video: Option<ProbeVideo>,
    pub audio: Vec<ProbeAudio>,
}

/// Lê os metadados de `path` pelo hub do FilmCraft (sem decodificar frames).
pub fn probe(path: &str) -> Result<ProbeReport, EngineError> {
    if !video::is_video_container(path) {
        return Err(EngineError::Open(format!("{path}: extensao nao suportada pelo probe (esperado: mp4, m4v, mov, mkv, webm, avi, m2ts, mts, ts)")));
    }
    let source = video::open_media(path)?;
    let info = source.info();
    let duration = info.duration.seconds();
    let bitrate = info.video.as_ref().and_then(|v| v.bitrate).or_else(|| metrics::estimate_bitrate(info.file_size, duration));
    let video = info.video.as_ref().map(|v| ProbeVideo {
        codec: v.codec.clone(),
        width: v.width,
        height: v.height,
        fps: v.frame_rate.sane().as_f64(),
        pixel_format: v.pixel_format.clone(),
        bitrate: v.bitrate,
        has_alpha: v.has_alpha,
    });
    let audio = info
        .audio_streams
        .iter()
        .map(|a| ProbeAudio { codec: a.codec.clone(), sample_rate: a.sample_rate, channels: a.channels, bits_per_sample: a.bits_per_sample })
        .collect();
    Ok(ProbeReport { path: path.to_string(), container: info.container.clone(), duration, file_size: info.file_size, bitrate, video, audio })
}

/// Um trecho de 10 s de tempo de mídia no benchmark (revela "estabilizou
/// porque o conteúdo ficou mais fácil" vs. melhoria real).
#[derive(Clone, Debug)]
pub struct BenchBucket {
    pub media_start: f64,
    pub frames: usize,
    pub fps: f64,
}

/// O que o `--bench` imprime.
#[derive(Clone, Debug)]
pub struct BenchReport {
    pub path: String,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: usize,
    /// Segundos de mídia decodificados.
    pub media_secs: f64,
    pub wall_secs: f64,
    pub decode_fps: f64,
    pub mean_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub buckets: Vec<BenchBucket>,
    /// Contadores do cache do fork no intervalo do bench.
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub seeks: u64,
    pub evicted: u64,
    /// Tempo médio dentro dos decoders por sample (ms).
    pub decoder_ms_per_sample: f64,
    pub cache_mb: f64,
    pub rss_mb: f64,
    pub rss_peak_mb: f64,
}

/// Decodifica `seconds` segundos (ou a faixa inteira, se `None`) o mais rápido
/// possível e mede o decode por frame.
pub fn bench(path: &str, seconds: Option<f64>) -> Result<BenchReport, EngineError> {
    let source = video::open_media(path)?;
    let info = source.info();
    let Some(vinfo) = VideoInfo::from_media(info) else {
        return Err(EngineError::Open(format!("{path}: a faixa nao tem video (o bench mede decode de video)")));
    };
    if !(vinfo.fps.is_finite() && vinfo.fps > 0.0) {
        return Err(EngineError::Open(format!("{path}: frame rate invalido ({})", vinfo.fps)));
    }
    let duration = (info.duration.seconds() > 0.0).then(|| info.duration.seconds());
    let media_limit = match (seconds, duration) {
        (Some(s), _) if s.is_finite() && s > 0.0 => s,
        (_, Some(d)) => d,
        // Sem duração e sem --seconds: um trecho limitado (não decodifica para sempre).
        _ => 60.0,
    };
    let frames = ((media_limit * vinfo.fps).ceil() as usize).max(1);
    let step = 1.0 / vinfo.fps;
    let repeat_limit = (5.0 / step).ceil().max(1.0) as usize;

    let before = filmcraft_codecs::gop_stats();
    let started = Instant::now();
    let mut samples_us: Vec<u32> = Vec::with_capacity(frames.min(200_000));
    let mut buckets: Vec<BenchBucket> = Vec::new();
    let mut bucket_start = started;
    let mut bucket_frames = 0usize;
    let mut bucket_idx = 0usize;
    let mut repeats = 0usize;
    let mut last: Option<Arc<filmcraft_frame::VideoFrame>> = None;
    let mut decoded = 0usize;
    let mut last_t = 0.0;

    for i in 0..frames {
        let t = i as f64 * step;
        last_t = t;
        let idx = (t / 10.0) as usize;
        if idx != bucket_idx {
            // Fecha o trecho anterior com o tempo de parede gasto nele.
            if bucket_frames > 0 {
                let bucket_wall = bucket_start.elapsed().as_secs_f64().max(1e-6);
                buckets.push(BenchBucket { media_start: bucket_idx as f64 * 10.0, frames: bucket_frames, fps: bucket_frames as f64 / bucket_wall });
            }
            bucket_idx = idx;
            bucket_start = Instant::now();
            bucket_frames = 0;
        }

        let t0 = Instant::now();
        let frame = source.video_frame(FrameRequest::full(Tick::from_seconds_f64(t))).map_err(|e| EngineError::Decode(format!("{path}: {e}")))?;
        let us = t0.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        samples_us.push(us);
        decoded += 1;
        bucket_frames += 1;

        let same = last.as_ref().is_some_and(|l| Arc::ptr_eq(l, &frame));
        if same {
            repeats += 1;
            if repeats >= repeat_limit {
                // Fim sem duração declarada: o source repete o último frame.
                drop(frame);
                break;
            }
        } else {
            repeats = 0;
        }
        last = Some(frame);
    }

    let wall = started.elapsed().as_secs_f64();
    if bucket_frames > 0 {
        let bucket_wall = bucket_start.elapsed().as_secs_f64().max(1e-6);
        buckets.push(BenchBucket { media_start: bucket_idx as f64 * 10.0, frames: bucket_frames, fps: bucket_frames as f64 / bucket_wall });
    }
    let after = filmcraft_codecs::gop_stats();
    let delta = after - before;
    let (rss_mb, rss_peak_mb) = metrics::rss_mb();

    let mean = if samples_us.is_empty() { 0.0 } else { samples_us.iter().map(|v| f64::from(*v)).sum::<f64>() / samples_us.len() as f64 / 1e3 };
    let mut sorted = samples_us;
    sorted.sort_unstable();
    let p95 = percentile(&sorted, 0.95);
    let max = sorted.last().map_or(0.0, |v| f64::from(*v) / 1e3);

    Ok(BenchReport {
        path: path.to_string(),
        codec: info.video.as_ref().map(|v| v.codec.clone()).unwrap_or_default(),
        width: vinfo.width,
        height: vinfo.height,
        fps: vinfo.fps,
        frames: decoded,
        media_secs: last_t + step,
        wall_secs: wall,
        decode_fps: if wall > 0.0 { decoded as f64 / wall } else { 0.0 },
        mean_ms: mean,
        p95_ms: p95,
        max_ms: max,
        buckets,
        cache_hits: delta.hits,
        cache_misses: delta.misses,
        seeks: delta.seeks,
        evicted: delta.evicted,
        decoder_ms_per_sample: delta.decode_ms_per_sample(),
        cache_mb: filmcraft_codecs::cached_bytes() as f64 / (1024.0 * 1024.0),
        rss_mb,
        rss_peak_mb,
    })
}

/// p95 (ou outro quantil) de amostras em µs → ms.
fn percentile(sorted_us: &[u32], q: f64) -> f64 {
    if sorted_us.is_empty() {
        return 0.0;
    }
    let idx = ((sorted_us.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    f64::from(sorted_us[idx.min(sorted_us.len() - 1)]) / 1e3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentil_do_bench() {
        let mut v: Vec<u32> = (1..=100).collect();
        v.sort_unstable();
        assert!((percentile(&v, 0.95) - 0.095).abs() < 1e-9);
        assert_eq!(percentile(&[], 0.95), 0.0);
    }

    #[test]
    fn probe_recusa_extensao_desconhecida_com_erro_claro() {
        let err = probe("nao-existe.txt").expect_err("precisa falhar");
        let text = err.to_string();
        assert!(text.contains("extensao nao suportada"), "{text}");
    }

    #[test]
    fn bench_recusa_arquivo_inexistente() {
        assert!(bench("nao-existe.mkv", Some(1.0)).is_err());
    }
}
