//! Evidence model: derived artefacts with integrity metadata (spec §32, §33).
//!
//! # Evidence is derived, the source is not
//!
//! The engine never modifies the source media (spec §11). Anything produced
//! during analysis — an extracted frame, a waveform excerpt, a container
//! structure dump — is written into the case directory as evidence with its
//! own hash, so a reviewer can confirm that what they are looking at is what
//! the engine produced.
//!
//! # Integrity, not authenticity
//!
//! The hashes here prove that an artefact has not changed *since it was
//! written*. They do not prove the source media was unaltered before the case
//! was opened; that claim rests on the acquisition record (spec §11) and,
//! ultimately, on the analyst's chain of custody (spec §64).

use serde::{Deserialize, Serialize};

use crate::asset::HashSet;
use crate::id::{AssetId, EvidenceId};
use crate::time::MediaTime;

/// What kind of artefact an evidence item holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvidenceKind {
    /// A single extracted frame image.
    ExtractedFrame,
    /// A contiguous run of extracted frames.
    FrameSequence,
    /// A waveform or spectrogram excerpt.
    AudioExcerpt,
    /// A dump of container box/structure layout.
    StructureDump,
    /// A metadata tree serialised to disk.
    MetadataExport,
    /// A timestamp or packet-level trace.
    TimingTrace,
    /// Any other retained artefact.
    Other,
}

impl EvidenceKind {
    /// Returns the stable lowercase tag used in manifests and the UI.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::ExtractedFrame => "extracted-frame",
            Self::FrameSequence => "frame-sequence",
            Self::AudioExcerpt => "audio-excerpt",
            Self::StructureDump => "structure-dump",
            Self::MetadataExport => "metadata-export",
            Self::TimingTrace => "timing-trace",
            Self::Other => "other",
        }
    }
}

/// How an evidence artefact was produced.
///
/// Recorded so a reviewer can judge whether an artefact is a direct capture or
/// a transformation. A re-encoded frame is weaker evidence than a direct dump.
///
/// `Copy` because the label is stored in the database, the artefact, and the
/// report without ever being modified after construction; cloning it to satisfy
/// the borrow checker would imply it could change, which it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    /// Copied byte-for-byte from the source.
    DirectCopy,
    /// Decoded from the source without re-encoding.
    LosslessExtract,
    /// Produced by a lossy transformation (e.g. a thumbnail).
    LossyTransform,
    /// Synthesised by the engine (e.g. a histogram or difference image).
    Derived,
}

impl Provenance {
    /// Returns the stable lowercase tag used in manifests and reports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::DirectCopy => "direct-copy",
            Self::LosslessExtract => "lossless-extract",
            Self::LossyTransform => "lossy-transform",
            Self::Derived => "derived",
        }
    }
}

/// Integrity metadata for a stored evidence artefact (spec §33).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceIntegrity {
    /// Size of the stored artefact in bytes.
    pub size_bytes: u64,
    /// Hashes of the stored artefact, computed when it was written.
    pub hashes: HashSet,
    /// Whether the hashes have been re-verified since being written.
    pub verified: bool,
}

/// A retained artefact supporting one or more findings (spec §32).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// This evidence item's identifier.
    pub id: EvidenceId,
    /// The asset this artefact was derived from.
    pub asset_id: AssetId,
    /// What kind of artefact this is.
    pub kind: EvidenceKind,
    /// How it was produced.
    pub provenance: Provenance,
    /// Path within the case directory, relative to the case root.
    ///
    /// Always relative: an absolute path would leak the analyst's directory
    /// layout into the report and would break when the case is moved.
    pub relative_path: String,
    /// Human-readable caption shown in the UI and reports.
    pub caption: Option<String>,
    /// Where in the source this artefact came from, when applicable.
    pub source_range: Option<(MediaTime, MediaTime)>,
    /// Integrity metadata recorded when the artefact was written.
    pub integrity: EvidenceIntegrity,
}

