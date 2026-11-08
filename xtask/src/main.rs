//! Tarefas de build/CI do Local Player RS (padrão local-office, versão mínima).
//!
//! - `cargo xtask version X.Y.Z` — bump atômico: Cargo.toml do app + lab-ui +
//!   `refresh do Cargo.lock.
//! - `cargo xtask check` — confere que as 2 versões estão sincronizadas.
//!
//! Zero dependências: manipulação de texto com std só (política de deps).

use std::fs;
use std::process::Command;

fn main() {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("version") => {
            let v = std::env::args().nth(2).unwrap_or_else(|| die("uso: cargo xtask version X.Y.Z"));
            version(&v);
        }
        Some("check") => check(),
        _ => die("uso: cargo xtask <version|check>"),
    }
}

/// (arquivo, prefixo da linha de versão)
fn version_slots() -> Vec<(String, String)> {
    vec![("Cargo.toml".into(), "version = ".into()), ("lab-ui/Cargo.toml".into(), "version = ".into())]
}

fn read_version(path: &str, prefix: &str) -> String {
    let text = fs::read_to_string(path).unwrap_or_else(|e| die(&format!("ler {path}: {e}")));
    let line = text.lines().find(|l| l.trim_start().starts_with(prefix)).unwrap_or_else(|| die(&format!("{path}: linha `{prefix}` não encontrada")));
    line.trim().strip_prefix(prefix).unwrap().trim().trim_matches('"').to_string()
}

fn set_version(path: &str, prefix: &str, v: &str) {
    let text = fs::read_to_string(path).unwrap_or_else(|e| die(&format!("ler {path}: {e}")));
    let mut out = String::with_capacity(text.len());
    let mut done = false;
    for line in text.lines() {
        if !done && line.trim_start().starts_with(prefix) {
            let indent = line.len() - line.trim_start().len();
            let quote = "\"";
            out.push_str(&line[..indent]);
            out.push_str(prefix);
            out.push_str(quote);
            out.push_str(v);
            out.push_str(quote);
            done = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    fs::write(path, out).unwrap_or_else(|e| die(&format!("escrever {path}: {e}")));
}

fn version(v: &str) {
    if v.split('.').count() != 3 || !v.chars().all(|c| c.is_ascii_digit() || c == '.') {
        die(&format!("versão inválida: {v} (esperado X.Y.Z)"));
    }
    for (path, prefix) in version_slots() {
        set_version(&path, &prefix, v);
        println!("  {path} -> {v}");
    }
    // Refresh do lock (só o pacote local muda).
    let st = Command::new("cargo").args(["update", "--workspace"]).status().unwrap_or_else(|e| die(&format!("cargo update: {e}")));
    if !st.success() {
        die("cargo update falhou");
    }
    println!("  Cargo.lock atualizado");
    check();
}

fn check() {
    let mut vers = Vec::new();
    for (path, prefix) in version_slots() {
        vers.push((path.clone(), read_version(&path, &prefix)));
    }
    let first = vers[0].1.clone();
    let mut ok = true;
    for (path, v) in &vers {
        if v != &first {
            eprintln!("DESSINCRONIZADO: {path} = {v} (esperado {first})");
            ok = false;
        }
    }
    if ok {
        println!("versões sincronizadas: {first}");
    } else {
        std::process::exit(1);
    }
}

fn die(msg: &str) -> ! {
    eprintln!("xtask: {msg}");
    std::process::exit(1);
}
