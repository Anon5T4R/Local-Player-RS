//! Teste REAL do TLS 100% Rust no caminho do update (experimento do app).
//!
//! Precisa de rede (api.github.com), por isso é `#[ignore]` — não roda no
//! `cargo test` normal nem no CI. Rode com:
//!
//! ```text
//! cargo test --test tls_update_check -- --ignored --nocapture
//! ```
//!
//! Verifica duas coisas:
//! 1. o handshake do provider puro (`rustls-rustcrypto`) com api.github.com e
//!    qual protocolo/suite o GitHub negociou (o ureq não expõe a suite, então o
//!    `probe_pure` faz o handshake direto com a mesma configuração);
//! 2. o caminho real do craft-update (`Http` + `check_for_update`) contra o
//!    repositório deste app — o mesmo que o botão "Verificar atualizações" usa.

use craft_update::{App, Http};

/// O mesmo `App` do `src/main.rs` (o binário não é importável por um teste de
/// integração; os campos públicos permitem repetir a entrada).
const LOCAL_PLAYER_RS: App = App {
    id: "local-player-rs",
    name: "Local Player RS",
    tagline: "Media player",
    repo: "Anon5T4R/Local-Player-RS",
    former_ids: &[],
    former_names: &[],
    icon_png: None,
    publisher: "Local Player RS contributors",
};

#[test]
#[ignore = "rede: precisa de api.github.com"]
fn github_update_check_works_with_the_pure_provider() {
    // Provider em uso (deve ser pure: é o caminho único, sem fallback padrão).
    println!("tls_provider (antes) = {}", craft_update::tls::status_line());

    // 1. Handshake direto: protocolo + suite que o GitHub negociou.
    let negotiated = craft_update::tls::probe_pure("api.github.com").expect("handshake puro com api.github.com");
    println!("github negociou = {negotiated}");

    // 2. O caminho real do update (GET na API de releases do repo do app).
    let http = Http::new("Local-Player-RS/TLS-test").expect("Http::new");
    let release = craft_update::check_for_update(&LOCAL_PLAYER_RS, &http, "0.0.0").expect("check de update via provider puro");
    match release {
        Some(r) => println!("release encontrada = {} ({})", r.tag, r.assets.len()),
        None => println!("release encontrada = nenhuma (tag/repo sem release publicada)"),
    }

    // Nada caiu para o provider padrão no caminho.
    assert!(craft_update::tls::is_pure(), "o provider trocou: {}", craft_update::tls::status_line());
    println!("tls_provider (depois) = {}", craft_update::tls::status_line());
}
