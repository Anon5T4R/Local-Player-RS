//! Contadores de desempenho do player (instrumentação permanente, não debug).
//!
//! Um [`Metrics`] é compartilhado (`Arc`) entre a thread do motor, a thread de
//! decode de vídeo e a UI. Tudo que importa para separar as hipóteses de
//! travada é medido aqui **em produção**:
//!
//! - latência de decode por frame (média da janela, p95, máximo) e fps de decode;
//! - frames exibidos/descartados e quantos foram descartados por atraso (> janela);
//! - atraso do frame apresentado em relação ao relógio mestre;
//! - profundidade do ring de vídeo da UI;
//! - custo do upload de textura e intervalo entre repaints;
//! - RSS do processo (e pico) e bytes no cache de frames do fork do FilmCraft.
//!
//! A tecla **D** mostra tudo num overlay técnico (inglês, monoespaçado: é
//! diagnóstico, não UI de produto). Com `LOCALPLAYER_STATS=<arquivo>` (ou
//! `LOCALPLAYER_STATS=1` para um arquivo no temp), uma linha CSV é gravada por
//! segundo — é o artefato que o João manda de volta.
//!
//! Sem `unwrap`/`panic`: mutexes envenenados são recuperados e leituras
//! impossíveis (RSS fora de Windows/Linux) viram `None`/0.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_codecs::hw::HwStats;

/// Amostras de latência de decode na janela do p95 (~10 s a 60 fps).
const WINDOW: usize = 512;
/// De quanto em quanto tempo as taxas (fps) são recalculadas.
const RATE_PERIOD: Duration = Duration::from_millis(250);
/// Uma linha de CSV por segundo.
const LOG_PERIOD: Duration = Duration::from_millis(1000);

/// Faixa aberta, do jeito que o overlay/CSV/probe reportam.
#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub container: String,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    /// bits/s declarado pelo container ou estimado por arquivo/duração.
    pub bitrate: Option<u64>,
    pub file_size: Option<u64>,
}

impl TrackInfo {
    /// Metadados de uma faixa aberta pelo hub do FilmCraft.
    pub fn from_media(info: &filmcraft_media::MediaInfo) -> Self {
        let video = info.video.as_ref();
        let bitrate = video.and_then(|v| v.bitrate).or_else(|| estimate_bitrate(info.file_size, info.duration.seconds()));
        Self {
            container: info.container.clone(),
            video_codec: video.map(|v| v.codec.clone()),
            audio_codec: info.audio().map(|a| a.codec.clone()),
            width: video.map_or(0, |v| v.width),
            height: video.map_or(0, |v| v.height),
            fps: video.map_or(0.0, |v| v.frame_rate.sane().as_f64()),
            duration: info.duration.seconds(),
            bitrate,
            file_size: info.file_size,
        }
    }
}

/// bits/s estimado de `file_size` e duração (o container nem sempre declara).
pub fn estimate_bitrate(file_size: Option<u64>, duration: f64) -> Option<u64> {
    let bytes = file_size?;
    if !duration.is_finite() || duration <= 0.0 {
        return None;
    }
    Some((bytes as f64 * 8.0 / duration) as u64)
}

/// Qual decoder está produzindo a imagem da faixa atual (overlay/log/bench).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HwMode {
    /// Sem faixa de vídeo aberta.
    #[default]
    Idle,
    /// O switch de hardware decoding está desligado.
    Off,
    /// O decoder de hardware pegou a faixa (nenhum quadro saiu ainda).
    Pending,
    /// Decodificando no hardware do sistema (Windows: Media Foundation / DXVA;
    /// Linux: VA-API / NVDEC).
    Hardware,
    /// O hardware começou e caiu para o decoder SW no meio do stream.
    HardwareThenSoftware,
    /// Decodificador pure-Rust (sem caminho de hardware para o codec, recusado, ou sem backend).
    Software,
}

impl HwMode {
    /// Texto curto do modo (coluna do overlay; o rótulo com backend fica em [`HwUsage::summary`]).
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "-",
            Self::Off => "off",
            Self::Pending => "pending",
            Self::Hardware => "hardware",
            Self::HardwareThenSoftware => "hardware → software",
            Self::Software => "software",
        }
    }
}

