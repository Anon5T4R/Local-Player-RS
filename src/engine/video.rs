//! Pipeline de vídeo (fase 3): container → decode em thread própria → frames.
//!
//! - **Demux + decode** pelo hub do FilmCraft (`filmcraft-codecs`): MP4/MOV
//!   (H.264/HEVC), MKV/WebM (VP8/VP9/AV1/H.264) e o que mais o hub abrir. O
//!   mesmo `SharedSource` serve o áudio ([`MediaAudio`]) e o vídeo, então o
//!   arquivo é aberto uma vez só.
//! - **Thread própria** ("video-decode"): pede frames ao hub por PTS e os envia
//!   por um canal **síncrono de 4 posições** — a thread para quando a UI não
//!   consome (pausa) e o ring nunca acumula 1080p/4K em memória.
//! - **Seek** é um comando com geração: frames de uma geração antiga são
//!   descartados pela UI. O hub acha o sync sample anterior (GOP-aware) sozinho.
//! - **Fim**: quando a posição passa da duração (ou o decoder devolve o mesmo
//!   frame por 5 s com duração desconhecida) a thread manda `End` e dorme.
//!
//! O áudio de arquivos de vídeo sai por aqui também: `MediaSource::audio`
//! decodifica AAC/MP3/Opus/AC-3/E-AC-3/FLAC/Vorbis/PCM com o próprio container,
//! já reamostrado para a taxa do device (o ring da fase 2 não muda).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fmt, path::Path};

use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_media::{FrameRequest, SharedReader, SharedSource};
use filmcraft_time::Tick;

use crate::metrics::Metrics;
use crate::video::Frame;

use super::decoder::{AudioSource, Chunk};
use super::error::EngineError;
use super::Clock;

/// Maior lado aceito no decode/render (igual ao limite do render).
pub const MAX_DIMENSION: u32 = crate::video::MAX_DIMENSION;
/// Frames que o decode pode adiantar antes de esperar a UI.
const RING: usize = 4;
/// 100 ms de áudio por chamada a 48 kHz (o ring da fase 2 reamostra/empacota).
const AUDIO_CHUNK_FRAMES: usize = 4_800;
/// Quantos frames repetidos (posição andando) contam como "acabou" quando o
/// container não declara duração: 5 s de vídeo.
const UNKNOWN_DURATION_REPEAT_SECS: f64 = 5.0;
/// Quanto de vídeo o decode pode decodificar **à frente** do relógio mestre.
///
/// Sem isso a thread decodifica o arquivo inteiro o mais rápido que o CPU
/// permite (o canal de 4 não segura nada: a UI drena tudo a cada repaint) e
/// empurra frames para o heap/cache sem relação com o playback — a fonte da
/// pressão de memória e da disputa de CPU com a UI (H1/H2/H3). 250 ms é folga
/// suficiente para o jitter do relógio de áudio e limita o heap.
const DECODE_LEAD_SECS: f64 = 0.25;
/// Passo de espera do pacing: curto o bastante para um seek/stop responder já.
const PACE_POLL: Duration = Duration::from_millis(20);

/// Metadados do vídeo de um container, do jeito que a UI precisa.
#[derive(Clone, Copy, Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    /// Quadros por segundo (nominal do container).
    pub fps: f64,
    pub duration: f64,
    /// Pixel aspect ratio (n, d); (0,0) = quadrado.
    pub par: (u32, u32),
}

impl VideoInfo {
    pub fn from_media(info: &filmcraft_media::MediaInfo) -> Option<Self> {
        let v = info.video.as_ref()?;
        Some(Self { width: v.width, height: v.height, fps: v.frame_rate.sane().as_f64(), duration: info.duration.seconds(), par: v.par })
    }
}

/// Extensões de container que o pipeline (e o seu áudio) abre pelo FilmCraft.
pub(super) fn is_video_container(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "m2ts" | "mts" | "ts")
}

/// Abre um container pelo hub, lendo o arquivo sob demanda (índice agora,
/// samples quando pedidos — não carrega o arquivo inteiro na memória).
pub(super) fn open_media(path: &str) -> Result<SharedSource, EngineError> {
    let file = filmcraft_media::reader::FileReader::open(Path::new(path)).map_err(|e| EngineError::Open(format!("{path}: {e}")))?;
    let reader: SharedReader = Arc::new(file);
    filmcraft_media::reader::open_reader(path, reader, &filmcraft_codecs::reader_openers(), &filmcraft_codecs::openers())
        .map_err(|e| EngineError::Open(e.to_string()))
}

// ── áudio do container ───────────────────────────────────────────────────

