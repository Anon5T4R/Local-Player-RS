//! Motor de reprodução — fases 2 (áudio) e 3 (vídeo).
//!
//! Arquitetura (plano em `docs/planos/plano-local-player-rs.md`):
//!
//! ```text
//! UI ──Cmd──▶ thread do motor ──decode áudio──▶ SampleRing ──▶ callback do cpal
//! │              └── abre o container ──Event::VideoOpened──▶ thread de vídeo
//! │                                                          (decode + ring de 4)
//! └──Event───── relógio (áudio, ou parede sem faixa de áudio) ──▶ UI escolhe o frame
//! ```
//!
//! - **Thread própria** dona do decoder de áudio e do stream: recebe [`Cmd`] por
//!   canal e devolve [`Event`] por outro. A UI nunca bloqueia em I/O de mídia.
//! - **Container de vídeo** (MP4/MOV/MKV/WebM/AVI/TS): aberto uma vez pelo hub do
//!   FilmCraft; o áudio sai de lá também (AAC/MP3/Opus/AC-3/E-AC-3/FLAC/Vorbis/PCM)
//!   e o vídeo roda numa thread separada (`engine::video`).
//! - **Arquivos só de áudio** continuam no caminho da fase 2 (symphonia; Opus em
//!   Ogg pelo FilmCraft) com resample linear quando a taxa difere do device.
//! - **Relógio mestre** = frames consumidos pelo callback (`SampleRing::played`).
//!   Sem faixa de áudio (vídeo mudo), o relógio é de parede, publicado nesta
//!   thread e congelado na pausa.
//! - **Ring SPSC sem lock** no callback; o decoder dorme enquanto o ring está
//!   cheio (o loop espera 10 ms no canal de comandos e tenta de novo).
//! - **Fim de faixa** → [`Event::TrackEnded`] → a UI avança a playlist (o motor
//!   não conhece a playlist). Vídeo mudo: a UI trata o `End` do pipeline.
//!
//! Limitações documentadas: legendas ficam para a fase 4; RGBA (Motion JPEG)
//! não é renderizado na fase 3; seek que falha vira [`Event::SeekFailed`]
//! (aviso não-fatal) e a faixa continua — nunca vira fim de faixa.

mod decoder;
mod diag;
mod error;
mod output;
mod resample;
mod ring;
mod transport;
mod video;

pub use diag::{bench, probe, BenchReport, ProbeReport};
pub use error::{EngineError, ErrorKind};
pub use transport::Transport;
pub use video::{VideoInfo, VideoMsg, VideoPipeline};

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::metrics::{Metrics, TrackInfo};
use resample::LinearResampler;
use ring::{secs_to_frames, SampleRing};

/// Teto do cache de frames decodificados do fork do FilmCraft (por faixa).
///
/// O fork já tem teto (384 MB por cache / 1 GB no processo), mas para um player
/// isso é folgado demais: 384 MB de frames + o working set do decoder + a UI é
/// o que uma máquina pequena sente como "quase congelou". O padrão aqui é
/// 128 MB; `LOCALPLAYER_FRAME_CACHE_MB` ajusta (0 = teto original do fork).
const DEFAULT_FRAME_CACHE_MB: usize = 128;

/// Aplica o orçamento do cache de frames (chamado no arranque, antes de abrir
/// qualquer faixa: o fork lê a configuração ao criar o cache).
pub fn configure_frame_cache() {
    let mb = std::env::var("LOCALPLAYER_FRAME_CACHE_MB").ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(DEFAULT_FRAME_CACHE_MB);
    let bytes = mb.saturating_mul(1024 * 1024);
    filmcraft_codecs::gop::set_frame_cache_budget(bytes);
}

