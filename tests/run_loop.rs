//! End-to-end run-loop integration tests.
//!
//! The unit tests inside `src/runs.rs` and `src/meta.rs` exercise the
//! run-advancement and salvage functions in isolation against hand-set
//! `Run` structs. This file proves the same machinery holds when you
//! drive *real* [`Board`]s through `resolve_round`: it plays a whole
//! campaign — materialize a board from an [`EncounterDef`], fire the
//! player's queue until the enemies are dead, observe the outcome with
//! [`encounter_outcome`], advance the run with [`advance_after_win`],
//! rebuild the next board around the carried-forward player Ship, and
//! repeat until [`AdvanceResult::Victorious`].
//!
//! Three behaviours that no single-module unit test can claim:
//!
//! 1. **Played-through victory.** The `Victorious` flag is set by a run
//!    the resolver actually won, not by `run.victorious = true` in the
//!    test setup. If `encounter_outcome` and `advance_after_win`
//!    disagree about when a sector is cleared, this test breaks.
//! 2. **Played-through defeat.** A real board where the player ship is
//!    destroyed routes through `mark_defeated`. If `encounter_outcome`
//!    stopped returning `Lost` when the player dies, the run would hang
//!    "InProgress" forever and this test would never call `mark_defeated`.
//! 3. **Salvage → meta accrual across a real win.** Enemies die on a
//!    live board; `salvage_for_encounter_win` reads the spawn list,
//!    `award_run_salvage` banks it, `accumulate_into_meta` crosses an
//!    unlock threshold. The integration catches a drift between "who the
//!    encounter fielded" (spawn list) and "what salvage they were worth."
//!
//! Plus a headless smoke test: `build_encounter_board` + one
//! `resolve_round` must not panic. That is the logic-layer guard for the
//! class of failure the wgpu render path hit — it runs in CI with no GPU.

use broadside_engine::meta::{
    accumulate_into_meta, award_run_salvage, award_run_salvage_with_catalog,
    salvage_for_encounter_win, MetaProgression, SUBSYSTEM_UNLOCK_THRESHOLDS,
};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::runs::{
    advance_after_win, build_encounter_board, current_encounter, encounter_outcome,
    generate_campaign, mark_defeated, AdvanceResult, EncounterOutcome,
};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, EncounterDef, Effect, Faction, LaneEnd, Mount, MovementMode,
    Orientation, Projectile, RangeBand, ReorientTo, Run, Sector, ShieldFace, ShieldProfile, Ship,
    ShipSpawn, Targeting, TargetingPattern, WeaponArchetype,
};
use broadside_engine::grid::{Dir4, Facing, Pos};
use std::collections::HashMap;

/// Shared 2-D invariant-A fixture builders (used by the #25 2-D kill probe; the
/// rest of this file is mid-migration on the 1-D harness — tracks #22/#25).
mod common;

/* =========================================================================
 * Fixtures — small, reusable ship + content builders.
 * ====================================================================== */

/// A player frigate at `cell` with `hull`, bow facing forward (`Fore`).
/// Carries one forward-arc mount loaded with the test "siege_beam"
/// weapon. Heat budget generous so the loop never lockouts.
fn player_frigate(cell: usize, hull: i32) -> Ship {
    Ship {
        id: "player".into(),
        faction: Faction::Player,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 12,
        locked_out: false,
        shield_profile: ShieldProfile {
            bow: ShieldFace { armour: 2, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 0 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        },
        mounts: vec![Mount {
            id: "m1".into(),
            arc: Arc::Forward,
            weapon: "siege_beam".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// A weak enemy hull at `cell`, bow=`Fore` so its soft *stern* faces an
/// attacker sitting at a lower cell. `hull` lets the test tune how many
/// shots a kill takes. No mounts / no AI threat — these are targets, so
/// the playthrough stays deterministic and we test the *loop*, not the AI
/// (which has its own suite in `resolve.rs`).
fn weak_enemy(id: &str, cell: usize, hull: i32) -> Ship {
    Ship {
        id: id.into(),
        faction: Faction::Enemy,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
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

/// An armed enemy that *will* shoot back, used in the defeat path. It
/// carries a forward beam and faces the player (bow=Aft so the bow points
/// down-lane toward cell 0, putting the player in its Forward arc). Hull
/// is deliberately high so it survives long enough for the world phase to
/// let it return fire — the defeat path is about the *board* killing the
/// player, not the player whiffing.
fn armed_enemy(id: &str, cell: usize, hull: i32) -> Ship {
    let mut e = weak_enemy(id, cell, hull);
    e.orientation = Orientation::BowOn { bow: LaneEnd::Aft };
    e.mounts = vec![Mount {
        id: "e1".into(),
        arc: Arc::Forward,
        weapon: "siege_beam".into(),
    }];
    e
}

/// The test weapon: a heavy forward beam. 6 raw damage, optimal at
/// PointBlank so adjacent shots land full, fires PointBlank/Close/Mid,
/// Forward-arc only. One shot kills a 3-hull weak-stern enemy at
/// point-blank (6 raw, no falloff, stern armour 0 → 6 lands).
fn siege_beam() -> Action {
    Action {
        id: "siege_beam".into(),
        name: "Siege Beam".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: 6, band_falloff: None }],
        r#mod: None,
        icon: None,
    }
}

/// A `flip` reorient: turn the ship 180° so its Forward arc points at the
/// OTHER lane-end. Arc-less SELF so it always fires; costs the turn (the #72
/// tension — facing the blind side means not firing this round).
fn flip_facing() -> Action {
    Action {
        id: "flip".into(),
        name: "Flip".into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::REORIENT { to: ReorientTo::Flip }],
        r#mod: None,
        icon: None,
    }
}

/// A one-cell THRUST to close range on an out-of-band enemy (bow-relative;
/// steps toward whichever end the player currently faces).
fn step_forward() -> Action {
    Action {
        id: "step".into(),
        name: "Step".into(),
        archetype: WeaponArchetype::Movement,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: None,
            direction_2d: None,
        }],
        r#mod: None,
        icon: None,
    }
}

/// Content serving the player's run-loop kit: the siege_beam plus the `flip`
/// reorient and `step` thrust the harness uses to face + close on pincering,
/// dynamic (#71) enemies. spawn_projectile panics — these scenarios fire
/// beams, not ordnance. Constructed via `LoopContent::new()`.
struct LoopContent(HashMap<String, Action>);
impl LoopContent {
    fn new() -> Self {
        let mut m = HashMap::new();
        for a in [siege_beam(), flip_facing(), step_forward()] {
            m.insert(a.id.clone(), a);
        }
        LoopContent(m)
    }
}
impl Content for LoopContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.0.get(id)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("run-loop scenarios don't fire ordnance");
    }
}

