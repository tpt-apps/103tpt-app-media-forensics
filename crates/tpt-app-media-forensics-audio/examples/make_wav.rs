//! Writes a synthetic WAV for exercising the audio analysis path.
//!
//! Produces a 1 kHz tone followed by a silent passage, so silence detection
//! and loudness gating both have something to find. No external encoder needed.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .expect("usage: make_wav <output.wav> [rate]")
        .into();
    let rate: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(48_000);

    let seconds = 3.0f64;
    let frames = (rate as f64 * seconds) as usize;
    let tone_end = frames / 2;
    let channels = 1u16;
    let bits = 16u16;

    let mut data: Vec<u8> = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let amplitude = if i < tone_end {
            (std::f64::consts::TAU * 1000.0 * i as f64 / rate as f64).sin()
        } else {
            0.0
        };
        let sample = (amplitude * i16::MAX as f64) as i16;
        data.extend_from_slice(&sample.to_le_bytes());
    }

    let byte_rate = rate * channels as u32 * (bits as u32 / 8);
    let mut wav: Vec<u8> = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&(channels * bits / 8).to_le_bytes());
    wav.extend_from_slice(&bits.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);

    std::fs::write(path, wav)
}
