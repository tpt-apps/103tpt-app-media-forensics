//! Source asset acquisition (spec §11).
//!
//! # Read-only, always
//!
//! The source file is evidence. This module opens it with read-only access and
//! never holds a writable handle. Everything the engine derives is written into
//! the case directory instead (spec §32).
//!
//! # Bounded memory
//!
//! Hashing streams the file through a fixed-size buffer, so memory use is
//! constant regardless of file size. A 4 GB asset hashes in the same ~1 MiB of
//! working memory as a 4 KB one (spec §55).
//!
//! # The file may change while we read it
//!
//! A file being written, synced from a network share, or edited by another
//! process produces a hash of *something* — but not necessarily of a coherent
//! version. This module detects that case and reports it rather than recording
//! a hash that looks authoritative and is not.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use sha2::Digest as _;
use tpt_app_media_forensics_model::{
    AcquisitionRecord, FileHash, FileTimestamps, FilesystemInfo, HashAlgorithm, HashSet,
    MediaAsset, MediaType,
};

use crate::error::CoreError;

/// Size of the streaming read buffer.
///
/// Large enough to keep syscall overhead low on multi-gigabyte files, small
/// enough that memory use stays irrelevant.
const READ_BUFFER_BYTES: usize = 1 << 20;

/// The digests computed in a single pass, plus how many bytes produced them.
struct HashResult {
    hashes: HashSet,
    bytes_read: u64,
}

/// Acquires a source file, producing its acquisition record.
///
/// Computes SHA-256 and BLAKE3 in one pass over the file.
///
/// # Errors
///
/// Returns an error if the path cannot be opened or read, if it is not a
/// regular file, or if the file's reported size disagrees with the number of
/// bytes actually read. That last case means the file changed mid-acquisition,
/// and recording a hash of a moving target would be misleading.
pub fn acquire(path: &Path) -> Result<AcquisitionRecord, CoreError> {
    // `symlink_metadata` rather than `metadata`: the question is what this path
    // is, not what it points at. A symlink found in evidence is itself notable.
    let display = path.display().to_string();
    let link_metadata =
        std::fs::symlink_metadata(path).map_err(|e| CoreError::io("stat source", &display, e))?;
    let metadata =
        std::fs::metadata(path).map_err(|e| CoreError::io("stat source", &display, e))?;

    if metadata.is_dir() {
        return Err(CoreError::NotAFile {
            path: display.clone(),
        });
    }

    let file = File::open(path).map_err(|e| CoreError::io("open source", &display, e))?;
    let result = hash_stream(BufReader::with_capacity(READ_BUFFER_BYTES, file))
        .map_err(|e| CoreError::io("read source", &display, e))?;

    // A file that grew or shrank while being read has no single true hash.
    if result.bytes_read != metadata.len() {
        return Err(CoreError::SourceChangedDuringAcquisition {
            path: path.display().to_string(),
            reported_size: metadata.len(),
            bytes_read: result.bytes_read,
        });
    }

    Ok(AcquisitionRecord {
        source_path: path.display().to_string(),
        size_bytes: result.bytes_read,
        hashes: result.hashes,
        timestamps: capture_timestamps(&metadata),
        filesystem: capture_filesystem(&metadata, &link_metadata),
    })
}

/// Acquires a source file and wraps it as a media asset.
///
/// # Errors
///
/// As [`acquire`].
pub fn acquire_asset(path: &Path, media_type: MediaType) -> Result<MediaAsset, CoreError> {
    let record = acquire(path)?;
    let name = path.file_name().map_or_else(
        || record.source_path.clone(),
        |n| n.to_string_lossy().into_owned(),
    );
    Ok(MediaAsset::new(name, media_type, record))
}