/* ---- encounter / sector fixtures ---------------------------------- */

/// A spawn referencing a `class_id` our test builder understands. The
/// playthrough's `class_to_ship` closure dispatches on this id.
///
/// `bow` matters: `build_encounter_board` overwrites the built Ship's
/// orientation with the SPAWN's, so the spawn — not the builder — decides
/// facing. `Fore` puts the enemy's soft stern toward the player at cell 0
/// (killable target); `Aft` swings the enemy's forward gun down-lane to
/// bear on the player (a threat that shoots back).
fn spawn(class_id: &str, cell: usize, hull: i32, bow: LaneEnd) -> ShipSpawn {
    ShipSpawn {
        class_id: class_id.into(),
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hp_override: Some(hull),
    }
}

/// Build a Ship from one of our test spawns. Mirrors the closure the bin
/// passes to `build_encounter_board`. Returns `None` for unknown ids so
/// the builder's skip path is exercised by real data.
fn build_ship(spawn: &ShipSpawn) -> Option<Ship> {
    match spawn.class_id.as_str() {
        "target" => Some(weak_enemy(&format!("{}@{}", spawn.class_id, spawn.cell), spawn.cell, 3)),
        "brute" => Some(armed_enemy(&format!("{}@{}", spawn.class_id, spawn.cell), spawn.cell, 20)),
        _ => None,
    }
}

fn encounter(id: &str, spawns: Vec<ShipSpawn>, is_boss: bool) -> EncounterDef {
    EncounterDef { id: id.into(), enemy_ships: spawns, hazards: Vec::new(), is_boss }
}

/// Materialize a capital-boss spawn (class_id = the capital's display name)
/// into a killable target ship, so the capital-salvage integration test can
/// actually WIN the boss encounter. The salvage value comes from the
/// CapitalDef tier endpoints, not this ship's hull — so a low hull is fine.
fn build_capital_ship(spawn: &ShipSpawn) -> Option<Ship> {
    Some(weak_enemy(
        &format!("{}@{}", spawn.class_id, spawn.cell),
        spawn.cell,
        spawn.hp_override.unwrap_or(3),
    ))
}

/// A two-sector campaign: sector 0 has two single-target encounters,
/// sector 1 has one boss encounter. Small enough to play to victory in a
/// handful of rounds, structured enough to exercise NextEncounter →
/// NextSector → Victorious in sequence.
fn two_sector_campaign() -> Vec<Sector> {
    vec![
        Sector {
            id: "s0".into(),
            name: "Approach".into(),
            patrol_tier: 1,
            encounters: vec![
                encounter("s0e0", vec![spawn("target", 1, 3, LaneEnd::Fore)], false),
                encounter("s0e1", vec![spawn("target", 1, 3, LaneEnd::Fore)], false),
            ],
        },
        Sector {
            id: "s1".into(),
            name: "Citadel".into(),
            patrol_tier: 2,
            encounters: vec![encounter("s1boss", vec![spawn("target", 1, 3, LaneEnd::Fore)], true)],
        },
    ]
}

