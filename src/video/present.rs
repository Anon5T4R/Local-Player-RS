//! Escolha do frame a mostrar pelo relógio (PTS) — o "play loop" do vídeo.
//!
//! Regra (mesma dos players de referência):
//! - frame com PTS **no futuro** → ainda não é a vez: a UI mantém o frame atual
//!   na tela (repeat);
//! - frames com PTS **no passado** → já eram: descarta, no máximo `max_drop` por
//!   tick (o resto cai no tick seguinte) e mostra o mais novo que já venceu.
//!
//! Sem relógio de áudio, o relógio é o de parede; com áudio, a posição do ring
//! (fase 2) é o relógio mestre e é ela que entra aqui.

use std::collections::VecDeque;

use super::Frame;

/// Consome de `queue` o frame que deve estar na tela em `clock` segundos.
///
/// `queue` está em ordem de PTS (a thread de decode entrega em ordem de
/// apresentação). `None` = nada venceu ainda; o frame atual continua na tela.
pub fn take_due(queue: &mut VecDeque<Frame>, clock: f64, max_drop: usize) -> Option<Frame> {
    if !clock.is_finite() {
        return None;
    }
    // Só o mais novo dos vencidos interessa; os anteriores são atraso puro.
    let mut dropped = 0;
    while queue.len() > 1 && queue.get(1).is_some_and(|f| f.pts <= clock) && dropped < max_drop {
        queue.pop_front();
        dropped += 1;
    }
    match queue.front() {
        Some(f) if f.pts <= clock => queue.pop_front(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_color::ColorInfo;
    use filmcraft_frame::Chroma;
    use std::sync::Arc;

    fn frame(pts: f64) -> Frame {
        Frame {
            width: 1,
            height: 1,
            chroma: Chroma::C420,
            planes: [Arc::new(vec![0]), Arc::new(vec![0]), Arc::new(vec![0])],
            color: ColorInfo::REC709,
            pts,
        }
    }

    fn queue(pts: &[f64]) -> VecDeque<Frame> {
        pts.iter().copied().map(frame).collect()
    }

    #[test]
    fn fila_vazia_nao_mostra_nada() {
        assert!(take_due(&mut VecDeque::new(), 1.0, 3).is_none());
    }

    #[test]
    fn frame_futuro_espera() {
        let mut q = queue(&[0.5]);
        assert!(take_due(&mut q, 0.4, 3).is_none());
        assert_eq!(q.len(), 1, "não pode descartar o futuro");
    }

    #[test]
    fn frame_vencido_e_mostrado() {
        let mut q = queue(&[0.0, 0.5]);
        let shown = take_due(&mut q, 0.1, 3);
        assert!(shown.is_some_and(|f| f.pts == 0.0));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn atrasados_sao_descartados_ate_o_mais_novo_vencido() {
        let mut q = queue(&[0.0, 0.033, 0.066, 0.1]);
        let shown = take_due(&mut q, 0.11, 3);
        assert!(shown.is_some_and(|f| (f.pts - 0.1).abs() < 1e-9), "mostra o mais novo vencido");
        assert!(q.is_empty());
    }

    #[test]
    fn limite_de_drops_por_tick() {
        let mut q = queue(&[0.0, 0.1, 0.2, 0.3, 0.4]);
        // no máximo 2 drops por tick: sobra o terceiro (0.2), ainda vencido.
        let shown = take_due(&mut q, 1.0, 2);
        assert!(shown.is_some_and(|f| (f.pts - 0.2).abs() < 1e-9));
        assert_eq!(q.len(), 2);
        // o próximo tick continua a limpeza.
        let shown = take_due(&mut q, 1.0, 2);
        assert!(shown.is_some_and(|f| (f.pts - 0.4).abs() < 1e-9));
    }

    #[test]
    fn relogio_invalido_nao_mostra_nada() {
        let mut q = queue(&[0.0]);
        assert!(take_due(&mut q, f64::NAN, 3).is_none());
        assert!(take_due(&mut q, f64::INFINITY, 3).is_none());
    }

    /// Repetição: depois de mostrar, a fila vazia mantém o frame na tela (None).
    #[test]
    fn fila_vazia_regete_o_frame_atual() {
        let mut q = queue(&[0.0]);
        assert!(take_due(&mut q, 0.0, 3).is_some());
        assert!(take_due(&mut q, 0.02, 3).is_none());
    }
}