/// O decoder em uso na faixa atual. Os contadores são do hub (globais e
/// monotônicos); [`Metrics::set_track`] rebaseia a contagem a cada faixa, então
/// os campos são o que aconteceu **desde a abertura** da faixa.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HwUsage {
    pub mode: HwMode,
    /// Backend registrado no arranque ("Media Foundation", "VA-API", "NVDEC"),
    /// quando há.
    pub backend: Option<&'static str>,
    /// Quadros que saíram do decoder de hardware.
    pub frames: u64,
    /// Sessões de hardware criadas (uma por decoder; seek reusa).
    pub sessions: u64,
    /// Streams que a factory recusou e foram para o software.
    pub declined: u64,
    /// Decoders de hardware que falharam no meio e continuaram em software.
    pub fallbacks: u64,
}

impl HwUsage {
    /// Uma célula do overlay: `on/off` e **qual** decoder (MF vs SW).
    pub fn summary(&self) -> String {
        match self.mode {
            HwMode::Idle => "-".into(),
            HwMode::Off => "off".into(),
            HwMode::Pending => "pending".into(),
            HwMode::Hardware => format!("{} · hardware", self.backend.unwrap_or("hw")),
            HwMode::HardwareThenSoftware => format!("{} · hardware → software", self.backend.unwrap_or("hw")),
            HwMode::Software => "software".into(),
        }
    }
}

/// A escolha de decoder a partir dos contadores e do stream. Pura: a matriz é
/// testada sem GPU (o teste de integração real é o `--bench`).
///
/// `codec_takes_hardware` = a família do codec está entre as que o backend
/// decodifica desta plataforma (H.264/HEVC no Linux; + VP9/AV1 no Windows); o
/// perfil/bit depth ainda podem recusar, e aí o contador `declined` conta a
/// história.
fn hw_mode(policy: bool, backend: Option<&'static str>, codec_takes_hardware: bool, d: HwStats) -> HwMode {
    if !policy {
        return HwMode::Off;
    }
    if backend.is_none() {
        // Plataforma sem backend (macOS por enquanto, ou sem VA-API/NVDEC no
        // Linux) ou registro falhou.
        return HwMode::Software;
    }
    if d.fallbacks > 0 {
        return HwMode::HardwareThenSoftware;
    }
    if d.frames > 0 {
        return HwMode::Hardware;
    }
    if d.declined > 0 || !codec_takes_hardware {
        return HwMode::Software;
    }
    HwMode::Pending
}

pub(crate) fn hw_delta(before: HwStats, after: HwStats) -> HwStats {
    HwStats {
        frames: after.frames.saturating_sub(before.frames),
        sessions: after.sessions.saturating_sub(before.sessions),
        declined: after.declined.saturating_sub(before.declined),
        fallbacks: after.fallbacks.saturating_sub(before.fallbacks),
    }
}

/// Fotografia dos contadores para o overlay/CSV.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Frames decodificados por segundo (janela de ~250 ms).
    pub decode_fps: f64,
    /// Latência média de decode na janela (ms).
    pub decode_avg_ms: f64,
    /// p95 da latência de decode na janela (ms).
    pub decode_p95_ms: f64,
    /// Pior latência de decode desde o arranque (ms).
    pub decode_max_ms: f64,
    /// Frames exibidos por segundo (janela de ~250 ms).
    pub display_fps: f64,
    pub presented: u64,
    pub dropped: u64,
    pub dropped_late: u64,
    /// Atraso do último frame exibido vs relógio mestre (ms; negativo = na frente).
    pub late_ms: f64,
    pub ring_depth: usize,
    /// Custo do último upload de textura (ms).
    pub upload_ms: f64,
    pub upload_avg_ms: f64,
    pub upload_max_ms: f64,
    /// Intervalo entre repaints da UI (ms) e a taxa correspondente.
    pub repaint_ms: f64,
    pub repaint_fps: f64,
    pub rss_mb: f64,
    pub rss_peak_mb: f64,
    /// Bytes residentes no cache de frames decodificados do fork.
    pub cache_mb: f64,
    /// Segundos desde a criação da instância.
    pub elapsed_s: f64,
    pub track: Option<TrackInfo>,
    /// Decoder em uso na faixa (hwdec on/off e qual backend).
    pub hw: HwUsage,
}