/* =========================================================================
 * Round driver — the loop the bin runs, minus the rendering.
 * ====================================================================== */

/// Outcome of fighting one encounter board to completion.
#[derive(Debug, PartialEq, Eq)]
enum FightResult {
    Won { rounds: usize },
    Lost { rounds: usize },
}

/// Drive `board` through `resolve_round` until it is no longer
/// `InProgress`. When `arm_player` is true, re-queue the player's
/// siege_beam each round (the queue clears after every resolve, exactly as
/// the bin re-arms it each turn). When false, the player sits idle and the
/// board outcome is driven entirely by enemy fire — the defeat path.
/// `cap` guards against an accidental infinite loop if the outcome logic
/// regresses — hitting it is itself a test failure.
fn fight_to_completion(
    board: &mut Board,
    content: &dyn Content,
    arm_player: bool,
    cap: usize,
) -> FightResult {
    for round in 1..=cap {
        if arm_player {
            queue_player_combat_action(board);
        }

        resolve_round(board, content);

        match encounter_outcome(board) {
            EncounterOutcome::Won => return FightResult::Won { rounds: round },
            EncounterOutcome::Lost => return FightResult::Lost { rounds: round },
            EncounterOutcome::InProgress => continue,
        }
    }
    panic!("fight did not terminate within {cap} rounds — outcome logic likely regressed");
}

/// Choose + queue the player's action for one round, modelling a real
/// playstyle against the #72 mid-lane pincer + #71's now-firing/moving
/// enemies. Targets the nearest live enemy and:
///   - out of the siege_beam's band (distance > 4 = past Mid) → `step` to
///     close range;
///   - in band but the Forward arc doesn't bear that lane-end → `flip` to
///     face it (costs the turn — the #72 reorient tension);
///   - in band and bearing → `siege_beam`.
///
/// A mountless/idle player (no siege_beam) is left alone (defeat path).
fn queue_player_combat_action(board: &mut Board) {
    // Snapshot player cell + facing and the nearest enemy's cell.
    let Some((pcell, pbow)) = board.cells.iter().flatten().find_map(|s| {
        if s.faction != Faction::Player {
            return None;
        }
        match s.orientation {
            Orientation::BowOn { bow } => Some((s.cell, bow)),
            // Broadside fires both ends; treat as already-bearing.
            Orientation::Broadside => Some((s.cell, bow_facing_nearest(board, s.cell))),
        }
    }) else {
        return;
    };
    let Some(enemy_cell) = board
        .cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .map(|s| s.cell)
        .min_by_key(|&e| e.abs_diff(pcell))
    else {
        return;
    };

    let dist = enemy_cell.abs_diff(pcell);
    // Lane-end the enemy lies toward, relative to the player.
    let toward = if enemy_cell >= pcell { LaneEnd::Fore } else { LaneEnd::Aft };

    let action = if dist > 4 {
        "step" // out of siege_beam band → close
    } else if pbow != toward {
        "flip" // in band but facing the wrong way → reorient to face it
    } else {
        "siege_beam" // in band and bearing → fire
    };

    if let Some(p) = board.cells[pcell].as_mut() {
        p.queue = vec![action.to_string()];
    }
}

/// Helper for a Broadside-stance player: which lane-end the nearest enemy
/// lies toward (Broadside bears both, so this just picks a valid `bow` value
/// for the bearing check — never triggers a needless flip).
fn bow_facing_nearest(board: &Board, pcell: usize) -> LaneEnd {
    board
        .cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .map(|s| s.cell)
        .min_by_key(|&e| e.abs_diff(pcell))
        .map(|e| if e >= pcell { LaneEnd::Fore } else { LaneEnd::Aft })
        .unwrap_or(LaneEnd::Fore)
}

/* =========================================================================
 * 0. #25/#41 CANARY DISAMBIGUATOR — the campaign-terminating mechanic in 2D.
 *
 * The single green test that answers "is the 2D campaign winnable": a 2D-aimed
 * player FIRES through resolve_round and KILLS one enemy end-to-end, so the
 * encounter resolves Won. If THIS is green, the generated_spawn_pool cap-timeout
 * was the 1-D test player-driver (it can't aim/close in 2D), NOT the engine —
 * confirming the stalemate diagnosis. (If a real 2D-driven campaign later still
 * cap-timeouts despite THIS being green, that's a separate resolver bug.)
 * ====================================================================== */

