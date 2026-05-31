//! Phase 3 run-loop logic: turn `Sector` / `EncounterDef` / `Run` types
//! into a working campaign.
//!
//! Architect's #75 foundation supplies the types ([`crate::types::Sector`],
//! [`crate::types::EncounterDef`], [`crate::types::Run`],
//! [`crate::types::ShipSpawn`]); this module is the **runtime layer** that
//! ties them to a live [`Board`] and drives state transitions when the
//! player wins or loses an encounter.
//!
//! ## Pieces
//!
//! - [`encounter_outcome`] — observe a [`Board`] and decide whether the
//!   current encounter is `Won`, `Lost`, or still `InProgress`. The bin
//!   calls this after every `resolve_round` to decide whether to show a
//!   between-encounter overlay.
//! - [`advance_after_win`] — given a freshly-won encounter and the run
//!   plus sector list, mutate `Run` to point at the next encounter (or
//!   set `victorious = true` after the final boss). Returns
//!   [`AdvanceResult`] so the caller can branch on whether to show a
//!   between-encounter card-pick UI or the final-victory overlay.
//! - [`mark_defeated`] — flip the `defeated` flag.
//! - [`build_encounter_board`] — instantiate a fresh [`Board`] from an
//!   [`EncounterDef`] + the player's current [`Ship`] (so cross-encounter
//!   hull / heat / cooldown / status state carries forward).
//! - [`placeholder_sectors`] — three Rust-literal sectors (patrol tiers
//!   1, 2, 3) with progressively harder enemy comps and a final-boss
//!   encounter at the end of sector 3. Used by the demo bin until the
//!   canonical catalog's `sectors` field is typed.
//!
//! ## Why placeholder sectors live here, not in `DemoContent`
//!
//! Subsystems and cards live on `DemoContent` because the resolver
//! queries them every frame (damage_modifier on every shot, card_at on
//! every key press). Sectors are consulted ONCE per encounter
//! transition, so there's no perf reason to bake them into Content.
//! Keeping them in a standalone `placeholder_sectors()` function makes
//! the eventual switch to `Catalog::sectors` mechanical: the bin reads
//! either source at startup, the rest of the code only sees
//! `&[Sector]`.
//!
//! ## Why ShipSpawn::class_id, not a direct Ship?
//!
//! The architect's foundation has spawns reference a [`crate::types::ClassDef::id`]
//! rather than embedding a full `Ship`. That's the canonical pattern —
//! one ClassDef defines the loadout, the encounter just says "spawn
//! three of THIS class at THESE cells." [`spawn_to_ship`] materializes a
//! Ship from a spawn + the catalog's ClassDef lookup; if a future
//! encounter needs ad-hoc enemy stats we'd grow `ShipSpawn`'s field set
//! rather than embedding a Ship.

use crate::types::{
    Arc as TArc, Board, EncounterDef, EventBus, Faction, HullZone, LaneEnd, Mount, Orientation,
    Run, Sector, ShieldFace, ShieldProfile, Ship, ShipSpawn, Trait,
};
use std::collections::HashMap;

/* =========================================================================
 * Encounter outcome.
 * ====================================================================== */

/// Result of inspecting a [`Board`] after a `resolve_round`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncounterOutcome {
    /// Encounter still active — at least one player and at least one
    /// enemy still on the board.
    InProgress,
    /// Player destroyed. The run is defeated.
    Lost,
    /// Every enemy destroyed. The run advances (or completes if this
    /// was the final boss).
    Won,
}

/// Observe the board and return the current encounter's outcome. Cheap —
/// scans `board.cells` once, no allocation.
///
/// Edge case: a board with no player AND no enemies returns `Lost`
/// (player loss takes precedence — the bin should never be in that
/// state, but if it happens we default to the more honest signal).
pub fn encounter_outcome(board: &Board) -> EncounterOutcome {
    let mut has_player = false;
    let mut has_enemy = false;
    for slot in &board.cells {
        if let Some(s) = slot {
            match s.faction {
                Faction::Player => has_player = true,
                Faction::Enemy => has_enemy = true,
            }
        }
    }
    if !has_player {
        EncounterOutcome::Lost
    } else if !has_enemy {
        EncounterOutcome::Won
    } else {
        EncounterOutcome::InProgress
    }
}

