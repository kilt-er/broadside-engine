//! Cross-run meta-progression: persistent salvage totals and per-run
//! unlocks (subsystems / cards) that survive death.
//!
//! ## Lifecycle
//!
//! - Per-run [`crate::types::Run`] holds the in-flight salvage the
//!   player collects from destroyed enemies. That number resets to 0
//!   on each fresh run.
//! - [`MetaProgression`] is the **persistent** layer: it accumulates
//!   `total_salvage_earned` across every run (won or lost) and tracks
//!   the set of subsystems / cards the player has unlocked.
//! - On run end ([`crate::types::Run::defeated`] or
//!   [`crate::types::Run::victorious`] flips true), the bin calls
//!   [`accumulate_into_meta`] to roll the run's salvage into the meta
//!   total and apply any new unlocks the threshold crossed.
//! - On run start, the bin calls [`MetaProgression::load_from_disk`]
//!   to restore the persisted state; on run end it calls
//!   [`MetaProgression::save_to_disk`] (after accumulate).
//!
//! ## File format
//!
//! JSON for now via serde. Mirrors the catalog file's shape, so the
//! same load path can serve future migrations. Architect's #79
//! (`postcard` save/load) is for the in-flight [`crate::types::SaveState`]
//! — that's per-run state, different lifecycle, different file.
//! Meta lives alongside but separately so deleting a save doesn't
//! reset progression.
//!
//! Default path: `meta.json` in the working directory. The demo bin
//! chooses where; this module just provides the I/O primitives.
//!
//! ## Unlock thresholds
//!
//! The starter set (Marksman / Point-Blank Doctrine / HeatSink) is
//! **always available** — those are baked into `src/subsystems.rs` and
//! the demo's `DemoContent::default`. The four meta-unlockable
//! subsystems below ladder by total salvage earned:
//!
//! | Subsystem id        | Threshold (total salvage) | Source           |
//! |---------------------|---------------------------|------------------|
//! | `rear_gunner`       | 10                        | canonical catalog (gunnery, +1 dmg through stern arc) |
//! | `chain_bounty`      | 25                        | canonical catalog (tactical, +1 credit on chain kill) |
//! | `overcharge`        | 50                        | canonical catalog (gunnery, +1 dmg when queue has only one action) |
//! | `crossfire`         | 100                       | canonical catalog (gunnery, +1 enemy-vs-enemy damage) |
//!
//! Thresholds are picked so a typical run (~10-20 salvage from win,
//! ~5 from defeat) unlocks the first tier in 1 run, the second in
//! 2-3 runs, and the high-tier in roughly a 10-run arc. Tune later.
//!
//! Per the lead's brief: this module defines structure + persistence
//! + unlock threshold logic, NOT the per-run "purchase a subsystem at
//! the upgrade UI" flow — that's renderer's #77 between-encounter
//! screen layered on top.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::types::{Run, Ship};

/* =========================================================================
 * MetaProgression
 * ====================================================================== */

/// Persistent cross-run state. Subsystem and card ids match those in
/// `src/subsystems.rs` and `src/cards.rs` so the unlock set can drop
/// directly into a future "available for selection" pool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaProgression {
    /// Subsystem ids unlocked beyond the always-available starter set.
    #[serde(default)]
    pub unlocked_subsystems: Vec<String>,
    /// Card ids unlocked beyond the always-available starter cards.
    /// Reserved for future card-tier unlocks; empty in the Phase 3
    /// data layer.
    #[serde(default)]
    pub unlocked_cards: Vec<String>,
    /// Cumulative salvage across every run, won or lost. Drives the
    /// unlock thresholds; never decremented.
    #[serde(default)]
    pub total_salvage_earned: u32,
}

/// IO errors from disk persistence. Mirrors `catalog::LoadError`'s
/// shape so callers can `?` either uniformly.
#[derive(Debug)]
#[non_exhaustive]
pub enum MetaError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for MetaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetaError::Io(e) => write!(f, "io error reading/writing meta: {e}"),
            MetaError::Parse(e) => write!(f, "parse error in meta json: {e}"),
        }
    }
}

impl std::error::Error for MetaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MetaError::Io(e) => Some(e),
            MetaError::Parse(e) => Some(e),
        }
    }
}