/// Threads do pool de decode do fork do FilmCraft (rayon), que por padrão usa
/// TODOS os núcleos (`available_parallelism`) e disputa CPU com a UI, o áudio e
/// o que mais estiver rodando na máquina.
///
/// `LOCALPLAYER_DECODE_THREADS=N` limita o pool a N threads; `0` (padrão) fica
/// no automático do fork. Vale a partir do próximo arranque: o pool do rayon é
/// construído uma vez, na primeira decodificação (por isso a chamada é no
/// arranque, antes de qualquer mídia abrir). Medido com o `--bench` no
/// ROADMAP.
pub fn configure_decode_threads() {
    let n = std::env::var("LOCALPLAYER_DECODE_THREADS").ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
    // Dois pools no fork: o que agenda o decode de cada cache (gop) e os pools
    // de worker dos PRÓPRIOS codecs (h264-*, hevc-*, vp9-*, av1-*) — são estes
    // que comem a CPU. O mesmo limite vale para os dois.
    filmcraft_codecs::gop::set_decode_pool_threads(n);
    filmcraft_codecs::set_decoder_threads(n);
    let mode = if n == 0 { "auto (todos os nucleos)".to_string() } else { format!("limite de {n} thread(s)") };
    eprintln!("decode: pool e codecs {mode}");
}

/// hwdec como primeira opção: registra o decoder de hardware do sistema na
/// frente dos pure-Rust do hub (Windows: Media Foundation + DXVA; Linux:
/// VA-API e NVDEC). Chamado no arranque, antes de qualquer mídia abrir —
/// inclusive `--probe`/`--bench`.
///
/// Streams que o decoder de hardware recusa (codec/perfil/bit depth/tamanho,
/// sem GPU/driver, switch off) e falhas no meio do stream caem no decoder SW
/// sem o chamador saber (`HybridDecoder`); nada fica indecodificável. macOS:
/// sem efeito até a fase da plataforma (o SW é a fundação).
pub fn register_hwdec() -> filmcraft_hwdec::Availability {
    let availability = filmcraft_hwdec::register();
    eprintln!("hwdec: {availability:?}");
    availability
}

/// Comandos da UI para o motor.
enum Cmd {
    /// Abre `path` (posição inicial em segundos) e toca.
    Play {
        path: String,
        start_at: f64,
    },
    Pause,
    Resume,
    Stop,
    /// Salta para `secs` na faixa atual.
    Seek(f64),
    /// Ganho linear 0..=1.
    Volume(f32),
    Mute(bool),
}

/// Eventos do motor para a UI.
#[derive(Debug)]
pub enum Event {
    /// Duração da faixa aberta (0 = desconhecida).
    DurationKnown(f64),
    /// Posição atual (relógio mestre) — enviada a cada ~250 ms.
    PositionHint { position: f64, duration: f64 },
    /// Mudança de transporte (tocando/pausado/parado).
    State(Transport),
    /// A faixa acabou (ou falhou de forma fatal): a UI decide o próximo passo.
    TrackEnded,
    /// Falha — a UI mostra a mensagem i18n correspondente ao [`ErrorKind`].
    Error(EngineError),
    /// Busca recusada (alvo além do fim etc.): **não** é fim de faixa. A
    /// faixa continua tocando em `position`; a UI mostra um aviso.
    SeekFailed { position: f64 },
    /// O container tem vídeo: a UI passa a dirigir o pipeline (frame por PTS).
    VideoOpened(VideoPipeline),
}

/// Relógio mestre visto pela UI: o ring de áudio quando há faixa de áudio;
/// senão o relógio de parede publicado pela thread do motor (vídeo mudo).
#[derive(Default)]
struct Clock {
    audio: Mutex<Option<Arc<SampleRing>>>,
    wall: AtomicU64,
}

impl Clock {
    fn set_audio(&self, ring: Option<Arc<SampleRing>>) {
        *self.audio.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ring;
    }

    fn position(&self) -> f64 {
        let ring = self.audio.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match ring.as_ref() {
            Some(r) => r.position_secs(),
            None => f64::from_bits(self.wall.load(Ordering::Acquire)),
        }
    }

    /// Posição de parede (sem áudio), publicada pela thread do motor.
    fn publish(&self, secs: f64) {
        let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        self.wall.store(secs.to_bits(), Ordering::Release);
    }

    /// Seek imediato: a UI e o pipeline de vídeo não esperam o comando chegar.
    fn jump(&self, secs: f64) {
        let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        if let Some(ring) = self.audio.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
            ring.flush(secs_to_frames(secs, ring.rate()));
        }
        self.publish(secs);
    }
}

/// Handle da UI para o motor. Só fala por canais; o motor roda numa thread.
pub struct PlayerEngine {
    cmd: Option<Sender<Cmd>>,
    ev: Receiver<Event>,
    clock: Arc<Clock>,
    transport: Transport,
    position: f64,
    duration: f64,
}