#[test]
fn player_fires_and_kills_one_enemy_in_2d_ends_the_encounter() {
    // Player at (0,0) bow EAST (Forward gun bears down row 0), armed with the
    // all-band siege_beam (range_band [Adjacent,Near,Far]). A weak naked enemy
    // at (2,0) — distance 2 = Near, in band, on the East ray → the shot bears.
    // siege_beam 6 raw * 0.6 Near = floor 3 onto the stern (armour 0) → kills the
    // hull-3 enemy. Drive resolve_round re-arming the shot; the encounter resolves
    // Won within a couple of rounds. This is the literal "2D campaign-terminating
    // kill works" proof (#41).
    let player = common::ship_2d("p", Faction::Player, Pos::new(0, 0), 30, Facing::Bow(Dir4::E), Arc::Forward, "siege_beam");
    // Weak naked enemy, bow West (weak stern toward the incoming East shot).
    let mut enemy = common::ship_2d("e", Faction::Enemy, Pos::new(2, 0), 3, Facing::Bow(Dir4::W), Arc::Forward, "siege_beam");
    enemy.shield_profile = common::naked_shields();
    let mut board = common::board_2d(vec![player, enemy]);
    let content = LoopContent::new();

    let mut outcome = EncounterOutcome::InProgress;
    let mut rounds = 0;
    while outcome == EncounterOutcome::InProgress && rounds < 8 {
        // Re-arm the player's siege_beam each round (the bin re-arms each turn),
        // finding it by id wherever it sits.
        if let Some(slot) = board.cells.iter().position(|c| c.as_ref().map(|s| s.id == "p").unwrap_or(false)) {
            if let Some(p) = board.cells[slot].as_mut() {
                p.queue = vec!["siege_beam".into()];
            }
        }
        resolve_round(&mut board, &content);
        outcome = encounter_outcome(&board);
        rounds += 1;
    }

    assert_eq!(
        outcome,
        EncounterOutcome::Won,
        "#41: a 2D-aimed player fires + kills the enemy end-to-end → encounter Won (got {outcome:?} after {rounds} rounds)",
    );
    assert!(
        !board.cells.iter().flatten().any(|s| s.faction == Faction::Enemy),
        "the enemy is destroyed",
    );
    assert!(
        board.cells.iter().flatten().any(|s| s.faction == Faction::Player),
        "the player survives the kill",
    );
}

/* =========================================================================
 * 1. Played-through victory across the whole campaign.
 * ====================================================================== */

// #[ignore]: stale 1-D fixture. The local spawn() helper pins pos=Pos::new(0,0) for
// every spawn; after C4's invariant-A placement (build_encounter_board places at
// spawn.pos.to_index()) all enemies collide at cell 0 and are skipped, so the player
// can't clear a real board and the played-through victory never sets (cap timeout).
// Plus the player-driver + enemy AI are still 1-D (C1 pending). NOT a 2-D engine bug
// (reviewer-confirmed). Restore campaign winnability on the 2-D fixture rewrite +
// C1/R6 — tracks #22. (Contrast: generated_spawn_pool_campaign_plays_through_to_victory
// PASSES because it uses the real generator's 2-D spawn positions, not spawn().)
#[ignore = "stale 1-D spawn() fixture (pos (0,0)) + 1-D player/AI; restore at 2-D run_loop fixture rewrite + C1/R6 — #22"]
#[test]
fn full_campaign_played_to_victory_sets_victorious() {
    let sectors = two_sector_campaign();
    let content = LoopContent::new();
    let mut run = Run::new(player_frigate(0, 30));

    // Track the advance discriminators we walk through; the campaign shape
    // dictates this exact sequence and the test pins it.
    let mut advances: Vec<AdvanceResult> = Vec::new();

    // Hard cap on encounters so a broken advance can't loop forever.
    for _ in 0..16 {
        let enc = match broadside_engine::runs::current_encounter(&run, &sectors) {
            Some(e) => e.clone(),
            None => break, // run ended (victorious or defeated)
        };

        let mut board = build_encounter_board(&enc, run.player.clone(), build_ship);
        // Cap 64: #71 enemies move + fire and the player spends turns
        // reorienting/closing against the #72 pincer, so a clear takes more
        // rounds than the old stationary-fire harness.
        let result = fight_to_completion(&mut board, &content, true, 64);
        assert!(
            matches!(result, FightResult::Won { .. }),
            "player should clear {} — got {result:?}",
            enc.id,
        );

        // Carry the surviving player Ship forward, exactly like the bin.
        run.player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .cloned()
            .expect("player survives a won encounter");

        advances.push(advance_after_win(&mut run, &sectors));
    }

    assert!(run.victorious, "a fully-played-through campaign sets victorious");
    assert!(!run.defeated, "a won run is not also defeated");
    assert_eq!(
        advances,
        vec![
            AdvanceResult::NextEncounter, // s0e0 -> s0e1
            AdvanceResult::NextSector,    // s0e1 cleared -> sector 1
            AdvanceResult::Victorious,    // s1boss cleared -> run won
        ],
        "the advance sequence must walk encounter -> sector -> victory exactly once each",
    );
}

/* =========================================================================
 * 2. Played-through defeat routes through mark_defeated.
 * ====================================================================== */

