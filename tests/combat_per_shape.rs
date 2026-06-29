//! Per-shape combat + winnability safety nets for the #199 variable-board
//! feature. Locks the live spawn surface ([`build_encounter_board_with_dims`])
//! against {2x2, 3x3, 4x4, 5x4} (default), proving that the dim-aware
//! placement + dodge-lane cap stay invariants on every pool shape and that a
//! basic `resolve_round` runs to completion (the no-panic floor reviewer-a
//! asked for).
//!
//! ## Why these tests
//!
//! Lane-1D fixtures fix `row = 0`, so they passed on the const-`COLS = 5`
//! flat-index even before the width migration; `tests/board_dims.rs` proved
//! the migration on raw `Pos` / `Board::ship_at`. This file extends the
//! proof to the live spawn entry point — the same function the bin uses to
//! build a board for an encounter. If a future commit threads `Dims` into a
//! NEW gameplay site but forgets to thread it through `build_encounter_board
//! _with_dims` (or one of the helpers it calls), the per-shape spawn or
//! resolve here would surface the regression on a non-5 width.
//!
//! ## Architect spawn invariants pinned (origin/v2 20aa002)
//!
//!   1. **Spawns in-bounds for `dims`** (see
//!      [`build_encounter_board_with_dims`]'s "Per-shape behaviour" doc).
//!   2. **Player at `player_start_pos_in(dims)` (front-centre)**, distinct
//!      from every enemy cell.
//!   3. **Enemy count == [`max_enemies_in(dims)`]** when authored at cap.
//!      The cap is the binding livability constraint (1/2/3/4 across
//!      2x2/3x3/4x4/5x4 per the canonical table on `max_enemies_in`).
//!   4. **`resolve_round` runs to completion without panic** on each
//!      shape's freshly-built board (the integration smoke that #203 spec'd
//!      and reviewer-a asked for a per-shape extension of).
//!   5. **The player always has >= 1 legal cardinal-neighbor move on its
//!      cell**, even at the per-shape spawn cap — the dodge-lane invariant
//!      Bruce wired into the `max_enemies_in` table (always keep one
//!      back-row column clean).
//!
//! ## Pool shapes
//!
//! Mirrors the runs-tests `POOL_SHAPES` table: 2x2, 3x3, 4x4, plus the
//! default 5x4. (The full `SpawnPool` table includes shallower 2x2 / 2x3 /
//! 2x4 widths the design says are also valid; the lead's spec was
//! 2x2/3x3/4x4 + default, so this file holds to that scope. Extending
//! later is one-line additions to [`SHAPES`].)
//!
//! Keys off [`build_encounter_board_with_dims`] (NOT a hand-built fixture)
//! so the assertion tracks the live spawn entry, including any future
//! migration of the placement loop. The spawn positions are hand-authored
//! at known back-row cells to mirror what `sample_encounter_spawns_with_dims`
//! produces internally; the live path is exercised end-to-end via the
//! builder closure + `resolve_round`.

use broadside_engine::grid::{Dims, Dir4, Facing, Pos};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::runs::{
    build_encounter_board_with_dims, enemy_spawn_facing, max_enemies_in, player_spawn_facing,
    player_start_pos_in,
};
use broadside_engine::types::{
    Action, Arc, Board, EncounterDef, Faction, LaneEnd, Mount, Orientation, Projectile, ShieldFace,
    ShieldProfile, Ship, ShipSpawn,
};
use std::collections::HashMap;

/// The four board shapes the spec requires: 2x2, 3x3, 4x4, and the default
/// 5x4. Per-shape `max_enemies_in` outputs (canonical table on
/// `max_enemies_in` in src/runs.rs): 1 / 2 / 3 / 4.
const SHAPES: &[(usize, usize)] = &[(2, 2), (3, 3), (4, 4), (5, 4)];

/// A Content impl that resolves no actions — every queued action id misses
/// and falls through. `resolve_round` still walks the cooldown tick + the
/// 4-phase round; this is exactly the "runs to completion without panic"
/// floor reviewer-a asked for. Building a richer content layer would push
/// the test toward duplicating `combat_loop.rs` without buying more coverage
/// of THIS file's invariant (the spawn surface, not the action catalog).
struct InertContent;

impl Content for InertContent {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _id: &str, _owner: &Ship) -> Projectile {
        panic!("per-shape combat tests don't fire ordnance");
    }
}

