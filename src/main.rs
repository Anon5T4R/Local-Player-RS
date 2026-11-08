//! Local Player RS — player de mídia 100% local e offline em Rust puro.
//!
//! Fases 2–4: shell egui/eframe da suíte (playlist de arquivo/pasta,
//! drag-and-drop, "abrir com", tema, i18n e autoupdate), motor de áudio de
//! verdade (`engine.rs`, relógio mestre = áudio), **vídeo SW** (demux/decode
//! pelo hub do FilmCraft em thread própria, frame escolhido por PTS e pintado
//! numa textura wgpu com shader YUV→RGB, `video/`) e **legenda externa**
//! SRT/WebVTT/ASS (`subtitle/`) desenhada pela egui sobre o vídeo.
//!
//! "Nada pesado na UI thread": listagem de pasta e os decoders (áudio e vídeo)
//! rodam em threads próprias e falam com a UI por canais; a UI só sobe o frame
//! devido para a GPU.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod engine;
mod icons;
mod metrics;
mod queue;
mod resume;
mod session_log;
mod subtitle;
mod video;

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use lab_ui::config::{self, Config};
use lab_ui::i18n::{self, Key};
use lab_ui::theme;

const APP_ID: &str = "local-player-rs";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Entrada do app na suíte (craft_update): o updater só baixa release deste
/// repositório. O `publisher` casa com o `Manufacturer` do MSI (o updater
/// confere os dois pra reconhecer a instalação).
static APP: craft_update::App = craft_update::App {
    id: "local-player-rs",
    name: "Local Player RS",
    tagline: "Media player",
    repo: "Anon5T4R/Local-Player-RS",
    former_ids: &[],
    former_names: &[],
    icon_png: None,
    publisher: "Local Player RS contributors",
};

/// Ícone da janela (além do .exe cunhado no build.rs — Windows pega os
/// dois; AppImage/desktop só vendo com o PNG do pacote).
const ICON_PNG: &[u8] = include_bytes!("../icons/icon.png");

/// Extensões aceitas na playlist (o filtro do diálogo, do drag-drop e dos args).
const MEDIA_EXTS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "mp3", "flac", "opus", "ogg", "wav", "m4a", "aac"];

fn is_media(p: &std::path::Path) -> bool {
    p.extension().and_then(|x| x.to_str()).map(|x| MEDIA_EXTS.contains(&x.to_ascii_lowercase().as_str())).unwrap_or(false)
}

fn main() -> eframe::Result<()> {
    // Windows: o helper de update é uma CÓPIA deste programa iniciada com
    // `--finish-update` (craft_update). Roda ANTES de tudo — o helper espera
    // o app sair e instala por conta própria.
    #[cfg(windows)]
    if let Some(result) = craft_update::self_update::finish(&APP, &std::env::args_os().skip(1).collect::<Vec<_>>()) {
        std::process::exit(i32::from(result.is_err()));
    }

    // "Abrir com" do Windows manda os caminhos como args (pode ser mais de um).
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Teto do cache de frames do fork ANTES de qualquer faixa abrir (o fork lê
    // a configuração na criação do cache).
    engine::configure_frame_cache();

    // hwdec como primeira opção ANTES de qualquer mídia abrir (o hub tenta os
    // decoders registrados primeiro; --probe/--bench também passam por aqui).
    let _ = engine::register_hwdec();

    // Diagnóstico sem janela: --probe / --bench / --help / --version saem aqui.
    if let Some(code) = cli::run(&args) {
        std::process::exit(code);
    }

    // Contadores permanentes: motor, decode de vídeo e UI compartilham o mesmo
    // `Arc<Metrics>` (overlay da tecla D e CSV de LOCALPLAYER_STATS).
    let metrics = Arc::new(metrics::Metrics::new());

    let cfg = config::load(APP_ID);

    let mut viewport = egui::ViewportBuilder::default().with_title(APP.name).with_inner_size([480.0, 600.0]);
    // Ícone da janela (o .exe já cunha o mesmo no build.rs — Resources).
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    // O `mut` só é usado pelo bloco cfg(windows) que configura o wgpu.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut options = eframe::NativeOptions { viewport, ..Default::default() };
    // Renderer wgpu no Windows (eframe 0.37 já traz wgpu com os backends
    // padrão — DX12 entra pelo default features do wgpu): a fase 2 pinta o
    // frame de vídeo numa textura wgpu, e o mesmo backend serve desde já.
    // DX12 only (Vulkan briga com drivers e o GL do wgpu não é caminho de
    // produção); a env WGPU_BACKEND vence como escape. DX12 compila com FXC
    // (d3dcompiler_47.dll, parte do Windows) — o default do wgpu 30 é
    // Auto/DXC, que pode carregar um dxcompiler.dll de terceiros no
    // search path e perder a janela antes do primeiro frame (mesmo
    // desenho do local-office/graphics.rs #603).
    #[cfg(windows)]
    {
        use eframe::egui_wgpu::WgpuSetup;
        use eframe::wgpu::{Backends, Dx12Compiler};
        if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
            create.instance_descriptor.backends = Backends::from_env().unwrap_or(Backends::DX12);
            create.instance_descriptor.backend_options.dx12.shader_compiler = Dx12Compiler::Fxc;
        }
        options.renderer = eframe::Renderer::Wgpu;
    }
    eframe::run_native(
        APP_ID,
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, cfg.theme);
            let mut app = PlayerApp::new(cfg, args, cc.wgpu_render_state.as_ref(), metrics);
            // Check de update no arranque: achou release nova, o prompt do
            // updater abre sozinho (craft_update::dialog).
            app.updater.check_at_start(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

/// Decodifica o PNG do ícone pra `IconData` (o `.with_icon` do egui).
fn load_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(ICON_PNG).ok()?;
    let rgba = img.to_rgba8().into_raw();
    Some(egui::IconData { width: img.width(), height: img.height(), rgba })
}

/// Abre a página da release no navegador — ação "Download" do prompt do
/// updater, para instalações que não se auto-atualizam (zip solto, dev build).
fn open_url(url: &str) {
    #[cfg(windows)]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb = HSTRING::from("open");
        let file = HSTRING::from(url);
        // > 32 = lançou (mesma convenção do antigo autoupdate com "runas").
        let res = unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        if res.0 as usize <= 32 {
            eprintln!("update: falha ao abrir {url} (código {})", res.0 as usize);
        }
    }
    #[cfg(not(windows))]
    {
        if std::process::Command::new("xdg-open").arg(url).spawn().is_err() {
            eprintln!("update: falha ao abrir {url}");
        }
    }
}

