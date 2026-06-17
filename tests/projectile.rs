//! Ordnance / projectile integration tests.
//!
//! Exercises `advance_projectile` (and the `run_world_phase` ordnance pass
//! that drives it) against the canonical TS `advanceProjectile` mechanics:
//!
//! 1. **Advance by speed** — a projectile steps `speed` cells per round, one
//!    cell at a time, in its `heading` direction.
//! 2. **Off-board removal** — a projectile that steps past either lane end
//!    is removed (no panic, no wraparound).
//! 3. **Impact on a non-owner occupant** — when a step lands on a cell holding
//!    a ship of a DIFFERENT faction, the payload (`DAMAGE` / `APPLY_STATUS`)
//!    is applied at that cell and the projectile is consumed.
//! 4. **Owner pass-through** — a projectile does NOT impact a same-faction
//!    ship; it passes over and keeps travelling.
//! 5. **Mid-flight impact within one multi-speed advance** — a speed-2
//!    projectile that reaches a target on its first sub-step impacts there and
//!    does not over-travel.
//! 6. **SPAWN_ORDNANCE launch** — firing a launcher action through the full
//!    `resolve_round` spawns a projectile via `Content::spawn_projectile` and
//!    puts it on `board.ordnance`.
//!
//! ## Damage model on impact (so the expected numbers are non-magic)
//!
//! `advance_projectile` applies each `DAMAGE` payload via
//! `apply_damage(impact_cell, amount, impact_cell, &dummy_weapon(), …)` —
//! source cell == target cell. `dummy_weapon()`'s damage effect carries
//! `bandFalloff: false`, and the resolver's falloff predicate keys off the
//! *weapon's* effects, so **impact damage never gets band falloff** regardless
//! of the payload's own flag: the payload `amount` lands raw, then the
//! target's directional shield (the zone facing the incoming direction)
//! subtracts armour / consumes a charge.
//!
//! Because source == target, `direction_to(impact_cell, impact_cell)` is
//! `Fore` (the `b >= a` branch), so the hit lands on the zone that faces the
//! Fore lane-end: BOW for a `bow=Fore` ship, STERN for a `bow=Aft` ship. The
//! fixtures below pick orientations to route impacts onto an armour-0 face so
//! the arithmetic is `hull -= amount`, keeping the assertions legible.

