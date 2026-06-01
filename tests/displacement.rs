//! Displacement invariant locks (content's spec, series A).
//!
//! Displacement is one of the systems with **no TypeScript reference** — the
//! TS `resolveSelfMove` / `resolveTargetMove` were stubs — so these tests are
//! the canonical behavior spec, not a parity check. They pin the exact
//! mechanics of `resolve_self_move` / `resolve_target_move` (THRUST / BURN /
//! SLIP / JUMP / TRACTOR_SWAP self-moves; Push / Pull / Swap target-moves).
//!
//! ## How these drive the (private) movement functions
//!
//! `resolve_self_move` / `resolve_target_move` are private to `resolve.rs`,
//! but the resolver re-exports the effect dispatcher `apply_effect` as `pub`.
//! `apply_effect` maps an `Effect::DISPLACE_SELF` to `resolve_self_move` and an
//! `Effect::DISPLACE_TARGET` to `resolve_target_move`, so building the matching
//! `Effect` and calling `apply_effect(&fx, &action, source_cell, &cells, …)`
//! exercises the real primitive directly — with the exact source / target
//! cells under test, and without the heat / cooldown / arc gating that
//! `apply_instant_action` would layer on top (the gates are tested elsewhere;
//! here we isolate the movement math).
//!
//! For `DISPLACE_SELF` the `cells` slice is unused (the mode reads
//! `source_cell`). For `DISPLACE_TARGET` the `cells` slice IS the target set;
//! A9's source==target degenerate case passes `&[source_cell]`.
//!
//! Collision damage routes through `apply_damage(landing, remaining, phantom,
//! &dummy_weapon(), …)`, so the directional shield mediates. To keep the
//! arithmetic legible, the moving / displaced ships use an **armour-0 profile**
//! so `remaining × 1` lands raw on hull.

use broadside_engine::resolve::{apply_effect, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, DisplaceMode, Effect, EventBus, Faction, LaneEnd, Mount,
    MovementMode, Orientation, Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting,
    TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

/// All-zero shield faces, so collision / push damage lands raw on hull and
/// the expected numbers are `remaining × 1`.
fn bare_profile() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace { armour: 0, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 0, charge: 0 },
        starboard: ShieldFace { armour: 0, charge: 0 },
    }
}

fn ship(id: &str, cell: usize, hull: i32, bow: LaneEnd) -> Ship {
    Ship {
        id: id.into(),
        faction: Faction::Player,
        cell,
        orientation: Orientation::BowOn { bow },
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: bare_profile(),
        mounts: Vec::new(),
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

fn board(size: usize, cells: Vec<Option<Ship>>) -> Board {
    Board {
        size,
        cells,
        ordnance: Vec::new(),
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
    }
}

/// Content with no actions / no ordnance — displacement primitives never look
/// either up; `apply_damage`'s `&dummy_weapon()` is internal to the resolver.
struct NoContent;
impl Content for NoContent {
    fn action(&self, _: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        unreachable!("displacement tests never spawn ordnance");
    }
}

/// A minimal arc-less action carrier. `apply_effect` only reads the effect we
/// pass plus (for DAMAGE) the action's effects, so the surrounding action is
/// inert for displacement — but `apply_effect` takes `&Action`, so we need
/// one. SELF pattern keeps it honest about how the bin would target a
/// self-move.
fn carrier() -> Action {
    Action {
        id: "_move".into(),
        name: "Move".into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: Vec::new(),
        r#mod: None,
        icon: None,
    }
}

/// Fire a DISPLACE_SELF effect at `source_cell`.
fn self_move(board: &mut Board, source_cell: usize, mode: MovementMode, distance: i32, dir: Option<LaneEnd>) {
    let fx = Effect::DISPLACE_SELF { mode, distance, direction: dir };
    apply_effect(&fx, &carrier(), source_cell, &[], board, &NoContent);
}

/// Fire a DISPLACE_TARGET effect from `source_cell` onto `target_cells`.
fn target_move(board: &mut Board, source_cell: usize, target_cells: &[usize], mode: DisplaceMode, distance: i32) {
    let fx = Effect::DISPLACE_TARGET { mode, distance };
    apply_effect(&fx, &carrier(), source_cell, target_cells, board, &NoContent);
}

/// Cell of the ship with the given id, if any.
fn cell_of(board: &Board, id: &str) -> Option<usize> {
    board.cells.iter().flatten().find(|s| s.id == id).map(|s| s.cell)
}

fn hull_of(board: &Board, id: &str) -> i32 {
    board.cells.iter().flatten().find(|s| s.id == id).expect("ship alive").hull
}

/* =========================================================================
 * A1 — THRUST ignores its distance argument; always moves exactly one cell.
 * ====================================================================== */

#[test]
fn a1_thrust_moves_exactly_one_cell_ignoring_distance() {
    let mut b = board(5, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, None]);
    self_move(&mut b, 1, MovementMode::THRUST, 3, None); // distance 3 is ignored
    assert_eq!(cell_of(&b, "p"), Some(2), "THRUST is canonically one cell, distance arg ignored");
    assert_eq!(hull_of(&b, "p"), 5, "clear move takes no collision damage");
}

/* =========================================================================
 * A2 — THRUST blocked by an occupant: stay put + 1 collision damage.
 * ====================================================================== */

#[test]
fn a2_thrust_into_occupant_stays_and_takes_one_collision() {
    let mut b = board(
        5,
        vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), Some(ship("x", 2, 5, LaneEnd::Fore)), None, None],
    );
    self_move(&mut b, 1, MovementMode::THRUST, 1, None);
    assert_eq!(cell_of(&b, "p"), Some(1), "blocked THRUST stays in place");
    assert_eq!(hull_of(&b, "p"), 4, "blocked THRUST takes exactly 1 collision (armour-0 => raw)");
}

