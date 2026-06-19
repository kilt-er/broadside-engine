//! Smoke test for the empty-skeleton catalog fixture at
//! `assets/broadside.catalog.example.json`.
//!
//! Distinct from `catalog_smoke.rs`, which targets the real (not-yet-shipped)
//! catalog export at `assets/broadside.catalog.json` and is `#[ignore]`d
//! pending bruce. This file's fixture is shipped in-repo and exercises the
//! serde shape of `Catalog` against a structural reference: every required
//! top-level key present, every array empty. It catches the kind of drift
//! where a schema-level field stops being optional or a casing rule changes
//! — failures here surface before the real catalog lands.

use broadside_engine::catalog::load_from_path;

const EXAMPLE_PATH: &str = "assets/broadside.catalog.example.json";

#[test]
fn empty_skeleton_catalog_parses() {
    let cat = load_from_path(EXAMPLE_PATH).expect("example skeleton must parse");

    // Meta is required and must be non-empty.
    assert_eq!(cat.meta.schema, "broadside.v0.example");
    assert_eq!(cat.meta.lane, vec![0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(
        cat.meta.bands.len(),
        5,
        "all five range bands must round-trip the camelCase mapping"
    );

    // All collection fields parse and are empty.
    assert!(cat.actions.is_empty());
    assert!(cat.mods.is_empty());
    assert!(cat.subsystems.is_empty());
    assert!(cat.statuses.is_empty());
    assert!(cat.enemies.is_empty());
    assert!(cat.capitals.is_empty());
    assert!(cat.classes.is_empty());
    assert!(cat.fieldkit.is_empty());
    assert!(cat.sectors.is_empty());
    assert!(cat.patrols.is_empty());
    assert!(cat.commendations.is_empty());
}
