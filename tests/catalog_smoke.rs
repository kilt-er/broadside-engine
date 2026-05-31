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
//! ## Why `#[ignore]`
//!
//! The catalog asset isn't checked in yet. Until it lands, running this
//! test on a fresh clone would fail with "file not found" — noise, not a
//! real regression. `#[ignore]` keeps the test out of the default `cargo
//! test` run; whoever adds the catalog flips this attribute off.
//!
//! Run explicitly with:
//!
//! ```sh
//! cargo test --test catalog_smoke -- --ignored
//! ```

use std::collections::HashSet;
use std::path::Path;

use broadside_engine::catalog::load_from_path;

const CATALOG_PATH: &str = "assets/broadside.catalog.json";

#[test]
#[ignore = "catalog asset not yet checked in; un-ignore once assets/broadside.catalog.json lands"]
fn catalog_asset_loads_and_ids_are_unique() {
    // Defensive guard so a stray `--include-ignored` run on a fresh clone
    // produces a clear skip message rather than an opaque IO panic.
    if !Path::new(CATALOG_PATH).exists() {
        eprintln!(
            "skipping: {CATALOG_PATH} not present. Drop the design-doc JSON \
             export there and re-run with `--ignored`.",
        );
        return;
    }

    let cat = load_from_path(CATALOG_PATH).expect("catalog must parse");

    // Schema sanity: the design doc's exporter sets a known schema string.
    // Don't pin the version (it will evolve) — just check the field exists.
    assert!(!cat.meta.schema.is_empty(), "catalog meta.schema must not be empty");

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
}
