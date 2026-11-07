//! Local Player RS — player de mídia 100% local e offline em Rust puro.
//!
//! Fases 2–4: shell egui/eframe da suíte (playlist de arquivo/pasta,
//! drag-and-drop, "abrir com", tema, i18n e autoupdate), motor de áudio de
//! verdade (`engine.rs`, relógio mestre = áudio), **vídeo SW** (demux/decode
//! pelo hub do FilmCraft em thread própria, frame escolhido por PTS e pintado
//! numa textura wgpu com shader YUV→RGB, `video/`) e **legenda externa**
//! SRT/VTT (`subtitle.rs`) desenhada pela egui sobre o vídeo.
//!
//! "Nada pesado na UI thread": listagem de pasta e os decoders (áudio e vídeo)
//! rodam em threads próprias e falam com a UI por canais; a UI só sobe o frame
//! devido para a GPU.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod queue;
mod resume;
mod subtitle;
mod video;

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
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

    let cfg = config::load(APP_ID);

    // "Abrir com" do Windows manda os caminhos como args (pode ser mais de um).
    let args: Vec<String> = std::env::args().skip(1).collect();

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
            let mut app = PlayerApp::new(cfg, args, cc.wgpu_render_state.as_ref());
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
    /// Posição de retomada aguardando a duração (se caiu no fim, volta a 0).
    pending_resume: f64,
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
}

