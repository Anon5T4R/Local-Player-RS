//! Embute os catálogos craft-i18n (`i18n/locales/<code>/localplayer.tsv`) no binário.
//! `CRAFT_I18N_DIR` sobrescreve o diretório; `CRAFT_I18N_REQUIRED=1` (releases)
//! transforma catálogo faltante em erro. Sem catálogos, a UI fica em inglês.

fn main() {
    println!("cargo::rerun-if-env-changed=CRAFT_I18N_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_I18N_REQUIRED");
    let required = std::env::var("CRAFT_I18N_REQUIRED").is_ok_and(|v| v.trim() == "1");
    let default_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../i18n");
    let dir = std::env::var_os("CRAFT_I18N_DIR").filter(|d| !d.is_empty()).map(std::path::PathBuf::from).unwrap_or(default_dir);
    let locales_dir = dir.join("locales");
    let level = if required { "error" } else { "warning" };
    let list = match std::fs::read_dir(&locales_dir) {
        Ok(l) => l,
        Err(e) => {
            println!("cargo::{level}=i18n catalogs: {e}; a UI fica em inglês");
            return;
        }
    };
    println!("cargo::rerun-if-changed={}", locales_dir.display());
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in list.flatten() {
        let path = entry.path();
        let Some(code) = path.file_name().and_then(std::ffi::OsStr::to_str).map(str::to_string) else { continue };
        let app = path.join("localplayer.tsv");
        if app.is_file() {
            println!("cargo::rerun-if-changed={}", app.display());
            entries.push((code, app));
        }
    }
    if entries.is_empty() {
        println!("cargo::{level}=i18n catalogs: nenhuma localplayer.tsv; a UI fica em inglês");
        return;
    }
    entries.sort();
    // Not canonicalized: on Windows that gives `\\?\C:\...`, which `include_str!` can't read.
    let mut o = String::from("/// (locale, catálogo do app), uma entrada por locale.\n");
    o.push_str("pub static CRAFT_I18N: &craft_i18n::Embedded = &[\n");
    for (code, app) in entries {
        o.push_str(&format!("    ({code:?}, \"\", include_str!({app:?})),\n"));
    }
    o.push_str("];\n");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_i18n.rs");
    if let Err(e) = std::fs::write(&out, o) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}
