//! Metadata consistency cross-checks (spec §26).
//!
//! # Observation, not accusation
//!
//! Spec §26 is explicit: *"Flag inconsistencies without asserting their
//! cause."*
//!
//! A container creation time two days earlier than a track creation time might
//! be a multi-pass encode, a transcoder that rewrote one box, a device with a
//! wrong clock, or tampering. The engine says which values disagree and by
//! how much. It does not say why, and this module has no vocabulary for
//! "why" at all — a deliberate constraint, because the temptation to write
//! "the file was edited" is exactly the failure the spec is guarding against.
//!
//! # What counts as a conflict
//!
//! Only *the same key* appearing in *different scopes* with *different
//! values*. Different keys are not compared: `encoder` and `creation_time` are
//! unrelated facts, not competing versions of one fact.

use serde::{Deserialize, Serialize};

use crate::tree::{MetadataEntry, MetadataTree, Scope};

/// The kind of disagreement found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConflictKind {
    /// The same key holds different values in different scopes.
    ConflictingValues,
    /// One scope carries a key that another scope also carries, but with an
    /// unparseable value, so agreement could not be established.
    UnparseableValue,
}

/// One inconsistency between metadata entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// The key whose values disagree.
    pub key: String,
    /// What sort of disagreement this is.
    pub kind: ConflictKind,
    /// Each distinct value with the scope it came from, in deterministic order.
    pub observations: Vec<Observation>,
}

impl Conflict {
    /// Renders a one-line summary, stating the disagreement without a cause.
    #[must_use]
    pub fn describe(&self) -> String {
        let parts: Vec<String> = self
            .observations
            .iter()
            .map(|o| format!("{}={} ({})", o.scope.tag(), o.value, o.source))
            .collect();
        format!("{}: {}", self.key, parts.join(" vs "))
    }
}

/// One value observed for a conflicting key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Where the value was found.
    pub scope: Scope,
    /// Index of the track, when track-scoped.
    pub track_index: Option<u32>,
    /// The value.
    pub value: String,
    /// The container element it came from.
    pub source: String,
}

impl Observation {
    fn from_entry(entry: &MetadataEntry) -> Self {
        Self {
            scope: entry.scope,
            track_index: entry.track_index,
            value: entry.value.clone(),
            source: entry.source.clone(),
        }
    }
}