struct PlayerApp {
    cfg: Config,
    playlist: resume::Playlist,
    /// Arquivo selecionado na playlist.
    idx: Option<usize>,
    /// Nome do arquivo atual (display).
    now: String,
    status: String,
    /// Listagem de "+ pasta" rodando fora da UI thread (read_dir em pasta
    /// de rede/nuvem trava a janela — mesma lentidão do Explorer).
    dir_rx: Option<Receiver<Result<Vec<String>, String>>>,
    /// Motor de áudio (thread própria; a UI só manda comandos e drena eventos).
    engine: engine::PlayerEngine,
    /// Verificação e instalação de atualização (craft_update): check no
    /// arranque/pedido, prompt egui próprio e self-update em thread própria.
    updater: craft_update::updater::Updater,
    /// "Retome de onde parou" (posição por arquivo).
    resume: resume::Resume,
    /// Volume do slider (0..100) e mute — persistidos em `player.json`.
    volume: f64,
    muted: bool,
    /// Espelho do estado do motor (eventos): botão play/pause e barra de seek.
    transport: engine::Transport,
    position: f64,
    duration: f64,
    /// Arrastando a barra de seek: o PositionHint do motor não pisa no valor.
    scrubbing: bool,
    scrub_value: f64,
    /// Posição salva capturada ao abrir a faixa. Abrir é sempre do começo;
    /// este é o gatilho do botão "Retomar" (o save do arquivo segue normal).
    resume_at: Option<f64>,
    /// `status` é um aviso (ganha o ícone de alerta em vez de texto puro).
    status_warn: bool,
    /// Falhas seguidas (não gira a playlist pra sempre com arquivos quebrados).
    error_streak: usize,
    /// Throttle do resume.json (grava a cada 5 s de playback, não a cada frame).
    last_resume_save: Instant,
    /// Pipeline de vídeo da faixa atual (thread de decode fora da UI).
    video: Option<engine::VideoPipeline>,
    /// Render wgpu do vídeo (YUV→RGB); `None` se o eframe não roda em wgpu.
    video_gpu: Option<video::gpu::VideoGpu>,
    /// Frames decodificados aguardando o PTS (ring pequeno, memória limitada).
    video_frames: VecDeque<video::Frame>,
    /// Já há um frame na GPU (o callback sabe o que pintar).
    video_shown: bool,
    /// Rotação em quartos (R / Shift+R), como no local-player antigo.
    rotation: u8,
    /// Fullscreen via viewport do egui (F).
    fullscreen: bool,
    /// Última interação (mouse/tecla): os controles somem por inatividade.
    last_activity: Instant,
    /// Seek pedido há pouco: repinta uns frames para mostrar o frame parado.
    seek_preview_until: Option<Instant>,
    /// Legenda externa da faixa atual (SRT/VTT ao lado do vídeo), carregada
    /// quando o container abre; `None` = sem arquivo/entradas válidas.
    subtitles: Option<subtitle::Subtitles>,
    /// Toggle de exibição (tecla S / botão CC); vale entre faixas.
    subtitles_on: bool,
    /// Atraso da legenda em segundos (+ = aparece depois; Z/X ±0,5 s).
    subtitle_delay: f64,
    /// Contadores de desempenho compartilhados com o motor e a thread de decode.
    metrics: Arc<metrics::Metrics>,
    /// Overlay de estatísticas (tecla D).
    show_stats: bool,
    /// Log automático de sessão/anomalias (sempre ligado; ver `session_log.rs`).
    session: session_log::SessionLog,
    /// PTS do último frame que foi para a tela (detecta stall com fila vazia).
    last_video_pts: f64,
}

impl PlayerApp {
    fn new(cfg: Config, args: Vec<String>, render_state: Option<&eframe::egui_wgpu::RenderState>, metrics: Arc<metrics::Metrics>) -> Self {
        // "Abrir com": adiciona os arquivos de args na playlist e seleciona o
        // primeiro.
        let mut playlist = resume::load_playlist();
        let mut first_arg: Option<String> = None;
        for a in &args {
            if !is_media(std::path::Path::new(a)) {
                continue;
            }
            if !playlist.files.contains(a) {
                playlist.files.push(a.clone());
            }
            if first_arg.is_none() {
                first_arg = Some(a.clone());
            }
        }
        let idx = first_arg.and_then(|f| playlist.files.iter().position(|x| *x == f));
        if idx.is_some() {
            resume::save_playlist(&playlist);
        }

        // Volume/mute persistem entre sessões e valem já no primeiro play.
        let prefs = resume::load_prefs();
        let volume = prefs.volume.clamp(0.0, 100.0);
        let mut engine = engine::PlayerEngine::new(metrics.clone());
        engine.set_volume((volume / 100.0) as f32);
        engine.set_muted(prefs.muted);

        let mut app = Self {
            cfg,
            playlist,
            idx: None,
            now: String::new(),
            status: String::new(),
            dir_rx: None,
            engine,
            updater: craft_update::updater::Updater::new(&APP, CURRENT_VERSION),
            resume: resume::load(),
            volume,
            muted: prefs.muted,
            transport: engine::Transport::Stopped,
            position: 0.0,
            duration: 0.0,
            scrubbing: false,
            scrub_value: 0.0,
            resume_at: None,
            status_warn: false,
            error_streak: 0,
            last_resume_save: Instant::now(),
            video: None,
            video_gpu: render_state.and_then(video::gpu::VideoGpu::new),
            video_frames: VecDeque::new(),
            video_shown: false,
            rotation: 0,
            fullscreen: false,
            last_activity: Instant::now(),
            seek_preview_until: None,
            subtitles: None,
            subtitles_on: true,
            subtitle_delay: 0.0,
            metrics,
            show_stats: false,
            session: session_log::SessionLog::new(),
            last_video_pts: 0.0,
        };
        if let Some(i) = idx {
            app.select(i);
        }
        app
    }

    /// Seleciona a faixa `i` e manda o motor abrir.
    ///
    /// Sempre do começo: retomar de onde parou é uma ação EXPLÍCITA do usuário
    /// (botão "Retomar"), nunca automática. A posição salva fica em
    /// `resume_at` só para oferecer o botão.
    fn select(&mut self, i: usize) {
        let Some(path) = self.playlist.files.get(i).cloned() else {
            return;
        };
        self.remember_current();
        let saved = resume::position_of(&self.resume, &path);
        let start = open_position(false, saved, self.duration);
        self.resume_at = saved;
        self.idx = Some(i);
        self.now = file_name(&path);
        self.status.clear();
        self.status_warn = false;
        self.position = start;
        self.duration = 0.0;
        self.scrubbing = false;
        self.drop_video();
        self.last_video_pts = start;
        self.engine.play(&path, start);
    }

    /// Lembra a posição da faixa atual (memória; persistir é com `resume::save`).
    fn remember_current(&mut self) {
        let Some(i) = self.idx else { return };
        let Some(path) = self.playlist.files.get(i).cloned() else { return };
        // Perto do fim (ou terminada) a próxima sessão recomeça do zero.
        let pos = if self.duration > 0.0 && self.position > self.duration - 3.0 { 0.0 } else { self.position };
        resume::remember(&mut self.resume, &path, pos);
    }

    fn save_prefs(&self) {
        resume::save_prefs(&resume::Prefs { volume: self.volume, muted: self.muted });
    }

    /// Botão play/pause: tocando → pausa; pausado → retoma; parado → abre a
    /// faixa selecionada na posição atual.
    fn toggle_play(&mut self) {
        match self.transport {
            engine::Transport::Playing => self.engine.pause(),
            engine::Transport::Paused => self.engine.resume(),
            engine::Transport::Stopped => {
                if let Some(path) = self.idx.and_then(|i| self.playlist.files.get(i)).cloned() {
                    // Tentativa explícita do usuário: libera o freio do
                    // "pula a playlist" (falhas anteriores não bloqueiam).
                    self.error_streak = 0;
                    let start = self.position.max(0.0);
                    self.engine.play(&path, start);
                }
            }
        }
    }

    fn stop_playback(&mut self) {
        self.remember_current();
        resume::save(&self.resume);
        self.engine.stop();
        self.drop_video();
        self.transport = engine::Transport::Stopped;
        self.position = 0.0;
    }

    /// Posição que o botão "Retomar" oferece agora: a posição salva da faixa
    /// quando ela é digna de retomada (`resumable`) e ainda está à frente do
    /// ponto atual. Passado o ponto salvo, o botão desaparece.
    fn resume_point(&self) -> Option<f64> {
        let point = resumable(self.resume_at?, self.duration)?;
        (self.position + 5.0 < point).then_some(point)
    }

