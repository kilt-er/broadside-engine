//! Displacement invariant locks (content's spec, series A).
//!
//! Displacement is one of the systems with **no TypeScript reference** — the
//! TS `resolveSelfMove` / `resolveTargetMove` were stubs — so these tests are
//! the canonical behavior spec, not a parity check. They pin the exact
//! mechanics of `resolve_self_move` / `resolve_target_move` (THRUST / BURN /
//! SLIP / JUMP / `TRACTOR_SWAP` self-moves; Push / Pull / Swap target-moves).
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
    Action, ActionCost, Board, DisplaceMode, Effect, EventBus, Faction, LaneEnd, MovementMode,
    Orientation, Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting,
    TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

/// All-zero shield faces, so collision / push damage lands raw on hull and
/// the expected numbers are `remaining × 1`.
const fn bare_profile() -> ShieldProfile {
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

/// A ship at column `cell` on **row 0** (so `pos.to_index() == cell`, and the
/// cell-delta asserts read as +/-1 columns), facing **E** (`bow`'s lane end maps
/// to the E-W axis: Fore->Bow(E), Aft->Bow(W)). Invariant A holds.
///
/// #22 2-D migration: displacement's `DISPLACE_SELF` moves run along `facing` (the
/// 2-D mover reads it), so a row-0 + Bow(E) layout makes a "forward" THRUST step
/// E = `+1` cell, matching every 1-D delta assertion. `DISPLACE_TARGET` is
/// geometry-derived (`direction_to` over pos) and also resolves cleanly on row 0.
fn ship(id: &str, cell: usize, hull: i32, bow: LaneEnd) -> Ship {
    use broadside_engine::grid::{Dir4, Facing, Pos};
    let pos = Pos::new(cell, 0);
    // Fore (the +lane direction) -> Bow(E) (the +column direction); Aft -> Bow(W).
    let facing = match bow {
        LaneEnd::Fore => Facing::Bow(Dir4::E),
        LaneEnd::Aft => Facing::Bow(Dir4::W),
    };
    Ship {
        id: id.into(),
        faction: Faction::Player,
        cell: pos.to_index(),
        pos,
        orientation: Orientation::BowOn { bow },
        facing,
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
        tail: None,
    }
}

/// Place ships on the fixed len-CELLS 5x4 grid at `cells[pos.to_index()]`
/// (invariant A). `_size` is ignored (kept so the many call sites read
/// unchanged); the board is always the real `COLS`-wide grid. Each test fits its
/// ships within row 0 (columns 0..COLS).
fn board(_size: usize, cells: Vec<Option<Ship>>) -> Board {
    // The callers pass a `Vec<Option<Ship>>` indexed by the OLD 1-D cell, which
    // on row 0 equals pos.to_index() — but the vec is only `size` long. Re-home
    // each present ship into a full len-CELLS grid by its pos.
    let mut grid: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    for slot in cells.into_iter().flatten() {
        let idx = slot.pos.to_index();
        assert!(grid[idx].is_none(), "two ships share cell {idx}");
        grid[idx] = Some(slot);
    }
    Board {
        size: broadside_engine::grid::COLS,
        cols: broadside_engine::grid::COLS,
        rows: broadside_engine::grid::ROWS,
        cells: grid,
        ordnance: Vec::new(),
        hazards: (0..broadside_engine::grid::CELLS)
            .map(|_| Vec::new())
            .collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
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
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
                broadside_engine::grid::Range::Far,
            ],
            optimal_range: broadside_engine::grid::Range::Adjacent,
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

/// Fire a `DISPLACE_SELF` effect at `source_cell`.
///
/// #22 2-D: the live 2-D mover (`resolve_self_move_2d`) reads `direction_2d` (else
/// the ship's `facing`), NOT the legacy 1-D `direction`. So translate the 1-D
/// `dir` override into its row-0 2-D cardinal — Fore (the +lane / +column
/// direction) -> E, Aft -> W — and pass it as `direction_2d`. `None` falls back
/// to the ship's facing (E for a Fore-bow ship, W for an Aft-bow ship), so a
/// no-override THRUST steps along the bow exactly as the 1-D version did.
fn self_move(
    board: &mut Board,
    source_cell: usize,
    mode: MovementMode,
    distance: i32,
    dir: Option<LaneEnd>,
) {
    use broadside_engine::grid::Dir4;
    let direction_2d = dir.map(|d| match d {
        LaneEnd::Fore => Dir4::E,
        LaneEnd::Aft => Dir4::W,
    });
    let fx = Effect::DISPLACE_SELF {
        mode,
        distance,
        direction: dir,
        direction_2d,
    };
    apply_effect(&fx, &carrier(), source_cell, &[], board, &NoContent);
}

/// Fire a `DISPLACE_TARGET` effect from `source_cell` onto `target_cells`.
fn target_move(
    board: &mut Board,
    source_cell: usize,
    target_cells: &[usize],
    mode: DisplaceMode,
    distance: i32,
) {
    let fx = Effect::DISPLACE_TARGET { mode, distance };
    apply_effect(
        &fx,
        &carrier(),
        source_cell,
        target_cells,
        board,
        &NoContent,
    );
}

/// Cell of the ship with the given id, if any.
fn cell_of(board: &Board, id: &str) -> Option<usize> {
    board
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == id)
        .map(|s| s.cell)
}

fn hull_of(board: &Board, id: &str) -> i32 {
    board
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == id)
        .expect("ship alive")
        .hull
}

