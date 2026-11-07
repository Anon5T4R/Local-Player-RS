//! CLI de diagnóstico do player (parse manual: nenhuma dependência nova).
//!
//! - `local-player-rs <arquivo...>` — comportamento normal (janela);
//! - `local-player-rs --probe <arquivo>` — metadados do container e sai;
//! - `local-player-rs --bench <arquivo> [--seconds N]` — decode puro, sem
//!   janela/áudio/GPU, e sai com o relatório.
//!
//! No Windows o release é subsistema GUI (sem console): quando não há stdout
//! herdado, o CLI se anexa ao console do PowerShell e escreve em `CONOUT$`
//! (com a code page em UTF-8), senão a saída se perderia.

use std::io::Write;

use crate::engine;

/// O que a linha de comando pediu.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Abre a janela (args viram mídia da playlist, como sempre).
    Run,
    Help,
    Version,
    Probe(String),
    Bench {
        path: String,
        seconds: Option<f64>,
    },
}

/// Interpreta os args (sem o nome do programa). `Err` = uso incorreto.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut probe: Option<String> = None;
    let mut bench: Option<(String, Option<f64>)> = None;
    let mut help = false;
    let mut version = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => help = true,
            "-V" | "--version" => version = true,
            "--probe" => {
                let path = args.get(i + 1).ok_or("--probe precisa de um arquivo")?;
                probe = Some(path.clone());
                i += 1;
            }
            "--bench" => {
                let path = args.get(i + 1).ok_or("--bench precisa de um arquivo")?;
                bench = Some((path.clone(), None));
                i += 1;
            }
            "--seconds" => {
                let raw = args.get(i + 1).ok_or("--seconds precisa de um número")?;
                let secs: f64 = raw.parse().map_err(|_| format!("--seconds inválido: {raw}"))?;
                if !secs.is_finite() || secs <= 0.0 {
                    return Err(format!("--seconds precisa ser > 0 (recebido {raw})"));
                }
                match bench.as_mut() {
                    Some((_, slot)) => *slot = Some(secs),
                    None => return Err("--seconds só vale com --bench".into()),
                }
                i += 1;
            }
            other if other.starts_with("--") => return Err(format!("opção desconhecida: {other}")),
            _ => {}
        }
        i += 1;
    }
    if help {
        return Ok(Command::Help);
    }
    if version {
        return Ok(Command::Version);
    }
    match (probe, bench) {
        (Some(_), Some(_)) => Err("use --probe ou --bench, não os dois".into()),
        (Some(path), None) => Ok(Command::Probe(path)),
        (None, Some((path, seconds))) => Ok(Command::Bench { path, seconds }),
        (None, None) => Ok(Command::Run),
    }
}

/// Executa o que o CLI pediu. `None` = sem comando de diagnóstico (segue para
/// a janela). `Some(código)` = o processo deve sair com esse código.
pub fn run(args: &[String]) -> Option<i32> {
    let command = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            let mut out = output();
            let _ = writeln!(out, "local-player-rs: {e}\n");
            print_help(&mut out);
            return Some(2);
        }
    };
    match command {
        Command::Run => None,
        Command::Help => {
            let mut out = output();
            print_help(&mut out);
            Some(0)
        }
        Command::Version => {
            let mut out = output();
            let _ = writeln!(out, "local-player-rs {}", env!("CARGO_PKG_VERSION"));
            Some(0)
        }
        Command::Probe(path) => {
            let mut out = output();
            match engine::probe(&path) {
                Ok(report) => {
                    let _ = print_probe(&mut out, &report);
                    Some(0)
                }
                Err(e) => {
                    let _ = writeln!(out, "probe: {e}");
                    Some(1)
                }
            }
        }
        Command::Bench { path, seconds } => {
            let mut out = output();
            match engine::bench(&path, seconds) {
                Ok(report) => {
                    let _ = print_bench(&mut out, &report);
                    Some(0)
                }
                Err(e) => {
                    let _ = writeln!(out, "bench: {e}");
                    Some(1)
                }
            }
        }
    }
}

fn print_help(out: &mut dyn Write) {
    let _ = writeln!(
        out,
        "local-player-rs {} — player local de mídia (Rust puro)\n\
         \n\
         USO:\n\
         \x20 local-player-rs [arquivo...]                  abre a janela\n\
         \x20 local-player-rs --probe <arquivo>              metadados do container e sai\n\
         \x20 local-player-rs --bench <arquivo> [--seconds N]  decode puro (sem UI/áudio/GPU)\n\
         \x20 local-player-rs --version | --help\n\
         \n\
         DIAGNÓSTICO EM TEMPO REAL:\n\
         \x20 tecla D               overlay de estatísticas no vídeo\n\
         \x20 LOCALPLAYER_STATS=<arquivo.csv|1>   uma linha por segundo\n\
         \x20 LOCALPLAYER_FRAME_CACHE_MB=<MB>     teto do cache de frames (padrão 128)",
        env!("CARGO_PKG_VERSION")
    );
}