    /// Retomada explícita (botão "Retomar"): toca a faixa na posição salva.
    fn resume_playback(&mut self, point: f64) {
        let point = open_position(true, Some(point), self.duration);
        if point <= 0.0 {
            return;
        }
        match self.transport {
            engine::Transport::Stopped => {
                if let Some(path) = self.idx.and_then(|i| self.playlist.files.get(i)).cloned() {
                    self.error_streak = 0;
                    self.position = point;
                    self.last_video_pts = point;
                    self.engine.play(&path, point);
                }
            }
            _ => self.seek_to(point),
        }
    }

    fn seek_to(&mut self, secs: f64) {
        let secs = if self.duration > 0.0 { secs.clamp(0.0, self.duration) } else { secs.max(0.0) };
        self.position = secs;
        self.engine.seek(secs);
        if let Some(v) = self.video.as_mut() {
            v.seek(secs);
        }
        self.video_frames.clear();
        // Relógio saltou: o "atraso" do frame antigo não é travada.
        self.last_video_pts = secs;
        // Em pausa não há repaint contínuo: repinta um instante para o frame
        // parado no novo ponto aparecer.
        self.seek_preview_until = Some(Instant::now() + Duration::from_millis(800));
    }

    fn seek_relative(&mut self, delta: f64) {
        if self.idx.is_some() {
            self.seek_to(self.position + delta);
        }
    }

    fn play_prev(&mut self) {
        let Some(i) = self.idx else { return };
        self.error_streak = 0;
        match queue::prev_index(self.playlist.files.len(), i) {
            Some(p) => self.select(p),
            // No começo da lista: "anterior" reinicia a faixa.
            None => self.seek_to(0.0),
        }
    }

    fn play_next(&mut self) {
        let Some(i) = self.idx else { return };
        self.error_streak = 0;
        if let Some(n) = queue::next_index(self.playlist.files.len(), i) {
            self.select(n);
        }
    }

    /// Fim de faixa (natural ou falha fatal): avança a playlist sozinho.
    fn on_track_ended(&mut self) {
        let Some(i) = self.idx else { return };
        if let Some(path) = self.playlist.files.get(i).cloned() {
            resume::remember(&mut self.resume, &path, 0.0);
        }
        resume::save(&self.resume);
        let len = self.playlist.files.len();
        // A playlist inteira falhando não pode virar loop infinito.
        if self.error_streak >= len.max(1) {
            self.error_streak = 0;
            self.transport = engine::Transport::Stopped;
            self.position = 0.0;
            return;
        }
        match queue::next_index(len, i) {
            Some(n) => self.select(n),
            None => {
                self.transport = engine::Transport::Stopped;
                self.position = 0.0;
            }
        }
    }

    fn handle_engine_event(&mut self, ev: engine::Event) {
        match ev {
            engine::Event::DurationKnown(d) => {
                self.duration = d;
                self.error_streak = 0;
                // Sessão de reprodução: linha com data/arquivo/codec/res/fps no
                // log automático (os metadados chegam pelo `Metrics` do motor).
                if let Some(path) = self.idx.and_then(|i| self.playlist.files.get(i)).cloned() {
                    if let Some(info) = self.metrics.snapshot().track {
                        self.session.track(&path, &info);
                    }
                }
            }
            engine::Event::PositionHint { position, duration } => {
                self.position = position;
                if duration > 0.0 {
                    self.duration = duration;
                }
            }
            engine::Event::State(t) => {
                self.transport = t;
                if t == engine::Transport::Stopped {
                    self.position = 0.0;
                    self.drop_video();
                }
            }
            engine::Event::TrackEnded => self.on_track_ended(),
            engine::Event::Error(e) => {
                // Thread do motor morta = anomalia no log automático.
                if e.kind() == engine::ErrorKind::Engine {
                    self.session.engine_dead(&e.to_string());
                }
                // Seek falho não conta para o "pula a playlist" (a faixa toca).
                if e.kind() != engine::ErrorKind::Seek {
                    self.error_streak += 1;
                }
                self.status = self.error_text(e.kind()).to_owned();
                self.status_warn = true;
                eprintln!("engine: {e}");
            }
            engine::Event::VideoOpened(pipeline) => {
                self.video = Some(pipeline);
                self.video_frames.clear();
                self.video_shown = false;
                // Legenda externa: procurada ao lado do vídeo agora que sabemos
                // que a faixa tem imagem (arquivo só de áudio não tem overlay).
                self.load_subtitles();
            }
        }
    }

    /// Solta o pipeline de vídeo (a thread de decode recebe Stop no Drop).
    fn drop_video(&mut self) {
        self.video = None;
        self.video_frames.clear();
        self.video_shown = false;
        self.seek_preview_until = None;
        self.last_video_pts = 0.0;
        // Sem imagem não há onde desenhar a legenda (e o atraso é por faixa).
        self.subtitles = None;
        self.subtitle_delay = 0.0;
    }

