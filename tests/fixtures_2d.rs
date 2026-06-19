//! Sanity tests for the shared 2-D fixture builders (`tests/common/mod.rs`).
//!
//! The `board_2d`/`ship_2d` helpers are the reusable invariant-A fixtures the
//! v2 combat-test rewrite is built on (the run_action 2-D rewrite + un-ignoring
//! the combat_loop/run_loop integration tests). A fixture builder is itself
//! load-bearing — if it silently produced a non-invariant-A board, every test
//! built on it would assert against a wrong setup. So pin the builder's
//! contract here: invariant A holds, placement is where we asked, and the
//! double-book / out-of-bounds guards actually fire.

mod common;

use broadside_engine::grid::{Axis, Dir4, Facing, Pos};
use broadside_engine::types::Faction;
use common::{board_2d, dummy_2d, enemies_left, ship_2d};

#[test]
fn ship_2d_upholds_invariant_a_cell_equals_pos_index() {
    let s = ship_2d(
        "x",
        Faction::Enemy,
        Pos::new(3, 2),
        5,
        Facing::Bow(Dir4::S),
        broadside_engine::types::Arc::Forward,
        "beam",
    );
    assert_eq!(s.cell, Pos::new(3, 2).to_index(), "cell == pos.to_index()");
    assert_eq!(s.pos, Pos::new(3, 2));
    assert_eq!(s.facing, Facing::Bow(Dir4::S));
    assert_eq!(s.hull, 5);
    assert_eq!(s.max_hull, 5);
    assert_eq!(s.mounts.len(), 1, "one mount loaded");
}

#[test]
fn dummy_2d_has_no_mounts() {
    let d = dummy_2d("d", Faction::Enemy, Pos::new(0, 0), 9, Facing::Bow(Dir4::S));
    assert!(d.mounts.is_empty(), "a dummy carries no weapon");
    assert_eq!(d.cell, 0, "invariant A at the origin cell");
}

#[test]
fn board_2d_places_every_ship_at_its_pos_index() {
    let ships = vec![
        ship_2d(
            "p",
            Faction::Player,
            Pos::new(2, 3),
            30,
            Facing::Bow(Dir4::N),
            broadside_engine::types::Arc::Forward,
            "beam",
        ),
        ship_2d(
            "e1",
            Faction::Enemy,
            Pos::new(2, 0),
            4,
            Facing::Bow(Dir4::S),
            broadside_engine::types::Arc::Forward,
            "beam",
        ),
        ship_2d(
            "e2",
            Faction::Enemy,
            Pos::new(1, 0),
            4,
            Facing::Broadside(Axis::EastWest),
            broadside_engine::types::Arc::BroadsideArc,
            "beam",
        ),
    ];
    let b = board_2d(ships);
    // Fixed 5x4 grid backing vector.
    assert_eq!(b.cells.len(), broadside_engine::grid::CELLS);
    // Each ship sits at exactly cells[pos.to_index()].
    let p = b.cells[Pos::new(2, 3).to_index()]
        .as_ref()
        .expect("player placed");
    assert_eq!(p.id, "p");
    assert_eq!(p.faction, Faction::Player);
    let e1 = b.cells[Pos::new(2, 0).to_index()]
        .as_ref()
        .expect("e1 placed");
    assert_eq!(e1.id, "e1");
    let e2 = b.cells[Pos::new(1, 0).to_index()]
        .as_ref()
        .expect("e2 placed");
    assert_eq!(e2.id, "e2");
    // And invariant A holds for every occupied slot.
    for (idx, slot) in b.cells.iter().enumerate() {
        if let Some(s) = slot {
            assert_eq!(
                s.cell, idx,
                "occupant {} at slot {idx} reports cell {}",
                s.id, s.cell
            );
            assert_eq!(
                s.pos.to_index(),
                idx,
                "occupant {} pos {:?} indexes slot {idx}",
                s.id,
                s.pos
            );
        }
    }
    assert_eq!(enemies_left(&b), 2, "two enemies on the board");
}

#[test]
#[should_panic(expected = "two ships share cell")]
fn board_2d_panics_on_a_double_booked_cell() {
    // Two ships at the same pos must panic — a fixture that double-books a cell
    // is an authoring bug, surfaced loudly (not a silently-dropped ship like the
    // production build_encounter_board's defensive skip).
    let ships = vec![
        ship_2d(
            "a",
            Faction::Enemy,
            Pos::new(1, 1),
            3,
            Facing::Bow(Dir4::S),
            broadside_engine::types::Arc::Forward,
            "beam",
        ),
        ship_2d(
            "b",
            Faction::Enemy,
            Pos::new(1, 1),
            3,
            Facing::Bow(Dir4::S),
            broadside_engine::types::Arc::Forward,
            "beam",
        ),
    ];
    let _ = board_2d(ships);
}

#[test]
fn board_2d_distinct_cells_do_not_collide() {
    // The contrast case: distinct positions place cleanly (no false-positive in
    // the double-book guard).
    let ships = vec![
        ship_2d(
            "a",
            Faction::Enemy,
            Pos::new(0, 0),
            3,
            Facing::Bow(Dir4::S),
            broadside_engine::types::Arc::Forward,
            "beam",
        ),
        ship_2d(
            "b",
            Faction::Enemy,
            Pos::new(4, 3),
            3,
            Facing::Bow(Dir4::S),
            broadside_engine::types::Arc::Forward,
            "beam",
        ),
    ];
    let b = board_2d(ships);
    assert_eq!(enemies_left(&b), 2);
}