#[test]
fn a2_thrust_into_wall_stays_and_takes_one_collision() {
    let mut b = board(5, vec![None, None, None, None, Some(ship("p", 4, 5, LaneEnd::Fore))]);
    self_move(&mut b, 4, MovementMode::THRUST, 1, None);
    assert_eq!(cell_of(&b, "p"), Some(4), "THRUST into the fore wall stays put");
    assert_eq!(hull_of(&b, "p"), 4, "wall collision is 1 damage");
}

/* =========================================================================
 * A3 — BURN stops one cell short of the first occupant; collision = remaining×1.
 * ====================================================================== */

#[test]
fn a3_burn_stops_short_of_occupant_and_bills_remaining_collision() {
    // p@1 BURN 4 toward x@4: advances 1->2->3 (2 cells), blocked at 4.
    // remaining = 4 - 2 = 2 collision damage. hull 5 - 2 = 3.
    let mut b = board(
        7,
        vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, Some(ship("x", 4, 5, LaneEnd::Fore)), None, None],
    );
    self_move(&mut b, 1, MovementMode::BURN, 4, None);
    assert_eq!(cell_of(&b, "p"), Some(3), "BURN halts one cell short of the occupant");
    assert_eq!(hull_of(&b, "p"), 3, "remaining distance (4-2=2) bills 2 collision damage");
}

/* =========================================================================
 * A4 — BURN over a clear lane advances the full distance, no collision.
 * ====================================================================== */

#[test]
fn a4_burn_clear_advances_full_distance() {
    let mut b = board(7, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, None, None, None]);
    self_move(&mut b, 1, MovementMode::BURN, 3, None);
    assert_eq!(cell_of(&b, "p"), Some(4), "clear BURN covers the full distance");
    assert_eq!(hull_of(&b, "p"), 5, "no block => no collision");
}

/* =========================================================================
 * A5 — SLIP passes through occupants, lands in the first free cell at/after
 *      start + distance.
 * ====================================================================== */

#[test]
fn a5_slip_passes_through_occupants_to_first_free_cell() {
    // p@1 SLIP 2: scans 2 cells ahead (1->2->3), both occupied; keeps walking
    // to the first free cell — cell 4. hull unchanged (SLIP never collides
    // when it finds a free cell).
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            Some(ship("a", 2, 5, LaneEnd::Fore)),
            Some(ship("b", 3, 5, LaneEnd::Fore)),
            None,
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::SLIP, 2, None);
    assert_eq!(cell_of(&b, "p"), Some(4), "SLIP slides through 2,3 and lands in the first free cell (4)");
    assert_eq!(hull_of(&b, "p"), 5, "a SLIP that finds a free cell takes no collision");
    assert!(cell_of(&b, "a") == Some(2) && cell_of(&b, "b") == Some(3), "passed-through ships don't move");
}