    /// Mute/unmute (botão e tecla M), persistindo a preferência.
    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.engine.set_muted(self.muted);
        self.save_prefs();
    }

    /// Fullscreen pela viewport do eframe/egui.
    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.set_fullscreen(ctx, !self.fullscreen);
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        // Entrar em fullscreen é "assistir": os controles somem logo.
        if on {
            self.last_activity = Instant::now() - Duration::from_secs(10);
        } else {
            self.last_activity = Instant::now();
        }
        ctx.request_repaint();
    }

    /// Gira o vídeo em quartos (`delta` = +1/−1).
    fn rotate(&mut self, ctx: &egui::Context, delta: i32) {
        self.rotation = (i32::from(self.rotation) + delta).rem_euclid(4) as u8;
        if let Some(gpu) = self.video_gpu.as_ref() {
            gpu.set_rotation(self.rotation);
        }
        ctx.request_repaint();
    }

    /// Atalhos de teclado (só fora de campo de texto). O overlay volta
    /// sozinho porque qualquer tecla conta como atividade.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (f, r, space, m, left, right, esc, shift, s, z, x, d) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::R),
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::M),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.shift,
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Z),
                i.key_pressed(egui::Key::X),
                i.key_pressed(egui::Key::D),
            )
        });
        if f {
            self.toggle_fullscreen(ctx);
        }
        if esc && self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
        if d {
            // Overlay de diagnóstico (tecla D), só com vídeo.
            self.show_stats = !self.show_stats;
            ctx.request_repaint();
        }
        if r {
            self.rotate(ctx, if shift { -1 } else { 1 });
        }
        if space && self.idx.is_some() {
            self.toggle_play();
        }
        if m {
            self.toggle_mute();
        }
        if s {
            self.toggle_subtitles();
        }
        if z {
            self.adjust_subtitle_delay(-0.5);
        }
        if x {
            self.adjust_subtitle_delay(0.5);
        }
        if left {
            self.seek_relative(-5.0);
        }
        if right {
            self.seek_relative(5.0);
        }
    }

    /// Liga/desliga a legenda (tecla S e botão CC). Sem legenda carregada não
    /// faz nada (o botão fica desabilitado).
    fn toggle_subtitles(&mut self) {
        if self.subtitles.is_some() {
            self.subtitles_on = !self.subtitles_on;
        }
    }

    /// Ajusta o atraso da legenda (+ = aparece depois) e anuncia no overlay de
    /// status, que já existe na barra de controles.
    fn adjust_subtitle_delay(&mut self, delta: f64) {
        if self.subtitles.is_none() {
            return;
        }
        self.subtitle_delay = (self.subtitle_delay + delta).clamp(-60.0, 60.0);
        let lang = self.cfg.lang;
        self.status = format!("{} {:+.1} {}", i18n::t(lang, Key::SubtitleDelay), self.subtitle_delay, i18n::t(lang, Key::SecondsShort));
        self.status_warn = false;
    }

    /// Carrega a legenda externa do arquivo atual. Sem arquivo ao lado (ou sem
    /// nenhuma entrada válida), o player segue sem legenda.
    fn load_subtitles(&mut self) {
        self.subtitles = None;
        self.subtitle_delay = 0.0;
        let Some(path) = self.idx.and_then(|i| self.playlist.files.get(i)).cloned() else {
            return;
        };
        let Some(file) = subtitle::find_for(std::path::Path::new(&path)) else {
            return;
        };
        match subtitle::Subtitles::load(&file) {
            Ok(subs) => self.subtitles = Some(subs),
            Err(e) => eprintln!("engine: legenda: {e}"),
        }
    }

    /// Qualquer mouse/tecla recente mantém os controles visíveis.
    fn note_activity(&mut self, ctx: &egui::Context) {
        let active = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || !i.events.is_empty());
        if active {
            self.last_activity = Instant::now();
        }
    }

    /// Drena o pipeline de vídeo, escolhe o frame devido pelo relógio mestre e
    /// sobe para a GPU. Fim/erro de vídeo viram o mesmo fluxo da fase 2.
    fn pump_video(&mut self, ctx: &egui::Context) {
        let Some(pipeline) = self.video.as_ref() else { return };
        let generation = pipeline.generation;
        let has_audio = pipeline.has_audio;
        let par = pipeline.info.par;
        // hwdec da faixa: o log de sessão ganha uma linha quando o modo fica
        // conhecido (hardware, software, ou a queda para o software no meio).
        self.session.hwdec(&self.metrics.hw_usage());
        let mut messages = Vec::new();
        while let Some(msg) = pipeline.try_recv() {
            messages.push(msg);
        }
        let mut ended = false;
        let mut failure: Option<(engine::ErrorKind, String)> = None;
        for msg in messages {
            match msg {
                engine::VideoMsg::Frame { frame, generation: g } => {
                    if g == generation {
                        self.video_frames.push_back(frame);
                    }
                }
                engine::VideoMsg::End { generation: g } => ended = ended || g == generation,
                engine::VideoMsg::Error { generation: g, detail } => {
                    if g == generation && failure.is_none() {
                        failure = Some((engine::ErrorKind::Decode, detail));
                    }
                }
            }
        }

        let clock = self.engine.position_now();
        // Política de skip: sempre o frame mais novo já vencido; o backlog vira
        // descarte (`dropped`), nunca reprodução acelerada para "correr atrás".
        // Como todo frame vencido sai da fila aqui, a profundidade que sobra é
        // só de frames futuros — limitada pelo pacing do decode (lead de 250 ms).
        let decision = video::present::take_due(&mut self.video_frames, clock, video::present::LATE_WINDOW);
        if decision.dropped > 0 {
            self.metrics.note_dropped(decision.dropped as u64, decision.dropped_late as u64);
            self.session.dropped(decision.dropped, self.video_frames.len());
        }
        self.metrics.set_ring_depth(self.video_frames.len());

        let mut uploaded = false;
        if let Some(frame) = decision.frame {
            self.metrics.note_late(decision.late);
            self.session.late(decision.late);
            let frame_pts = frame.pts;
            if let Some(gpu) = &self.video_gpu {
                let started = Instant::now();
                match gpu.upload(&frame, par, self.rotation) {
                    Ok(()) => {
                        let elapsed = started.elapsed();
                        self.metrics.note_upload(elapsed);
                        self.session.upload(elapsed);
                        self.metrics.note_presented();
                        self.video_shown = true;
                        self.last_video_pts = frame_pts;
                        uploaded = true;
                    }
                    Err(video::gpu::UploadError::TooLarge(detail)) => failure = Some((engine::ErrorKind::TooLarge, detail)),
                    Err(video::gpu::UploadError::Invalid(detail)) => failure = Some((engine::ErrorKind::Decode, detail)),
                }
            }
        } else if self.transport.is_playing() {
            // Fila vazia mas o relógio andou: o vídeo está parado (decode
            // travado/abertura lenta) mesmo sem frame atrasado para medir.
            let behind = clock - self.last_video_pts;
            if behind > video::present::LATE_WINDOW {
                self.session.late(behind);
            }
        }

        if let Some((kind, detail)) = failure {
            eprintln!("engine: vídeo: {detail}");
            self.status = self.error_text(kind).to_owned();
            self.status_warn = true;
            self.error_streak += 1;
            self.engine.stop();
            self.drop_video();
            self.on_track_ended();
            return;
        }
        if ended && !has_audio {
            // Vídeo mudo acabou: com áudio, quem decide o fim é o ring.
            self.engine.stop();
            self.drop_video();
            self.on_track_ended();
            return;
        }
        if uploaded {
            ctx.request_repaint();
        }
    }

    /// Área central quando há vídeo: preto + paint callback do frame atual.
    fn show_video(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, egui::CornerRadius::ZERO, egui::Color32::BLACK);
        let response = ui.allocate_rect(rect, egui::Sense::click());
        if response.double_clicked() {
            self.toggle_fullscreen(ui.ctx());
        }
        if self.video_shown {
            if let Some(gpu) = self.video_gpu.as_ref() {
                let shape = gpu.shape(rect);
                ui.painter().add(shape);
            }
            self.show_subtitle(ui, rect);
        }
        if self.show_stats {
            self.stats_overlay(ui, rect);
        }
        // Senão: preto até o primeiro frame (o decode leva poucos ms).
    }

    /// Overlay técnico da tecla D (inglês monoespaçado de propósito: é
    /// diagnóstico, não UI de produto — logo não passa pelo i18n).
    fn stats_overlay(&self, ui: &egui::Ui, rect: egui::Rect) {
        let s = self.metrics.snapshot();
        let text = stats_text(&s, self.position, self.duration);
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::monospace(12.0), egui::Color32::from_gray(235));
        let pad = egui::vec2(10.0, 8.0);
        let bg = egui::Rect::from_min_size(rect.min + egui::vec2(12.0, 12.0), galley.size() + pad * 2.0);
        let painter = ui.painter();
        painter.rect_filled(bg, egui::CornerRadius::same(6), egui::Color32::from_black_alpha(190));
        painter.galley(bg.min + pad, galley, egui::Color32::from_gray(235));
    }

    /// Legenda por cima do vídeo: dentro do letterbox, com o estilo que o
    /// arquivo declara (SRT/VTT usam o padrão — caixa semitransparente; ASS usa
    /// contorno, cor, tamanho e alinhamento do script).
    fn show_subtitle(&self, ui: &egui::Ui, rect: egui::Rect) {
        let (Some(subs), Some(pipeline)) = (self.subtitles.as_ref(), self.video.as_ref()) else { return };
        if !self.subtitles_on {
            return;
        }
        // O atraso desloca o relógio de consulta: + = a fala entra depois.
        let time = self.engine.position_now() - self.subtitle_delay;
        let Some(cue) = subs.cue_at(time) else { return };
        let aspect = video::geometry::display_aspect(pipeline.info.width, pipeline.info.height, pipeline.info.par, self.rotation);
        let viewport = video::geometry::Rect::new(rect.min.x, rect.min.y, rect.width(), rect.height());
        let inner = video::geometry::letterbox(viewport, aspect, 1.0);
        if inner.w < 2.0 || inner.h < 2.0 {
            return;
        }
        // Tamanho em fração da altura da imagem (no SRT/VTT o padrão é 5%,
        // no ASS vem do `PlayRes` do script).
        let font_px = (inner.h * cue.style.size).clamp(9.0, (inner.h * 0.45).max(9.0));
        let margins = subtitle::Margins {
            left: cue.style.margin_l * inner.w,
            right: cue.style.margin_r * inner.w,
            top: cue.style.margin_v * inner.h,
            bottom: cue.style.margin_v * inner.h,
        };
        let font_id = egui::FontId::new(
            font_px,
            match cue.style.family {
                subtitle::Family::Monospace => egui::FontFamily::Monospace,
                subtitle::Family::Proportional => egui::FontFamily::Proportional,
            },
        );
        let wrap = (inner.w - margins.left - margins.right).max(1.0);
        let base = rgb(cue.style.color);
        // Uma passada por cor: o contorno do ASS precisa da MESMA galley pintada
        // no `OutlineColour`, e a cor por seção venceria o fallback do painter.
        let build = |override_color: Option<egui::Color32>| {
            let mut job = egui::text::LayoutJob::default();
            job.wrap.max_width = wrap;
            for segment in &cue.segments {
                let color = override_color.unwrap_or_else(|| segment.color.map_or(base, rgb));
                job.append(
                    &segment.text,
                    0.0,
                    egui::TextFormat { font_id: font_id.clone(), color, italics: segment.italic || cue.style.italic, ..Default::default() },
                );
            }
            job
        };
        let painter = ui.painter();
        let galley = painter.layout_job(build(None));
        let pad = if cue.style.backdrop == subtitle::Backdrop::Box { (font_px * 0.22).max(2.0) } else { 0.0 };
        let placed = subtitle::place(inner, (galley.size().x + pad * 2.0, galley.size().y + pad * 2.0), &cue.anchor, margins);
        let origin = egui::pos2(placed.x + pad, placed.y + pad);
        match cue.style.backdrop {
            subtitle::Backdrop::Box => {
                let bg = egui::Rect::from_min_size(egui::pos2(placed.x, placed.y), egui::vec2(placed.w, placed.h));
                painter.rect_filled(bg, egui::CornerRadius::same(4), egui::Color32::from_black_alpha(130));
            }
            subtitle::Backdrop::Outline => {
                let outline = painter.layout_job(build(Some(rgb(cue.style.outline))));
                let thickness = (font_px * 0.055).max(1.0);
                for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                    painter.galley(origin + egui::vec2(dx * thickness, dy * thickness), outline.clone(), rgb(cue.style.outline));
                }
            }
        }
        painter.galley(origin, galley.clone(), base);
        // "Negrito" do ASS: a egui não tem fonte pesada registrada, então a
        // segunda passada de meio pixel engrossa o traço.
        if cue.style.bold || cue.segments.iter().any(|s| s.bold) {
            painter.galley(origin + egui::vec2(0.6, 0.0), galley, base);
        }
    }

    /// Barra de controles do player (painel na janela, overlay no fullscreen).
    ///
    /// Linha 1: transporte + relógio (esquerda) e volume/tela cheia/legenda
    /// (direita). Linha 2: seek de largura cheia. Depois: "Retomar" (só com
    /// posição salva digna) e a linha de status. Os ícones são desenhados
    /// (`src/icons.rs`): os glyphs de fonte que estavam aqui (⏮ ▶ ⏸ ⏹ ⏭ 🔇 🔊)
    /// não existem nas fontes embutidas e saíam todos com o MESMO glifo de
    /// "falta" — o bug dos quatro botões idênticos do print.
    fn controls_panel(&mut self, ui: &mut egui::Ui) {
        let lang = self.cfg.lang;
        let t = |k: Key| i18n::t(lang, k);
        let can_play = self.idx.is_some();
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.horizontal(|ui| {
            if icons::icon_button(ui, icons::Icon::Previous, t(Key::Previous), false, can_play).on_hover_text(t(Key::Previous)).clicked() {
                self.play_prev();
            }
            let (icon, hover) = if self.transport.is_playing() { (icons::Icon::Pause, t(Key::Pause)) } else { (icons::Icon::Play, t(Key::Play)) };
            if icons::icon_button(ui, icon, hover, false, can_play).on_hover_text(hover).clicked() {
                self.toggle_play();
            }
            if icons::icon_button(ui, icons::Icon::Stop, t(Key::Stop), false, can_play).on_hover_text(t(Key::Stop)).clicked() {
                self.stop_playback();
            }
            if icons::icon_button(ui, icons::Icon::Next, t(Key::Next), false, can_play).on_hover_text(t(Key::Next)).clicked() {
                self.play_next();
            }
            ui.add_space(4.0);
            // Relógio em fonte de largura fixa: o tempo não "pula" a cada segundo.
            ui.label(egui::RichText::new(format!("{:>8} / {:>8}", fmt_time(self.position), fmt_time(self.duration))).monospace());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (icon, hover) = if self.muted { (icons::Icon::Muted, t(Key::Unmute)) } else { (icons::Icon::Volume, t(Key::Mute)) };
                if icons::icon_button(ui, icon, hover, self.muted, true).on_hover_text(hover).clicked() {
                    self.toggle_mute();
                }
                // O volume come o que sobra da linha: com vídeo aparecem girar
                // e legenda, e o slider encolhe em vez de estourar por cima do
                // relógio (janela de 480 pts).
                let trailing = if self.video.is_some() { 3.0 } else { 1.0 };
                let reserved = trailing * (icons::BUTTON.x + ui.spacing().item_spacing.x) + 4.0;
                ui.spacing_mut().slider_width = (ui.available_width() - reserved).clamp(40.0, 96.0);
                let volume =
                    ui.add(egui::Slider::new(&mut self.volume, 0.0..=100.0).show_value(false).trailing_fill(true)).on_hover_text(t(Key::Volume));
                if volume.changed() {
                    self.engine.set_volume((self.volume / 100.0) as f32);
                }
                if volume.drag_stopped() {
                    self.save_prefs();
                }
                let (icon, hover) = if self.fullscreen {
                    (icons::Icon::ExitFullscreen, t(Key::ExitFullscreen))
                } else {
                    (icons::Icon::Fullscreen, t(Key::Fullscreen))
                };
                if icons::icon_button(ui, icon, hover, self.fullscreen, true).on_hover_text(hover).clicked() {
                    self.toggle_fullscreen(ui.ctx());
                }
                if self.video.is_some() {
                    if icons::icon_button(ui, icons::Icon::Rotate, t(Key::Rotate), false, true).on_hover_text(t(Key::Rotate)).clicked() {
                        self.rotate(ui.ctx(), 1);
                    }
                    let cc_on = self.subtitles_on && self.subtitles.is_some();
                    let cc_hover = match self.subtitles.as_ref() {
                        Some(subs) => format!("{}\n{}", t(Key::Subtitles), subs.path),
                        None => t(Key::Subtitles).to_owned(),
                    };
                    if icons::icon_button(ui, icons::Icon::Subtitles, t(Key::Subtitles), cc_on, self.subtitles.is_some())
                        .on_hover_text(cc_hover)
                        .clicked()
                    {
                        self.toggle_subtitles();
                    }
                }
            });
        });
        ui.add_space(4.0);
        // Seek: durante o arrasto a UI manda; fora dele a posição vem do
        // relógio mestre (PositionHint). Antes o slider ficava com a largura
        // padrão do egui (100 pts) e aparecia como um slider órfão no canto.
        let dur = self.duration.max(0.0);
        let mut value = if self.scrubbing { self.scrub_value } else { self.position.clamp(0.0, dur) };
        let bar = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = ui.available_width().max(80.0);
                ui.add_enabled(dur > 0.0, egui::Slider::new(&mut value, 0.0..=dur.max(0.001)).show_value(false).trailing_fill(true))
            })
            .inner;
        if bar.drag_started() {
            self.scrubbing = true;
        }
        if self.scrubbing {
            self.scrub_value = value;
        }
        if bar.drag_stopped() || (bar.changed() && !bar.dragged()) {
            self.scrubbing = false;
            self.seek_to(value);
        }
        // Retomar: só quando a posição salva vale a pena e ainda estamos antes
        // dela (passado o ponto, não há para onde voltar).
        if let Some(point) = self.resume_point() {
            ui.add_space(2.0);
            let hover = i18n::t(lang, Key::ResumeAt).replace("{time}", &fmt_time(point));
            if ghost(ui, t(Key::ResumePosition)).on_hover_text(hover).clicked() {
                self.resume_playback(point);
            }
        }
        if !self.status.is_empty() {
            ui.horizontal(|ui| {
                if self.status_warn {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    icons::paint(ui, rect, icons::Icon::Warning, ui.visuals().warn_fg_color);
                }
                ui.label(egui::RichText::new(&self.status).small().weak());
            });
        }
    }

    /// Mensagem i18n do tipo de erro (o detalhe técnico vai só pro stderr).
    fn error_text(&self, kind: engine::ErrorKind) -> &'static str {
        let key = match kind {
            engine::ErrorKind::NoDevice => Key::ErrorNoOutputDevice,
            engine::ErrorKind::Open => Key::ErrorOpenFailed,
            engine::ErrorKind::Decode => Key::ErrorDecodeFailed,
            engine::ErrorKind::Seek => Key::ErrorSeekUnsupported,
            engine::ErrorKind::TooLarge => Key::ErrorVideoTooLarge,
            engine::ErrorKind::Engine => Key::ErrorEngineStopped,
        };
        i18n::t(self.cfg.lang, key)
    }

    /// Verifica atualizações agora (botão "Verificar atualizações").
    /// O resultado sai na linha de status (nada encontrado/erro); release
    /// nova abre o prompt do próprio updater.
    fn check_updates(&mut self, ctx: &egui::Context) {
        self.updater.check_now(ctx);
    }
}

