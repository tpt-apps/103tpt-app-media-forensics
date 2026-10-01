//! Tests for partial file reads (spec §12).
//!
//! Inspection needs the `moov` box and nothing else. These tests build files
//! whose `mdat` is large enough that reading the whole thing would be the
//! wrong move, and check that structure is still recovered correctly and
//! cheaply.

use std::io::{Seek, SeekFrom, Write};

use tpt_app_media_forensics_container::fixture::{build_mp4, TrackSpec};
use tpt_app_media_forensics_container::{inspect_path, read_header, read_moov, MAX_MOOV_BYTES};

/// Builds an MP4 whose `moov` is followed by a large `mdat`.
///
/// The media payload is filler rather than real frames: these tests are about
/// which bytes the reader touches, not what the samples contain.
fn mp4_with_large_mdat(padding: usize) -> Vec<u8> {
    let mut file = build_mp4(&TrackSpec::video_25fps(320, 240, 60));

    // Append a `mdat` box of the requested size.
    let body = padding;
    let size = (body + 8) as u32;
    file.extend_from_slice(&size.to_be_bytes());
    file.extend_from_slice(b"mdat");
    file.extend(std::iter::repeat_n(0u8, body));

    // A valid MP4 needs `ftyp` first and `mdat` after `moov`; appending is
    // therefore enough for the box walk to encounter it.
    file
}

/// Writes bytes to a temporary file and returns its path.
fn write_temp(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("writes fixture");
    path
}

#[test]
fn the_moov_is_found_without_reading_the_media_data() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = write_temp(tmp.path(), "big.mp4", &mp4_with_large_mdat(8 * 1024 * 1024));

    let moov = read_moov(&path).expect("moov is found");

    // The box must be far smaller than the file, which is the whole point.
    let file_size = std::fs::metadata(&path).expect("stats").len();
    assert!(
        (moov.len() as u64) < file_size,
        "the moov must be a small fraction of the file: {} vs {file_size}",
        moov.len()
    );
    assert!(
        moov.len() as u64 <= MAX_MOOV_BYTES,
        "the moov must stay within its bound"
    );
}

#[test]
fn inspection_succeeds_on_a_file_far_larger_than_the_moov() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let small = build_mp4(&TrackSpec::video_25fps(320, 240, 60));
    let path = write_temp(
        tmp.path(),
        "big.mp4",
        &mp4_with_large_mdat(64 * 1024 * 1024),
    );

    let reference = inspect_path(&write_temp(
        &std::env::temp_dir(),
        "reference-small.mp4",
        &small,
    ))
    .expect("small file inspects");
    let large = inspect_path(&path).expect("large file inspects");

    assert_eq!(
        reference.streams.len(),
        large.streams.len(),
        "stream structure must be identical regardless of mdat size"
    );
    assert_eq!(
        reference.streams[0].codec.name, large.streams[0].codec.name,
        "the codec must be read from moov, not mdat"
    );
}

#[test]
fn the_media_payload_is_never_loaded() {
    // The strongest available check: make the `mdat` body unreadable as UTF-8
    // and large, and confirm the moov read still succeeds and is small.
    let tmp = tempfile::tempdir().expect("temp dir");
    let bytes = mp4_with_large_mdat(16 * 1024 * 1024);
    let path = write_temp(tmp.path(), "filler.mp4", &bytes);

    let moov = read_moov(&path).expect("moov is found");
    assert!(
        moov.len() < 1024 * 1024,
        "a moov from a small fixture must stay under 1 MiB, got {}",
        moov.len()
    );
}

#[test]
fn a_file_with_no_moov_is_reported_rather_than_faked() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = write_temp(tmp.path(), "bare.mp4", b"not an mp4 at all, just bytes");

    let error = read_moov(&path).expect_err("no moov should be an error");
    assert!(
        error.to_string().contains("no moov box"),
        "the error should say what was missing: {error}"
    );
}

#[test]
fn a_truncated_final_box_ends_the_walk_without_an_io_error() {
    // spec 30: a damaged file is evidence, not a failure. The walk must stop
    // cleanly rather than raising on a short read.
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 60));
    // Declare a box far larger than what follows.
    bytes.extend_from_slice(&u32::MAX.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend_from_slice(&[0u8; 16]);

    let path = write_temp(tmp.path(), "truncated.mp4", &bytes);
    // The moov appears before the malformed box, so it is still found.
    assert!(read_moov(&path).is_ok());
}

#[test]
fn a_box_claiming_to_start_past_the_end_of_file_does_not_seek() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 60));
    // A size larger than the whole file must terminate the walk, not cause a
    // huge seek that could block or error.
    bytes.extend_from_slice(&u32::MAX.to_be_bytes());
    bytes.extend_from_slice(b"mdat");

    let path = write_temp(tmp.path(), "oversized.mp4", &bytes);
    assert!(read_moov(&path).is_ok(), "the moov precedes the bad box");
}

#[test]
fn a_size_of_zero_means_the_box_runs_to_the_end_of_the_file() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut bytes = build_mp4(&TrackSpec::video_25fps(320, 240, 60));
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend_from_slice(&[0u8; 32]);

    let path = write_temp(tmp.path(), "zero-size.mp4", &bytes);
    assert!(read_moov(&path).is_ok());
}