impl Default for PlayerEngine {
    fn default() -> Self {
        Self::new(Arc::new(Metrics::new()))
    }
}

impl PlayerEngine {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let tx = ev_tx.clone();
        let clock = Arc::new(Clock::default());
        let shared = clock.clone();
        let spawned = std::thread::Builder::new().name("audio-engine".into()).spawn(move || run(cmd_rx, tx, shared, metrics));
        let cmd = match spawned {
            Ok(_) => Some(cmd_tx),
            Err(e) => {
                // Sem thread não há quem leia comandos nem emita eventos.
                let _ = ev_tx.send(Event::Error(EngineError::EngineStopped(format!("engine thread: {e}"))));
                None
            }
        };
        Self { cmd, ev: ev_rx, clock, transport: Transport::Stopped, position: 0.0, duration: 0.0 }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.cmd {
            let _ = tx.send(cmd);
        }
    }

    /// Abre `path` e toca a partir de `start_at` segundos.
    pub fn play(&mut self, path: &str, start_at: f64) {
        self.position = start_at.max(0.0);
        self.duration = 0.0;
        self.clock.set_audio(None);
        self.clock.publish(self.position);
        self.send(Cmd::Play { path: path.to_string(), start_at });
    }

    pub fn pause(&mut self) {
        self.send(Cmd::Pause);
    }

    pub fn resume(&mut self) {
        self.send(Cmd::Resume);
    }

    pub fn stop(&mut self) {
        self.clock.set_audio(None);
        self.clock.publish(0.0);
        self.send(Cmd::Stop);
    }

    /// Seek em segundos (o motor responde [`Event::Error`] se não suportar).
    pub fn seek(&mut self, secs: f64) {
        let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        self.position = secs;
        self.clock.jump(secs);
        self.send(Cmd::Seek(secs));
    }

    /// Ganho linear 0..=1 (a UI converte do slider 0..100).
    pub fn set_volume(&mut self, gain: f32) {
        self.send(Cmd::Volume(gain));
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.send(Cmd::Mute(muted));
    }

    /// Posição agora, do relógio mestre — o pipeline de vídeo usa isto para
    /// escolher o frame (o `PositionHint` de 250 ms é grosso demais).
    pub fn position_now(&self) -> f64 {
        self.clock.position()
    }

    /// Drena os eventos do motor, atualizando o estado espelhado na UI.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.ev.try_recv() {
                Ok(ev) => {
                    match &ev {
                        Event::DurationKnown(d) => self.duration = *d,
                        Event::PositionHint { position, duration } => {
                            self.position = *position;
                            if *duration > 0.0 {
                                self.duration = *duration;
                            }
                        }
                        Event::State(t) => self.transport = *t,
                        Event::TrackEnded => {
                            self.transport = Transport::Stopped;
                            self.position = 0.0;
                        }
                        Event::Error(_) | Event::VideoOpened(_) => {}
                        Event::SeekFailed { position } => self.position = *position,
                    }
                    out.push(ev);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // Thread do motor morreu sem passar por um catch_unwind:
                    // vira erro visível uma única vez (não deixa a UI
                    // "tocando" sem áudio e sem evento).
                    if self.cmd.take().is_some() {
                        self.transport = Transport::Stopped;
                        self.position = 0.0;
                        out.push(Event::Error(EngineError::EngineStopped("engine thread ended".into())));
                    }
                    break;
                }
            }
        }
        out
    }
}

// ── thread do motor ──────────────────────────────────────────────────────

/// PCM já convertido para stereo na taxa do device, esperando espaço no ring.
#[derive(Default)]
struct Pending {
    data: Vec<f32>,
    /// Cursor em frames.
    pos: usize,
}

impl Pending {
    fn reset(&mut self) {
        self.data.clear();
        self.pos = 0;
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len() / 2
    }

    fn rest(&self) -> &[f32] {
        self.data.get(self.pos * 2..).unwrap_or(&[])
    }
}

/// Faixa de áudio aberta: decoder (fonte) + resampler quando a taxa difere.
struct AudioPart {
    source: Box<dyn decoder::AudioSource>,
    resampler: Option<LinearResampler>,
}