impl Drop for PlayerApp {
    fn drop(&mut self) {
        // Sessão sempre consistente, mesmo fechando no meio da faixa.
        self.remember_current();
        resume::save(&self.resume);
        self.save_prefs();
        // Fecha o log automático (sem playback, não há arquivo para fechar).
        self.session.end();
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        // Instrumentação permanente: taxas/cadência e o CSV de LOCALPLAYER_STATS.
        self.metrics.note_repaint();
        self.metrics.tick(Instant::now());
        // Log automático: pressão de memória (amostra a cada 5 s).
        self.session.check_rss();
        // Update (craft_update): bombeia o check/instalação que rodam fora
        // da UI thread (mesmo desenho do Local-Keys).
        self.updater.tick(ctx);
        // Atividade do usuário e atalhos (F fullscreen, R/Shift+R rotação,
        // Espaço play/pause, M mute, ←/→ ±5 s).
        self.note_activity(ctx);
        self.handle_shortcuts(ctx);

        // Motor: drena eventos (posição, fim de faixa, erros) e, se a faixa
        // tem vídeo, escolhe o frame devido pelo relógio mestre.
        for ev in self.engine.poll() {
            self.handle_engine_event(ev);
        }
        self.pump_video(ctx);

        // Cadência de repaint: com vídeo, acorda quando o próximo frame vence
        // (não a 120 Hz fixos: repaint à toa compete com o decode pela CPU e
        // pela GPU — H3). Áudio puro a 10 Hz basta para a barra de seek.
        let previewing = self.seek_preview_until.is_some_and(|t| Instant::now() < t);
        if self.transport.is_playing() {
            if self.video.is_some() {
                let clock = self.engine.position_now();
                let wait_ms = match self.video_frames.front() {
                    Some(next) => ((next.pts - clock) * 1_000.0).clamp(4.0, 50.0) as u64,
                    // Fila vazia: acorda cedo para drenar o canal do decode.
                    None => 8,
                };
                ctx.request_repaint_after(Duration::from_millis(wait_ms));
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        } else if previewing {
            // Seek em pausa: repinta uns frames para o frame parado aparecer.
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.idx.is_some() {
            // Parado/pausado com faixa aberta: um frame a cada 250 ms garante
            // que os eventos de comando do motor (play/stop/erro) cheguem à UI
            // mesmo sem interação do usuário.
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        // Resume: grava no máximo a cada 5 s (write a cada tick = stutter).
        if self.transport.is_playing() && self.last_resume_save.elapsed() >= Duration::from_secs(5) {
            self.last_resume_save = Instant::now();
            self.remember_current();
            resume::save(&self.resume);
        }

        let lang = self.cfg.lang;
        let t = |k: Key| i18n::t(lang, k);

        // Check pedido ("Verificar atualizações"): nada encontrado/erro vira
        // linha de status; release nova abre o prompt do updater lá embaixo.
        match self.updater.status().clone() {
            craft_update::updater::Status::UpToDate => {
                self.status = t(Key::UpToDate).into();
                self.status_warn = false;
                self.updater.dismiss_status();
            }
            craft_update::updater::Status::Failed(e) => {
                self.status = e;
                self.status_warn = true;
                self.updater.dismiss_status();
            }
            _ => {}
        }

        // Listagem de "+ pasta" chegou (rodou fora da UI thread).
        if let Some(rx) = &self.dir_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(files) => {
                        for f in files {
                            if !self.playlist.files.contains(&f) {
                                self.playlist.files.push(f);
                            }
                        }
                        resume::save_playlist(&self.playlist);
                    }
                    Err(e) => {
                        self.status = e;
                        self.status_warn = true;
                    }
                }
                self.dir_rx = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(150));
            }
        }