#[ignore = "stale 1-D spawn() fixture (pos (0,0)) — brute can't bear on stacked player so the loss never resolves; restore at 2-D run_loop fixture rewrite + C1/R6 — #22"]
#[test]
fn losing_an_encounter_on_a_real_board_marks_run_defeated() {
    let content = LoopContent::new();
    // A near-dead player (hull 1) versus a high-hull brute that shoots
    // back, and we DON'T arm the player (`arm_player: false`) — so the
    // player sits idle and the board outcome is decided entirely by the
    // brute's return fire. This is the "the board killed me" path the
    // run-loop must handle.
    // #72: build_encounter_board now forces the player to the MID cell
    // (size/2), ignoring the passed cell. The encounter's max spawn cell sets
    // the lane size; brute@4 → canonical_lane_size(4) = 5 → mid = 2. So the
    // player lands at cell 2 and the brute at cell 4 (no collision — a brute
    // at cell 2 would now be skipped as colliding with the mid-lane player).
    // Brute bow=Aft so its forward arc bears down-lane (toward lower cells) on
    // the player at 2; distance 2 = Close.
    let enc = encounter("ambush", vec![spawn("brute", 4, 20, LaneEnd::Aft)], false);
    let mut run = Run::new(player_frigate(0, 1));
    // Route the brute's fire onto the player's soft stern. The hit arrives
    // FROM the Fore direction (brute sits at the higher cell), so the player
    // must face bow=Aft to put its stern — armour 0 — toward the brute.
    // With distance 2 (Close) and siege_beam optimal PointBlank, falloff
    // leaves floor(6 * 0.66) = 3 damage, which a 1-hull player can't survive.
    run.player.orientation = Orientation::BowOn { bow: LaneEnd::Aft };

    let mut board = build_encounter_board(&enc, run.player.clone(), build_ship);
    let result = fight_to_completion(&mut board, &content, false, 16);

    assert!(
        matches!(result, FightResult::Lost { .. }),
        "a 1-hull player against an armed brute loses the board — got {result:?}",
    );
    assert_eq!(
        encounter_outcome(&board),
        EncounterOutcome::Lost,
        "board outcome is Lost once the player ship is destroyed",
    );

    // The bin's lose branch: mark the run defeated.
    mark_defeated(&mut run);
    assert!(run.defeated, "real defeat flips run.defeated");
    assert!(!run.victorious, "a defeated run is not victorious");
}

/* =========================================================================
 * 3. Salvage → meta accrual across a real won encounter.
 * ====================================================================== */

#[ignore = "stale 1-D spawn() fixture (pos (0,0)) — targets collide at cell 0, fight never resolves Won; salvage logic itself untouched; restore at 2-D run_loop fixture rewrite + C1/R6 — #22"]
#[test]
fn winning_an_encounter_accrues_salvage_into_the_run() {
    let content = LoopContent::new();
    // Three weak targets — each worth 1 salvage (max_hull 3 → tier 1).
    let enc = encounter(
        "haul",
        vec![
            spawn("target", 1, 3, LaneEnd::Fore),
            spawn("target", 2, 3, LaneEnd::Fore),
            spawn("target", 3, 3, LaneEnd::Fore),
        ],
        false,
    );
    let mut run = Run::new(player_frigate(0, 30));

    let mut board = build_encounter_board(&enc, run.player.clone(), build_ship);
    let result = fight_to_completion(&mut board, &content, true, 48);
    assert!(matches!(result, FightResult::Won { .. }), "player clears the haul — got {result:?}");

    // The bin awards salvage off the encounter's spawn list once the
    // board is Won. Three tier-1 kills → 3 salvage.
    assert_eq!(
        salvage_for_encounter_win(&enc, build_ship),
        3,
        "three max_hull-3 enemies are worth 1 salvage each",
    );

    award_run_salvage(&mut run, &enc, build_ship);
    assert_eq!(run.salvage, 3, "the run banks the encounter's salvage");
}

/// The CATALOG-LESS fallback reward path: `award_run_salvage` (no catalog)
/// still applies the flat `is_boss → ×2` multiplier. This is NOT the
/// canonical capital reward anymore — the live bin path uses the tier-scaled
/// `award_run_salvage_with_catalog` (see
/// `capital_boss_win_accrues_tier_scaled_salvage_into_the_run` below). This
/// test pins the still-valid no-catalog fallback (placeholder campaign with
/// no CapitalDefs), so it must NOT be read as "capitals pay ×2 in the game."
#[ignore = "stale 1-D spawn() fixture (pos (0,0)) — boss target stacked, fight never resolves Won; restore at 2-D run_loop fixture rewrite + C1/R6 — #22"]
#[test]
fn catalogless_boss_fallback_doubles_salvage_on_a_real_win() {
    let content = LoopContent::new();
    let enc = encounter("boss", vec![spawn("target", 1, 3, LaneEnd::Fore)], true);
    let mut run = Run::new(player_frigate(0, 30));

    let mut board = build_encounter_board(&enc, run.player.clone(), build_ship);
    assert!(matches!(
        fight_to_completion(&mut board, &content, true, 48),
        FightResult::Won { .. }
    ));

    award_run_salvage(&mut run, &enc, build_ship);
    assert_eq!(
        run.salvage, 2,
        "catalog-less fallback: the flat is_boss ×2 doubles the one tier-1 kill (1 -> 2)",
    );
}

