//! Erros do motor de áudio.
//!
//! A UI nunca mostra `detail` cru: ela traduz o [`ErrorKind`] e usa o detalhe
//! só como diagnóstico (log/stderr). Assim o motor não depende de i18n.

use std::fmt;

/// Classe do erro — a UI mapeia cada uma para uma string do catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Não há saída de áudio (device default ausente/ocupado).
    NoDevice,
    /// O arquivo não abriu (formato desconhecido, corrompido, sem faixa).
    Open,
    /// O arquivo abriu mas a decodificação falhou no meio.
    Decode,
    /// O formato não permite seek (ou o seek falhou).
    Seek,
    /// A mídia declara dimensões absurdas (proteção contra OOM).
    TooLarge,
    /// A thread do motor morreu (falha interna fora dos catch_unwind).
    Engine,
}

/// Erro do motor de playback.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    #[error("no audio output device: {0}")]
    NoDevice(String),
    #[error("could not open the file: {0}")]
    Open(String),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("media dimensions are too large: {0}")]
    TooLarge(String),
    #[error("the audio engine stopped: {0}")]
    EngineStopped(String),
}

impl EngineError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NoDevice(_) => ErrorKind::NoDevice,
            Self::Open(_) => ErrorKind::Open,
            Self::Decode(_) => ErrorKind::Decode,
            Self::TooLarge(_) => ErrorKind::TooLarge,
            Self::EngineStopped(_) => ErrorKind::Engine,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoDevice => "no device",
            Self::Open => "open",
            Self::Decode => "decode",
            Self::Seek => "seek",
            Self::TooLarge => "too large",
            Self::Engine => "engine",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classe_e_detalhe() {
        let e = EngineError::Open("unsupported codec".into());
        assert_eq!(e.kind(), ErrorKind::Open);
        assert_eq!(e.to_string(), "could not open the file: unsupported codec");
        assert_eq!(ErrorKind::Seek.to_string(), "seek");
        assert_eq!(EngineError::TooLarge("16384×16384".into()).kind(), ErrorKind::TooLarge);
        assert_eq!(EngineError::EngineStopped("thread ended".into()).kind(), ErrorKind::Engine);
    }
}