        // Drag & drop de arquivos.
        let dropped: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().map(|d| d.path().display().to_string()).collect());
        if !dropped.is_empty() {
            let mut added = false;
            for f in dropped {
                if is_media(std::path::Path::new(&f)) && !self.playlist.files.contains(&f) {
                    self.playlist.files.push(f);
                    added = true;
                }
            }
            if added {
                resume::save_playlist(&self.playlist);
            }
        }

        // Em fullscreen a janela é só vídeo (os controles viram um overlay
        // flutuante embaixo); sem interação recente, até os painéis somem.
        let hide_chrome = self.video.is_some() && self.transport.is_playing() && self.last_activity.elapsed() >= Duration::from_millis(2500);
        // Controles escondidos = cursor escondido junto (a seta parada sobre a
        // imagem entrega que a UI está lá). Qualquer movimento/tecla repõe os dois.
        if hide_chrome {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        if !self.fullscreen && !hide_chrome {
            egui::containers::Panel::top("topo").show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Rótulo/hora só quando há largura: em janela estreita o
                    // right_to_left abaixo estoura pra esquerda (a titlebar já
                    // identifica o app).
                    if ui.available_width() >= 620.0 {
                        ui.strong(APP.name);
                        ui.label(egui::RichText::new(&self.now).small().weak());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if lab_ui::settings_ui(ui, &mut self.cfg, false).0 {
                            theme::apply(ctx, self.cfg.theme);
                            let _ = config::save(APP_ID, &self.cfg);
                        }
                        if ghost(ui, t(Key::CheckUpdates)).clicked() {
                            self.check_updates(ctx);
                        }
                        if self.updater.status() == &craft_update::updater::Status::Checking {
                            ui.spinner();
                        }
                    });
                });
            });
        }

        if !hide_chrome && !self.fullscreen {
            egui::containers::Panel::bottom("controles").show(ui, |ui| self.controls_panel(ui));

            // Playlist: painel inferior fixo.
            egui::containers::Panel::bottom("playlist").exact_size(140.0).resizable(false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(t(Key::Items));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(format!("+ {}", t(Key::AddFile))).clicked() {
                            if let Some(p) = pick_media() {
                                if !self.playlist.files.contains(&p) {
                                    self.playlist.files.push(p);
                                    resume::save_playlist(&self.playlist);
                                }
                            }
                        }
                        if ui.button(format!("+ {}", t(Key::AddFolder))).clicked() {
                            if let Some(dir) = pick_dir() {
                                // Fora da UI thread: read_dir em pasta de
                                // rede/nuvem (OneDrive) tem a MESMA lentidão do
                                // Explorer — na UI thread travava a janela.
                                let (tx, rx) = std::sync::mpsc::channel();
                                std::thread::spawn(move || {
                                    let _ = tx.send(list_media(&dir));
                                });
                                self.dir_rx = Some(rx);
                            }
                        }
                        if ui.add_enabled(!self.playlist.files.is_empty(), egui::Button::new(t(Key::Clear))).clicked() {
                            self.playlist.files.clear();
                            self.idx = None;
                            self.now.clear();
                            self.status.clear();
                            self.status_warn = false;
                            self.resume_at = None;
                            self.engine.stop();
                            self.transport = engine::Transport::Stopped;
                            self.position = 0.0;
                            self.duration = 0.0;
                            self.drop_video();
                            resume::save_playlist(&self.playlist);
                        }
                    });
                });
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // Sem clone por frame: só os índices.
                    for i in 0..self.playlist.files.len() {
                        let name = file_name(&self.playlist.files[i]);
                        let is_now = self.idx == Some(i);
                        let clicked = playlist_row(ui, &name, is_now, &self.playlist.files[i]).clicked();
                        if clicked {
                            self.error_streak = 0;
                            self.select(i);
                        }
                    }
                });
            });
        }

        // Centro: vídeo (paint callback wgpu) ou a dica de áudio/playlist.
        egui::CentralPanel::default().show(ui, |ui| {
            if self.video.is_some() {
                self.show_video(ui);
            } else {
                ui.centered_and_justified(|ui| {
                    if self.idx.is_some() {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&self.now).heading());
                            ui.label(egui::RichText::new(t(Key::AudioOnlyHint)).weak());
                        });
                    } else {
                        ui.label(egui::RichText::new(t(Key::DragMediaHere)).weak());
                    }
                });
            }
        });

        // Fullscreen: controles flutuantes por cima do vídeo (somem com a
        // inatividade, voltam com mouse/tecla).
        if self.fullscreen && !hide_chrome {
            egui::Area::new(egui::Id::unique("controles-fullscreen")).anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0)).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(520.0);
                    self.controls_panel(ui);
                });
            });
        }

        // Prompt de update (craft_update): modal próprio do crate, por cima
        // de tudo.
        match self.updater.show(ctx) {
            craft_update::updater::Action::None => {}
            craft_update::updater::Action::OpenUrl(url) => open_url(&url),
            craft_update::updater::Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

// ── diálogos (mesmo desenho do Local-Keys) ───────────────────────────────

/// Cores derivadas do tema vigente, pro que sobrou de diálogo: os ghost
/// buttons.
#[derive(Clone, Copy)]
struct Skin {
    muted: egui::Color32,
    /// Lavagem de hover (ghost buttons).
    hover: egui::Color32,
}

impl Skin {
    fn of(ui: &egui::Ui) -> Self {
        Self::from_visuals(ui.visuals())
    }

    fn from_visuals(v: &egui::Visuals) -> Self {
        let text = v.override_text_color.unwrap_or(v.widgets.noninteractive.fg_stroke.color);
        Self {
            muted: text.gamma_multiply(0.6),
            hover: if v.dark_mode { egui::Color32::from_white_alpha(18) } else { egui::Color32::from_black_alpha(14) },
        }
    }
}

/// Ação de linha: transparente até o hover (o "ghost button" do local-pdf).
fn ghost(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let skin = Skin::of(ui);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, skin.muted);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + 14.0, 24.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
    if resp.hovered() {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(6), skin.hover);
    }
    let pos = rect.center() - galley.size() / 2.0;
    ui.painter().galley(pos, galley, skin.muted);
    resp
}