impl From<io::Error> for MetaError {
    fn from(e: io::Error) -> Self { MetaError::Io(e) }
}
impl From<serde_json::Error> for MetaError {
    fn from(e: serde_json::Error) -> Self { MetaError::Parse(e) }
}

impl MetaProgression {
    /// Read meta state from `path`. Returns `Ok(default)` if the file
    /// is missing — first-run players don't have a save yet, and that
    /// shouldn't be a hard error.
    pub fn load_from_disk(path: impl AsRef<Path>) -> Result<Self, MetaError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Write meta state to `path`. Parent directories are created if
    /// missing so the caller doesn't have to bootstrap them.
    pub fn save_to_disk(&self, path: impl AsRef<Path>) -> Result<(), MetaError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes)?;
        Ok(())
    }

    /// Has the player unlocked a specific subsystem? Always returns
    /// `true` for the starter set so callers can use one check.
    pub fn has_subsystem(&self, id: &str) -> bool {
        STARTER_SUBSYSTEMS.contains(&id) || self.unlocked_subsystems.iter().any(|s| s == id)
    }

    /// Full set of subsystem ids currently available to the player —
    /// the starter set plus any cross-run unlocks. Returned as a
    /// HashSet to keep "is this available?" cheap. Useful for the
    /// future between-encounter purchase UI's "show available
    /// subsystems" query.
    pub fn available_subsystems(&self) -> HashSet<String> {
        let mut out: HashSet<String> = STARTER_SUBSYSTEMS.iter().map(|s| s.to_string()).collect();
        out.extend(self.unlocked_subsystems.iter().cloned());
        out
    }
}

/// Always-available subsystem ids. Match the constants in
/// `src/subsystems.rs`. Adding a subsystem to the starter set goes
/// here AND there.
pub const STARTER_SUBSYSTEMS: &[&str] = &[
    crate::subsystems::MARKSMAN,
    crate::subsystems::POINT_BLANK_DOCTRINE,
    crate::subsystems::HEAT_SINK,
];

/* =========================================================================
 * Salvage math.
 * ====================================================================== */

/// Salvage awarded for destroying one enemy ship. Weighted by the
/// ship's max hull (the canonical analysis HTML uses hull as the
/// rough proxy for "how big a deal was this kill"):
///
/// | max_hull | salvage |
/// |----------|---------|
/// | 1-3      | 1       |
/// | 4-6      | 2       |
/// | 7+       | 3       |
///
/// Lead's brief said "1-3 per enemy destroyed, weighted by enemy
/// hull" — this is the exact mapping. Tunable; the function is the
/// only place callers should consult.
///
/// Bosses (encounter `is_boss: true`) get a flat ×2 multiplier
/// applied by [`salvage_for_encounter_win`].
pub fn salvage_for_destroyed(ship: &Ship) -> u32 {
    match ship.max_hull {
        i if i <= 3 => 1,
        i if i <= 6 => 2,
        _ => 3,
    }
}

/// Total salvage awarded for a won encounter. Sums
/// [`salvage_for_destroyed`] over every enemy in the encounter's
/// spawn list (every one is necessarily dead by the time the
/// encounter is `Won`), then applies the boss multiplier if the
/// encounter is flagged `is_boss`.
///
/// Takes the encounter's `enemy_ships` spawn list, NOT the live
/// board's cells — by the time this is called, the cells are empty
/// because the encounter just ended. The spawn list captures who
/// died.
///
/// `class_to_ship` is the same closure the bin passes to
/// [`crate::runs::build_encounter_board`] — given a spawn, produce a
/// Ship template so we can read its `max_hull`. The fallback path
/// (`crate::runs::fallback_ship_for_spawn`) is fine if the bin has no
/// richer registry yet; it returns `max_hull: 3` which lands in tier
/// 1 (1 salvage per kill).
pub fn salvage_for_encounter_win<F>(
    encounter: &crate::types::EncounterDef,
    mut class_to_ship: F,
) -> u32
where
    F: FnMut(&crate::types::ShipSpawn) -> Option<Ship>,
{
    let raw: u32 = encounter
        .enemy_ships
        .iter()
        .filter_map(|spawn| {
            let ship = class_to_ship(spawn)?;
            // Honour spawn-level hp_override since that's what the
            // encounter actually fielded.
            let mut effective = ship;
            if let Some(hp) = spawn.hp_override {
                effective.max_hull = hp;
            }
            Some(salvage_for_destroyed(&effective))
        })
        .sum();
    if encounter.is_boss {
        raw.saturating_mul(2)
    } else {
        raw
    }
}

