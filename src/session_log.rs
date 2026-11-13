//! Log automático de sessão — sempre ligado, sem o usuário precisar ligar nada.
//!
//! Um arquivo por execução do app em:
//!
//! - Windows: `%APPDATA%\local-player-rs\logs\`
//! - Linux: `$XDG_DATA_HOME/local-player-rs/logs` (ou `~/.local/share/local-player-rs/logs`)
//!
//! Cada faixa tocada escreve uma linha `track` (data, arquivo, container, codec,
//! resolução, fps, bitrate, duração). Durante a reprodução, **anomalias** viram
//! linhas `event=…`:
//!
//! - `stall` — o frame apresentado ficou > 250 ms atrás do relógio mestre (uma
//!   linha por episódio, com o pior atraso e quantos frames foram descartados);
//! - `drop_burst` — um tick descartou ≥ 4 frames (o apresentador pulou o atraso);
//! - `upload_ms` — upload de textura acima de 30 ms (disputa com a UI);
//! - `rss_mb` — working set acima de 700 MB (pressão de memória), e `rss_back`
//!   quando volta;
//! - `engine_dead` — a thread do motor de áudio morreu.
//!
//! Rotação: no máximo [`KEEP`] arquivos (um por execução); os mais velhos são
//! apagados ao abrir um novo. Custo sempre-ligado: uma linha por evento (com
//! throttle por categoria), flush imediato. O CSV completo por segundo continua
//! sendo só com `LOCALPLAYER_STATS` (`src/metrics.rs`).
//!
//! Tudo é best-effort: falha de I/O desliga o log sem afetar o player (uma vez,
//! no stderr). Sem `unwrap`/`panic`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::metrics::{HwMode, HwUsage, TrackInfo};

/// Arquivos de log mantidos (um por execução).
const KEEP: usize = 10;
/// Atraso (s) que caracteriza travada.
const STALL_SECS: f64 = 0.250;
/// Descartar isto ou mais frames num tick = burst.
const DROP_BURST: usize = 4;
/// Upload de textura acima disto (ms) é anomalia.
const UPLOAD_MS: f64 = 30.0;
/// Working set acima disto (MB) é pressão de memória.
const RSS_HIGH_MB: f64 = 700.0;
/// Intervalo mínimo entre linhas da mesma categoria (anti-spam).
const DROP_THROTTLE: Duration = Duration::from_secs(1);
const UPLOAD_THROTTLE: Duration = Duration::from_secs(1);
const RSS_CHECK: Duration = Duration::from_secs(5);
const RSS_LOG: Duration = Duration::from_secs(30);

/// Episódio de travada em andamento.
struct Stall {
    start: Instant,
    last: Instant,
    worst: f64,
    drops: u64,
}

struct State {
    dir: Option<PathBuf>,
    file: Option<BufWriter<File>>,
    stall: Option<Stall>,
    last_drop: Option<Instant>,
    last_upload: Option<Instant>,
    last_rss_check: Instant,
    last_rss_log: Option<Instant>,
    rss_high: bool,
    /// Modo de hwdec já registrado para a faixa (uma linha por modo).
    hw_logged: Option<HwMode>,
    /// Linha do provider TLS do update, esperando a primeira escrita (uma por
    /// execução; execução sem playback não cria log).
    pending_tls: Option<String>,
}

/// Escritor do log de sessão (compartilhado por referência; só a UI escreve).
pub struct SessionLog {
    state: Mutex<State>,
}

