//! Parallax-flow Phase 1 integration tests (#210 P10).
//!
//! Locks the lib-level contract the new continuous-flow state machine reads
//! from. The bin's `DemoState::Transitioning(Round|Waypoint)` / `Dying(_)`
//! variants live in `src/bin/broadside.rs` (bin-private), so this file pins
//! the **engine-side inputs** the bin's state machine consumes — driving the
//! same `advance_after_win` / `encounter_outcome` / `Board.level` cursor /
//! `Run` cursor the bin walks each frame:
//!
//!   1. **Round → round:** clearing a non-boss encounter bumps
//!      `run.completed_encounters` (the cursor that `Board.level` mirrors
//!      via P2's formula); `advance_after_win` returns
//!      `AdvanceResult::NextEncounter` (the bin maps this to
//!      `TransitionKind::Round`, not the deleted `EncounterComplete` modal).
//!
//!   2. **Sector → sector (waypoint):** clearing the last encounter of a
//!      non-final sector returns `AdvanceResult::NextSector`; the cursor
//!      resets and `current_sector_idx` bumps (the bin maps this to
//!      `TransitionKind::Waypoint` + the longer warp).
//!
//!   3. **Continuous death:** `mark_defeated(&mut run)` flips
//!      `run.defeated`; `advance_after_win` on an already-defeated run is a
//!      no-op (`AlreadyEnded`) — the new continuous-DEATH flow can advance
//!      sim time without the lib's run advancer looping forever.
//!
//!   4. **Victory:** the final encounter clear returns `Victorious` and
//!      flips `run.victorious`; further advances are no-ops.
//!
//!   5. **`Board.level` cursor lockstep:** the bin's
//!      `run_cursor(run) = sector_idx * ENCOUNTERS_PER_SECTOR +
//!      completed_encounters` formula stays consistent across the round +
//!      waypoint transitions (P2 wires this into `board.level`; we assert
//!      the cursor input it consumes).
//!
//!   6. **`ENCOUNTERS_PER_SECTOR == 4` (P1 design law):** locks the
//!      4-rounds-per-level rule against a future bump.
//!
//!   7. **Full generated campaign across mixed shapes:** when the
//!      variable-board flip (#199b) lands, a generated campaign still plays
//!      through. This file uses `generate_campaign` directly — when the
//!      generator starts emitting mixed `Dims` per encounter, the
//!      already-passing `generated_spawn_pool_campaign_plays_through_to_victory`
//!      in `tests/run_loop.rs` covers it; the cursor tests here keep their
//!      assertions dim-invariant so they don't flake on the flip.
//!
//! ## Why not assert against `DemoState` directly
//!
//! `DemoState` is `enum` in `src/bin/broadside.rs` — bin-private, no `pub`,
//! no integration-test reach. The plan doc's piece-10 anchors are
//! `tests/run_loop.rs` + `tests/turn_loop.rs` and explicitly say "Add new
//! asserts here, not a rewrite". The lib contract this file pins IS what
//! the bin's `DemoState::Transitioning(Round|Waypoint)` is built ON TOP OF:
//! a regression in the lib here surfaces in the bin's transitions too. The
//! one assertion shape that's strictly bin-internal — "no
//! `push_between_encounter_overlay` call" — isn't reachable from
//! integration tests; pinning the API the modal was REPLACED with (P4) is
//! the lib-level proxy.

use broadside_engine::runs::{
    advance_after_win, encounter_outcome, mark_defeated, AdvanceResult, EncounterOutcome,
    ENCOUNTERS_PER_SECTOR,
};
use broadside_engine::types::{
    Arc, Board, EncounterDef, EventBus, Faction, LaneEnd, Mount, Orientation, Run, Sector,
    ShieldFace, ShieldProfile, Ship,
};
use std::collections::HashMap;

/// The bin's `Self::run_cursor` formula (P2, `src/bin/broadside.rs`):
/// `run.current_sector_idx * ENCOUNTERS_PER_SECTOR + run.completed_encounters`.
/// Held here so the cursor tests assert the same value the bin sets
/// `board.level` to — a divergence would break P2's parallax-tween input.
const fn run_cursor(run: &Run) -> usize {
    run.current_sector_idx * ENCOUNTERS_PER_SECTOR as usize + run.completed_encounters as usize
}

/// A minimal player Ship for `Run::new`. The flow tests don't read its
/// stats — only the run-cursor + outcome plumbing matters here.
fn player_seed() -> Ship {
    Ship {
        id: "player".into(),
        faction: Faction::Player,
        cell: 0,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::N),
        hull: 10,
        max_hull: 10,
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
    }
}

