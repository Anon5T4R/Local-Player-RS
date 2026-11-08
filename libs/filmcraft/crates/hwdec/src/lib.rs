//! OS hardware video decoding (layer L5): the operating system's video decoders in front of
//! FilmCraft's own, with a transparent fallback to them.
//!
//! [`register`] puts the platform's hardware decoder factory in front of the pure-Rust decoders
//! (`filmcraft_codecs::register_video_decoder`). On Windows that is Media Foundation / DXVA for
//! H.264, HEVC, VP9 and AV1 streams; on Linux it is VA-API ([`vaapi`]; Intel, AMD and other Mesa
//! / vendor drivers) and NVDEC ([`nvdec`]; NVIDIA's proprietary driver), for H.264 and HEVC.
//! Streams they do not take (format, profile, bit depth, chroma, size, no hardware, or the
//! Hardware decoding switch is off) keep using the software decoders, and one that fails
//! mid-stream switches to them ([`HybridDecoder`]) and logs it. A file is never made undecodable by
//! registering a hardware decoder.
//!
//! Pictures are the software decoder's: same planes (bit-exact on the parity fixtures), colour,
//! pixel aspect, pts and presentation order, so the two are interchangeable.
//!
//! Fork local-player-rs: this crate is an extraction of `filmcraft-platform`'s video decoders and
//! the pieces they need (Annex B conversion, the biplanar readback, [`HybridDecoder`] and, for
//! Linux, the CUDA context NVDEC maps frames through) from storytold/filmcraft. It exists so a
//! player can use the hardware paths without the editor crates the upstream platform crate depends
//! on (project, render, export, engine) and without the hardware encoders (NVENC, VideoToolbox).
//! macOS (VideoToolbox) is not ported.
//!
//! This is the one crate of this fork allowed to use `unsafe` (OS FFI), and only in its FFI
//! modules (`media_foundation::{gpu,mft}`, `vaapi::{ffi,va}`, `nvdec::{cuvid,ffi,device}`), as
//! upstream documents in docs/adr/0001-platform-ffi.md. Functionality is identical to upstream;
//! every local divergence is marked `// fork local-player-rs:`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// Compiled everywhere so their tests run on every system (the Windows and Linux decoders are the
// users).
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
mod annexb;
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
mod biplanar;
pub mod codec;
pub mod hybrid;
#[cfg(target_os = "windows")]
pub mod media_foundation;
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub mod nvdec;
// Compiled on every target so the buffer layout (`abi_tests`) and the H.264 / HEVC front ends
// (`tests`, with a recording stand-in for the driver) are tested without a GPU or a Linux box.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod vaapi;

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
    #[cfg(target_os = "linux")]
    {
        // VA-API first, then NVDEC in front of it: NVIDIA's proprietary driver has no VA-API of
        // its own, so what NVDEC declines still reaches VA-API (a second GPU) and then the
        // software decoders — the same order upstream registers them in.
        let mut available = match vaapi::va::probe() {
            Ok(driver) => {
                static VAAPI: std::sync::Once = std::sync::Once::new();
                VAAPI.call_once(|| log::info!("hardware decoding through VA-API: {driver}"));
                filmcraft_codecs::register_video_decoder(vaapi_factory);
                Availability::Available("VA-API")
            }
            Err(why) => {
                log::info!("no VA-API hardware decoding: {why}");
                Availability::Unavailable("no VA-API driver (libva and a DRM render node) and no NVIDIA driver (NVDEC)")
            }
        };
        #[cfg(target_pointer_width = "64")]
        match nvdec::cuvid::probe() {
            Ok(driver) => {
                static NVDEC: std::sync::Once = std::sync::Once::new();
                NVDEC.call_once(|| log::info!("hardware decoding through NVDEC: {driver}"));
                filmcraft_codecs::register_video_decoder(nvdec_factory);
                available = Availability::Available("NVDEC");
            }
            Err(why) => log::info!("no NVDEC hardware decoding: {why}"),
        }
        // the backend the overlay and the session log print is the last one registered
        if let Availability::Available(name) = available {
            filmcraft_codecs::hw::set_hw_backend(name);
        }
        available
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Availability::Unavailable("hardware video decoding is not implemented on this platform yet (Windows and Linux first; macOS next)")
    }
}