impl Snapshot {
    /// Cabeçalho do CSV (uma linha por segundo quando `LOCALPLAYER_STATS` está setado).
    pub const CSV_HEADER: &'static str =
        "t_s,decode_fps,decode_avg_ms,decode_p95_ms,decode_max_ms,display_fps,presented,dropped,dropped_late,late_ms,ring,upload_ms,upload_avg_ms,repaint_ms,rss_mb,rss_peak_mb,cache_mb";

    /// Linha CSV do snapshot (sem quebra de linha).
    pub fn csv_line(&self) -> String {
        format!(
            "{:.1},{:.1},{:.2},{:.2},{:.2},{:.1},{},{},{},{:.1},{},{:.2},{:.2},{:.1},{:.1},{:.1},{:.1}",
            self.elapsed_s,
            self.decode_fps,
            self.decode_avg_ms,
            self.decode_p95_ms,
            self.decode_max_ms,
            self.display_fps,
            self.presented,
            self.dropped,
            self.dropped_late,
            self.late_ms,
            self.ring_depth,
            self.upload_ms,
            self.upload_avg_ms,
            self.repaint_ms,
            self.rss_mb,
            self.rss_peak_mb,
            self.cache_mb,
        )
    }
}

/// Estado mutável de amostragem (tudo que exige lock).
struct Rates {
    /// Últimas latências de decode (µs) para o p95.
    window: VecDeque<u32>,
    last: Instant,
    decode_base: u64,
    presented_base: u64,
    dropped_base: u64,
    decode_fps: f64,
    display_fps: f64,
    upload_avg_ms: f64,
    repaint_ms: f64,
    rss_mb: f64,
    rss_peak_mb: f64,
}

impl Rates {
    fn new(now: Instant) -> Self {
        Self {
            window: VecDeque::with_capacity(WINDOW),
            last: now,
            decode_base: 0,
            presented_base: 0,
            dropped_base: 0,
            decode_fps: 0.0,
            display_fps: 0.0,
            upload_avg_ms: 0.0,
            repaint_ms: 0.0,
            rss_mb: 0.0,
            rss_peak_mb: 0.0,
        }
    }
}

struct Log {
    file: BufWriter<File>,
    last: Instant,
}

/// Contadores compartilhados. Barato de clonar por `Arc`.
pub struct Metrics {
    started: Instant,
    decode_count: AtomicU64,
    decode_total_us: AtomicU64,
    decode_max_us: AtomicU64,
    presented: AtomicU64,
    dropped: AtomicU64,
    dropped_late: AtomicU64,
    upload_count: AtomicU64,
    upload_total_us: AtomicU64,
    upload_max_us: AtomicU64,
    upload_last_us: AtomicU64,
    ring_depth: AtomicUsize,
    /// Atraso do último frame exibido (µs; negativo = adiantado).
    late_us: AtomicI64,
    /// Último intervalo entre repaints (µs).
    repaint_us: AtomicU64,
    /// `elapsed().as_nanos()` do último repaint (0 = primeiro ainda).
    repaint_last_ns: AtomicU64,
    track: Mutex<Option<TrackInfo>>,
    /// Contadores de hardware no momento em que a faixa abriu (as taxas por faixa).
    hw_base: Mutex<HwStats>,
    rates: Mutex<Rates>,
    log: Mutex<Option<Log>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    /// Cria os contadores e, se `LOCALPLAYER_STATS` estiver setado, o CSV.
    pub fn new() -> Self {
        let now = Instant::now();
        let log = open_log(now);
        Self {
            started: now,
            decode_count: AtomicU64::new(0),
            decode_total_us: AtomicU64::new(0),
            decode_max_us: AtomicU64::new(0),
            presented: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            dropped_late: AtomicU64::new(0),
            upload_count: AtomicU64::new(0),
            upload_total_us: AtomicU64::new(0),
            upload_max_us: AtomicU64::new(0),
            upload_last_us: AtomicU64::new(0),
            ring_depth: AtomicUsize::new(0),
            late_us: AtomicI64::new(0),
            repaint_us: AtomicU64::new(0),
            repaint_last_ns: AtomicU64::new(0),
            track: Mutex::new(None),
            hw_base: Mutex::new(HwStats::default()),
            rates: Mutex::new(Rates::new(now)),
            log: Mutex::new(log),
        }
    }

