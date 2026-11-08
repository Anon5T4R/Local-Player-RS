//! The pure part of the backend choice: which codec families the OS decoder can take, at which
//! bit depth, and the verdict a diagnostic reports for a stream.
//!
//! Fork local-player-rs: upstream's platform crate has no such module — its `hardware_decoder_for`
//! answers by creating a decoder. Keeping the family / bit-depth / policy decision pure lets the
//! player unit-test it on any machine (no GPU, no Windows) and lets `--probe` say "provável"
//! before any device is touched.

/// The codec families the OS backends know: Media Foundation / DXVA (Windows) decodes all four;
/// VA-API and NVDEC (Linux) decode H.264 and HEVC (see [`linux_supported`]).
///
/// Mapped from the hub's display labels (`H.264 (High)`, `HEVC`, `VP9 (Profile 0)`, `AV1 (Main
/// Profile)`), which is what `--probe` has without the container's sample entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Vp9,
    Av1,
}

impl Codec {
    /// The name used in diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
        }
    }
}

/// The codec family of a hub label, `None` for the codecs the backend does not take (ProRes, DNx,
/// MJPEG, MPEG-2, APV, uncompressed…).
pub fn family(label: &str) -> Option<Codec> {
    let l: String = label.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    if l.starts_with("h.264") || l.starts_with("h264") || l.starts_with("avc") {
        Some(Codec::H264)
    } else if l.starts_with("hevc") || l.starts_with("h.265") || l.starts_with("h265") {
        Some(Codec::Hevc)
    } else if l.starts_with("vp9") {
        Some(Codec::Vp9)
    } else if l.starts_with("av1") {
        Some(Codec::Av1)
    } else {
        None
    }
}

/// Bits per sample a pixel-format label declares (`4:2:0 10-bit`, `YUV 4:2:0 8-bit`,
/// `yuv420p10le`, `p010`): the declared depth, or 8 when it does not say (the hub's default).
pub fn bit_depth(pixel_format: &str) -> u32 {
    let lower = pixel_format.to_ascii_lowercase();
    if lower.contains("p010") || lower.contains("p0l0") {
        return 10;
    }
    // `<n>-bit`, `<n> bit`, `<n>bit`: the digits right before the first "bit".
    if let Some(at) = lower.find("bit") {
        let head = lower[..at].trim_end_matches(|c: char| !c.is_ascii_digit());
        let digits: String = head.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
        if let Ok(n) = digits.parse::<u32>()
            && (8..=16).contains(&n)
        {
            return n;
        }
    }
    // `yuv420p10le` / `yuv422p12be`: digits right before a trailing endianness suffix.
    for suffix in ["le", "be"] {
        if let Some(rest) = lower.strip_suffix(suffix) {
            let digits: String = rest.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
            if let Ok(n) = digits.parse::<u32>()
                && (8..=16).contains(&n)
            {
                return n;
            }
        }
    }
    8
}

/// Whether the Windows backend has a decoder for the family at that depth. The GPU, the DXVA
/// profile and the decoder MFT still get the final say, per stream; on Linux see
/// [`linux_supported`].
pub fn supported(codec: Codec, bits: u32) -> bool {
    match codec {
        // DXVA H.264 decoders are 8-bit (Baseline / Main / High); Hi10 and 4:2:2 / 4:4:4 are not.
        Codec::H264 => bits == 8,
        Codec::Hevc => matches!(bits, 8 | 10),
        Codec::Vp9 => matches!(bits, 8 | 10),
        Codec::Av1 => matches!(bits, 8 | 10),
    }
}

/// Whether the Linux backends (VA-API and NVDEC) decode the family at that depth. Both take
/// H.264 8-bit and HEVC 8- / 10-bit 4:2:0 only; VP9 and AV1 are the Windows backend's.
///
/// Fork local-player-rs: upstream has no pure decision (it asks a decoder), and the player needs
/// one the `--probe` diagnostic can answer from the hub's label on any machine.
pub fn linux_supported(codec: Codec, bits: u32) -> bool {
    matches!(codec, Codec::H264 | Codec::Hevc) && supported(codec, bits)
}

/// Whether this build's backend decodes the family at any depth (H.264 and HEVC on Linux; the
/// four families on Windows). The overlay's "the decoder should take this stream" line only has
/// the codec label, and the per-stream profile / bit depth still get the final say.
pub fn taken(label: &str) -> bool {
    match family(label) {
        Some(codec) => platform_takes(codec),
        None => false,
    }
}

/// Whether this platform's hardware decoder covers the family (see [`taken`]).
pub fn platform_takes(codec: Codec) -> bool {
    #[cfg(target_os = "linux")]
    {
        matches!(codec, Codec::H264 | Codec::Hevc)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = codec;
        true
    }
}

/// Whether this platform's hardware decoder takes the family at that depth: the Linux backends'
/// narrower set ([`linux_supported`]) or the Windows backend's ([`supported`]).
pub fn platform_supported(codec: Codec, bits: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux_supported(codec, bits)
    }
    #[cfg(not(target_os = "linux"))]
    {
        supported(codec, bits)
    }
}

/// The decoder a stream will use for its pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Hardware,
    Software,
}

