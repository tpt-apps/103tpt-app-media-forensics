//! Strongly typed identifiers for forensic entities.
//!
//! Each entity kind gets a distinct [`EntityId`] newtype so that an `AssetId`
//! can never be passed where a `CaseId` is expected — a real source of
//! cross-wiring bugs once a case holds thousands of assets and findings.
//!
//! # Determinism
//!
//! Identifiers are *content-derived*, not random. Given the same acquisition
//! record, the same case name, and the same analysis fingerprint, the engine
//! must produce byte-identical output across runs and machines (spec §77).
//! Random identifiers would make reports non-reproducible and would defeat
//! cache keying (spec §54).

use core::fmt;

use serde::{Deserialize, Serialize};

/// The 128-bit value backing every [`EntityId`].
pub type IdValue = u128;

/// The entity kind an [`EntityId`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum EntityKind {
    /// A forensic case (spec §9).
    Case = 0x01,
    /// A media asset within a case (spec §10).
    Asset = 0x02,
    /// A single run of the analysis engine.
    Analysis = 0x03,
    /// A rule observation (spec §34).
    Finding = 0x04,
    /// A derived artefact retained for examination (spec §32).
    Evidence = 0x05,
    /// A generated report (spec §59).
    Report = 0x06,
    /// A stream inside a container (spec §13).
    Stream = 0x07,
}

impl EntityKind {
    /// Returns the lowercase ASCII tag mixed into derived identifiers.
    ///
    /// This string is part of the derivation contract: changing it changes
    /// every derived ID, so it must remain stable across releases.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Case => "case",
            Self::Asset => "asset",
            Self::Analysis => "analysis",
            Self::Finding => "finding",
            Self::Evidence => "evidence",
            Self::Report => "report",
            Self::Stream => "stream",
        }
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// A 128-bit, strongly typed identifier.
///
/// Implements [`Ord`] so that sorting by ID is stable and reproducible when
/// rendering findings and evidence lists.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntityId {
    kind: EntityKind,
    value: IdValue,
}

impl EntityId {
    /// Constructs an identifier from a raw 128-bit value.
    ///
    /// The value is forced into the version-4/variant-1 layout
    /// ([RFC 4122](https://www.rfc-editor.org/rfc/rfc4122)) so that every ID
    /// has an identical shape, keeping rendering, sorting, and storage layout
    /// stable regardless of how the value was derived.
    #[must_use]
    pub const fn from_value(kind: EntityKind, value: IdValue) -> Self {
        // Clear the version nibble and set it to 4 purely as a shape
        // convention, then set the two variant bits to RFC 4122's `10`.
        let cleared = value & !(0xF000u128 << 64);
        let versioned = cleared | (0x4u128 << 112);
        let variant = versioned & !(0xC000u128 << 48) | (0x8000u128 << 48);
        Self {
            kind,
            value: variant,
        }
    }

    /// Derives a stable identifier from a kind and a list of field values.
    ///
    /// Uses a 128-bit FNV-1a over the length-prefixed concatenation of the
    /// entity kind tag and each field, so that `derive("ab", "c")` and
    /// `derive("a", "bc")` produce different IDs.
    ///
    /// This is a deterministic, non-cryptographic hash: it makes results
    /// reproducible, but it is not a claim of uniqueness and must not be used
    /// for security purposes. Integrity always rests on
    /// [`EvidenceIntegrity`](crate::evidence::EvidenceIntegrity) and the
    /// content hashes recorded at acquisition.
    #[must_use]
    pub fn new_derived<F: AsRef<[u8]>>(kind: EntityKind, fields: &[F]) -> Self {
        let mut hasher = Fnv1a128::new();
        hasher.write(kind.tag().as_bytes());
        for field in fields {
            let bytes = field.as_ref();
            // Length-prefix so concatenations cannot collide by reordering.
            hasher.write(&(bytes.len() as u64).to_be_bytes());
            hasher.write(bytes);
        }
        Self::from_value(kind, hasher.finish())
    }

    /// Returns the entity kind this identifier refers to.
    #[must_use]
    pub const fn kind(self) -> EntityKind {
        self.kind
    }

    /// Returns the raw 128-bit value.
    #[must_use]
    pub const fn value(self) -> IdValue {
        self.value
    }

    /// Parses an identifier from its canonical hyphenated form.
    ///
    /// The inverse of [`Self::to_canonical_string`], used when reading a case
    /// manifest written by an earlier run. Returns [`None`] for malformed
    /// input rather than panicking, since manifests are external data.
    #[must_use]
    pub fn from_canonical_str(kind: EntityKind, canonical: &str) -> Option<Self> {
        let hex = canonical.replace('-', "");
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut value: IdValue = 0;
        for byte in hex.bytes() {
            let digit = (byte as char).to_digit(16)? as IdValue;
            value = (value << 4) | digit;
        }
        Some(Self::from_value(kind, value))
    }

    /// Returns the canonical hyphenated (UUID-style) text form.
    ///
    /// This is the representation written to reports and the case manifest
    /// (spec §58), so it must remain stable.
    #[must_use]
    pub fn to_canonical_string(self) -> String {
        format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            (self.value >> 96) as u32,
            (self.value >> 80) as u16,
            (self.value >> 64) as u16,
            (self.value >> 48) as u16,
            self.value & 0x0000_FFFF_FFFF_FFFF
        )
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.tag(), self.to_canonical_string())
    }
}

