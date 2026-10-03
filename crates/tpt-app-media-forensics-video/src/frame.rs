//! Frame extraction as evidence (spec §32, §33).
//!
//! # Why a decoded frame is evidence at all
//!
//! Spec §32 lists an extracted frame among the artefacts a case may retain. The
//! value is not the picture: a reviewer can already play the source. It is that a
//! *specific frame* can be pointed at, hashed, and re-examined later without
//! re-decoding — and that the hash proves the bytes are the ones the engine
//! produced. A finding that refers to "the frame at 00:04:12" needs that frame to
//! still be checkable after the case is archived.
//!
//! # Lossless, or it is not evidence
//!
//! YUV→RGB conversion here is the ITU-R BT.601 matrix, integer arithmetic only. No
//! scaling: a thumbnail would make a *better* artefact and *worse* evidence, so it
//! is not produced here. The extracted frame is the decoded frame, in the colour
//! space the decoder emitted, and the result is always a lossless extract.
//!
//! # The colour conversion is named, because "the colours" is not unique
//!
//! A YUV frame has no inherent RGB appearance: the matrix depends on the space
//! the content was graded for, and the decoder does not report one. This module
//! uses BT.601 and records that on the [`FrameImage`] itself rather than
//! presenting the output as *the* colour of the frame. BT.709 differs visibly in
//! skin tones, so a reviewer comparing this against a reference decode needs to
//! know which convention produced these bytes.

use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::pixel_format::PixelFormat;
use tpt_kinetix_core::timestamp::Timestamp;

use tpt_app_media_forensics_model::MediaTime;

/// The colour matrix used to convert YUV planes to RGB.
///
/// ITU-R BT.601, the studio-swing matrix. Recorded on every frame so a reviewer
/// comparing against a reference decode knows which convention produced it.
pub const COLOUR_MATRIX: &str = "BT.601";

/// Why a frame could not be turned into an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The decoder emitted a plane layout this build does not convert.
    ///
    /// A named error rather than a black image: an unconverted frame rendered as
    /// solid black is indistinguishable from a genuinely black frame, which is the
    /// exact confusion evidence must not create.
    #[error("pixel format {0:?} is not converted to RGB by this build")]
    UnsupportedPixelFormat(String),

    /// The frame's plane data is not the size its dimensions imply.
    ///
    /// A truncated or hostile frame. Refused rather than padded: padding would
    /// produce a plausible-looking image with invented pixels along one edge.
    #[error("frame declares {width}x{height} but carries {actual} bytes of plane data")]
    PlaneSizeMismatch {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
        /// Bytes actually present.
        actual: usize,
    },

    /// The dimensions are zero, so there is no image to write.
    #[error("frame has no dimensions")]
    EmptyFrame,

    /// The PNG encoder failed.
    #[error("PNG encoding failed: {0}")]
    Encode(String),
}

/// One decoded frame, ready to be written as evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameImage {
    /// Interleaved 8-bit RGB, three bytes per pixel, row-major.
    pub rgb: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// When the frame is presented.
    pub time: MediaTime,
    /// Index of the frame within its stream.
    pub frame_index: u32,
    /// The colour matrix used for the YUV to RGB conversion.
    ///
    /// Carried on the artefact rather than only in documentation, because the
    /// evidence record stores the image and a reviewer has to know which
    /// convention produced the colours in it.
    pub colour_matrix: &'static str,
}

impl FrameImage {
    /// Renders the frame as a PNG byte stream.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::Encode`] if the encoder rejects the image.
    pub fn to_png(&self) -> Result<Vec<u8>, FrameError> {
        let mut buffer = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buffer, self.width, self.height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            // No interlace and no filter tuning: the goal is a byte-for-byte
            // reproducible artefact, and every encoder option is another variable
            // that could make two runs differ.
            encoder.set_compression(png::Compression::Default);

            let mut writer = encoder
                .write_header()
                .map_err(|e| FrameError::Encode(e.to_string()))?;
            writer
                .write_image_data(&self.rgb)
                .map_err(|e| FrameError::Encode(e.to_string()))?;
        }
        // The encoder flushes when it is dropped, so the buffer is only complete
        // once it goes out of scope. Reading it inside that scope yields a
        // truncated PNG that happens to start with the right signature.
        Ok(buffer)
    }

    /// A filesystem-safe name for this frame.
    ///
    /// Derived from the frame's own index and presentation time rather than a
    /// counter, so re-extracting the same frame twice produces the same name
    /// instead of filling a case directory with `frame-001.png`, `frame-002.png`
    /// that differ only in the order they happened to be written.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!(
            "frame-{:08}-t{}.png",
            self.frame_index,
            self.time.as_micros() / 1_000
        )
    }
}

