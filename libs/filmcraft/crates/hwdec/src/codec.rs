//! The pure part of the backend choice: which codec families the OS decoder can take, at which
//! bit depth, and the verdict a diagnostic reports for a stream.
//!
//! Fork local-player-rs: upstream's platform crate has no such module — its `hardware_decoder_for`
//! answers by creating a decoder. Keeping the family / bit-depth / policy decision pure lets the
//! player unit-test it on any machine (no GPU, no Windows) and lets `--probe` say "provável"
//! before any device is touched.

/// The codec families the Windows backend (Media Foundation / DXVA) decodes in hardware.
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

/// Whether the backend has a decoder for the family at that depth. The GPU, the DXVA profile and
/// the decoder MFT still get the final say, per stream.
pub fn supported(codec: Codec, bits: u32) -> bool {
    match codec {
        // DXVA H.264 decoders are 8-bit (Baseline / Main / High); Hi10 and 4:2:2 / 4:4:4 are not.
        Codec::H264 => bits == 8,
        Codec::Hevc => matches!(bits, 8 | 10),
        Codec::Vp9 => matches!(bits, 8 | 10),
        Codec::Av1 => matches!(bits, 8 | 10),
    }
}

/// The decoder a stream will use for its pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Hardware,
    Software,
}

/// The choice for one stream: hardware only when the setting is on, a backend was registered, and
/// the stream's family and bit depth are ones the backend takes. Pure so the matrix is unit-tested;
/// [`crate::probe`] adds the GPU/MFT checks a diagnostic can afford.
pub fn choose(label: &str, bits: u32, hardware_decoding: bool, registered: bool) -> Choice {
    if !hardware_decoding || !registered {
        return Choice::Software;
    }
    match family(label) {
        Some(codec) if supported(codec, bits) => Choice::Hardware,
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
    }
}