impl Default for SessionLog {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionLog {
    /// Resolve a pasta de logs e prepara o estado (o arquivo nasce no primeiro
    /// evento — uma execução sem playback não deixa arquivo vazio).
    pub fn new() -> Self {
        let dir = log_dir();
        if let Some(d) = &dir {
            if let Err(e) = std::fs::create_dir_all(d) {
                eprintln!("log: nao consegui criar {}: {e}", d.display());
                return Self { state: Mutex::new(State::disabled()) };
            }
        }
        Self { state: Mutex::new(State::new(dir)) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Começou a tocar `path` (metadados do container como o motor os viu).
    pub fn track(&self, path: &str, info: &TrackInfo) {
        let mut st = self.lock();
        close_stall(&mut st, "track_change");
        // Cada faixa decide de novo: o modo de hwdec é registrado quando conhecido.
        st.hw_logged = None;
        let video = match (&info.video_codec, info.width, info.height) {
            (Some(c), w, h) if w > 0 && h > 0 => {
                let pixfmt = info.pixel_format.as_deref().map_or(String::new(), |p| format!(" {p}"));
                format!("{c} {w}x{h} {:.3}fps{pixfmt}", info.fps)
            }
            (Some(c), _, _) => (*c).clone(),
            _ => "-".to_string(),
        };
        let bitrate = info.bitrate.map_or("-".to_string(), |b| format!("{:.2}Mb/s", b as f64 / 1e6));
        let policy = if filmcraft_codecs::hw::hardware_decoding() { "on" } else { "off" };
        let backend = filmcraft_codecs::hw::hw_backend().unwrap_or("-");
        let line = format!(
            "track file=\"{}\" container={} video=\"{}\" audio={} bitrate={} dur={:.3}s hwdec={} backend={}",
            escape(path),
            info.container,
            video,
            info.audio_codec.as_deref().unwrap_or("-"),
            bitrate,
            info.duration,
            policy,
            backend
        );
        write_line(&mut st, &line);
    }

    /// Decoder usado pela faixa (hardware vs SW), uma linha por modo. O hub
    /// conta os quadros de hardware; quando o modo fica conhecido, isso entra no
    /// log — inclusive a queda para o software no meio do stream.
    pub fn hwdec(&self, usage: &HwUsage) {
        if matches!(usage.mode, HwMode::Idle | HwMode::Pending) {
            return;
        }
        let mut st = self.lock();
        if st.hw_logged == Some(usage.mode) {
            return;
        }
        st.hw_logged = Some(usage.mode);
        let line = format!(
            "event=hwdec mode={} backend={} frames={} sessions={} declined={} fallbacks={}",
            usage.mode.label(),
            usage.backend.unwrap_or("-"),
            usage.frames,
            usage.sessions,
            usage.declined,
            usage.fallbacks
        );
        write_line(&mut st, &line);
    }

    /// Atraso (s) do último frame apresentado vs relógio mestre.
    pub fn late(&self, secs: f64) {
        if !secs.is_finite() {
            return;
        }
        let mut st = self.lock();
        if secs > STALL_SECS {
            let now = Instant::now();
            match st.stall.as_mut() {
                Some(s) => {
                    s.last = now;
                    s.worst = s.worst.max(secs);
                }
                None => st.stall = Some(Stall { start: now, last: now, worst: secs, drops: 0 }),
            }
        } else {
            close_stall(&mut st, "recovered");
        }
    }

    /// Frames descartados num tick (`ring` = profundidade que restou).
    pub fn dropped(&self, n: usize, ring: usize) {
        if n == 0 {
            return;
        }
        let mut st = self.lock();
        if let Some(s) = st.stall.as_mut() {
            s.drops += n as u64;
        }
        if n >= DROP_BURST && throttle(&mut st.last_drop, DROP_THROTTLE) {
            let line = format!("event=drop_burst n={n} ring={ring}");
            write_line(&mut st, &line);
        }
    }

    /// Custo do upload de textura do frame.
    pub fn upload(&self, elapsed: Duration) {
        let ms = elapsed.as_secs_f64() * 1e3;
        if ms <= UPLOAD_MS {
            return;
        }
        let mut st = self.lock();
        if throttle(&mut st.last_upload, UPLOAD_THROTTLE) {
            let line = format!("event=upload_ms={ms:.1}");
            write_line(&mut st, &line);
        }
    }

    /// Amostra o RSS (a cada 5 s) e registra pressão de memória.
    pub fn check_rss(&self) {
        let mut st = self.lock();
        // Sem sessão de reprodução aberta não há o que correlacionar: não cria log.
        if st.file.is_none() {
            return;
        }
        let now = Instant::now();
        if now.saturating_duration_since(st.last_rss_check) < RSS_CHECK {
            return;
        }
        st.last_rss_check = now;
        let (rss, peak) = crate::metrics::rss_mb();
        if rss >= RSS_HIGH_MB {
            let due = !st.rss_high || st.last_rss_log.is_none_or(|t| now.saturating_duration_since(t) >= RSS_LOG);
            if due {
                let line = format!("event=rss_mb={rss:.0} peak_mb={peak:.0}");
                write_line(&mut st, &line);
                st.last_rss_log = Some(now);
            }
            st.rss_high = true;
        } else if st.rss_high {
            let line = format!("event=rss_back mb={rss:.0} peak_mb={peak:.0}");
            write_line(&mut st, &line);
            st.rss_high = false;
        }
    }

    /// A thread do motor morreu (o app fica sem áudio e sem eventos).
    pub fn engine_dead(&self, detail: &str) {
        let mut st = self.lock();
        let line = format!("event=engine_dead detail=\"{}\"", escape(detail));
        write_line(&mut st, &line);
    }

    /// Provider TLS do update (`tls_provider=pure` ou `fallback(motivo)`).
    ///
    /// O puro é o normal: fica pendente e sai antes da primeira linha da
    /// sessão, para uma execução sem playback não criar log. Uma troca para o
    /// provider padrão é anomalia (o update podia ter falhado sem ela) e abre o
    /// log na hora.
    pub fn tls(&self, line: &str) {
        let mut st = self.lock();
        if line.starts_with("tls_provider=pure") {
            st.pending_tls = Some(line.to_string());
        } else {
            st.pending_tls = None;
            write_line(&mut st, line);
        }
    }

    /// Check de update pedido pelo usuário começou. A linha pendente do provider
    /// TLS (`tls_provider=...`) sai antes desta — é o jeito de ver o provider em
    /// uso no log sem depender de tocar uma faixa.
    pub fn update_check_started(&self) {
        let mut st = self.lock();
        write_line(&mut st, "event=update_check start");
    }

    /// Check de update pedido pelo usuário falhou. A linha pendente do provider
    /// TLS (`tls_provider=...`) sai antes desta, então uma falha de handshake
    /// com o provider puro deixa o rastro completo no log.
    pub fn update_check_failed(&self, detail: &str) {
        let mut st = self.lock();
        let line = format!("event=update_check_failed detail=\"{}\"", escape(detail));
        write_line(&mut st, &line);
    }

    /// Fecha a sessão (fim normal do app). Sem playback no run, não cria arquivo.
    pub fn end(&self) {
        let mut st = self.lock();
        if st.file.is_none() {
            return;
        }
        close_stall(&mut st, "session_end");
        write_line(&mut st, "session end");
        st.file = None;
    }
}

impl State {
    fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            file: None,
            stall: None,
            last_drop: None,
            last_upload: None,
            last_rss_check: Instant::now(),
            last_rss_log: None,
            rss_high: false,
            hw_logged: None,
            pending_tls: None,
        }
    }