/* =========================================================================
 * A1 — THRUST ignores its distance argument; always moves exactly one cell.
 * ====================================================================== */

// #[ignore] (all the failing a*): stale 1-D fixture — R6/R6b moved displacement
// (DISPLACE_SELF/TARGET) to 2-D (reads pos); these build 1-D boards (pos (0,0)).
// NOT a 2-D bug — the resolver's rsm2d_*/rt2d_* unit tests prove the 2-D movers.
// Restore via board_2d/ship_2d (real positions + 2-D direction asserts) — #22.
#[test]
fn a1_thrust_moves_exactly_one_cell_ignoring_distance() {
    let mut b = board(
        5,
        vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, None],
    );
    self_move(&mut b, 1, MovementMode::THRUST, 3, None); // distance 3 is ignored
    assert_eq!(
        cell_of(&b, "p"),
        Some(2),
        "THRUST is canonically one cell, distance arg ignored"
    );
    assert_eq!(hull_of(&b, "p"), 5, "clear move takes no collision damage");
}

/* =========================================================================
 * A2 — THRUST blocked by an occupant OR wall: stay put, NO collision damage.
 *
 * (#323 Bruce ruling 2026-07-01) A blocked forward move is a clean NO-OP:
 * neither party takes damage. Pre-#323 both wall and occupant billed 1
 * collision damage to the actor -- the ram-into-enemy loop that Bruce hit
 * (player damaged every ram, enemy AI also ramming so enemy also bled,
 * both dying with no explosion + false-win). Zero-damage on both cases
 * makes basic THRUST-into-block a true no-op. BURN / SLIP / JUMP still
 * bill their remaining-distance collision damage (skill-move cost intact).
 * ====================================================================== */

#[test]
fn a2_thrust_into_occupant_stays_and_takes_no_damage() {
    let mut b = board(
        5,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            Some(ship("x", 2, 5, LaneEnd::Fore)),
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::THRUST, 1, None);
    assert_eq!(cell_of(&b, "p"), Some(1), "blocked THRUST stays in place");
    assert_eq!(
        hull_of(&b, "p"),
        5,
        "blocked THRUST takes NO collision damage (#323 Bruce ruling)"
    );
    assert_eq!(
        hull_of(&b, "x"),
        5,
        "blocked THRUST leaves the OCCUPANT untouched (#323 Bruce ruling)"
    );
}

#[test]
fn a2_thrust_into_wall_stays_and_takes_no_damage() {
    let mut b = board(
        5,
        vec![None, None, None, None, Some(ship("p", 4, 5, LaneEnd::Fore))],
    );
    self_move(&mut b, 4, MovementMode::THRUST, 1, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(4),
        "THRUST into the fore wall stays put"
    );
    assert_eq!(
        hull_of(&b, "p"),
        5,
        "wall block takes NO collision damage (#323 Bruce ruling)"
    );
}