/// Linha da playlist: largura toda, altura fixa e o par hover/seleção do tema
/// (mesmo tratamento de lista dos irmãos, com o caminho completo no tooltip).
fn playlist_row(ui: &mut egui::Ui, name: &str, selected: bool, path: &str) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, name));
    let visuals = ui.visuals();
    let fill = if selected {
        visuals.selection.bg_fill
    } else if response.hovered() {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    if fill != egui::Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, visuals.widgets.inactive.corner_radius, fill);
    }
    let color = if selected { visuals.strong_text_color() } else { visuals.text_color() };
    let galley = ui.painter().layout_no_wrap(name.to_owned(), egui::TextStyle::Button.resolve(ui.style()), color);
    let pos = egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
    response.on_hover_text(path)
}

/// Nome do arquivo para exibição (fallback: o caminho inteiro).
fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// Progresso mínimo que vale retomar (10 s) e a folga até o fim (30 s).
const RESUME_MIN_SECS: f64 = 10.0;
const RESUME_END_SECS: f64 = 30.0;

/// Posição salva digna de "Retomar": pelo menos [`RESUME_MIN_SECS`] de faixa
/// ouvida e pelo menos [`RESUME_END_SECS`] antes do fim (retomar quase no fim
/// não faz sentido). Duração desconhecida ou inválida não convida a retomar.
fn resumable(saved: f64, duration: f64) -> Option<f64> {
    if !saved.is_finite() || !duration.is_finite() || duration <= 0.0 {
        return None;
    }
    (saved >= RESUME_MIN_SECS && saved <= duration - RESUME_END_SECS).then_some(saved)
}