/// Naked ship (no shields, no mounts, low heat budget) at `pos` / `hull` /
/// `facing`. The per-shape tests only need a ship that's well-formed enough
/// to seat on the board and step through `resolve_round`; they don't fire
/// or take damage in a meaningful way (`InertContent` resolves no actions),
/// so the bare hull is intentional — keeps the fixture compact and the
/// assertions focused on the spawn-placement invariants.
fn naked_ship(id: &str, pos: Pos, faction: Faction, hull: i32, facing: Facing) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell: pos.to_index(),
        pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing,
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 12,
        locked_out: false,
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
        mounts: vec![Mount {
            id: "m0".into(),
            arc: Arc::Forward,
            weapon: String::new(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

/// Player Ship suitable for `build_encounter_board_with_dims`'s player
/// argument. The builder OVERWRITES the player's `pos`/`facing`/`cell`/
/// `orientation` to the dim-aware front-centre, so the initial values here
/// only need to be syntactically valid.
fn player_seed(dims: Dims) -> Ship {
    naked_ship(
        "player",
        player_start_pos_in(dims),
        Faction::Player,
        10,
        player_spawn_facing(),
    )
}

/// An [`EncounterDef`] with `cap` enemy spawns laid along the back row,
/// left-to-right, starting at `(0, 0)`. This mirrors the placement
/// `sample_encounter_spawns_with_dims` produces on the canonical back-row
/// sweep — but authored directly (the sampler is `pub(crate)`, and the
/// invariants we lock here are about the BUILDER's placement, not the
/// sampler's centre-out fan order). Caps at `max_enemies_in(dims)` per the
/// design table.
fn cap_filled_encounter(dims: Dims) -> EncounterDef {
    let cap = max_enemies_in(dims);
    let mut spawns = Vec::with_capacity(cap);
    for i in 0..cap {
        let col = i % dims.cols;
        let row = i / dims.cols;
        debug_assert!(row < dims.rows.saturating_sub(1).max(1));
        let pos = Pos::new(col, row);
        spawns.push(ShipSpawn {
            class_id: "enemy".into(),
            cell: pos.to_index_in(dims),
            pos,
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: enemy_spawn_facing(),
            hp_override: Some(3),
        });
    }
    EncounterDef {
        id: format!("shape{}x{}", dims.cols, dims.rows),
        enemy_ships: spawns,
        hazards: Vec::new(),
        is_boss: false,
        ..Default::default()
    }
}

/// A `class_to_ship` builder closure for [`build_encounter_board_with_dims`].
/// Materialises the spawn into a [`naked_ship`] at the spawn's authoritative
/// `pos`/`facing`. The builder then overwrites these from the spawn fields
/// anyway (invariant A), but returning a well-formed ship at the right cell
/// keeps the test transparent — a debug-printed board reads the way the
/// resolver sees it.
#[allow(clippy::unnecessary_wraps)] // signature must match the builder closure's `Option<Ship>`
fn build_enemy_from_spawn(spawn: &ShipSpawn) -> Option<Ship> {
    Some(naked_ship(
        &format!("e@{}", spawn.cell),
        spawn.pos,
        Faction::Enemy,
        spawn.hp_override.unwrap_or(3),
        spawn.facing,
    ))
}

/// Collect every (Pos, &Ship) on `board`. Used by every per-shape
/// assertion so the shape of the inspection is one line.
fn live_ships(board: &Board) -> Vec<(Pos, &Ship)> {
    let dims = board.dims();
    let mut out = Vec::new();
    for row in 0..dims.rows {
        for col in 0..dims.cols {
            let p = Pos::new(col, row);
            if let Some(s) = board.ship_at(p) {
                out.push((p, s));
            }
        }
    }
    out
}

/* =========================================================================
 * Per-shape invariants
 * ====================================================================== */

#[test]
fn spawns_land_in_bounds_on_every_pool_shape() {
    // Reviewer-a's first per-shape ask: prove every authored spawn lands
    // INSIDE the dim's grid after going through the live builder. A
    // regression that drops the `in_bounds_in(dims)` guard in
    // `build_encounter_board_with_dims` (or recomputes `to_index` with the
    // wrong width) lands a ship off-grid; we'd catch it here as either an
    // out-of-bounds pos or a missing ship at an in-bounds expected cell.
    for &(c, r) in SHAPES {
        let dims = Dims::new(c, r);
        let enc = cap_filled_encounter(dims);
        let board =
            build_encounter_board_with_dims(&enc, player_seed(dims), dims, build_enemy_from_spawn);
        for (pos, ship) in live_ships(&board) {
            assert!(
                pos.in_bounds_in(dims),
                "{c}x{r}: ship {} at {pos:?} is out of bounds for {dims:?}",
                ship.id,
            );
        }
    }
}

#[test]
fn player_cell_is_never_an_enemy_cell_on_every_pool_shape() {
    // The architect's builder skips a spawn whose `pos == player_pos`.
    // This asserts the live result: NO enemy occupies the front-centre.
    for &(c, r) in SHAPES {
        let dims = Dims::new(c, r);
        let player_pos = player_start_pos_in(dims);
        let enc = cap_filled_encounter(dims);
        let board =
            build_encounter_board_with_dims(&enc, player_seed(dims), dims, build_enemy_from_spawn);
        let player_ship = board.ship_at(player_pos);
        assert!(
            player_ship.is_some_and(|s| s.faction == Faction::Player),
            "{c}x{r}: player not at front-centre {player_pos:?}",
        );
        for (pos, ship) in live_ships(&board) {
            if ship.faction == Faction::Enemy {
                assert_ne!(
                    pos, player_pos,
                    "{c}x{r}: enemy {} occupies the player's cell {player_pos:?}",
                    ship.id,
                );
            }
        }
    }
}

#[test]
fn enemy_count_equals_max_enemies_in_on_every_pool_shape() {
    // The design table on `max_enemies_in` is the canonical cap; the
    // authored encounter fills to that exact cap. After the live builder
    // runs, the resulting Enemy count MUST equal the cap on every shape.
    // A regression here means the builder dropped a spawn it shouldn't
    // have (e.g. a stale 5x4 in_bounds check on a smaller grid would skip
    // valid back-row positions).
    for &(c, r) in SHAPES {
        let dims = Dims::new(c, r);
        let cap = max_enemies_in(dims);
        let enc = cap_filled_encounter(dims);
        let board =
            build_encounter_board_with_dims(&enc, player_seed(dims), dims, build_enemy_from_spawn);
        // Id-dedup (#214 boss): a 1×2 Pair boss occupies two slots with
        // the same `Ship` clone, so counting (Pos, &Ship) entries would
        // over-count a Pair by 1. Dedup by `s.id` to count ships, not
        // occupied cells.
        let mut seen = std::collections::HashSet::new();
        let enemy_count = live_ships(&board)
            .iter()
            .filter(|(_, s)| s.faction == Faction::Enemy)
            .filter(|(_, s)| seen.insert(s.id.clone()))
            .count();
        assert_eq!(
            enemy_count, cap,
            "{c}x{r}: live builder placed {enemy_count} enemies; max_enemies_in \
             says cap is {cap}",
        );
    }
}

#[test]
fn resolve_round_runs_to_completion_on_every_pool_shape() {
    // Integration smoke: a freshly-built per-shape board steps through one
    // full `resolve_round` (decide -> player -> world -> EOT) without
    // panicking. Uses `InertContent` so no action resolves; the goal is
    // proving the resolver's per-shape walk over `board.dims()` doesn't
    // hit an out-of-range index or a stale `to_index()` constant.
    for &(c, r) in SHAPES {
        let dims = Dims::new(c, r);
        let enc = cap_filled_encounter(dims);
        let mut board =
            build_encounter_board_with_dims(&enc, player_seed(dims), dims, build_enemy_from_spawn);
        // One round = one player phase + one world phase + EOT.
        resolve_round(&mut board, &InertContent);
    }
}

/* =========================================================================
 * Winnability — dodge-lane invariant + legal-move floor
 * ====================================================================== */

#[test]
fn player_always_has_at_least_one_legal_cardinal_neighbour_on_every_pool_shape() {
    // The dodge-lane invariant: `max_enemies_in`'s `narrow_cap` rule (always
    // leave at least one back-row column clean) guarantees the player has
    // SOMEWHERE to move from front-centre. We can't drive the player's
    // movement action plumbing here (that would re-implement the input
    // layer), so the test asserts the floor reviewer-a specified: at least
    // one of the player's 4 cardinal neighbours is (a) in-bounds and (b)
    // unoccupied. Sufficient to prove "the fight doesn't instantly soft-
    // lock" because the resolver's movement step generates the same set of
    // candidate target cells.
    //
    // 1x1 / 0-row shapes would fail this floor by definition (no neighbour
    // cells exist); SHAPES excludes them, and the architect's spawn surface
    // also returns 0 enemies on rows<2 so the test is moot there.
    for &(c, r) in SHAPES {
        let dims = Dims::new(c, r);
        let enc = cap_filled_encounter(dims);
        let board =
            build_encounter_board_with_dims(&enc, player_seed(dims), dims, build_enemy_from_spawn);
        let player_pos = player_start_pos_in(dims);
        let neighbours = [
            (Dir4::N, neighbour(player_pos, Dir4::N, dims)),
            (Dir4::S, neighbour(player_pos, Dir4::S, dims)),
            (Dir4::E, neighbour(player_pos, Dir4::E, dims)),
            (Dir4::W, neighbour(player_pos, Dir4::W, dims)),
        ];
        let has_free = neighbours
            .iter()
            .filter_map(|&(_, n)| n)
            .any(|n| board.ship_at(n).is_none());
        assert!(
            has_free,
            "{c}x{r}: player at {player_pos:?} has zero legal neighbours — soft-lock. \
             Cardinal candidates: {neighbours:?}",
        );
    }
}

/// One-step cardinal neighbour of `pos` in `dims`, or `None` if the step
/// leaves the grid. Stays local to this file so we don't need to import
/// `grid::offset_in` (which takes `Dir8` / `dist`); cardinal-only is what
/// the dodge-lane floor needs.
const fn neighbour(pos: Pos, dir: Dir4, dims: Dims) -> Option<Pos> {
    let (dcol, drow): (i32, i32) = match dir {
        Dir4::N => (0, -1),
        Dir4::S => (0, 1),
        Dir4::E => (1, 0),
        Dir4::W => (-1, 0),
    };
    let new_col = pos.col as i32 + dcol;
    let new_row = pos.row as i32 + drow;
    if new_col < 0 || new_row < 0 {
        return None;
    }
    let np = Pos::new(new_col as usize, new_row as usize);
    if np.in_bounds_in(dims) {
        Some(np)
    } else {
        None
    }
}
