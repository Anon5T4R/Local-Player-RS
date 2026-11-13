//! Escolha do frame a mostrar pelo relógio (PTS) — o "play loop" do vídeo.
//!
//! Regra (a mesma dos players de referência, e a correção do relato de "acelerou
//! pra compensar"):
//!
//! - frame com PTS **no futuro** → ainda não é a vez: a UI mantém o frame atual
//!   na tela (repeat);
//! - frames com PTS **no passado** → já eram: **todos** são descartados e apenas
//!   o mais novo que já venceu é exibido. Nunca se exibe um frame velho só
//!   porque ele está na frente da fila: um backlog de segundos vira **skip**, não
//!   reprodução em velocidade máxima.
//!
//! A janela (`LATE_WINDOW`) classifica o atraso: acima dela o frame é contado
//! como `dropped_late` no [`crate::metrics`] — é o número que diz se o decode
//! está devendo (H1) ou se a UI é que atrasa (H3).
//!
//! Sem relógio de áudio, o relógio é o de parede; com áudio, a posição do ring
//! (fase 2) é o relógio mestre e é ela que entra aqui.

use std::collections::VecDeque;

use super::Frame;

/// Atraso (s) a partir do qual um frame é considerado atrasado, não só "chegou
/// junto". ~4 frames a 24 fps; folga para o jitter do relógio de áudio.
pub const LATE_WINDOW: f64 = 0.150;

/// Decisão do apresentador para um tick.
#[derive(Debug, Default)]
pub struct Decision {
    /// Frame a exibir (`None` = mantém o atual na tela).
    pub frame: Option<Frame>,
    /// Frames vencidos descartados antes do exibido.
    pub dropped: usize,
    /// Quantos desses estavam atrasados além da janela.
    pub dropped_late: usize,
    /// Atraso do frame exibido (s); ≤ 0 = na hora ou adiantado.
    pub late: f64,
}

