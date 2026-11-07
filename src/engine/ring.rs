//! Ring buffer SPSC decoder → callback do cpal, **sem lock no callback**.
//!
//! Desenho:
//! - o produtor (thread do motor) é o único que escreve `write` e as amostras
//!   depois dele; publica com `Release`;
//! - o consumidor (callback do cpal) é o único que mexe em `read`, consome
//!   com `Acquire` e devolve o espaço com `Release`;
//! - `epoch` é o "flush": o produtor incrementa depois de reposicionar o
//!   relógio; o callback vê a mudança e descarta o que estava bufferizado
//!   (seek/stop). Amostras novas escritas no meio do descarte podem cair
//!   junto — no máximo um buffer de áudio, nunca desalinha o relógio.
//!
//! O conteúdo do ring é sempre **stereo entrelaçado f32 na taxa do device**.
//! `played` é o relógio mestre: só avança quando o callback consome
//! (pausa congela a posição).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Segundos → frames na taxa `rate` (arredonda; entradas inválidas = 0).
pub(super) fn secs_to_frames(secs: f64, rate: u32) -> u64 {
    if !secs.is_finite() || secs <= 0.0 || rate == 0 {
        return 0;
    }
    (secs * f64::from(rate)).round() as u64
}

/// Frames → segundos (taxa 0 = 0,0).
pub(super) fn frames_to_secs(frames: u64, rate: u32) -> f64 {
    if rate == 0 {
        0.0
    } else {
        frames as f64 / f64::from(rate)
    }
}

pub(super) struct SampleRing {
    /// Amostras em bits de f32 (`AtomicU32`): produtor e consumidor acessam
    /// sem lock e sem `unsafe`; a ordem é garantida pelos índices
    /// Release/Acquire.
    data: Box<[AtomicU32]>,
    /// Capacidade em frames (potência de 2 → máscara no índice).
    cap: usize,
    /// Total de frames já publicados (monotônico).
    write: AtomicU64,
    /// Total de frames já consumidos (monotônico).
    read: AtomicU64,
    /// Incrementado pelo produtor a cada flush (seek/stop/play).
    epoch: AtomicU64,
    /// Frames do ponto de origem do trecho atual (seek inicial).
    base: AtomicU64,
    /// Frames consumidos desde o último flush (relógio mestre).
    played: AtomicU64,
    eof: AtomicBool,
    /// Época em que o callback sinalizou o fim (`u64::MAX` = nenhum).
    /// Carimbar a época evita um "fim" atrasado da faixa anterior chegar
    /// depois do Play da próxima (o flush invalida o aviso).
    ended_epoch: AtomicU64,
    paused: AtomicBool,
    /// Ganho do usuário 0..=1 em bits de f32.
    gain: AtomicU32,
    muted: AtomicBool,
    rate: u32,
}

impl SampleRing {
    pub(super) fn new(rate: u32, cap_frames: usize) -> Self {
        let cap = cap_frames.max(2).next_power_of_two();
        Self {
            data: (0..cap * 2).map(|_| AtomicU32::new(0)).collect::<Vec<_>>().into_boxed_slice(),
            cap,
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            base: AtomicU64::new(0),
            played: AtomicU64::new(0),
            eof: AtomicBool::new(false),
            ended_epoch: AtomicU64::new(u64::MAX),
            paused: AtomicBool::new(true),
            gain: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            rate,
        }
    }

    /// Enfileira `samples` (stereo entrelaçado). Devolve quantos **frames**
    /// couberam; o chamador mantém o resto e tenta de novo (ring cheio =
    /// decoder dorme).
    pub(super) fn push(&self, samples: &[f32]) -> usize {
        let total = samples.len() / 2;
        let mask = self.cap - 1;
        let mut done = 0;
        while done < total {
            let r = self.read.load(Ordering::Acquire);
            let w = self.write.load(Ordering::Relaxed);
            let used = w.saturating_sub(r) as usize;
            let free = self.cap.saturating_sub(used);
            if free == 0 {
                break;
            }
            let n = free.min(total - done);
            let mut i = 0;
            while i < n {
                let slot = ((w as usize + i) & mask) * 2;
                // Até o fim do anel numa cópia só.
                let take = (n - i).min(self.cap - slot / 2);
                let src = &samples[(done + i) * 2..(done + i + take) * 2];
                for (dst, s) in self.data[slot..slot + take * 2].iter().zip(src) {
                    dst.store(s.to_bits(), Ordering::Relaxed);
                }
                i += take;
            }
            self.write.store(w + n as u64, Ordering::Release);
            done += n;
        }
        done
    }