impl fmt::Debug for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntityId({self})")
    }
}
/// Declares a newtype wrapper around [`EntityId`] for a specific entity kind.
macro_rules! entity_id {
    ($(#[$meta:meta])* $name:ident, $kind:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(EntityId);

        impl $name {
            /// The entity kind of this identifier type.
            pub const KIND: EntityKind = $kind;

            /// Wraps a raw identifier value, asserting the kind matches.
            ///
            /// Returns `None` if `raw` belongs to a different entity kind,
            /// which catches bugs at deserialisation boundaries.
            #[must_use]
            pub const fn new(raw: EntityId) -> Option<Self> {
                if raw.kind() as u8 == $kind as u8 {
                    Some(Self(raw))
                } else {
                    None
                }
            }

            /// Derives a stable identifier for this entity kind.
            #[must_use]
            pub fn new_derived<F: AsRef<[u8]>>(fields: &[F]) -> Self {
                Self(EntityId::new_derived($kind, fields))
            }

            /// Returns the underlying untyped identifier.
            #[must_use]
            pub const fn raw(self) -> EntityId {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

entity_id!(
    /// Identifies a forensic case (spec §9).
    CaseId,
    EntityKind::Case
);
entity_id!(
    /// Identifies a media asset within a case (spec §10).
    AssetId,
    EntityKind::Asset
);
entity_id!(
    /// Identifies a single run of the analysis engine (spec §63).
    AnalysisId,
    EntityKind::Analysis
);
entity_id!(
    /// Identifies a rule observation (spec §34).
    FindingId,
    EntityKind::Finding
);
entity_id!(
    /// Identifies a retained evidence artefact (spec §32).
    EvidenceId,
    EntityKind::Evidence
);
entity_id!(
    /// Identifies a generated report (spec §59).
    ReportId,
    EntityKind::Report
);
entity_id!(
    /// Identifies a stream within a container (spec §13).
    ///
    /// Stream IDs are scoped to their owning asset; the analysis engine pairs
    /// them with the asset ID when recording results.
    StreamId,
    EntityKind::Stream
);

/// A minimal 128-bit FNV-1a hash.
///
/// Chosen over a heavier hasher because identifiers need stable, portable,
/// dependency-free derivation — not cryptographic strength.
struct Fnv1a128 {
    state: u128,
}

impl Fnv1a128 {
    /// FNV-1a 128-bit offset basis: `6c622e72fbb374e2fe1e7a7b0fbe8bec`.
    const OFFSET: u128 = 0x6c62_2e72_fbb3_74e2_fe1e_7a7b_0fbe_8bec;
    /// FNV-1a 128-bit prime: `2^88 + 2^8 + 0x3b`.
    const PRIME: u128 = 0x0100_0000_0000_0000_0000_013b;

    fn new() -> Self {
        Self {
            state: Self::OFFSET,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.state ^= u128::from(byte);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(self) -> u128 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_ids_are_deterministic() {
        let a = CaseId::new_derived(&["Case Alpha", "e3b0c44298fc1c14"]);
        let b = CaseId::new_derived(&["Case Alpha", "e3b0c44298fc1c14"]);
        assert_eq!(a, b, "identical inputs must yield identical IDs (spec §77)");
    }

    #[test]
    fn different_fields_yield_different_ids() {
        assert_ne!(
            AssetId::new_derived(&["a", "b"]),
            AssetId::new_derived(&["b", "a"])
        );
    }

    #[test]
    fn field_boundaries_prevent_concatenation_collisions() {
        // Without length-prefixing, "ab"+"c" and "a"+"bc" would hash alike.
        assert_ne!(
            FindingId::new_derived(&["ab", "c"]),
            FindingId::new_derived(&["a", "bc"])
        );
    }

    #[test]
    fn entity_kinds_are_namespaced() {
        let fields = ["shared", "fields"];
        assert_ne!(
            CaseId::new_derived(&fields).raw().value(),
            AssetId::new_derived(&fields).raw().value(),
            "identical field values must not collide across entity kinds"
        );
    }

    #[test]
    fn newtype_rejects_mismatched_kind() {
        let case = CaseId::new_derived(&["x"]);
        assert!(AssetId::new(case.raw()).is_none());
        assert!(CaseId::new(case.raw()).is_some());
    }

    #[test]
    fn canonical_string_is_stable_and_sized() {
        let text = CaseId::new_derived(&["stability"]).to_string();
        assert!(text.starts_with("case:"));
        assert_eq!(text.len(), "case:".len() + 36);
    }

    #[test]
    fn derived_value_forces_rfc4122_shape() {
        let id = EntityId::from_value(EntityKind::Asset, 0);
        assert_eq!((id.value() >> 112) & 0xF, 0x4, "version nibble");
        assert_eq!((id.value() >> 62) & 0b11, 0b10, "variant bits");
    }

    #[test]
    fn canonical_string_round_trips() {
        let id = CaseId::new_derived(&["round trip"]);
        let canonical = id.raw().to_canonical_string();
        let parsed = EntityId::from_canonical_str(EntityKind::Case, &canonical);
        assert_eq!(parsed, Some(id.raw()));
    }

    #[test]
    fn canonical_parsing_rejects_malformed_input() {
        // Manifests are external data; bad input must not panic.
        for bad in ["", "zzz", "case-not-hex", "---"] {
            assert_eq!(EntityId::from_canonical_str(EntityKind::Case, bad), None);
        }
    }

    #[test]
    fn ordering_is_total_and_stable() {
        let mut ids = vec![
            CaseId::new_derived(&["c"]),
            CaseId::new_derived(&["a"]),
            CaseId::new_derived(&["b"]),
        ];
        ids.sort();
        let mut reversed = ids.clone();
        reversed.reverse();
        reversed.sort();
        assert_eq!(ids, reversed, "sorting must be deterministic (spec §77)");
    }
}
