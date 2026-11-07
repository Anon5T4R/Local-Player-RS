//! Saída de áudio via cpal.
//!
//! Abre o device default UMA vez, com a configuração default do device
//! (formato/ taxa/ canais), e mantém o stream tocando do começo ao fim do app:
//! parado/pausado o callback escreve silêncio. O ring é nosso (`SampleRing`,
//! stereo f32) e o callback só converte/mistura para os canais e o formato do
//! device — nenhum lock e nenhuma alocação no caminho normal.
//!
//! **Limitação:** sample formats exóticos (I24/U24/I48/U48) não são suportados
//! e viram erro (`NoDevice`) — na prática Windows/WASAPI e ALSA entregam
//! f32/i16/u16. A taxa do device é fixa por sessão (trocar de device exige
//! reabrir o app).

use std::sync::mpsc::Sender;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use super::error::EngineError;
use super::ring::SampleRing;
use super::Event;

/// Quantos frames pré-alocamos para o scratch do callback (buffer típico é
/// 128–2048; 8192 cobre sobra). Crescer depois aloca — só na primeira vez que
/// um buffer maior aparecer.
const SCRATCH_FRAMES: usize = 8_192;

pub(super) struct Output {
    pub ring: Arc<SampleRing>,
    pub rate: u32,
    _stream: cpal::Stream,
}

/// Abre o device default e sobe o stream (já tocando silêncio).
pub(super) fn start(tx: Sender<Event>) -> Result<Output, EngineError> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| EngineError::NoDevice("no default output device".into()))?;
    let supported = device.default_output_config().map_err(|e| EngineError::NoDevice(e.to_string()))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let rate = config.sample_rate.0;
    // ~1 s de buffer (potência de 2 ≥ taxa): dá folga pro decoder e pro
    // scheduler sem estourar a latência de seek (o flush descarta tudo).
    let ring = Arc::new(SampleRing::new(rate, rate as usize));
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, &config, ring.clone(), tx),
        SampleFormat::I16 => build::<i16>(&device, &config, ring.clone(), tx),
        SampleFormat::U16 => build::<u16>(&device, &config, ring.clone(), tx),
        SampleFormat::I32 => build::<i32>(&device, &config, ring.clone(), tx),
        SampleFormat::I8 => build::<i8>(&device, &config, ring.clone(), tx),
        SampleFormat::U8 => build::<u8>(&device, &config, ring.clone(), tx),
        other => Err(EngineError::NoDevice(format!("unsupported sample format: {other:?}"))),
    }?;
    stream.play().map_err(|e| EngineError::NoDevice(e.to_string()))?;
    Ok(Output { ring, rate, _stream: stream })
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, ring: Arc<SampleRing>, tx: Sender<Event>) -> Result<cpal::Stream, EngineError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let channels = channels.max(1);
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * 2];
    let mut epoch = ring.epoch();
    let mut err_tx = Some(tx);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _info| {
                let frames = data.len() / channels;
                let need = frames * 2;
                if scratch.len() < need {
                    scratch.resize(need, 0.0);
                }
                // Sem lock, sem panic: o ring só mexe em átomos e índices
                // mascarados; qualquer estado estranho vira silêncio.
                let n = ring.fill(&mut scratch[..need], &mut epoch);
                let (gain, muted) = ring.gain();
                let gain = if muted { 0.0 } else { gain };
                for f in 0..frames {
                    let (l, r) =
                        if f < n { ((scratch[2 * f] * gain).clamp(-1.0, 1.0), (scratch[2 * f + 1] * gain).clamp(-1.0, 1.0)) } else { (0.0, 0.0) };
                    let frame = &mut data[f * channels..(f + 1) * channels];
                    for (c, sample) in frame.iter_mut().enumerate() {
                        let v = match (channels, c) {
                            (1, _) => (l + r) * 0.5,
                            (_, 0) => l,
                            (_, 1) => r,
                            _ => 0.0,
                        };
                        *sample = T::from_sample(v);
                    }
                }
            },
            move |err| {
                // Erro do device (desconectou/ocupado): avisa a UI uma vez.
                // Fora do caminho normal de áudio, então a alocação do canal
                // não importa.
                if let Some(tx) = err_tx.take() {
                    let _ = tx.send(Event::Error(EngineError::NoDevice(err.to_string())));
                }
            },
            None,
        )
        .map_err(|e| EngineError::NoDevice(e.to_string()))
}
