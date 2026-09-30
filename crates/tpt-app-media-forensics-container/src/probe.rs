//! Container probing and format detection (spec §12).
//!
//! Detection is by file signature, not by file extension. An extension is a
//! claim made by whoever named the file; the leading bytes are evidence. A
//! `.mp4` that is actually a Matroska file, or a renamed `.mov`, is exactly the
//! kind of mismatch this tool exists to surface.

use std::path::Path;

use tpt_app_media_forensics_model::StreamKind;

/// The container format of a media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerFormat {
    /// ISO Base Media File Format: MP4, MOV, 3GP, and relatives.
    IsoBmff,
    /// Matroska or WebM.
    Matroska,
    /// A raw audio elementary stream, e.g. WAV.
    Wav,
    /// AIFF.
    Aiff,
    /// Ogg container.
    Ogg,
    /// FLAC.
    Flac,
    /// A format this build does not recognise.
    Unknown,
}

impl ContainerFormat {
    /// Returns the stable lowercase tag used in reports and rule IDs.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::IsoBmff => "isobmff",
            Self::Matroska => "matroska",
            Self::Wav => "wav",
            Self::Aiff => "aiff",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
            Self::Unknown => "unknown",
        }
    }

    /// Returns the file extension conventionally used for this format.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::IsoBmff => "mp4",
            Self::Matroska => "mkv",
            Self::Wav => "wav",
            Self::Aiff => "aiff",
            Self::Ogg => "ogg",
            Self::Flac => "flac",
            Self::Unknown => "",
        }
    }

    /// Returns `true` when this format carries audio and/or video tracks.
    #[must_use]
    pub const fn is_container(self) -> bool {
        matches!(self, Self::IsoBmff | Self::Matroska)
    }
}

/// Identifies a container from its leading bytes.
///
/// Takes only the header, so probing is cheap: the caller can slice the first
/// few kilobytes rather than reading the whole file.
///
/// # Errors
///
/// Never. A file too short to identify is [`ContainerFormat::Unknown`], which
/// is an observation rather than a failure.
#[must_use]
pub fn detect(header: &[u8]) -> ContainerFormat {
    // ISO BMFF: a box size followed by one of the file-type brands.
    if header.len() >= 12 && &header[4..8] == b"ftyp" {
        return ContainerFormat::IsoBmff;
    }
    // Matroska/WebM: EBML magic.
    if header.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return ContainerFormat::Matroska;
    }
    // RIFF containers; the `WAVE` form is the one we care about.
    if header.len() >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WAVE" {
        return ContainerFormat::Wav;
    }
    // AIFF: `FORM` ... `AIFF`.
    if header.len() >= 12 && header.starts_with(b"FORM") && &header[8..12] == b"AIFF" {
        return ContainerFormat::Aiff;
    }
    if header.starts_with(b"OggS") {
        return ContainerFormat::Ogg;
    }
    if header.starts_with(b"fLaC") {
        return ContainerFormat::Flac;
    }
    ContainerFormat::Unknown
}

/// Reads just enough of a file to identify its container.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read. A file shorter than
/// [`PROBE_BYTES`] is not an error: it is read to whatever length it has and
/// reported as [`ContainerFormat::Unknown`].
pub fn detect_file(path: &Path) -> std::io::Result<ContainerFormat> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)?;
    let mut header = vec![0u8; PROBE_BYTES];
    let read = file.read(&mut header)?;
    header.truncate(read);
    Ok(detect(&header))
}

/// Number of leading bytes needed to identify any supported container.
pub const PROBE_BYTES: usize = 16;

/// Reports whether the container's declared format matches its file extension.
///
/// A mismatch is a finding, not an error: renamed files are ordinary, and the
/// analyst needs to know which name the evidence arrived under.
///
/// # Errors
///
/// Never.
#[must_use]
pub fn extension_matches(path: &Path, format: ContainerFormat) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(extension) => {
            let lowered = extension.to_ascii_lowercase();
            lowered == format.extension()
                // MOV, 3GP, and M4V are all ISO BMFF.
                || (format == ContainerFormat::IsoBmff
                    && matches!(lowered.as_str(), "mov" | "3gp" | "m4v" | "m4a"))
                // WebM is Matroska.
                || (format == ContainerFormat::Matroska && lowered == "webm")
        }
        None => false,
    }
}