#[test]
fn a5_slip_clamps_to_edge_and_bills_collision_when_no_free_cell() {
    // p@1 SLIP 2 with 2,3,4,5,6 all occupied: no free cell ahead => clamp to
    // edge (6) and bill collision = (distance - advanced).max(1). advanced =
    // (6 - 1) = 5 along +1, so (2 - 5).max(1) = 1.
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            Some(ship("a", 2, 5, LaneEnd::Fore)),
            Some(ship("b", 3, 5, LaneEnd::Fore)),
            Some(ship("c", 4, 5, LaneEnd::Fore)),
            Some(ship("d", 5, 5, LaneEnd::Fore)),
            Some(ship("e", 6, 5, LaneEnd::Fore)),
        ],
    );
    self_move(&mut b, 1, MovementMode::SLIP, 2, None);
    // No free cell ahead => the SLIP edge-clamp branch fires: p clamps to the
    // fore edge (cell 6) and is billed the floor-1 collision (hull 5 -> 4).
    // NOTE: this exercises a corner of resolve_self_move that only arises when
    // the ENTIRE forward lane is packed — not reachable in normal play (a
    // 7-cell lane never holds 6 ships). The assertions below pin the current
    // behavior exactly so any future change to the clamp branch is visible.
    assert_eq!(cell_of(&b, "p"), Some(6), "no free cell => p clamps to the fore edge");
    assert_eq!(hull_of(&b, "p"), 4, "edge clamp bills the floor-1 collision (5 -> 4)");
}

/* =========================================================================
 * A6 — JUMP onto an occupied cell is a no-op; onto a clear cell it lands.
 * ====================================================================== */

#[test]
fn a6_jump_onto_occupied_cell_is_a_noop() {
    let mut b = board(7, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, Some(ship("x", 3, 5, LaneEnd::Fore)), None, None, None]);
    self_move(&mut b, 1, MovementMode::JUMP, 2, None); // target 1+2 = 3, occupied
    assert_eq!(cell_of(&b, "p"), Some(1), "JUMP onto an occupied cell fails with no move");
    assert_eq!(hull_of(&b, "p"), 5, "failed JUMP deals no collision (it ignores the path)");
}

#[test]
fn a6_jump_onto_clear_cell_blinks_directly() {
    let mut b = board(7, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, None, None, None]);
    self_move(&mut b, 1, MovementMode::JUMP, 2, None); // target 3, clear
    assert_eq!(cell_of(&b, "p"), Some(3), "JUMP blinks straight to the target cell");
    assert_eq!(hull_of(&b, "p"), 5, "clean JUMP, no collision");
}

/* =========================================================================
 * A7 — TRACTOR_SWAP trades cells with the adjacent bow-ward occupant.
 * ====================================================================== */

#[test]
fn a7_tractor_swap_trades_with_adjacent_occupant() {
    let mut b = board(5, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), Some(ship("x", 2, 5, LaneEnd::Fore)), None, None]);
    self_move(&mut b, 1, MovementMode::TRACTOR_SWAP, 1, None);
    assert_eq!(cell_of(&b, "p"), Some(2), "swapper takes the occupant's cell");
    assert_eq!(cell_of(&b, "x"), Some(1), "occupant takes the swapper's cell");
    assert_eq!(hull_of(&b, "p"), 5, "swap is a controlled trade, no collision");
    assert_eq!(hull_of(&b, "x"), 5, "swapped partner unharmed");
}

#[test]
fn a7_tractor_swap_with_no_adjacent_occupant_is_a_noop() {
    let mut b = board(5, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, None]);
    self_move(&mut b, 1, MovementMode::TRACTOR_SWAP, 1, None);
    assert_eq!(cell_of(&b, "p"), Some(1), "nothing adjacent to swap with => no move");
}

/* =========================================================================
 * A8 — direction override beats the bow-derived default.
 * ====================================================================== */

#[test]
fn a8_thrust_direction_override_moves_against_the_bow() {
    // bow=Fore would step +1, but dir:Some(Aft) forces -1.
    let mut b = board(5, vec![None, None, Some(ship("p", 2, 5, LaneEnd::Fore)), None, None]);
    self_move(&mut b, 2, MovementMode::THRUST, 1, Some(LaneEnd::Aft));
    assert_eq!(cell_of(&b, "p"), Some(1), "explicit Aft direction overrides the Fore bow");
}