    /// Consome até `out.len()/2` frames para `out` (stereo entrelaçado).
    /// Retorna os frames preenchidos. Roda **no callback**: sem lock, sem
    /// alocação, sem panic (índices mascarados).
    pub(super) fn fill(&self, out: &mut [f32], epoch: &mut u64) -> usize {
        // Flush pendente (seek/stop): descarta o buffer velho.
        let e = self.epoch.load(Ordering::Acquire);
        if e != *epoch {
            *epoch = e;
            self.played.store(0, Ordering::Relaxed);
            let w = self.write.load(Ordering::Acquire);
            self.read.store(w, Ordering::Release);
        }
        if self.paused.load(Ordering::Relaxed) {
            return 0;
        }
        let want = out.len() / 2;
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Relaxed);
        let avail = w.saturating_sub(r) as usize;
        let n = want.min(avail);
        if n > 0 {
            let mask = self.cap - 1;
            let mut i = 0;
            while i < n {
                let slot = ((r as usize + i) & mask) * 2;
                let take = (n - i).min(self.cap - slot / 2);
                let dst = &mut out[i * 2..(i + take) * 2];
                for (d, src) in dst.iter_mut().zip(&self.data[slot..slot + take * 2]) {
                    *d = f32::from_bits(src.load(Ordering::Relaxed));
                }
                i += take;
            }
            self.read.store(r + n as u64, Ordering::Release);
            self.played.fetch_add(n as u64, Ordering::Relaxed);
        }
        // Fim de faixa: só quando o buffer esvaziou e o decoder sinalizou EOF.
        // O aviso leva a época observada nesta chamada: se um flush (troca de
        // faixa/seek) já aconteceu, `take_ended` não casa e o fim da faixa
        // anterior não pula a próxima.
        if n >= avail && self.eof.load(Ordering::Acquire) && !self.paused.load(Ordering::Relaxed) {
            self.ended_epoch.store(*epoch, Ordering::Release);
        }
        n
    }

    /// Novo trecho: zera relógio/EOF e manda o callback descartar o buffer.
    pub(super) fn flush(&self, base_frames: u64) {
        self.eof.store(false, Ordering::Release);
        self.ended_epoch.store(u64::MAX, Ordering::Release);
        self.base.store(base_frames, Ordering::Release);
        self.played.store(0, Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::Release);
    }

    pub(super) fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Época atual — o callback começa a contar daqui (um flush anterior ao
    /// primeiro callback não pode ser confundido com seek pendente).
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(super) fn set_volume(&self, gain: f32) {
        let g = if gain.is_finite() { gain.clamp(0.0, 1.0) } else { 1.0 };
        self.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// (ganho, mudo) — lido pelo callback a cada buffer.
    pub(super) fn gain(&self) -> (f32, bool) {
        (f32::from_bits(self.gain.load(Ordering::Relaxed)), self.muted.load(Ordering::Relaxed))
    }

    pub(super) fn set_eof(&self, eof: bool) {
        self.eof.store(eof, Ordering::Release);
    }

    /// Consome o "chegou ao fim" (uma vez), só se o aviso é da época atual.
    pub(super) fn take_ended(&self) -> bool {
        let epoch = self.epoch.load(Ordering::Acquire);
        if self.ended_epoch.load(Ordering::Acquire) != epoch {
            return false;
        }
        self.ended_epoch.compare_exchange(epoch, u64::MAX, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    /// Posição do relógio mestre em segundos.
    pub(super) fn position_secs(&self) -> f64 {
        let base = self.base.load(Ordering::Acquire);
        let played = self.played.load(Ordering::Acquire);
        frames_to_secs(base.saturating_add(played), self.rate)
    }

    /// Taxa do device dona deste ring (frames/segundo).
    pub(super) fn rate(&self) -> u32 {
        self.rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(frames: usize, tag: f32) -> Vec<f32> {
        (0..frames).flat_map(|i| [i as f32 + tag, -(i as f32) - tag]).collect()
    }

    #[test]
    fn push_e_fill_devolvem_as_mesmas_amostras() {
        let ring = SampleRing::new(48_000, 1024);
        ring.set_paused(false);
        let input = stereo(100, 0.0);
        assert_eq!(ring.push(&input), 100);
        let mut out = vec![0.0; 200];
        let mut epoch = 0;
        assert_eq!(ring.fill(&mut out, &mut epoch), 100);
        assert_eq!(out, input);
    }

    #[test]
    fn ring_cheio_aceita_so_o_que_cabe_e_da_a_volta() {
        let ring = SampleRing::new(48_000, 4);
        ring.set_paused(false);
        assert_eq!(ring.push(&stereo(6, 0.0)), 4);
        let mut out = vec![0.0; 8];
        let mut epoch = ring.epoch();
        assert_eq!(ring.fill(&mut out, &mut epoch), 4);
        // 4 lidos + 3 escritos: exercita o wrap-around do anel de 4 frames.
        let more = stereo(3, 100.0);
        assert_eq!(ring.push(&more), 3);
        assert_eq!(ring.fill(&mut out, &mut epoch), 3);
        assert_eq!(&out[..6], &more[..]);
    }

    #[test]
    fn pausa_nao_consome_nem_anda_o_relogio() {
        let ring = SampleRing::new(48_000, 64);
        ring.set_paused(false);
        ring.flush(0);
        assert_eq!(ring.push(&stereo(10, 0.0)), 10);
        let mut out = vec![0.0; 20];
        let mut epoch = ring.epoch();
        assert_eq!(ring.fill(&mut out, &mut epoch), 10);
        assert!((ring.position_secs() - 10.0 / 48_000.0).abs() < 1e-12);
        ring.set_paused(true);
        assert_eq!(ring.push(&stereo(5, 0.0)), 5);
        assert_eq!(ring.fill(&mut out, &mut epoch), 0);
        assert!((ring.position_secs() - 10.0 / 48_000.0).abs() < 1e-12);
    }

    #[test]
    fn flush_reposiciona_o_relogio_e_descarta_o_buffer() {
        let ring = SampleRing::new(48_000, 64);
        ring.set_paused(false);
        ring.push(&stereo(30, 0.0));
        let mut out = vec![0.0; 60];
        let mut epoch = 0;
        assert_eq!(ring.fill(&mut out, &mut epoch), 30);

        ring.flush(48_000 * 2); // seek para 2 s
        assert!((ring.position_secs() - 2.0).abs() < 1e-9);
        // O callback descarta o que sobrou (nada aqui) e o relógio segue do base.
        assert_eq!(ring.fill(&mut out, &mut epoch), 0);
        ring.push(&stereo(48_000, 0.0)); // só cabem 64 frames no ring de teste
        let mut out = vec![0.0; 120];
        assert_eq!(ring.fill(&mut out, &mut epoch), 60);
        assert!((ring.position_secs() - (2.0 + 60.0 / 48_000.0)).abs() < 1e-9);
    }

    #[test]
    fn eof_so_vira_fim_depois_de_drenar_e_fora_da_pausa() {
        let ring = SampleRing::new(48_000, 64);
        ring.set_paused(false);
        ring.push(&stereo(2, 0.0));
        let mut out = vec![0.0; 4];
        let mut epoch = 0;
        assert_eq!(ring.fill(&mut out, &mut epoch), 2);
        ring.set_eof(true);
        assert!(!ring.take_ended());
        assert_eq!(ring.fill(&mut out, &mut epoch), 0);
        assert!(ring.take_ended());
        assert!(!ring.take_ended());

        // Pausado no fim da faixa: não pode "terminar" sozinho.
        ring.set_paused(true);
        ring.set_eof(true);
        assert_eq!(ring.fill(&mut out, &mut epoch), 0);
        assert!(!ring.take_ended());
    }

    #[test]
    fn flush_invalida_fim_pendente_da_faixa_anterior() {
        let ring = SampleRing::new(48_000, 64);
        ring.set_paused(false);
        let mut out = vec![0.0; 4];
        let mut epoch = ring.epoch();
        // Fim detectado pelo callback, e o Play da próxima faixa chega antes
        // de o motor drenar o aviso (troca de faixa rápida).
        ring.set_eof(true);
        assert_eq!(ring.fill(&mut out, &mut epoch), 0);
        ring.flush(0);
        assert!(!ring.take_ended(), "fim da faixa antiga não pode pular a nova");
    }

    #[test]
    fn volume_e_mudo_com_clamp() {
        let ring = SampleRing::new(48_000, 8);
        ring.set_volume(0.25);
        assert_eq!(ring.gain(), (0.25, false));
        ring.set_volume(7.0);
        assert_eq!(ring.gain(), (1.0, false));
        ring.set_volume(f32::NAN);
        assert_eq!(ring.gain(), (1.0, false));
        ring.set_muted(true);
        assert_eq!(ring.gain(), (1.0, true));
    }

    #[test]
    fn conversao_de_tempo() {
        assert_eq!(secs_to_frames(2.0, 48_000), 96_000);
        assert_eq!(secs_to_frames(-1.0, 48_000), 0);
        assert_eq!(secs_to_frames(f64::NAN, 48_000), 0);
        assert_eq!(secs_to_frames(1.0, 0), 0);
        assert!((frames_to_secs(24_000, 48_000) - 0.5).abs() < 1e-12);
        assert_eq!(frames_to_secs(1, 0), 0.0);
    }
}
