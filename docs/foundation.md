# TPT Foundation Integration

How this project consumes the TPT media foundation, and three discrepancies
between `spec.txt` and the crates as they actually exist.

## Availability

All nine foundation repositories are reachable and resolve as git
dependencies. **None are published on crates.io.**

The ecosystem convention (`tpt-solutions/tpt-av-asset` is the reference) is a
git dependency with a **pinned revision**:

```toml
tpt-kinetix-core = { git = "https://github.com/tpt-solutions/tpt-kinetix", rev = "9747a2b1..." }
```

Branches are never used. A branch pin makes the build non-reproducible, which
would break spec §77 (identical results across runs) and spec §63 (a report must
record what produced it). When a pin moves, `AnalysisVersion::CURRENT` must be
reviewed and bumped if any result could change.

> A note on how availability was determined: the GitHub **web UI** returns 404
> for several of these repositories to an unauthenticated request, while `git`
> access works normally. Do not conclude a repository is unavailable from a web
> fetch alone — test the access path cargo actually uses.

## Discrepancy 1 — the spec lists repository names, not crate names

`spec.txt` §2 lists `tpt-kinetix`, `tpt-cadence`, and so on. Those are
*repository* names. The actual crates are the sub-crates inside them:

| Spec name | Repository | Crates actually available |
|---|---|---|
| tpt-kinetix | `tpt-solutions/tpt-kinetix` | `tpt-kinetix-core`, `-demux`, `-h264`, `-av1`, `-aac`, `-bitstream`, `-mux`, `-lossless`, `-stream`, `-pipeline`, `-vision`, `-face`, `-screen`, `-realtime`, `-lean`, `-kg`, `-volumetric`, `-cli`, `-test-utils` |
| tpt-cadence | `tpt-solutions/tpt-cadence` | `tpt-av-cadence-core`, `-wav`, `-aiff`, `-flac`, `-aac`, `-opus`, `-mp3`, `-ogg`, `-vorbis`, `-pcm`, `-test-utils`, `-cli` |
| tpt-visual | `tpt-solutions/tpt-visual` | `tpt-av-visual`, `-color`, `-compositor`, `-effects`, `-timeline`, `-utils` |
| tpt-audio | `tpt-solutions/tpt-audio` | `tpt-av-audio`, `-core`, `-io`, `-plugin`, `-timeline`, `-utils` |
| tpt-voice | `tpt-solutions/tpt-voice` | `tpt-av-voice-transcribe`, `-diarize`, `-align`, `-isolate`, `-tts`, `-utils` |
| tpt-av-asset | `tpt-solutions/tpt-av-asset` | `tpt-av-asset-utils`, `-db`, `-cache`, `-proxy`, `-watcher`, `-pipeline`, `-cli` |
| tpt-av-sync | `tpt-solutions/tpt-av-sync` | `tpt-av-sync-utils`, `-crdt`, `-net`, `-playhead`, `-presence`, `-server` |
| tpt-av-test | `tpt-solutions/tpt-av-test` | `tpt-av-test-reference`, `-fuzz`, `-benchmark`, `-mock`, `-macros`, `-vectors` |
| tpt-dsp | `tpt-solutions/tpt-dsp` | `tpt-dsp-core`, `-audio`, `-analysis`, `-control`, `-io`, `-viz`, `-wasm`, `-cli` |

Note that `tpt-audio` and `tpt-visual` use the `tpt-av-` prefix for their
sub-crates, not `tpt-audio-` / `tpt-visual-`.

**Decision:** the spec's list is read as a list of *repositories to build on*.
Integration is driven by what those repositories actually expose.

## Discrepancy 2 — `tpt-av-sync` is not an A/V measurement tool

**Resolved.** `spec.txt` §23 says "Use tpt-av-sync" to calculate audio PTS,
video PTS, estimated offset, and drift. The spec is explicit.

The real `tpt-av-sync` is a **CRDT-based real-time collaboration engine**:
multi-user timeline editing, playhead synchronisation, conflict-free state
replication, presence, and a network transport layer. `tpt-cadence` describes it
as sitting "in the collaboration layer", alongside `tpt-audio` and
`tpt-visual`.

`tpt-av-sync-playhead`'s public API confirms this: `PlayheadSync::new(peer_id,
sample_rate)`, `ClockSynchronizer::observe(offset_ms, rtt_ms)`,
`DriftCompensator::on_offset_sample(offset_ms, now_ms)`. These are *peer
network* clocks measured by NTP-style round trips, not media tracks.

A source search across all six foundation repositories for `av_sync_offset`,
`audio_video_offset`, `av_offset`, `AudioVideoSync`, and `VideoAudioSync`
returns **zero matches**. No crate in the foundation measures the offset
between an audio and a video track.

**Resolution.** The §23 requirement is unmet by the foundation, so it is
implemented directly in the `-timing` crate from decoded packet timestamps. The
`DriftCompensator` *pattern* — EWMA smoothing over a series of offset samples —
is a sound model for that problem and is reimplemented there, but the crate
itself is not used: it is a networking and collaboration crate, and pulling
network code into an offline forensic analysis path would be wrong regardless
of how its API happens to look.

The spec's line is left in `todo.md` reworded rather than deleted, so the
original intent stays traceable.

## Discrepancy 3 — `tpt-visual` requires a GPU

`tpt-visual` composites and processes video through `wgpu` (Vulkan, Metal,
D3D12, GL). Its tests skip themselves when no adapter is present.

A forensic tool has to work on whatever machine the evidence is examined on,
including a headless workstation or a VM with no GPU. Making GPU rendering a
requirement of the *analysis* path would be wrong.

**Decision:** `tpt-visual` is not a dependency of the analysis path. Frame
statistics that could otherwise come from a GPU colour pipeline are computed on
the CPU from decoded frames. `tpt-visual` may later be used for *rendering*
evidence (thumbnails, difference views in the UI), where GPU absence is a
degraded-display problem rather than a lost-finding problem.

## What is integrated

| Crate | Purpose in this project |
|---|---|
| `tpt-kinetix-core` | `Packet`, `Timestamp`, `CodecId`, `MediaType` |
| `tpt-kinetix-demux` | MP4/MKV box parsing; the container layer (spec §12, §13) |

The container crate maps Kinetix tracks into this engine's `StreamAnalysis`
model and owns two decisions the demuxer does not:

- **Format detection is by signature, not by extension.** An extension is a
  claim; the leading bytes are evidence.
- **Whole-file loading is capped.** `Mp4Demuxer::new` takes `Vec<u8>`, which
  departs from the streaming requirement in spec §55. `MAX_INSPECTED_BYTES`
  (2 GiB) refuses larger files with an explicit error rather than allowing an
  allocation failure to become a crash.

## Not yet integrated

`tpt-av-cadence-*` (audio decode), `tpt-kinetix-h264` (video decode),
`tpt-av-test-*` (conformance/fuzz harness). They are declared and verified to
resolve, but no crate depends on them yet.

`cargo generate --git https://github.com/tpt-solutions/tpt-av-test templates/consumer-crate`
scaffolds the `tpt-av-test` dev-dependency wiring, which is how that harness is
meant to be consumed. Note that a plain `cargo fetch` of that repository fails
because its `templates/consumer-crate` workspace member contains unresolved
`{{project-name}}` placeholders; the `path` subdirectory form used in
`Cargo.toml` avoids it.