    fn lock_rates(&self) -> MutexGuard<'_, Rates> {
        self.rates.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_log(&self) -> MutexGuard<'_, Option<Log>> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Faixa aberta (metadados para overlay/CSV). Rebaseia os contadores de
    /// hardware: os do hub são globais e monotônicos.
    pub fn set_track(&self, info: TrackInfo) {
        *self.track.lock().unwrap_or_else(PoisonError::into_inner) = Some(info);
        *self.hw_base.lock().unwrap_or_else(PoisonError::into_inner) = filmcraft_codecs::hw::hw_stats();
    }

    /// Decoder em uso na faixa atual (barato: só átomos + o track).
    pub fn hw_usage(&self) -> HwUsage {
        let delta = hw_delta(*self.hw_base.lock().unwrap_or_else(PoisonError::into_inner), filmcraft_codecs::hw::hw_stats());
        let backend = filmcraft_codecs::hw::hw_backend();
        let track = self.track.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let video = track.as_ref().and_then(|t| t.video_codec.as_deref());
        let mode = match video {
            None => HwMode::Idle,
            Some(codec) => {
                let takes = filmcraft_hwdec::codec::taken(codec);
                hw_mode(filmcraft_codecs::hw::hardware_decoding(), backend, takes, delta)
            }
        };
        HwUsage { mode, backend, frames: delta.frames, sessions: delta.sessions, declined: delta.declined, fallbacks: delta.fallbacks }
    }

    /// Uma requisição de frame ao decoder terminou (`elapsed` = decode + planos).
    pub fn note_decoded(&self, elapsed: Duration) {
        let us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self.decode_count.fetch_add(1, Ordering::Relaxed);
        self.decode_total_us.fetch_add(us, Ordering::Relaxed);
        self.decode_max_us.fetch_max(us, Ordering::Relaxed);
        let mut r = self.lock_rates();
        if r.window.len() == WINDOW {
            r.window.pop_front();
        }
        r.window.push_back(us.min(u64::from(u32::MAX)) as u32);
    }

    /// Um frame foi entregue à GPU.
    pub fn note_presented(&self) {
        self.presented.fetch_add(1, Ordering::Relaxed);
    }

    /// `late` frames foram descartados; `late_dropped` deles estavam atrasados
    /// além da janela do apresentador.
    pub fn note_dropped(&self, late: u64, late_dropped: u64) {
        self.dropped.fetch_add(late, Ordering::Relaxed);
        self.dropped_late.fetch_add(late_dropped, Ordering::Relaxed);
    }

    /// Profundidade do ring de vídeo que a UI segura agora.
    pub fn set_ring_depth(&self, depth: usize) {
        self.ring_depth.store(depth, Ordering::Relaxed);
    }

    /// Atraso (s) do frame exibido vs relógio; negativo = adiantado.
    pub fn note_late(&self, secs: f64) {
        let us = if secs.is_finite() { (secs * 1e6).clamp(-1e9, 1e9) as i64 } else { 0 };
        self.late_us.store(us, Ordering::Relaxed);
    }

    /// Custo do upload do último frame (ms).
    pub fn note_upload(&self, elapsed: Duration) {
        let us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self.upload_count.fetch_add(1, Ordering::Relaxed);
        self.upload_total_us.fetch_add(us, Ordering::Relaxed);
        self.upload_max_us.fetch_max(us, Ordering::Relaxed);
        self.upload_last_us.store(us, Ordering::Relaxed);
    }