/// Maps a Kinetix track media type to the engine's stream kind.
///
/// # Errors
///
/// Never; `Other` maps to [`StreamKind::Unknown`].
#[must_use]
pub fn stream_kind_of(media_type: tpt_kinetix_core::codec::MediaType) -> StreamKind {
    match media_type {
        tpt_kinetix_core::codec::MediaType::Video => StreamKind::Video,
        tpt_kinetix_core::codec::MediaType::Audio => StreamKind::Audio,
        tpt_kinetix_core::codec::MediaType::Other => StreamKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal ISO-BMFF header for the given brand.
    fn isobmff_header(brand: &[u8; 4]) -> Vec<u8> {
        let mut header = vec![0u8; 12];
        header[0..4].copy_from_slice(&(24u32).to_be_bytes());
        header[4..8].copy_from_slice(b"ftyp");
        header[8..12].copy_from_slice(brand);
        header
    }

    #[test]
    fn detects_mp4_by_signature() {
        assert_eq!(detect(&isobmff_header(b"isom")), ContainerFormat::IsoBmff);
        assert_eq!(detect(&isobmff_header(b"qt  ")), ContainerFormat::IsoBmff);
    }

    #[test]
    fn detects_matroska_and_webm() {
        assert_eq!(
            detect(&[0x1A, 0x45, 0xDF, 0xA3, 0x01, 0x00]),
            ContainerFormat::Matroska
        );
    }

    #[test]
    fn detects_wav() {
        let mut header = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
        header.truncate(16);
        assert_eq!(detect(&header), ContainerFormat::Wav);
    }

    #[test]
    fn detects_aiff() {
        let mut header = b"FORM\x00\x00\x00\x10AIFFCOMM".to_vec();
        header.truncate(16);
        assert_eq!(detect(&header), ContainerFormat::Aiff);
    }

    #[test]
    fn detects_ogg_and_flac() {
        assert_eq!(detect(b"OggS\x00\x02\x00\x00"), ContainerFormat::Ogg);
        assert_eq!(detect(b"fLaC\x00\x00\x00\x22"), ContainerFormat::Flac);
    }

    #[test]
    fn unknown_data_is_unknown_not_a_guess() {
        assert_eq!(detect(b"not a media file at all"), ContainerFormat::Unknown);
        assert_eq!(detect(&[]), ContainerFormat::Unknown);
        assert_eq!(detect(b"\x00\x01\x02"), ContainerFormat::Unknown);
    }

    #[test]
    fn short_file_does_not_panic() {
        // Truncated headers must be handled, not trusted (spec §75).
        for len in 0..16 {
            let header = vec![0xA5u8; len];
            let _ = detect(&header);
        }
    }

    #[test]
    fn extension_mismatch_is_detected() {
        let renamed = Path::new("evidence.mp4");
        assert!(extension_matches(renamed, ContainerFormat::IsoBmff));
        assert!(
            !extension_matches(renamed, ContainerFormat::Matroska),
            "an .mp4 that is really Matroska is a finding"
        );
    }

    #[test]
    fn mov_and_webm_are_recognised_as_family_members() {
        assert!(extension_matches(
            Path::new("a.mov"),
            ContainerFormat::IsoBmff
        ));
        assert!(extension_matches(
            Path::new("a.webm"),
            ContainerFormat::Matroska
        ));
        assert!(extension_matches(
            Path::new("a.3gp"),
            ContainerFormat::IsoBmff
        ));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(extension_matches(
            Path::new("A.MP4"),
            ContainerFormat::IsoBmff
        ));
    }

    #[test]
    fn no_extension_reports_mismatch() {
        // Absence of a claim is not agreement with one.
        assert!(!extension_matches(
            Path::new("noextension"),
            ContainerFormat::IsoBmff
        ));
    }

    #[test]
    fn format_tags_are_stable() {
        assert_eq!(ContainerFormat::IsoBmff.tag(), "isobmff");
        assert_eq!(ContainerFormat::Matroska.tag(), "matroska");
    }
}