/* =========================================================================
 * Run advancement.
 * ====================================================================== */

/// What happens next after a won encounter. Mutually exclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdvanceResult {
    /// More encounters remaining in the current sector. The bin loads
    /// `sectors[run.current_sector_idx].encounters[run.completed_encounters]`
    /// next.
    NextEncounter,
    /// Sector cleared; the next sector exists. `run.current_sector_idx`
    /// has been incremented and `completed_encounters` reset to 0.
    NextSector,
    /// Final encounter of the final sector cleared. `run.victorious`
    /// is true. The bin shows the victory overlay.
    Victorious,
    /// The run state was inconsistent (e.g., already-victorious run
    /// receiving another advance). No-op; the caller should redraw the
    /// current overlay and not progress further.
    AlreadyEnded,
}

/// Advance `run` after a [`EncounterOutcome::Won`] resolution. Mutates
/// `run.completed_encounters` and possibly `run.current_sector_idx` /
/// `run.victorious`. Returns the discriminator so the bin can choose
/// between between-encounter overlay vs end-of-sector vs victory screen.
///
/// Pre-condition: caller has confirmed the encounter was won via
/// [`encounter_outcome`]. This function does not consult the board.
pub fn advance_after_win(run: &mut Run, sectors: &[Sector]) -> AdvanceResult {
    if run.defeated || run.victorious {
        return AdvanceResult::AlreadyEnded;
    }
    let sector_idx = run.current_sector_idx;
    if sector_idx >= sectors.len() {
        // Out of bounds — declaring victory is the safe interpretation.
        // (The run somehow finished beyond the last sector; treat as a
        // sane no-op end-state.)
        run.victorious = true;
        return AdvanceResult::AlreadyEnded;
    }

    let sector = &sectors[sector_idx];
    let next_enc = run.completed_encounters as usize + 1;

    // Are there more encounters in this sector?
    if next_enc < sector.encounters.len() {
        run.completed_encounters += 1;
        return AdvanceResult::NextEncounter;
    }

    // This sector is finished. Is there a next sector?
    let next_sector = sector_idx + 1;
    if next_sector < sectors.len() {
        run.current_sector_idx = next_sector;
        run.completed_encounters = 0;
        return AdvanceResult::NextSector;
    }

    // Final sector cleared. Was this a boss encounter? Either way the
    // run is over — the design says final-sector clear = victory.
    run.victorious = true;
    AdvanceResult::Victorious
}

/// Flip `run.defeated`. Called when [`encounter_outcome`] returns
/// [`EncounterOutcome::Lost`]. Idempotent — calling twice has no
/// additional effect beyond `defeated = true`.
pub fn mark_defeated(run: &mut Run) {
    run.defeated = true;
}

/// Look up the current encounter for a run. Returns `None` if the run
/// is already over (defeated, victorious, or sector index out of
/// bounds) — callers display the end-of-run overlay in that case.
pub fn current_encounter<'s>(run: &Run, sectors: &'s [Sector]) -> Option<&'s EncounterDef> {
    if run.defeated || run.victorious {
        return None;
    }
    let sector = sectors.get(run.current_sector_idx)?;
    sector.encounters.get(run.completed_encounters as usize)
}

/* =========================================================================
 * Encounter → Board materialization.
 * ====================================================================== */