/// The choice for one stream: hardware only when the setting is on, a backend was registered, and
/// the stream's family and bit depth are ones this platform's backend takes. Pure so the matrix is
/// unit-tested; [`crate::probe`] adds the driver/GPU checks a diagnostic can afford.
pub fn choose(label: &str, bits: u32, hardware_decoding: bool, registered: bool) -> Choice {
    if !hardware_decoding || !registered {
        return Choice::Software;
    }
    match family(label) {
        Some(codec) if platform_supported(codec, bits) => Choice::Hardware,
        _ => Choice::Software,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_come_from_the_hub_labels() {
        for label in ["H.264", "H.264 (High)", "H.264 (Baseline)", "AVC", "h264"] {
            assert_eq!(family(label), Some(Codec::H264), "{label}");
        }
        for label in ["HEVC", "H.265", "hevc (Main 10)"] {
            assert_eq!(family(label), Some(Codec::Hevc), "{label}");
        }
        assert_eq!(family("VP9 (Profile 0)"), Some(Codec::Vp9));
        assert_eq!(family("AV1 (Main Profile)"), Some(Codec::Av1));
        for label in ["Apple ProRes", "Motion JPEG", "MPEG-2 Video", "Uncompressed YUV 4:2:2", "VP8", ""] {
            assert_eq!(family(label), None, "{label}");
        }
    }

    #[test]
    fn bit_depth_reads_the_labels_the_hub_writes() {
        assert_eq!(bit_depth("4:2:0 8-bit"), 8);
        assert_eq!(bit_depth("YUV 4:2:0 10-bit"), 10);
        assert_eq!(bit_depth("YUV 4:2:2 10-bit"), 10);
        assert_eq!(bit_depth("yuv420p10le"), 10);
        assert_eq!(bit_depth("yuv422p12be"), 12);
        assert_eq!(bit_depth("p010"), 10);
        assert_eq!(bit_depth(""), 8, "unknown means the 8-bit default");
        assert_eq!(bit_depth("garbage"), 8);
    }

    #[test]
    fn h264_is_only_taken_at_8_bits_and_hevc_up_to_10() {
        assert!(supported(Codec::H264, 8));
        assert!(!supported(Codec::H264, 10), "Hi10 is not a DXVA profile");
        assert!(supported(Codec::Hevc, 8));
        assert!(supported(Codec::Hevc, 10));
        assert!(!supported(Codec::Hevc, 12));
        assert!(supported(Codec::Vp9, 8) && supported(Codec::Vp9, 10));
        assert!(supported(Codec::Av1, 8) && supported(Codec::Av1, 10));
        assert!(!supported(Codec::Av1, 12));
    }

    #[test]
    fn the_choice_is_hardware_only_when_everything_lines_up() {
        // policy off, or nothing registered: software.
        assert_eq!(choose("H.264", 8, false, true), Choice::Software);
        assert_eq!(choose("H.264", 8, true, false), Choice::Software);
        // a codec the backend does not take, or a depth it does not: software.
        assert_eq!(choose("Apple ProRes", 8, true, true), Choice::Software);
        assert_eq!(choose("H.264 (High 10)", 10, true, true), Choice::Software);
        // the player's 1080p case: hardware.
        assert_eq!(choose("H.264 (High)", 8, true, true), Choice::Hardware);
        assert_eq!(choose("HEVC", 10, true, true), Choice::Hardware);
        // the families differ per platform: VP9 belongs to the Windows backend.
        #[cfg(target_os = "linux")]
        assert_eq!(choose("VP9 (Profile 0)", 8, true, true), Choice::Software);
        #[cfg(not(target_os = "linux"))]
        assert_eq!(choose("VP9 (Profile 0)", 8, true, true), Choice::Hardware);
    }

    /// The Linux matrix, pure and on every host: H.264 8-bit and HEVC 8- / 10-bit only.
    #[test]
    fn linux_takes_h264_and_hevc_only() {
        assert!(linux_supported(Codec::H264, 8));
        assert!(!linux_supported(Codec::H264, 10), "VA-API and NVDEC are 8-bit H.264 here");
        assert!(linux_supported(Codec::Hevc, 8));
        assert!(linux_supported(Codec::Hevc, 10));
        assert!(!linux_supported(Codec::Hevc, 12));
        assert!(!linux_supported(Codec::Vp9, 8), "VP9 belongs to the Windows backend");
        assert!(!linux_supported(Codec::Av1, 10), "AV1 belongs to the Windows backend");
    }

    /// The overlay's label-only verdict follows the platform's families.
    #[test]
    fn taken_follows_the_platform_families() {
        assert!(!taken("Apple ProRes"));
        assert!(!taken(""));
        assert!(taken("H.264 (High)"));
        assert!(taken("HEVC"));
        // A platform without a Linux backend keeps the four families (Windows): the overlay still
        // needs the backend registered before it draws a hardware line, which `hw_mode` checks.
        #[cfg(target_os = "linux")]
        {
            assert!(!taken("VP9 (Profile 0)"));
            assert!(!taken("AV1 (Main Profile)"));
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(taken("VP9 (Profile 0)"));
            assert!(taken("AV1 (Main Profile)"));
        }
    }
}