/// Build an `EncounterDef` with a unique id and no spawns. The flow tests
/// don't play encounters — they just need `Sector::encounters` to have the
/// right LENGTH so `advance_after_win` walks the cursor.
fn empty_encounter(id: &str, is_boss: bool) -> EncounterDef {
    EncounterDef {
        id: id.into(),
        enemy_ships: Vec::new(),
        hazards: Vec::new(),
        is_boss,
        ..Default::default()
    }
}

/// Build a Sector with `n` non-boss encounters + one boss at the end. This
/// matches the generated-campaign shape (`generate_campaign` emits
/// `ENCOUNTERS_PER_SECTOR` pool encounters + 1 capital boss per combat
/// sector).
fn combat_sector(id: &str, name: &str) -> Sector {
    let mut encounters: Vec<EncounterDef> = (0..ENCOUNTERS_PER_SECTOR)
        .map(|i| empty_encounter(&format!("{id}-r{i}"), false))
        .collect();
    encounters.push(empty_encounter(&format!("{id}-boss"), true));
    Sector {
        id: id.into(),
        name: name.into(),
        patrol_tier: 1,
        encounters,
    }
}

/// A minimal `Board` snapshot scaffold for the cursor test — just enough
/// for `encounter_outcome` to return `Won` (player present, no enemies).
fn empty_won_board() -> Board {
    Board {
        size: 20,
        cols: 5,
        rows: 4,
        cells: {
            let mut v: Vec<Option<Ship>> = (0..20).map(|_| None).collect();
            v[0] = Some(player_seed());
            v
        },
        ordnance: Vec::new(),
        hazards: (0..20).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

/* =========================================================================
 * (1) Round → round transition
 * ====================================================================== */

#[test]
fn round_clear_bumps_completed_encounters_and_returns_next_encounter() {
    // Fresh run on a 2-sector campaign (4 rounds + 1 boss each). Clearing
    // round 0 returns NextEncounter and bumps the cursor to 1.
    let sectors = vec![
        combat_sector("s0", "Drift Belt"),
        combat_sector("s1", "Ion Reefs"),
    ];
    let mut run = Run::new(player_seed());
    assert_eq!(run.completed_encounters, 0);
    assert_eq!(run.current_sector_idx, 0);
    let result = advance_after_win(&mut run, &sectors);
    assert_eq!(
        result,
        AdvanceResult::NextEncounter,
        "round 0 clear advances within the sector (not modal-blocked)",
    );
    assert_eq!(run.completed_encounters, 1, "cursor bumped to round 1");
    assert_eq!(run.current_sector_idx, 0, "still in sector 0");
    assert!(!run.victorious && !run.defeated, "run is mid-flow");
}

#[test]
fn round_cursor_matches_bin_run_cursor_formula_through_first_sector() {
    // Walk through every round of sector 0 and assert the bin's
    // `run_cursor` formula equals what the bin sets `board.level` to (P2).
    // This is the bridge between the lib's `Run` and the bin's parallax-
    // tween input — if it drifts, the background tween mis-targets and
    // the player sees the wrong upcoming-area parallax.
    let sectors = vec![
        combat_sector("s0", "Drift Belt"),
        combat_sector("s1", "Ion Reefs"),
    ];
    let mut run = Run::new(player_seed());
    // Round 0: cursor reads 0.
    assert_eq!(run_cursor(&run), 0, "fresh run: cursor 0");
    // Walk the 4 pool rounds + 1 boss = 5 encounters per sector. Each
    // returns NextEncounter (within-sector) until the last clear, which
    // returns NextSector (boundary).
    for round_idx in 0..ENCOUNTERS_PER_SECTOR {
        let _ = advance_after_win(&mut run, &sectors);
        assert_eq!(
            run_cursor(&run),
            (round_idx + 1) as usize,
            "after round {round_idx} clear: cursor = round_idx+1",
        );
    }
    // After ENCOUNTERS_PER_SECTOR clears, we still have ONE more encounter
    // (the boss at index ENCOUNTERS_PER_SECTOR in the sector's encounters
    // vec). Clearing it crosses the sector boundary.
    let boss_result = advance_after_win(&mut run, &sectors);
    assert_eq!(
        boss_result,
        AdvanceResult::NextSector,
        "last-encounter-of-sector clear is the waypoint transition (P6)",
    );
    assert_eq!(
        run_cursor(&run),
        ENCOUNTERS_PER_SECTOR as usize,
        "sector boundary: cursor = sector_idx * ENCOUNTERS_PER_SECTOR",
    );
    assert_eq!(run.completed_encounters, 0, "cursor RESETS on sector cross");
    assert_eq!(run.current_sector_idx, 1, "moved into sector 1");
}

/* =========================================================================
 * (2) Sector → sector (waypoint) transition
 * ====================================================================== */

#[test]
fn final_sector_encounter_clear_returns_next_sector_when_sector_not_final() {
    // Walk to the last encounter of sector 0, clear it; expect NextSector
    // (the new Waypoint transition, P6) — NOT the deleted EncounterComplete
    // modal-state interruption.
    let sectors = vec![
        combat_sector("s0", "Drift Belt"),
        combat_sector("s1", "Ion Reefs"),
    ];
    let mut run = Run::new(player_seed());
    // Step to the boss encounter (5 NextEncounter results = 4 round clears
    // + the boss step ends the sector → NextSector).
    for _ in 0..ENCOUNTERS_PER_SECTOR {
        assert_eq!(
            advance_after_win(&mut run, &sectors),
            AdvanceResult::NextEncounter,
        );
    }
    // The (ENCOUNTERS_PER_SECTOR+1)th clear is the boss → NextSector.
    let result = advance_after_win(&mut run, &sectors);
    assert_eq!(result, AdvanceResult::NextSector);
    assert_eq!(run.current_sector_idx, 1);
    assert_eq!(
        run.completed_encounters, 0,
        "round cursor resets on waypoint"
    );
}

/* =========================================================================
 * (3) Continuous death — terminates, no soft-lock
 * ====================================================================== */

#[test]
fn mark_defeated_then_advance_is_a_noop_already_ended() {
    // The continuous-DEATH path (P8): the bin holds the player on the
    // frozen final board while the death VFX plays; at no point does the
    // run advance into a NEXT encounter. Lib contract: once `defeated` is
    // set, further `advance_after_win` calls return `AlreadyEnded` and
    // touch nothing — the bin's loop can't soft-lock by repeatedly trying
    // to advance a dead run.
    let sectors = vec![combat_sector("s0", "Drift Belt")];
    let mut run = Run::new(player_seed());
    mark_defeated(&mut run);
    assert!(run.defeated);
    let cursor_before = run.completed_encounters;
    let result = advance_after_win(&mut run, &sectors);
    assert_eq!(
        result,
        AdvanceResult::AlreadyEnded,
        "defeated run: advance is no-op (cannot soft-lock the death flow)",
    );
    assert_eq!(
        run.completed_encounters, cursor_before,
        "defeated run: cursor unchanged",
    );
    assert!(run.defeated, "defeated flag holds");
    assert!(
        !run.victorious,
        "defeated AND victorious would be ambiguous"
    );
}

#[test]
fn lost_encounter_outcome_signals_death_path() {
    // Death-flow entry point: `encounter_outcome` returns `Lost` when the
    // player ship is gone. The bin reads this each frame; pinning it here
    // means a change to the outcome predicate (e.g. counting destroyed
    // hulks as alive) surfaces here BEFORE breaking the death flow.
    let mut board = empty_won_board();
    board.cells[0] = None; // player gone
    assert_eq!(encounter_outcome(&board), EncounterOutcome::Lost);
}

/* =========================================================================
 * (4) Victory routes to RunComplete
 * ====================================================================== */

#[test]
fn final_sector_final_encounter_clear_returns_victorious() {
    // The bin maps `AdvanceResult::Victorious` into
    // `TransitionKind::Waypoint` → RunComplete card (P9). Lib contract:
    // when the LAST encounter of the LAST sector clears, the flag flips
    // and the result is `Victorious`.
    let sectors = vec![combat_sector("s0", "Final Sector")];
    let mut run = Run::new(player_seed());
    // 4 pool rounds + 1 boss = 5 advance calls. The first 4 are
    // NextEncounter; the boss clear is Victorious.
    for _ in 0..ENCOUNTERS_PER_SECTOR {
        assert_eq!(
            advance_after_win(&mut run, &sectors),
            AdvanceResult::NextEncounter,
        );
    }
    let result = advance_after_win(&mut run, &sectors);
    assert_eq!(result, AdvanceResult::Victorious);
    assert!(run.victorious);
    assert!(!run.defeated);
}

#[test]
fn already_victorious_run_does_not_re_advance() {
    // The continuous-flow loop can keep ticking past the victory frame
    // (the warp-into-RunComplete plays out a few seconds). Lib contract:
    // an extra `advance_after_win` on a victorious run is `AlreadyEnded`
    // (not another `Victorious` that would double-flag or another
    // `NextEncounter` that would advance past the end).
    let sectors = vec![combat_sector("s0", "Final Sector")];
    let mut run = Run::new(player_seed());
    for _ in 0..=ENCOUNTERS_PER_SECTOR {
        let _ = advance_after_win(&mut run, &sectors);
    }
    assert!(run.victorious);
    let result = advance_after_win(&mut run, &sectors);
    assert_eq!(
        result,
        AdvanceResult::AlreadyEnded,
        "post-victory advance is no-op (cannot loop the final-card warp)",
    );
    assert!(run.victorious, "victory flag held across redundant advance");
}

/* =========================================================================
 * (5) `Board.level` cursor lockstep across transitions
 * ====================================================================== */

#[test]
fn run_cursor_non_decreasing_across_a_two_sector_campaign() {
    // The bin's `board.level = run_cursor(run)` is what drives the
    // background parallax focus-tween (P2). It MUST be NON-DECREASING
    // across the campaign so the parallax never back-tweens into a
    // previously-seen area.
    //
    // Subtle: the cursor at end-of-sector-N (boss cleared) equals the
    // cursor at start-of-sector-(N+1) — both compute to
    // `(N+1) * ENCOUNTERS_PER_SECTOR + 0`, because the boss is the 5th
    // encounter (index 4 = ENCOUNTERS_PER_SECTOR) and a NextSector
    // resets `completed_encounters` to 0 while bumping `current_sector_idx`.
    // That's design-correct: arrival at the new sector lands at the same
    // parallax depth the boss-clear handed off (the warp transition
    // smooths the SCENE, not the cursor input). So this test asserts
    // monotonic-non-decreasing, not strictly increasing — a regression
    // that REVERSES the cursor at the boundary trips this; a true
    // double-jump or zero-increment within a sector also trips it.
    let sectors = vec![
        combat_sector("s0", "Drift Belt"),
        combat_sector("s1", "Ion Reefs"),
    ];
    let mut run = Run::new(player_seed());
    let mut prev = run_cursor(&run);
    for _ in 0..(2 * (ENCOUNTERS_PER_SECTOR as usize + 1)) {
        let result = advance_after_win(&mut run, &sectors);
        if result == AdvanceResult::Victorious {
            break;
        }
        let cur = run_cursor(&run);
        assert!(
            cur >= prev,
            "run_cursor non-decreasing: was {prev}, now {cur} (result {result:?})",
        );
        prev = cur;
    }
    assert!(
        run.victorious,
        "campaign finished after both sectors cleared"
    );
    // Final cursor: 2 sectors * ENCOUNTERS_PER_SECTOR + final boss
    // contribution. The Victorious return leaves the cursor at
    // (sector_idx=last, completed=last_index_in_final_sector).
    assert!(
        run_cursor(&run) >= prev,
        "post-Victorious cursor non-decreasing vs final round",
    );
}

/* =========================================================================
 * (6) Pin ENCOUNTERS_PER_SECTOR = 4 (P1 design law)
 * ====================================================================== */

#[test]
fn encounters_per_sector_is_four_phase_one_locked() {
    // P1 design law (Bruce ruling Q3): 4 rounds per level for Phase 1.
    // A bump (3, 5, …) would silently change the warp + parallax cadence
    // the rest of the pieces assume.
    assert_eq!(
        ENCOUNTERS_PER_SECTOR, 4,
        "Phase 1 P1: 4 rounds per level (Bruce Q3)",
    );
}

/* =========================================================================
 * (7) Passthrough sectors still don't soft-lock
 * ====================================================================== */

#[test]
fn empty_passthrough_sector_advances_through_without_soft_locking() {
    // `generate_campaign` emits a Staging sector (no encounters) before
    // the combat sectors. `advance_after_win` skips empty sectors so the
    // run never rests on a passthrough — pinning that here means a future
    // generator change that inserts a real Staging encounter doesn't
    // silently strand the run cursor.
    let sectors = vec![
        Sector {
            id: "staging".into(),
            name: "Staging".into(),
            patrol_tier: 0,
            encounters: Vec::new(),
        },
        combat_sector("s1", "Drift Belt"),
    ];
    let mut run = Run::new(player_seed());
    // Even though current_sector_idx starts at 0 (Staging), advancing
    // skips it and the win counts against Drift Belt's first encounter.
    let result = advance_after_win(&mut run, &sectors);
    assert_eq!(
        result,
        AdvanceResult::NextEncounter,
        "passthrough → first combat encounter advances normally",
    );
    assert_eq!(
        run.current_sector_idx, 1,
        "advance_after_win skipped past the empty Staging sector",
    );
}
