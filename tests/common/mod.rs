//! Shared 2-D test fixtures (blueprint tester lane — the reusable
//! `board_2d`/`ship_2d` invariant-A builders).
//!
//! Built for the v2 combat migration: the legacy `ship()`/`spawn()` helpers in
//! `combat_loop.rs` / `run_loop.rs` pin `pos = Pos::new(0,0)` for every ship
//! (an A3.1 EXPAND transitional default), so once the firing path reads
//! `ship.pos` (R3) every ship stacks on grid cell `(0,0)` and no shot connects.
//! These builders place ships at **real** 2-D positions with **real** bearing
//! facings, upholding **invariant A** (`cells[pos.to_index()] == the ship` and
//! `ship.cell == pos.to_index()`), so a `Bow`/`Forward` shot actually bears in
//! the 2-D targeting model.
//!
//! Consumers: the 2-D-fixture rewrite of the `run_action` tests (resolver-owned
//! `resolve.rs` keeps an inline copy of this shape) and the un-ignoring of the
//! `combat_loop` / `run_loop` integration tests as the 2-D combat stack lands.
//!
//! This is a `tests/common/` submodule (NOT its own test binary, per Cargo
//! convention): integration test files pull it in with `mod common;`.

#![allow(dead_code)] // each integration test file uses only the subset it needs

use broadside_engine::grid::{Dir4, Facing, Pos, CELLS};
use broadside_engine::types::{
    Arc, Board, EventBus, Faction, Mount, Orientation, ShieldFace, ShieldProfile, Ship,
};
use std::collections::HashMap;

/// A frigate-grade hull: strong bow (2), weak stern (0), medium flanks (1) —
/// the canonical [`broadside_engine::geometry::default_shield_profile`] shape.
pub const fn frigate_shields() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace {
            armour: 2,
            charge: 0,
        },
        stern: ShieldFace {
            armour: 0,
            charge: 0,
        },
        port: ShieldFace {
            armour: 1,
            charge: 0,
        },
        starboard: ShieldFace {
            armour: 1,
            charge: 0,
        },
    }
}

/// A bare hull — no armour, no charge on any zone. Use when a test wants every
/// point of damage to land on hull (observable) rather than being soaked.
pub const fn naked_shields() -> ShieldProfile {
    ShieldProfile {
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
    }
}

/// The legacy 1-D [`Orientation`] consistent with a 2-D [`Facing`], for the
/// transition window (the resolver still mirrors both). `Bow(_)` → `BowOn` with
/// the matching cardinal mapped to a lane end (N/W → Aft side, S/E → Fore side
/// is arbitrary here — only the 2-D `facing` drives the live path); `Broadside`
/// → `Broadside`. Kept so a fixture's legacy field is never left contradictory.
const fn legacy_orientation_for(facing: Facing) -> Orientation {
    match facing {
        Facing::Bow(dir) => {
            // Map the cardinal to *some* lane end deterministically; the 2-D
            // path ignores this, and invariant-A consumers read `facing`.
            let bow_end = match dir {
                Dir4::S | Dir4::E => broadside_engine::types::LaneEnd::Fore,
                Dir4::N | Dir4::W => broadside_engine::types::LaneEnd::Aft,
            };
            Orientation::BowOn { bow: bow_end }
        }
        Facing::Broadside(_) => Orientation::Broadside,
    }
}

/// Build a [`Ship`] at a real 2-D `pos` with a real bearing `facing`, carrying
/// one `arc`-mount loaded with `weapon`. Upholds **invariant A**:
/// `ship.cell == pos.to_index()` (so 1-D and 2-D readers agree during the
/// transition; the 2-D firing path reads `pos`/`facing`).
///
/// Defaults: `heat_max` 12 (generous, no accidental lockout), `frigate_shields`,
/// empty queue/cooldowns/statuses/traits. Override fields on the returned Ship
/// for test-specific needs (e.g. `naked_shields`, `heat_max`, `traits`).
pub fn ship_2d(
    id: &str,
    faction: Faction,
    pos: Pos,
    hull: i32,
    facing: Facing,
    arc: Arc,
    weapon: &str,
) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell: pos.to_index(), // invariant A
        pos,
        orientation: legacy_orientation_for(facing),
        facing,
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 12,
        locked_out: false,
        shield_profile: frigate_shields(),
        mounts: vec![Mount {
            id: format!("{id}-m1"),
            arc,
            weapon: weapon.into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// A mountless [`Ship`] at `pos` (a pure target/dummy — no weapon, no arc).
pub fn dummy_2d(id: &str, faction: Faction, pos: Pos, hull: i32, facing: Facing) -> Ship {
    let mut s = ship_2d(id, faction, pos, hull, facing, Arc::Turret, "noop");
    s.mounts.clear();
    s
}

/// Build a [`Board`] over the fixed `CELLS`-length (5×4) grid (post-A3 Board
/// EXPAND), placing each ship at `cells[ship.pos.to_index()]` — **invariant A**.
///
/// Panics if two ships share a cell or a ship is out of bounds: a fixture that
/// double-books a cell is a test-authoring bug we want surfaced loudly, not a
/// silently-dropped ship.
pub fn board_2d(ships: Vec<Ship>) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..CELLS).map(|_| None).collect();
    let hazards: Vec<Vec<broadside_engine::types::Hazard>> =
        (0..CELLS).map(|_| Vec::new()).collect();
    for s in ships {
        assert!(
            s.pos.in_bounds(),
            "ship {} pos {:?} out of bounds",
            s.id,
            s.pos
        );
        let idx = s.pos.to_index();
        assert_eq!(
            s.cell, idx,
            "ship {} breaks invariant A (cell {} != pos.to_index() {idx})",
            s.id, s.cell
        );
        assert!(
            cells[idx].is_none(),
            "two ships share cell {idx} (pos {:?})",
            s.pos
        );
        cells[idx] = Some(s);
    }
    Board {
        size: broadside_engine::grid::COLS,
        cols: broadside_engine::grid::COLS,
        rows: broadside_engine::grid::ROWS,
        cells,
        ordnance: Vec::new(),
        hazards,
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

/// Count the live `Faction::Enemy` ships on a board.
pub fn enemies_left(b: &Board) -> usize {
    b.cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .count()
}