/// Whether [`register`] has put a hardware decoder factory in front of our decoders.
pub fn registered() -> bool {
    #[cfg(target_os = "windows")]
    {
        filmcraft_codecs::video_decoder_registered(media_foundation_factory)
    }
    #[cfg(target_os = "linux")]
    {
        #[cfg(target_pointer_width = "64")]
        if filmcraft_codecs::video_decoder_registered(nvdec_factory) {
            return true;
        }
        filmcraft_codecs::video_decoder_registered(vaapi_factory)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// Whether the platform backend exists on this system (a Direct3D 11 video device on Windows, a VA
/// display on a DRM render node or an NVIDIA driver on Linux), so a caller can tell "no hardware
/// here" from "not registered".
pub fn available() -> bool {
    #[cfg(target_os = "windows")]
    {
        media_foundation::available()
    }
    #[cfg(target_os = "linux")]
    {
        if vaapi::va::probe().is_ok() {
            return true;
        }
        #[cfg(target_pointer_width = "64")]
        {
            nvdec::cuvid::probe().is_ok()
        }
        #[cfg(not(target_pointer_width = "64"))]
        {
            false
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
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

/// The VA-API factory: a [`HybridDecoder`] around [`vaapi::VaDecoder`] for H.264 and HEVC streams
/// this system's VA-API driver decodes, `None` otherwise (other codecs keep the software
/// decoders).
#[cfg(target_os = "linux")]
pub fn vaapi_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry)?.ok()?;
    match vaapi::VaDecoder::new(info.clone()) {
        Ok(va) => Some(Ok(Box::new(HybridDecoder::new(Box::new(va), entry.clone(), info)))),
        Err(why) => {
            log::info!("hardware decoding declined for {} video: {why}", entry.codec.name());
            filmcraft_codecs::hw::note_hw_declined();
            None
        }
    }
}

/// The NVDEC factory: a [`HybridDecoder`] around [`nvdec::NvDecoder`] for H.264 and HEVC streams
/// this system's NVIDIA GPU decodes, `None` otherwise (VA-API and the software decoders keep
/// those). Fork local-player-rs: registered before VA-API, so a decline here is followed by
/// VA-API's own verdict and only that one counts it.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub fn nvdec_factory(entry: &filmcraft_isobmff::SampleEntry) -> Option<filmcraft_codecs::Result<Box<dyn filmcraft_codecs::VideoDecoder>>> {
    if !filmcraft_codecs::hw::hardware_decoding() {
        return None;
    }
    let info = filmcraft_codecs::hw::NalStreamInfo::from_entry(entry)?.ok()?;
    match nvdec::NvDecoder::new(info.clone()) {
        Ok(nv) => Some(Ok(Box::new(HybridDecoder::new(Box::new(nv), entry.clone(), info)))),
        Err(why) => {
            log::info!("NVDEC declined {} video: {why}", entry.codec.name());
            if !filmcraft_codecs::video_decoder_registered(vaapi_factory) {
                filmcraft_codecs::hw::note_hw_declined();
            }
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
/// family and bit depth first, then whether the platform backend exists and takes the stream — on
/// Windows the process-wide Direct3D device is created once (tens of ms) and the GPU must list the
/// DXVA profile, no decoder MFT; on Linux the VA-API driver is asked for a configuration (no
/// surfaces, no decoder) and NVDEC answers from being registered (it parses the stream itself).
pub fn probe(label: &str, width: u32, height: u32, pixel_format: &str) -> StreamProbe {
    let bits = codec::bit_depth(pixel_format);
    let backend = filmcraft_codecs::hw::hw_backend();
    let software = |reason: String| StreamProbe { choice: Choice::Software, backend, reason };
    #[cfg(target_os = "linux")]
    {
        let _ = (width, height);
        let Some(family) = codec::family(label) else {
            return software("codec outside the VA-API / NVDEC decoders".into());
        };
        if !codec::linux_supported(family, bits) {
            return software(format!("no hardware decoder for {} {bits}-bit", family.name()));
        }
        if !filmcraft_codecs::hw::hardware_decoding() {
            return software("hardware decoding is off".into());
        }
        if !registered() {
            // Fork local-player-rs: say why nothing is registered, not just that nothing is.
            let mut why = match vaapi::va::probe() {
                Ok(driver) => format!("VA-API is present ({driver}) but no decoder backend was registered"),
                Err(e) => format!("no VA-API driver: {e}"),
            };
            #[cfg(target_pointer_width = "64")]
            if let Err(e) = nvdec::cuvid::probe() {
                why.push_str(&format!("; no NVDEC: {e}"));
            }
            return software(why);
        }
        match backend {
            Some("VA-API") => match vaapi::supports(family, bits) {
                Ok(()) => {
                    StreamProbe { choice: Choice::Hardware, backend, reason: format!("VA-API takes {} {bits}-bit on this driver", family.name()) }
                }
                Err(why) => software(why),
            },
            Some(name) => {
                // NVDEC parses the stream itself, so only the family matrix can answer here;
                // say when VA-API also watches behind it (a second GPU / a decline).
                let behind = if vaapi::va::probe().is_ok() { ", VA-API behind it" } else { "" };
                StreamProbe { choice: Choice::Hardware, backend, reason: format!("{name} takes {} {bits}-bit{behind}", family.name()) }
            }
            None => software("no hardware decoder backend registered".into()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Some(family) = codec::family(label) else {
            #[cfg(target_os = "windows")]
            return software("codec outside the Media Foundation decoders".into());
            #[cfg(not(target_os = "windows"))]
            return software("codec outside the hardware decoders".into());
        };
        if !codec::supported(family, bits) {
            return software(format!("no hardware decoder for {} {bits}-bit", family.name()));
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
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (width, height, family);
            software("hardware decoding is not implemented on this platform yet".into())
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

    /// Fork local-player-rs: a platform without the backend (or with it off) declines instead of
    /// claiming hardware — the software decoder is what plays the file.
    #[test]
    fn unavailable_hardware_declines_and_software_decodes() {
        if !available() {
            let p = probe("H.264 (High)", 1920, 1080, "4:2:0 8-bit");
            assert_eq!(p.choice, Choice::Software, "{p:?}");
            assert_eq!(p.backend, None, "{p:?}");
            assert!(p.reason.contains("not registered") || p.reason.contains("not implemented"), "{}", p.reason);
        }
        // With the factory unregistered, the pure choice is software whatever the codec.
        assert_eq!(codec::choose("H.264", 8, true, false), Choice::Software);
    }

    /// Fork local-player-rs: the Linux fallback, on a box without libva / a DRM render node /
    /// an NVIDIA driver (the CI and agent case). Registration declines, no backend is reported,
    /// and the hub keeps its pure-Rust decoder in front: the file still plays. Skipped when the
    /// machine does have a backend (a real GPU), where the benches answer instead.
    #[test]
    #[cfg(target_os = "linux")]
    fn linux_without_a_driver_declines_and_software_keeps_playing() {
        if available() {
            return;
        }
        assert!(matches!(register(), Availability::Unavailable(_)));
        assert!(!registered());
        assert_eq!(filmcraft_codecs::hw::hw_backend(), None);
        let p = probe("HEVC", 3840, 2160, "YUV 4:2:0 10-bit");
        assert_eq!(p.choice, Choice::Software, "{p:?}");
        assert!(p.reason.contains("no VA-API driver"), "{}", p.reason);
        // the gate the hub asks before it calls a factory: nothing registered, so software
        assert_eq!(codec::choose("HEVC", 10, true, registered()), Choice::Software);
    }
}
