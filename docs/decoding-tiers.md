# Decoding Tiers

Whether an analysis needs decoded pixels, or only packet and container data,
is the single biggest architectural question in this project. It determines how
much of the forensic signal survives a stream the decoder cannot handle, and it
decides what analysis can safely depend on a large third-party component.

## Two tiers

```text
Tier 1  packet / container          Tier 2  pixel / sample
─────────────────────────────      ─────────────────────────────
container boxes                   decoded frames (YUV/RGB)
stream enumeration                decoded audio PCM
PTS/DTS, timebase, edit lists     frame luminance, histogram
keyframe positions (stss)         perceptual hashes, scene changes
sample tables (stsz/stsc/stco)    duplicate / near-duplicate detection
GOP structure                     loudness, spectrum, silence
trailing data, padding
```

**Tier 1 runs always.** It is cheap, needs no decoder, and cannot be defeated
by a malformed bitstream.

**Tier 2 runs only when a decoder is available** and the stream is supported.

## What this buys

The strong forensic signals are Tier 1. GOP structure, keyframe distribution,
timestamp monotonicity, duration consistency, and metadata cross-checks are all
read from container boxes — and they are exactly what catches concatenation,
re-encoding, and timeline surgery.

Keeping them in Tier 1 means a file with an undecodable video track still
yields its full structural report. Had GOP analysis been built on decoded
frames, a decoder failure would have silently removed the evidence instead of
just the picture.

## H.264 is integrated, never implemented

`tpt-kinetix-h264` is an existing, pure-Rust decoder: CAVLC and CABAC, I/P/B
slices, intra and inter prediction, weighted prediction, reference-picture
management, and deblocking — verified **bit-exact against ffmpeg**. Spec §73
describes this project as a commercial layer on top of that stack.

Writing a second H.264 decoder here would duplicate a conformance-tested
component, contradict the spec's own architecture, and put an
attacker-controlled parser in the path of every forensic conclusion.

## `pixel_exact` matters more than it looks

`tpt-kinetix-h264` reports `capabilities().pixel_exact`, and
`with_strict(true)` makes `decode()` return `NotPixelExact` rather than
emitting approximate frames.

This is the correct behaviour for a forensic tool, and the engine must honour
it. Frame statistics — mean luminance, histograms, perceptual hashes — computed
on approximate pixels would be **fabricated evidence**: numbers that look like
measurements but describe a frame that never existed. A finding derived from
them would be indefensible if challenged.

So the rule is:

> When the decoder cannot decode pixel-exactly, Tier 2 measurements are
> withheld and the gap is reported as an explicit limitation. Tier 1 findings
> are unaffected.

A tool that quietly approximates is worse than one that says "not measured".

## Sampling must be recorded

Decoding every frame of a two-hour file is expensive, so Tier 2 will sample.
Spec §77 requires that sampling be recorded, and §21 requires the same for
measurements.

Every sampled result therefore carries its methodology — which frames, how
many, and why — and the report states it. An unsampled "this file has no
duplicate frames" claim is not supportable; "no duplicate frames among 512
keyframes sampled at 3.5 s intervals" is.