/// Faixa de áudio do container via hub: janelas aleatórias na taxa do device.
pub(super) struct MediaAudio {
    source: SharedSource,
    rate: u32,
    /// Total de frames na taxa do device (`i64::MAX` quando a duração é desconhecida).
    total: i64,
    pos: i64,
}

impl MediaAudio {
    pub(super) fn open(source: SharedSource, rate: u32, duration: f64) -> Result<Box<dyn AudioSource>, EngineError> {
        if source.info().audio().is_none() {
            return Err(EngineError::Open("no audio track".into()));
        }
        if rate == 0 {
            return Err(EngineError::NoDevice("device sample rate is zero".into()));
        }
        let total = if duration.is_finite() && duration > 0.0 { (duration * f64::from(rate)) as i64 } else { i64::MAX };
        Ok(Box::new(Self { source, rate, total: total.max(1), pos: 0 }))
    }
}

impl AudioSource for MediaAudio {
    fn next_chunk(&mut self) -> Result<Option<Chunk>, String> {
        if self.pos >= self.total {
            return Ok(None);
        }
        let frames = (self.total - self.pos).min(AUDIO_CHUNK_FRAMES as i64).max(0) as usize;
        if frames == 0 {
            return Ok(None);
        }
        let buf = self.source.audio(self.pos, frames, self.rate).map_err(|e| e.to_string())?;
        self.pos += frames as i64;
        let channels = buf.channel_count();
        let n = buf.frames();
        if channels == 0 || n == 0 {
            return Ok(None);
        }
        // Planar → entrelaçado: o motor converte para stereo (`decoder::to_stereo`).
        let mut samples = Vec::with_capacity(n * channels);
        for i in 0..n {
            for ch in &buf.channels {
                samples.push(ch.get(i).copied().unwrap_or(0.0));
            }
        }
        Ok(Some(Chunk { rate: self.rate, channels, samples }))
    }

    fn seek(&mut self, secs: f64) -> Result<(), String> {
        let s = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        let frame = (s * f64::from(self.rate)).round();
        self.pos = (frame as i64).clamp(0, self.total);
        Ok(())
    }

    fn duration(&self) -> Option<f64> {
        (self.total != i64::MAX && self.rate > 0).then(|| self.total as f64 / f64::from(self.rate))
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

// ── pipeline de vídeo ────────────────────────────────────────────────────

enum VideoCmd {
    Seek { secs: f64, generation: u64 },
    Stop,
}

/// Mensagem da thread de decode para a UI.
#[derive(Debug)]
pub enum VideoMsg {
    Frame {
        frame: Frame,
        generation: u64,
    },
    /// Chegou ao fim do vídeo (a UI avança a playlist quando não há áudio).
    End {
        generation: u64,
    },
    /// Falha de decode/seek: a UI mostra o erro e pula a faixa.
    Error {
        generation: u64,
        detail: String,
    },
}

/// Handle da UI para o pipeline de uma faixa.
pub struct VideoPipeline {
    pub info: VideoInfo,
    /// A faixa também tem áudio: o fim quem decide é o ring de áudio.
    pub has_audio: bool,
    /// Geração corrente de seek (mensagens de gerações antigas são velhas).
    pub generation: u64,
    cmd: Sender<VideoCmd>,
    rx: Receiver<VideoMsg>,
}

impl fmt::Debug for VideoPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoPipeline").field("info", &self.info).field("has_audio", &self.has_audio).finish_non_exhaustive()
    }
}

impl VideoPipeline {
    /// Seek: incrementa a geração, manda a thread reposicionar e descarta o que
    /// ainda estava no canal (frames velhos).
    pub fn seek(&mut self, secs: f64) {
        self.generation = self.generation.wrapping_add(1);
        let _ = self.cmd.send(VideoCmd::Seek { secs: if secs.is_finite() { secs.max(0.0) } else { 0.0 }, generation: self.generation });
        self.drain();
    }

    /// Próxima mensagem (não bloqueia).
    pub fn try_recv(&self) -> Option<VideoMsg> {
        self.rx.try_recv().ok()
    }

    /// Descarta o que está na fila — chamado no seek (o decode pode estar
    /// bloqueado no canal cheio e só vê o comando depois de liberar espaço).
    pub fn drain(&self) {
        while self.rx.try_recv().is_ok() {}
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        let _ = self.cmd.send(VideoCmd::Stop);
    }
}