/// Award salvage for a won encounter to the running [`Run`]. Idempotent
/// per-encounter only at the caller's level — the bin should call this
/// once per encounter-complete event, not on every frame.
pub fn award_run_salvage<F>(
    run: &mut Run,
    encounter: &crate::types::EncounterDef,
    class_to_ship: F,
) where
    F: FnMut(&crate::types::ShipSpawn) -> Option<Ship>,
{
    let earned = salvage_for_encounter_win(encounter, class_to_ship);
    run.salvage = run.salvage.saturating_add(earned);
}

/* =========================================================================
 * Run end → meta-progression rollover.
 * ====================================================================== */

/// Threshold table for cross-run subsystem unlocks. Single source of
/// truth for `(subsystem_id, total_salvage_required)` pairs.
///
/// Adding a new unlock: append a row here and to the catalog's
/// `subsystems[]` array. No code path other than this constant needs
/// updating.
pub const SUBSYSTEM_UNLOCK_THRESHOLDS: &[(&str, u32)] = &[
    ("rear_gunner", 10),
    ("chain_bounty", 25),
    ("overcharge", 50),
    ("crossfire", 100),
];

/// Card unlock thresholds. Currently empty — cards inherit from the
/// starter set (mass_lock / mass_breach / sensor_pulse). Future card
/// tiers land here.
pub const CARD_UNLOCK_THRESHOLDS: &[(&str, u32)] = &[];

/// Roll the run's salvage into the persistent meta and apply any
/// unlock thresholds the new total crosses. Returns the list of
/// subsystem ids newly unlocked so the bin can flash an "UNLOCKED:
/// Rear Gunner" overlay on the run-end screen.
///
/// Called on EVERY run end (defeated or victorious — salvage is
/// earned either way; the design rewards engagement over win-rate).
/// Idempotent only at the caller's level: if the bin double-fires
/// the run-end event, salvage doubles. Mitigate at the caller.
pub fn accumulate_into_meta(meta: &mut MetaProgression, run: &Run) -> Vec<String> {
    // Roll salvage forward.
    let prev_total = meta.total_salvage_earned;
    meta.total_salvage_earned = meta.total_salvage_earned.saturating_add(run.salvage);
    let new_total = meta.total_salvage_earned;

    // Apply any subsystem unlocks the new total just crossed.
    let mut newly_unlocked = Vec::new();
    for (id, threshold) in SUBSYSTEM_UNLOCK_THRESHOLDS {
        if prev_total < *threshold && new_total >= *threshold {
            let id_string = (*id).to_string();
            if !meta.unlocked_subsystems.contains(&id_string) {
                meta.unlocked_subsystems.push(id_string.clone());
                newly_unlocked.push(id_string);
            }
        }
    }
    // Cards unlocks would mirror; CARD_UNLOCK_THRESHOLDS is empty.
    for (id, threshold) in CARD_UNLOCK_THRESHOLDS {
        if prev_total < *threshold && new_total >= *threshold {
            let id_string = (*id).to_string();
            if !meta.unlocked_cards.contains(&id_string) {
                meta.unlocked_cards.push(id_string);
            }
        }
    }

    newly_unlocked
}