/// (#323 Bruce repro 2026-07-01) Bruce's exact scenario: player MOVES
/// FORWARD into an enemy-occupied cell repeatedly. Pre-#323 each ram billed
/// 1 collision damage to the actor via `apply_damage(landing=ship_pos)`, so
/// after N rams the actor bled N hull. Combined with the enemy AI also
/// ramming the player (its world-phase drives the same THRUST path), both
/// ships bled every turn until one hit 0 hull, vanished with no explosion,
/// and the win-check tripped a false advance. New behavior: 5 forward-move
/// commands into the same enemy → both hulls stay at max, enemy is still
/// present, no destroys registered. Runs the resolver primitive directly
/// (like the A-series does) so the assertion is on the movement math
/// itself, independent of the bin's input pipeline.
#[test]
fn a2_repeated_ram_into_enemy_never_damages_either_ship() {
    let mut b = board(
        5,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            Some(ship("x", 2, 5, LaneEnd::Fore)),
            None,
            None,
        ],
    );
    let destroys_before = b.destroys_this_window;
    // Five rams -- Bruce reports the false-win at 2. Five here proves it's
    // not a delayed effect at higher hit-counts.
    for _ in 0..5 {
        self_move(&mut b, 1, MovementMode::THRUST, 1, None);
    }
    assert_eq!(
        cell_of(&b, "p"),
        Some(1),
        "player never moves (enemy blocks every ram)"
    );
    assert_eq!(
        hull_of(&b, "p"),
        5,
        "player hull FULL after 5 rams (was 0 pre-#323 with 1 damage/ram)"
    );
    assert_eq!(
        cell_of(&b, "x"),
        Some(2),
        "enemy STILL PRESENT after 5 rams (was silently removed pre-#323 when its own AI's return-rams dropped it to 0 hull)"
    );
    assert_eq!(
        hull_of(&b, "x"),
        5,
        "enemy hull FULL after 5 rams (never touched by the block)"
    );
    assert_eq!(
        b.destroys_this_window, destroys_before,
        "no destroys logged during the ram loop -- no false-win chain"
    );
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
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            None,
            None,
            Some(ship("x", 4, 5, LaneEnd::Fore)),
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::BURN, 4, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(3),
        "BURN halts one cell short of the occupant"
    );
    assert_eq!(
        hull_of(&b, "p"),
        3,
        "remaining distance (4-2=2) bills 2 collision damage"
    );
}

/* =========================================================================
 * A4 — BURN over a clear lane advances the full distance, no collision.
 * ====================================================================== */

#[test]
fn a4_burn_clear_advances_full_distance() {
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            None,
            None,
            None,
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::BURN, 3, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(4),
        "clear BURN covers the full distance"
    );
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
    assert_eq!(
        cell_of(&b, "p"),
        Some(4),
        "SLIP slides through 2,3 and lands in the first free cell (4)"
    );
    assert_eq!(
        hull_of(&b, "p"),
        5,
        "a SLIP that finds a free cell takes no collision"
    );
    assert!(
        cell_of(&b, "a") == Some(2) && cell_of(&b, "b") == Some(3),
        "passed-through ships don't move"
    );
}

#[test]
fn a5_slip_no_free_cell_ahead_stays_put_and_bills_collision() {
    // #22 2-D (row 0, COLS=5): p@0 SLIP 2 with cols 1,2,3,4 all occupied — the
    // entire forward lane is packed and col 5 is off-grid, so the SLIP finds NO
    // free cell. NOTE the 2-D resolver differs from the old 1-D here: rather than
    // clamping to the edge cell, resolve_self_move_2d's SLIP "ran off the lane
    // before a free cell" branch keeps the ship at its ORIGIN and bills the
    // floor-1 collision (hull 5 -> 4). This is a deliberate 2-D behaviour (pinned
    // by the resolver's rsm2d_* units); a corner not reachable in normal play
    // (a row never holds COLS ships). The asserts pin the current 2-D behaviour.
    let mut b = board(
        5,
        vec![
            Some(ship("p", 0, 5, LaneEnd::Fore)),
            Some(ship("a", 1, 5, LaneEnd::Fore)),
            Some(ship("b", 2, 5, LaneEnd::Fore)),
            Some(ship("c", 3, 5, LaneEnd::Fore)),
            Some(ship("d", 4, 5, LaneEnd::Fore)),
        ],
    );
    self_move(&mut b, 0, MovementMode::SLIP, 2, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(0),
        "no free cell ahead => the 2-D SLIP keeps p at its origin"
    );
    assert_eq!(
        hull_of(&b, "p"),
        4,
        "the no-free-cell SLIP bills the floor-1 collision (5 -> 4)"
    );
}

