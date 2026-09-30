//! Writes a synthetic MP4 to the path given as the first argument.
//!
//! Used to produce the corrupt-media corpus (spec §76) and to exercise the
//! CLI against structurally valid media without an external encoder.
//!
//! Pass `gopchange` as the second argument to emit a track whose GOP length
//! changes partway through, which is the condition spec §15 describes.

use std::path::PathBuf;

use tpt_app_media_forensics_container::fixture::{build_mp4, build_mp4_stsd_gop_change, TrackSpec};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path: PathBuf = args
        .next()
        .expect("usage: make_fixture <output.mp4> [gopchange]")
        .into();
    let variant = args.next().unwrap_or_default();

    let bytes = if variant == "gopchange" {
        build_mp4_stsd_gop_change()
    } else {
        build_mp4(&TrackSpec::video_25fps(1920, 1080, 250))
    };
    std::fs::write(path, bytes)
}