/// Consome de `queue` o frame que deve estar na tela em `clock` segundos.
///
/// `queue` está em ordem de PTS (a thread de decode entrega em ordem de
/// apresentação). Só o mais novo dos vencidos interessa: os anteriores são
/// atraso puro. `None` = nada venceu ainda; o frame atual continua na tela.
pub fn take_due(queue: &mut VecDeque<Frame>, clock: f64, window: f64) -> Decision {
    if !clock.is_finite() {
        return Decision::default();
    }
    // Índice do frame mais novo já vencido (a fila está ordenada por PTS).
    let Some(newest) = queue.iter().rposition(|f| f.pts <= clock) else {
        return Decision::default();
    };
    let window = if window.is_finite() { window.max(0.0) } else { LATE_WINDOW };
    let mut dropped = 0;
    let mut dropped_late = 0;
    while dropped < newest {
        let Some(stale) = queue.pop_front() else { break };
        if clock - stale.pts > window {
            dropped_late += 1;
        }
        dropped += 1;
    }
    let Some(frame) = queue.pop_front() else {
        return Decision { dropped, dropped_late, ..Decision::default() };
    };
    let late = clock - frame.pts;
    Decision { frame: Some(frame), dropped, dropped_late, late }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::Planes;
    use filmcraft_color::ColorInfo;
    use filmcraft_frame::Chroma;
    use std::sync::Arc;

    fn frame(pts: f64) -> Frame {
        Frame {
            width: 1,
            height: 1,
            chroma: Chroma::C420,
            bits: 8,
            planes: Planes::U8([Arc::new(vec![0]), Arc::new(vec![0]), Arc::new(vec![0])]),
            color: ColorInfo::REC709,
            pts,
        }
    }

    fn queue(pts: &[f64]) -> VecDeque<Frame> {
        pts.iter().copied().map(frame).collect()
    }

    #[test]
    fn fila_vazia_nao_mostra_nada() {
        let d = take_due(&mut VecDeque::new(), 1.0, LATE_WINDOW);
        assert!(d.frame.is_none());
        assert_eq!((d.dropped, d.dropped_late), (0, 0));
    }

    #[test]
    fn frame_futuro_espera() {
        let mut q = queue(&[0.5]);
        let d = take_due(&mut q, 0.4, LATE_WINDOW);
        assert!(d.frame.is_none());
        assert_eq!(q.len(), 1, "não pode descartar o futuro");
    }

    #[test]
    fn frame_vencido_e_mostrado() {
        let mut q = queue(&[0.0, 0.5]);
        let d = take_due(&mut q, 0.1, LATE_WINDOW);
        assert!(d.frame.is_some_and(|f| f.pts == 0.0));
        assert_eq!((d.dropped, d.dropped_late), (0, 0));
        assert!((d.late - 0.1).abs() < 1e-9);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn varios_vencidos_mostram_o_mais_novo_e_descartam_o_resto() {
        let mut q = queue(&[0.0, 0.033, 0.066, 0.1]);
        let d = take_due(&mut q, 0.11, LATE_WINDOW);
        assert!(d.frame.is_some_and(|f| (f.pts - 0.1).abs() < 1e-9), "mostra o mais novo vencido");
        assert!(q.is_empty());
        assert_eq!(d.dropped, 3);
        // Nenhum deles estava além de 150 ms: são drops de cadência, não de atraso.
        assert_eq!(d.dropped_late, 0);
    }

    /// O caso do relato: o decode entregou segundos de vídeo de uma vez (ou o
    /// relógio saltou) e a fila está toda no passado. Tem de pular para o
    /// presente numa tacada — e **um** frame por chamada, nunca um "fast-forward"
    /// do backlog.
    #[test]
    fn backlog_de_segundos_vira_skip_nao_fast_forward() {
        // 10 s a 24 fps = 240 frames, todos vencidos com o relógio em 10 s.
        let pts: Vec<f64> = (0..240).map(|i| i as f64 / 24.0).collect();
        let mut q = queue(&pts);
        let d = take_due(&mut q, 10.0, LATE_WINDOW);
        let Some(shown) = d.frame else { panic!("o mais novo vencido precisa ser exibido") };
        assert!((shown.pts - 239.0 / 24.0).abs() < 1e-9, "mostra o mais novo (presente), não o começo do backlog");
        assert_eq!(d.dropped, 239);
        // Os dois últimos vencidos estão dentro da janela de 150 ms; o resto não.
        assert_eq!(d.dropped_late, 237, "todos os atrasados além da janela são contados");
        assert!(q.is_empty(), "o backlog inteiro sai da memória neste tick");
        // O tick seguinte não reexibe nada: não há backlog acelerado para "correr atrás".
        let d = take_due(&mut q, 10.0 + 1.0 / 24.0, LATE_WINDOW);
        assert!(d.frame.is_none());
    }

    #[test]
    fn atraso_alem_da_janela_e_classificado_sem_dobrar_o_frame() {
        // Dois vencidos, o da frente 400 ms atrasado.
        let mut q = queue(&[1.0, 1.4]);
        let d = take_due(&mut q, 1.45, LATE_WINDOW);
        assert!(d.frame.is_some_and(|f| (f.pts - 1.4).abs() < 1e-9));
        assert_eq!((d.dropped, d.dropped_late), (1, 1));
        assert!((d.late - 0.05).abs() < 1e-9, "o exibido está 50 ms atrasado");
        assert!(q.is_empty());
    }

    #[test]
    fn relogio_invalido_nao_mostra_nada() {
        let mut q = queue(&[0.0]);
        assert!(take_due(&mut q, f64::NAN, LATE_WINDOW).frame.is_none());
        assert!(take_due(&mut q, f64::INFINITY, LATE_WINDOW).frame.is_none());
        assert_eq!(q.len(), 1, "relógio inválido não pode consumir a fila");
    }

    /// Repetição: depois de mostrar, a fila vazia mantém o frame na tela (None).
    #[test]
    fn fila_vazia_regete_o_frame_atual() {
        let mut q = queue(&[0.0]);
        assert!(take_due(&mut q, 0.0, LATE_WINDOW).frame.is_some());
        assert!(take_due(&mut q, 0.02, LATE_WINDOW).frame.is_none());
    }

    /// Um frame por chamada: com vários vencidos nunca se entrega mais de um.
    #[test]
    fn no_maximo_um_frame_por_tick() {
        let mut q = queue(&[0.0, 0.01, 0.02, 0.03]);
        let d = take_due(&mut q, 0.04, LATE_WINDOW);
        assert!(d.frame.is_some());
        assert!(q.is_empty());
    }
}