/* =========================================================================
 * A6 — JUMP onto an occupied cell is a no-op; onto a clear cell it lands.
 * ====================================================================== */

#[test]
fn a6_jump_onto_occupied_cell_is_a_noop() {
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            None,
            Some(ship("x", 3, 5, LaneEnd::Fore)),
            None,
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::JUMP, 2, None); // target 1+2 = 3, occupied
    assert_eq!(
        cell_of(&b, "p"),
        Some(1),
        "JUMP onto an occupied cell fails with no move"
    );
    assert_eq!(
        hull_of(&b, "p"),
        5,
        "failed JUMP deals no collision (it ignores the path)"
    );
}

#[test]
fn a6_jump_onto_clear_cell_blinks_directly() {
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            None,
            None,
            None,
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::JUMP, 2, None); // target 3, clear
    assert_eq!(
        cell_of(&b, "p"),
        Some(3),
        "JUMP blinks straight to the target cell"
    );
    assert_eq!(hull_of(&b, "p"), 5, "clean JUMP, no collision");
}

/* =========================================================================
 * A7 — TRACTOR_SWAP trades cells with the adjacent bow-ward occupant.
 * ====================================================================== */

#[test]
fn a7_tractor_swap_trades_with_adjacent_occupant() {
    let mut b = board(
        5,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            Some(ship("x", 2, 5, LaneEnd::Fore)),
            None,
            None,
        ],
    );
    self_move(&mut b, 1, MovementMode::TRACTOR_SWAP, 1, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(2),
        "swapper takes the occupant's cell"
    );
    assert_eq!(
        cell_of(&b, "x"),
        Some(1),
        "occupant takes the swapper's cell"
    );
    assert_eq!(
        hull_of(&b, "p"),
        5,
        "swap is a controlled trade, no collision"
    );
    assert_eq!(hull_of(&b, "x"), 5, "swapped partner unharmed");
}

#[test]
fn a7_tractor_swap_with_no_adjacent_occupant_is_a_noop() {
    let mut b = board(
        5,
        vec![None, Some(ship("p", 1, 5, LaneEnd::Fore)), None, None, None],
    );
    self_move(&mut b, 1, MovementMode::TRACTOR_SWAP, 1, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(1),
        "nothing adjacent to swap with => no move"
    );
}

/* =========================================================================
 * A8 — direction override beats the bow-derived default.
 * ====================================================================== */

#[test]
fn a8_thrust_direction_override_moves_against_the_bow() {
    // bow=Fore would step +1, but dir:Some(Aft) forces -1.
    let mut b = board(
        5,
        vec![None, None, Some(ship("p", 2, 5, LaneEnd::Fore)), None, None],
    );
    self_move(&mut b, 2, MovementMode::THRUST, 1, Some(LaneEnd::Aft));
    assert_eq!(
        cell_of(&b, "p"),
        Some(1),
        "explicit Aft direction overrides the Fore bow"
    );
}

