//! Lógica pura da playlist (avançar/voltar), separada da UI para ser testável.
//!
//! Sem wrap-around: no fim da lista o player para; no começo, "anterior"
//! reinicia a faixa atual (a UI decide).

/// Próxima faixa depois de `current`, se existir.
pub fn next_index(len: usize, current: usize) -> Option<usize> {
    if current + 1 < len {
        Some(current + 1)
    } else {
        None
    }
}

/// Faixa anterior, se existir (no índice 0 a UI reinicia a atual).
pub fn prev_index(len: usize, current: usize) -> Option<usize> {
    if current > 0 && current < len {
        Some(current - 1)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avanca_ate_o_fim_sem_dar_a_volta() {
        assert_eq!(next_index(3, 0), Some(1));
        assert_eq!(next_index(3, 1), Some(2));
        assert_eq!(next_index(3, 2), None);
        assert_eq!(next_index(0, 0), None);
        assert_eq!(next_index(1, 0), None);
    }

    #[test]
    fn volta_ate_o_primeiro() {
        assert_eq!(prev_index(3, 2), Some(1));
        assert_eq!(prev_index(3, 1), Some(0));
        assert_eq!(prev_index(3, 0), None);
        assert_eq!(prev_index(0, 0), None);
    }
}