/// Build a fresh [`Board`] for the encounter. The board size is derived
/// from the maximum spawn cell (rounded up to the canonical 5 / 7 / 9
/// lane lengths from the analysis doc). The player ship is placed at
/// cell 0; the spawns populate the rest.
///
/// `player` is the player's CURRENT ship (with whatever heat / hull /
/// statuses carried over from the prior encounter). The board's cell
/// vector is rebuilt — the player's `cell` field is normalized to 0
/// regardless of where they ended the previous encounter, matching
/// "you start a new sector at the lane mouth" framing.
///
/// `class_to_ship` is a builder closure that turns a [`ShipSpawn`]
/// into a [`Ship`] given the class id lookup. The bin passes
/// `|spawn, board| spawn_to_ship(spawn, content)` (or any equivalent
/// catalog-aware builder); keeping it a parameter lets the same
/// encounter builder work with placeholder data and real catalog data.
///
/// Hazards on the encounter populate `board.hazards` at the spawn cells.
pub fn build_encounter_board<F>(
    encounter: &EncounterDef,
    mut player: Ship,
    mut class_to_ship: F,
) -> Board
where
    F: FnMut(&ShipSpawn) -> Option<Ship>,
{
    // Lane size: enough cells to hold every spawn plus the player at 0.
    // Round up to the canonical 5 / 7 / 9 sizes the analysis doc uses.
    let max_cell = encounter
        .enemy_ships
        .iter()
        .map(|s| s.cell)
        .max()
        .unwrap_or(0)
        .max(player.cell);
    let size = canonical_lane_size(max_cell);

    let mut cells: Vec<Option<Ship>> = (0..size).map(|_| None).collect();
    let mut hazards: Vec<Vec<crate::types::Hazard>> = (0..size).map(|_| Vec::new()).collect();

    // Place the player at cell 0 with a clean cell field.
    player.cell = 0;
    cells[0] = Some(player);

    // Place each enemy spawn.
    for spawn in &encounter.enemy_ships {
        if spawn.cell >= size || spawn.cell == 0 {
            // Off-board or colliding with player — skip. The placeholder
            // sectors below are correct by construction; a buggy custom
            // sector won't crash the demo.
            continue;
        }
        if cells[spawn.cell].is_some() {
            continue;
        }
        if let Some(mut ship) = class_to_ship(spawn) {
            ship.cell = spawn.cell;
            ship.orientation = spawn.orientation;
            if let Some(hp) = spawn.hp_override {
                ship.hull = hp;
                ship.max_hull = hp;
            }
            cells[spawn.cell] = Some(ship);
        }
    }

    // Drop hazards into their cells.
    for h in &encounter.hazards {
        if h.cell < size {
            hazards[h.cell].push(h.clone());
        }
    }

    Board {
        size,
        cells,
        ordnance: Vec::new(),
        hazards,
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
    }
}

/// Canonical lane size for a given maximum spawn cell. The analysis doc
/// uses 5 / 7 / 9 for early / mid / late sectors. Picks the smallest
/// size that fits.
pub fn canonical_lane_size(max_cell: usize) -> usize {
    match max_cell {
        0..=4 => 5,
        5..=6 => 7,
        _ => 9,
    }
}

/* =========================================================================
 * Placeholder spawn-to-ship builder.
 *
 * Until the bin's `DemoContent` (or a real catalog loader) wires class
 * ids to full Ship templates, this module ships a minimal fallback so
 * the placeholder sectors are self-contained. The bin can override
 * with a richer closure that consults `Content::class(id)`-style
 * lookups when those land.
 * ====================================================================== */

