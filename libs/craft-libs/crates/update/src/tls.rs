//! // local-player-rs: provider cripto do rustls no caminho do craft-update.
//!
//! Experimento do Local Player RS (não é padrão da suíte): as requisições de update (check de
//! release e download do instalador) usam o provider **100% Rust** (`rustls-rustcrypto`) como
//! caminho ÚNICO. Um handshake que falhe vira erro normal — visível na linha de status do app
//! (botão "Verificar atualizações") e no log de sessão — em vez de cair calado para outro
//! provider.
//!
//! Válvula de emergência, **desligada por padrão**: `LOCALPLAYER_TLS_FALLBACK=1` faz a MESMA
//! requisição ser repetida uma vez com o provider padrão do ureq (ring) quando o handshake puro
//! falha por TLS/transporte, registrando `tls_provider=fallback(motivo)`. É para o João usar se
//! um servidor do release parar de negociar com o provider puro — nunca o caminho normal.

use std::io::ErrorKind;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use rustls::crypto::CryptoProvider;

const PURE: u8 = 0;
const STANDARD: u8 = 1;

/// Provider em uso. Começa puro; só a válvula opt-in manda para o padrão.
static MODE: AtomicU8 = AtomicU8::new(PURE);
/// Sobe a cada mudança de provider — o app compara com a última que logou.
static VERSION: AtomicU64 = AtomicU64::new(0);
/// Motivo da troca (`fallback(...)`), preenchido no máximo uma vez.
static REASON: Mutex<Option<String>> = Mutex::new(None);

fn reason() -> MutexGuard<'static, Option<String>> {
    REASON.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Lê o ambiente uma vez: `LOCALPLAYER_TLS_FALLBACK=1` liga a válvula de
/// emergência (retry no provider padrão quando o puro falha).
fn fallback_enabled() -> bool {
    static ONCE: OnceLock<bool> = OnceLock::new();
    *ONCE.get_or_init(|| std::env::var("LOCALPLAYER_TLS_FALLBACK").is_ok_and(|v| v.trim() == "1"))
}

/// O provider puro que o ureq recebe em cada agente.
pub fn pure_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls_rustcrypto::provider())
}

/// O provider puro está em uso? (É o caminho único; só deixa de valer depois
/// de uma troca de válvula de emergência.)
pub fn is_pure() -> bool {
    MODE.load(Ordering::Relaxed) == PURE
}

/// Versão do estado do provider (muda quando ele troca).
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

/// Linha para o log de sessão do app: `tls_provider=pure` ou
/// `tls_provider=fallback(motivo)` (a segunda só com a válvula ligada).
pub fn status_line() -> String {
    if is_pure() {
        return "tls_provider=pure".into();
    }
    match reason().clone() {
        Some(r) => format!("tls_provider=fallback({r})"),
        None => "tls_provider=standard".into(),
    }
}

/// Só faz sentido repetir no provider padrão com a válvula ligada, enquanto o
/// puro está ativo, e quando a falha foi de TLS/transporte (um 404/500 é
/// resposta, não handshake).
pub fn may_fallback(e: &ureq::Error) -> bool {
    fallback_enabled() && is_pure() && looks_like_tls_or_transport(e)
}

fn looks_like_tls_or_transport(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::Protocol(_) | ureq::Error::ConnectionFailed => true,
        // Um servidor que não negocia a suite derruba a conexão no meio do
        // handshake: para o ureq isso chega como I/O, não como erro de TLS.
        ureq::Error::Io(io) => matches!(
            io.kind(),
            ErrorKind::UnexpectedEof
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::BrokenPipe
                | ErrorKind::NotConnected
                | ErrorKind::InvalidData
        ),
        _ => false,
    }
}

/// Registra a troca para o padrão (uma vez; o retry usa o outro conjunto de
/// agentes). Só é chamada com a válvula `LOCALPLAYER_TLS_FALLBACK=1`.
pub fn note_fallback(e: &ureq::Error) {
    if MODE.swap(STANDARD, Ordering::Relaxed) == STANDARD {
        return;
    }
    let why = compact(&format!("{e}"));
    *reason() = Some(why.clone());
    VERSION.fetch_add(1, Ordering::Relaxed);
    eprintln!("update: TLS puro falhou ({why}); LOCALPLAYER_TLS_FALLBACK=1, repetindo com o provider padrão");
}

/// Texto curto e numa linha só (o motivo vai para o log de sessão).
fn compact(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 140 {
        let mut short: String = flat.chars().take(139).collect();
        short.push('…');
        short
    } else {
        flat
    }
}

/// Diagnóstico do teste ignorado do app (`tests/tls_update_check.rs`): um
/// handshake real com `host:443` usando o provider puro, devolvendo a versão de
/// protocolo e a suite que o servidor negociou. Não é chamado em produção.
pub fn probe_pure(host: &str) -> Result<String, String> {
    let provider = pure_provider();
    let verifier = rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|e| format!("verifier: {e}"))?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .map_err(|e| format!("builder: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| format!("host inválido: {e}"))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| format!("client: {e}"))?;
    let mut sock = std::net::TcpStream::connect((host, 443u16)).map_err(|e| format!("connect: {e}"))?;
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(15)));
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).map_err(|e| format!("handshake: {e}"))?;
    }
    let version = conn.protocol_version().map_or_else(|| "-".to_string(), |v| format!("{v:?}"));
    let suite = conn.negotiated_cipher_suite().map_or_else(|| "-".to_string(), |s| format!("{:?}", s.suite()));
    Ok(format!("{version} {suite}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numa linha só e curto: o motivo vai para o log de sessão.
    #[test]
    fn the_reason_fits_one_log_line() {
        assert_eq!(compact("a\nb   c"), "a b c");
        let long = compact(&"x".repeat(400));
        assert_eq!(long.chars().count(), 140, "{long}");
        assert!(long.ends_with('…'));
    }

    /// Resposta HTTP (404/500) não é motivo para repetir o handshake; queda de
    /// conexão e erro de TLS são.
    #[test]
    fn only_transport_trouble_triggers_a_retry() {
        assert!(looks_like_tls_or_transport(&ureq::Error::Tls("no shared cipher")));
        assert!(looks_like_tls_or_transport(&ureq::Error::Io(std::io::Error::from(ErrorKind::UnexpectedEof))));
        assert!(!looks_like_tls_or_transport(&ureq::Error::StatusCode(500)));
        assert!(!looks_like_tls_or_transport(&ureq::Error::Timeout(ureq::Timeout::Global)));
        assert!(!looks_like_tls_or_transport(&ureq::Error::HostNotFound));
    }
}
