//! Head-on swap collision (#torpedo-swap) — a torpedo advancing A→B and an
//! enemy simultaneously moving B→A on the same turn must COLLIDE, not
//! pass through each other. Bruce: "doesn't make sense" if they swap
//! cells without impact.
//!
//! The resolver's `advance_projectile_2d` steps every projectile ONE cell
//! at a time along its `heading8`. The standard occupancy check reads
//! `board.ship_at(new_pos)` — the CURRENT state. If an enemy that started
//! at `new_pos` has already vacated it (moved to the torpedo's previous
//! cell in the same logical turn), the destination is empty and the
//! torpedo would phase through without the swap-detection here.
//!
//! The fix (in `advance_projectile_2d`): when the standard occ check at
//! `new_pos` finds nothing, ALSO look at the projectile's PREVIOUS cell
//! (`cur`). If a hostile ship is now sitting there, they crossed — the
//! torpedo detonates on the enemy at its NEW position (`cur`, the cell
//! the torpedo just left). Mirrors the resolver's `Swap` detection in
//! `resolve_target_move_2d` for push/pull/swap.

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::grid::{Dims, Dir4, Dir8, Facing, Pos};
use broadside_engine::resolve::{advance_ordnance, advance_projectile_2d, Content};
use broadside_engine::types::{
    Action, Arc, Board, Effect, EventBus, Faction, LaneEnd, Mount, Orientation, Projectile, Ship,
};
use std::collections::HashMap;

/// Minimal `Content` — the tests build every action inline; no catalog
/// lookup, no projectile spawn.
struct NoContent;
impl Content for NoContent {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _kind: &str, _owner: &Ship) -> Projectile {
        panic!("torpedo_swap tests don't spawn ordnance via Content")
    }
}

