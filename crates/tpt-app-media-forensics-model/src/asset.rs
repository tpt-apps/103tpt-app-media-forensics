//! Media asset and acquisition record (spec §10, §11).
//!
//! # Read-only source guarantee
//!
//! The source media is evidence. Nothing in the engine may write to it, and
//! every derived artefact is written into the case directory instead. This
//! module records *what* the source was — size, timestamps, hashes — so that
//! any later question "was this file altered?" has an answer anchored to a
//! hash taken at acquisition rather than to a file inspected in place.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::id::AssetId;

/// A hash algorithm the acquisition step can compute.
///
/// `Ord` is implemented by tag rather than derived, so that a `BTreeMap` keyed
/// by this type iterates in the same alphabetical order as the manifest
/// documents (`blake3` before `sha256`), independent of declaration order.
///
/// Serialises as its lowercase tag (`"sha256"`) rather than the Rust variant
/// name, because the manifest is a stable, externally consumed format
/// (spec §58) and variant naming is an implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    /// SHA-256 — interoperability with legal/evidentiary workflows (spec §10).
    #[serde(rename = "sha256")]
    Sha256,
    /// BLAKE3 — fast local integrity checking and cache keying (spec §10).
    #[serde(rename = "blake3")]
    Blake3,
}

impl HashAlgorithm {
    /// Returns the canonical lowercase name used in manifests and reports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Blake3 => "blake3",
        }
    }

    /// Number of bytes in a digest of this algorithm.
    #[must_use]
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Blake3 => 32,
        }
    }

    /// All algorithms, in the fixed order used for manifests.
    pub const ALL: [Self; 2] = [Self::Sha256, Self::Blake3];
}

impl Ord for HashAlgorithm {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.tag().cmp(other.tag())
    }
}

impl PartialOrd for HashAlgorithm {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A content hash, stored as lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileHash {
    /// Which algorithm produced this digest.
    pub algorithm: HashAlgorithm,
    /// Lowercase hexadecimal digest.
    pub hex: String,
}

impl FileHash {
    /// Builds a hash record from raw digest bytes.
    #[must_use]
    pub fn from_bytes(algorithm: HashAlgorithm, digest: &[u8]) -> Self {
        Self {
            algorithm,
            hex: to_hex(digest),
        }
    }

    /// Returns the digest length in bytes recorded by this hash.
    #[must_use]
    pub fn digest_len(&self) -> usize {
        self.hex.len() / 2
    }
}

/// Formats bytes as lowercase hexadecimal.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0F), 16).unwrap_or('0'));
    }
    out
}

/// The set of hashes computed for an asset at acquisition (spec §10).
///
/// A `BTreeMap` rather than a `HashSet` so that serialisation order is fixed:
/// two runs over the same file must emit the same manifest bytes (spec §77).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HashSet {
    hashes: BTreeMap<HashAlgorithm, String>,
}

impl HashSet {
    /// Builds a hash set from algorithm/digest pairs.
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = FileHash>) -> Self {
        let mut hashes = BTreeMap::new();
        for entry in entries {
            hashes.insert(entry.algorithm, entry.hex);
        }
        Self { hashes }
    }

    /// Records a hash, replacing any previous value for that algorithm.
    pub fn insert(&mut self, hash: FileHash) {
        self.hashes.insert(hash.algorithm, hash.hex);
    }

    /// Returns the hex digest for `algorithm`, if it was computed.
    #[must_use]
    pub fn get(&self, algorithm: HashAlgorithm) -> Option<&str> {
        self.hashes.get(&algorithm).map(String::as_str)
    }

    /// Returns the SHA-256 digest, the interoperability hash (spec §10).
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        self.get(HashAlgorithm::Sha256)
    }

    /// Returns the BLAKE3 digest, the fast local hash (spec §10).
    #[must_use]
    pub fn blake3(&self) -> Option<&str> {
        self.get(HashAlgorithm::Blake3)
    }

    /// Returns `true` when both SHA-256 and BLAKE3 were computed.
    ///
    /// A partial hash set means the acquisition was interrupted or a profile
    /// requested fewer algorithms; the UI and reports must not present a
    /// partially hashed asset as fully verified.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        HashAlgorithm::ALL
            .iter()
            .all(|alg| self.get(*alg).is_some())
    }

    /// Iterates the hashes in deterministic algorithm order.
    pub fn iter(&self) -> impl Iterator<Item = (&HashAlgorithm, &String)> {
        self.hashes.iter()
    }
}