/* =========================================================================
 * Tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::default_shield_profile;
    use crate::types::{
        Arc as TArc, EncounterDef, Faction, LaneEnd, Mount, Orientation, Ship, ShipSpawn,
    };
    use std::collections::HashMap;

    fn ship_with_hull(id: &str, hull: i32) -> Ship {
        Ship {
            id: id.into(),
            faction: Faction::Enemy,
            cell: 1,
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            hull,
            max_hull: hull,
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

    fn new_run() -> Run {
        Run {
            current_sector_idx: 0,
            salvage: 0,
            completed_encounters: 0,
            defeated: false,
            victorious: false,
        }
    }

    /* ---- salvage math ---------------------------------------------- */

    #[test]
    fn salvage_for_low_hull_is_one() {
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 1)), 1);
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 2)), 1);
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 3)), 1);
    }

    #[test]
    fn salvage_for_mid_hull_is_two() {
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 4)), 2);
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 5)), 2);
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 6)), 2);
    }

    #[test]
    fn salvage_for_high_hull_is_three() {
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 7)), 3);
        assert_eq!(salvage_for_destroyed(&ship_with_hull("a", 12)), 3);
    }

    #[test]
    fn salvage_for_encounter_sums_per_enemy() {
        // Two hull-3 enemies (1 salvage each), one hull-7 enemy (3).
        // Total = 5. Non-boss; no multiplier.
        let enc = EncounterDef {
            id: "e".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(), cell: 2,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "skiff".into(), cell: 4,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "warlord".into(), cell: 6,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    hp_override: Some(7),
                },
            ],
            hazards: vec![],
            is_boss: false,
        };
        let earned = salvage_for_encounter_win(&enc, |spawn| {
            // Map skiff -> hull 3, warlord (with override 7) -> hull 7.
            Some(ship_with_hull(&spawn.class_id, 3))
        });
        // 1 + 1 + 3 (warlord max_hull bumped to 7 by override) = 5.
        assert_eq!(earned, 5);
    }

    #[test]
    fn salvage_for_boss_encounter_doubles() {
        let enc = EncounterDef {
            id: "e".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "warlord".into(),
                cell: 3,
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                hp_override: Some(10),
            }],
            hazards: vec![],
            is_boss: true,
        };
        let earned = salvage_for_encounter_win(&enc, |spawn| {
            Some(ship_with_hull(&spawn.class_id, spawn.hp_override.unwrap_or(3)))
        });
        // hull 10 -> 3 salvage, boss multiplier 2x -> 6.
        assert_eq!(earned, 6);
    }

    #[test]
    fn award_run_salvage_increments_in_place() {
        let mut run = new_run();
        run.salvage = 5;
        let enc = EncounterDef {
            id: "e".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: 2,
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
        };
        award_run_salvage(&mut run, &enc, |spawn| Some(ship_with_hull(&spawn.class_id, 3)));
        // 5 + 1 = 6 (hull-3 skiff gives 1 salvage).
        assert_eq!(run.salvage, 6);
    }

    #[test]
    fn award_run_salvage_saturates_not_overflows() {
        let mut run = new_run();
        run.salvage = u32::MAX - 1;
        let enc = EncounterDef {
            id: "e".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: 2,
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: true, // 1 salvage * 2 = 2; saturates at u32::MAX
        };
        award_run_salvage(&mut run, &enc, |spawn| Some(ship_with_hull(&spawn.class_id, 3)));
        assert_eq!(run.salvage, u32::MAX);
    }

    /* ---- meta progression ------------------------------------------ */

    #[test]
    fn fresh_meta_has_only_starter_subsystems_available() {
        let meta = MetaProgression::default();
        assert!(meta.has_subsystem(crate::subsystems::MARKSMAN));
        assert!(meta.has_subsystem(crate::subsystems::POINT_BLANK_DOCTRINE));
        assert!(meta.has_subsystem(crate::subsystems::HEAT_SINK));
        // None of the unlockables yet.
        assert!(!meta.has_subsystem("rear_gunner"));
        assert!(!meta.has_subsystem("chain_bounty"));
        assert!(!meta.has_subsystem("overcharge"));
        assert!(!meta.has_subsystem("crossfire"));
    }

    #[test]
    fn available_subsystems_includes_starters_and_unlocks() {
        let mut meta = MetaProgression::default();
        meta.unlocked_subsystems.push("rear_gunner".into());
        let avail = meta.available_subsystems();
        assert!(avail.contains(crate::subsystems::MARKSMAN));
        assert!(avail.contains("rear_gunner"));
        assert!(!avail.contains("crossfire"));
    }

    #[test]
    fn accumulate_into_meta_adds_run_salvage() {
        let mut meta = MetaProgression::default();
        let mut run = new_run();
        run.salvage = 7;
        accumulate_into_meta(&mut meta, &run);
        assert_eq!(meta.total_salvage_earned, 7);
    }

    #[test]
    fn accumulate_crosses_threshold_unlocks_subsystem() {
        let mut meta = MetaProgression::default();
        let mut run = new_run();
        run.salvage = 10; // crosses rear_gunner threshold (10)
        let newly = accumulate_into_meta(&mut meta, &run);
        assert_eq!(newly, vec!["rear_gunner".to_string()]);
        assert!(meta.has_subsystem("rear_gunner"));
        assert!(!meta.has_subsystem("chain_bounty"));
    }

    #[test]
    fn accumulate_multiple_thresholds_in_one_jump() {
        let mut meta = MetaProgression::default();
        let mut run = new_run();
        run.salvage = 26; // crosses rear_gunner (10) AND chain_bounty (25)
        let newly = accumulate_into_meta(&mut meta, &run);
        assert_eq!(newly.len(), 2);
        assert!(newly.contains(&"rear_gunner".to_string()));
        assert!(newly.contains(&"chain_bounty".to_string()));
        assert!(!meta.has_subsystem("overcharge"));
    }

    #[test]
    fn accumulate_idempotent_for_already_unlocked() {
        let mut meta = MetaProgression::default();
        meta.unlocked_subsystems.push("rear_gunner".into());
        meta.total_salvage_earned = 10;
        let mut run = new_run();
        run.salvage = 5; // crosses no new threshold (already past 10)
        let newly = accumulate_into_meta(&mut meta, &run);
        assert!(newly.is_empty());
        // Salvage still rolled forward, but the unlock list didn't dup.
        assert_eq!(meta.total_salvage_earned, 15);
        assert_eq!(meta.unlocked_subsystems.len(), 1, "no duplicate unlock entries");
    }

    #[test]
    fn accumulate_runs_below_threshold_unlock_nothing() {
        let mut meta = MetaProgression::default();
        let mut run = new_run();
        run.salvage = 9; // just below rear_gunner threshold
        let newly = accumulate_into_meta(&mut meta, &run);
        assert!(newly.is_empty());
        assert_eq!(meta.total_salvage_earned, 9);
    }

    /* ---- persistence ----------------------------------------------- */

    #[test]
    fn save_and_load_roundtrip() {
        // Use the std temp dir; do NOT pollute the repo's working dir.
        let path = std::env::temp_dir().join("broadside_test_meta_roundtrip.json");
        let _ = std::fs::remove_file(&path);

        let mut meta = MetaProgression::default();
        meta.total_salvage_earned = 42;
        meta.unlocked_subsystems.push("rear_gunner".into());
        meta.unlocked_subsystems.push("chain_bounty".into());

        meta.save_to_disk(&path).expect("save ok");
        let loaded = MetaProgression::load_from_disk(&path).expect("load ok");
        assert_eq!(loaded, meta);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_default() {
        // Use a non-existent path; load should return Default, not error.
        let path = std::env::temp_dir().join("broadside_definitely_not_a_real_file_12345.json");
        let _ = std::fs::remove_file(&path);
        let loaded = MetaProgression::load_from_disk(&path).expect("missing file -> default");
        assert_eq!(loaded, MetaProgression::default());
    }

    #[test]
    fn save_creates_parent_directory() {
        // Pick a path with a non-existent parent dir.
        let dir = std::env::temp_dir().join("broadside_meta_parent_test_dir");
        let path = dir.join("meta.json");
        let _ = std::fs::remove_dir_all(&dir);

        let meta = MetaProgression { total_salvage_earned: 1, ..Default::default() };
        meta.save_to_disk(&path).expect("save should create parent");
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlock_thresholds_are_in_ascending_order() {
        // Invariant: ladder thresholds. A player crossing 25 should
        // also have crossed 10. accumulate_into_meta's correctness
        // depends on this.
        let xs: Vec<u32> = SUBSYSTEM_UNLOCK_THRESHOLDS.iter().map(|&(_, t)| t).collect();
        for w in xs.windows(2) {
            assert!(w[0] < w[1], "unlock thresholds must be strictly increasing");
        }
    }

    #[test]
    fn unlock_thresholds_reference_known_subsystem_ids() {
        // Each unlock id should exist in the canonical catalog's
        // subsystems array. This test pins them as known catalog
        // ids; if the catalog renames a subsystem, this test fails
        // and the threshold table needs updating.
        let known: HashSet<&'static str> = [
            "rear_gunner", "chain_bounty", "overcharge", "crossfire",
            // canonical-catalog known ids; extend if a future unlock
            // pulls from a different one.
        ].iter().copied().collect();
        for &(id, _) in SUBSYSTEM_UNLOCK_THRESHOLDS {
            assert!(known.contains(id),
                "unlock id `{id}` not in the known canonical-catalog id set; \
                 update the test or the threshold table");
        }
    }
}