    /// Um repaint da UI começou (chamado uma vez por frame).
    pub fn note_repaint(&self) {
        let now_ns = self.started.elapsed().as_nanos() as u64;
        let prev = self.repaint_last_ns.swap(now_ns, Ordering::Relaxed);
        if prev != 0 && now_ns > prev {
            self.repaint_us.store((now_ns - prev) / 1_000, Ordering::Relaxed);
        }
    }

    /// Recálculo periódico das taxas + gravação do CSV (chamado pela UI).
    pub fn tick(&self, now: Instant) {
        let refresh = now.saturating_duration_since(self.lock_rates().last) >= RATE_PERIOD;
        if refresh {
            let (rss, peak) = (rss_bytes(), rss_peak_bytes());
            {
                let mut r = self.lock_rates();
                let elapsed = now.saturating_duration_since(r.last);
                if elapsed >= RATE_PERIOD {
                    let secs = elapsed.as_secs_f64();
                    let decode = self.decode_count.load(Ordering::Relaxed);
                    let presented = self.presented.load(Ordering::Relaxed);
                    let dropped = self.dropped.load(Ordering::Relaxed);
                    if secs > 0.0 {
                        r.decode_fps = decode.saturating_sub(r.decode_base) as f64 / secs;
                        r.display_fps = presented.saturating_sub(r.presented_base) as f64 / secs;
                    }
                    r.decode_base = decode;
                    r.presented_base = presented;
                    r.dropped_base = dropped;
                    r.rss_mb = rss.map_or(0.0, bytes_to_mb);
                    r.rss_peak_mb = peak.map_or(0.0, bytes_to_mb);
                    let uploads = self.upload_count.load(Ordering::Relaxed);
                    if uploads > 0 {
                        r.upload_avg_ms = self.upload_total_us.load(Ordering::Relaxed) as f64 / 1e3 / uploads as f64;
                    }
                    let repaint = self.repaint_us.load(Ordering::Relaxed) as f64 / 1e3;
                    // Intervalo absurdo (pausa longa/primeiro frame) não entra na média.
                    if repaint > 0.0 && repaint < 1_000.0 {
                        r.repaint_ms = if r.repaint_ms > 0.0 { r.repaint_ms * 0.8 + repaint * 0.2 } else { repaint };
                    }
                    r.last = now;
                }
            }
        }

        // Uma linha por segundo: o artefato que volta para diagnóstico.
        let mut log = self.lock_log();
        if let Some(l) = log.as_mut() {
            if now.saturating_duration_since(l.last) >= LOG_PERIOD {
                let snap = self.snapshot();
                if writeln!(l.file, "{}", snap.csv_line()).is_ok() {
                    let _ = l.file.flush();
                }
                l.last = now;
            }
        }
    }