impl AudioPart {
    fn new(source: Box<dyn decoder::AudioSource>, device_rate: u32) -> Self {
        let src_rate = source.sample_rate();
        let resampler = (src_rate != device_rate && src_rate > 0).then(|| LinearResampler::new(src_rate, device_rate));
        Self { source, resampler }
    }

    fn seek(&mut self, secs: f64) -> decoder::Seeked {
        let outcome = match catch_unwind(AssertUnwindSafe(|| self.source.seek(secs))) {
            Ok(outcome) => outcome,
            Err(_) => {
                // Pânico dentro do seek: pior caso, volta ao começo.
                let position = match catch_unwind(AssertUnwindSafe(|| self.source.seek(0.0))) {
                    Ok(outcome) => outcome.position,
                    Err(_) => 0.0,
                };
                decoder::Seeked { position, warning: Some("decoder panicked while seeking".into()) }
            }
        };
        // Qualquer reposicionamento invalida a interpolação do resampler.
        if let Some(r) = &mut self.resampler {
            r.reset();
        }
        outcome
    }
}

/// Faixa aberta. `audio = None` = vídeo mudo (o relógio vira o de parede).
struct Track {
    path: String,
    duration: Option<f64>,
    audio: Option<AudioPart>,
}

impl Track {
    fn seek(&mut self, secs: f64) -> decoder::Seeked {
        match self.audio.as_mut() {
            Some(a) => a.seek(secs),
            // Vídeo mudo: o seek é do pipeline (UI) + relógio de parede.
            None => decoder::Seeked { position: if secs.is_finite() { secs.max(0.0) } else { 0.0 }, warning: None },
        }
    }
}

/// Relógio de parede: usado só quando a faixa não tem áudio.
#[derive(Default)]
struct WallClock {
    base: f64,
    started: Option<Instant>,
}

impl WallClock {
    fn playing(base: f64) -> Self {
        Self { base, started: Some(Instant::now()) }
    }

    fn position(&self) -> f64 {
        self.base + self.started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0)
    }

    fn resume(&mut self) {
        if self.started.is_none() {
            self.started = Some(Instant::now());
        }
    }

    fn pause(&mut self) {
        if let Some(t) = self.started.take() {
            self.base += t.elapsed().as_secs_f64();
        }
    }

    fn seek(&mut self, secs: f64) {
        self.base = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        if self.started.is_some() {
            self.started = Some(Instant::now());
        }
    }

    fn stop(&mut self) {
        self.base = 0.0;
        self.started = None;
    }
}

/// Resultado de abrir um arquivo (container ou áudio puro).
struct OpenedTrack {
    audio: Option<Box<dyn decoder::AudioSource>>,
    video: Option<VideoPipeline>,
    duration: Option<f64>,
}

/// Estado mutável da thread do motor (o que o loop toca a cada tick).
struct EngineState {
    session: Option<output::Output>,
    track: Option<Track>,
    pending: Pending,
    transport: Transport,
    wall: WallClock,
    last_hint: Instant,
}

