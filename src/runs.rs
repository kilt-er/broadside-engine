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
    Arc as TArc, Board, Catalog, EncounterDef, EventBus, Faction, HullZone, LaneEnd, Mount,
    Orientation, Run, Sector, SectorDef, ShieldFace, ShieldProfile, Ship, ShipSpawn, Trait,
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
    for s in board.cells.iter().flatten() {
        match s.faction {
            Faction::Player => has_player = true,
            Faction::Enemy => has_enemy = true,
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
    // Skip past empty passthrough sectors (e.g. Staging) so the win is
    // accounted against the sector `current_encounter` actually served, and
    // the run never rests on a sector that has no encounters to fight.
    while run.current_sector_idx < sectors.len()
        && sectors[run.current_sector_idx].encounters.is_empty()
    {
        run.current_sector_idx += 1;
        run.completed_encounters = 0;
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
    // The encounter at the current offset within the current sector...
    if let Some(enc) = sectors
        .get(run.current_sector_idx)
        .and_then(|s| s.encounters.get(run.completed_encounters as usize))
    {
        return Some(enc);
    }
    // ...or, when the current sector is an empty passthrough (Staging), the
    // first encounter of the next non-empty sector. `advance_after_win`
    // keeps the run from resting on an empty sector, so this only fires for
    // a run that opens on one.
    let next = run.current_sector_idx.saturating_add(1);
    sectors
        .get(next..)
        .into_iter()
        .flatten()
        .find_map(|s| s.encounters.first())
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

    // #72: place the player at the MIDDLE of the lane, not the edge. A
    // mid-lane player can be threatened from BOTH ends — they must rotate to
    // keep their armoured face toward whichever side is closing, instead of
    // edge-camping one direction. resolver's #68 close-move already pulls
    // enemies in from both sides, so the spawn distribution (pincer) +
    // mid-start together make the fight directional.
    let player_cell = player_start_cell(size);
    player.cell = player_cell;
    cells[player_cell] = Some(player);

    // Place each enemy spawn.
    for spawn in &encounter.enemy_ships {
        if spawn.cell >= size || spawn.cell == player_cell {
            // Off-board or colliding with the (mid-lane) player — skip. The
            // placeholder sectors below are correct by construction; a buggy
            // custom sector won't crash the demo.
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
        fire_events: Vec::new(),
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

/// The player's starting cell for a lane of `size`: the MIDDLE cell (#72).
/// Lane 5 → 2, 7 → 3, 9 → 4 (integer `size / 2`). A mid-lane start means the
/// player can be threatened from both ends and must rotate to face whichever
/// side is closing — bruce's "start in the middle, much more challenging."
/// Used by [`build_encounter_board`] (placement) and [`sample_encounter_spawns`]
/// (pincer distribution + facing) so both agree on where the player is.
pub fn player_start_cell(size: usize) -> usize {
    size / 2
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

/// Build the final-boss [`Ship`] for the Citadel Warlord encounter
/// (task #83). High-hull, multi-weapon, `ReactorBreach` trait so the
/// kill splashes neighbors — the kind of pressure that earns the
/// run-end overlay. The bin's spawn callback dispatches to this when
/// it sees `spawn.class_id == "warlord"`; everything else falls
/// through to [`fallback_ship_for_spawn`].
///
/// Tuning rationale:
/// - Hull 14 (vs the cap of 7 for regular enemies in the canonical
///   `enemies[]`) — communicates "this fight is different." If
///   `spawn.hp_override` is set, it wins (lets the encounter tier-scale).
/// - Three mounts (Forward pulse_laser, Forward missile_salvo,
///   broadside beam_cannon) so the AI's telegraph queue surfaces
///   serious moves a turn ahead. The player sees the threat and has
///   to respond.
/// - `Trait::ReactorBreach` — the resolver consumes this in
///   `destroy()` (see `resolve.rs::1004`): splash damage to neighbors
///   on death. Mechanically the boss isn't just a sponge; killing it
///   matters at point-blank range.
/// - Strong bow facing the player (`BowOn { bow: Aft }`) so the
///   approach has to break through the bow's armour or maneuver
///   around it.
pub fn boss_ship_for_spawn(spawn: &ShipSpawn) -> Ship {
    let mut s = Ship {
        id: format!("{}@{}", spawn.class_id, spawn.cell),
        faction: Faction::Enemy,
        cell: spawn.cell,
        // v2 (A3 EXPAND): carry the spawn's 2-D pos/facing through (both default
        // until content's spawn-gen C4 sets real grid coordinates).
        pos: spawn.pos,
        orientation: spawn.orientation,
        facing: spawn.facing,
        hull: 14,
        max_hull: 14,
        heat: 0,
        heat_max: 8, // generous heat budget so the boss can sustain fire
        locked_out: false,
        shield_profile: ShieldProfile {
            // Stronger bow armour than regular enemies — the design's
            // "front of the boss is hard to crack" feel. Stern is still
            // the soft underbelly; the player is rewarded for flanking.
            bow: ShieldFace { armour: 3, charge: 1 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 0 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        },
        mounts: vec![
            // Forward pulse_laser — the AI fires this when range allows.
            Mount { id: "m1".into(), arc: TArc::Forward, weapon: "pulse_laser".into() },
            // Forward beam_cannon — high-damage telegraphed move.
            Mount { id: "m2".into(), arc: TArc::Forward, weapon: "beam_cannon".into() },
            // Broadside missile_salvo — punishes the player for
            // sitting in the flank arc trying to dodge the bow.
            Mount { id: "m3".into(), arc: TArc::BroadsideArc, weapon: "missile_salvo".into() },
        ],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: vec![Trait::ReactorBreach],
        klass: Some(spawn.class_id.clone()),
    };
    if let Some(hp) = spawn.hp_override {
        s.hull = hp;
        s.max_hull = hp;
    }
    s
}

/// Is `class_id` a known capital (a sector-end boss) in `catalog.capitals`?
/// Capitals are matched by their canonical display name — the form the #60
/// generator's [`capital_spawn`] writes into `ShipSpawn.class_id`. Used by the
/// bin's spawn closure to route capital spawns to the armed-boss synthesizer
/// ([`capital_boss_ship_for_spawn`]) instead of the hull-3 fallback.
pub fn is_capital_spawn(class_id: &str, catalog: &Catalog) -> bool {
    catalog.capitals.iter().any(|c| c.name == class_id)
}

/// Synthesize an ARMED boss [`Ship`] for a sector-end capital spawn (#69).
///
/// Before this, every named capital except the Citadel `warlord` degraded to
/// [`fallback_ship_for_spawn`] (hull 3, one pulse_laser) because
/// [`capital_spawn`] writes the capital's DISPLAY name into `class_id` — which
/// isn't in `enemies[]`, so catalog synthesis missed, and only `class_id ==
/// "warlord"` routed to a boss. Result: trivial sector bosses. This routes any
/// capital to the same hand-tuned boss baseline the warlord uses — a real
/// fight, not a popgun.
///
/// **Flat armed-boss baseline — deliberately NOT scaled off the CapitalDef's
/// salvage fields.** `salvage_p1`/`salvage_p7` are the meta-currency REWARD for
/// the kill (architect's #63 ruling, 4622de8), not combat stats; coupling hull
/// to them would bake in a wrong reward↔toughness correlation. So every capital
/// gets the warlord's hull-14 / ReactorBreach / three-mount shell here, just
/// re-labelled with its own name. Per-capital DISTINCT mechanics (Twins = two
/// ships, Coward flees, Stagemaster flips you — see
/// `docs/design/capital_distinctiveness.md`) stay DEFERRED per bruce; this is
/// the "popgun → real boss" bug fix only.
///
/// `catalog` is taken for the capital lookup (so the synthesized ship can carry
/// the matched [`crate::types::CapitalDef`] identity) and to keep the signature
/// future-proof for when per-capital stats land; the combat shell is flat
/// today. Falls back to the generic boss shell if the name isn't a known
/// capital (defensive — a typo'd capital still spawns a real boss, not a
/// popgun).
pub fn capital_boss_ship_for_spawn(spawn: &ShipSpawn, _catalog: &Catalog) -> Ship {
    // Flat baseline: reuse the warlord's armed-boss shape, keyed on the
    // capital's own name (already in spawn.class_id) so the renderer/HUD label
    // and the salvage lookup (which matches CapitalDef by name) both work.
    boss_ship_for_spawn(spawn)
}

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
        // v2 (A3 EXPAND): carry the spawn's 2-D pos/facing through (both default
        // until content's spawn-gen C4 sets real grid coordinates).
        pos: spawn.pos,
        orientation: spawn.orientation,
        facing: spawn.facing,
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
        // v2 (A3 EXPAND): 2-D pos/facing default until content's spawn-gen (C4)
        // re-keys the placeholder/generated sectors onto the 5×4 grid.
        pos: crate::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow },
        facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
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
            // Boss encounter — the run-end gate. The Citadel Warlord
            // (`class_id: "warlord"`) dispatches via the bin's spawn
            // callback to `boss_ship_for_spawn` — hull 14, three mounts
            // covering forward + broadside, `ReactorBreach` trait so the
            // kill splashes neighbors. Two `voidrunner` escorts at
            // either flank — the warlord's bow is hard to break, so the
            // intended play is to clear the escorts first (they have
            // `Agile` per the canonical EnemyDef) then maneuver to the
            // warlord's stern. `is_boss: true` is the flag
            // `AdvanceResult::Victorious` reads when the encounter is
            // won.
            EncounterDef {
                id: "citadel_boss".into(),
                enemy_ships: vec![
                    // Forward escort — closer to the player, harasses on
                    // the approach.
                    ShipSpawn {
                        class_id: "voidrunner".into(),
                        cell: 3,
                        pos: crate::grid::Pos::new(0, 0),
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                        hp_override: None,
                    },
                    // The warlord itself — mid-board, bow facing the
                    // player. boss_ship_for_spawn supplies the rich
                    // loadout; hp_override stays None so the function's
                    // 14-hull default applies.
                    ShipSpawn {
                        class_id: "warlord".into(),
                        cell: 5,
                        pos: crate::grid::Pos::new(0, 0),
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                        hp_override: None,
                    },
                    // Aft escort — covers the warlord's stern; the
                    // player has to break through to flank.
                    ShipSpawn {
                        class_id: "voidrunner".into(),
                        cell: 6,
                        pos: crate::grid::Pos::new(0, 0),
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                        hp_override: None,
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
 * #60 — spawn-pool encounter generator (data-driven campaign).
 *
 * Replaces the hand-authored [`placeholder_sectors`] with runtime
 * generation from the canonical [`SectorDef`] catalog data, per the design
 * doc's dynamic-spawn-pool model (broadside-analysis.html §XI, 788-796):
 *
 *   - Each sector's `intro[]` lists the enemy ship TYPES first introduced
 *     there; they ENTER a global run pool on arrival and persist for the
 *     rest of the run ("seen once → can appear in any later sector").
 *   - Encounters are NOT authored per-sector — they're SAMPLED from the
 *     accumulated pool, scaled by the sector `lane` (board size) and the
 *     run's patrol tier.
 *   - Each sector ENDS in its `capital` boss engagement (§VIII 699: "each
 *     sector ends in a capital-ship engagement — no waves, just the boss").
 *
 * Determinism (#111): generation is a pure function of
 * (route, sector node, patrol_tier) via a wang-hash PRNG — no global RNG,
 * so a given run-state always produces the same sector. The generator owns
 * no I/O; the bin feeds it the loaded [`Catalog`].
 *
 * BALANCE KNOBS (flagged for bruce — sensible doc-aligned defaults here,
 * tune later): [`ENCOUNTERS_PER_SECTOR`], enemies-per-encounter
 * ([`encounter_enemy_count`]), and uniform pool sampling. None of these are
 * pinned by the doc; they're the playtest dials.
 * ====================================================================== */

/// Default non-boss encounters generated per sector before the capital
/// fight. Doc-silent balance knob — start at 2 (a short sector reads well;
/// the boss is the third beat). Flagged for bruce.
pub const ENCOUNTERS_PER_SECTOR: u32 = 2;

/// Deterministic hash PRNG (mirrors the `wang_hash` used render-side in
/// hud.rs; kept local so runs.rs doesn't reach across the render boundary).
/// Pure: same seed → same value, so generation is reproducible (#111).
fn wang_hash(mut x: u32) -> u32 {
    x = (x ^ 61).wrapping_mul(0x27D4_EB2D);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 16;
    x
}

/// The run's accumulated spawn pool: enemy `class_id`s unlocked by every
/// sector visited so far. Derived from the route (no mutable run-state
/// field needed) — the pool at sector N is the union of `intro[]` over
/// sectors 0..=N, mapped from catalog display names to enemy ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnPool {
    /// Enemy class ids available to sample, in first-seen order (stable so
    /// generation is deterministic).
    pub class_ids: Vec<String>,
}

impl SpawnPool {
    /// Build the pool visible AT sector index `up_to_idx` (inclusive):
    /// union of `intro[]` from sectors[0..=up_to_idx], display-name →
    /// enemy-id via the catalog's `enemies[]`. Unknown intro names are
    /// skipped (logged) rather than poisoning the pool with a dangling id.
    pub fn accumulate(sectors: &[SectorDef], up_to_idx: usize, catalog: &Catalog) -> Self {
        // Display-name (lowercased) → enemy id map from the catalog.
        let name_to_id: HashMap<String, String> = catalog
            .enemies
            .iter()
            .map(|e| (e.name.to_lowercase(), e.id.clone()))
            .collect();

        let mut class_ids: Vec<String> = Vec::new();
        let end = up_to_idx.min(sectors.len().saturating_sub(1));
        for sector in &sectors[..=end] {
            for intro_name in &sector.intro {
                let id = resolve_enemy_id(intro_name, &name_to_id);
                match id {
                    Some(id) if !class_ids.contains(&id) => class_ids.push(id),
                    Some(_) => {} // already pooled
                    None => eprintln!(
                        "[runs] sector `{}` intro `{intro_name}` has no matching enemy id; skipped",
                        sector.name,
                    ),
                }
            }
        }
        SpawnPool { class_ids }
    }

    pub fn is_empty(&self) -> bool {
        self.class_ids.is_empty()
    }
}

/// Resolve an intro/capital display name to a catalog enemy id. Already-id
/// (snake_case) forms pass through; otherwise looked up by lowercased
/// display name.
fn resolve_enemy_id(name: &str, name_to_id: &HashMap<String, String>) -> Option<String> {
    if name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Some(name.to_string());
    }
    name_to_id.get(&name.to_lowercase()).cloned()
}

/// Enemies in a generated non-boss encounter, scaled by lane size. Doc-silent
/// balance knob — wider lanes hold more ships. 5→2, 7→3, 9→4. Flagged for
/// bruce.
fn encounter_enemy_count(lane: u8) -> usize {
    match lane {
        0..=5 => 2,
        6..=7 => 3,
        _ => 4,
    }
}

/// Generate the runtime [`Sector`] (encounters + boss) for `sector_def`,
/// given the accumulated `pool`, the run's `patrol_tier`, and the
/// `catalog` (for capital lookup). Deterministic in
/// `(sector_def.node, patrol_tier)`.
///
/// Produces [`ENCOUNTERS_PER_SECTOR`] pool-sampled encounters followed by
/// the capital boss encounter (if the sector has a `capital`). If the pool
/// is empty (e.g. the run-start Staging sector introduces nothing and no
/// prior sector seeded the pool), only the boss encounter — if any — is
/// emitted; a sector with neither is a passthrough (empty `encounters`,
/// which `encounter_outcome` treats as already-won).
pub fn generate_sector(
    sector_def: &SectorDef,
    pool: &SpawnPool,
    patrol_tier: u8,
    catalog: &Catalog,
) -> Sector {
    // Seed from the node string + patrol tier so each sector's layout is
    // stable per run-state but varies across sectors / difficulties.
    let node_seed = sector_def
        .node
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let base_seed = node_seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(patrol_tier as u32);

    let mut encounters: Vec<EncounterDef> = Vec::new();

    if !pool.is_empty() {
        let count = encounter_enemy_count(sector_def.lane);
        for e in 0..ENCOUNTERS_PER_SECTOR {
            let enemy_ships = sample_encounter_spawns(
                pool,
                sector_def.lane,
                count,
                base_seed.wrapping_add(e.wrapping_mul(0x1000_0001)),
            );
            if enemy_ships.is_empty() {
                continue;
            }
            encounters.push(EncounterDef {
                id: format!("{}_e{e}", sector_def.node),
                enemy_ships,
                hazards: Vec::new(),
                is_boss: false,
            });
        }
    }

    // Capital boss encounter at sector end (if this sector has a capital).
    if let Some(boss) = sector_def
        .capital
        .as_ref()
        .and_then(|cap| capital_spawn(cap, sector_def.lane, catalog))
    {
        encounters.push(EncounterDef {
            id: format!("{}_boss", sector_def.node),
            enemy_ships: vec![boss],
            hazards: Vec::new(),
            is_boss: true,
        });
    }

    Sector {
        id: sector_def.node.clone(),
        name: sector_def.name.clone(),
        patrol_tier,
        encounters,
    }
}

/// Sample `count` enemy spawns for one encounter from the pool, PINCERING the
/// mid-lane player (#72): enemies are distributed on BOTH sides of the player's
/// middle cell and each bows toward the player, so the player is threatened
/// fore AND aft and must rotate to face whichever side is closing. Deterministic
/// in `seed`; never spawns on the player's cell.
///
/// Facing: an enemy AFT of the player (lower cell) bows `Fore` (toward higher
/// cells → toward the player); an enemy FORE of the player (higher cell) bows
/// `Aft` (toward the player). Combined with resolver's #68 close-move (which
/// closes toward the player from either side), this makes the approach
/// directional from both ends.
///
/// Distribution: walk outward from the mid cell, alternating aft / fore
/// (mid-1, mid+1, mid-2, mid+2, …) so the first two enemies straddle the
/// player and additional enemies fan out symmetrically.
fn sample_encounter_spawns(
    pool: &SpawnPool,
    lane: u8,
    count: usize,
    seed: u32,
) -> Vec<ShipSpawn> {
    let lane = lane as usize;
    if lane < 2 || pool.is_empty() {
        return Vec::new();
    }
    let mid = player_start_cell(lane);
    // Build the pincer cell order: alternate aft (mid-k) / fore (mid+k),
    // k = 1, 2, 3, …, keeping cells in [0, lane) and skipping the player cell.
    let usable = lane.saturating_sub(1); // every cell except the player's
    let n = count.min(usable);
    let mut cells: Vec<usize> = Vec::with_capacity(n);
    let mut k = 1usize;
    while cells.len() < n && k <= lane {
        // Aft side first (mid - k), then fore side (mid + k).
        if mid >= k {
            cells.push(mid - k);
            if cells.len() == n {
                break;
            }
        }
        if mid + k < lane {
            cells.push(mid + k);
        }
        k += 1;
    }

    let mut spawns = Vec::with_capacity(cells.len());
    for (i, &cell) in cells.iter().enumerate() {
        let pick = wang_hash(seed.wrapping_add(i as u32)) as usize % pool.class_ids.len();
        let class_id = pool.class_ids[pick].clone();
        // Bow toward the player: aft-of-player faces Fore, fore-of-player faces Aft.
        let bow = if cell < mid { LaneEnd::Fore } else { LaneEnd::Aft };
        spawns.push(ShipSpawn {
            class_id,
            cell,
            // v2 (A3 EXPAND): default until content's 2-D spawn-gen (C4).
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hp_override: None,
        });
    }
    spawns
}

/// Build the capital boss spawn for `capital_name` at the sector's lane.
/// Looks the capital up in the catalog's (loosely-typed) `capitals[]` for
/// existence; the boss ship itself is materialized by the bin's
/// `boss_ship_for_spawn` via the `warlord` dispatch today (the capital
/// roster's per-boss stats aren't a typed catalog section yet — that's a
/// future `CapitalDef` from architect). For now the capital spawns as a
/// boss-class ship at mid-lane, carrying the capital's display name as its
/// class_id so the bin/renderer can label it.
///
/// Returns `None` if the capital name isn't in the catalog (defensive — a
/// typo'd capital just yields a boss-less sector rather than crashing).
fn capital_spawn(capital_name: &str, lane: u8, catalog: &Catalog) -> Option<ShipSpawn> {
    // Confirm the capital exists in the catalog's typed capitals[]
    // (architect's CapitalDef, #63 — was a loose Value before).
    let known = catalog
        .capitals
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(capital_name));
    if !known {
        return None;
    }
    let mid = (lane as usize / 2).max(1);
    Some(ShipSpawn {
        // class_id carries the capital's canonical name; the bin's
        // spawn callback maps capitals to boss_ship_for_spawn. Until a
        // typed CapitalDef lands, all capitals share the boss synthesizer
        // (hull 14, ReactorBreach) — distinct per-capital stats are a
        // future content+architect follow-up.
        class_id: capital_name.to_string(),
        cell: mid,
        // v2 (A3 EXPAND): default until content's 2-D spawn-gen (C4).
        pos: crate::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
        facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
        hp_override: None,
    })
}

/// Generate the full campaign: a runtime [`Sector`] per catalog
/// [`SectorDef`], with the spawn pool accumulated along the route. This is
/// the data-driven replacement for [`placeholder_sectors`] — the bin uses
/// it when a catalog is loaded, falling back to the placeholders otherwise.
///
/// Note the pool is accumulated PER sector index (sector N sees intro from
/// 0..=N), so earlier sectors field smaller pools — matching the doc's
/// "ships unlock as you progress" model. `patrol_tier` is the run's global
/// difficulty tier (1-7).
pub fn generate_campaign(catalog: &Catalog, patrol_tier: u8) -> Vec<Sector> {
    let sectors = &catalog.sectors;
    sectors
        .iter()
        .enumerate()
        .map(|(idx, sd)| {
            let pool = SpawnPool::accumulate(sectors, idx, catalog);
            generate_sector(sd, &pool, patrol_tier, catalog)
        })
        .collect()
}

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
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
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
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
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
            fire_events: vec![],
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
    fn progression_passes_through_an_empty_leading_sector() {
        // A Staging-style passthrough (no encounters) followed by a combat
        // sector. The run opens on the empty sector; `current_encounter`
        // must serve the combat sector's first encounter, and
        // `advance_after_win` must normalize past the empty sector instead
        // of stalling. The generated campaign (#60) opens on an empty
        // Staging sector and was unwinnable until this.
        let staging = Sector {
            id: "staging".into(),
            name: "Staging".into(),
            patrol_tier: 1,
            encounters: Vec::new(),
        };
        let combat = Sector {
            id: "drift".into(),
            name: "Drift".into(),
            patrol_tier: 1,
            encounters: vec![enc("c_e0", Vec::new(), false), enc("c_boss", Vec::new(), true)],
        };
        let sectors = vec![staging, combat];
        let mut run = new_run();

        // Opens on the empty sector but serves the combat sector's first enc.
        assert_eq!(
            current_encounter(&run, &sectors).map(|e| e.id.as_str()),
            Some("c_e0"),
            "current_encounter skips the empty Staging passthrough",
        );

        // Winning it normalizes the run into the combat sector.
        advance_after_win(&mut run, &sectors);
        assert_eq!(run.current_sector_idx, 1, "advance lands in the combat sector");
        assert_eq!(
            current_encounter(&run, &sectors).map(|e| e.id.as_str()),
            Some("c_boss"),
            "next encounter is the combat sector's boss",
        );

        // Clearing the final-sector boss flips victorious — the empty
        // leading sector never stalled the run.
        advance_after_win(&mut run, &sectors);
        assert!(run.victorious, "campaign completes; the empty sector did not stall it");
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
    fn build_board_places_player_mid_lane() {
        // #72: the player starts at the MIDDLE cell, not the edge. Enemy at
        // cell 4 keeps the lane at size 5 → player mid cell = 2.
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: 4,
                pos: crate::grid::Pos::new(0, 0),
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
        };
        let player = make_player(3, 8); // pre-board cell shouldn't matter
        let board = build_encounter_board(&enc, player, |spawn| Some(fallback_ship_for_spawn(spawn)));
        // Player ends up at the middle cell (size 5 → cell 2) regardless of
        // their pre-board cell.
        let mid = player_start_cell(board.size);
        assert_eq!(mid, 2, "size-5 lane → mid cell 2");
        let at_mid = board.cells[mid].as_ref().unwrap();
        assert_eq!(at_mid.faction, Faction::Player);
        assert_eq!(at_mid.cell, mid);
        // Hull state carries over (proof that the prior-encounter ship
        // is preserved, not reset).
        assert_eq!(at_mid.hull, 8);
        // Cell 0 is now empty (player no longer edge-parked).
        assert!(board.cells[0].is_none(), "player vacated the lane edge");
    }

    #[test]
    fn build_board_spawns_enemies_at_their_cells() {
        // Cells 1 and 4 (size-5 lane, player mid = cell 2 stays clear).
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: 1,
                    pos: crate::grid::Pos::new(0, 0),
                    orientation: Orientation::BowOn { bow: LaneEnd::Fore },
                    facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "lancer".into(),
                    cell: 4,
                    pos: crate::grid::Pos::new(0, 0),
                    orientation: Orientation::Broadside,
                    facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                    hp_override: Some(7),
                },
            ],
            hazards: vec![],
            is_boss: false,
        };
        let board = build_encounter_board(&enc, make_player(0, 10),
            |spawn| Some(fallback_ship_for_spawn(spawn)));
        let e1 = board.cells[1].as_ref().unwrap();
        assert_eq!(e1.faction, Faction::Enemy);
        let e4 = board.cells[4].as_ref().unwrap();
        assert_eq!(e4.faction, Faction::Enemy);
        assert_eq!(e4.orientation, Orientation::Broadside);
        assert_eq!(e4.hull, 7, "hp_override applied");
    }

    #[test]
    fn build_board_skips_player_cell_spawn_to_avoid_collision() {
        // #72: the player now sits mid-lane, so a spawn ON the mid cell (not
        // cell 0) is the collision case. cells 0 and 4 keep the lane size 5
        // → mid cell 2; the cell-2 spawn must be dropped.
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: 2, // tries to spawn ON the mid-lane player
                    pos: crate::grid::Pos::new(0, 0),
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: 4, // keeps lane size at 5
                    pos: crate::grid::Pos::new(0, 0),
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
                    hp_override: None,
                },
            ],
            hazards: vec![],
            is_boss: false,
        };
        let board = build_encounter_board(&enc, make_player(0, 10),
            |spawn| Some(fallback_ship_for_spawn(spawn)));
        let mid = player_start_cell(board.size);
        let at_mid = board.cells[mid].as_ref().unwrap();
        // Player kept the mid cell; the colliding enemy spawn was dropped.
        assert_eq!(at_mid.faction, Faction::Player);
        // A cell-0 spawn would now be VALID (player vacated the edge) — confirm
        // the non-colliding enemy at cell 4 still placed.
        assert_eq!(board.cells[4].as_ref().unwrap().faction, Faction::Enemy);
    }

    #[test]
    fn boss_ship_for_spawn_has_climactic_loadout() {
        // Task #83: the Citadel Warlord needs to FEEL like a boss when
        // it spawns. This test pins the climactic invariants — hull
        // jump (far above the 1..=7 cap of regular enemies), the
        // ReactorBreach trait (kill-splash on death), and a
        // multi-mount loadout so the AI's telegraph queue surfaces
        // serious threats a turn ahead.
        let spawn = ShipSpawn {
            class_id: "warlord".into(),
            cell: 5,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hp_override: None,
        };
        let s = boss_ship_for_spawn(&spawn);
        assert_eq!(s.faction, Faction::Enemy);
        assert_eq!(s.cell, 5);
        assert!(
            s.hull >= 14,
            "boss hull should be the climactic-tier default (14), got {}",
            s.hull,
        );
        assert_eq!(s.max_hull, s.hull);
        assert!(
            s.traits.contains(&Trait::ReactorBreach),
            "boss must carry ReactorBreach so the kill splashes neighbors",
        );
        assert!(
            s.mounts.len() >= 3,
            "boss needs >= 3 mounts so the AI telegraph surfaces real threats; got {}",
            s.mounts.len(),
        );
        // The bow shield is the player's frontal pressure — should be
        // tougher than the canonical default (armour 2).
        assert!(
            s.shield_profile.bow.armour >= 3,
            "boss bow armour should be 3+ to make the frontal approach a real fight",
        );
    }

    #[test]
    fn boss_ship_for_spawn_honors_hp_override() {
        // The encounter author can still tier-scale by passing
        // `hp_override`. Confirms the boss synthesizer doesn't
        // hardcode hull past the override path.
        let spawn = ShipSpawn {
            class_id: "warlord".into(),
            cell: 5,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hp_override: Some(20),
        };
        let s = boss_ship_for_spawn(&spawn);
        assert_eq!(s.hull, 20);
        assert_eq!(s.max_hull, 20);
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

    /* ---- #60 spawn-pool encounter generator ----------------------- */

    /// Minimal catalog with 3 sectors (Staging→Drift Belt→Ion Reefs), the
    /// enemies they introduce, and the two capitals — enough to exercise
    /// pool accumulation + generation. Uses the canonical transformer so
    /// the SectorDef capital deserializer + enemies shape are real.
    fn gen_catalog() -> crate::types::Catalog {
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
                { "id": "lancer", "name": "Lancer", "hull": 1, "hull5": 2, "traits": ["Burn-Hard"],
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
        crate::catalog_canonical::from_canonical_value(json).expect("gen catalog parses")
    }

    #[test]
    fn spawn_pool_accumulates_intro_along_the_route() {
        let cat = gen_catalog();
        // At Staging (idx 0): nothing introduced yet.
        let p0 = SpawnPool::accumulate(&cat.sectors, 0, &cat);
        assert!(p0.is_empty(), "Staging introduces nothing");
        // At Drift Belt (idx 1): Skiff + Lancer, display-name → id.
        let p1 = SpawnPool::accumulate(&cat.sectors, 1, &cat);
        assert_eq!(p1.class_ids, vec!["skiff".to_string(), "lancer".to_string()]);
        // At Ion Reefs (idx 2): pool carries forward + adds gunboat.
        let p2 = SpawnPool::accumulate(&cat.sectors, 2, &cat);
        assert_eq!(
            p2.class_ids,
            vec!["skiff".to_string(), "lancer".to_string(), "gunboat".to_string()],
            "pool accumulates across the route",
        );
    }

    #[test]
    fn generate_sector_produces_encounters_then_boss() {
        let cat = gen_catalog();
        let pool = SpawnPool::accumulate(&cat.sectors, 1, &cat); // skiff+lancer
        let sector = generate_sector(&cat.sectors[1], &pool, 1, &cat);
        // ENCOUNTERS_PER_SECTOR non-boss + 1 boss.
        assert_eq!(
            sector.encounters.len(),
            ENCOUNTERS_PER_SECTOR as usize + 1,
            "N pool encounters + the capital boss",
        );
        // Last encounter is the boss; earlier ones are not.
        let last = sector.encounters.last().unwrap();
        assert!(last.is_boss, "sector ends in the capital boss");
        assert_eq!(last.enemy_ships.len(), 1, "boss is a single capital ship");
        assert_eq!(last.enemy_ships[0].class_id, "The Dasher");
        // #72: player starts mid-lane (lane 5 → cell 2); enemies pincer
        // around it and must never spawn ON the player cell.
        let mid = player_start_cell(cat.sectors[1].lane as usize);
        for e in &sector.encounters[..sector.encounters.len() - 1] {
            assert!(!e.is_boss);
            // Non-boss encounters draw from the pool (skiff/lancer) at
            // distinct cells straddling the mid-lane player.
            assert!(!e.enemy_ships.is_empty());
            let mut saw_aft = false;
            let mut saw_fore = false;
            for sp in &e.enemy_ships {
                assert_ne!(sp.cell, mid, "enemies never spawn on the player's mid cell");
                if sp.cell < mid { saw_aft = true; }
                if sp.cell > mid { saw_fore = true; }
                assert!(
                    pool.class_ids.contains(&sp.class_id),
                    "spawn {} drawn from the pool", sp.class_id,
                );
            }
            // Lane 5 → 2 enemies per encounter (encounter_enemy_count), and
            // the pincer puts one on each side of the player.
            assert_eq!(e.enemy_ships.len(), 2);
            assert!(saw_aft && saw_fore, "the two enemies pincer the mid player (one each side)");
        }
    }

    /* ---- #69: capitals synthesize as ARMED bosses, not popguns -------- */

    #[test]
    fn is_capital_spawn_matches_catalog_capitals_by_name() {
        let cat = gen_catalog();
        // The generator writes the capital's DISPLAY name into class_id.
        assert!(is_capital_spawn("The Dasher", &cat), "known capital");
        assert!(!is_capital_spawn("skiff", &cat), "regular enemy is not a capital");
        assert!(!is_capital_spawn("warlord", &cat), "warlord id is not a catalog capital name");
        assert!(!is_capital_spawn("Nonexistent", &cat), "unknown name");
    }

    #[test]
    fn capital_boss_is_armed_not_a_popgun() {
        // The #69 bug: capitals degraded to fallback_ship_for_spawn (hull 3,
        // ONE pulse_laser). The fix routes them to an armed boss baseline.
        let cat = gen_catalog();
        let sp = ShipSpawn {
            class_id: "The Dasher".into(),
            cell: 3,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hp_override: None,
        };
        let boss = capital_boss_ship_for_spawn(&sp, &cat);
        let popgun = fallback_ship_for_spawn(&sp);
        // Materially tougher + more mounts than the fallback popgun.
        assert!(boss.hull > popgun.hull, "capital boss must out-hull the fallback ({} vs {})", boss.hull, popgun.hull);
        assert!(boss.mounts.len() > popgun.mounts.len(),
            "capital boss must out-gun the fallback ({} vs {} mounts)", boss.mounts.len(), popgun.mounts.len());
        // Carries the boss's signature ReactorBreach pressure trait.
        assert!(boss.traits.contains(&Trait::ReactorBreach), "capital boss carries ReactorBreach");
        // Identity preserved so the HUD label + salvage-by-name lookup still work.
        assert_eq!(boss.klass.as_deref(), Some("The Dasher"));
        // hp_override still wins (lets an encounter tier-scale a capital).
        let mut sp2 = sp.clone();
        sp2.hp_override = Some(25);
        assert_eq!(capital_boss_ship_for_spawn(&sp2, &cat).hull, 25);
    }

    #[test]
    fn generate_sector_is_deterministic() {
        let cat = gen_catalog();
        let pool = SpawnPool::accumulate(&cat.sectors, 2, &cat);
        let a = generate_sector(&cat.sectors[2], &pool, 3, &cat);
        let b = generate_sector(&cat.sectors[2], &pool, 3, &cat);
        assert_eq!(a, b, "same (node, tier, pool) → identical sector (#111 determinism)");
        // Different patrol tier → (potentially) different layout seed; at
        // minimum it must still be self-consistent and boss-terminated.
        let c = generate_sector(&cat.sectors[2], &pool, 5, &cat);
        assert!(c.encounters.last().unwrap().is_boss);
    }

    #[test]
    fn staging_sector_has_no_encounters_and_no_boss() {
        // Staging: empty intro (pool empty at idx 0) + capital "—" → None.
        let cat = gen_catalog();
        let pool = SpawnPool::accumulate(&cat.sectors, 0, &cat);
        let sector = generate_sector(&cat.sectors[0], &pool, 1, &cat);
        assert!(
            sector.encounters.is_empty(),
            "Staging is a passthrough: no pool enemies, no capital",
        );
    }

    #[test]
    fn generate_campaign_covers_every_catalog_sector() {
        let cat = gen_catalog();
        let campaign = generate_campaign(&cat, 1);
        assert_eq!(campaign.len(), cat.sectors.len(), "one runtime Sector per SectorDef");
        // Lane sizes carry through (Ion Reefs is lane 7 → 3 enemies/encounter).
        let ion = &campaign[2];
        let non_boss = ion.encounters.iter().find(|e| !e.is_boss).unwrap();
        assert_eq!(non_boss.enemy_ships.len(), 3, "lane 7 → 3 enemies per encounter");
    }

    #[test]
    fn unknown_capital_yields_bossless_sector_not_a_crash() {
        let cat = gen_catalog();
        let pool = SpawnPool::accumulate(&cat.sectors, 1, &cat);
        // A SectorDef naming a capital absent from catalog.capitals[].
        let bogus = SectorDef {
            name: "Nowhere".into(),
            node: "9".into(),
            lane: 5,
            intro: vec![],
            capital: Some("The Phantom Menace".into()),
        };
        let sector = generate_sector(&bogus, &pool, 1, &cat);
        // No pool enemies queued here (intro empty for THIS sector but pool
        // is non-empty from the route) → encounters generate; but the
        // unknown capital adds NO boss.
        assert!(
            sector.encounters.iter().all(|e| !e.is_boss),
            "unknown capital → no boss encounter, no panic",
        );
    }
}