    fn disabled() -> Self {
        Self::new(None)
    }
}

/// `true` se já passou `min` desde `last` (e atualiza `last` então).
fn throttle(last: &mut Option<Instant>, min: Duration) -> bool {
    let now = Instant::now();
    match last {
        Some(t) if now.saturating_duration_since(*t) < min => false,
        _ => {
            *last = Some(now);
            true
        }
    }
}

/// Fecha o episódio de travada (se houver) com uma linha só.
fn close_stall(st: &mut State, reason: &str) {
    let Some(s) = st.stall.take() else { return };
    let dur_ms = s.last.saturating_duration_since(s.start).as_secs_f64() * 1e3;
    let line = format!("event=stall reason={reason} worst_ms={:.0} dur_ms={dur_ms:.0} drops={}", s.worst * 1e3, s.drops);
    write_line(st, &line);
}

/// Escreve uma linha com timestamp UTC; abre o arquivo (e roda a rotação) na
/// primeira escrita. A linha pendente do provider TLS (se houver) sai antes.
fn write_line(st: &mut State, text: &str) {
    if let Some(tls) = st.pending_tls.take() {
        write_raw(st, &tls);
    }
    write_raw(st, text);
}

fn write_raw(st: &mut State, text: &str) {
    if st.file.is_none() {
        let Some(dir) = st.dir.clone() else { return };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("log: nao consegui criar {}: {e}", dir.display());
            st.dir = None;
            return;
        }
        rotate(&dir, KEEP);
        let name = format!("player-{}-{}.log", file_stamp(), std::process::id());
        let path = dir.join(name);
        match File::create(&path) {
            Ok(file) => {
                let mut out = BufWriter::new(file);
                let _ = writeln!(out, "{} session start pid={} version={}", now_stamp(), std::process::id(), env!("CARGO_PKG_VERSION"));
                let _ = out.flush();
                st.file = Some(out);
            }
            Err(e) => {
                eprintln!("log: nao consegui abrir {}: {e}", path.display());
                st.dir = None;
                return;
            }
        }
    }
    let failed = match st.file.as_mut() {
        Some(out) => writeln!(out, "{} {text}", now_stamp()).and_then(|_| out.flush()).is_err(),
        None => false,
    };
    if failed {
        // Disco cheio/permissão: desliga de vez em vez de tentar por frame.
        st.file = None;
        st.dir = None;
    }
}

/// Pasta de logs da plataforma (só do próprio app, nada compartilhado).
fn log_dir() -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(appdata).join("local-player-rs").join("logs"));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(data).join("local-player-rs").join("logs"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local").join("share").join("local-player-rs").join("logs"))
}

/// Mantém os `keep` arquivos `player-*.log` mais novos (nomes ordenam por data).
fn rotate(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("log")).unwrap_or(false)
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("player-"))
        })
        .collect();
    if logs.len() < keep {
        return;
    }
    logs.sort();
    // Os mais novos são os últimos; apaga o excedente a partir do começo.
    let cut = logs.len().saturating_sub(keep.saturating_sub(1));
    for old in logs.iter().take(cut) {
        let _ = std::fs::remove_file(old);
    }
}

/// `"` e quebras de linha não podem quebrar a linha do log.
fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '"' => '\'',
            '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