/// Onde o play abre a faixa: **sempre do começo** por padrão — retomar é uma
/// ação explícita (`use_resume`) e só vale se a posição salva for `resumable`.
fn open_position(use_resume: bool, saved: Option<f64>, duration: f64) -> f64 {
    match saved {
        Some(saved) if use_resume => resumable(saved, duration).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// "m:ss" (ou "h:mm:ss" acima de 1 h); duração desconhecida = 0:00.
fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0:00".into();
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Cor RGB do parse de legenda para o `Color32` do egui.
fn rgb(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// Texto do overlay da tecla D: uma linha por grupo, colunas de largura fixa.
///
/// Unidade por linha (todo campo de tempo em ms; fps e Hz separados) e
/// `pos`/`dura` com o relógio mestre contra a duração da faixa — diagnóstico
/// sem virar planilha embaralhada.
fn stats_text(s: &metrics::Snapshot, position: f64, duration: f64) -> String {
    let mut rows: Vec<(&str, Vec<String>)> = Vec::new();
    if let Some(t) = &s.track {
        rows.push((
            "track",
            vec![
                t.video_codec.clone().unwrap_or_else(|| "-".into()),
                format!("{}x{}", t.width, t.height),
                format!("{:.3} fps", t.fps),
                t.container.clone(),
            ],
        ));
        rows.push((
            "audio",
            vec![
                t.audio_codec.clone().unwrap_or_else(|| "no audio".into()),
                format!("{:.2} Mb/s", t.bitrate.map_or(0.0, |b| b as f64 / 1e6)),
                format!("file {:.0} MB", t.file_size.map_or(0.0, |b| b as f64 / (1024.0 * 1024.0))),
            ],
        ));
    }
    rows.push(("hwdec", vec![s.hw.summary(), format!("{} frames", s.hw.frames)]));
    // Sem duração do motor, a do container serve; posição pode passar do fim
    // por um frame (o relógio mestre corre antes do `TrackEnded`).
    let total = if duration > 0.0 { duration } else { s.track.as_ref().map_or(0.0, |t| t.duration) };
    let percent = if total > 0.0 { (position / total * 100.0).clamp(0.0, 100.0) } else { 0.0 };
    rows.push(("pos", vec![format!("{} / {}", fmt_time(position), fmt_time(total)), format!("{percent:.1}%")]));
    rows.push(("decode", vec![format!("{:5.1} fps", s.decode_fps)]));
    rows.push((
        "latency",
        vec![format!("avg {:6.2} ms", s.decode_avg_ms), format!("p95 {:6.2} ms", s.decode_p95_ms), format!("max {:6.2} ms", s.decode_max_ms)],
    ));
    rows.push((
        "present",
        vec![
            format!("{:5.1} fps", s.display_fps),
            format!("shown {}", s.presented),
            format!("drop {} (late {})", s.dropped, s.dropped_late),
            format!("ring {}", s.ring_depth),
        ],
    ));
    rows.push(("late", vec![format!("{:+.0} ms", s.late_ms)]));
    rows.push((
        "upload",
        vec![format!("{:5.2} ms", s.upload_ms), format!("avg {:5.2} ms", s.upload_avg_ms), format!("max {:5.2} ms", s.upload_max_ms)],
    ));
    rows.push(("repaint", vec![format!("{:5.1} ms", s.repaint_ms), format!("{:.1} Hz", s.repaint_fps)]));
    rows.push(("memory", vec![format!("rss {:.0} MB", s.rss_mb), format!("peak {:.0} MB", s.rss_peak_mb), format!("cache {:.1} MB", s.cache_mb)]));
    stats_table(&rows)
}

/// Tabela monoespaçada: rótulo numa coluna, células alinhadas pela maior de
/// cada coluna. Sem espaços sobrando no fim das linhas.
fn stats_table(rows: &[(&str, Vec<String>)]) -> String {
    use std::fmt::Write as _;
    let label_w = rows.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0) + 2;
    let columns = rows.iter().map(|(_, cells)| cells.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for (_, cells) in rows {
        for (i, cell) in cells.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for (label, cells) in rows {
        let _ = write!(out, "{label:<label_w$}");
        for (i, cell) in cells.iter().enumerate() {
            let _ = write!(out, "{cell}");
            if i + 1 < cells.len() {
                for _ in 0..widths[i].saturating_sub(cell.chars().count()) {
                    out.push(' ');
                }
                out.push_str("  ");
            }
        }
        out.push('\n');
    }
    let used = out.trim_end().len();
    out.truncate(used);
    out
}

fn pick_media() -> Option<String> {
    #[cfg(windows)]
    {
        rfd::FileDialog::new().add_filter("Mídia", MEDIA_EXTS).pick_file().map(|p| p.display().to_string())
    }
    #[cfg(not(windows))]
    {
        None // Linux: caminho digitado (política do lab: AppImage enxuto)
    }
}

fn pick_dir() -> Option<String> {
    #[cfg(windows)]
    {
        rfd::FileDialog::new().pick_folder().map(|p| p.display().to_string())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn list_media(dir: &str) -> Result<Vec<String>, String> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| is_media(&e.path()))
        .map(|e| e.path().display().to_string())
        .collect();
    files.sort_by_key(|f| f.to_lowercase());
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colunas de largura fixa: o início de cada célula casa entre as linhas e
    /// nenhuma linha termina com espaço.
    #[test]
    fn tabela_de_stats_alinha_colunas() {
        let rows = [
            ("a", vec!["1".to_owned(), "xx".to_owned()]),
            ("bbb", vec!["22".to_owned(), "y".to_owned()]),
            ("cc", vec!["333".to_owned(), "zzz".to_owned()]),
        ];
        let text = stats_table(&rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(lines[0].find("xx"), lines[1].find("y"), "{text}");
        assert_eq!(lines[0].find("xx"), lines[2].find("zzz"), "{text}");
        assert!(lines[0].starts_with("a    "), "{text:?}");
        for line in &lines {
            assert!(!line.ends_with(' '), "{line:?}");
        }
    }

    /// A linha `pos/dura` existe mesmo sem faixa aberta e usa o relógio do
    /// player quando a duração do motor é conhecida.
    #[test]
    fn overlay_mostra_pos_e_duracao() {
        let snapshot = metrics::Snapshot::default();
        let text = stats_text(&snapshot, 65.0, 130.0);
        assert!(text.contains("pos"), "{text}");
        assert!(text.contains("1:05 / 2:10"), "{text}");
        assert!(text.contains("50.0%"), "{text}");
        // Sem duração não inventa percentual.
        let text = stats_text(&snapshot, 5.0, 0.0);
        assert!(text.contains("0:05 / 0:00"), "{text}");
        assert!(text.contains("0.0%"), "{text}");
    }

    /// O overlay ganha a linha do hwdec com o decoder em uso (MF vs SW).
    #[test]
    fn overlay_mostra_hwdec() {
        let snapshot = metrics::Snapshot {
            hw: metrics::HwUsage { mode: metrics::HwMode::Hardware, backend: Some("Media Foundation"), frames: 12, ..Default::default() },
            ..Default::default()
        };
        let text = stats_text(&snapshot, 0.0, 0.0);
        assert!(text.contains("hwdec"), "{text}");
        assert!(text.contains("Media Foundation · hardware"), "{text}");
        assert!(text.contains("12 frames"), "{text}");
    }

    /// O botão "Retomar" só aparece fora das bordas: pelo menos 10 s de faixa
    /// ouvida e pelo menos 30 s antes do fim.
    #[test]
    fn retomar_so_vale_fora_das_bordas() {
        // Começo: menos de 10 s não vale retomar.
        assert_eq!(resumable(9.9, 3600.0), None);
        assert_eq!(resumable(0.0, 3600.0), None);
        assert_eq!(resumable(10.0, 3600.0), Some(10.0));
        // Fim: 30 s antes do fim ainda vale; a menos disso, não.
        assert_eq!(resumable(3570.0, 3600.0), Some(3570.0));
        assert_eq!(resumable(3570.1, 3600.0), None);
        assert_eq!(resumable(3600.0, 3600.0), None);
        // Duração desconhecida ou inválida, e posição salva inválida, não convidam.
        assert_eq!(resumable(120.0, 0.0), None);
        assert_eq!(resumable(120.0, f64::NAN), None);
        assert_eq!(resumable(120.0, f64::INFINITY), None);
        assert_eq!(resumable(f64::NAN, 3600.0), None);
        // Faixa curta: as duas bordas só coexistem na janela exata de 10 s.
        assert_eq!(resumable(10.0, 40.0), Some(10.0));
        assert_eq!(resumable(11.0, 40.0), None);
    }

    /// Play sem o botão "Retomar" começa SEMPRE do zero; só o pedido explícito
    /// (e válido) abre na posição salva.
    #[test]
    fn play_sem_retomar_comeca_do_zero() {
        assert_eq!(open_position(false, Some(1200.0), 3600.0), 0.0);
        assert_eq!(open_position(false, None, 3600.0), 0.0);
        assert_eq!(open_position(true, Some(1200.0), 3600.0), 1200.0);
        // Pedido explícito com posição fora da faixa digna também abre do começo.
        assert_eq!(open_position(true, Some(2.0), 3600.0), 0.0);
        assert_eq!(open_position(true, Some(3590.0), 3600.0), 0.0);
        assert_eq!(open_position(true, None, 3600.0), 0.0);
    }
}