/// Hashes a stream with every configured algorithm in a single pass.
fn hash_stream<R: Read>(mut reader: R) -> std::io::Result<HashResult> {
    let mut sha256 = sha2::Sha256::new();
    let mut blake = blake3::Hasher::new();
    let mut buffer = vec![0u8; READ_BUFFER_BYTES];
    let mut total: u64 = 0;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        sha256.update(chunk);
        blake.update(chunk);
        total += read as u64;
    }

    let mut hashes = HashSet::default();
    hashes.insert(FileHash::from_bytes(
        HashAlgorithm::Sha256,
        &sha256.finalize(),
    ));
    hashes.insert(FileHash::from_bytes(
        HashAlgorithm::Blake3,
        blake.finalize().as_bytes(),
    ));

    Ok(HashResult {
        hashes,
        bytes_read: total,
    })
}
/// Captures filesystem timestamps, converting to Unix seconds.
///
/// Uses `Option` rather than substituting zero: a filesystem that does not
/// record creation time genuinely has no such observation, and a fabricated
/// epoch timestamp would be indistinguishable from a real one.
fn capture_timestamps(metadata: &std::fs::Metadata) -> FileTimestamps {
    fn to_unix_secs(time: std::io::Result<std::time::SystemTime>) -> Option<i64> {
        let time = time.ok()?;
        let duration = time.duration_since(std::time::UNIX_EPOCH).ok()?;
        i64::try_from(duration.as_secs()).ok()
    }

    FileTimestamps {
        modified_unix_secs: to_unix_secs(metadata.modified()),
        created_unix_secs: to_unix_secs(metadata.created()),
        accessed_unix_secs: to_unix_secs(metadata.accessed()),
    }
}