impl PlayerApp {
    fn new(cfg: Config, args: Vec<String>, render_state: Option<&eframe::egui_wgpu::RenderState>) -> Self {
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
        let mut engine = engine::PlayerEngine::new();
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
            pending_resume: 0.0,
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
        };
        if let Some(i) = idx {
            app.select(i);
        }
        app
    }

    /// Seleciona a faixa `i` e manda o motor abrir (retomando a posição salva).
    fn select(&mut self, i: usize) {
        let Some(path) = self.playlist.files.get(i).cloned() else {
            return;
        };
        self.remember_current();
        let start = resume::position_of(&self.resume, &path).unwrap_or(0.0);
        self.idx = Some(i);
        self.now = file_name(&path);
        self.status.clear();
        self.position = start;
        self.duration = 0.0;
        self.scrubbing = false;
        self.pending_resume = start;
        self.drop_video();
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

    fn seek_to(&mut self, secs: f64) {
        let secs = if self.duration > 0.0 { secs.clamp(0.0, self.duration) } else { secs.max(0.0) };
        self.position = secs;
        self.engine.seek(secs);
        if let Some(v) = self.video.as_mut() {
            v.seek(secs);
        }
        self.video_frames.clear();
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
                // Posição salva caiu no fim (ou além do fim): recomeça do zero.
                if self.pending_resume > 0.0 && d > 0.0 && self.pending_resume >= d - 3.0 {
                    self.pending_resume = 0.0;
                    self.position = 0.0;
                    self.engine.seek(0.0);
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
                // Seek falho não conta para o "pula a playlist" (a faixa toca).
                if e.kind() != engine::ErrorKind::Seek {
                    self.error_streak += 1;
                }
                self.status = format!("⚠ {}", self.error_text(e.kind()));
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
        let (f, r, space, m, left, right, esc, shift, s, z, x) = ctx.input(|i| {
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
            )
        });
        if f {
            self.toggle_fullscreen(ctx);
        }
        if esc && self.fullscreen {
            self.set_fullscreen(ctx, false);
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
        let mut uploaded = false;
        // Descarta no máximo o ring inteiro por tick: um atraso grande não
        // segura a fila, e o frame atual fica na tela até o próximo vencer.
        if let Some(frame) = video::present::take_due(&mut self.video_frames, clock, 3) {
            if let Some(gpu) = &self.video_gpu {
                match gpu.upload(&frame, par, self.rotation) {
                    Ok(()) => {
                        self.video_shown = true;
                        uploaded = true;
                    }
                    Err(video::gpu::UploadError::TooLarge(detail)) => failure = Some((engine::ErrorKind::TooLarge, detail)),
                    Err(video::gpu::UploadError::Invalid(detail)) => failure = Some((engine::ErrorKind::Decode, detail)),
                }
            }
        }

        if let Some((kind, detail)) = failure {
            eprintln!("engine: vídeo: {detail}");
            self.status = format!("⚠ {}", self.error_text(kind));
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
        // Senão: preto até o primeiro frame (o decode leva poucos ms).
    }

    /// Legenda por cima do vídeo: dentro do letterbox, na base, com caixa
    /// semitransparente e a fonte da egui (o contorno é o fundo escuro).
    fn show_subtitle(&self, ui: &egui::Ui, rect: egui::Rect) {
        let (Some(subs), Some(pipeline)) = (self.subtitles.as_ref(), self.video.as_ref()) else { return };
        if !self.subtitles_on {
            return;
        }
        // O atraso desloca o relógio de consulta: + = a fala entra depois.
        let time = self.engine.position_now() - self.subtitle_delay;
        let Some(text) = subs.text_at(time) else { return };
        let aspect = video::geometry::display_aspect(pipeline.info.width, pipeline.info.height, pipeline.info.par, self.rotation);
        let viewport = video::geometry::Rect::new(rect.min.x, rect.min.y, rect.width(), rect.height());
        let inner = video::geometry::letterbox(viewport, aspect, 1.0);
        if inner.w < 2.0 || inner.h < 2.0 {
            return;
        }
        let font_size = (inner.h * 0.05).clamp(14.0, 44.0);
        let pad_x = (font_size * 0.45).max(6.0);
        let pad_y = (font_size * 0.18).max(3.0);
        let margin = (inner.h * 0.035).clamp(8.0, 56.0);
        let wrap = (inner.w * 0.92 - pad_x * 2.0).max(1.0);
        let painter = ui.painter();
        let galley = painter.layout(text.to_owned(), egui::FontId::proportional(font_size), egui::Color32::WHITE, wrap);
        let placed = subtitle::place_bottom(inner, (galley.size().x + pad_x * 2.0, galley.size().y + pad_y * 2.0), margin);
        let bg = egui::Rect::from_min_size(egui::pos2(placed.x, placed.y), egui::vec2(placed.w, placed.h));
        painter.rect_filled(bg, egui::CornerRadius::same(4), egui::Color32::from_black_alpha(130));
        painter.galley(bg.min + egui::vec2(pad_x, pad_y), galley, egui::Color32::WHITE);
    }

    /// Barra de controles do player (painel na janela, overlay no fullscreen).
    fn controls_panel(&mut self, ui: &mut egui::Ui) {
        let lang = self.cfg.lang;
        let t = |k: Key| i18n::t(lang, k);
        let can_play = self.idx.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(can_play, egui::Button::new("⏮")).on_hover_text(t(Key::Previous)).clicked() {
                self.play_prev();
            }
            let (icon, hover) = if self.transport.is_playing() { ("⏸", t(Key::Pause)) } else { ("▶", t(Key::Play)) };
            if ui.add_enabled(can_play, egui::Button::new(icon)).on_hover_text(hover).clicked() {
                self.toggle_play();
            }
            if ui.add_enabled(can_play, egui::Button::new("⏹")).on_hover_text(t(Key::Stop)).clicked() {
                self.stop_playback();
            }
            if ui.add_enabled(can_play, egui::Button::new("⏭")).on_hover_text(t(Key::Next)).clicked() {
                self.play_next();
            }
            ui.label(egui::RichText::new(format!("{} / {}", fmt_time(self.position), fmt_time(self.duration))).monospace());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (icon, hover) = if self.muted { ("🔇", t(Key::Unmute)) } else { ("🔊", t(Key::Mute)) };
                if ui.selectable_label(self.muted, icon).on_hover_text(hover).clicked() {
                    self.toggle_mute();
                }
                let volume = ui.add(egui::Slider::new(&mut self.volume, 0.0..=100.0).show_value(false)).on_hover_text(t(Key::Volume));
                if volume.changed() {
                    self.engine.set_volume((self.volume / 100.0) as f32);
                }
                if volume.drag_stopped() {
                    self.save_prefs();
                }
                let (icon, hover) = if self.fullscreen { ("⤡", t(Key::ExitFullscreen)) } else { ("⛶", t(Key::Fullscreen)) };
                if ui.selectable_label(self.fullscreen, icon).on_hover_text(hover).clicked() {
                    self.toggle_fullscreen(ui.ctx());
                }
                if self.video.is_some() && ui.selectable_label(false, "⟳").on_hover_text(t(Key::Rotate)).clicked() {
                    self.rotate(ui.ctx(), 1);
                }
                let cc_on = self.subtitles_on && self.subtitles.is_some();
                let cc_hover = match self.subtitles.as_ref() {
                    Some(subs) => format!("{}\n{}", t(Key::Subtitles), subs.path),
                    None => t(Key::Subtitles).to_owned(),
                };
                if self.video.is_some()
                    && ui.add_enabled(self.subtitles.is_some(), egui::Button::selectable(cc_on, "CC")).on_hover_text(cc_hover).clicked()
                {
                    self.toggle_subtitles();
                }
            });
        });
        // Seek: durante o arrasto a UI manda; fora dele a posição vem do
        // relógio mestre (PositionHint).
        let dur = self.duration.max(0.0);
        let mut value = if self.scrubbing { self.scrub_value } else { self.position.clamp(0.0, dur) };
        let bar = ui.add_enabled(dur > 0.0, egui::Slider::new(&mut value, 0.0..=dur.max(0.001)).show_value(false));
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
        if !self.status.is_empty() {
            ui.label(egui::RichText::new(&self.status).small().weak());
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
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
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

        // Cadência de repaint: vídeo toca a ~120 Hz (o vsync limita); áudio a
        // 10 Hz basta para a barra de seek.
        let previewing = self.seek_preview_until.is_some_and(|t| Instant::now() < t);
        if self.transport.is_playing() {
            let ms = if self.video.is_some() { 8 } else { 100 };
            ctx.request_repaint_after(Duration::from_millis(ms));
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
                self.updater.dismiss_status();
            }
            craft_update::updater::Status::Failed(e) => {
                self.status = e;
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
                    Err(e) => self.status = format!("⚠ {e}"),
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
                            self.engine.stop();
                            self.transport = engine::Transport::Stopped;
                            self.position = 0.0;
                            self.duration = 0.0;
                            self.drop_video();
                            resume::save_playlist(&self.playlist);
                        }
                    });
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Sem clone por frame: só os índices.
                    for i in 0..self.playlist.files.len() {
                        let name = file_name(&self.playlist.files[i]);
                        let is_now = self.idx == Some(i);
                        if ui.selectable_label(is_now, &name).clicked() {
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
                    ui.set_min_width(420.0);
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

/// Nome do arquivo para exibição (fallback: o caminho inteiro).
fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
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
