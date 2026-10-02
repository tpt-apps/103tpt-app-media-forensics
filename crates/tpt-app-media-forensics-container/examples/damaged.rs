//! Writes damaged and well-formed MP4 files for manual inspection.
//!
//! The unit tests build their bytes in memory. This exists for the case that
//! needs a real file on disk: running the CLI over a genuinely truncated
//! container and reading the report an analyst would receive.
//!
//! ```text
//! cargo run -p tpt-app-media-forensics-container --example damaged -- <out-dir>
//! ```

use std::path::PathBuf;

use tpt_app_media_forensics_container::fixture::{
    build_mp4, build_mp4_with_bitrate_drop, TrackSpec,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    std::fs::create_dir_all(&dir)?;

    let whole = build_mp4(&TrackSpec::video_25fps(640, 480, 50));
    let half = whole.len() / 2;

    let clean = dir.join("clean.mp4");
    std::fs::write(&clean, &whole)?;
    println!("{} ({} bytes)", clean.display(), whole.len());

    let truncated = dir.join("truncated.mp4");
    std::fs::write(&truncated, &whole[..half])?;
    println!("{} ({} bytes)", truncated.display(), half);

    // A file whose bitrate drops over frames 60..100, for exercising §29.
    let dropped = dir.join("bitrate-drop.mp4");
    let bytes = build_mp4_with_bitrate_drop(60, 40);
    std::fs::write(&dropped, &bytes)?;
    println!("{} ({} bytes)", dropped.display(), bytes.len());

    Ok(())
}