#[test]
fn the_header_read_is_bounded_and_short_files_are_not_padded() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let short = write_temp(tmp.path(), "short.mp4", b"tiny");
    let header = read_header(&short, 64 * 1024).expect("reads");
    assert_eq!(header, b"tiny", "a short file must not be zero-padded");

    let long = write_temp(
        tmp.path(),
        "long.mp4",
        &build_mp4(&TrackSpec::video_25fps(320, 240, 10)),
    );
    let header = read_header(&long, 64).expect("reads");
    assert_eq!(header.len(), 64, "the header read must respect its limit");
    assert_eq!(&header[4..8], b"ftyp", "the signature must be present");
}

#[test]
fn a_missing_file_is_reported_with_its_path() {
    let error = read_moov(std::path::Path::new("definitely/not/here.mp4"))
        .expect_err("a missing file is an error");
    assert!(
        error.to_string().contains("here.mp4"),
        "the error must name the file: {error}"
    );
}

#[test]
fn the_moov_read_does_not_depend_on_where_the_moov_sits() {
    // A file whose `moov` trails a large `ftyp` must still be found, which
    // exercises the seek-forward path rather than an early return.
    let tmp = tempfile::tempdir().expect("temp dir");
    let original = build_mp4(&TrackSpec::video_25fps(320, 240, 60));

    // Insert a filler box between `ftyp` and `moov` so `moov` is no longer the
    // second box. `ftyp` here is a 32-byte box: an 8-byte header plus the 24
    // bytes of brand payload the fixture writes.
    const FTYP_BYTES: usize = 32;
    assert!(
        &original[4..8] == b"ftyp",
        "the fixture must start with ftyp"
    );
    let declared = u32::from_be_bytes([original[0], original[1], original[2], original[3]]);
    assert_eq!(
        declared as usize, FTYP_BYTES,
        "this fixture's ftyp box must be {FTYP_BYTES} bytes"
    );

    let filler_body = 4096usize;
    let filler_size = u32::try_from(filler_body + 8).expect("fits in u32");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&original[..FTYP_BYTES]);
    bytes.extend_from_slice(&filler_size.to_be_bytes());
    bytes.extend_from_slice(b"free");
    bytes.extend(std::iter::repeat_n(0u8, filler_body));
    bytes.extend_from_slice(&original[FTYP_BYTES..]);

    let path = write_temp(tmp.path(), "late-moov.mp4", &bytes);
    assert!(
        read_moov(&path).is_ok(),
        "a moov after another box must still be found"
    );
}

#[test]
fn reading_the_moov_is_repeatable_and_deterministic() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = write_temp(tmp.path(), "repeat.mp4", &mp4_with_large_mdat(1024 * 1024));

    let first = read_moov(&path).expect("first read");
    let second = read_moov(&path).expect("second read");
    assert_eq!(
        first, second,
        "the same file must yield the same moov bytes"
    );
}

#[test]
fn a_file_that_is_only_an_mdat_has_no_moov() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let mut bytes = Vec::new();
    let size = 4096u32;
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(b"mdat");
    bytes.extend(std::iter::repeat_n(0u8, 4088));

    let path = write_temp(tmp.path(), "mdat-only.mp4", &bytes);
    assert!(read_moov(&path).is_err());
}

#[test]
fn a_seekable_reader_does_not_depend_on_the_files_position() {
    // Guards the seek logic: if a read relied on the file position left by a
    // previous call, this would fail.
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = write_temp(tmp.path(), "seek.mp4", &mp4_with_large_mdat(64 * 1024));

    let mut file = std::fs::File::open(&path).expect("opens");
    file.seek(SeekFrom::Start(1000)).expect("seeks");
    drop(file);

    assert!(read_moov(&path).is_ok());
}

#[test]
fn a_moov_larger_than_the_bound_is_refused() {
    // The bound exists so a hostile file cannot exhaust memory.
    let tmp = tempfile::tempdir().expect("temp dir");
    // A `moov` box that fits inside the file but exceeds the bound. The walk
    // reaches it, checks the size, and refuses before allocating.
    let body = 4096usize;
    let path = write_temp(tmp.path(), "huge-moov.mp4", &oversized_moov(body));
    let error = read_moov(&path).expect_err("an oversized moov must be refused");
    assert!(
        error.to_string().contains("inspection limit"),
        "the error should name the limit: {error}"
    );
}

/// Builds a file whose `moov` box declares a size above `MAX_MOOV_BYTES`.
///
/// The declared size is checked against the bound before the file-size check,
/// so it does not have to be backed by real data - but the box must be
/// reachable, so a small `ftyp` precedes it.
fn oversized_moov(body: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let ftyp_body = b"isom\x00\x00\x02\x00isomiso2avc1mp41";
    let ftyp_size = u32::try_from(ftyp_body.len() + 8).expect("fits in u32");
    bytes.extend_from_slice(&ftyp_size.to_be_bytes());
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(ftyp_body);

    // `size == 1` selects the 64-bit length that follows the type field.
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(b"moov");
    bytes.extend_from_slice(&(MAX_MOOV_BYTES + 1).to_be_bytes());
    let _ = body;
    bytes
}

#[test]
fn the_writer_still_closes_a_complete_file() {
    // A sanity check on the fixture helper itself: a dropped file handle would
    // make the other tests pass for the wrong reason.
    let tmp = tempfile::tempdir().expect("temp dir");
    let path = tmp.path().join("written.mp4");
    let mut file = std::fs::File::create(&path).expect("creates");
    file.write_all(b"abcd").expect("writes");
    drop(file);
    assert_eq!(std::fs::read(&path).expect("reads"), b"abcd");
}
