//! Writes a synthetic MP4 to the path given as the first argument.
//!
//! Variants:
//!   (none)       a plain valid track
//!   gopchange    keyframe spacing changes partway through (spec §15)
//!   metadata     carries metadata atoms with a deliberate scope conflict

use std::path::PathBuf;

use tpt_app_media_forensics_container::fixture::{
    build_mp4, build_mp4_stsd_gop_change, build_mp4_with_metadata, TrackSpec,
};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path: PathBuf = args
        .next()
        .expect("usage: make_fixture <output.mp4> [gopchange|metadata]")
        .into();
    let variant = args.next().unwrap_or_default();

    let track = TrackSpec::video_25fps(1920, 1080, 250);
    let bytes = match variant.as_str() {
        "gopchange" => build_mp4_stsd_gop_change(),
        "metadata" => build_mp4_with_metadata(&track),
        _ => build_mp4(&track),
    };
    std::fs::write(path, bytes)
}