/// The LIVE reward path (the bin's `award_encounter_salvage`): a capital-boss
/// win, played through real boards, banks the doc-canonical TIER-SCALED
/// capital salvage (CapitalDef salvage_p1→salvage_p7 interpolation) into the
/// run, and `accumulate_into_meta` rolls it forward. content's meta.rs units
/// pin `capital_salvage_for_tier` / `salvage_for_capital_encounter` at the
/// function level; this locks the same value flowing through the run loop
/// exactly as the bin awards it.
///
/// The Dasher: salvage_p1=2, salvage_p7=7. At patrol tier 4 the interpolation
/// is 2 + (7-2)*(4-1)/6 = 2 + 2 = 4 (matching content's
/// `capital_salvage_interpolates_p1_to_p7_by_tier`).
#[ignore = "stale 1-D spawn() fixture (pos (0,0)) — capital target stacked, fight never resolves Won; tier-scaled salvage math untouched; restore at 2-D run_loop fixture rewrite + C1/R6 — #22"]
#[test]
fn capital_boss_win_accrues_tier_scaled_salvage_into_the_run() {
    // Catalog with one capital carrying the tier endpoints.
    let json = serde_json::json!({
        "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
        "actions": [], "mods": [], "subsystems": [], "statuses": [],
        "enemies": [], "patrols": [],
        "capitals": [
            { "id": "dasher", "name": "The Dasher", "sector": "Drift Belt",
              "corrupt": true, "sP1": 2, "sP7": 7 },
        ],
    });
    let catalog = broadside_engine::catalog_canonical::from_canonical_value(json)
        .expect("capital catalog parses");

    // A capital-boss encounter: a single boss spawn whose class_id is the
    // capital's display NAME (exactly how runs::capital_spawn builds it). The
    // boss materializes as a killable target so the player wins it.
    let enc = encounter(
        "drift_belt_boss",
        vec![spawn("The Dasher", 1, 3, LaneEnd::Fore)],
        true,
    );
    let mut run = Run::new(player_frigate(0, 30));
    let content = LoopContent::new();

    let mut board = build_encounter_board(&enc, run.player.clone(), build_capital_ship);
    assert!(matches!(
        fight_to_completion(&mut board, &content, true, 48),
        FightResult::Won { .. }
    ));

    // The bin's live award path, at patrol tier 4.
    let patrol_tier = 4u8;
    award_run_salvage_with_catalog(&mut run, &enc, &catalog, patrol_tier, build_capital_ship);
    assert_eq!(
        run.salvage, 4,
        "The Dasher (sP1=2, sP7=7) at tier 4 pays the interpolated 4 — NOT the flat ×2 fallback (which would be 2)",
    );

    // And it rolls into the persistent meta total like any other run salvage.
    let mut meta = MetaProgression::default();
    accumulate_into_meta(&mut meta, &run);
    assert_eq!(
        meta.total_salvage_earned, 4,
        "tier-scaled capital salvage accrues into the meta total through the rollover",
    );

    // Tier sensitivity: the SAME capital win at tier 1 pays the low endpoint
    // (2), at tier 7 the high endpoint (7) — proving the reward genuinely
    // scales with patrol tier through the live award path, not a flat value.
    let award_at = |tier: u8| {
        let mut r = Run::new(player_frigate(0, 30));
        award_run_salvage_with_catalog(&mut r, &enc, &catalog, tier, build_capital_ship);
        r.salvage
    };
    assert_eq!(award_at(1), 2, "tier 1 → salvage_p1");
    assert_eq!(award_at(7), 7, "tier 7 → salvage_p7");
    assert!(award_at(1) < award_at(4) && award_at(4) < award_at(7), "salvage rises with tier");
}

#[test]
fn run_salvage_crossing_threshold_unlocks_subsystem_in_meta() {
    // The first unlock threshold is rear_gunner @ 10 salvage. Bank a run
    // worth exactly that and confirm the rollover unlocks it.
    let (first_id, first_threshold) = SUBSYSTEM_UNLOCK_THRESHOLDS[0];
    assert_eq!(first_id, "rear_gunner", "test pins the first-tier unlock id");

    let mut meta = MetaProgression::default();
    assert!(
        !meta.unlocked_subsystems.contains(&first_id.to_string()),
        "fresh meta has no cross-run unlocks",
    );

    let mut run = Run::new(player_frigate(0, 30));
    run.salvage = first_threshold; // a run that earned exactly the threshold

    let newly = accumulate_into_meta(&mut meta, &run);

    assert_eq!(newly, vec![first_id.to_string()], "crossing the threshold reports the unlock");
    assert_eq!(meta.total_salvage_earned, first_threshold, "salvage rolled into the meta total");
    assert!(
        meta.unlocked_subsystems.contains(&first_id.to_string()),
        "the unlocked subsystem persists in meta",
    );
}

