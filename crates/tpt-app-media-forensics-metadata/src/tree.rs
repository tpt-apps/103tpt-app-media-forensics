//! Structured metadata model (spec §25).
//!
//! # Why a tree, and why each entry remembers where it came from
//!
//! Metadata in a container is scattered across boxes at three levels: the
//! container itself (`moov`), each track (`trak`), and each codec (`stsd`).
//! The same logical key — a creation time, an encoder string — can appear in
//! more than one place with a different value.
//!
//! Flattening that into a map would destroy the very information that makes
//! conflicts detectable. So every [`MetadataEntry`] retains its `scope` and
//! `source`, and duplicates are preserved rather than overwritten. Two
//! `creation_time` values are only interesting *because* they came from
//! different places.
//!
//! Ordering is deterministic: entries are stored in a `BTreeMap` keyed by
//! scope-then-key, so two runs over the same file produce identical trees
//! (spec §77).

use serde::{Deserialize, Serialize};

/// Which part of the container an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Container-level metadata, e.g. `moov`.
    Container,
    /// Per-track metadata, e.g. `trak`.
    Track,
    /// Per-codec / sample-description metadata.
    Codec,
}

impl Scope {
    /// Returns the stable lowercase tag used in reports.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Track => "track",
            Self::Codec => "codec",
        }
    }
}

/// One metadata value, with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MetadataEntry {
    /// Which part of the container this came from.
    pub scope: Scope,
    /// Index of the track this belongs to, for track and codec scopes.
    pub track_index: Option<u32>,
    /// The key as written in the container, e.g. `creation_time`.
    pub key: String,
    /// The value, as a string. Metadata is not reliably typed.
    pub value: String,
    /// The container element the value was read from, e.g. `mvhd`.
    pub source: String,
}

impl MetadataEntry {
    /// Builds a container-scoped entry.
    #[must_use]
    pub fn container(
        key: impl Into<String>,
        value: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            scope: Scope::Container,
            track_index: None,
            key: key.into(),
            value: value.into(),
            source: source.into(),
        }
    }

    /// Builds a track-scoped entry.
    #[must_use]
    pub fn track(
        track_index: u32,
        key: impl Into<String>,
        value: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            scope: Scope::Track,
            track_index: Some(track_index),
            key: key.into(),
            value: value.into(),
            source: source.into(),
        }
    }

    /// Builds a codec-scoped entry.
    #[must_use]
    pub fn codec(
        track_index: u32,
        key: impl Into<String>,
        value: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            scope: Scope::Codec,
            track_index: Some(track_index),
            key: key.into(),
            value: value.into(),
            source: source.into(),
        }
    }
}

/// A metadata tree for one asset.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MetadataTree {
    /// All entries, ordered by scope, track, and key.
    pub entries: Vec<MetadataEntry>,
}

impl MetadataTree {
    /// Builds a tree from entries, sorting them deterministically.
    #[must_use]
    pub fn new(mut entries: Vec<MetadataEntry>) -> Self {
        entries.sort();
        Self { entries }
    }

    /// Returns every entry with the given key, across all scopes.
    #[must_use]
    pub fn find(&self, key: &str) -> Vec<&MetadataEntry> {
        self.entries.iter().filter(|e| e.key == key).collect()
    }

    /// Returns entries for one scope.
    #[must_use]
    pub fn by_scope(&self, scope: Scope) -> Vec<&MetadataEntry> {
        self.entries.iter().filter(|e| e.scope == scope).collect()
    }

    /// Returns the number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no metadata was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_sorted_deterministically() {
        let a = MetadataEntry::container("creation_time", "2026-08-01", "mvhd");
        let b = MetadataEntry::track(0, "creation_time", "2026-08-03", "tkhd");
        let c = MetadataEntry::codec(0, "encoder", "x264", "avcC");

        let tree = MetadataTree::new(vec![c.clone(), b.clone(), a.clone()]);
        let keys: Vec<&str> = tree.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["creation_time", "creation_time", "encoder"]);
        assert_eq!(tree.entries[0].scope, Scope::Container);
    }

    #[test]
    fn same_key_in_two_scopes_is_kept_not_overwritten() {
        // This is the whole point of the tree: a conflict is only visible
        // because both values survive.
        let tree = MetadataTree::new(vec![
            MetadataEntry::container("creation_time", "2026-08-01", "mvhd"),
            MetadataEntry::track(0, "creation_time", "2026-08-03", "tkhd"),
        ]);

        assert_eq!(tree.find("creation_time").len(), 2);
        assert_eq!(tree.by_scope(Scope::Container).len(), 1);
        assert_eq!(tree.by_scope(Scope::Track).len(), 1);
    }

    #[test]
    fn provenance_is_retained_per_entry() {
        let tree = MetadataTree::new(vec![
            MetadataEntry::container("encoder", "Lavf60", "moov"),
            MetadataEntry::track(0, "encoder", "Lavf58", "tkhd"),
        ]);
        let sources: Vec<&str> = tree
            .find("encoder")
            .iter()
            .map(|e| e.source.as_str())
            .collect();
        assert!(sources.contains(&"moov"));
        assert!(sources.contains(&"tkhd"));
    }

    #[test]
    fn lookup_of_a_missing_key_is_empty_not_an_error() {
        let tree = MetadataTree::default();
        assert!(tree.is_empty());
        assert!(tree.find("nope").is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn tree_round_trips_through_json() {
        let tree = MetadataTree::new(vec![MetadataEntry::container("encoder", "x", "moov")]);
        let json = serde_json::to_string(&tree).expect("serialises");
        let back: MetadataTree = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(tree, back);
    }

    #[test]
    fn scope_tags_are_stable() {
        assert_eq!(Scope::Container.tag(), "container");
        assert_eq!(Scope::Codec.tag(), "codec");
    }
}