/// Timestamp ISO-8601 UTC do instante atual.
fn now_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format_epoch_utc(secs)
}

/// `YYYYMMDD-HHMMSS` UTC (nome do arquivo; ordem lexicográfica = cronológica).
fn file_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (y, m, d, hh, mm, ss) = civil(secs);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
fn format_epoch_utc(secs: u64) -> String {
    let (y, m, d, hh, mm, ss) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// (ano, mês, dia, hora, minuto, segundo) em UTC.
fn civil(secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3_600, (rem / 60) % 60, rem % 60)
}

/// Dias desde 1970-01-01 → (ano, mês, dia). Algoritmo de Howard Hinnant,
/// válido para todo o calendário gregoriano proleptico.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datas_utc_conhecidas() {
        assert_eq!(format_epoch_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_epoch_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        // 2024 é bissexto: 29 de fevereiro existe.
        assert_eq!(format_epoch_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn rotacao_mantem_os_mais_novos() {
        let dir = std::env::temp_dir().join(format!(
            "lp-rs-log-rot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        for day in 1..=12 {
            let path = dir.join(format!("player-202601{day:02}-000000-1.log"));
            assert!(std::fs::write(&path, b"x").is_ok());
        }
        // Um arquivo fora do padrão não é tocado.
        assert!(std::fs::write(dir.join("outro.txt"), b"x").is_ok());
        rotate(&dir, 10);
        let mut left: Vec<String> =
            std::fs::read_dir(&dir).map(|d| d.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).collect()).unwrap_or_default();
        left.sort();
        assert_eq!(left.len(), 10, "9 logs (o 10º nasce agora) + o outro.txt: {left:?}");
        assert!(left.contains(&"outro.txt".to_string()));
        assert!(!left.contains(&"player-20260101-000000-1.log".to_string()), "o mais velho sai");
        assert!(left.contains(&"player-20260112-000000-1.log".to_string()), "o mais novo fica");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessao_escreve_cabecalho_e_eventos() {
        let dir = std::env::temp_dir().join(format!(
            "lp-rs-log-sess-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let log = SessionLog { state: Mutex::new(State::new(Some(dir.clone()))) };
        let info = TrackInfo {
            container: "matroska".into(),
            video_codec: Some("h264".into()),
            audio_codec: Some("opus".into()),
            pixel_format: Some("yuv420p10le".into()),
            width: 1920,
            height: 1080,
            fps: 23.976,
            duration: 12.5,
            bitrate: Some(2_000_000),
            file_size: Some(3_000_000),
        };
        log.track("C:\\filme \"x\".mkv", &info);
        log.late(0.4);
        log.dropped(6, 3);
        log.late(0.4);
        log.late(0.01); // fecha o episódio
        log.end();
        let text = std::fs::read_dir(&dir)
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| e.ok())
            .and_then(|e| std::fs::read_to_string(e.path()).ok())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("session start"), "{text}");
        assert!(text.contains("track file=\"C:\\filme 'x'.mkv\" container=matroska"), "{text}");
        assert!(text.contains("1920x1080 23.976fps yuv420p10le"), "{text}");
        assert!(text.contains("event=stall reason=recovered"), "{text}");
        assert!(text.contains("drops=6"), "o burst entra no episódio: {text}");
        assert!(text.contains("event=drop_burst n=6 ring=3"), "{text}");
        assert!(text.contains("session end"), "{text}");
    }

    #[test]
    fn hwdec_entra_no_log_uma_vez_por_modo() {
        let dir = std::env::temp_dir().join(format!(
            "lp-rs-log-hw-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let log = SessionLog { state: Mutex::new(State::new(Some(dir.clone()))) };
        let info = TrackInfo {
            container: "matroska".into(),
            video_codec: Some("H.264".into()),
            width: 1920,
            height: 1080,
            fps: 24.0,
            ..TrackInfo::default()
        };
        log.track("a.mkv", &info);
        let hardware = HwUsage { mode: HwMode::Hardware, backend: Some("Media Foundation"), frames: 30, sessions: 1, declined: 0, fallbacks: 0 };
        // O mesmo modo não repete a linha; a queda para o software entra.
        log.hwdec(&hardware);
        log.hwdec(&hardware);
        log.hwdec(&HwUsage { mode: HwMode::HardwareThenSoftware, fallbacks: 1, ..hardware });
        log.end();
        let text = std::fs::read_dir(&dir)
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| e.ok())
            .and_then(|e| std::fs::read_to_string(e.path()).ok())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("hwdec=on"), "{text}");
        assert_eq!(text.matches("event=hwdec").count(), 2, "{text}");
        assert!(text.contains("mode=hardware backend=Media Foundation frames=30"), "{text}");
        assert!(text.contains("mode=hardware → software"), "{text}");
    }
}