/* =========================================================================
 * 4. Headless smoke test — the logic-layer guard for the render crash.
 * ====================================================================== */

#[test]
fn build_board_and_first_resolve_round_does_not_panic() {
    // No wgpu, no window — pure logic. This is the cheap CI-runnable
    // sentinel for "the engine boots and takes one step." If a future
    // change makes board materialization or the first round panic (the
    // class of failure the render path hit), this fails in CI with no GPU.
    let sectors = two_sector_campaign();
    let content = LoopContent::new();
    let run = Run::new(player_frigate(0, 30));

    let enc = broadside_engine::runs::current_encounter(&run, &sectors)
        .expect("a fresh run has a current encounter");
    let mut board = build_encounter_board(enc, run.player.clone(), build_ship);

    // One round. Just must not panic.
    resolve_round(&mut board, &content);

    // And the board is still coherent: player present, grid shape intact.
    // v2 (A3 Board EXPAND): the cell vector is now the fixed-size 5×4 grid
    // (`grid::CELLS` = 20), no longer `size`-length — `build_encounter_board`
    // builds len-CELLS backing Vecs so the 2-D occupancy view works. `size`
    // is the (transitional) logical lane length, dropped at CONTRACT.
    assert_eq!(
        board.cells.len(),
        broadside_engine::grid::CELLS,
        "board cell vector is the fixed 5×4 grid (len CELLS)",
    );
    assert!(
        board.cells.iter().flatten().any(|s| s.faction == Faction::Player),
        "player ship still on the board after one round",
    );
}

/* =========================================================================
 * 5. Played-through victory over a DATA-DRIVEN (#60 spawn-pool) campaign.
 *
 * `src/runs.rs` has inline unit tests for the generator (pool accumulation,
 * encounters-then-boss, determinism, staging passthrough, campaign coverage,
 * unknown-capital). This is the missing INTEGRATION claim: a campaign built
 * by `generate_campaign(catalog)` actually PLAYS — its pool-sampled enemies
 * and capital bosses materialize into real Boards, get cleared through
 * `resolve_round`, and the run advances sector→sector to a played-through
 * Victorious. If the generator emitted spawns the board-builder/resolver
 * couldn't field (bad cell, unmaterializable class id, a sector that never
 * resolves Won), this hangs or breaks — the unit tests can't catch that.
 * ====================================================================== */

/// A minimal canonical catalog with a spawn pool: two combat sectors
/// (intro ship-types + a capital each) plus a leading Staging passthrough.
/// Mirrors the shape `generate_campaign` consumes. Built via the canonical
/// transformer so it exercises the real catalog → SectorDef path.
fn generated_campaign_catalog() -> broadside_engine::types::Catalog {
    // Full canonical shape (mirrors src/runs.rs's gen_catalog) so the
    // transformer's enemy / capital / SectorDef deserializers are exercised
    // for real — the minimal shape misses required fields (hull5, traits,
    // sector, weapons, the meta/actions header).
    let json = serde_json::json!({
        "meta": { "schema": "x", "lane": [5,7,9], "newAxes": [], "bands": ["close"] },
        "actions": [
            { "id": "pulse_laser", "name": "Pulse Laser", "archetype": "beam",
              "heat": 1, "cd": 0, "band": "close", "pattern": "BEAM",
              "arc": "forward", "freeplay": false, "effects": ["DAMAGE"] },
        ],
        "mods": [], "subsystems": [], "statuses": [], "patrols": [],
        "enemies": [
            { "id": "skiff", "name": "Skiff", "hull": 3, "hull5": 4, "traits": [],
              "sector": "Drift Belt", "weapons": ["Pulse Laser"] },
            { "id": "lancer", "name": "Lancer", "hull": 1, "hull5": 2, "traits": [],
              "sector": "Drift Belt", "weapons": ["Pulse Laser"] },
            { "id": "gunboat", "name": "Gunboat", "hull": 4, "hull5": 5, "traits": [],
              "sector": "Ion Reefs", "weapons": ["Pulse Laser"] },
        ],
        "capitals": [
            { "id": "dasher", "name": "The Dasher", "sector": "Drift Belt" },
            { "id": "impaler", "name": "The Impaler", "sector": "Ion Reefs" },
        ],
        "sectors": [
            { "name": "Staging",    "node": "0",   "lane": 5, "intro": [],                  "capital": "—" },
            { "name": "Drift Belt", "node": "1",   "lane": 5, "intro": ["Skiff","Lancer"], "capital": "The Dasher" },
            { "name": "Ion Reefs",  "node": "2.1", "lane": 7, "intro": ["Gunboat"],         "capital": "The Impaler" },
        ],
    });
    broadside_engine::catalog_canonical::from_canonical_value(json)
        .expect("generated-campaign catalog parses")
}

