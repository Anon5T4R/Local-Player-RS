//! OS hardware video decoding (layer L5): the operating system's video decoders in front of
//! FilmCraft's own, with a transparent fallback to them.
//!
//! [`register`] puts the platform's hardware decoder factory in front of the pure-Rust decoders
//! (`filmcraft_codecs::register_video_decoder`). On Windows that is Media Foundation / DXVA for
//! H.264, HEVC, VP9 and AV1 streams; streams it does not take (format, profile, bit depth, chroma,
//! size, no hardware, or the Hardware decoding switch is off) keep using the software decoders,
//! and one that fails mid-stream switches to them ([`HybridDecoder`]) and logs it. A file is never
//! made undecodable by registering a hardware decoder.
//!
//! Pictures are the software decoder's: same planes (bit-exact on the parity fixtures), colour,
//! pixel aspect, pts and presentation order, so the two are interchangeable.
//!
//! Fork local-player-rs: this crate is an extraction of `filmcraft-platform`'s Windows video
//! decoder and the pieces it needs (Annex B conversion, the biplanar readback and
//! [`HybridDecoder`]) from storytold/filmcraft. It exists so a player can use the hardware path
//! without the editor crates the upstream platform crate depends on (project, render, export,
//! engine) and without the hardware encoders (NVENC, VideoToolbox). The Linux (VA-API / NVDEC) and
//! macOS (VideoToolbox) decoders are the next units: the crate is laid out so each one becomes a
//! module behind its `cfg`, registered in [`register`] and listed in [`Availability`].
//!
//! This is the one crate of this fork allowed to use `unsafe` (OS FFI), and only in its FFI
//! modules (`media_foundation::gpu`, `media_foundation::mft`), as upstream documents in
//! docs/adr/0001-platform-ffi.md.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// Compiled everywhere so their tests run on every system (the Windows decoder is the only user).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod annexb;
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
mod biplanar;
pub mod codec;
pub mod hybrid;
#[cfg(target_os = "windows")]
pub mod media_foundation;

pub use codec::{Choice, Codec};
pub use hybrid::HybridDecoder;

/// What [`register`] made available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// A hardware decoder factory was registered (its name).
    Available(&'static str),
    /// Nothing to register on this system (why).
    Unavailable(&'static str),
}

/// Register the platform's hardware video decoders (call once at startup; repeated calls are
/// harmless). Streams they do not take, and every stream while hardware decoding is Off
/// (`filmcraft_codecs::hw::set_hardware_decoding`), keep using FilmCraft's own decoders.
pub fn register() -> Availability {
    #[cfg(target_os = "windows")]
    {
        filmcraft_codecs::register_video_decoder(media_foundation_factory);
        filmcraft_codecs::hw::set_hw_backend("Media Foundation");
        Availability::Available("Media Foundation")
    }
    // Fork local-player-rs: the Linux (VA-API / NVDEC) and macOS (VideoToolbox) decoders are the
    // next units. Until they land, every platform but Windows decodes in software — the pure-Rust
    // decoders stay the foundation, as decided for the player.
    #[cfg(not(target_os = "windows"))]
    {
        Availability::Unavailable("hardware video decoding is not implemented on this platform yet (Windows first; Linux/macOS next)")
    }
}

/// Whether [`register`] has put a hardware decoder factory in front of our decoders.
pub fn registered() -> bool {
    #[cfg(target_os = "windows")]
    {
        filmcraft_codecs::video_decoder_registered(media_foundation_factory)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Whether the platform backend exists on this system (a Direct3D 11 video device on Windows), so
/// a caller can tell "no hardware here" from "not registered".
pub fn available() -> bool {
    #[cfg(target_os = "windows")]
    {
        media_foundation::available()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// The Media Foundation factory: a [`HybridDecoder`] around [`media_foundation::MfDecoder`] for
/// H.264 / HEVC / VP9 / AV1 streams a Direct3D-aware decoder MFT can decode with DXVA on this
/// system's GPU, `None` otherwise.
#[cfg(target_os = "windows")]
pub fn media_foundation_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = media_foundation::stream_info(entry)?;
    match media_foundation::MfDecoder::new(info.clone()) {
        Ok(mf) => Some(Ok(Box::new(HybridDecoder::new(Box::new(mf), entry.clone(), info)))),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}

/// What a diagnostic (`--probe`) can say about hardware decoding for one stream, without opening a
/// decoder for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamProbe {
    /// [`Choice::Hardware`] = the backend should take this stream ("provável").
    pub choice: Choice,
    /// The registered backend, when there is one.
    pub backend: Option<&'static str>,
    /// Why, in English (the caller formats it for the user).
    pub reason: String,
}

/// The hardware-decoding verdict for a stream described by the hub's metadata
/// (`VideoStreamInfo::codec` / `pixel_format`), the setting and what is registered.
///
/// Fork local-player-rs: upstream only has `hardware_decoder_for(entry)`, which creates a Direct3D
/// device and a decoder MFT to answer. A player asks at `--probe` time, so this checks the codec
/// family and bit depth first, then whether a decoder MFT exists and the GPU lists the DXVA
/// profile for it — the process-wide Direct3D device is created once (tens of ms), no decoder MFT.
pub fn probe(label: &str, width: u32, height: u32, pixel_format: &str) -> StreamProbe {
    let bits = codec::bit_depth(pixel_format);
    let backend = filmcraft_codecs::hw::hw_backend();
    let software = |reason: String| StreamProbe { choice: Choice::Software, backend, reason };
    let Some(family) = codec::family(label) else {
        return software("codec outside the Media Foundation decoders".into());
    };
    if !codec::supported(family, bits) {
        return software(format!("no hardware decoder for {} {bits}-bit", family.name()));
    }
    // Fork local-player-rs: no backend on this platform yet — say that, not "not registered".
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (width, height);
        return software("hardware decoding is not implemented on this platform yet".into());
    }
    #[cfg(target_os = "windows")]
    {
        if !filmcraft_codecs::hw::hardware_decoding() {
            return software("hardware decoding is off".into());
        }
        if !registered() {
            return software("no hardware decoder backend registered".into());
        }
        match media_foundation::supports(family, bits, (width.max(1), height.max(1))) {
            Ok(()) => StreamProbe { choice: Choice::Hardware, backend, reason: format!("Media Foundation takes {} {bits}-bit", family.name()) },
            Err(why) => software(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registering twice is harmless and reports the same thing (as upstream's setting test).
    #[test]
    fn registering_twice_is_harmless() {
        let first = register();
        assert_eq!(register(), first);
        assert_eq!(registered(), matches!(first, Availability::Available(_)));
    }

    #[test]
    fn probe_reports_the_reason_a_codec_is_software() {
        let p = probe("Apple ProRes", 1920, 1080, "");
        assert_eq!(p.choice, Choice::Software);
        assert!(p.reason.contains("outside"), "{}", p.reason);
    }
}
