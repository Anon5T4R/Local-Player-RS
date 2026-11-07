//! Estado do transporte (máquina de estados pura, testável sem UI nem áudio).

/// O que o motor está fazendo agora.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// Sem faixa aberta (nada decodifica nem consome).
    #[default]
    Stopped,
    /// Consumindo amostras (o relógio mestre anda).
    Playing,
    /// Faixa aberta, mas o callback não consome (posição congelada).
    Paused,
}

impl Transport {
    pub fn is_playing(self) -> bool {
        matches!(self, Self::Playing)
    }

    /// A thread de decode deve manter o ring abastecido? (pausa ainda
    /// pré-bufferiza; parado não decodifica nada)
    pub fn decodes(self) -> bool {
        !matches!(self, Self::Stopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pausa_congela_posicao_mas_segue_decodificando() {
        assert!(!Transport::Paused.is_playing());
        assert!(Transport::Paused.decodes());
        assert!(!Transport::Stopped.decodes());
        assert!(Transport::Playing.decodes());
    }
}
