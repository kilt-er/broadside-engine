//! Variable-board-dim safety nets for the #199 de-hardcode. Active since
//! the architect's `to_index`/`from_index` width migration landed on
//! `origin/v2` at `17f7edb`.
//!
//! Three reviewer-a recommendations the engine MUST hold green once the
//! migration commit drops:
//!
//! 1. **3×3 invariant-A flat-index roundtrip.** Builds a 3×3 board and
//!    asserts every cell's `Pos::to_index_in(dims) ↔ from_index_in(dims)`
//!    roundtrips. This is the test the migration must pass on a non-5
//!    width: the existing 1-D-lane fixtures use `row = 0`, which makes
//!    `to_index == col` regardless of width (so they pass even on a
//!    still-hardcoded 5-wide `to_index`). Catching the migration bug
//!    requires `row > 0`.
//!
//! 2. **`Board::ship_at` on a non-5 board.** Seats a uniquely-identifiable
//!    ship at every cell of a 3×3 board, walks `Pos`s, asserts `ship_at`
//!    returns the right one. Catches a migration that fixes `to_index_in`
//!    but leaves `ship_at` (or any other Board accessor) reading the
//!    constant `COLS`.
//!
//! 3. **`BoardSnapshot` serde-default snapshot.** Deserialises a snapshot
//!    JSON with `cols` / `rows` OMITTED and confirms the loaded snapshot
//!    reads `cols = COLS` / `rows = ROWS` (the boot dims). Locks "old saves
//!    still load" — pre-migration saves don't carry cols/rows fields.
//!
//! ## Active since `17f7edb`
//!
//! Origin/v2 `17f7edb` ("Width migration: thread `board.dims()` through every
//! flat-index gameplay site (GATE)") landed both the foundation (`Board.cols`
//! / `.rows` + `Dims` carrier) and the width migration (`to_index`/
//! `from_index` rewired to `to_index_in(dims)` / `from_index_in(index,
//! dims)`, `Board::ship_at` reading `self.dims()`). All three tests below
//! exercise the live path and are expected to pass; a failure of #1 or #2
//! against any future commit is a real regression in the width-migration
//! contract.
//!
//! ## Architect API (origin/v2 17f7edb)
//!
//!   - Dims type: [`broadside_engine::grid::Dims`] `{ cols, rows }`, `Default`
//!     = `COLS × ROWS`, `Dims::new(cols, rows)` builder.
//!   - Board fields: `Board.cols` / `Board.rows` + `Board::dims() -> Dims`.
//!     No `with_dims` constructor — tests build a Board via a struct
//!     literal, matching the existing `tests/common::board_2d` pattern.
//!   - Index helpers: `Pos::to_index_in(self, dims) -> usize`,
//!     `Pos::from_index_in(index, dims) -> Option<Self>` (note: args order
//!     is `(index, dims)`, NOT `(dims, index)`; result is `Option`, not
//!     bare `Pos`).
//!   - Serde: `BoardSnapshot.cols` / `BoardSnapshot.rows` flat,
//!     `#[serde(default = "default_cols/rows")]` → `crate::grid::COLS`/`ROWS`.

use broadside_engine::grid::{Dims, Pos, COLS, ROWS};
use broadside_engine::types::{
    Arc, Board, BoardSnapshot, EventBus, Faction, Mount, Orientation, ShieldFace, ShieldProfile,
    Ship,
};
use std::collections::HashMap;