#[test]
fn a8_thrust_with_no_override_follows_aft_bow() {
    let mut b = board(5, vec![None, None, Some(ship("p", 2, 5, LaneEnd::Aft)), None, None]);
    self_move(&mut b, 2, MovementMode::THRUST, 1, None);
    assert_eq!(cell_of(&b, "p"), Some(1), "bow=Aft with no override steps toward lower cells");
}

/* =========================================================================
 * A9 — DISPLACE_TARGET Swap with source == target is a no-op.
 *      Pins the #97 mechanically-dead-signature fix against regression.
 * ====================================================================== */

#[test]
fn a9_swap_source_equals_target_is_a_noop() {
    // The #97 dead-signature shape: a SELF-targeted Swap resolves its target
    // set to the source's own cell, so resolve_target_move sees
    // source_cell == target_cell and must early-return without touching the
    // board.
    let mut b = board(5, vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, Some(ship("x", 3, 5, LaneEnd::Fore)), None]);
    target_move(&mut b, 1, &[1], DisplaceMode::Swap, 2); // target == source (cell 1)
    assert_eq!(cell_of(&b, "p"), Some(1), "self-Swap leaves the source where it is");
    assert_eq!(cell_of(&b, "x"), Some(3), "self-Swap touches no other ship");
    assert_eq!(hull_of(&b, "p"), 5, "no damage from a degenerate swap");
}

/* =========================================================================
 * A10 — Push moves the target away from the source; Pull toward, stopping one
 *       short of the source. Blocked moves bill remaining×1 collision.
 * ====================================================================== */

#[test]
fn a10_push_moves_target_away_from_source() {
    // src@1, tgt@2, clear behind. Push 2 => tgt away from src (toward higher
    // cells): 2 -> 4.
    let mut b = board(7, vec![None, Some(ship("src", 1, 5, LaneEnd::Fore)), Some(ship("tgt", 2, 5, LaneEnd::Fore)), None, None, None, None]);
    target_move(&mut b, 1, &[2], DisplaceMode::Push, 2);
    assert_eq!(cell_of(&b, "tgt"), Some(4), "Push drives the target 2 cells away from the source");
    assert_eq!(hull_of(&b, "tgt"), 5, "clear push, no collision");
}

#[test]
fn a10_pull_stops_one_cell_short_of_the_source() {
    // src@1, tgt@4. Pull 2 => tgt toward src: 4 -> 3 -> 2 (2 cells). It would
    // continue toward 1 but stops because the source occupies cell 1.
    let mut b = board(7, vec![None, Some(ship("src", 1, 5, LaneEnd::Fore)), None, None, Some(ship("tgt", 4, 5, LaneEnd::Fore)), None, None]);
    target_move(&mut b, 1, &[4], DisplaceMode::Pull, 2);
    assert_eq!(cell_of(&b, "tgt"), Some(2), "Pull draws the target toward the source by 2");
    assert_eq!(hull_of(&b, "tgt"), 5, "unobstructed pull, no collision");
}

#[test]
fn a10_push_blocked_by_wall_bills_remaining_collision() {
    // src@4, tgt@5, push toward the fore wall. distance 3: 5 -> 6 (1 cell),
    // blocked by the wall. remaining = 3 - 1 = 2 collision. hull 5 - 2 = 3.
    let mut b = board(7, vec![None, None, None, None, Some(ship("src", 4, 5, LaneEnd::Fore)), Some(ship("tgt", 5, 5, LaneEnd::Fore)), None]);
    target_move(&mut b, 4, &[5], DisplaceMode::Push, 3);
    assert_eq!(cell_of(&b, "tgt"), Some(6), "push reaches the last cell then the wall blocks");
    assert_eq!(hull_of(&b, "tgt"), 3, "remaining distance (3-1=2) bills 2 collision damage");
}

/* =========================================================================
 * A11 — collision attribution: the displaced/blocked ship's hull drops by
 *       exactly remaining×1 (covered numerically by A2/A3/A10; this asserts
 *       the only-the-moving-ship-is-hurt invariant explicitly).
 * ====================================================================== */

#[test]
fn a11_collision_damages_only_the_moving_ship_not_the_blocker() {
    let mut b = board(
        5,
        vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), Some(ship("blocker", 2, 5, LaneEnd::Fore)), None, None],
    );
    self_move(&mut b, 1, MovementMode::THRUST, 1, None);
    assert_eq!(hull_of(&b, "p"), 4, "the rammer eats the 1 collision");
    assert_eq!(hull_of(&b, "blocker"), 5, "the blocker is untouched by the rammer's collision");
}
