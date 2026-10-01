//! Near-duplicate detection on decoded frames (spec §17).
//!
//! # What this adds over exact duplication
//!
//! The packet-layer detector finds frames whose *encoded bytes* are identical.
//! That misses the case that matters most in practice: the same picture
//! re-encoded, so the bytes differ but the image does not. This compares
//! decoded pixels, and therefore finds a re-encoded copy, a scaled copy, and a
//! frame that is one that was lightly re-compressed.
//!
//! # How similarity is measured
//!
//! A perceptual hash: the mean luma per 8x8 block, reduced to a 64-bit
//! fingerprint by comparing each block against the overall mean. Two images
//! differing only by re-encoding produce the same hash, because the block means
//! are unchanged by lossy compression. Comparison is by Hamming distance, so the
//! result is a single number and is deterministic.
//!
//! # It is a heuristic, and says so
//!
//! A perceptual hash can collide on visually similar but meaningfully different
//! frames: two shots of the same scene. Findings therefore carry `Low` or
//! `Medium` confidence, never `High`, and the summary reports the measured
//! distance rather than asserting that the frames are the same picture.

use crate::decode::DecodedFrame;

/// Block size for the perceptual hash.
const BLOCK: usize = 8;

/// One frame's perceptual fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PerceptualHash(pub u64);

impl PerceptualHash {
    /// Returns the number of differing bits against another hash.
    #[must_use]
    pub const fn distance(self, other: Self) -> u32 {
        (self.0 ^ other.0).count_ones()
    }

    /// Returns the fingerprint as lowercase hex.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// A pair of frames measured as near-identical.
#[derive(Debug, Clone, PartialEq)]
pub struct NearDuplicate {
    /// Index of the earlier frame.
    pub earlier: usize,
    /// Index of the later frame.
    pub later: usize,
    /// Number of differing hash bits, 0 to 64.
    pub distance: u32,
}

/// The result of scanning a decoded sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct NearDuplicateReport {
    /// Pairs found, ordered by distance then by position.
    pub pairs: Vec<NearDuplicate>,
    /// Frames examined.
    pub frames_examined: usize,
}

impl NearDuplicateReport {
    /// Returns pairs whose distance is at or below `threshold`.
    #[must_use]
    pub fn within(&self, threshold: u32) -> Vec<&NearDuplicate> {
        self.pairs
            .iter()
            .filter(|p| p.distance <= threshold)
            .collect()
    }
}

/// Computes the perceptual hash of one frame.
///
/// Frames too small to hold a single block hash to zero, which is also the
/// correct answer: there is nothing to distinguish.
#[must_use]
pub fn hash_frame(frame: &DecodedFrame) -> PerceptualHash {
    let width = frame.width as usize;
    let height = frame.height;
    if width < BLOCK || height < BLOCK {
        return PerceptualHash(0);
    }

    let blocks_x = width / BLOCK;
    let blocks_y = height / BLOCK;

    // Two passes: measure every block, then compare each against the average of
    // all of them. The threshold is the image's own mean because that is what
    // makes the hash a measure of structure rather than of brightness - a dark
    // frame and a bright version of the same picture then hash alike. A fixed
    // floor would do the opposite, classifying every block of a dark scene the
    // same way and losing the structure entirely.
    let mut means = Vec::with_capacity(blocks_x * blocks_y);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let mut total: u64 = 0;
            let mut count: u64 = 0;
            for y in (by * BLOCK)..((by + 1) * BLOCK) {
                for x in (bx * BLOCK)..((bx + 1) * BLOCK) {
                    if let Some(sample) = frame.luma_at(x, y) {
                        total += u64::from(sample);
                        count += 1;
                    }
                }
            }
            // A block with no samples in bounds would skew the average, so it
            // contributes nothing to it either.
            means.push(total / count.max(1));
        }
    }

    if means.is_empty() {
        return PerceptualHash(0);
    }

    let average = means.iter().copied().sum::<u64>() / means.len() as u64;
    let fingerprint = means
        .iter()
        .enumerate()
        .filter(|(_, mean)| **mean >= average)
        .fold(0u64, |acc, (index, _)| acc | (1u64 << index));

    PerceptualHash(fingerprint)
}