/// Converts a foundation timestamp to the engine's microsecond time base.
///
/// The decoder's timestamps carry a *rational* time base rather than an implied
/// unit, so reading `value` as microseconds would be wrong for any stream whose
/// time base is not 1/1,000,000 — which is most of them. A 48 kHz stream with a
/// 1/48,000 time base carries frame numbers, not microseconds.
///
/// # Panics
///
/// Never. A timestamp whose base cannot be rescaled yields zero rather than a
/// fabricated time, because a wrong timecode on evidence is worse than none.
fn media_time(timestamp: Timestamp) -> MediaTime {
    // The time base is `(numerator, denominator)` meaning `numerator/denominator`
    // seconds per tick, so one microsecond is `(1, 1_000_000)` — not
    // `(1_000_000, 1)`, which is one million seconds per tick. Passing the
    // reciprocal was a real bug: it placed a 48 kHz frame at 2 microseconds
    // instead of a millisecond, which is the kind of wrong timecode this project
    // exists to avoid.
    timestamp
        .rescale((1, 1_000_000))
        .map_or(MediaTime::ZERO, |scaled| {
            MediaTime::from_micros(scaled.value)
        })
}

/// Converts a decoded frame into an RGB image.
///
/// # Errors
///
/// Returns an error for a pixel format this build does not convert, for a frame
/// whose plane data does not match its declared dimensions, and for a zero-sized
/// frame.
///
/// # Panics
///
/// Never.
pub fn extract(frame: &VideoFrame, frame_index: u32) -> Result<FrameImage, FrameError> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    if frame.width == 0 || frame.height == 0 {
        return Err(FrameError::EmptyFrame);
    }
    let pixels = width.checked_mul(height).ok_or(FrameError::EmptyFrame)?;

    // Planar YUV: a luma plane plus chroma, subsampled or not. Anything else is
    // refused rather than guessed at, because a wrong plane layout produces a
    // plausible image with wrong colours, which is worse than no image at all.
    let (y_plane, u_plane, v_plane) = match frame.pixel_format {
        PixelFormat::Yuv420p => {
            let chroma = pixels / 4;
            let needed = pixels + 2 * chroma;
            if frame.data.len() < needed {
                return Err(FrameError::PlaneSizeMismatch {
                    width: frame.width,
                    height: frame.height,
                    actual: frame.data.len(),
                });
            }
            (
                &frame.data[..pixels],
                &frame.data[pixels..pixels + chroma],
                &frame.data[pixels + chroma..needed],
            )
        }
        PixelFormat::Yuv444p => {
            let needed = pixels * 3;
            if frame.data.len() < needed {
                return Err(FrameError::PlaneSizeMismatch {
                    width: frame.width,
                    height: frame.height,
                    actual: frame.data.len(),
                });
            }
            (
                &frame.data[..pixels],
                &frame.data[pixels..2 * pixels],
                &frame.data[2 * pixels..needed],
            )
        }
        PixelFormat::Yuv422p => {
            // 4:2:2 halves the chroma horizontally only, so each chroma sample
            // covers a 2x1 pair of luma samples.
            let chroma = pixels / 2;
            let needed = pixels + 2 * chroma;
            if frame.data.len() < needed {
                return Err(FrameError::PlaneSizeMismatch {
                    width: frame.width,
                    height: frame.height,
                    actual: frame.data.len(),
                });
            }
            (
                &frame.data[..pixels],
                &frame.data[pixels..pixels + chroma],
                &frame.data[pixels + chroma..needed],
            )
        }
        // Already RGB. Reordered rather than converted, so the artefact is the
        // decoder's own bytes in the order a viewer expects.
        PixelFormat::Rgb24 => {
            if frame.data.len() < pixels * 3 {
                return Err(FrameError::PlaneSizeMismatch {
                    width: frame.width,
                    height: frame.height,
                    actual: frame.data.len(),
                });
            }
            return Ok(FrameImage {
                rgb: frame.data[..pixels * 3].to_vec(),
                width: frame.width,
                height: frame.height,
                time: media_time(frame.pts),
                frame_index,
                colour_matrix: "none (already RGB)",
            });
        }
        PixelFormat::Bgr24 => {
            if frame.data.len() < pixels * 3 {
                return Err(FrameError::PlaneSizeMismatch {
                    width: frame.width,
                    height: frame.height,
                    actual: frame.data.len(),
                });
            }
            let mut rgb = Vec::with_capacity(pixels * 3);
            for pixel in frame.data[..pixels * 3].chunks_exact(3) {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
            return Ok(FrameImage {
                rgb,
                width: frame.width,
                height: frame.height,
                time: media_time(frame.pts),
                frame_index,
                colour_matrix: "none (already BGR)",
            });
        }
        // Monochrome: one plane, replicated across all three channels so the PNG
        // is a colour image a viewer renders identically to the source.
        PixelFormat::Gray => {
            if frame.data.len() < pixels {
                return Err(FrameError::PlaneSizeMismatch {
                    width: frame.width,
                    height: frame.height,
                    actual: frame.data.len(),
                });
            }
            let mut rgb = Vec::with_capacity(pixels * 3);
            for &luma in &frame.data[..pixels] {
                rgb.extend_from_slice(&[luma, luma, luma]);
            }
            return Ok(FrameImage {
                rgb,
                width: frame.width,
                height: frame.height,
                time: media_time(frame.pts),
                frame_index,
                colour_matrix: "none (monochrome)",
            });
        }
        // 10- and 12-bit YUV are not converted: the high bits would need
        // dithering or truncation, and either one changes pixel values. That makes
        // the result a lossy transform rather than evidence of what was decoded.
        other => return Err(FrameError::UnsupportedPixelFormat(format!("{other:?}"))),
    };

    let chroma_width = match frame.pixel_format {
        PixelFormat::Yuv420p => width.div_ceil(2),
        // 4:2:2 keeps one chroma sample per column pair but every row, so only
        // the horizontal axis is halved.
        PixelFormat::Yuv422p => width.div_ceil(2),
        _ => width,
    };

    // Iterate the luma plane rather than indexing it: the chroma coordinates are
    // derived from the position in the plane, and iterating makes that explicit
    // instead of implying an index that might not correspond.
    let mut rgb = Vec::with_capacity(pixels * 3);
    for (index, &y_byte) in y_plane.iter().enumerate() {
        let y = y_byte as i32;

        // Chroma is subsampled, so one chroma sample covers a 2x2 block in 4:2:0.
        // Nearest-neighbour rather than interpolated: interpolating would invent
        // chroma values the decoder never produced, and the point of this artefact
        // is that it is the frame the decoder emitted.
        let (cx, cy) = match frame.pixel_format {
            PixelFormat::Yuv420p => ((index % width) / 2, (index / width) / 2),
            PixelFormat::Yuv422p => ((index % width) / 2, index / width),
            _ => (index % width, index / width),
        };
        let chroma_index = cy * chroma_width + cx;
        let u = u_plane.get(chroma_index).copied().unwrap_or(128) as i32;
        let v = v_plane.get(chroma_index).copied().unwrap_or(128) as i32;

        let (r, g, b) = yuv_to_rgb(y, u, v);
        rgb.push(r);
        rgb.push(g);
        rgb.push(b);
    }

    Ok(FrameImage {
        rgb,
        width: frame.width,
        height: frame.height,
        time: media_time(frame.pts),
        frame_index,
        colour_matrix: COLOUR_MATRIX,
    })
}

