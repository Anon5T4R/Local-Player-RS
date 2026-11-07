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
//! não é renderizado na fase 3; seek em formato que não suporta vira
//! [`ErrorKind::Seek`] sem derrubar nada.

mod decoder;
mod error;
mod output;
mod resample;
mod ring;
mod transport;
mod video;

pub use error::{EngineError, ErrorKind};
pub use transport::Transport;
pub use video::{VideoInfo, VideoMsg, VideoPipeline};

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use resample::LinearResampler;
use ring::{secs_to_frames, SampleRing};

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
        Self::new()
    }
}

impl PlayerEngine {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = channel();
        let (ev_tx, ev_rx) = channel();
        let tx = ev_tx.clone();
        let clock = Arc::new(Clock::default());
        let shared = clock.clone();
        let spawned = std::thread::Builder::new().name("audio-engine".into()).spawn(move || run(cmd_rx, tx, shared));
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

    fn seek(&mut self, secs: f64) -> Result<(), EngineError> {
        match catch_unwind(AssertUnwindSafe(|| self.source.seek(secs))) {
            Ok(Ok(())) => {
                if let Some(r) = &mut self.resampler {
                    r.reset();
                }
                Ok(())
            }
            Ok(Err(detail)) => {
                eprintln!("engine: seek falhou: {detail}");
                Err(EngineError::Seek)
            }
            Err(_) => Err(EngineError::Seek),
        }
    }
}

/// Faixa aberta. `audio = None` = vídeo mudo (o relógio vira o de parede).
struct Track {
    path: String,
    duration: Option<f64>,
    audio: Option<AudioPart>,
}

impl Track {
    fn seek(&mut self, secs: f64) -> Result<(), EngineError> {
        match self.audio.as_mut() {
            Some(a) => a.seek(secs),
            // Vídeo mudo: o seek é do pipeline (UI) + relógio de parede.
            None => Ok(()),
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

fn run(rx: Receiver<Cmd>, tx: Sender<Event>, clock: Arc<Clock>) {
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
            Ok(cmd) => handle_command(cmd, &mut st, &tx, &clock),
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

fn handle_command(cmd: Cmd, st: &mut EngineState, tx: &Sender<Event>, clock: &Arc<Clock>) {
    match cmd {
        Cmd::Play { path, start_at } => {
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
            let opened = match catch_unwind(AssertUnwindSafe(|| open_track_media(&path, start_at, rate))) {
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
            let audio = match audio {
                Some(source) => {
                    let mut part = AudioPart::new(source, rate);
                    if start_at > 0.0 {
                        if let Err(e) = part.seek(start_at) {
                            fail_play(st, clock, tx, e);
                            return;
                        }
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
            st.wall.seek(secs);
            if let Some(t) = st.track.as_mut() {
                match t.seek(secs) {
                    Ok(()) => {
                        st.pending.reset();
                        if let Some(s) = st.session.as_ref() {
                            s.ring.flush(secs_to_frames(secs, s.rate));
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Event::Error(e));
                    }
                }
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
fn open_track_media(path: &str, start_at: f64, rate: u32) -> Result<OpenedTrack, EngineError> {
    if video::is_video_container(path) {
        let source = video::open_media(path)?;
        let info = source.info();
        let duration = info.duration.seconds();
        let has_audio = info.has_audio();
        let video = match VideoInfo::from_media(info) {
            Some(v) => {
                if v.width > video::MAX_DIMENSION || v.height > video::MAX_DIMENSION {
                    return Err(EngineError::TooLarge(format!("{}×{}", v.width, v.height)));
                }
                Some(video::spawn(&source, v, has_audio, start_at)?)
            }
            None => None,
        };
        let audio = if has_audio { Some(video::MediaAudio::open(source.clone(), rate, duration)?) } else { None };
        Ok(OpenedTrack { audio, video, duration: (duration > 0.0).then_some(duration) })
    } else {
        let source = decoder::open(path).map_err(EngineError::Open)?;
        let duration = source.duration();
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