use broadside_engine::resolve::{advance_projectile, resolve_round, run_world_phase, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, LaneEnd, Mount, Orientation,
    Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Status, StatusKind, Targeting,
    TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

/// A ship with an armour-0 STERN (the canonical soft underbelly) and an
/// armour-2 BOW. `bow` decides which face an impact (which always arrives
/// from the Fore direction — see module doc) lands on.
fn target_ship(id: &str, cell: usize, hull: i32, bow: LaneEnd) -> Ship {
    Ship {
        id: id.into(),
        faction: Faction::Enemy,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
            bow: ShieldFace { armour: 2, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 0 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        },
        mounts: Vec::new(),
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// A bare projectile heading `heading` from `cell`, dealing `dmg` raw on
/// impact (falloff bypassed by the impact path), `speed` cells/round, owned
/// by `owner`.
fn projectile(id: &str, cell: usize, heading: LaneEnd, speed: u32, dmg: i32, owner: Faction) -> Projectile {
    Projectile {
        id: id.into(),
        kind: "torpedo".into(),
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        heading,
        heading8: broadside_engine::grid::Dir8::N,
        speed,
        hull: 2,
        payload: vec![Effect::DAMAGE { amount: dmg, band_falloff: Some(false) }],
        owner_faction: owner,
    }
}

/// A 2-D projectile at a real `pos` heading `heading8` (the field the live
/// `advance_projectile_2d` / R5 reads, alongside `pos`). `cell` mirrors
/// `pos.to_index()` (invariant A). Use this for the live-path tests
/// (`run_world_phase` / `resolve_round`); the bare `projectile()` above drives
/// the still-1-D `advance_projectile` primitive directly.
fn projectile_2d(
    id: &str,
    pos: broadside_engine::grid::Pos,
    heading8: broadside_engine::grid::Dir8,
    speed: u32,
    dmg: i32,
    owner: Faction,
) -> Projectile {
    Projectile {
        id: id.into(),
        kind: "torpedo".into(),
        cell: pos.to_index(),
        pos,
        // 1-D `heading` is the dead mirror; the live path reads `heading8`.
        heading: LaneEnd::Fore,
        heading8,
        speed,
        hull: 2,
        payload: vec![Effect::DAMAGE { amount: dmg, band_falloff: Some(false) }],
        owner_faction: owner,
    }
}

/// A board with the given ships and ordnance, sized `size`.
fn board(size: usize, cells: Vec<Option<Ship>>, ordnance: Vec<Projectile>) -> Board {
    Board {
        size,
        cells,
        ordnance,
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

/// Content that returns no actions and spawns a fixed torpedo on demand —
/// for the SPAWN_ORDNANCE launch test.
struct OrdContent {
    launcher: Action,
}
impl Content for OrdContent {
    fn action(&self, id: &str) -> Option<&Action> {
        (id == "launch_torpedo").then_some(&self.launcher)
    }
    fn spawn_projectile(&self, kind: &str, owner: &Ship) -> Projectile {
        // 2-D spawn: the torpedo starts on the owner's cell and heads along the
        // owner's bow cardinal (heading8), so the live advance_projectile_2d
        // (R5) steps it correctly. Mirrors the real catalog spawn shape (#42).
        let heading8 = match owner.facing {
            broadside_engine::grid::Facing::Bow(d) => d.to_dir8(),
            broadside_engine::grid::Facing::Broadside(axis) => axis.dirs().0.to_dir8(),
        };
        Projectile {
            id: format!("{}:{}", owner.id, kind),
            kind: kind.into(),
            cell: owner.pos.to_index(),
            pos: owner.pos,
            heading: LaneEnd::Fore,
            heading8,
            speed: 1,
            hull: 2,
            payload: vec![Effect::DAMAGE { amount: 3, band_falloff: Some(false) }],
            owner_faction: owner.faction,
        }
    }
}

/// Content with nothing — for the pure advance_projectile tests that never
/// look up an action or spawn ordnance.
struct NoContent;
impl Content for NoContent {
    fn action(&self, _: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        unreachable!("advance-only tests never spawn ordnance");
    }
}

/* =========================================================================
 * 1. Advance by speed.
 * ====================================================================== */

#[test]
fn projectile_advances_one_cell_per_speed_when_lane_is_clear() {
    // Speed-1 torpedo at cell 1 heading Fore on an empty 7-cell lane.
    let mut b = board(7, vec![None; 7], vec![projectile("t", 1, LaneEnd::Fore, 1, 3, Faction::Player)]);
    advance_projectile("t", &mut b, &NoContent);
    assert_eq!(b.ordnance.len(), 1, "still in flight over empty cells");
    assert_eq!(b.ordnance[0].cell, 2, "stepped exactly one cell Fore");
}

#[test]
fn speed_two_projectile_advances_two_cells_in_one_pass() {
    let mut b = board(7, vec![None; 7], vec![projectile("t", 1, LaneEnd::Fore, 2, 3, Faction::Player)]);
    advance_projectile("t", &mut b, &NoContent);
    assert_eq!(b.ordnance[0].cell, 3, "speed 2 = two cells per advance");
}

#[test]
fn aft_heading_projectile_steps_toward_lower_cells() {
    let mut b = board(7, vec![None; 7], vec![projectile("t", 5, LaneEnd::Aft, 1, 3, Faction::Enemy)]);
    advance_projectile("t", &mut b, &NoContent);
    assert_eq!(b.ordnance[0].cell, 4, "Aft heading decrements the cell");
}

/* =========================================================================
 * 2. Off-board removal.
 * ====================================================================== */

#[test]
fn projectile_stepping_past_the_fore_end_is_removed() {
    // At the last cell heading Fore: the next step overflows the lane.
    let mut b = board(7, vec![None; 7], vec![projectile("t", 6, LaneEnd::Fore, 1, 3, Faction::Player)]);
    advance_projectile("t", &mut b, &NoContent);
    assert!(b.ordnance.is_empty(), "ran off the fore end and was removed");
}

#[test]
fn projectile_stepping_past_the_aft_end_is_removed() {
    // At cell 0 heading Aft: the next step underflows (checked_sub -> None).
    let mut b = board(7, vec![None; 7], vec![projectile("t", 0, LaneEnd::Aft, 1, 3, Faction::Enemy)]);
    advance_projectile("t", &mut b, &NoContent);
    assert!(b.ordnance.is_empty(), "ran off the aft end and was removed");
}

/* =========================================================================
 * 3. Impact on a non-owner occupant.
 * ====================================================================== */

#[test]
fn projectile_impacts_enemy_occupant_and_applies_damage() {
    // Player torpedo at cell 1 heading Fore; an enemy at cell 2 with its soft
    // stern facing the Fore-arriving hit (bow=Aft -> stern faces Fore).
    let enemy = target_ship("e", 2, 6, LaneEnd::Aft);
    let mut b = board(
        7,
        vec![None, None, Some(enemy), None, None, None, None],
        vec![projectile("t", 1, LaneEnd::Fore, 1, 3, Faction::Player)],
    );
    advance_projectile("t", &mut b, &NoContent);

    assert!(b.ordnance.is_empty(), "projectile consumed on impact");
    assert_eq!(
        b.cells[2].as_ref().expect("enemy survives 3 dmg of 6 hull").hull,
        3,
        "3 raw payload damage onto the armour-0 stern: 6 - 3 = 3",
    );
}

#[test]
fn impact_on_strong_bow_is_reduced_by_armour() {
    // Same shot, but the enemy faces bow=Fore so the armour-2 BOW eats the
    // Fore-arriving hit: 3 raw - 2 armour = 1 lands.
    let enemy = target_ship("e", 2, 6, LaneEnd::Fore);
    let mut b = board(
        7,
        vec![None, None, Some(enemy), None, None, None, None],
        vec![projectile("t", 1, LaneEnd::Fore, 1, 3, Faction::Player)],
    );
    advance_projectile("t", &mut b, &NoContent);

    assert_eq!(
        b.cells[2].as_ref().expect("enemy survives").hull,
        5,
        "bow armour 2 soaks 2 of the 3 raw: 6 - 1 = 5",
    );
}

#[test]
fn projectile_applies_status_payload_on_impact() {
    let enemy = target_ship("e", 2, 6, LaneEnd::Aft);
    let mut proj = projectile("t", 1, LaneEnd::Fore, 1, 0, Faction::Player);
    proj.payload = vec![Effect::APPLY_STATUS { status: StatusKind::HullBreach, duration: 3 }];
    let mut b = board(
        7,
        vec![None, None, Some(enemy), None, None, None, None],
        vec![proj],
    );
    advance_projectile("t", &mut b, &NoContent);

    assert!(b.ordnance.is_empty(), "projectile consumed on impact");
    let statuses = &b.cells[2].as_ref().expect("enemy alive").statuses;
    assert_eq!(
        statuses,
        &vec![Status { kind: StatusKind::HullBreach, duration: 3, face: None }],
        "the APPLY_STATUS payload landed as a HullBreach(3) on the target",
    );
}

/* =========================================================================
 * 4. Owner pass-through.
 * ====================================================================== */

#[test]
fn projectile_passes_over_its_own_faction_without_impacting() {
    // Player torpedo at cell 1 heading Fore; a PLAYER ship sits at cell 2.
    // Same-faction => no impact; the torpedo keeps flying to cell 2 and
    // remains live.
    let ally = target_ship("ally", 2, 6, LaneEnd::Aft);
    let ally = Ship { faction: Faction::Player, ..ally };
    let mut b = board(
        7,
        vec![None, None, Some(ally), None, None, None, None],
        vec![projectile("t", 1, LaneEnd::Fore, 1, 3, Faction::Player)],
    );
    advance_projectile("t", &mut b, &NoContent);

    assert_eq!(b.ordnance.len(), 1, "owner-faction ship is not impacted");
    assert_eq!(b.ordnance[0].cell, 2, "torpedo flew onto the ally's cell, still live");
    assert_eq!(
        b.cells[2].as_ref().expect("ally untouched").hull,
        6,
        "ally took no damage from its own side's ordnance",
    );
}

/* =========================================================================
 * 5. Mid-flight impact within one multi-speed advance.
 * ====================================================================== */

#[test]
fn speed_two_projectile_impacts_on_its_first_substep_and_stops() {
    // Speed-2 torpedo at cell 1; enemy at cell 2 (one step away). It must
    // impact at cell 2 on the first sub-step and NOT travel to cell 3.
    let enemy = target_ship("e", 2, 6, LaneEnd::Aft);
    let other = target_ship("e2", 3, 6, LaneEnd::Aft);
    let mut b = board(
        7,
        vec![None, None, Some(enemy), Some(other), None, None, None],
        vec![projectile("t", 1, LaneEnd::Fore, 2, 3, Faction::Player)],
    );
    advance_projectile("t", &mut b, &NoContent);

    assert!(b.ordnance.is_empty(), "consumed at the first occupant");
    assert_eq!(
        b.cells[2].as_ref().expect("first enemy hit").hull,
        3,
        "impacted the cell-2 enemy (6 - 3 = 3), did not skip to cell 3",
    );
    assert_eq!(
        b.cells[3].as_ref().expect("second enemy untouched").hull,
        6,
        "the cell-3 ship is untouched — a speed-2 torpedo stops at the first target",
    );
}

/* =========================================================================
 * 6. SPAWN_ORDNANCE launch through the full round.
 * ====================================================================== */

/// A forward launcher action that spawns a torpedo on fire.
fn launcher_action() -> Action {
    Action {
        id: "launch_torpedo".into(),
        name: "Launch Torpedo".into(),
        archetype: WeaponArchetype::Ordnance,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::ORDNANCE,
            band: vec![RangeBand::Close, RangeBand::Mid, RangeBand::Long],
            optimal_band: RangeBand::Mid,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::SPAWN_ORDNANCE { projectile: "torpedo".into() }],
        r#mod: None,
        icon: None,
    }
}

// #22 RESTORED: the launcher fires through the live 2-D path (resolve_round ->
// fire_player_queue -> SPAWN_ORDNANCE) and the spawned torpedo advances through
// the 2-D ordnance phase (advance_projectile_2d, R5). Player at (2,3) Bow(N): its
// Forward ORDNANCE arc bears one step N (the spawn cell (2,2), in-bounds), so the
// launcher fires; the torpedo spawns at the player's cell (2,3) heading N and the
// same round's ordnance phase steps it one cell N to (2,2). Enemy at (2,0) is
// dressing (the spawn doesn't depend on it).
#[test]
fn firing_a_launcher_spawns_a_projectile_on_the_board() {
    use broadside_engine::grid::{Dir4, Facing, Pos};
    let player = {
        let mut p = target_ship("player", Pos::new(2, 3).to_index(), 12, LaneEnd::Fore);
        p.faction = Faction::Player;
        p.pos = Pos::new(2, 3);
        p.facing = Facing::Bow(Dir4::N);
        p.mounts = vec![Mount { id: "m1".into(), arc: Arc::Forward, weapon: "launch_torpedo".into() }];
        p.queue = vec!["launch_torpedo".into()];
        p
    };
    let enemy = {
        let mut e = target_ship("e", Pos::new(2, 0).to_index(), 6, LaneEnd::Aft);
        e.pos = Pos::new(2, 0);
        e.facing = Facing::Bow(Dir4::S);
        e
    };
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    let (pi, ei) = (player.pos.to_index(), enemy.pos.to_index());
    cells[pi] = Some(player);
    cells[ei] = Some(enemy);
    let mut b = board(broadside_engine::grid::COLS, cells, vec![]);
    let content = OrdContent { launcher: launcher_action() };

    // Snapshot ordnance count before firing.
    assert_eq!(b.ordnance.len(), 0, "no ordnance before the launch");

    resolve_round(&mut b, &content);

    // The spawned torpedo started at the player's cell (2,3) heading N at speed
    // 1; one ordnance-phase advance steps it N to (2,2). So after the round it
    // exists and has moved.
    assert_eq!(b.ordnance.len(), 1, "the launcher spawned exactly one projectile");
    assert_eq!(b.ordnance[0].kind, "torpedo");
    assert_eq!(b.ordnance[0].owner_faction, Faction::Player);
    assert_eq!(
        b.ordnance[0].pos, Pos::new(2, 2),
        "spawned at the player's cell (2,3), advanced one cell N to (2,2) in the same round's ordnance phase",
    );
}

/* =========================================================================
 * 7. World-phase ordnance pass drives advance for every live projectile.
 * ====================================================================== */

// #22 RESTORED: the live ordnance phase (run_world_phase -> advance_projectile_2d,
// R5) steps each projectile along its `heading8` over the 2-D grid. Two
// independent projectiles on different rows so they don't interact:
//   "a" at (1,0) heading E -> (2,0) [index 2]; "z" at (3,2) heading W -> (2,2)
//   [index 12]. Empty board (no occupants) so neither impacts.
#[test]
fn world_phase_advances_all_live_projectiles() {
    use broadside_engine::grid::{Dir8, Pos};
    let mut b = board(
        broadside_engine::grid::COLS,
        vec![None; broadside_engine::grid::CELLS],
        vec![
            projectile_2d("a", Pos::new(1, 0), Dir8::E, 1, 3, Faction::Player),
            projectile_2d("z", Pos::new(3, 2), Dir8::W, 1, 3, Faction::Enemy),
        ],
    );
    run_world_phase(&mut b, &NoContent);

    let pos_of = |id: &str| b.ordnance.iter().find(|p| p.id == id).map(|p| p.pos);
    assert_eq!(pos_of("a"), Some(Pos::new(2, 0)), "E-heading projectile advanced one cell E");
    assert_eq!(pos_of("z"), Some(Pos::new(2, 2)), "W-heading projectile advanced one cell W");
}
