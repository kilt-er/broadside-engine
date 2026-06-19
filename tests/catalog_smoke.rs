//! Regression net against catalog drift.
//!
//! When the design-doc-exported JSON is dropped into
//! `assets/broadside.catalog.json`, this test loads it through the public
//! API and pins down a few cheap structural invariants:
//!
//! - the file parses (every required section is present and well-typed)
//! - every `action.id` is unique
//! - every `subsystem.id` is unique
//! - every `mod.id` is unique
//! - every `action.r#mod`, if present, refers to a real `mod.id`
//!
//! These are the kinds of typos that don't surface until a player presses a
//! button on the broken action — easier to catch at `cargo test`.
//!
//! ## History
//!
//! Held local with `#[ignore]` from initial landing (commit f4901aa) until
//! the canonical catalog asset landed via content's transformer (#73,
//! `dcd232a`). Un-ignored at that point — the asset is now committed and
//! the test runs in the regular `cargo test` suite.

use std::collections::HashSet;
use std::path::Path;

use broadside_engine::catalog::load_from_path;

const CATALOG_PATH: &str = "assets/broadside.catalog.json";

#[test]
fn catalog_asset_loads_and_ids_are_unique() {
    // Defensive guard: if the asset ever goes missing again, produce a
    // clear failure rather than an opaque IO panic deep in serde_json.
    assert!(
        Path::new(CATALOG_PATH).exists(),
        "{CATALOG_PATH} must be checked into the repo; if it's gone, the \
         catalog transformer (#73) or its output got dropped",
    );

    let cat = load_from_path(CATALOG_PATH).expect("catalog must parse");

    // Schema sanity: the design doc's exporter sets a known schema string.
    // Don't pin the version (it will evolve) — just check the field exists.
    assert!(
        !cat.meta.schema.is_empty(),
        "catalog meta.schema must not be empty"
    );

    // Unique action ids.
    let mut ids = HashSet::new();
    for a in &cat.actions {
        assert!(
            ids.insert(a.id.as_str()),
            "duplicate action id in catalog: {}",
            a.id,
        );
    }

    // Unique subsystem ids.
    let mut sids = HashSet::new();
    for s in &cat.subsystems {
        assert!(
            sids.insert(s.id.as_str()),
            "duplicate subsystem id in catalog: {}",
            s.id,
        );
    }

    // Unique mod ids.
    let mut mids = HashSet::new();
    for m in &cat.mods {
        assert!(
            mids.insert(m.id.as_str()),
            "duplicate mod id in catalog: {}",
            m.id,
        );
    }

    // Every `action.r#mod`, if present, must resolve to a known mod id.
    // This is the classic "typo in a YAML reference" failure mode.
    for a in &cat.actions {
        if let Some(mod_id) = &a.r#mod {
            assert!(
                mids.contains(mod_id.as_str()),
                "action {} references unknown mod {}",
                a.id,
                mod_id,
            );
        }
    }

    // Unique class ids.
    let mut cids = HashSet::new();
    for cl in &cat.classes {
        assert!(
            cids.insert(cl.id.as_str()),
            "duplicate class id in catalog: {}",
            cl.id,
        );
    }

    // Every patrol tier must have a non-empty mod string. The canonical
    // catalog has one PatrolDef per tier 1..=7; checking the count is
    // brittle (the design has shifted tiers before) so we just assert
    // the data shape.
    for p in &cat.patrols {
        assert!(
            !p.r#mod.is_empty(),
            "patrol tier {} has empty mod field",
            p.n
        );
    }
}

/// Every action id referenced by a class's `set1` / `set2` / `signature`
/// must resolve to a real action id in `catalog.actions`. Catches
/// display-name vs action-id drift (the classic "Pulse Laser" string
/// where "pulse_laser" was meant).
///
/// Task #82 normalized set1/set2 references to action ids. Task #84
/// added the five class-signature Action records (slip/ram/phase/throw/
/// swap_toss) to canonical `actions[]`, normalized the `signature`
/// fields in `classes[]` to the matching snake_case ids, and extended
/// the canonical transformer's id-keyword inference for slip/phase/
/// swap_toss so the inflated Effects use the right `mode`. With both
/// landed, this regression test runs in the regular suite.
#[test]
fn class_loadout_action_ids_all_resolve() {
    assert!(Path::new(CATALOG_PATH).exists());
    let cat = load_from_path(CATALOG_PATH).expect("catalog must parse");

    let action_ids: HashSet<&str> = cat.actions.iter().map(|a| a.id.as_str()).collect();

    for cl in &cat.classes {
        for action_ref in cl.set1.iter().chain(cl.set2.iter()) {
            assert!(
                action_ids.contains(action_ref.as_str()),
                "class {} references unknown action in loadout: {action_ref:?}",
                cl.id,
            );
        }
        assert!(
            action_ids.contains(cl.signature.as_str()),
            "class {} signature {:?} does not resolve to an action id",
            cl.id,
            cl.signature,
        );
    }
}
