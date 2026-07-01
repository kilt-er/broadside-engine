//! Live-boss #214 regression: every capital in the shipped
//! `broadside.catalog.json` must carry `footprint: "pair"` so
//! `App::build_current_board` in the bin lifts the encounter's boss
//! through `place_capital_pair` and gives it (a) the 2× hull render and
//! (b) the dual-cell hittable/blocking mechanics the resolver already
//! supports. When a boss ships with `footprint: "single"` (the serde
//! default), the bin's spawn closure never calls `place_capital_pair`
//! and the boss renders as a plain 1-cell enemy — Bruce's live report
//! ("no material difference in the size of the boss ship" via
//! `BROADSIDE_START_AT_BOSS=1`).
//!
//! Pins the actual asset the live campaign reads — not a test catalog —
//! so a future edit to the JSON that drops `footprint: "pair"` on any
//! authored capital fails here.

use std::path::Path;

use broadside_engine::catalog::load_from_path;
use broadside_engine::runs::capital_footprint;
use broadside_engine::types::Footprint;

const CATALOG_PATH: &str = "assets/broadside.catalog.json";

#[test]
fn every_capital_in_the_shipped_catalog_is_a_pair_boss() {
    assert!(
        Path::new(CATALOG_PATH).exists(),
        "{CATALOG_PATH} must be checked into the repo",
    );
    let cat = load_from_path(CATALOG_PATH).expect("catalog must parse");
    assert!(
        !cat.capitals.is_empty(),
        "catalog must ship at least one capital"
    );

    let non_pair: Vec<&str> = cat
        .capitals
        .iter()
        .filter(|c| c.footprint != Footprint::Pair)
        .map(|c| c.name.as_str())
        .collect();

    assert!(
        non_pair.is_empty(),
        "every capital must ship with `footprint: \"pair\"` so the live \
         campaign boss spawn goes through place_capital_pair; the \
         following capitals are still Single: {non_pair:?}",
    );
}

#[test]
fn capital_footprint_lookup_matches_the_shipped_asset_for_every_capital() {
    // Also route through the SAME lookup helper the bin uses
    // (`capital_footprint(class_id, cat)` at bin/broadside.rs:1825). If a
    // capital's name in `enc.enemy_ships[].class_id` ever drifts from the
    // catalog's `name` field (case-sensitive match, see runs.rs:541), the
    // lookup returns Footprint::Single by default and the bin silently
    // skips the pair placement — same failure Bruce saw.
    let cat = load_from_path(CATALOG_PATH).expect("catalog must parse");
    for capital in &cat.capitals {
        let looked_up = capital_footprint(&capital.name, &cat);
        assert_eq!(
            looked_up,
            Footprint::Pair,
            "capital_footprint(\"{}\") must resolve to Pair — is the \
             `name` field the same the spawn-gen writes into \
             ShipSpawn.class_id?",
            capital.name,
        );
    }
}