/// Sobe a thread de decode de `source` e devolve o handle da UI.
///
/// `metrics` recebe os contadores de decode; `clock` é o relógio mestre: a
/// thread só decodifica até [`DECODE_LEAD_SECS`] à frente dele (pacing).
pub(super) fn spawn(
    source: &SharedSource,
    info: VideoInfo,
    has_audio: bool,
    start: f64,
    metrics: Arc<Metrics>,
    clock: Arc<Clock>,
) -> Result<VideoPipeline, EngineError> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (frame_tx, frame_rx) = sync_channel(RING);
    let src = source.clone();
    let io = Iface { rx: cmd_rx, tx: frame_tx, name: source.info().name.clone(), metrics, clock };
    let spawned = std::thread::Builder::new().name("video-decode".into()).spawn(move || worker(src, info, start, io));
    match spawned {
        Ok(_) => Ok(VideoPipeline { info, has_audio, generation: 0, cmd: cmd_tx, rx: frame_rx }),
        Err(e) => Err(EngineError::Open(format!("video thread: {e}"))),
    }
}

struct Playback {
    generation: u64,
    pos: f64,
    /// `false` depois do `End` (só um Seek revive).
    live: bool,
}

fn apply(cmd: VideoCmd, state: &mut Playback, last: &mut Option<Arc<VideoFrame>>) -> bool {
    match cmd {
        VideoCmd::Seek { secs, generation } => {
            state.generation = generation;
            state.pos = secs.max(0.0);
            state.live = true;
            *last = None;
            false
        }
        VideoCmd::Stop => true,
    }
}