/// Filesystem timestamps recorded at acquisition (spec §11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTimestamps {
    /// Last-modified time, in whole seconds since the Unix epoch.
    pub modified_unix_secs: Option<i64>,
    /// Created time where the platform exposes one (spec §11).
    pub created_unix_secs: Option<i64>,
    /// Last-accessed time, where recorded.
    pub accessed_unix_secs: Option<i64>,
}

/// Filesystem context recorded at acquisition (spec §11).
///
/// Fields are populated on a best-effort basis: a filesystem that does not
/// expose a value leaves it `None` rather than substituting a guess. An absent
/// volume serial number is an observation in its own right.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemInfo {
    /// Filesystem type as reported by the OS, e.g. `NTFS`, `APFS`.
    pub filesystem: Option<String>,
    /// Volume label or mount point name.
    pub volume_name: Option<String>,
    /// Platform-native file identifier where available.
    pub file_index: Option<u64>,
    /// Number of hard links to the file, where the platform exposes it.
    ///
    /// More than one link means the file is reachable under another path,
    /// which matters when establishing what evidence a filesystem held.
    pub link_count: Option<u64>,
    /// Volume serial number, where the platform exposes one (Windows).
    pub volume_serial_number: Option<u32>,
    /// File size reported by the filesystem metadata, in bytes.
    ///
    /// Recorded separately from [`AcquisitionRecord::size_bytes`], which is the
    /// number of bytes actually read. A disagreement means the file changed
    /// while it was being hashed.
    pub metadata_size_bytes: u64,
    /// Unix permission bits, where the platform exposes them.
    pub unix_permissions: Option<u32>,
}

/// The broad category of a media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    /// A container holding audio and/or video.
    Container,
    /// Raw audio, e.g. WAV or FLAC.
    Audio,
    /// A still image.
    Image,
    /// A document or text artefact, e.g. a report or notes file.
    Document,
    /// Screenshot or other supporting image evidence.
    Screenshot,
    /// Any other file in the case.
    Other,
}

impl MediaType {
    /// Returns the stable lowercase tag used in the manifest and UI.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Audio => "audio",
            Self::Image => "image",
            Self::Document => "document",
            Self::Screenshot => "screenshot",
            Self::Other => "other",
        }
    }
}
/// An immutable record of a source file taken at acquisition (spec §11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcquisitionRecord {
    /// Absolute path of the source file, as it was at acquisition.
    pub source_path: String,
    /// File size in bytes at acquisition.
    pub size_bytes: u64,
    /// Content hashes computed at acquisition.
    pub hashes: HashSet,
    /// Filesystem timestamps at acquisition.
    pub timestamps: FileTimestamps,
    /// Filesystem context at acquisition.
    pub filesystem: FilesystemInfo,
}

impl AcquisitionRecord {
    /// Re-checks a source against this record.
    ///
    /// Requires a complete hash set on both sides *and* an unchanged size.
    /// Size alone is not evidence of integrity, but a size change is a
    /// decisive signal that something happened.
    #[must_use]
    pub fn verify_against(&self, size_bytes: u64, hashes: &HashSet) -> AssetIntegrity {
        if !self.hashes.is_complete() || !hashes.is_complete() {
            return AssetIntegrity::Unverifiable;
        }
        let all_match = self
            .hashes
            .iter()
            .all(|(algorithm, expected)| hashes.get(*algorithm) == Some(expected.as_str()));
        match (all_match, size_bytes == self.size_bytes) {
            (true, true) => AssetIntegrity::Intact,
            (false, true) => AssetIntegrity::HashMismatch,
            (_, false) => AssetIntegrity::Modified,
        }
    }
}