/// A fresh 5×4 board with no ships / no ordnance / no threats.
fn empty_5x4_board() -> Board {
    let dims = Dims::default();
    let n = dims.cell_count();
    Board {
        size: n,
        cols: dims.cols,
        rows: dims.rows,
        cells: (0..n).map(|_| None).collect(),
        ordnance: Vec::new(),
        hazards: (0..n).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

/// A single-cell ship with naked shields (so the payload lands on hull
/// directly — makes the assertions arithmetic, not shield-dependent).
fn naked_ship(id: &str, faction: Faction, pos: Pos, hull: i32, facing: Facing) -> Ship {
    let dims = Dims::default();
    Ship {
        id: id.into(),
        faction,
        cell: pos.to_index_in(dims),
        pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
        facing,
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: broadside_engine::types::ShieldProfile {
            bow: broadside_engine::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            stern: broadside_engine::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: broadside_engine::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: broadside_engine::types::ShieldFace {
                armour: 0,
                charge: 0,
            },
        },
        mounts: vec![Mount {
            id: format!("{id}-m1"),
            arc: Arc::Forward,
            weapon: "_noop".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

/// A single-cell ship using the shared `default_shield_profile` (a bare
/// convenience for the player fixture where shields don't matter for the
/// assertion under test).
fn player_at(pos: Pos) -> Ship {
    let mut s = naked_ship("player", Faction::Player, pos, 10, Facing::Bow(Dir4::N));
    s.shield_profile = default_shield_profile();
    s
}

/// A player-owned torpedo `Projectile` pointing north (`Dir8::N`), speed 1,
/// hull 1, carrying a fixed-amount DAMAGE payload with band-falloff
/// disabled (so the payload lands raw regardless of range).
fn player_torpedo(pos: Pos, damage: i32) -> Projectile {
    let dims = Dims::default();
    Projectile {
        id: "t".into(),
        kind: "torpedo".into(),
        cell: pos.to_index_in(dims),
        pos,
        heading: LaneEnd::Fore,
        heading8: Dir8::N,
        speed: 1,
        hull: 1,
        payload: vec![Effect::DAMAGE {
            amount: damage,
            band_falloff: Some(false),
        }],
        owner_faction: Faction::Player,
    }
}

/* =========================================================================
 * (a) Head-on SWAP — the primary invariant Bruce called out.
 * ====================================================================== */

#[test]
fn head_on_swap_torpedo_hits_enemy_not_phase_through() {
    // Setup: torpedo at (2, 2) heading N, enemy at (2, 1) heading S. If the
    // enemy moves B→A (row 1 → row 2) in the same turn the torpedo advances
    // A→B (row 2 → row 1), a naive occupancy check on `new_pos` reads the
    // now-empty (2, 1) and phase-throughs. The swap-detection must see the
    // enemy at the torpedo's previous cell (2, 2) and route the impact there.
    let mut board = empty_5x4_board();
    let dims = board.dims();
    let torpedo_pos = Pos::new(2, 2);
    let enemy_pos = Pos::new(2, 1);
    // Seat the player + enemy.
    board.cells[Pos::new(2, 3).to_index_in(dims)] = Some(player_at(Pos::new(2, 3)));
    board.cells[enemy_pos.to_index_in(dims)] = Some(naked_ship(
        "enemy",
        Faction::Enemy,
        enemy_pos,
        3,
        Facing::Bow(Dir4::S),
    ));
    board.ordnance.push(player_torpedo(torpedo_pos, 2));

    // Simulate the enemy's move happening BEFORE the ordnance step — the
    // canonical worst-case for phase-through. Move the enemy from (2, 1) to
    // (2, 2), leaving (2, 1) empty. This is the exact state a bin's real-
    // time / decoupled loop would produce if enemy movement preceded the
    // ordnance advance in the same turn (per lead's audit ambiguity).
    let enemy = board.cells[enemy_pos.to_index_in(dims)].take().unwrap();
    let torpedo_prev_idx = torpedo_pos.to_index_in(dims);
    board.cells[torpedo_prev_idx] = Some(Ship {
        pos: torpedo_pos,
        cell: torpedo_prev_idx,
        ..enemy
    });

    // Advance the torpedo one step. Its heading is N, so it steps
    // (2, 2) → (2, 1). (2, 1) is now empty. Without swap-detection this
    // phase-throughs; with the fix the torpedo sees the enemy at its
    // previous cell (2, 2) and detonates there.
    advance_projectile_2d("t", &mut board, &NoContent);

    // The torpedo must be consumed (impact fired).
    assert!(
        board.ordnance.iter().all(|p| p.id != "t"),
        "torpedo consumed by the cross-swap impact"
    );
    // The enemy must have taken the payload damage (hull 3 - 2 = 1).
    let enemy_now = board.cells[torpedo_prev_idx]
        .as_ref()
        .expect("enemy still alive at its new position (2, 2)");
    assert_eq!(
        enemy_now.hull, 1,
        "enemy took the torpedo payload on the cross-swap"
    );
}

/* =========================================================================
 * (b) Standard head-on (no pre-vacate) — a control case that must ALSO
 *     collide via the normal occ check path. Guards against a refactor
 *     that accidentally makes the swap-detection the ONLY hit path.
 * ====================================================================== */

#[test]
fn head_on_no_prevacate_torpedo_hits_enemy_via_standard_occ_check() {
    let mut board = empty_5x4_board();
    let dims = board.dims();
    let torpedo_pos = Pos::new(2, 2);
    let enemy_pos = Pos::new(2, 1);
    board.cells[Pos::new(2, 3).to_index_in(dims)] = Some(player_at(Pos::new(2, 3)));
    board.cells[enemy_pos.to_index_in(dims)] = Some(naked_ship(
        "enemy",
        Faction::Enemy,
        enemy_pos,
        3,
        Facing::Bow(Dir4::S),
    ));
    board.ordnance.push(player_torpedo(torpedo_pos, 2));

    // Enemy has NOT moved. Torpedo advances (2, 2) → (2, 1). Occ check
    // finds the enemy directly — this is the pre-existing hit path.
    advance_projectile_2d("t", &mut board, &NoContent);

    assert!(
        board.ordnance.iter().all(|p| p.id != "t"),
        "torpedo consumed by the standard occ check impact"
    );
    let enemy_now = board.cells[enemy_pos.to_index_in(dims)]
        .as_ref()
        .expect("enemy still alive at (2, 1)");
    assert_eq!(enemy_now.hull, 1, "enemy took the payload directly");
}

/* =========================================================================
 * (c) NO ship in the way, no swap — torpedo advances without impact.
 *     Guards the swap-detection from firing on any random forward step.
 * ====================================================================== */

#[test]
fn torpedo_advances_freely_when_no_ship_crosses_path() {
    let mut board = empty_5x4_board();
    let dims = board.dims();
    board.cells[Pos::new(2, 3).to_index_in(dims)] = Some(player_at(Pos::new(2, 3)));
    let torpedo_pos = Pos::new(2, 2);
    board.ordnance.push(player_torpedo(torpedo_pos, 2));

    advance_projectile_2d("t", &mut board, &NoContent);

    // Torpedo advanced from (2, 2) to (2, 1) with no impact — still in
    // flight.
    let live = board
        .ordnance
        .iter()
        .find(|p| p.id == "t")
        .expect("torpedo still in flight");
    assert_eq!(
        live.pos,
        Pos::new(2, 1),
        "torpedo advanced one cell N without impact"
    );
}

/* =========================================================================
 * (d) OWNER at the previous cell must NOT trigger the swap-detection —
 *     an enemy torpedo passing over its own launcher (which stayed put)
 *     shouldn't detonate. Faction gate the same as the standard occ
 *     check.
 * ====================================================================== */

#[test]
fn owner_at_previous_cell_does_not_trigger_swap_impact() {
    let mut board = empty_5x4_board();
    let dims = board.dims();
    board.cells[Pos::new(2, 3).to_index_in(dims)] = Some(player_at(Pos::new(2, 3)));
    // A friendly (Player-faction) ship sitting at the torpedo's PREVIOUS
    // cell. Real-world case: the launcher didn't move; the torpedo just
    // left its cell going forward. The swap-detection must faction-gate
    // and NOT fire against the owner.
    let torpedo_pos = Pos::new(2, 2);
    board.cells[torpedo_pos.to_index_in(dims)] = Some(naked_ship(
        "launcher",
        Faction::Player,
        torpedo_pos,
        5,
        Facing::Bow(Dir4::N),
    ));
    board.ordnance.push(player_torpedo(torpedo_pos, 2));

    advance_projectile_2d("t", &mut board, &NoContent);

    // Torpedo still in flight; launcher untouched.
    let live = board
        .ordnance
        .iter()
        .find(|p| p.id == "t")
        .expect("torpedo not consumed by same-faction cell");
    assert_eq!(live.pos, Pos::new(2, 1));
    let launcher = board.cells[torpedo_pos.to_index_in(dims)]
        .as_ref()
        .expect("owner still on the board");
    assert_eq!(launcher.hull, 5, "owner took no damage");
}

/* =========================================================================
 * (e) End-to-end via `advance_ordnance` (the public phase seam
 *     `run_world_phase` composes) — same swap scenario as (a).
 * ====================================================================== */

#[test]
fn advance_ordnance_phase_seam_hits_on_cross_swap() {
    let mut board = empty_5x4_board();
    let dims = board.dims();
    let torpedo_pos = Pos::new(2, 2);
    let enemy_pos = Pos::new(2, 1);
    board.cells[Pos::new(2, 3).to_index_in(dims)] = Some(player_at(Pos::new(2, 3)));
    board.cells[enemy_pos.to_index_in(dims)] = Some(naked_ship(
        "enemy",
        Faction::Enemy,
        enemy_pos,
        3,
        Facing::Bow(Dir4::S),
    ));
    board.ordnance.push(player_torpedo(torpedo_pos, 2));

    // Pre-vacate (2, 1) and seat the enemy at (2, 2), the cell the
    // torpedo will leave. `advance_ordnance` should still route the
    // impact through the swap-detection.
    let enemy = board.cells[enemy_pos.to_index_in(dims)].take().unwrap();
    let torpedo_prev_idx = torpedo_pos.to_index_in(dims);
    board.cells[torpedo_prev_idx] = Some(Ship {
        pos: torpedo_pos,
        cell: torpedo_prev_idx,
        ..enemy
    });

    advance_ordnance(&mut board, &NoContent);

    assert!(
        board.ordnance.iter().all(|p| p.id != "t"),
        "advance_ordnance consumed the torpedo on cross-swap"
    );
    let enemy_now = board.cells[torpedo_prev_idx]
        .as_ref()
        .expect("enemy still present at its swapped-into position");
    assert_eq!(enemy_now.hull, 1);
}