    /// Fotografia dos contadores (não recalcula taxas: `tick` faz isso).
    pub fn snapshot(&self) -> Snapshot {
        let (avg_window, p95) = {
            let r = self.lock_rates();
            let n = r.window.len();
            if n == 0 {
                (0.0, 0.0)
            } else {
                let mut v: Vec<u32> = r.window.iter().copied().collect();
                v.sort_unstable();
                let avg = v.iter().map(|x| f64::from(*x)).sum::<f64>() / n as f64 / 1e3;
                (avg, percentile_ms(&v, 0.95))
            }
        };
        let rates = self.lock_rates();
        let uploads = self.upload_count.load(Ordering::Relaxed);
        let upload_last = self.upload_last_us.load(Ordering::Relaxed) as f64 / 1e3;
        let upload_max = self.upload_max_us.load(Ordering::Relaxed) as f64 / 1e3;
        let repaint_ms = rates.repaint_ms;
        // Fora do literal: um `MutexGuard` temporário de campo vive até o fim do
        // statement, e `hw_usage()` pega o mesmo lock do track de novo.
        let track = self.track.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let hw = self.hw_usage();
        Snapshot {
            decode_fps: rates.decode_fps,
            decode_avg_ms: avg_window,
            decode_p95_ms: p95,
            decode_max_ms: self.decode_max_us.load(Ordering::Relaxed) as f64 / 1e3,
            display_fps: rates.display_fps,
            presented: self.presented.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            dropped_late: self.dropped_late.load(Ordering::Relaxed),
            late_ms: self.late_us.load(Ordering::Relaxed) as f64 / 1e3,
            ring_depth: self.ring_depth.load(Ordering::Relaxed),
            upload_ms: upload_last,
            upload_avg_ms: if uploads > 0 { rates.upload_avg_ms } else { 0.0 },
            upload_max_ms: upload_max,
            repaint_ms,
            repaint_fps: if repaint_ms > 0.0 { 1_000.0 / repaint_ms } else { 0.0 },
            rss_mb: rates.rss_mb,
            rss_peak_mb: rates.rss_peak_mb,
            cache_mb: bytes_to_mb(filmcraft_codecs::cached_bytes() as u64),
            elapsed_s: self.started.elapsed().as_secs_f64(),
            track,
            hw,
        }
    }
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// (RSS atual, pico) do processo em MB — usado pelo `--bench`.
pub fn rss_mb() -> (f64, f64) {
    (rss_bytes().map_or(0.0, bytes_to_mb), rss_peak_bytes().map_or(0.0, bytes_to_mb))
}

/// p95 (ou outro quantil) de uma janela já ordenada, em ms.
fn percentile_ms(sorted: &[u32], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    f64::from(sorted[idx.min(sorted.len() - 1)]) / 1e3
}

/// Abre o CSV de `LOCALPLAYER_STATS` (`1` = arquivo no temp).
fn open_log(now: Instant) -> Option<Log> {
    let raw = std::env::var("LOCALPLAYER_STATS").ok()?;
    let raw = raw.trim();
    if raw.is_empty() || raw == "0" {
        return None;
    }
    let path =
        if raw == "1" { std::env::temp_dir().join(format!("localplayer-stats-{}.csv", std::process::id())) } else { std::path::PathBuf::from(raw) };
    match File::create(&path) {
        Ok(f) => {
            let mut file = BufWriter::new(f);
            let _ = writeln!(file, "{}", Snapshot::CSV_HEADER);
            let _ = file.flush();
            eprintln!("stats: gravando em {}", path.display());
            Some(Log { file, last: now })
        }
        Err(e) => {
            eprintln!("stats: nao consegui abrir {}: {e}", path.display());
            None
        }
    }
}

/// RSS atual do processo (Windows: working set; Linux: VmRSS).
#[cfg(windows)]
fn rss_bytes() -> Option<u64> {
    process_memory().map(|(ws, _)| ws as u64)
}

/// Pico de RSS do processo desde o arranque.
#[cfg(windows)]
fn rss_peak_bytes() -> Option<u64> {
    process_memory().map(|(_, peak)| peak as u64)
}

#[cfg(windows)]
fn process_memory() -> Option<(usize, usize)> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    // SAFETY: `counters` é um struct válido do tamanho declarado em `cb` e o
    // pseudo-handle do processo atual é sempre válido.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    ok.ok().map(|_| (counters.WorkingSetSize, counters.PeakWorkingSetSize))
}

/// Leitura de um campo `kB` de `/proc/self/status` (Linux).
#[cfg(target_os = "linux")]
fn status_kb(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with(field))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(target_os = "linux")]
fn rss_bytes() -> Option<u64> {
    status_kb("VmRSS:")
}