#[test]
fn a8_thrust_with_no_override_follows_aft_bow() {
    let mut b = board(
        5,
        vec![None, None, Some(ship("p", 2, 5, LaneEnd::Aft)), None, None],
    );
    self_move(&mut b, 2, MovementMode::THRUST, 1, None);
    assert_eq!(
        cell_of(&b, "p"),
        Some(1),
        "bow=Aft with no override steps toward lower cells"
    );
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
    let mut b = board(
        5,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            None,
            Some(ship("x", 3, 5, LaneEnd::Fore)),
            None,
        ],
    );
    target_move(&mut b, 1, &[1], DisplaceMode::Swap, 2); // target == source (cell 1)
    assert_eq!(
        cell_of(&b, "p"),
        Some(1),
        "self-Swap leaves the source where it is"
    );
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
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("src", 1, 5, LaneEnd::Fore)),
            Some(ship("tgt", 2, 5, LaneEnd::Fore)),
            None,
            None,
            None,
            None,
        ],
    );
    target_move(&mut b, 1, &[2], DisplaceMode::Push, 2);
    assert_eq!(
        cell_of(&b, "tgt"),
        Some(4),
        "Push drives the target 2 cells away from the source"
    );
    assert_eq!(hull_of(&b, "tgt"), 5, "clear push, no collision");
}

#[test]
fn a10_pull_stops_one_cell_short_of_the_source() {
    // src@1, tgt@4. Pull 2 => tgt toward src: 4 -> 3 -> 2 (2 cells). It would
    // continue toward 1 but stops because the source occupies cell 1.
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("src", 1, 5, LaneEnd::Fore)),
            None,
            None,
            Some(ship("tgt", 4, 5, LaneEnd::Fore)),
            None,
            None,
        ],
    );
    target_move(&mut b, 1, &[4], DisplaceMode::Pull, 2);
    assert_eq!(
        cell_of(&b, "tgt"),
        Some(2),
        "Pull draws the target toward the source by 2"
    );
    assert_eq!(hull_of(&b, "tgt"), 5, "unobstructed pull, no collision");
}

#[test]
fn a10_push_blocked_by_wall_bills_remaining_collision() {
    // #22 2-D (row 0, COLS=5): src@2, tgt@3, push E toward the E wall. distance
    // 3: 3 -> 4 (1 cell), then 4 -> col 5 is off-grid (wall). remaining = 3 - 1
    // = 2 collision. hull 5 - 2 = 3. (Shifted into the 5-wide grid; same shape
    // as the old 7-wide src@4/tgt@5 case.)
    let mut b = board(
        5,
        vec![
            None,
            None,
            Some(ship("src", 2, 5, LaneEnd::Fore)),
            Some(ship("tgt", 3, 5, LaneEnd::Fore)),
            None,
        ],
    );
    target_move(&mut b, 2, &[3], DisplaceMode::Push, 3);
    assert_eq!(
        cell_of(&b, "tgt"),
        Some(4),
        "push reaches the last cell (4) then the wall blocks"
    );
    assert_eq!(
        hull_of(&b, "tgt"),
        3,
        "remaining distance (3-1=2) bills 2 collision damage"
    );
}

/* =========================================================================
 * A11 — collision attribution (BURN/SLIP/JUMP): the mover eats the
 *       collision, the blocker is untouched. THRUST is a special case
 *       under #323 -- it now takes NO damage at all (see A2 tests); the
 *       "actor is the only ship damaged" invariant is now asserted through
 *       BURN, whose remaining×1 collision still applies (skill-move cost
 *       stays intact per Bruce's ruling scope: only basic THRUST is
 *       de-fanged, BURN/SLIP/JUMP keep their existing collision math).
 * ====================================================================== */

#[test]
fn a11_burn_collision_damages_only_the_moving_ship_not_the_blocker() {
    let mut b = board(
        7,
        vec![
            None,
            Some(ship("p", 1, 5, LaneEnd::Fore)),
            None,
            None,
            Some(ship("blocker", 4, 5, LaneEnd::Fore)),
            None,
            None,
        ],
    );
    // BURN 4 into blocker at cell 4: mover advances 1→2→3 (2 cells), blocked
    // at 4. remaining = 4 - 2 = 2 collision damage attributed to the mover.
    self_move(&mut b, 1, MovementMode::BURN, 4, None);
    assert_eq!(
        hull_of(&b, "p"),
        3,
        "the burning rammer eats the 2 collision"
    );
    assert_eq!(
        hull_of(&b, "blocker"),
        5,
        "the blocker is untouched by the rammer's collision (still)"
    );
}