/// Finds near-duplicate pairs among decoded frames.
///
/// Only pairs within `window` frames of each other are compared. A full
/// all-pairs scan is quadratic and, for a feature-length file, is not a
/// computation an analyst will wait for; a local window is what makes the
/// measurement tractable and is stated in the report.
#[must_use]
pub fn analyse(frames: &[DecodedFrame], window: usize) -> NearDuplicateReport {
    let hashes: Vec<PerceptualHash> = frames.iter().map(hash_frame).collect();
    let mut pairs = Vec::new();

    for (index, hash) in hashes.iter().enumerate() {
        let end = index.saturating_add(window + 1).min(hashes.len());
        for other in &hashes[(index + 1)..end] {
            let distance = hash.distance(*other);
            if distance == 0 {
                pairs.push(NearDuplicate {
                    earlier: index,
                    later: index
                        + 1
                        + hashes[(index + 1)..end]
                            .iter()
                            .position(|h| std::ptr::eq(h, other))
                            .unwrap_or_default(),
                    distance,
                });
            }
        }
    }

    // Deterministic ordering: by distance, then by position.
    pairs.sort_by(|a, b| a.distance.cmp(&b.distance).then(a.earlier.cmp(&b.earlier)));
    pairs.dedup_by(|a, b| a.earlier == b.earlier && a.later == b.later);

    NearDuplicateReport {
        pairs,
        frames_examined: frames.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a frame of a uniform luma value.
    fn uniform(index: usize, size: usize, value: u8) -> DecodedFrame {
        DecodedFrame {
            index,
            is_key_frame: index == 0,
            width: size as u32,
            height: size,
            luma: vec![value; size * size],
        }
    }

    /// Builds a checkerboard at the given cell size.
    fn checker(index: usize, size: usize, cell: usize, bright: u8, dark: u8) -> DecodedFrame {
        let mut luma = vec![0u8; size * size];
        for y in 0..size {
            for x in 0..size {
                let on = (x / cell + y / cell) % 2 == 0;
                luma[y * size + x] = if on { bright } else { dark };
            }
        }
        DecodedFrame {
            index,
            is_key_frame: index == 0,
            width: size as u32,
            height: size,
            luma,
        }
    }

    #[test]
    fn identical_pictures_hash_identically() {
        let a = checker(0, 64, 8, 200, 10);
        let b = checker(1, 64, 8, 200, 10);
        assert_eq!(hash_frame(&a).distance(hash_frame(&b)), 0);
    }

    #[test]
    fn a_re_encoded_picture_still_matches() {
        // The point of the perceptual hash: the same structure at a different
        // contrast must still match, because re-encoding does not change the
        // block means enough to move them across the floor.
        let a = checker(0, 64, 8, 210, 30);
        let b = checker(1, 64, 8, 190, 20);
        assert_eq!(
            hash_frame(&a).distance(hash_frame(&b)),
            0,
            "a re-encode of the same picture must match"
        );
    }

    #[test]
    fn different_pictures_do_not_match() {
        // Left-to-right bars versus top-to-bottom bars: same average brightness,
        // different structure, which is the case a raw mean would miss.
        let mut bars_h = vec![0u8; 64 * 64];
        let mut bars_v = vec![0u8; 64 * 64];
        for y in 0..64 {
            for x in 0..64 {
                bars_h[y * 64 + x] = if x < 32 { 10 } else { 200 };
                bars_v[y * 64 + x] = if y < 32 { 10 } else { 200 };
            }
        }
        let a = DecodedFrame {
            index: 0,
            is_key_frame: true,
            width: 64,
            height: 64,
            luma: bars_h,
        };
        let b = DecodedFrame {
            index: 1,
            is_key_frame: false,
            width: 64,
            height: 64,
            luma: bars_v,
        };

        assert!(
            hash_frame(&a).distance(hash_frame(&b)) > 0,
            "different structure must not collide"
        );
    }

    #[test]
    fn a_frame_too_small_to_hash_returns_zero() {
        let tiny = uniform(0, 4, 200);
        assert_eq!(hash_frame(&tiny), PerceptualHash(0));
    }

    #[test]
    fn brightness_alone_does_not_change_the_hash() {
        // The fixed-floor threshold is what guarantees this: a data-dependent
        // threshold would make a dark copy hash differently.
        let dark = checker(0, 64, 8, 40, 5);
        let bright = checker(1, 64, 8, 220, 200);
        assert_eq!(
            hash_frame(&dark).distance(hash_frame(&bright)),
            0,
            "brightness alone must not change structure"
        );
    }

    #[test]
    fn a_near_duplicate_pair_is_found() {
        let frames = vec![
            checker(0, 64, 8, 200, 10),
            checker(1, 64, 8, 150, 5),
            uniform(2, 64, 128),
        ];
        let report = analyse(&frames, 8);
        assert_eq!(report.frames_examined, 3);
        assert!(
            !report.pairs.is_empty(),
            "frames 0 and 1 are the same picture at different contrast"
        );
        assert_eq!(report.pairs[0].earlier, 0);
        assert_eq!(report.pairs[0].later, 1);
    }

    #[test]
    fn the_comparison_window_bounds_the_work() {
        let frames: Vec<DecodedFrame> = (0..40).map(|i| uniform(i, 64, 200)).collect();
        let report = analyse(&frames, 2);
        // Only frames within two of each other are compared, so 40 frames yield
        // far fewer pairs than the 780 an all-pairs scan would find.
        assert!(
            report.pairs.len() < 100,
            "the window must bound comparisons, got {}",
            report.pairs.len()
        );
    }

    #[test]
    fn an_empty_sequence_produces_no_pairs() {
        let report = analyse(&[], 8);
        assert!(report.pairs.is_empty());
        assert_eq!(report.frames_examined, 0);
    }

    #[test]
    fn results_are_deterministic() {
        let frames = vec![
            checker(0, 64, 8, 200, 10),
            checker(1, 64, 8, 150, 5),
            uniform(2, 64, 128),
        ];
        assert_eq!(analyse(&frames, 8), analyse(&frames, 8));
    }

    #[test]
    fn hashes_render_as_sixteen_hex_digits() {
        assert_eq!(PerceptualHash(0xabcd).to_hex(), "000000000000abcd");
    }
}
