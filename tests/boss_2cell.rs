//! Multi-cell boss (#214) invariants — locks the architect's 1×2 footprint
//! contract via the resolver's live damage / targeting / move / destroy paths.
//!
//! The architect's 75179ae resolver half routes:
//!   - `apply_damage_2d` — tail-cell hit redirects ALL writes (target-lock,
//!     shield pool, hull, destroy) to the PRIMARY slot, gated on
//!     `tail.is_some()`. Shield ZONE keys off the actual impact cell (a tail
//!     flank reads as a flank).
//!   - `resolve_targeting_2d` — dedupes the returned `Vec<Pos>` by ship id, so
//!     an AoE/splash that covers both cells of a Pair boss emits ONE entry.
//!   - `destroy` — clears BOTH primary + tail slots on a Pair kill.
//!   - `resolve_target_move_2d` (push/pull/swap) — bails on `tail.is_some()`
//!     at the entry guard; a Pair boss is immovable.
//!   - `Board::find_pos_by_id` — returns the PRIMARY pos for a Pair boss
//!     (primary-slot guard via `slot_pos == ship.pos`).
//!   - `Board::ship_id_at` — returns the boss id on either primary OR tail
//!     (delegates through `ship_at` which sees the mirror clone).
//!
//! Each test seats a fresh Pair boss on a 5×4 board with the player at
//! `(2, 3)` (front-centre, default `player_start_pos_in`). Boss primary at
//! `(0, 0)` + tail at `(1, 0)` → footprint = `[(0,0), (1,0)]`. Driving the
//! live entry points (no shortcut into internal state) so a future refactor
//! that breaks the contract surfaces in exactly the failing case.

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::grid::{Dir4, Facing, Pos};
use broadside_engine::resolve::{apply_damage_2d, apply_effect, resolve_targeting_2d, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, DisplaceMode, Effect, EventBus, Faction, LaneEnd, Mount,
    Orientation, Projectile, RangeBand, Ship, Targeting, TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

/// Build the player ship at `pos` facing N (bow up-lane). The boss tests
/// don't fire FROM the player; the player is just there to keep the board
/// well-formed (1 player + 1 boss), so the mounts/shields are minimal.
fn player_at(pos: Pos) -> Ship {
    Ship {
        id: "player".into(),
        faction: Faction::Player,
        cell: pos.to_index(),
        pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: Facing::Bow(Dir4::N),
        hull: 10,
        max_hull: 10,
        heat: 0,
        heat_max: 12,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: vec![Mount {
            id: "p-m1".into(),
            arc: Arc::Forward,
            weapon: "_boss_probe".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

/// Build a Pair boss whose PRIMARY is `primary` and TAIL is `tail`. Naked
/// shields so the damage tests don't have to fight the soak pool — every
/// point of damage that survives target-lock lands on hull directly. Both
/// `primary` and `tail` are populated on the board by [`seat_boss`].
fn pair_boss(primary: Pos, tail: Pos, hull: i32) -> Ship {
    Ship {
        id: "boss".into(),
        faction: Faction::Enemy,
        cell: primary.to_index(),
        pos: primary,
        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
        facing: Facing::Bow(Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        // Naked shields so post-redirect damage lands on hull (zone-keyed
        // soak with 0/0 face is a pass-through).
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
        mounts: Vec::new(),
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: Some(tail),
    }
}

/// Build a fresh 5×4 board with the player at front-centre `(2, 3)` and a
/// Pair boss at `boss_primary` / `boss_tail`. Seats the SAME Ship clone in
/// both slots — that's the architect's contract: `cells[primary] = Some(boss)`
/// AND `cells[tail] = Some(boss.clone())`. `find_pos_by_id` walks back to
/// the primary by checking `slot_pos == ship.pos`.
fn seat_boss(boss_primary: Pos, boss_tail: Pos, hull: i32) -> Board {
    let dims = broadside_engine::grid::Dims::default();
    let n = dims.cell_count();
    let mut cells: Vec<Option<Ship>> = (0..n).map(|_| None).collect();
    let player_pos = Pos::new(2, 3);
    cells[player_pos.to_index_in(dims)] = Some(player_at(player_pos));
    let boss = pair_boss(boss_primary, boss_tail, hull);
    cells[boss_primary.to_index_in(dims)] = Some(boss.clone());
    cells[boss_tail.to_index_in(dims)] = Some(boss); // tail mirror clone
    Board {
        size: n,
        cols: dims.cols,
        rows: dims.rows,
        cells,
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

/// A minimal `Content` for the resolver — every test action is built inline,
/// no catalog lookup, no projectile spawn.
struct NoContent;
impl Content for NoContent {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _kind: &str, _owner: &Ship) -> Projectile {
        panic!("boss_2cell tests don't fire ordnance");
    }
}

/// A direct-damage Action with falloff DISABLED so the raw `amount` lands
/// as-is (zone-keyed shield soak still applies; naked shields are 0/0 so
/// it's a pass-through). Used by `apply_damage_2d` calls in the damage
/// tests so the expected hull delta is exactly `raw`.
fn direct_damage_action(raw_amount: i32) -> Action {
    Action {
        id: "_boss_probe".into(),
        name: "Probe".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
                RangeBand::Extreme,
            ],
            optimal_band: RangeBand::Mid,
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
                broadside_engine::grid::Range::Far,
            ],
            optimal_range: broadside_engine::grid::Range::Near,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE {
            amount: raw_amount,
            band_falloff: Some(false),
        }],
        r#mod: None,
        icon: None,
    }
}

/// An area-of-effect `BLAST` action — `hits_all` true so the resolver's
/// `resolve_targeting_2d` produces multiple cells covering the blast
/// footprint. Used by case (b) to verify the id-dedup in
/// `resolve_targeting_2d`.
fn blast2_action() -> Action {
    Action {
        id: "_boss_blast".into(),
        name: "Blast".into(),
        archetype: WeaponArchetype::Ordnance,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            pattern: TargetingPattern::BLAST,
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
                RangeBand::Extreme,
            ],
            optimal_band: RangeBand::Mid,
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
                broadside_engine::grid::Range::Far,
            ],
            optimal_range: broadside_engine::grid::Range::Far,
            requires_arc: None,
            facing_relative: false,
            hits_all: true,
        },
        effects: vec![Effect::DAMAGE {
            amount: 2,
            band_falloff: Some(false),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Resolve `Effect::DISPLACE_TARGET(mode, dist)` against the boss via the
/// live `apply_effect` path. `cells` is the resolved-targeting cell list the
/// resolver hands `apply_effect`; we pass `boss_pos.to_index()` directly so
/// the test exercises the same code path the round runner would.
fn try_displace_boss(
    board: &mut Board,
    attacker_pos: Pos,
    boss_pos: Pos,
    mode: DisplaceMode,
    distance: i32,
) {
    let action = Action {
        id: "_boss_shove".into(),
        name: "Shove".into(),
        archetype: WeaponArchetype::Displacement,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            pattern: TargetingPattern::SELF, // shape irrelevant; we hand-supply the cell list
            band: vec![RangeBand::PointBlank, RangeBand::Close],
            optimal_band: RangeBand::PointBlank,
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
            ],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DISPLACE_TARGET { mode, distance }],
        r#mod: None,
        icon: None,
    };
    let fx = action.effects[0].clone();
    let dims = board.dims();
    apply_effect(
        &fx,
        &action,
        attacker_pos.to_index_in(dims),
        &[boss_pos.to_index_in(dims)],
        board,
        &NoContent,
    );
}

/* =========================================================================
 * (a) Tail-cell shot damages shared HP
 * ====================================================================== */

#[test]
fn tail_cell_shot_damages_shared_hp_and_keeps_both_slots() {
    let primary = Pos::new(0, 0);
    let tail = Pos::new(1, 0);
    let mut board = seat_boss(primary, tail, 10);
    let attacker_pos = Pos::new(2, 3); // player at front-centre
    let action = direct_damage_action(3);

    apply_damage_2d(tail, 3, attacker_pos, &action, &mut board, &NoContent);

    // Boss is still alive on BOTH slots, hull dropped by 3 (naked shield
    // 0/0 → pass-through). `ship_id_at` returns the boss id on either cell.
    assert_eq!(
        board.ship_id_at(primary),
        Some("boss"),
        "primary still occupied after tail hit",
    );
    assert_eq!(
        board.ship_id_at(tail),
        Some("boss"),
        "tail mirror still occupied after tail hit",
    );
    assert_eq!(
        board.ship_at(primary).unwrap().hull,
        10 - 3,
        "tail-cell hit damages the shared (primary) HP pool",
    );
    // The tail mirror should reflect the post-hit hull (refresh_tail_mirror).
    assert_eq!(
        board.ship_at(tail).unwrap().hull,
        10 - 3,
        "tail mirror refreshed with post-hit hull",
    );
}

/* =========================================================================
 * (b) Splash hitting BOTH cells damages once (id-dedup in
 *     resolve_targeting_2d)
 * ====================================================================== */

#[test]
fn splash_hitting_both_cells_damages_boss_once() {
    // BLAST_2 from the player's centre, far enough that the splash from a
    // chosen anchor cell would naturally cover BOTH boss cells if the
    // resolver weren't deduping. The architect's resolve_targeting_2d
    // dedupes by ship id, so each apply_damage_2d call counts the boss
    // exactly once.
    //
    // Drive the live `resolve_targeting_2d` directly + count how many
    // returned cells map to the boss id; the dedup guarantees ≤ 1.
    let primary = Pos::new(0, 0);
    let tail = Pos::new(1, 0);
    let board = seat_boss(primary, tail, 10);
    let attacker_pos = Pos::new(2, 3);
    let action = blast2_action();

    let cells = resolve_targeting_2d(&action, &board, attacker_pos);
    // Count how many returned cells RESOLVE to the boss. The architect's
    // dedup-by-id guarantees the boss appears at most ONCE in the returned
    // Vec — even if the splash anchor's BLAST_2 footprint covers both
    // primary and tail.
    let boss_hits = cells
        .iter()
        .filter(|&&p| board.ship_id_at(p) == Some("boss"))
        .count();
    assert!(
        boss_hits <= 1,
        "resolve_targeting_2d must dedup the Pair boss to a single entry; got {boss_hits} cells \
         resolving to the boss out of {cells:?}",
    );
}

/* =========================================================================
 * (c) Move onto either boss cell is blocked
 * ====================================================================== */

#[test]
fn move_onto_either_boss_cell_is_blocked() {
    // The architect's move scan reads `board.ship_at(next).is_some()`, which
    // returns Some for BOTH primary and tail slots (the mirror clone is a
    // populated cell). So any THRUST/BURN attempt to step onto either of
    // the boss's two cells must NOT relocate the player.
    //
    // We drive this through `apply_effect` with a player THRUST whose target
    // is the boss's tail cell; the move path's `board.ship_at(next).is_some()`
    // gate must reject it.
    let primary = Pos::new(0, 0);
    let tail = Pos::new(1, 0);
    let mut board = seat_boss(primary, tail, 10);
    // Move the player adjacent to the boss tail so a single N step targets
    // the tail cell. Player at (1, 1), facing N → THRUST 1 lands at (1, 0)
    // = tail. The boss occupancy must block it.
    let dims = board.dims();
    let player_start = Pos::new(1, 1);
    let player_idx = player_start.to_index_in(dims);
    // Re-seat the player from front-centre to (1, 1).
    let player_front_idx = Pos::new(2, 3).to_index_in(dims);
    let mut p = board.cells[player_front_idx]
        .take()
        .expect("player at front");
    p.pos = player_start;
    p.cell = player_idx;
    p.facing = Facing::Bow(Dir4::N);
    board.cells[player_idx] = Some(p);

    // THRUST 1 N — by the resolver's contract, blocked by the tail-cell
    // occupant.
    let thrust = Action {
        id: "_boss_step".into(),
        name: "Step".into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost {
            heat: 0,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            range_band: vec![broadside_engine::grid::Range::Adjacent],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DISPLACE_SELF {
            mode: broadside_engine::types::MovementMode::THRUST,
            distance: 1,
            direction: None,
            direction_2d: Some(Dir4::N),
        }],
        r#mod: None,
        icon: None,
    };
    let fx = thrust.effects[0].clone();
    apply_effect(
        &fx,
        &thrust,
        player_idx,
        &[player_idx], // DISPLACE_SELF: cells list is the source itself
        &mut board,
        &NoContent,
    );

    // Player should NOT have moved into the tail cell.
    assert!(
        board.ship_at(tail).map(|s| s.id.as_str()) == Some("boss"),
        "tail cell still occupied by the boss (player did not steal it)",
    );
    assert!(
        board.ship_at(player_start).map(|s| s.id.as_str()) == Some("player"),
        "player held position (blocked by tail occupant)",
    );

    // Repeat for the primary cell: move the player to (0, 1) and try a N
    // thrust → target = (0, 0) = primary; also blocked.
    let mut board2 = seat_boss(primary, tail, 10);
    let dims2 = board2.dims();
    let player_start2 = Pos::new(0, 1);
    let pi2 = player_start2.to_index_in(dims2);
    let pf2 = Pos::new(2, 3).to_index_in(dims2);
    let mut p2 = board2.cells[pf2].take().expect("player at front");
    p2.pos = player_start2;
    p2.cell = pi2;
    p2.facing = Facing::Bow(Dir4::N);
    board2.cells[pi2] = Some(p2);

    let fx2 = thrust.effects[0].clone();
    apply_effect(&fx2, &thrust, pi2, &[pi2], &mut board2, &NoContent);
    assert!(
        board2.ship_at(primary).map(|s| s.id.as_str()) == Some("boss"),
        "primary cell still occupied by the boss",
    );
    assert!(
        board2.ship_at(player_start2).map(|s| s.id.as_str()) == Some("player"),
        "player held position (blocked by primary occupant)",
    );
}

/* =========================================================================
 * (d) Push / Pull / Swap on the boss is a no-op
 * ====================================================================== */

#[test]
fn push_pull_swap_on_pair_boss_is_a_noop() {
    // The architect's resolve_target_move_2d bails on `tail.is_some()` at
    // entry — Pair bosses are immovable in every mode. Verify all three.
    for mode in [DisplaceMode::Push, DisplaceMode::Pull, DisplaceMode::Swap] {
        let primary = Pos::new(0, 0);
        let tail = Pos::new(1, 0);
        let mut board = seat_boss(primary, tail, 10);
        let attacker_pos = Pos::new(2, 3);

        try_displace_boss(&mut board, attacker_pos, primary, mode, 2);

        // Boss must still occupy BOTH slots, primary `pos` unchanged.
        assert_eq!(
            board.ship_id_at(primary),
            Some("boss"),
            "{mode:?}: primary still the boss (immovable Pair)",
        );
        assert_eq!(
            board.ship_id_at(tail),
            Some("boss"),
            "{mode:?}: tail still the boss (immovable Pair)",
        );
        let boss_after = board.ship_at(primary).unwrap();
        assert_eq!(
            boss_after.pos, primary,
            "{mode:?}: boss.pos unchanged after displace attempt",
        );
        assert_eq!(boss_after.tail, Some(tail), "{mode:?}: boss.tail unchanged",);
        // Swap-specific: the attacker (player at (2,3)) must NOT have
        // teleported into the boss's slot.
        if mode == DisplaceMode::Swap {
            assert_eq!(
                board.ship_id_at(attacker_pos),
                Some("player"),
                "Swap: player stayed at attacker_pos (no swap with immovable boss)",
            );
        }
    }
}

/* =========================================================================
 * (e) Primary destroy clears BOTH slots
 * ====================================================================== */

#[test]
fn primary_destroy_clears_both_slots() {
    // Run damage that kills the boss (hull → 0 via tail-cell hit). The
    // architect's destroy() walks `ship.tail` and clears the mirror slot,
    // leaving NO orphan tail entry behind. Drive through apply_damage_2d
    // so we exercise the live damage → destroy chain.
    let primary = Pos::new(0, 0);
    let tail = Pos::new(1, 0);
    let mut board = seat_boss(primary, tail, 3);
    let attacker_pos = Pos::new(2, 3);
    let lethal = direct_damage_action(5); // 5 > hull 3 → kill

    apply_damage_2d(tail, 5, attacker_pos, &lethal, &mut board, &NoContent);

    // BOTH slots cleared (no orphan tail mirror).
    assert!(
        board.ship_at(primary).is_none(),
        "primary slot cleared after lethal hit",
    );
    assert!(
        board.ship_at(tail).is_none(),
        "tail mirror cleared after lethal hit (no orphan slot)",
    );
    // find_pos_by_id returns None for a destroyed boss.
    assert_eq!(
        board.find_pos_by_id("boss"),
        None,
        "destroyed boss yields no primary",
    );
}

/* =========================================================================
 * (f) find_pos_by_id returns PRIMARY (never tail)
 * ====================================================================== */

#[test]
fn find_pos_by_id_returns_primary_not_tail() {
    let primary = Pos::new(0, 0);
    let tail = Pos::new(1, 0);
    let board = seat_boss(primary, tail, 10);
    // The architect's `find_pos_by_id` guards on `slot_pos == ship.pos`:
    // the tail-mirror clone has `pos == primary` (≠ tail), so the lookup
    // returns the primary slot's Pos.
    assert_eq!(
        board.find_pos_by_id("boss"),
        Some(primary),
        "find_pos_by_id resolves a Pair boss to its PRIMARY slot",
    );
    assert_ne!(
        board.find_pos_by_id("boss"),
        Some(tail),
        "find_pos_by_id must never return the tail slot",
    );
    // The player (single-cell) still works the same way.
    assert_eq!(
        board.find_pos_by_id("player"),
        Some(Pos::new(2, 3)),
        "single-cell ship lookup unchanged",
    );
}
