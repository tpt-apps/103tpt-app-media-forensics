//! probe
fn main() {
    use tpt_app_media_forensics_container::fixture::{build_mp4_av, TrackSpec};
    let b = build_mp4_av(&TrackSpec::video_25fps(320,240,10), &TrackSpec::audio_48khz(2400), 40);
    let moov = &b[32..32+10598];
    for at in [8usize, 116, 124, 565] {
        if at + 8 > moov.len() { continue; }
        println!("at {at} declared {} kind {:?}", u32::from_be_bytes(moov[at..at+4].try_into().unwrap()), String::from_utf8_lossy(&moov[at+4..at+8]));
    }
}