#[cfg(target_os = "linux")]
fn rss_peak_bytes() -> Option<u64> {
    status_kb("VmHWM:")
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rss_bytes() -> Option<u64> {
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rss_peak_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentil_usa_a_janela_ordenada() {
        // A janela guarda µs e o percentil sai em ms.
        let v: Vec<u32> = (1..=100).collect();
        assert!((percentile_ms(&v, 0.95) - 0.095).abs() < 1e-9);
        assert!((percentile_ms(&v, 0.5) - 0.051).abs() < 1e-9);
        assert_eq!(percentile_ms(&[], 0.95), 0.0);
        assert!((percentile_ms(&[7], 0.95) - 0.007).abs() < 1e-12);
    }

    #[test]
    fn contadores_refletem_as_notas() {
        let m = Metrics::new();
        m.note_decoded(Duration::from_millis(2));
        m.note_decoded(Duration::from_millis(4));
        m.note_presented();
        m.note_dropped(3, 2);
        m.note_late(0.045);
        m.note_upload(Duration::from_millis(1));
        m.set_ring_depth(5);
        let s = m.snapshot();
        assert!((s.decode_avg_ms - 3.0).abs() < 1e-9, "{}", s.decode_avg_ms);
        assert!((s.decode_max_ms - 4.0).abs() < 1e-9);
        assert_eq!((s.presented, s.dropped, s.dropped_late), (1, 3, 2));
        assert!((s.late_ms - 45.0).abs() < 1e-6);
        assert!((s.upload_ms - 1.0).abs() < 1e-9);
        assert_eq!(s.ring_depth, 5);
    }

    #[test]
    fn csv_tem_cabecalho_e_uma_linha_por_snapshot() {
        let snap = Snapshot { decode_fps: 24.0, presented: 2, dropped: 1, ring_depth: 3, rss_mb: 512.0, ..Snapshot::default() };
        let line = snap.csv_line();
        let fields: Vec<&str> = line.split(',').collect();
        assert_eq!(fields.len(), Snapshot::CSV_HEADER.split(',').count());
        assert_eq!(fields[1], "24.0");
        assert_eq!((fields[6], fields[7]), ("2", "1"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn bitrate_estimado_por_arquivo() {
        // 1 MB em 8 s = 1 Mbit/s.
        assert_eq!(estimate_bitrate(Some(1 << 20), 8.0), Some(1 << 20));
        assert_eq!(estimate_bitrate(Some(1 << 20), 0.0), None);
        assert_eq!(estimate_bitrate(None, 10.0), None);
    }

    #[test]
    fn modo_do_hwdec_segue_politica_codec_e_contadores() {
        let s = |frames, sessions, declined, fallbacks| HwStats { frames, sessions, declined, fallbacks };
        // Switch off vence tudo.
        assert_eq!(hw_mode(false, Some("Media Foundation"), true, s(100, 1, 0, 0)), HwMode::Off);
        // Sem backend registrado (macOS por enquanto, ou sem VA-API/NVDEC): software.
        assert_eq!(hw_mode(true, None, true, s(0, 0, 0, 0)), HwMode::Software);
        // Codec fora do backend: software desde já (sem esperar o primeiro frame).
        assert_eq!(hw_mode(true, Some("Media Foundation"), false, s(0, 0, 0, 0)), HwMode::Software);
        // Faixa H.264 e nada decodeado ainda: pendente.
        assert_eq!(hw_mode(true, Some("Media Foundation"), true, s(0, 0, 0, 0)), HwMode::Pending);
        // Sessão criada, quadros saindo: hardware.
        assert_eq!(hw_mode(true, Some("Media Foundation"), true, s(42, 1, 0, 0)), HwMode::Hardware);
        // Hardware falhou no meio: hardware → software.
        assert_eq!(hw_mode(true, Some("Media Foundation"), true, s(42, 1, 0, 1)), HwMode::HardwareThenSoftware);
        // A factory recusou o stream (perfil/tamanho): software.
        assert_eq!(hw_mode(true, Some("Media Foundation"), true, s(0, 0, 1, 0)), HwMode::Software);
    }

    #[test]
    fn uso_do_hwdec_resume_o_modo() {
        let usage = HwUsage { mode: HwMode::Hardware, backend: Some("Media Foundation"), frames: 7, sessions: 1, declined: 0, fallbacks: 0 };
        assert_eq!(usage.summary(), "Media Foundation · hardware");
        assert_eq!(HwUsage { mode: HwMode::Software, ..usage.clone() }.summary(), "software");
        assert_eq!(HwUsage { mode: HwMode::Idle, ..usage }.summary(), "-");
    }
}