/// Finds keys whose values disagree between scopes.
///
/// Keys appearing in only one scope, or with the same value everywhere, are not
/// conflicts. Ordering is by key then by scope, so output is reproducible
/// (spec §77).
#[must_use]
pub fn find_conflicts(tree: &MetadataTree) -> Vec<Conflict> {
    let mut keys: Vec<&str> = tree.entries.iter().map(|e| e.key.as_str()).collect();
    keys.sort_unstable();
    keys.dedup();

    let mut conflicts = Vec::new();

    for key in keys {
        let entries = tree.find(key);
        if entries.len() < 2 {
            continue;
        }

        // Group by scope: a conflict is disagreement *between* scopes, not two
        // tracks within the same scope disagreeing about their own data.
        let mut scopes: Vec<Scope> = entries.iter().map(|e| e.scope).collect();
        scopes.sort_unstable();
        scopes.dedup();
        if scopes.len() < 2 {
            continue;
        }

        let distinct: Vec<&str> = {
            let mut values: Vec<&str> = entries.iter().map(|e| e.value.as_str()).collect();
            values.sort_unstable();
            values.dedup();
            values
        };

        if distinct.len() > 1 {
            let mut observations: Vec<Observation> =
                entries.iter().map(|e| Observation::from_entry(e)).collect();
            observations.sort_by(|a, b| {
                a.scope
                    .cmp(&b.scope)
                    .then_with(|| a.track_index.cmp(&b.track_index))
                    .then_with(|| a.source.cmp(&b.source))
            });
            conflicts.push(Conflict {
                key: key.to_owned(),
                kind: ConflictKind::ConflictingValues,
                observations,
            });
        }
    }

    conflicts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: Vec<MetadataEntry>) -> MetadataTree {
        MetadataTree::new(entries)
    }

    #[test]
    fn agreeing_values_across_scopes_are_not_a_conflict() {
        let t = tree(vec![
            MetadataEntry::container("creation_time", "2026-08-01", "mvhd"),
            MetadataEntry::track(0, "creation_time", "2026-08-01", "tkhd"),
        ]);
        assert!(find_conflicts(&t).is_empty());
    }

    #[test]
    fn disagreeing_values_across_scopes_are_reported() {
        // The spec's own example.
        let t = tree(vec![
            MetadataEntry::container("creation_time", "2026-08-01", "mvhd"),
            MetadataEntry::track(0, "creation_time", "2026-08-03", "tkhd"),
        ]);

        let conflicts = find_conflicts(&t);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, "creation_time");
        assert_eq!(conflicts[0].kind, ConflictKind::ConflictingValues);
        assert_eq!(conflicts[0].observations.len(), 2);
    }

    #[test]
    fn two_tracks_disagreeing_within_one_scope_is_not_a_conflict() {
        // Track 0 and track 1 having different creation times is normal; they
        // are not competing versions of one fact.
        let t = tree(vec![
            MetadataEntry::track(0, "creation_time", "2026-08-01", "tkhd"),
            MetadataEntry::track(1, "creation_time", "2026-08-02", "tkhd"),
        ]);
        assert!(find_conflicts(&t).is_empty());
    }

    #[test]
    fn a_key_in_only_one_scope_is_never_a_conflict() {
        let t = tree(vec![
            MetadataEntry::container("encoder", "Lavf60", "moov"),
            MetadataEntry::track(0, "language", "und", "mdhd"),
        ]);
        assert!(find_conflicts(&t).is_empty());
    }

    #[test]
    fn the_description_states_the_disagreement_and_not_a_cause() {
        let t = tree(vec![
            MetadataEntry::container("creation_time", "2026-08-01", "mvhd"),
            MetadataEntry::track(0, "creation_time", "2026-08-03", "tkhd"),
        ]);
        let text = find_conflicts(&t)[0].describe();

        assert!(text.contains("2026-08-01"));
        assert!(text.contains("2026-08-03"));
        assert!(text.contains("mvhd"));
        // The engine must not speculate about why.
        for forbidden in ["tamper", "edit", "because", "forged", "manipulat"] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "a conflict must not assert a cause: {text}"
            );
        }
    }

    #[test]
    fn multiple_conflicts_are_reported_in_key_order() {
        let t = tree(vec![
            MetadataEntry::container("encoder", "Lavf60", "moov"),
            MetadataEntry::track(0, "encoder", "Lavf58", "tkhd"),
            MetadataEntry::container("creation_time", "2026-08-01", "mvhd"),
            MetadataEntry::track(0, "creation_time", "2026-08-03", "tkhd"),
        ]);

        let conflicts = find_conflicts(&t);
        assert_eq!(conflicts.len(), 2);
        assert_eq!(conflicts[0].key, "creation_time", "sorted by key");
        assert_eq!(conflicts[1].key, "encoder");
    }

    #[test]
    fn observations_are_in_deterministic_order() {
        let t = tree(vec![
            MetadataEntry::track(1, "encoder", "b", "tkhd"),
            MetadataEntry::container("encoder", "a", "moov"),
            MetadataEntry::track(0, "encoder", "c", "tkhd"),
        ]);
        let a = find_conflicts(&t);
        let b = find_conflicts(&t);
        assert_eq!(a, b, "spec §77 requires identical output across runs");

        let scopes: Vec<Scope> = a[0].observations.iter().map(|o| o.scope).collect();
        let mut sorted = scopes.clone();
        sorted.sort();
        assert_eq!(scopes, sorted, "observations are ordered by scope");
    }

    #[test]
    fn an_empty_tree_has_no_conflicts() {
        assert!(find_conflicts(&MetadataTree::default()).is_empty());
    }

    #[test]
    fn three_way_disagreement_lists_every_value() {
        let t = tree(vec![
            MetadataEntry::container("encoder", "a", "moov"),
            MetadataEntry::track(0, "encoder", "b", "tkhd"),
            MetadataEntry::codec(0, "encoder", "c", "avcC"),
        ]);
        let conflicts = find_conflicts(&t);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].observations.len(), 3);
    }
}
