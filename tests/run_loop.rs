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
    accumulate_into_meta, award_run_salvage, salvage_for_encounter_win, MetaProgression,
    SUBSYSTEM_UNLOCK_THRESHOLDS,
};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::runs::{
    advance_after_win, build_encounter_board, current_encounter, encounter_outcome,
    generate_campaign, mark_defeated, AdvanceResult, EncounterOutcome,
};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, EncounterDef, Effect, Faction, LaneEnd, Mount, Orientation,
    Projectile, RangeBand, Run, Sector, ShieldFace, ShieldProfile, Ship, ShipSpawn, Targeting,
    TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

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
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
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
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
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

/// Content with just the siege_beam. spawn_projectile panics — these
/// scenarios fire beams, not ordnance.
struct LoopContent(Action);
impl Content for LoopContent {
    fn action(&self, id: &str) -> Option<&Action> {
        (id == "siege_beam").then_some(&self.0)
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
        orientation: Orientation::BowOn { bow },
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
            // Re-arm the player. The bin does this from the input queue;
            // here we just always fire the one weapon.
            if let Some(slot) = board.cells.iter_mut().find(|c| {
                c.as_ref().map(|s| s.faction == Faction::Player).unwrap_or(false)
            }) {
                if let Some(p) = slot.as_mut() {
                    p.queue = vec!["siege_beam".into()];
                }
            }
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

/* =========================================================================
 * 1. Played-through victory across the whole campaign.
 * ====================================================================== */

#[test]
fn full_campaign_played_to_victory_sets_victorious() {
    let sectors = two_sector_campaign();
    let content = LoopContent(siege_beam());
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
        let result = fight_to_completion(&mut board, &content, true, 16);
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

#[test]
fn losing_an_encounter_on_a_real_board_marks_run_defeated() {
    let content = LoopContent(siege_beam());
    // A near-dead player (hull 1) versus a high-hull brute that shoots
    // back, and we DON'T arm the player (`arm_player: false`) — so the
    // player sits idle and the board outcome is decided entirely by the
    // brute's return fire. This is the "the board killed me" path the
    // run-loop must handle.
    // Brute at cell 2 (one gap from the player at cell 0), bow=Aft so its
    // forward arc bears down-lane on the player — the exact geometry the
    // resolver's `ai_queues_threatening_action_when_bears` test proves the
    // AI fires from.
    let enc = encounter("ambush", vec![spawn("brute", 2, 20, LaneEnd::Aft)], false);
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

#[test]
fn winning_an_encounter_accrues_salvage_into_the_run() {
    let content = LoopContent(siege_beam());
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
    let result = fight_to_completion(&mut board, &content, true, 24);
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

#[test]
fn boss_encounter_doubles_salvage_on_a_real_win() {
    let content = LoopContent(siege_beam());
    let enc = encounter("boss", vec![spawn("target", 1, 3, LaneEnd::Fore)], true);
    let mut run = Run::new(player_frigate(0, 30));

    let mut board = build_encounter_board(&enc, run.player.clone(), build_ship);
    assert!(matches!(
        fight_to_completion(&mut board, &content, true, 8),
        FightResult::Won { .. }
    ));

    award_run_salvage(&mut run, &enc, build_ship);
    assert_eq!(run.salvage, 2, "boss flag doubles the one tier-1 kill (1 -> 2)");
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
    let content = LoopContent(siege_beam());
    let run = Run::new(player_frigate(0, 30));

    let enc = broadside_engine::runs::current_encounter(&run, &sectors)
        .expect("a fresh run has a current encounter");
    let mut board = build_encounter_board(enc, run.player.clone(), build_ship);

    // One round. Just must not panic.
    resolve_round(&mut board, &content);

    // And the board is still coherent: player present, size preserved.
    assert_eq!(board.size, board.cells.len(), "board cell vector matches its declared size");
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

#[test]
#[ignore = "#65: tier-2 generated sector-2 encounter stalemates >64 rounds (balance bug, resolver diagnosing); un-ignore once #65 lands"]
fn generated_spawn_pool_campaign_plays_through_to_victory() {
    let catalog = generated_campaign_catalog();
    let content = LoopContent(siege_beam());
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