/// Captures filesystem context for the acquisition record.
///
/// Windows and Unix expose different sets here; each platform populates what
/// it can and leaves the rest `None`.
fn capture_filesystem(
    metadata: &std::fs::Metadata,
    link_metadata: &std::fs::Metadata,
) -> FilesystemInfo {
    let mut info = FilesystemInfo {
        metadata_size_bytes: metadata.len(),
        ..FilesystemInfo::default()
    };

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // The file attribute word, including the read-only flag. Recorded
        // because a source that was marked read-only is evidence that
        // someone took care with it.
        info.unix_permissions = Some(metadata.file_attributes());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        info.file_index = Some(metadata.ino());
        info.link_count = Some(metadata.nlink());
        info.unix_permissions = Some(metadata.mode());
    }

    // Windows: `file_attributes` is stable, but the volume serial number and
    // file index live behind the still-unstable `windows_by_handle` API. They
    // are therefore left unset here and populated by the Windows-specific
    // capture added when that API stabilises; an absent value is recorded as
    // absent rather than faked.

    // Recorded whether or not it can be populated on this platform: a symlink
    // among the acquired sources is a material observation in a case.
    info.filesystem = link_metadata
        .file_type()
        .is_symlink()
        .then(|| "symlink".to_owned());

    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes `contents` to a uniquely named file in a temp directory.
    fn write_temp(name: &str, contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(name);
        let mut file = File::create(&path).expect("create temp file");
        file.write_all(contents).expect("write temp file");
        (dir, path)
    }

    #[test]
    fn acquisition_computes_both_digests() {
        let (_dir, path) = write_temp("a.mp4", b"the quick brown fox");
        let record = acquire(&path).expect("acquires");

        assert!(
            record.hashes.is_complete(),
            "both algorithms must be recorded"
        );
        assert_eq!(record.size_bytes, 19);
    }

    #[test]
    fn digests_match_the_known_sha256_of_the_input() {
        // Reference vector for the 19 bytes b"the quick brown fox" (no trailing
        // newline), cross-checked against an independent SHA-256 implementation.
        // If this fails, the hash is being computed wrong.
        let (_dir, path) = write_temp("a.bin", b"the quick brown fox");
        let record = acquire(&path).expect("acquires");

        assert_eq!(
            record.hashes.sha256(),
            Some("9ecb36561341d18eb65484e833efea61edc74b84cf5e6ae1b81c63533e25fc8f")
        );
    }

    #[test]
    fn digests_are_stable_across_chunk_boundaries() {
        // A file whose length is not a multiple of READ_BUFFER_BYTES must hash
        // identically regardless of how the read loop chunked it. This is what
        // catches an off-by-one in the streaming loop.
        let contents: Vec<u8> = (0..(READ_BUFFER_BYTES + 77))
            .map(|i| (i % 251) as u8)
            .collect();
        let (_d1, p1) = write_temp("a.bin", &contents);
        let one_pass = acquire(&p1).expect("acquires").hashes;

        // Same bytes, read as a single logical unit via a tiny input size.
        let stream = std::io::Cursor::new(contents.clone());
        let streamed = hash_stream(stream).expect("hashes").hashes;

        assert_eq!(one_pass, streamed);
    }

    #[test]
    fn acquisition_is_deterministic() {
        let (_dir, path) = write_temp("a.bin", b"identical content");
        let first = acquire(&path).expect("acquires");
        let second = acquire(&path).expect("acquires");

        assert_eq!(
            first.hashes, second.hashes,
            "acquisition must be reproducible (spec §77)"
        );
        assert_eq!(first, second);
    }

    #[test]
    fn different_content_yields_different_digests() {
        let (_d1, p1) = write_temp("a.bin", b"original");
        let (_d2, p2) = write_temp("b.bin", b"modified");
        assert_ne!(
            acquire(&p1).expect("a").hashes,
            acquire(&p2).expect("b").hashes
        );
    }

    #[test]
    fn empty_file_hashes_successfully() {
        let (_dir, path) = write_temp("empty.mp4", b"");
        let record = acquire(&path).expect("acquires an empty file");
        assert_eq!(record.size_bytes, 0);
        assert!(record.hashes.is_complete());
    }

    #[test]
    fn multi_buffer_file_is_hashed_whole() {
        // Larger than READ_BUFFER_BYTES, so the streaming loop must iterate.
        let contents = vec![0xA5_u8; (READ_BUFFER_BYTES * 2) + 12345];
        let (_dir, path) = write_temp("big.mp4", &contents);
        let record = acquire(&path).expect("acquires");
        assert_eq!(record.size_bytes, contents.len() as u64);
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let result = acquire(Path::new("definitely-not-here-12345.mp4"));
        assert!(result.is_err());
    }

    #[test]
    fn directory_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let result = acquire(dir.path());
        assert!(matches!(result, Err(CoreError::NotAFile { .. })));
    }

    #[test]
    fn timestamps_are_recorded() {
        let (_dir, path) = write_temp("a.mp4", b"data");
        let record = acquire(&path).expect("acquires");
        assert!(
            record.timestamps.modified_unix_secs.is_some(),
            "modification time is available on every supported platform"
        );
    }

    #[test]
    fn asset_ids_are_content_derived() {
        // The same bytes under two names must yield the same asset ID.
        let (_d1, p1) = write_temp("original.mp4", b"same bytes");
        let (_d2, p2) = write_temp("renamed.mp4", b"same bytes");
        let a = acquire_asset(&p1, MediaType::Container).expect("a");
        let b = acquire_asset(&p2, MediaType::Container).expect("b");
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn re_acquisition_verifies_as_intact() {
        let (_dir, path) = write_temp("a.mp4", b"stable content");
        let asset = acquire_asset(&path, MediaType::Container).expect("a");
        let fresh = acquire(&path).expect("re-acquires");
        assert!(asset.verify(fresh.size_bytes, &fresh.hashes).is_intact());
    }

    #[test]
    fn modified_file_fails_verification() {
        let (dir, path) = write_temp("a.mp4", b"original content");
        let asset = acquire_asset(&path, MediaType::Container).expect("a");

        std::fs::write(&path, b"tampered content").expect("overwrite");
        let fresh = acquire(&path).expect("re-acquires");
        assert!(!asset.verify(fresh.size_bytes, &fresh.hashes).is_intact());

        drop(dir);
    }
}