/// The outcome of re-checking a source against its acquisition record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetIntegrity {
    /// Every recorded hash still matches.
    Intact,
    /// The size matches but at least one hash differs: content changed.
    HashMismatch,
    /// The size differs: the file was replaced or truncated.
    Modified,
    /// Not enough information to decide (e.g. an incomplete hash set).
    Unverifiable,
}

impl AssetIntegrity {
    /// Returns `true` when the source is provably unchanged since acquisition.
    #[must_use]
    pub const fn is_intact(self) -> bool {
        matches!(self, Self::Intact)
    }
}

/// A media asset within a case (spec §10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAsset {
    /// This asset's identifier.
    pub id: AssetId,
    /// Display name, usually the file name.
    pub name: String,
    /// What kind of file this is.
    pub media_type: MediaType,
    /// The acquisition record captured before analysis.
    pub acquisition: AcquisitionRecord,
}

impl MediaAsset {
    /// Builds an asset from its acquisition record.
    ///
    /// The asset ID is derived from the SHA-256 digest when available,
    /// falling back to the full source path. Deriving from content means
    /// importing the same file twice into one case yields the same ID, so a
    /// duplicate import is detectable rather than silently doubling the case.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        media_type: MediaType,
        acquisition: AcquisitionRecord,
    ) -> Self {
        let discriminator = acquisition
            .hashes
            .sha256()
            .map_or_else(|| acquisition.source_path.as_str(), |hash| hash);
        Self {
            id: AssetId::new_derived(&[discriminator]),
            name: name.into(),
            media_type,
            acquisition,
        }
    }

    /// Returns the file size in bytes recorded at acquisition.
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        self.acquisition.size_bytes
    }

    /// Returns the SHA-256 digest recorded at acquisition.
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        self.acquisition.hashes.sha256()
    }

    /// Returns the BLAKE3 digest recorded at acquisition.
    #[must_use]
    pub fn blake3(&self) -> Option<&str> {
        self.acquisition.hashes.blake3()
    }

    /// Re-checks the source file against this asset's acquisition record.
    ///
    /// Intended to be called with freshly computed values, e.g. by the `hash`
    /// CLI command or the UI's integrity check.
    #[must_use]
    pub fn verify(&self, size_bytes: u64, hashes: &HashSet) -> AssetIntegrity {
        self.acquisition.verify_against(size_bytes, hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_hashes() -> HashSet {
        HashSet::new([
            FileHash::from_bytes(HashAlgorithm::Sha256, &[0xAA; 32]),
            FileHash::from_bytes(HashAlgorithm::Blake3, &[0xBB; 32]),
        ])
    }

    fn record(hashes: HashSet, size: u64) -> AcquisitionRecord {
        AcquisitionRecord {
            source_path: r"C:\cases\alpha\original.mp4".to_owned(),
            size_bytes: size,
            hashes,
            timestamps: FileTimestamps {
                modified_unix_secs: Some(1_755_000_000),
                ..FileTimestamps::default()
            },
            filesystem: FilesystemInfo {
                filesystem: Some("NTFS".to_owned()),
                ..FilesystemInfo::default()
            },
        }
    }

    #[test]
    fn hex_encoding_is_lowercase_and_padded() {
        assert_eq!(to_hex(&[0x00, 0x0F, 0xA0, 0xFF]), "000fa0ff");
    }

    #[test]
    fn digest_length_is_reported_in_bytes() {
        assert_eq!(
            FileHash::from_bytes(HashAlgorithm::Sha256, &[0xAB; 32]).digest_len(),
            32
        );
    }

    #[test]
    fn complete_hash_set_requires_both_algorithms() {
        assert!(complete_hashes().is_complete());

        let partial = HashSet::new([FileHash::from_bytes(HashAlgorithm::Sha256, &[0xAA; 32])]);
        assert!(
            !partial.is_complete(),
            "partial hashing is not verification"
        );
        assert!(partial.sha256().is_some());
        assert!(partial.blake3().is_none());
    }

    #[test]
    fn identical_content_yields_identical_asset_id() {
        let a = MediaAsset::new(
            "original.mp4",
            MediaType::Container,
            record(complete_hashes(), 10),
        );
        let b = MediaAsset::new(
            "renamed.mp4",
            MediaType::Container,
            record(complete_hashes(), 10),
        );
        assert_eq!(
            a.id, b.id,
            "content-derived IDs make duplicate imports detectable"
        );
    }

    #[test]
    fn different_content_yields_different_asset_id() {
        let a = MediaAsset::new("a.mp4", MediaType::Container, record(complete_hashes(), 10));
        let mut other = complete_hashes();
        other.insert(FileHash::from_bytes(HashAlgorithm::Sha256, &[0xCC; 32]));
        let b = MediaAsset::new("a.mp4", MediaType::Container, record(other, 10));
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn unhashed_assets_fall_back_to_path_discriminator() {
        let a = MediaAsset::new("x", MediaType::Other, record(HashSet::default(), 1));
        let b = MediaAsset::new("x", MediaType::Other, record(HashSet::default(), 1));
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn verify_reports_intact_when_nothing_changed() {
        let asset = MediaAsset::new(
            "a.mp4",
            MediaType::Container,
            record(complete_hashes(), 4096),
        );
        assert!(asset.verify(4096, &complete_hashes()).is_intact());
    }

    #[test]
    fn verify_reports_modified_when_size_changes() {
        let asset = MediaAsset::new(
            "a.mp4",
            MediaType::Container,
            record(complete_hashes(), 4096),
        );
        assert_eq!(
            asset.verify(2048, &complete_hashes()),
            AssetIntegrity::Modified
        );
    }

    #[test]
    fn verify_reports_hash_mismatch_when_size_holds() {
        let asset = MediaAsset::new(
            "a.mp4",
            MediaType::Container,
            record(complete_hashes(), 4096),
        );
        let mut tampered = complete_hashes();
        tampered.insert(FileHash::from_bytes(HashAlgorithm::Sha256, &[0xDD; 32]));
        assert_eq!(asset.verify(4096, &tampered), AssetIntegrity::HashMismatch);
    }

    #[test]
    fn verify_refuses_to_confirm_an_incomplete_recheck() {
        // A partial recheck cannot confirm integrity, so it must not claim it.
        let asset = MediaAsset::new(
            "a.mp4",
            MediaType::Container,
            record(complete_hashes(), 4096),
        );
        let partial = HashSet::new([FileHash::from_bytes(HashAlgorithm::Sha256, &[0xAA; 32])]);
        assert_eq!(
            asset.verify(4096, &partial),
            AssetIntegrity::Unverifiable,
            "cannot-verify must not be reported as intact"
        );
    }

    #[test]
    fn media_type_tags_are_stable() {
        assert_eq!(MediaType::Container.tag(), "container");
        assert_eq!(MediaType::Screenshot.tag(), "screenshot");
    }

    #[test]
    fn hash_set_serialises_in_deterministic_order() {
        let a = serde_json::to_string(&complete_hashes()).expect("serialises");
        let b = serde_json::to_string(&complete_hashes()).expect("serialises");
        assert_eq!(a, b, "manifest bytes must be reproducible (spec §77)");
        assert!(
            a.starts_with("{\"blake3\""),
            "BTreeMap order, not insertion"
        );
    }
}