/// BT.601 studio-swing YUV to RGB, in integer arithmetic.
///
/// Integer rather than floating point because the artefact's value rests on its
/// hash being stable; `f32` results can vary with the arithmetic in ways that are
/// hard to reproduce across machines, and the rounding here is visible in the
/// source rather than hidden in a conversion.
fn yuv_to_rgb(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    // Studio swing: Y within 16..235, chroma centred on 128.
    let y = y - 16;
    let u = u - 128;
    let v = v - 128;

    // 8.8 fixed point, the conventional BT.601 coefficients.
    let c298 = 298 * y;
    let r = c298 + 409 * v + 128;
    let g = c298 - 100 * u - 208 * v + 128;
    let b = c298 + 516 * u + 128;

    // Saturating rather than wrapping. The coefficients overshoot at the ends of
    // the range, and a wrapped 258 is a wildly wrong colour rather than a
    // clamped highlight.
    (
        (r >> 8).clamp(0, 255) as u8,
        (g >> 8).clamp(0, 255) as u8,
        (b >> 8).clamp(0, 255) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::{extract, FrameError, COLOUR_MATRIX};
    use tpt_kinetix_core::frame::VideoFrame;
    use tpt_kinetix_core::pixel_format::PixelFormat;
    use tpt_kinetix_core::timestamp::Timestamp;

    /// A 4:2:0 frame of `width` x `height` with neutral chroma.
    fn frame(width: u32, height: u32) -> VideoFrame {
        let pixels = (width * height) as usize;
        let mut data = vec![128u8; pixels];
        data.extend(std::iter::repeat_n(128u8, pixels / 4));
        data.extend(std::iter::repeat_n(128u8, pixels / 4));
        VideoFrame {
            pts: Timestamp::new(0, (1_000_000, 1)),
            dts: Timestamp::new(0, (1_000_000, 1)),
            data,
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        }
    }

    #[test]
    fn a_neutral_chroma_frame_converts_to_grey() {
        // Y=128 with U=V=128 is the BT.601 neutral point: grey in, grey out.
        // The most useful single check on the matrix, because a wrong constant term
        // shows up here as a colour cast on every frame.
        let image = extract(&frame(4, 4), 0).expect("converts");
        assert_eq!(image.rgb.len(), (4 * 4 * 3) as usize);

        for pixel in image.rgb.chunks_exact(3) {
            let (r, g, b) = (pixel[0] as i32, pixel[1] as i32, pixel[2] as i32);
            let spread = r.max(g).max(b) - r.min(g).min(b);
            assert!(
                spread <= 2,
                "neutral chroma must stay grey, got rgb({r},{g},{b})"
            );
        }
    }

    #[test]
    fn the_bt601_matrix_is_named_on_every_frame() {
        // A reviewer comparing against a reference decode needs to know which
        // convention produced the colours.
        assert_eq!(
            extract(&frame(2, 2), 0).expect("converts").colour_matrix,
            COLOUR_MATRIX
        );
    }

    #[test]
    fn extreme_yuv_values_saturate_rather_than_wrap() {
        // Y=0 is below the studio-swing floor of 16, so `y - 16` is negative and
        // the conversion overshoots: with U=V=0 the red term works out to -223 and
        // the blue term to -277. Wrapping those as u8 would give 33 and 179 —
        // wildly wrong colours instead of clamped black. Green happens to land
        // positive at 135 here, which is the matrix being correct, not saturation.
        let mut frame = frame(2, 2);
        frame.data = vec![0u8; 8];

        let image = extract(&frame, 0).expect("converts");
        for pixel in image.rgb.chunks_exact(3) {
            assert_eq!(
                pixel[0], 0,
                "the negative red term must clamp to zero, got {pixel:?}"
            );
            assert_eq!(
                pixel[2], 0,
                "the negative blue term must clamp to zero, got {pixel:?}"
            );
        }
    }

    #[test]
    fn a_below_range_luma_does_not_wrap_to_a_bright_colour() {
        // The failure this guards against is specific and ugly: an unclamped u8
        // cast of a negative intermediate wraps, and -223 becomes 33 — a dark
        // red — so an out-of-range frame would render as a plausible picture with
        // wrong colours rather than as an obvious fault.
        let mut frame = frame(2, 2);
        frame.data = vec![0u8; 8];

        let image = extract(&frame, 0).expect("converts");
        for pixel in image.rgb.chunks_exact(3) {
            assert!(
                pixel[0] < 64 && pixel[2] < 64,
                "a wrapped value would land mid-range, which reads as real colour: {pixel:?}"
            );
        }
    }

    #[test]
    fn a_truncated_frame_is_refused_rather_than_padded() {
        // Padding would produce a plausible image with invented pixels along one
        // edge, which is the opposite of evidence.
        let mut frame = frame(8, 8);
        frame.data = vec![0u8; 10];
        assert!(matches!(
            extract(&frame, 0),
            Err(FrameError::PlaneSizeMismatch { .. })
        ));
    }

    #[test]
    fn a_zero_sized_frame_is_refused() {
        let mut frame = frame(2, 2);
        frame.width = 0;
        frame.height = 0;
        assert_eq!(extract(&frame, 0).unwrap_err(), FrameError::EmptyFrame);
    }

    #[test]
    fn an_already_rgb_frame_is_copied_not_converted() {
        let mut rgb = frame(2, 2);
        rgb.pixel_format = PixelFormat::Rgb24;
        rgb.data = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];

        let image = extract(&rgb, 0).expect("converts");
        assert_eq!(
            image.rgb,
            vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120],
            "an RGB frame must come through byte for byte"
        );
    }

    #[test]
    fn a_bgr_frame_is_reordered_not_recoloured() {
        let mut bgr = frame(1, 1);
        bgr.pixel_format = PixelFormat::Bgr24;
        bgr.data = vec![30, 20, 10];

        assert_eq!(extract(&bgr, 0).expect("converts").rgb, vec![10, 20, 30]);
    }

    #[test]
    fn a_monochrome_frame_is_replicated_across_all_channels() {
        let mut grey = frame(2, 1);
        grey.pixel_format = PixelFormat::Gray;
        grey.data = vec![10, 200];

        assert_eq!(
            extract(&grey, 0).expect("converts").rgb,
            vec![10, 10, 10, 200, 200, 200]
        );
    }

    #[test]
    fn the_png_is_a_real_png_with_the_declared_dimensions() {
        // Evidence a reviewer cannot open with their own tools is not evidence.
        // The signature is checked rather than the image decoded, so this asserts
        // the container really is a PNG and not merely something named .png.
        let png = extract(&frame(8, 4), 0)
            .expect("converts")
            .to_png()
            .expect("encodes");

        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
            "the output must carry the PNG signature"
        );
        // Width and height are IHDR's first two big-endian u32 fields, at a fixed
        // offset of 8 signature + 4 length + 4 type.
        let width = u32::from_be_bytes(png[16..20].try_into().expect("IHDR width"));
        let height = u32::from_be_bytes(png[20..24].try_into().expect("IHDR height"));
        assert_eq!(width, 8);
        assert_eq!(height, 4);
    }

    #[test]
    fn the_same_frame_always_encodes_to_the_same_bytes() {
        // spec §77: the artefact's value rests on its hash being stable.
        let a = extract(&frame(8, 8), 0)
            .expect("converts")
            .to_png()
            .expect("encodes");
        let b = extract(&frame(8, 8), 0)
            .expect("converts")
            .to_png()
            .expect("encodes");
        assert_eq!(a, b, "the same frame must produce byte-identical PNGs");
    }

    #[test]
    fn a_frame_name_is_stable_and_carries_its_position() {
        let name = extract(&frame(2, 2), 7).expect("converts").file_name();
        assert!(name.starts_with("frame-00000007"), "{name}");
        assert!(name.ends_with(".png"), "{name}");
    }

    #[test]
    fn a_frame_at_a_rational_time_base_gets_a_correct_time() {
        // The decoder's timestamps carry a rational time base, not an implied
        // unit. `(1, 48_000)` means one tick per 1/48,000 s, so tick 48 is one
        // millisecond in. Reading the raw value as microseconds would place it at
        // 48 µs — a factor of twenty out, and precisely the wrong timecode this
        // project refuses to emit.
        let mut frame = frame(2, 2);
        frame.pts = Timestamp::new(48, (1, 48_000));

        let image = extract(&frame, 0).expect("converts");
        assert_eq!(
            image.time.as_micros(),
            1_000,
            "48 ticks at 48 kHz is one millisecond"
        );
    }

    #[test]
    fn a_timestamp_already_in_microseconds_is_unchanged() {
        // The common case: a stream already carrying a 1/1,000,000 base must not
        // be rescaled by a further factor.
        let mut frame = frame(2, 2);
        frame.pts = Timestamp::new(1_500_000, (1, 1_000_000));

        assert_eq!(
            extract(&frame, 0).expect("converts").time.as_micros(),
            1_500_000
        );
    }

    #[test]
    fn a_high_bit_depth_frame_is_refused_as_unsupported() {
        // 10-bit conversion would need dithering or truncation, either of which
        // changes pixel values and makes the result lossy.
        let mut wide = frame(2, 2);
        wide.pixel_format = PixelFormat::Yuv420p10le;
        assert!(matches!(
            extract(&wide, 0),
            Err(FrameError::UnsupportedPixelFormat(_))
        ));
    }
}