/// Bare hull marker — the test uses `hull` as a unique per-cell ID, so a
/// `ship_at(pos)` that returns the wrong cell's ship surfaces with the WRONG
/// hull value rather than a generic "not the ship I expected".
fn marker_ship(id: String, pos: Pos, hull: i32) -> Ship {
    Ship {
        id,
        faction: Faction::Enemy,
        cell: pos.to_index_in(Dims::new(3, 3)),
        pos,
        orientation: Orientation::Broadside,
        facing: broadside_engine::grid::Facing::Broadside(broadside_engine::grid::Axis::EastWest),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 12,
        locked_out: false,
        mounts: vec![Mount {
            id: "m0".to_string(),
            arc: Arc::Forward,
            weapon: String::new(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        shield_profile: ShieldProfile {
            bow: ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: ShieldFace {
                armour: 0,
                charge: 0,
            },
        },
        klass: None,
        tail: None,
    }
}

/// (1) 3×3 invariant-A flat-index roundtrip.
///
/// Validates the migration on a non-5 width. The bug this catches: a
/// `to_index` that still bakes the constant `COLS = 5` will produce
/// `(col, row=1) -> col + 5` on a 3×3 board, which is OUT OF RANGE for
/// `cells.len() == 9`. Every `Pos(col, row)` on a 3×3 must satisfy
/// `from_index_in(dims, to_index_in(dims, p)) == p`.
#[test]
fn pos_to_index_from_index_roundtrips_on_a_3x3_board() {
    let dims = Dims { cols: 3, rows: 3 };
    for row in 0..dims.rows {
        for col in 0..dims.cols {
            let p = Pos::new(col, row);
            let i = p.to_index_in(dims);
            assert!(
                i < dims.cols * dims.rows,
                "to_index_in produced out-of-range {i} for cell {p:?} on \
                 {dims:?} (max {max})",
                max = dims.cols * dims.rows - 1,
            );
            let q = Pos::from_index_in(i, dims)
                .expect("from_index_in returned None for an in-range index");
            assert_eq!(
                p, q,
                "to_index/from_index roundtrip failed: {p:?} -> {i} -> {q:?} \
                 on {dims:?}",
            );
        }
    }
}

/// (2) `Board::ship_at` returns the right ship on a 3×3.
///
/// Validates that the Board APIs (`ship_at`, the cells vec layout) actually
/// USE the dims-aware index — not just that the helpers exist. Seats a
/// distinct marker ship at every cell, walks `Pos`s, asserts `ship_at`
/// returns the SAME marker at the SAME `Pos`. Catches a migration that
/// updates `to_index_in` but leaves `ship_at` reading the constant `COLS`.
#[test]
fn board_ship_at_returns_seated_ship_on_a_3x3_board() {
    // Build a 3×3 board by struct literal (the architect's 17f7edb leaves
    // Board construction as direct field assignment — no `with_dims`
    // constructor). Seat a uniquely-identifiable ship (`hull = idx as i32`)
    // at every cell. If a future change to `Board::ship_at` (or any
    // accessor that goes through `to_index_in(self.dims())`) regresses to
    // the const-`COLS` index, the wrong hull will surface — much louder
    // than `Option::None`.
    let dims = Dims::new(3, 3);
    let cell_count = dims.cell_count();
    let mut cells: Vec<Option<Ship>> = (0..cell_count).map(|_| None).collect();
    for row in 0..dims.rows {
        for col in 0..dims.cols {
            let p = Pos::new(col, row);
            let idx = p.to_index_in(dims);
            let s = marker_ship(format!("m{idx}"), p, idx as i32);
            cells[idx] = Some(s);
        }
    }
    let board = Board {
        size: dims.cell_count(),
        cols: dims.cols,
        rows: dims.rows,
        cells,
        ordnance: Vec::new(),
        hazards: (0..cell_count).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    };
    for row in 0..dims.rows {
        for col in 0..dims.cols {
            let p = Pos::new(col, row);
            let expected = p.to_index_in(dims) as i32;
            let actual = board
                .ship_at(p)
                .unwrap_or_else(|| panic!("board.ship_at({p:?}) returned None on a 3x3 board"))
                .hull;
            assert_eq!(
                actual, expected,
                "Board::ship_at on a 3x3 board returned the wrong ship at \
                 {p:?}: expected hull={expected} (the marker seated there), \
                 got hull={actual} — the migration left a flat-index accessor \
                 reading the legacy const COLS",
            );
        }
    }
}

/// (3) `BoardSnapshot` with `cols` / `rows` OMITTED deserialises to boot dims.
///
/// Locks "old saves still load" after the migration. Pre-migration JSON
/// files don't have cols/rows fields; the migration ships
/// `BoardSnapshot.cols` / `.rows` with `#[serde(default)]` → `COLS` / `ROWS`,
/// so a pre-v2 save (missing those fields) deserialises to the boot dims.
/// Without the serde default every pre-v2 save would fail to deserialise.
#[test]
fn board_snapshot_omitted_cols_rows_default_to_boot_dims() {
    // 5×4 = 20 null cells (matches the boot CELLS = COLS * ROWS).
    const JSON: &str = r#"{
        "size": 20,
        "cells": [null, null, null, null, null, null, null, null, null, null,
                  null, null, null, null, null, null, null, null, null, null],
        "ordnance": [],
        "hazards": [],
        "patrol": 0
    }"#;
    let snap: BoardSnapshot = serde_json::from_str(JSON)
        .expect("BoardSnapshot must accept JSON without cols/rows fields");
    assert_eq!(
        snap.cols, COLS,
        "missing-cols defaulted to {} (want {COLS})",
        snap.cols,
    );
    assert_eq!(
        snap.rows, ROWS,
        "missing-rows defaulted to {} (want {ROWS})",
        snap.rows,
    );
}
