//! Evidence storage (spec §32, §33).
//!
//! Writes derived artefacts into the case directory, recording hashes and
//! provenance for each. Every write is followed by a re-read and re-hash so
//! that stored evidence is verified rather than assumed.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