/// Materialize a generated spawn into a Ship. The generator emits pool
/// ship-type ids (skiff/lancer/gunboat) for regular enemies and the
/// capital's display name (The Dasher / The Impaler) for bosses — all
/// bow=Aft (facing the player). All become low-hull, mountless targets so
/// the player's siege_beam clears them; the point is that EVERY generated
/// class id materializes (no `None` drop would silently empty an encounter).
fn build_generated_ship(spawn: &ShipSpawn) -> Option<Ship> {
    // hp_override is None on generated spawns; give regulars hull 3 and
    // capitals a bit more so the boss encounter takes a couple of rounds.
    let hull = match spawn.class_id.as_str() {
        "The Dasher" | "The Impaler" => 6,
        _ => 3,
    };
    Some(weak_enemy(
        &format!("{}@{}", spawn.class_id, spawn.cell),
        spawn.cell,
        hull,
    ))
}

// #[ignore]: REGRESSED after R4/R6/R6b/#28 (passed at #22 time — uses the REAL
// generator's 2-D spawn positions, not stale fixtures). Root cause is NOT the
// spawn fixtures (those are 2-D-correct) — it's the 1-D test-harness player-driver
// queue_player_combat_action (drives off cell/orientation/LaneEnd) which can't
// pilot a now-fully-2D fight (2-D fire+damage+move), so the campaign never resolves
// Won/Lost → cap timeout (run_loop.rs:350). Gated on #25 (migrate the player-driver
// to 2-D) + the run_loop 2-D fixture rewrite. NOTE: re-check "didn't terminate" at
// the #25 migration — if a real 2-D driver STILL hangs, that's a real resolver bug
// to flag, not a harness gap.
#[ignore = "regressed: 1-D player-driver can't pilot 2-D combat → no termination; restore at #25 + run_loop 2-D fixtures — #22"]
#[test]
fn generated_spawn_pool_campaign_plays_through_to_victory() {
    let catalog = generated_campaign_catalog();
    // The shared LoopContent (siege_beam + flip + step) drives the moving,
    // reorienting player via fight_to_completion — handles both the lane-7
    // Long-range spawns (close with `step`) and the #72 pincer (face each
    // side with `flip`).
    let content = LoopContent::new();
    let sectors = generate_campaign(&catalog, 1);

    // Sanity: the generated campaign has the expected shape before we play
    // it — a Staging passthrough (no encounters) + two combat sectors that
    // each end in a capital boss.
    assert_eq!(sectors.len(), 3, "one runtime Sector per catalog SectorDef");
    assert!(sectors[0].encounters.is_empty(), "Staging is a passthrough");
    assert!(
        sectors[1].encounters.last().is_some_and(|e| e.is_boss),
        "Drift Belt ends in its capital boss",
    );
    assert!(
        sectors[2].encounters.last().is_some_and(|e| e.is_boss),
        "Ion Reefs ends in its capital boss",
    );

    // Play the whole generated campaign through the real run-loop.
    let mut run = Run::new(player_frigate(0, 60));
    let mut encounters_played = 0usize;
    let mut bosses_beaten = 0usize;

    for _ in 0..64 {
        let enc = match current_encounter(&run, &sectors) {
            Some(e) => e.clone(),
            None => break, // run ended (victorious or defeated)
        };

        let mut board = build_encounter_board(&enc, run.player.clone(), build_generated_ship);
        // Moving, reorienting player (the #65 + #72 fix): close range on far
        // spawns and flip to face whichever side is pincering, then fire.
        let result = fight_to_completion(&mut board, &content, true, 64);
        assert!(
            matches!(result, FightResult::Won { .. }),
            "player clears generated encounter {} — got {result:?}",
            enc.id,
        );
        encounters_played += 1;
        if enc.is_boss {
            bosses_beaten += 1;
        }

        run.player = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .cloned()
            .expect("player survives a won encounter");

        advance_after_win(&mut run, &sectors);
    }

    assert!(run.victorious, "a fully-played generated campaign reaches Victorious");
    assert!(!run.defeated, "a won generated run is not also defeated");
    assert_eq!(bosses_beaten, 2, "both capital bosses (The Dasher, The Impaler) were beaten");
    // ENCOUNTERS_PER_SECTOR (2) pool encounters + 1 boss in each of the two
    // combat sectors = 6; Staging contributes none.
    assert_eq!(
        encounters_played, 6,
        "played 2 pool encounters + 1 boss in each of the 2 combat sectors",
    );
}