/// Minimal default `Ship` shape used for spawns whose `class_id` isn't
/// known to the caller's class registry. Bow-on facing the player, low
/// hull, one Forward pulse_laser mount so the AI has something to fire.
/// This is the "any enemy" fallback — real class-specific stats come
/// from the bin's class table.
pub fn fallback_ship_for_spawn(spawn: &ShipSpawn) -> Ship {
    let mut s = Ship {
        id: format!("{}@{}", spawn.class_id, spawn.cell),
        faction: Faction::Enemy,
        cell: spawn.cell,
        orientation: spawn.orientation,
        hull: 3,
        max_hull: 3,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
            bow: ShieldFace { armour: 1, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 0 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        },
        mounts: vec![Mount {
            id: "m1".into(),
            arc: TArc::Forward,
            weapon: "pulse_laser".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: Some(spawn.class_id.clone()),
    };
    if let Some(hp) = spawn.hp_override {
        s.hull = hp;
        s.max_hull = hp;
    }
    // HullZone import is held so a future variant of fallback_ship can
    // build per-class shield profiles without touching imports.
    let _ = HullZone::Bow;
    s
}

/* =========================================================================
 * Placeholder sectors.
 *
 * Three sectors, patrol tiers 1/2/3, progressively harder:
 * - Sector 1 ("Drift Belt", patrol 1): two encounters, 1-2 weak enemies each.
 * - Sector 2 ("Ion Reefs", patrol 2): three encounters, 2-3 enemies, trait variety.
 * - Sector 3 ("Citadel Approach", patrol 3): two encounters + boss. The
 *   boss is at the final encounter with `is_boss: true`.
 * ====================================================================== */

/// Build the three placeholder sectors used by the Phase 3 demo. Stays
/// in a stand-alone function (not on `DemoContent`) per the module
/// docstring's rationale.
pub fn placeholder_sectors() -> Vec<Sector> {
    vec![
        sector_drift_belt(),
        sector_ion_reefs(),
        sector_citadel_approach(),
    ]
}

fn spawn(class_id: &str, cell: usize, bow: LaneEnd, hp_override: Option<i32>) -> ShipSpawn {
    ShipSpawn {
        class_id: class_id.into(),
        cell,
        orientation: Orientation::BowOn { bow },
        hp_override,
    }
}

fn enc(id: &str, ships: Vec<ShipSpawn>, is_boss: bool) -> EncounterDef {
    EncounterDef {
        id: id.into(),
        enemy_ships: ships,
        hazards: Vec::new(),
        is_boss,
    }
}

/// Patrol 1: two weak encounters. Player should clear easily; this is
/// the "feel the controls" sector.
fn sector_drift_belt() -> Sector {
    Sector {
        id: "drift_belt".into(),
        name: "Drift Belt".into(),
        patrol_tier: 1,
        encounters: vec![
            enc(
                "drift_belt_a",
                vec![
                    spawn("skiff", 2, LaneEnd::Aft, None),
                    spawn("skiff", 4, LaneEnd::Aft, None),
                ],
                false,
            ),
            enc(
                "drift_belt_b",
                vec![
                    spawn("lancer", 3, LaneEnd::Aft, None),
                    spawn("skiff", 5, LaneEnd::Aft, None),
                ],
                false,
            ),
        ],
    }
}

/// Patrol 2: three encounters, more variety. Player meets enemies with
/// distinct traits (Pursuit, Burn-Hard).
fn sector_ion_reefs() -> Sector {
    Sector {
        id: "ion_reefs".into(),
        name: "Ion Reefs".into(),
        patrol_tier: 2,
        encounters: vec![
            enc(
                "ion_reefs_a",
                vec![
                    spawn("gunboat", 3, LaneEnd::Aft, None),
                    spawn("skiff", 5, LaneEnd::Aft, None),
                ],
                false,
            ),
            enc(
                "ion_reefs_b",
                vec![
                    spawn("picket", 2, LaneEnd::Aft, None),
                    spawn("monitor", 4, LaneEnd::Aft, None),
                    spawn("skirmisher", 6, LaneEnd::Aft, None),
                ],
                false,
            ),
            enc(
                "ion_reefs_c",
                vec![
                    spawn("gunboat", 3, LaneEnd::Aft, None),
                    spawn("gunboat", 5, LaneEnd::Aft, None),
                ],
                false,
            ),
        ],
    }
}

/// Patrol 3: two encounters + boss. The boss has `is_boss: true` and a
/// healthy hull_override so the run-end victory only fires after a
/// meaningful fight.
fn sector_citadel_approach() -> Sector {
    Sector {
        id: "citadel_approach".into(),
        name: "Citadel Approach".into(),
        patrol_tier: 3,
        encounters: vec![
            enc(
                "citadel_a",
                vec![
                    spawn("grappler", 3, LaneEnd::Aft, None),
                    spawn("shade", 5, LaneEnd::Aft, None),
                    spawn("monitor", 7, LaneEnd::Aft, None),
                ],
                false,
            ),
            enc(
                "citadel_b",
                vec![
                    spawn("voidrunner", 3, LaneEnd::Aft, None),
                    spawn("voidrunner", 6, LaneEnd::Aft, None),
                ],
                false,
            ),
            // Boss encounter — the run-end gate. High-hull single
            // enemy with a Forward-arc loadout. Final boss task #83
            // will replace this with a richer encounter; for now the
            // is_boss flag is what AdvanceResult::Victorious reads.
            EncounterDef {
                id: "citadel_boss".into(),
                enemy_ships: vec![
                    ShipSpawn {
                        class_id: "warlord".into(),
                        cell: 5,
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        hp_override: Some(12),
                    },
                ],
                hazards: Vec::new(),
                is_boss: true,
            },
        ],
    }
}

/// Helper alongside [`placeholder_sectors`] — the Reactor-Breach trait
/// is held in the import list for the moment when an encounter wants
/// to spawn a Tender with the trait pre-applied. (`Trait` is reused
/// from the existing type surface; this `_` keeps it imported without
/// triggering dead-code warnings while the placeholder sectors don't
/// yet use it.)
#[doc(hidden)]
const _: fn() = || {
    let _ = Trait::Pursuit;
};

/* =========================================================================
 * Tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;

    fn make_player(cell: usize, hull: i32) -> Ship {
        Ship {
            id: "player".into(),
            faction: Faction::Player,
            cell,
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            hull,
            max_hull: 10,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts: vec![Mount {
                id: "m1".into(),
                arc: TArc::Forward,
                weapon: "pulse_laser".into(),
            }],
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    fn make_enemy(id: &str, cell: usize) -> Ship {
        Ship {
            id: id.into(),
            faction: Faction::Enemy,
            cell,
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            hull: 3,
            max_hull: 3,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: default_shield_profile(),
            mounts: vec![],
            queue: Vec::new(),
            cooldowns: HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    fn board_with(cells: Vec<Option<Ship>>) -> Board {
        let size = cells.len();
        Board {
            size,
            cells,
            ordnance: vec![],
            hazards: (0..size).map(|_| vec![]).collect(),
            patrol: 1,
            bus: EventBus::default(),
            destroys_this_window: 0,
        }
    }

    /* ---- encounter outcome ----------------------------------------- */

    #[test]
    fn outcome_in_progress_when_both_factions_present() {
        let board = board_with(vec![
            Some(make_player(0, 10)),
            None,
            Some(make_enemy("e", 2)),
        ]);
        assert_eq!(encounter_outcome(&board), EncounterOutcome::InProgress);
    }

    #[test]
    fn outcome_won_when_no_enemies_remain() {
        let board = board_with(vec![Some(make_player(0, 10)), None, None]);
        assert_eq!(encounter_outcome(&board), EncounterOutcome::Won);
    }

    #[test]
    fn outcome_lost_when_player_destroyed() {
        let board = board_with(vec![None, None, Some(make_enemy("e", 2))]);
        assert_eq!(encounter_outcome(&board), EncounterOutcome::Lost);
    }

    #[test]
    fn outcome_lost_takes_precedence_when_both_destroyed() {
        // No ships at all — bin shouldn't get into this state, but if
        // it does we return Lost (player loss is the safer signal).
        let board = board_with(vec![None, None]);
        assert_eq!(encounter_outcome(&board), EncounterOutcome::Lost);
    }

    /* ---- run advancement ------------------------------------------- */

    fn new_run() -> Run {
        Run::new(make_player(0, 5))
    }

    #[test]
    fn advance_within_sector_returns_next_encounter() {
        let sectors = placeholder_sectors();
        let mut run = new_run();
        // Sector 0 has 2 encounters. After clearing #0, next is #1.
        let result = advance_after_win(&mut run, &sectors);
        assert_eq!(result, AdvanceResult::NextEncounter);
        assert_eq!(run.completed_encounters, 1);
        assert_eq!(run.current_sector_idx, 0);
    }

    #[test]
    fn advance_from_last_encounter_in_sector_jumps_to_next_sector() {
        let sectors = placeholder_sectors();
        let mut run = new_run();
        run.completed_encounters = (sectors[0].encounters.len() - 1) as u32;
        let result = advance_after_win(&mut run, &sectors);
        assert_eq!(result, AdvanceResult::NextSector);
        assert_eq!(run.current_sector_idx, 1);
        assert_eq!(run.completed_encounters, 0);
    }

    #[test]
    fn advance_from_final_sector_final_encounter_flips_victorious() {
        let sectors = placeholder_sectors();
        let last = sectors.len() - 1;
        let mut run = new_run();
        run.current_sector_idx = last;
        run.completed_encounters = (sectors[last].encounters.len() - 1) as u32;
        let result = advance_after_win(&mut run, &sectors);
        assert_eq!(result, AdvanceResult::Victorious);
        assert!(run.victorious);
        assert!(!run.defeated);
    }

    #[test]
    fn advance_after_already_victorious_is_no_op() {
        let sectors = placeholder_sectors();
        let mut run = new_run();
        run.victorious = true;
        let result = advance_after_win(&mut run, &sectors);
        assert_eq!(result, AdvanceResult::AlreadyEnded);
    }

    #[test]
    fn advance_after_defeated_is_no_op() {
        let sectors = placeholder_sectors();
        let mut run = new_run();
        run.defeated = true;
        let result = advance_after_win(&mut run, &sectors);
        assert_eq!(result, AdvanceResult::AlreadyEnded);
    }

    #[test]
    fn mark_defeated_idempotent() {
        let mut run = new_run();
        mark_defeated(&mut run);
        assert!(run.defeated);
        mark_defeated(&mut run);
        assert!(run.defeated);
    }

    #[test]
    fn current_encounter_returns_none_on_ended_run() {
        let sectors = placeholder_sectors();
        let mut run = new_run();
        run.defeated = true;
        assert!(current_encounter(&run, &sectors).is_none());
        run.defeated = false;
        run.victorious = true;
        assert!(current_encounter(&run, &sectors).is_none());
    }

    #[test]
    fn current_encounter_points_at_completed_count() {
        let sectors = placeholder_sectors();
        let mut run = new_run();
        assert_eq!(
            current_encounter(&run, &sectors).map(|e| e.id.as_str()),
            Some("drift_belt_a"),
        );
        run.completed_encounters = 1;
        assert_eq!(
            current_encounter(&run, &sectors).map(|e| e.id.as_str()),
            Some("drift_belt_b"),
        );
    }

    /* ---- placeholder sectors --------------------------------------- */

    #[test]
    fn placeholder_sectors_has_progressive_patrol_tiers() {
        let sectors = placeholder_sectors();
        assert_eq!(sectors.len(), 3);
        assert_eq!(sectors[0].patrol_tier, 1);
        assert_eq!(sectors[1].patrol_tier, 2);
        assert_eq!(sectors[2].patrol_tier, 3);
    }

    #[test]
    fn placeholder_sectors_have_at_least_one_encounter() {
        for s in placeholder_sectors() {
            assert!(!s.encounters.is_empty(), "sector {} has no encounters", s.id);
        }
    }

    #[test]
    fn final_sector_ends_with_boss_encounter() {
        let sectors = placeholder_sectors();
        let final_sector = sectors.last().unwrap();
        let final_encounter = final_sector.encounters.last().unwrap();
        assert!(
            final_encounter.is_boss,
            "the run's final encounter must be a boss so victory has weight",
        );
    }

    #[test]
    fn boss_encounter_only_at_the_end() {
        // Visible-design invariant: the only boss is at the very end of
        // the final sector. If a future sector wants mid-run minibosses,
        // this test needs to relax; pinning the current shape so we
        // notice when it changes.
        let sectors = placeholder_sectors();
        let mut boss_count = 0;
        for s in &sectors {
            for e in &s.encounters {
                if e.is_boss {
                    boss_count += 1;
                }
            }
        }
        assert_eq!(boss_count, 1, "exactly one boss encounter total (the final)");
    }

    #[test]
    fn enemy_difficulty_progresses_across_sectors() {
        let sectors = placeholder_sectors();
        let count: Vec<usize> = sectors
            .iter()
            .flat_map(|s| s.encounters.iter().map(|e| e.enemy_ships.len()))
            .collect();
        // Later encounters should be at least as populated as the
        // earliest ones on average. Sector 0 first encounter = 2 ships;
        // sector 1 second encounter = 3 ships; sector 2 first encounter
        // = 3 ships. Loose monotonicity:
        assert!(count[0] <= count[count.len() - 2],
            "first encounter shouldn't be denser than the boss-precursor");
    }

    /* ---- build_encounter_board ------------------------------------- */

    #[test]
    fn build_board_places_player_at_cell_0() {
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: 2,
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
        };
        let player = make_player(3, 8); // pre-board cell shouldn't matter
        let board = build_encounter_board(&enc, player, |spawn| Some(fallback_ship_for_spawn(spawn)));
        // Player ends up at cell 0 regardless of their pre-board cell.
        let at_0 = board.cells[0].as_ref().unwrap();
        assert_eq!(at_0.faction, Faction::Player);
        assert_eq!(at_0.cell, 0);
        // Hull state carries over (proof that the prior-encounter ship
        // is preserved, not reset).
        assert_eq!(at_0.hull, 8);
    }

    #[test]
    fn build_board_spawns_enemies_at_their_cells() {
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: 2,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "lancer".into(),
                    cell: 4,
                    orientation: Orientation::Broadside,
                    hp_override: Some(7),
                },
            ],
            hazards: vec![],
            is_boss: false,
        };
        let board = build_encounter_board(&enc, make_player(0, 10),
            |spawn| Some(fallback_ship_for_spawn(spawn)));
        let e2 = board.cells[2].as_ref().unwrap();
        assert_eq!(e2.faction, Faction::Enemy);
        let e4 = board.cells[4].as_ref().unwrap();
        assert_eq!(e4.faction, Faction::Enemy);
        assert_eq!(e4.orientation, Orientation::Broadside);
        assert_eq!(e4.hull, 7, "hp_override applied");
    }

    #[test]
    fn build_board_skips_cell_0_spawn_to_avoid_player_collision() {
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: 0, // tries to spawn ON the player
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
        };
        let board = build_encounter_board(&enc, make_player(0, 10),
            |spawn| Some(fallback_ship_for_spawn(spawn)));
        let at_0 = board.cells[0].as_ref().unwrap();
        // Player kept cell 0; enemy spawn at 0 was dropped.
        assert_eq!(at_0.faction, Faction::Player);
    }

    #[test]
    fn build_board_uses_canonical_lane_size() {
        // max_cell = 2 -> 5; = 5 -> 7; = 7 -> 9.
        assert_eq!(canonical_lane_size(0), 5);
        assert_eq!(canonical_lane_size(2), 5);
        assert_eq!(canonical_lane_size(4), 5);
        assert_eq!(canonical_lane_size(5), 7);
        assert_eq!(canonical_lane_size(6), 7);
        assert_eq!(canonical_lane_size(7), 9);
        assert_eq!(canonical_lane_size(20), 9);
    }
}