impl Evidence {
    /// Builds an evidence record with `verified` forced to false.
    ///
    /// Newly written evidence has not yet been re-read and re-hashed, so it
    /// must not claim verification until [`Self::mark_verified`] is called.
    #[must_use]
    pub fn new(
        asset_id: AssetId,
        kind: EvidenceKind,
        provenance: Provenance,
        relative_path: impl Into<String>,
        integrity: EvidenceIntegrity,
    ) -> Self {
        let relative_path = relative_path.into();
        let id =
            EvidenceId::new_derived(&[asset_id.raw().to_string().as_str(), relative_path.as_str()]);
        Self {
            id,
            asset_id,
            kind,
            provenance,
            relative_path,
            caption: None,
            source_range: None,
            integrity: EvidenceIntegrity {
                verified: false,
                ..integrity
            },
        }
    }

    /// Records that the artefact's hashes have been re-verified.
    pub fn mark_verified(&mut self) {
        self.integrity.verified = true;
    }

    /// Re-checks a stored artefact against its recorded integrity metadata.
    ///
    /// Returns `false` for an artefact whose hashes were never computed, since
    /// "cannot verify" must never be reported as "verified".
    #[must_use]
    pub fn verify(&self, size_bytes: u64, hashes: &HashSet) -> bool {
        if !self.integrity.hashes.is_complete() {
            return false;
        }
        if size_bytes != self.integrity.size_bytes {
            return false;
        }
        self.integrity
            .hashes
            .iter()
            .all(|(algorithm, expected)| hashes.get(*algorithm) == Some(expected.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{FileHash, HashAlgorithm};

    fn hashes(byte: u8) -> HashSet {
        HashSet::new([
            FileHash::from_bytes(HashAlgorithm::Sha256, &[byte; 32]),
            FileHash::from_bytes(HashAlgorithm::Blake3, &[byte; 32]),
        ])
    }

    fn evidence() -> Evidence {
        Evidence::new(
            AssetId::new_derived(&["asset"]),
            EvidenceKind::ExtractedFrame,
            Provenance::LosslessExtract,
            "evidence/frames/frame-001002.png",
            EvidenceIntegrity {
                size_bytes: 2_048,
                hashes: hashes(0x11),
                verified: true,
            },
        )
    }

    #[test]
    fn new_evidence_starts_unverified() {
        let mut e = evidence();
        assert!(!e.integrity.verified, "written is not the same as verified");
        e.mark_verified();
        assert!(e.integrity.verified);
    }

    #[test]
    fn evidence_paths_are_relative_not_absolute() {
        let e = evidence();
        assert!(
            !e.relative_path.contains(':'),
            "must not embed a drive letter"
        );
        assert!(!e.relative_path.starts_with('/'), "must not be absolute");
    }

    #[test]
    fn evidence_id_is_derived_from_asset_and_path() {
        assert_eq!(
            evidence().id,
            evidence().id,
            "same asset + path must give the same ID"
        );
    }

    #[test]
    fn verify_accepts_unchanged_artefact() {
        assert!(evidence().verify(2_048, &hashes(0x11)));
    }

    #[test]
    fn verify_rejects_changed_content() {
        assert!(!evidence().verify(2_048, &hashes(0x22)));
    }

    #[test]
    fn verify_rejects_changed_size() {
        assert!(!evidence().verify(1_024, &hashes(0x11)));
    }

    #[test]
    fn verify_refuses_to_confirm_unhashed_artefact() {
        let mut e = evidence();
        e.integrity.hashes = HashSet::default();
        assert!(
            !e.verify(2_048, &hashes(0x11)),
            "cannot-verify must not be reported as verified"
        );
    }

    #[test]
    fn provenance_tags_are_stable() {
        assert_eq!(Provenance::DirectCopy.tag(), "direct-copy");
        assert_eq!(Provenance::LossyTransform.tag(), "lossy-transform");
    }

    #[test]
    fn evidence_kind_tags_are_stable() {
        assert_eq!(EvidenceKind::ExtractedFrame.tag(), "extracted-frame");
        assert_eq!(EvidenceKind::StructureDump.tag(), "structure-dump");
    }
}
