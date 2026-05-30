//! Integration coverage for the placeholder sections of [`Catalog`].
//!
//! Architect's inline test in `src/catalog.rs` (`loads_minimal_catalog`) uses
//! `mods: []`, `subsystems: []`, etc. with the keys PRESENT as empty arrays.
//! `#[serde(default)]` on `capitals`, `classes`, `fieldkit`, `sectors`, and
//! `commendations` claims another contract: those keys can be absent entirely
//! and still produce empty vecs. This file pins that contract down so a
//! future tightening (removing `#[serde(default)]` to require explicit
//! arrays, for example) shows up as a test failure rather than a silent
//! shape drift.
//!
//! Reference: `types.rs:651-661`.

use broadside_engine::catalog::load_from_bytes;

/// Catalog JSON missing every `#[serde(default)]` placeholder section: no
/// `capitals`, `classes`, `fieldkit`, `sectors`, or `commendations` keys.
/// The required sections (`meta`, `actions`, `mods`, `subsystems`,
/// `statuses`, `enemies`, `patrols`) are present but empty.
const CATALOG_WITH_PLACEHOLDERS_ABSENT: &str = r#"
{
  "meta": {
    "schema": "broadside.v0",
    "lane": [0,1,2,3,4,5,6],
    "newAxes": ["range","orientation","ordnance","heat"],
    "bands": ["pointBlank","close","mid","long","extreme"]
  },
  "actions": [],
  "mods": [],
  "subsystems": [],
  "statuses": [],
  "enemies": [],
  "patrols": [{ "n": 1, "mod": "baseline" }]
}
"#;

/// Same shape as above, but with the placeholder sections present as `[]`.
/// This is the variant `loads_minimal_catalog` already exercises, replicated
/// here so the absent / empty equivalence is asserted side-by-side rather
/// than across two files.
const CATALOG_WITH_PLACEHOLDERS_EMPTY: &str = r#"
{
  "meta": {
    "schema": "broadside.v0",
    "lane": [0,1,2,3,4,5,6],
    "newAxes": ["range","orientation","ordnance","heat"],
    "bands": ["pointBlank","close","mid","long","extreme"]
  },
  "actions": [],
  "mods": [],
  "subsystems": [],
  "statuses": [],
  "enemies": [],
  "capitals": [],
  "classes": [],
  "fieldkit": [],
  "sectors": [],
  "patrols": [{ "n": 1, "mod": "baseline" }],
  "commendations": []
}
"#;

#[test]
fn catalog_parses_with_placeholder_sections_absent() {
    let cat = load_from_bytes(CATALOG_WITH_PLACEHOLDERS_ABSENT.as_bytes())
        .expect("absent placeholder sections should default to empty vecs");
    assert!(cat.capitals.is_empty());
    assert!(cat.classes.is_empty());
    assert!(cat.fieldkit.is_empty());
    assert!(cat.sectors.is_empty());
    assert!(cat.commendations.is_empty());
}

#[test]
fn catalog_placeholders_absent_equals_placeholders_empty() {
    // Equivalence: a catalog with `capitals: []` is indistinguishable from
    // a catalog with no `capitals` key at all, as far as the in-memory
    // struct is concerned. This is the actual `#[serde(default)]` contract.
    let absent = load_from_bytes(CATALOG_WITH_PLACEHOLDERS_ABSENT.as_bytes()).unwrap();
    let empty = load_from_bytes(CATALOG_WITH_PLACEHOLDERS_EMPTY.as_bytes()).unwrap();
    assert_eq!(absent, empty);
}