/// Drena os comandos pendentes; `true` = parar a thread.
fn drain_commands(rx: &Receiver<VideoCmd>, state: &mut Playback, last: &mut Option<Arc<VideoFrame>>) -> bool {
    loop {
        match rx.try_recv() {
            Ok(cmd) => {
                if apply(cmd, state, last) {
                    return true;
                }
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

/// Resultado do pacing de um frame.
enum Pace {
    /// Pode decodificar: o frame está dentro da janela do relógio.
    Ready,
    /// Um comando mexeu no estado (seek): reavaliar pos/geração no loop.
    Restart,
    /// A thread deve terminar (Stop ou UI fechou).
    Stop,
}

/// Segura a thread até o frame de `pts` estar a menos de [`DECODE_LEAD_SECS`] do
/// relógio mestre. Enquanto espera, os comandos (seek/stop) são atendidos.
fn pace(clock: &Clock, pts: f64, rx: &Receiver<VideoCmd>, state: &mut Playback, last: &mut Option<Arc<VideoFrame>>) -> Pace {
    loop {
        let ahead = pts - clock.position();
        if !ahead.is_finite() || ahead <= DECODE_LEAD_SECS {
            return Pace::Ready;
        }
        let wait = Duration::from_secs_f64((ahead - DECODE_LEAD_SECS).clamp(0.005, PACE_POLL.as_secs_f64()));
        match rx.recv_timeout(wait) {
            Ok(cmd) => {
                // Um Seek muda a posição: voltar ao loop para reavaliar. Stop sai.
                return if apply(cmd, state, last) { Pace::Stop } else { Pace::Restart };
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Pace::Stop,
        }
    }
}

/// Windows: decode abaixo da UI na prioridade da thread (sem isso o decode,
/// que satura os cores, esfomeia o event loop da janela).
#[cfg(windows)]
fn lower_thread_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: pseudo-handle da thread atual, sempre válido no contexto chamado.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
}

#[cfg(not(windows))]
fn lower_thread_priority() {}

/// Canal da UI + instrumentação e relógio: o que o worker precisa além do
/// source e da faixa.
struct Iface {
    rx: Receiver<VideoCmd>,
    tx: SyncSender<VideoMsg>,
    name: String,
    metrics: Arc<Metrics>,
    clock: Arc<Clock>,
}

fn worker(source: SharedSource, info: VideoInfo, start: f64, io: Iface) {
    let Iface { rx, tx, name, metrics, clock } = io;
    lower_thread_priority();
    let step = if info.fps.is_finite() && info.fps > 0.0 { 1.0 / info.fps } else { 1.0 / 30.0 };
    let repeat_limit = (UNKNOWN_DURATION_REPEAT_SECS / step).ceil().max(1.0) as usize;
    let mut state = Playback { generation: 0, pos: start.max(0.0), live: true };
    let mut last: Option<Arc<VideoFrame>> = None;
    let mut repeats = 0usize;

    loop {
        let generation_before = state.generation;
        if drain_commands(&rx, &mut state, &mut last) {
            return;
        }
        if state.generation != generation_before {
            repeats = 0;
        }
        if !state.live {
            // Fim do vídeo: dorme até um Seek (nada de girar CPU).
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(cmd) => {
                    if apply(cmd, &mut state, &mut last) {
                        return;
                    }
                    repeats = 0;
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        // Pacing: nunca decodificar muito à frente do relógio mestre (memória e
        // CPU ficam proporcionais ao playback, não ao throughput do decoder).
        match pace(&clock, state.pos, &rx, &mut state, &mut last) {
            Pace::Ready => {}
            Pace::Restart => {
                repeats = 0;
                continue;
            }
            Pace::Stop => return,
        }

        let tick = Tick::from_seconds_f64(state.pos);
        let started = Instant::now();
        let decoded = catch_unwind(AssertUnwindSafe(|| source.video_frame(FrameRequest::full(tick))));
        let frame = match decoded {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                let _ = tx.send(VideoMsg::Error { generation: state.generation, detail: format!("{name}: {e}") });
                return;
            }
            Err(_) => {
                // Panic de biblioteca (arquivo corrompido) não mata a UI: vira erro.
                let _ = tx.send(VideoMsg::Error { generation: state.generation, detail: format!("{name}: decoder panicked") });
                return;
            }
        };
        let same = last.as_ref().is_some_and(|l| Arc::ptr_eq(l, &frame));
        if same {
            repeats += 1;
            metrics.note_decoded(started.elapsed());
        } else {
            repeats = 0;
            // Latência percebida do frame: pedido ao hub + planos prontos para a GPU.
            match frame_planes(&frame, state.pos) {
                Ok(planes) => {
                    metrics.note_decoded(started.elapsed());
                    last = Some(frame);
                    if tx.send(VideoMsg::Frame { frame: planes, generation: state.generation }).is_err() {
                        return; // UI fechou
                    }
                }
                Err(e) => {
                    metrics.note_decoded(started.elapsed());
                    let _ = tx.send(VideoMsg::Error { generation: state.generation, detail: format!("{name}: {e}") });
                    return;
                }
            }
        }

        state.pos += step;
        let over = info.duration > 0.0 && state.pos >= info.duration;
        if over || repeats >= repeat_limit {
            let _ = tx.send(VideoMsg::End { generation: state.generation });
            state.live = false;
        }
    }
}

/// Extrai os planos YUV8 do frame do hub (Yuv16 vira 8 bits aqui, na thread de
/// decode; RGBA não é suportado na saída de vídeo da fase 3).
fn frame_planes(frame: &VideoFrame, pts: f64) -> Result<Frame, String> {
    let (width, height) = (frame.width, frame.height);
    match &frame.data {
        PixelData::Yuv8 { planes, chroma, .. } => Ok(Frame { width, height, chroma: *chroma, planes: planes.clone(), color: frame.color, pts }),
        PixelData::Yuv16 { planes, chroma, bits, .. } => Ok(Frame {
            width,
            height,
            chroma: *chroma,
            planes: [Arc::new(shift_to_8(&planes[0], *bits)), Arc::new(shift_to_8(&planes[1], *bits)), Arc::new(shift_to_8(&planes[2], *bits))],
            color: frame.color,
            pts,
        }),
        PixelData::Rgba8(_) | PixelData::RgbaF32(_) => Err(format!("unsupported pixel format ({}×{height} RGBA)", width)),
    }
}

/// `u16` de `bits` bits → `u8` (mantém os 8 bits altos).
fn shift_to_8(plane: &[u16], bits: u32) -> Vec<u8> {
    let shift = bits.saturating_sub(8).min(8);
    plane.iter().map(|v| (*v >> shift).min(u8::MAX as u16) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifica_containers() {
        for p in ["a.mp4", "A.MOV", "b.mkv", "c.webm", "d.m4v", "e.avi"] {
            assert!(is_video_container(p), "{p}");
        }
        for p in ["a.mp3", "a.flac", "a.opus", "a.m4a", "a.wav", "sem-extensao"] {
            assert!(!is_video_container(p), "{p}");
        }
    }

    #[test]
    fn yuv16_desce_para_8_bits() {
        // 10 bits: 0, 512 e 1023 → 0, 128, 255.
        assert_eq!(shift_to_8(&[0, 512, 1023], 10), vec![0, 128, 255]);
        // 8 bits passa direto.
        assert_eq!(shift_to_8(&[1, 200, 255], 8), vec![1, 200, 255]);
        // valores acima do esperado não estouram o u8.
        assert_eq!(shift_to_8(&[65_535], 16), vec![255]);
    }

    /// Gera um Y4M minúsculo (4:2:0) com `frames` quadros e grava no temp.
    fn y4m_no_temp(w: u32, h: u32, frames: usize, tag: &str) -> String {
        let planos = (w * h + 2 * ((w.div_ceil(2)) * (h.div_ceil(2)))) as usize;
        let mut file = format!("YUV4MPEG2 W{w} H{h} F25:1 Ip A1:1 C420\n").into_bytes();
        for i in 0..frames {
            file.extend_from_slice(b"FRAME\n");
            let mut frame = vec![(i * 16) as u8; planos];
            frame[0] = 255;
            file.extend_from_slice(&frame);
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("lp-rs-video-{tag}-{}-{nanos}.y4m", std::process::id()));
        assert!(std::fs::write(&path, &file).is_ok(), "gravar Y4M de teste");
        path.to_string_lossy().to_string()
    }

    /// Relógio mestre de teste com a posição já no fim: nada de pacing.
    fn free_clock() -> Arc<Clock> {
        let clock = Arc::new(Clock::default());
        clock.publish(1_000_000.0);
        clock
    }

    /// Gera um Y4M minúsculo (4:2:0, 3 frames) e exercita o caminho real
    /// abrir → pedir frames → planos YUV8 → 3 frames em sequência.
    #[test]
    fn abre_e_decodifica_um_y4m_pequeno() {
        let (w, h) = (32u32, 24u32);
        let name = y4m_no_temp(w, h, 3, "small");

        let source = open_media(&name).expect("abrir Y4M");
        let info = source.info();
        let vinfo = VideoInfo::from_media(info).expect("vídeo");
        assert_eq!((vinfo.width, vinfo.height), (w, h));
        assert!((vinfo.fps - 25.0).abs() < 1e-9, "{vinfo:?}");

        let pipeline = spawn(&source, vinfo, false, 0.0, Arc::new(Metrics::new()), free_clock()).expect("spawn");
        let mut frames = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while frames < 3 && std::time::Instant::now() < deadline {
            match pipeline.rx.recv_timeout(Duration::from_millis(500)) {
                Ok(VideoMsg::Frame { frame, generation }) => {
                    assert_eq!(generation, 0);
                    assert_eq!((frame.width, frame.height), (w, h));
                    assert_eq!(frame.planes[0].len(), (w * h) as usize);
                    assert_eq!(frame.planes[1].len(), ((w.div_ceil(2)) * h.div_ceil(2)) as usize);
                    frames += 1;
                }
                Ok(VideoMsg::End { .. }) => break,
                Ok(VideoMsg::Error { detail, .. }) => panic!("decodificação falhou: {detail}"),
                Err(e) => panic!("sem frame do worker: {e}"),
            }
        }
        let _ = std::fs::remove_file(&name);
        assert_eq!(frames, 3, "três frames do Y4M");
    }

    /// Pacing: com o relógio parado o decode não pode decodificar o arquivo
    /// inteiro (é isso que enchia a memória e esfomeava a UI). Avançar o
    /// relógio libera os frames.
    #[test]
    fn pacing_segura_o_decode_no_relogio() {
        let name = y4m_no_temp(32, 24, 60, "pace");
        let source = open_media(&name).expect("abrir Y4M");
        let vinfo = VideoInfo::from_media(source.info()).expect("vídeo");
        let clock = Arc::new(Clock::default());
        clock.publish(0.0);
        let pipeline = spawn(&source, vinfo, false, 0.0, Arc::new(Metrics::new()), clock.clone()).expect("spawn");

        // ~350 ms de lead a 25 fps = ~9 frames; com o relógio parado, muitos
        // menos que os 60 do arquivo chegam.
        let mut early = 0;
        while pipeline.rx.recv_timeout(Duration::from_millis(300)).is_ok() {
            early += 1;
        }
        assert!(early > 0, "o primeiro frame precisa chegar");
        assert!(early < 40, "decode não pode correr o arquivo com o relógio parado (chegou {early})");

        // Avançar o relógio libera o resto.
        clock.publish(10.0);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut total = early;
        while total < 60 && std::time::Instant::now() < deadline {
            match pipeline.rx.recv_timeout(Duration::from_millis(500)) {
                Ok(VideoMsg::Frame { .. }) => total += 1,
                Ok(VideoMsg::End { .. }) | Ok(VideoMsg::Error { .. }) => break,
                Err(_) => break,
            }
        }
        let _ = std::fs::remove_file(&name);
        assert!(total > early, "o relógio avançando precisa liberar frames (antes {early}, depois {total})");
    }

    /// Container inválido vira erro, não panic.
    #[test]
    fn arquivo_corrompido_vira_erro() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("lp-rs-video-bad-{}-{nanos}.mp4", std::process::id()));
        assert!(std::fs::write(&path, b"isto nao e um mp4").is_ok());
        let result = open_media(&path.to_string_lossy());
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err(), "arquivo inválido precisa falhar");
    }
}