fn run(rx: Receiver<Cmd>, tx: Sender<Event>, clock: Arc<Clock>, metrics: Arc<Metrics>) {
    let session = match output::start(tx.clone()) {
        Ok(o) => Some(o),
        Err(e) => {
            let _ = tx.send(Event::Error(e));
            None
        }
    };
    let mut st = EngineState {
        session,
        track: None,
        pending: Pending::default(),
        transport: Transport::Stopped,
        wall: WallClock::default(),
        last_hint: Instant::now(),
    };

    loop {
        // Relógio publicado para a UI quando não há áudio (vídeo mudo).
        let pos = match (st.track.as_ref(), st.session.as_ref()) {
            (Some(t), Some(s)) if t.audio.is_some() => s.ring.position_secs(),
            _ => st.wall.position(),
        };
        clock.publish(pos);

        // Enquanto há faixa aberta, acorda a cada 10 ms para abastecer o ring;
        // parado, pode dormir mais (comandos acordam na hora).
        let wait = if st.transport.decodes() { Duration::from_millis(10) } else { Duration::from_millis(120) };
        match rx.recv_timeout(wait) {
            Ok(cmd) => handle_command(cmd, &mut st, &tx, &clock, &metrics),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let mut fail: Option<EngineError> = None;
        let mut ended = false;
        if let Some(s) = st.session.as_ref() {
            if let Some(t) = st.track.as_mut() {
                if t.audio.is_some() {
                    if st.pending.done() {
                        match decode_step(t) {
                            Ok(Some(chunk)) => fill_pending(t, &mut st.pending, chunk, s.rate),
                            Ok(None) => s.ring.set_eof(true),
                            Err(e) => fail = Some(e),
                        }
                    }
                    if fail.is_none() && !st.pending.done() {
                        st.pending.pos += s.ring.push(st.pending.rest());
                    }
                    if s.ring.take_ended() {
                        ended = true;
                    }
                }
            }
        }

        if let Some(e) = fail {
            stop_track(&mut st, &clock);
            let _ = tx.send(Event::Error(e));
            let _ = tx.send(Event::State(st.transport));
            let _ = tx.send(Event::TrackEnded);
        } else if ended {
            stop_track(&mut st, &clock);
            let _ = tx.send(Event::State(st.transport));
            let _ = tx.send(Event::TrackEnded);
        }

        if st.last_hint.elapsed() >= Duration::from_millis(250) {
            st.last_hint = Instant::now();
            if let Some(t) = st.track.as_ref() {
                let duration = t.duration.unwrap_or(0.0);
                let _ = tx.send(Event::PositionHint { position: clock.position(), duration });
            }
        }
    }
}

/// Zera faixa/buffer/relógios e marca o transporte como parado.
fn stop_track(st: &mut EngineState, clock: &Clock) {
    st.track = None;
    st.pending.reset();
    if let Some(s) = &st.session {
        s.ring.flush(0);
        s.ring.set_paused(true);
    }
    clock.set_audio(None);
    st.wall.stop();
    st.transport = Transport::Stopped;
}

fn handle_command(cmd: Cmd, st: &mut EngineState, tx: &Sender<Event>, clock: &Arc<Clock>, metrics: &Arc<Metrics>) {
    match cmd {
        Cmd::Play { path, start_at } => {
            let mut start_at = start_at;
            // Troca de faixa: descarta a anterior e o buffer velho.
            st.track = None;
            st.pending.reset();
            clock.set_audio(None);
            if let Some(s) = st.session.as_ref() {
                s.ring.flush(secs_to_frames(start_at, s.rate));
                s.ring.set_paused(true);
            }
            // Device ausente/desconectado desde o arranque: tenta abrir de
            // novo a cada Play (headphone plugado depois entra sem reabrir o
            // app). Sem device um vídeo ainda toca mudo.
            let mut device_error: Option<EngineError> = None;
            if st.session.is_none() {
                match output::start(tx.clone()) {
                    Ok(o) => st.session = Some(o),
                    Err(e) => device_error = Some(e),
                }
            }
            let rate = st.session.as_ref().map_or(48_000, |s| s.rate);
            let opened = match catch_unwind(AssertUnwindSafe(|| open_track_media(&path, start_at, rate, metrics, clock))) {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => {
                    fail_play(st, clock, tx, e);
                    return;
                }
                Err(_) => {
                    fail_play(st, clock, tx, EngineError::Open("decoder panicked while opening".into()));
                    return;
                }
            };
            let OpenedTrack { audio, video, duration } = opened;
            let mut audio = audio;
            // Sem saída de áudio: vídeo continua mudo; só áudio é fatal.
            if audio.is_some() && st.session.is_none() {
                audio = None;
                let e = device_error.take().unwrap_or_else(|| EngineError::NoDevice("no audio output device".into()));
                let _ = tx.send(Event::Error(e));
            }
            if audio.is_none() && video.is_none() {
                let e = device_error.take().unwrap_or_else(|| EngineError::NoDevice("no audio output device".into()));
                fail_play(st, clock, tx, e);
                return;
            }
            if let Some(p) = video {
                let _ = tx.send(Event::VideoOpened(p));
            }
            let mut duration = duration;
            let mut seek_warning: Option<String> = None;
            let audio = match audio {
                Some(source) => {
                    let mut part = AudioPart::new(source, rate);
                    if start_at > 0.0 {
                        // Retomada em ponto salvo: se o alvo não existir mais
                        // (arquivo trocado/cortado), toca do último ponto bom
                        // e avisa — abrir nunca pode pular a faixa.
                        let outcome = part.seek(start_at);
                        start_at = outcome.position;
                        seek_warning = outcome.warning;
                    }
                    duration = duration.or_else(|| part.source.duration());
                    Some(part)
                }
                None => None,
            };
            if let Some(s) = st.session.as_ref() {
                s.ring.flush(secs_to_frames(start_at, s.rate));
                s.ring.set_paused(false);
                if audio.is_some() {
                    clock.set_audio(Some(s.ring.clone()));
                }
            }
            st.track = Some(Track { path, duration, audio });
            st.wall = WallClock::playing(start_at);
            st.transport = Transport::Playing;
            let _ = tx.send(Event::DurationKnown(duration.unwrap_or(0.0)));
            let _ = tx.send(Event::State(st.transport));
            if let Some(detail) = seek_warning {
                eprintln!("engine: seek inicial recusado: {detail} (começando em {start_at:.3}s)");
                let _ = tx.send(Event::SeekFailed { position: start_at });
            }
        }
        Cmd::Pause => {
            if st.track.is_some() {
                if let Some(s) = st.session.as_ref() {
                    s.ring.set_paused(true);
                }
                st.wall.pause();
                st.transport = Transport::Paused;
                let _ = tx.send(Event::State(st.transport));
            }
        }
        Cmd::Resume => {
            if st.track.is_some() {
                if let Some(s) = st.session.as_ref() {
                    s.ring.set_paused(false);
                }
                st.wall.resume();
                st.transport = Transport::Playing;
                let _ = tx.send(Event::State(st.transport));
            }
        }
        Cmd::Stop => {
            stop_track(st, clock);
            let _ = tx.send(Event::State(st.transport));
        }
        Cmd::Seek(secs) => {
            let outcome = st.track.as_mut().map(|t| t.seek(secs));
            match outcome {
                Some(outcome) => {
                    st.wall.seek(outcome.position);
                    st.pending.reset();
                    if let Some(s) = st.session.as_ref() {
                        s.ring.flush(secs_to_frames(outcome.position, s.rate));
                    }
                    if let Some(detail) = outcome.warning {
                        eprintln!("engine: seek para {secs:.3}s recusado: {detail} (faixa segue em {:.3}s)", outcome.position);
                        // Aviso não-fatal: a playlist NÃO anda e a faixa continua.
                        let _ = tx.send(Event::SeekFailed { position: outcome.position });
                    }
                }
                None => st.wall.seek(secs),
            }
        }
        Cmd::Volume(gain) => {
            if let Some(s) = st.session.as_ref() {
                s.ring.set_volume(gain);
            }
        }
        Cmd::Mute(muted) => {
            if let Some(s) = st.session.as_ref() {
                s.ring.set_muted(muted);
            }
        }
    }
}

/// Erro fatal de abertura/troca: zera o estado e manda a UI pular a faixa.
fn fail_play(st: &mut EngineState, clock: &Clock, tx: &Sender<Event>, e: EngineError) {
    stop_track(st, clock);
    let _ = tx.send(Event::Error(e));
    let _ = tx.send(Event::State(st.transport));
    let _ = tx.send(Event::TrackEnded);
}

/// Abre o arquivo pelo melhor backend: container de vídeo pelo hub do FilmCraft
/// (vídeo + áudio de uma vez); arquivo de áudio pela fase 2 (symphonia/Opus).
fn open_track_media(path: &str, start_at: f64, rate: u32, metrics: &Arc<Metrics>, clock: &Arc<Clock>) -> Result<OpenedTrack, EngineError> {
    if video::is_video_container(path) {
        let source = video::open_media(path)?;
        let info = source.info();
        let duration = info.duration.seconds();
        let has_audio = info.has_audio();
        metrics.set_track(TrackInfo::from_media(info));
        let video = match VideoInfo::from_media(info) {
            Some(v) => {
                if v.width > video::MAX_DIMENSION || v.height > video::MAX_DIMENSION {
                    return Err(EngineError::TooLarge(format!("{}×{}", v.width, v.height)));
                }
                Some(video::spawn(&source, v, has_audio, start_at, metrics.clone(), clock.clone())?)
            }
            None => None,
        };
        let audio = if has_audio { Some(video::MediaAudio::open(source.clone(), rate, duration)?) } else { None };
        Ok(OpenedTrack { audio, video, duration: (duration > 0.0).then_some(duration) })
    } else {
        let source = decoder::open(path).map_err(EngineError::Open)?;
        let duration = source.duration();
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("audio").to_ascii_lowercase();
        metrics.set_track(TrackInfo {
            container: ext.clone(),
            video_codec: None,
            audio_codec: Some(ext),
            width: 0,
            height: 0,
            fps: 0.0,
            duration: duration.unwrap_or(0.0),
            bitrate: crate::metrics::estimate_bitrate(std::fs::metadata(path).ok().map(|m| m.len()), duration.unwrap_or(0.0)),
            file_size: std::fs::metadata(path).ok().map(|m| m.len()),
        });
        Ok(OpenedTrack { audio: Some(source), video: None, duration })
    }
}

fn decode_step(track: &mut Track) -> Result<Option<decoder::Chunk>, EngineError> {
    let Some(part) = track.audio.as_mut() else { return Ok(None) };
    match catch_unwind(AssertUnwindSafe(|| part.source.next_chunk())) {
        Ok(Ok(chunk)) => Ok(chunk),
        Ok(Err(detail)) => Err(EngineError::Decode(format!("{}: {detail}", track.path))),
        Err(_) => Err(EngineError::Decode(format!("{}: decoder panicked", track.path))),
    }
}

fn fill_pending(track: &mut Track, pending: &mut Pending, chunk: decoder::Chunk, device_rate: u32) {
    let stereo = decoder::to_stereo(&chunk.samples, chunk.channels);
    pending.reset();
    if chunk.rate == 0 || chunk.rate == device_rate {
        pending.data = stereo;
        return;
    }
    let Some(part) = track.audio.as_mut() else { return };
    let rs = part.resampler.get_or_insert_with(|| LinearResampler::new(chunk.rate, device_rate));
    rs.process(&stereo, &mut pending.data);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fonte que recusa alvos além de 10 s e informa a posição restaurada.
    struct FakeSource;

    impl decoder::AudioSource for FakeSource {
        fn next_chunk(&mut self) -> Result<Option<decoder::Chunk>, String> {
            Ok(None)
        }

        fn seek(&mut self, secs: f64) -> decoder::Seeked {
            if secs > 10.0 {
                decoder::Seeked { position: 3.0, warning: Some("alvo fora do arquivo".into()) }
            } else {
                decoder::Seeked { position: secs, warning: None }
            }
        }

        fn duration(&self) -> Option<f64> {
            Some(10.0)
        }

        fn sample_rate(&self) -> u32 {
            44_100
        }
    }

    fn state_with_fake_track() -> EngineState {
        EngineState {
            session: None,
            track: Some(Track {
                path: "faixa.mp3".into(),
                duration: Some(10.0),
                audio: Some(AudioPart { source: Box::new(FakeSource), resampler: None }),
            }),
            pending: Pending::default(),
            transport: Transport::Playing,
            wall: WallClock::playing(0.0),
            last_hint: Instant::now(),
        }
    }

    /// Regressão do MP3 que pulava faixa: um seek recusado vira
    /// `SeekFailed` (aviso) e **nunca** `TrackEnded`; o relógio volta para a
    /// posição restaurada.
    #[test]
    fn seek_recusado_vira_aviso_e_nao_fim_de_faixa() {
        let (tx, rx) = channel();
        let clock = Arc::new(Clock::default());
        let metrics = Arc::new(Metrics::new());
        let mut st = state_with_fake_track();

        handle_command(Cmd::Seek(1.0e12), &mut st, &tx, &clock, &metrics);
        drop(tx);

        let events: Vec<Event> = rx.try_iter().collect();
        let warned = events.iter().any(|ev| matches!(ev, Event::SeekFailed { position } if (*position - 3.0).abs() < 1e-9));
        let ended = events.iter().any(|ev| matches!(ev, Event::TrackEnded));
        let failed = events.iter().any(|ev| matches!(ev, Event::Error(_)));
        assert!(warned, "seek recusado não gerou SeekFailed");
        assert!(!ended, "seek recusado gerou TrackEnded (pula a playlist)");
        assert!(!failed, "seek recusado virou erro fatal");
        assert!((st.wall.position() - 3.0).abs() < 0.1, "relógio não voltou: {}", st.wall.position());
    }
}