/// Formata segundos como `h:mm:ss.mmm`.
fn fmt_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "desconhecida".into();
    }
    let total_ms = (secs * 1000.0).round() as u64;
    let (h, m, s, ms) = (total_ms / 3_600_000, (total_ms / 60_000) % 60, (total_ms / 1000) % 60, total_ms % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// bit/s → "Mb/s" legível.
fn fmt_bitrate(bps: u64) -> String {
    let mbps = bps as f64 / 1e6;
    format!("{mbps:.2} Mb/s")
}

fn print_probe(out: &mut dyn Write, r: &engine::ProbeReport) -> std::io::Result<()> {
    writeln!(out, "file       {}", r.path)?;
    writeln!(out, "container  {}", r.container)?;
    writeln!(out, "duration   {} ({:.3} s)", fmt_duration(r.duration), r.duration)?;
    if let Some(size) = r.file_size {
        writeln!(out, "size       {:.2} MB", size as f64 / (1024.0 * 1024.0))?;
    }
    if let Some(bps) = r.bitrate {
        writeln!(out, "bitrate    {}", fmt_bitrate(bps))?;
    }
    match &r.video {
        Some(v) => {
            writeln!(out, "video      {} {}x{} {:.3} fps {}", v.codec, v.width, v.height, v.fps, v.pixel_format)?;
            if v.has_alpha {
                writeln!(out, "           alpha: yes")?;
            }
            if let Some(bps) = v.bitrate {
                writeln!(out, "           stream bitrate {}", fmt_bitrate(bps))?;
            }
        }
        None => writeln!(out, "video      (nenhum)")?,
    }
    if r.audio.is_empty() {
        writeln!(out, "audio      (nenhum)")?;
    }
    for (i, a) in r.audio.iter().enumerate() {
        let bits = a.bits_per_sample.map_or(String::new(), |b| format!(" {b} bits"));
        writeln!(out, "audio #{}   {} {} Hz {} ch{}", i + 1, a.codec, a.sample_rate, a.channels, bits)?;
    }
    Ok(())
}

fn print_bench(out: &mut dyn Write, r: &engine::BenchReport) -> std::io::Result<()> {
    writeln!(out, "file       {}", r.path)?;
    writeln!(out, "stream     {} {}x{} {:.3} fps", r.codec, r.width, r.height, r.fps)?;
    writeln!(out, "decoded    {} frames ({:.2} s de mídia) em {:.3} s de parede", r.frames, r.media_secs, r.wall_secs)?;
    writeln!(out, "decode     {:.1} fps  mean {:.2} ms  p95 {:.2} ms  max {:.2} ms", r.decode_fps, r.mean_ms, r.p95_ms, r.max_ms)?;
    if !r.buckets.is_empty() {
        writeln!(out, "por trecho de 10 s:")?;
        for b in &r.buckets {
            writeln!(out, "  {:>6.0}s  {:>6} frames  {:>7.1} fps", b.media_start, b.frames, b.fps)?;
        }
    }
    writeln!(out, "cache      hits {}  misses {}  seeks {}  evicted {}", r.cache_hits, r.cache_misses, r.seeks, r.evicted)?;
    writeln!(out, "decoder    {:.3} ms/sample   cache {:.1} MB", r.decoder_ms_per_sample, r.cache_mb)?;
    writeln!(out, "rss        {:.1} MB (pico {:.1} MB)", r.rss_mb, r.rss_peak_mb)?;
    Ok(())
}

/// Saída do CLI: stdout herdado quando existe; senão o console do processo pai
/// (Windows GUI), com UTF-8.
#[cfg(windows)]
fn output() -> Box<dyn Write> {
    use windows::Win32::System::Console::{AttachConsole, GetStdHandle, SetConsoleOutputCP, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
    // SAFETY: chamadas de console do próprio processo; handles inválidos viram
    // `Err`/artefato e o fallback é o stdout do Rust.
    let inherited = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok();
    if !inherited {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            let _ = SetConsoleOutputCP(65001);
        }
    }
    if !inherited {
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            return Box::new(file);
        }
    }
    Box::new(std::io::stdout())
}

#[cfg(not(windows))]
fn output() -> Box<dyn Write> {
    Box::new(std::io::stdout())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn sem_flags_e_player_normal() {
        assert_eq!(parse(&args(&["filme.mkv", "outro.mp4"])), Ok(Command::Run));
        assert_eq!(parse(&[]), Ok(Command::Run));
    }

    #[test]
    fn probe_consome_o_arquivo() {
        assert_eq!(parse(&args(&["--probe", "a.mkv"])), Ok(Command::Probe("a.mkv".into())));
    }

    #[test]
    fn bench_com_segundos() {
        assert_eq!(parse(&args(&["--bench", "a.mkv", "--seconds", "12.5"])), Ok(Command::Bench { path: "a.mkv".into(), seconds: Some(12.5) }));
        assert_eq!(parse(&args(&["--bench", "a.mkv"])), Ok(Command::Bench { path: "a.mkv".into(), seconds: None }));
    }

    #[test]
    fn versao_e_help_vencem() {
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
        assert_eq!(parse(&args(&["-V"])), Ok(Command::Version));
    }

    #[test]
    fn erros_de_uso_sao_claros() {
        assert!(parse(&args(&["--probe"])).is_err());
        assert!(parse(&args(&["--probe", "a.mkv", "--bench", "b.mkv"])).is_err());
        assert!(parse(&args(&["--seconds", "3"])).is_err());
        assert!(parse(&args(&["--bench", "a.mkv", "--seconds", "0"])).is_err());
        assert!(parse(&args(&["--bench", "a.mkv", "--seconds", "abc"])).is_err());
        assert!(parse(&args(&["--nope"])).is_err());
    }

    #[test]
    fn duracoes_formatadas() {
        assert_eq!(fmt_duration(65.5), "1:05.500");
        assert_eq!(fmt_duration(3725.0), "1:02:05.000");
        assert_eq!(fmt_duration(-1.0), "desconhecida");
    }
}
