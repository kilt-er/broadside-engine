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
//! queries them every frame (`damage_modifier` on every shot, `card_at` on
//! every key press). Sectors are consulted ONCE per encounter
//! transition, so there's no perf reason to bake them into Content.
//! Keeping them in a standalone `placeholder_sectors()` function makes
//! the eventual switch to `Catalog::sectors` mechanical: the bin reads
//! either source at startup, the rest of the code only sees
//! `&[Sector]`.
//!
//! ## Why `ShipSpawn::class_id`, not a direct Ship?
//!
//! The architect's foundation has spawns reference a [`crate::types::ClassDef::id`]
//! rather than embedding a full `Ship`. That's the canonical pattern —
//! one `ClassDef` defines the loadout, the encounter just says "spawn
//! three of THIS class at THESE cells." [`spawn_to_ship`] materializes a
//! Ship from a spawn + the catalog's `ClassDef` lookup; if a future
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
pub const fn mark_defeated(run: &mut Run) {
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

/// Build a fresh [`Board`] for the encounter on the fixed 5×4 grid (v2 C4).
/// The player is placed at the front-centre cell ([`player_start_pos`], bow N
/// toward the enemies); each enemy spawn is placed at its 2-D [`ShipSpawn::pos`]
/// on the back rows.
///
/// `player` is the player's CURRENT ship (with whatever heat / hull / statuses
/// carried over from the prior encounter). The board's cell vector is rebuilt;
/// the player's `pos`/`facing` are normalized to the front-centre start
/// regardless of where they ended the previous encounter ("you start a new
/// encounter at the front").
///
/// `class_to_ship` is a builder closure that turns a [`ShipSpawn`] into a
/// [`Ship`] given the class id lookup. The bin passes a catalog-aware builder;
/// keeping it a parameter lets the same encounter builder work with placeholder
/// data and real catalog data.
///
/// **INVARIANT (A) — slot == `pos.to_index()`.** Every ship is stored at
/// `cells[ship.pos.to_index()]` with `ship.pos` set to its real grid [`Pos`], so
/// [`Board::ship_at`] called with `pos` returns `cells[pos.to_index()]`. The resolver's
/// R3 ray-walk and R4 `apply_damage` depend on this; a ship whose slot and
/// `pos` disagree is invisible to `ship_at`. A spawn whose `pos` collides with
/// an already-placed ship (or the player) is skipped — placeholder/generated
/// sectors are collision-free by construction, but a hand-authored sector with
/// a duplicate cell won't corrupt the board.
///
/// Hazards on the encounter populate `board.hazards` at their 2-D [`Hazard::pos`].
pub fn build_encounter_board<F>(
    encounter: &EncounterDef,
    mut player: Ship,
    mut class_to_ship: F,
) -> Board
where
    F: FnMut(&ShipSpawn) -> Option<Ship>,
{
    // v2 (A3 Board EXPAND): fixed len-CELLS (20) backing Vecs so the 2-D
    // occupancy view (Board::ship_at(pos) = cells[pos.to_index()]) is valid over
    // the whole 5×4 grid.
    let mut cells: Vec<Option<Ship>> = (0..crate::grid::CELLS).map(|_| None).collect();
    let mut hazards: Vec<Vec<crate::types::Hazard>> =
        (0..crate::grid::CELLS).map(|_| Vec::new()).collect();

    // Player at the front-centre cell, bow pointed N (into the board, toward the
    // enemies). Normalize both the 2-D pos/facing and the legacy 1-D
    // cell/orientation (transition window). Invariant (A): slot == pos.to_index().
    let player_pos = player_start_pos();
    player.pos = player_pos;
    player.facing = player_spawn_facing();
    player.cell = player_pos.to_index();
    player.orientation = Orientation::BowOn { bow: LaneEnd::Fore };
    cells[player_pos.to_index()] = Some(player);

    // Place each enemy spawn at its 2-D pos (invariant A).
    for spawn in &encounter.enemy_ships {
        if !spawn.pos.in_bounds() || spawn.pos == player_pos {
            // Off-grid or colliding with the player — skip (defensive).
            continue;
        }
        let idx = spawn.pos.to_index();
        if cells[idx].is_some() {
            continue; // a prior spawn already holds this cell
        }
        if let Some(mut ship) = class_to_ship(spawn) {
            // Force slot == pos.to_index(): trust the spawn's authoritative 2-D
            // pos/facing over whatever the builder defaulted, and keep the
            // legacy fields consistent for the transition.
            ship.pos = spawn.pos;
            ship.facing = spawn.facing;
            ship.cell = idx;
            ship.orientation = spawn.orientation;
            if let Some(hp) = spawn.hp_override {
                ship.hull = hp;
                ship.max_hull = hp;
            }
            cells[idx] = Some(ship);
        }
    }

    // Drop hazards into their 2-D cells.
    for h in &encounter.hazards {
        if h.pos.in_bounds() {
            hazards[h.pos.to_index()].push(h.clone());
        }
    }

    Board {
        // `size` is the legacy 1-D lane length, kept for the transition window;
        // 2-D placement uses the fixed CELLS grid. Report the grid width so any
        // remaining 1-D reader sees a sane lane spanning the columns.
        size: crate::grid::COLS,
        cells,
        ordnance: Vec::new(),
        hazards,
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

/// Canonical lane size for a given maximum spawn cell. The analysis doc
/// uses 5 / 7 / 9 for early / mid / late sectors. Picks the smallest
/// size that fits.
pub const fn canonical_lane_size(max_cell: usize) -> usize {
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
pub const fn player_start_cell(size: usize) -> usize {
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
/// - Three mounts (Forward `pulse_laser`, Forward `missile_salvo`,
///   broadside `beam_cannon`) so the AI's telegraph queue surfaces
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
            bow: ShieldFace {
                armour: 3,
                charge: 1,
            },
            stern: ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: ShieldFace {
                armour: 1,
                charge: 0,
            },
            starboard: ShieldFace {
                armour: 1,
                charge: 0,
            },
        },
        mounts: vec![
            // Forward pulse_laser — the AI fires this when range allows.
            Mount {
                id: "m1".into(),
                arc: TArc::Forward,
                weapon: "pulse_laser".into(),
            },
            // Forward beam_cannon — high-damage telegraphed move.
            Mount {
                id: "m2".into(),
                arc: TArc::Forward,
                weapon: "beam_cannon".into(),
            },
            // Broadside missile_salvo — punishes the player for
            // sitting in the flank arc trying to dodge the bow.
            Mount {
                id: "m3".into(),
                arc: TArc::BroadsideArc,
                weapon: "missile_salvo".into(),
            },
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
/// [`fallback_ship_for_spawn`] (hull 3, one `pulse_laser`) because
/// [`capital_spawn`] writes the capital's DISPLAY name into `class_id` — which
/// isn't in `enemies[]`, so catalog synthesis missed, and only `class_id ==
/// "warlord"` routed to a boss. Result: trivial sector bosses. This routes any
/// capital to the same hand-tuned boss baseline the warlord uses — a real
/// fight, not a popgun.
///
/// **Flat armed-boss baseline — deliberately NOT scaled off the `CapitalDef`'s
/// salvage fields.** `salvage_p1`/`salvage_p7` are the meta-currency REWARD for
/// the kill (architect's #63 ruling, 4622de8), not combat stats; coupling hull
/// to them would bake in a wrong reward↔toughness correlation. So every capital
/// gets the warlord's hull-14 / `ReactorBreach` / three-mount shell here, just
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
/// hull, one Forward `pulse_laser` mount so the AI has something to fire.
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
            bow: ShieldFace {
                armour: 1,
                charge: 0,
            },
            stern: ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: ShieldFace {
                armour: 1,
                charge: 0,
            },
            starboard: ShieldFace {
                armour: 1,
                charge: 0,
            },
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

/* =========================================================================
 * v2 2-D spawn geometry (C4).
 *
 * Blueprint decisions #2 (5×4 grid) + #8 (rows = dodge space). The 1-D pincer
 * (enemies straddling a mid-lane player on a line) is REPLACED by a fixed 2-D
 * layout, ratified by the lead 2026-06-14:
 *
 *   - PLAYER at the front-center cell, bow pointed N (INTO the board, toward the
 *     enemies on row 0). The player faces the threat, not the camera.
 *   - ENEMIES on the BACK rows (row 0 first, then row 1), fanned across the
 *     columns, all bow=S (toward the player). Lateral column spread + the
 *     two-row depth gradient is the dodge space.
 *
 * Spawn facings are set EXPLICITLY here (player N / enemies S) — NOT derived via
 * `facing_from_orientation`, which is the MIGRATE stopgap for *other* 1-D
 * constructs. These spawns are authoritative: they know the real 2-D layout.
 * The legacy `cell`/`orientation` fields are still populated for the transition
 * window (read by code not yet migrated); the 2-D `pos`/`facing` are the source
 * of truth for placement and are what `build_encounter_board` keys on.
 * ====================================================================== */

/// The player's fixed 2-D start cell: front-center (`col = COLS/2`, the front
/// row `ROWS-1`). Blueprint decision #8 — the player anchors at the front and
/// the rows ahead are pure dodge space.
pub const fn player_start_pos() -> crate::grid::Pos {
    crate::grid::Pos::new(crate::grid::COLS / 2, crate::grid::ROWS - 1)
}

/// The player's spawn stance: bow pointed N (toward row 0 / the enemies). The
/// player faces INTO the board, so its strong bow meets the incoming threat.
pub const fn player_spawn_facing() -> crate::grid::Facing {
    crate::grid::Facing::Bow(crate::grid::Dir4::N)
}

/// Every enemy's spawn stance: bow pointed S (toward the player). Combined with
/// the resolver's close-move (which steps toward the player), the approach is
/// bow-first from the back rows.
pub const fn enemy_spawn_facing() -> crate::grid::Facing {
    crate::grid::Facing::Bow(crate::grid::Dir4::S)
}

/// Column order for fanning enemies across one back row: centre-out
/// (`2, 1, 3, 0, 4` for `COLS == 5`) so small encounters cluster toward the
/// middle (in front of the front-centre player) and larger ones fan to the
/// edges. Deterministic and total over `0..COLS`.
fn back_row_column_order() -> Vec<usize> {
    let mid = crate::grid::COLS / 2;
    let mut cols = vec![mid];
    let mut k = 1usize;
    while cols.len() < crate::grid::COLS {
        // mid - k then mid + k, dropping any that fall off the row.
        if mid >= k {
            cols.push(mid - k);
        }
        if mid + k < crate::grid::COLS {
            cols.push(mid + k);
        }
        k += 1;
    }
    cols
}

/// The back-row [`crate::grid::Pos`] for the `i`-th enemy in an encounter: fill
/// row 0 across the centre-out column order, then row 1, then (defensively) row
/// 2. Returns `None` once the back rows are exhausted — encounters cap at 4
/// enemies ([`encounter_enemy_count`]) so the first two rows (10 slots) always
/// suffice; the `None` is a guard, not an expected path.
///
/// The two back rows give the depth gradient (decision #8): row-0 enemies are
/// Far/Near from the front-centre player, row-1 ones one band closer, so the
/// player reads a wall with depth and dodges laterally between threatened
/// columns.
fn enemy_spawn_pos(i: usize) -> Option<crate::grid::Pos> {
    let order = back_row_column_order();
    let per_row = order.len(); // == COLS
    let row = i / per_row;
    let col = order[i % per_row];
    // Enemies occupy the back rows only; never the front row (the player's).
    if row >= crate::grid::ROWS - 1 {
        return None;
    }
    Some(crate::grid::Pos::new(col, row))
}

/// Map a placeholder sector's 1-D lane `cell` onto a back-row 2-D [`Pos`]
/// (the placeholder sectors below author 1-D cells; this re-keys them onto the
/// grid). Columns wrap across `COLS`; each full wrap drops to the next back row,
/// so a spread of cells fans across row 0 then row 1. Bounded to the back rows
/// (row 0..ROWS-1) — a cell that would overflow past the back rows clamps to the
/// last back row's last column (defensive; the placeholder cells stay small).
fn placeholder_cell_to_pos(cell: usize) -> crate::grid::Pos {
    let col = cell % crate::grid::COLS;
    let row = (cell / crate::grid::COLS).min(crate::grid::ROWS.saturating_sub(2));
    crate::grid::Pos::new(col, row)
}

fn spawn(class_id: &str, cell: usize, bow: LaneEnd, hp_override: Option<i32>) -> ShipSpawn {
    ShipSpawn {
        class_id: class_id.into(),
        cell,
        // v2 (C4): re-key the placeholder sector's 1-D cell onto a back-row 2-D
        // Pos, and set the spawn stance EXPLICITLY to bow=S (toward the player).
        // The legacy cell/orientation stay set for the transition window.
        pos: placeholder_cell_to_pos(cell),
        orientation: Orientation::BowOn { bow },
        facing: enemy_spawn_facing(),
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
/// healthy `hull_override` so the run-end victory only fires after a
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
                    // v2 (C4) 2-D boss layout: warlord at back-row centre
                    // (2,0); one escort pushed a row forward to (2,1) — in
                    // front of the warlord, harassing the approach so the
                    // player must break past it; the other escort flanking on
                    // the back row at (3,0). All bow=S toward the player. The
                    // "clear the escorts, then flank the warlord's stern" read
                    // carries over: the forward escort blocks the lane to the
                    // boss, and the warlord's stern (facing N) is reachable
                    // only after clearing the front.
                    //
                    // Forward escort — one row closer to the player.
                    ShipSpawn {
                        class_id: "voidrunner".into(),
                        cell: crate::grid::Pos::new(2, 1).to_index(),
                        pos: crate::grid::Pos::new(2, 1),
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        facing: enemy_spawn_facing(),
                        hp_override: None,
                    },
                    // The warlord itself — back-row centre, bow facing the
                    // player. boss_ship_for_spawn supplies the rich loadout;
                    // hp_override stays None so the function's 14-hull default
                    // applies.
                    ShipSpawn {
                        class_id: "warlord".into(),
                        cell: crate::grid::Pos::new(2, 0).to_index(),
                        pos: crate::grid::Pos::new(2, 0),
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        facing: enemy_spawn_facing(),
                        hp_override: None,
                    },
                    // Flank escort — back row, off to one side of the warlord.
                    ShipSpawn {
                        class_id: "voidrunner".into(),
                        cell: crate::grid::Pos::new(3, 0).to_index(),
                        pos: crate::grid::Pos::new(3, 0),
                        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                        facing: enemy_spawn_facing(),
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
const fn wang_hash(mut x: u32) -> u32 {
    x = (x ^ 0x3D).wrapping_mul(0x27D4_EB2D);
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
    /// union of `intro[]` from sectors[`0..=up_to_idx`], display-name →
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
        Self { class_ids }
    }

    pub const fn is_empty(&self) -> bool {
        self.class_ids.is_empty()
    }
}

/// Resolve an intro/capital display name to a catalog enemy id. Already-id
/// (`snake_case`) forms pass through; otherwise looked up by lowercased
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
const fn encounter_enemy_count(lane: u8) -> usize {
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
    let node_seed = sector_def.node.bytes().fold(0u32, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    let base_seed = node_seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(u32::from(patrol_tier));

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

/// Sample `count` enemy spawns for one encounter from the pool, distributed
/// across the BACK ROWS of the 5×4 grid (v2 C4, replacing the v1 1-D pincer).
/// Enemies fill row 0 centre-out, then row 1, each bow=S toward the
/// front-centre player. Deterministic in `seed`.
///
/// The 1-D pincer's "threatened from both ends, rotate to face" intent becomes
/// the 2-D "lateral column spread + depth gradient": the player at the front
/// centre faces a wall of enemies fanned across the columns and two rows deep,
/// and dodges laterally between threatened columns while keeping its bow (N)
/// toward the incoming threat. `count` is capped by [`encounter_enemy_count`]
/// (≤4), well within the two back rows' 10 slots.
///
/// The legacy `cell` field is still populated (centre-out lane order) for the
/// transition window; the 2-D `pos` from [`enemy_spawn_pos`] is the source of
/// truth for placement.
fn sample_encounter_spawns(pool: &SpawnPool, lane: u8, count: usize, seed: u32) -> Vec<ShipSpawn> {
    // `lane` (the v1 1-D lane length) no longer drives distribution — the grid
    // is a fixed 5×4 and enemies fan across the back rows. Kept in the signature
    // for the `generate_sector` call site / future per-sector tuning.
    let _ = lane;
    if pool.is_empty() {
        return Vec::new();
    }
    // Cap at the back-row capacity (row 0 + row 1 = 2 * COLS slots). Encounters
    // never request this many, but keep placement total.
    let max_back_row = (crate::grid::ROWS.saturating_sub(1)) * crate::grid::COLS;
    let n = count.min(max_back_row);

    let mut spawns = Vec::with_capacity(n);
    for i in 0..n {
        let Some(pos) = enemy_spawn_pos(i) else {
            break; // back rows exhausted (guard; not reached at count ≤ 4)
        };
        let pick = wang_hash(seed.wrapping_add(i as u32)) as usize % pool.class_ids.len();
        let class_id = pool.class_ids[pick].clone();
        spawns.push(ShipSpawn {
            class_id,
            // Legacy 1-D cell for the transition window: the spawn's grid index.
            // Not load-bearing for placement (2-D `pos` is), just kept non-stale.
            cell: pos.to_index(),
            pos,
            // Bow toward the player (S). Set explicitly — NOT via
            // facing_from_orientation (see the 2-D spawn-geometry section).
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: enemy_spawn_facing(),
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
/// `class_id` so the bin/renderer can label it.
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
    // v2 (C4): a capital is a single ship — place it at the back-row centre
    // (row 0, centre column), bow=S toward the player. `lane` is no longer used
    // for placement (the grid is fixed 5×4); kept in the signature for the
    // capital-lookup call sites and future per-capital tuning.
    let _ = lane;
    let boss_pos = crate::grid::Pos::new(crate::grid::COLS / 2, 0);
    Some(ShipSpawn {
        // class_id carries the capital's canonical name; the bin's
        // spawn callback maps capitals to boss_ship_for_spawn. Until a
        // typed CapitalDef lands, all capitals share the boss synthesizer
        // (hull 14, ReactorBreach) — distinct per-capital stats are a
        // future content+architect follow-up.
        class_id: capital_name.to_string(),
        cell: boss_pos.to_index(),
        pos: boss_pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
        facing: enemy_spawn_facing(),
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
            level: 0,
            threats: vec![],
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
            encounters: vec![
                enc("c_e0", Vec::new(), false),
                enc("c_boss", Vec::new(), true),
            ],
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
        assert_eq!(
            run.current_sector_idx, 1,
            "advance lands in the combat sector"
        );
        assert_eq!(
            current_encounter(&run, &sectors).map(|e| e.id.as_str()),
            Some("c_boss"),
            "next encounter is the combat sector's boss",
        );

        // Clearing the final-sector boss flips victorious — the empty
        // leading sector never stalled the run.
        advance_after_win(&mut run, &sectors);
        assert!(
            run.victorious,
            "campaign completes; the empty sector did not stall it"
        );
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
            assert!(
                !s.encounters.is_empty(),
                "sector {} has no encounters",
                s.id
            );
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
        assert_eq!(
            boss_count, 1,
            "exactly one boss encounter total (the final)"
        );
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
        assert!(
            count[0] <= count[count.len() - 2],
            "first encounter shouldn't be denser than the boss-precursor"
        );
    }

    /* ---- build_encounter_board ------------------------------------- */

    // NOTE (C4): these three build_board tests carry MINIMAL green-keeping
    // patches to the new 2-D placement contract (player front-centre Pos(2,3);
    // ships stored at cells[pos.to_index()], invariant A). The proper 2-D
    // rewrite + expanded coverage (enemy_spawn_pos ordering, collision-vs-(2,3),
    // back-row fan) is the tester's follow-up, task #17.

    #[test]
    fn build_board_places_player_front_center() {
        // v2 (C4): player starts at the front-CENTRE grid cell Pos(2,3), bow N,
        // regardless of pre-board state. Slot == pos.to_index() (invariant A).
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: crate::grid::Pos::new(0, 0).to_index(),
                pos: crate::grid::Pos::new(0, 0),
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                facing: enemy_spawn_facing(),
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
        };
        let player = make_player(3, 8); // pre-board cell shouldn't matter
        let board =
            build_encounter_board(&enc, player, |spawn| Some(fallback_ship_for_spawn(spawn)));
        let ppos = player_start_pos();
        assert_eq!(ppos, crate::grid::Pos::new(2, 3));
        let at = board.ship_at(ppos).unwrap();
        assert_eq!(at.faction, Faction::Player);
        assert_eq!(at.pos, ppos);
        assert_eq!(
            at.cell,
            ppos.to_index(),
            "invariant A: slot == pos.to_index()"
        );
        assert_eq!(
            at.facing,
            player_spawn_facing(),
            "player bow N into the board"
        );
        // Hull state carries over (prior-encounter ship preserved, not reset).
        assert_eq!(at.hull, 8);
    }

    #[test]
    fn build_board_spawns_enemies_at_their_pos() {
        // v2 (C4): enemies placed at their 2-D spawn.pos (invariant A), not the
        // legacy 1-D cell. Two back-row spawns at (0,0) and (4,1).
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: crate::grid::Pos::new(0, 0).to_index(),
                    pos: crate::grid::Pos::new(0, 0),
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "lancer".into(),
                    cell: crate::grid::Pos::new(4, 1).to_index(),
                    pos: crate::grid::Pos::new(4, 1),
                    orientation: Orientation::Broadside,
                    facing: crate::grid::Facing::Broadside(crate::grid::Axis::EastWest),
                    hp_override: Some(7),
                },
            ],
            hazards: vec![],
            is_boss: false,
        };
        let board = build_encounter_board(&enc, make_player(0, 10), |spawn| {
            Some(fallback_ship_for_spawn(spawn))
        });
        let e1 = board.ship_at(crate::grid::Pos::new(0, 0)).unwrap();
        assert_eq!(e1.faction, Faction::Enemy);
        assert_eq!(e1.pos, crate::grid::Pos::new(0, 0), "invariant A");
        let e2 = board.ship_at(crate::grid::Pos::new(4, 1)).unwrap();
        assert_eq!(e2.faction, Faction::Enemy);
        assert_eq!(
            e2.facing,
            crate::grid::Facing::Broadside(crate::grid::Axis::EastWest)
        );
        assert_eq!(e2.hull, 7, "hp_override applied");
    }

    #[test]
    fn build_board_skips_player_pos_spawn_to_avoid_collision() {
        // v2 (C4): a spawn whose pos collides with the front-centre player
        // Pos(2,3) is dropped; a non-colliding back-row spawn still places.
        let enc = EncounterDef {
            id: "test".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: player_start_pos().to_index(),
                    pos: player_start_pos(), // tries to spawn ON the player
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: crate::grid::Pos::new(2, 0).to_index(),
                    pos: crate::grid::Pos::new(2, 0),
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                },
            ],
            hazards: vec![],
            is_boss: false,
        };
        let board = build_encounter_board(&enc, make_player(0, 10), |spawn| {
            Some(fallback_ship_for_spawn(spawn))
        });
        // Player kept its cell; the colliding enemy spawn was dropped.
        let at = board.ship_at(player_start_pos()).unwrap();
        assert_eq!(at.faction, Faction::Player);
        // The non-colliding back-row enemy still placed.
        assert_eq!(
            board.ship_at(crate::grid::Pos::new(2, 0)).unwrap().faction,
            Faction::Enemy
        );
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
    /// the `SectorDef` capital deserializer + enemies shape are real.
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
        assert_eq!(
            p1.class_ids,
            vec!["skiff".to_string(), "lancer".to_string()]
        );
        // At Ion Reefs (idx 2): pool carries forward + adds gunboat.
        let p2 = SpawnPool::accumulate(&cat.sectors, 2, &cat);
        assert_eq!(
            p2.class_ids,
            vec![
                "skiff".to_string(),
                "lancer".to_string(),
                "gunboat".to_string()
            ],
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
        // v2 (C4): non-boss encounters fan enemies across the BACK ROWS, all
        // bow=S, never on the front-centre player cell. (The proper back-row
        // distribution coverage is the tester's #17; this is the green-keeping
        // smoke check.)
        let ppos = player_start_pos();
        for e in &sector.encounters[..sector.encounters.len() - 1] {
            assert!(!e.is_boss);
            assert!(!e.enemy_ships.is_empty());
            for sp in &e.enemy_ships {
                assert_ne!(sp.pos, ppos, "enemies never spawn on the player cell");
                assert!(
                    sp.pos.row < crate::grid::ROWS - 1,
                    "enemies on the back rows"
                );
                assert_eq!(
                    sp.facing,
                    enemy_spawn_facing(),
                    "enemies bow S toward the player"
                );
                assert_eq!(
                    sp.cell,
                    sp.pos.to_index(),
                    "invariant A: legacy cell tracks pos"
                );
                assert!(
                    pool.class_ids.contains(&sp.class_id),
                    "spawn {} drawn from the pool",
                    sp.class_id,
                );
            }
            // Lane-5 sector → 2 enemies per encounter (encounter_enemy_count).
            assert_eq!(e.enemy_ships.len(), 2);
        }
    }

    /* ---- #17: 2-D spawn-distribution coverage (the back-row fan) ---------
     *
     * The four build_board_* tests above pin PLACEMENT (a ship lands at
     * cells[pos.to_index()]). These pin GENERATION — the private helpers C4
     * uses to choose where enemies go: the centre-out column order, the
     * row-fill walk, the placeholder cell re-key, and that a generated
     * encounter's spawns are a mutually-disjoint back-row set that never
     * touches the front-centre player. (`use super::*` brings the private
     * helpers into scope; they have no external test surface.)
     * ------------------------------------------------------------------ */

    #[test]
    fn back_row_column_order_is_centre_out() {
        // Centre first, then alternate out: the small-encounter clustering that
        // puts the first enemies dead in front of the front-centre player.
        assert_eq!(back_row_column_order(), vec![2, 1, 3, 0, 4]);
        // It is a permutation of every column (no column dropped or repeated).
        let mut sorted = back_row_column_order();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..crate::grid::COLS).collect::<Vec<_>>());
        assert_eq!(back_row_column_order().len(), crate::grid::COLS);
    }

    #[test]
    fn enemy_spawn_pos_fills_row_zero_centre_out_then_row_one() {
        // i 0..COLS fills row 0 in centre-out order; the next COLS fills row 1
        // in the same column order. This is the wall-with-depth (decision #8).
        let order = [2usize, 1, 3, 0, 4];
        for (i, &col) in order.iter().enumerate() {
            assert_eq!(
                enemy_spawn_pos(i),
                Some(crate::grid::Pos::new(col, 0)),
                "row-0 slot {i}"
            );
        }
        for (k, &col) in order.iter().enumerate() {
            let i = crate::grid::COLS + k;
            assert_eq!(
                enemy_spawn_pos(i),
                Some(crate::grid::Pos::new(col, 1)),
                "row-1 slot {i}"
            );
        }
    }

    #[test]
    fn enemy_spawn_pos_never_lands_on_the_front_player_row() {
        // Every Some(pos) is on a back row (row < ROWS-1), so an enemy can never
        // be generated onto the player's front row. Walk well past the cap.
        for i in 0..(crate::grid::CELLS * 2) {
            if let Some(pos) = enemy_spawn_pos(i) {
                assert!(pos.in_bounds(), "slot {i} pos {pos:?} in bounds");
                assert!(
                    pos.row < crate::grid::ROWS - 1,
                    "slot {i} pos {pos:?} is a back row"
                );
            }
        }
    }

    #[test]
    fn enemy_spawn_pos_exhausts_after_the_back_rows() {
        // The back rows hold (ROWS-1) full rows of COLS = the only Some slots;
        // the first index past them is None. (Encounters cap at 4 enemies, so
        // this None is a guard, but pin the exact boundary so a row-count change
        // is a visible break.)
        let back_slots = (crate::grid::ROWS - 1) * crate::grid::COLS;
        assert!(
            enemy_spawn_pos(back_slots - 1).is_some(),
            "last back-row slot is Some"
        );
        assert_eq!(
            enemy_spawn_pos(back_slots),
            None,
            "first slot past the back rows is None"
        );
        assert_eq!(enemy_spawn_pos(back_slots + 1), None);
    }

    #[test]
    fn enemy_spawn_pos_slots_are_mutually_distinct_within_the_back_rows() {
        // No two distinct in-range slots share a cell — a generated encounter
        // never double-books a back-row cell.
        let back_slots = (crate::grid::ROWS - 1) * crate::grid::COLS;
        let mut seen = std::collections::HashSet::new();
        for i in 0..back_slots {
            let pos = enemy_spawn_pos(i).expect("in-range slot is Some");
            assert!(seen.insert(pos), "slot {i} pos {pos:?} duplicated");
        }
        assert_eq!(
            seen.len(),
            back_slots,
            "every back-row cell used exactly once"
        );
    }

    #[test]
    fn placeholder_cell_to_pos_rekeys_onto_the_back_rows() {
        // 1-D placeholder cells fan across columns (mod COLS), dropping a row per
        // full wrap, clamped to the back rows (row <= ROWS-2).
        assert_eq!(placeholder_cell_to_pos(0), crate::grid::Pos::new(0, 0));
        assert_eq!(placeholder_cell_to_pos(2), crate::grid::Pos::new(2, 0));
        assert_eq!(
            placeholder_cell_to_pos(crate::grid::COLS - 1),
            crate::grid::Pos::new(crate::grid::COLS - 1, 0)
        );
        // Wrapping past COLS drops to row 1.
        assert_eq!(
            placeholder_cell_to_pos(crate::grid::COLS),
            crate::grid::Pos::new(0, 1)
        );
        assert_eq!(
            placeholder_cell_to_pos(crate::grid::COLS + 3),
            crate::grid::Pos::new(3, 1)
        );
        // A large cell clamps the row to the last back row (ROWS-2), never the
        // player's front row.
        let big = placeholder_cell_to_pos(crate::grid::CELLS * 3);
        assert!(
            big.row <= crate::grid::ROWS - 2,
            "clamped off the front row: {big:?}"
        );
        assert!(big.in_bounds());
    }

    #[test]
    fn placeholder_cell_to_pos_never_returns_the_front_player_row() {
        // Over a wide sweep of placeholder cells, none re-keys onto row ROWS-1.
        for cell in 0..(crate::grid::CELLS * 4) {
            let pos = placeholder_cell_to_pos(cell);
            assert!(pos.in_bounds(), "cell {cell} -> {pos:?} in bounds");
            assert!(
                pos.row < crate::grid::ROWS - 1,
                "cell {cell} -> {pos:?} stays off the front row"
            );
        }
    }

    #[test]
    fn generated_encounter_spawns_are_a_disjoint_back_row_set_clear_of_the_player() {
        // End-to-end on the generation path: every non-boss encounter's spawns
        // occupy DISTINCT back-row cells, none on the front-centre player. This
        // is the property the build-board placement relies on (no two enemies
        // collide, player cell stays free) — asserted on real generated data.
        let cat = gen_catalog();
        let pool = SpawnPool::accumulate(&cat.sectors, 1, &cat);
        let sector = generate_sector(&cat.sectors[1], &pool, 1, &cat);
        let ppos = player_start_pos();
        for e in sector.encounters.iter().filter(|e| !e.is_boss) {
            let mut seen = std::collections::HashSet::new();
            for sp in &e.enemy_ships {
                assert!(sp.pos.in_bounds(), "spawn {sp:?} in bounds");
                assert!(
                    sp.pos.row < crate::grid::ROWS - 1,
                    "spawn on a back row: {:?}",
                    sp.pos
                );
                assert_ne!(sp.pos, ppos, "spawn never on the player cell");
                assert!(seen.insert(sp.pos), "two spawns share cell {:?}", sp.pos);
            }
        }
    }

    #[test]
    fn build_board_fans_a_full_encounter_across_distinct_back_row_cells() {
        // Drive build_encounter_board with the centre-out enemy_spawn_pos slots
        // for a max-size (4) encounter: all four land at their distinct back-row
        // cells (invariant A), and the player holds the front-centre cell. Pins
        // that placement preserves the generated fan without collision.
        let n: usize = 4;
        let enemy_ships: Vec<ShipSpawn> = (0..n)
            .map(|i| {
                let pos = enemy_spawn_pos(i).expect("slots 0..4 are Some");
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: pos.to_index(),
                    pos,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                }
            })
            .collect();
        let board = build_encounter_board(
            &EncounterDef {
                id: "fan".into(),
                enemy_ships,
                hazards: vec![],
                is_boss: false,
            },
            make_player(0, 10),
            |spawn| Some(fallback_ship_for_spawn(spawn)),
        );
        // Player at front-centre.
        assert_eq!(
            board.ship_at(player_start_pos()).unwrap().faction,
            Faction::Player
        );
        // All four enemies present, each at its slot's cell (invariant A).
        let mut enemy_cells = std::collections::HashSet::new();
        for i in 0..n {
            let pos = enemy_spawn_pos(i).unwrap();
            let ship = board
                .ship_at(pos)
                .unwrap_or_else(|| panic!("enemy missing at slot {i} {pos:?}"));
            assert_eq!(ship.faction, Faction::Enemy, "slot {i}");
            assert_eq!(ship.cell, pos.to_index(), "invariant A at slot {i}");
            assert_eq!(ship.facing, enemy_spawn_facing(), "enemy bow S at slot {i}");
            assert!(
                enemy_cells.insert(pos),
                "slot {i} cell {pos:?} double-booked"
            );
        }
        assert_eq!(enemy_cells.len(), n, "four distinct enemy cells");
    }

    /* ---- #69: capitals synthesize as ARMED bosses, not popguns -------- */

    #[test]
    fn is_capital_spawn_matches_catalog_capitals_by_name() {
        let cat = gen_catalog();
        // The generator writes the capital's DISPLAY name into class_id.
        assert!(is_capital_spawn("The Dasher", &cat), "known capital");
        assert!(
            !is_capital_spawn("skiff", &cat),
            "regular enemy is not a capital"
        );
        assert!(
            !is_capital_spawn("warlord", &cat),
            "warlord id is not a catalog capital name"
        );
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
        assert!(
            boss.hull > popgun.hull,
            "capital boss must out-hull the fallback ({} vs {})",
            boss.hull,
            popgun.hull
        );
        assert!(
            boss.mounts.len() > popgun.mounts.len(),
            "capital boss must out-gun the fallback ({} vs {} mounts)",
            boss.mounts.len(),
            popgun.mounts.len()
        );
        // Carries the boss's signature ReactorBreach pressure trait.
        assert!(
            boss.traits.contains(&Trait::ReactorBreach),
            "capital boss carries ReactorBreach"
        );
        // Identity preserved so the HUD label + salvage-by-name lookup still work.
        assert_eq!(boss.klass.as_deref(), Some("The Dasher"));
        // hp_override still wins (lets an encounter tier-scale a capital).
        let mut sp2 = sp;
        sp2.hp_override = Some(25);
        assert_eq!(capital_boss_ship_for_spawn(&sp2, &cat).hull, 25);
    }

    #[test]
    fn generate_sector_is_deterministic() {
        let cat = gen_catalog();
        let pool = SpawnPool::accumulate(&cat.sectors, 2, &cat);
        let a = generate_sector(&cat.sectors[2], &pool, 3, &cat);
        let b = generate_sector(&cat.sectors[2], &pool, 3, &cat);
        assert_eq!(
            a, b,
            "same (node, tier, pool) → identical sector (#111 determinism)"
        );
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
        assert_eq!(
            campaign.len(),
            cat.sectors.len(),
            "one runtime Sector per SectorDef"
        );
        // Lane sizes carry through (Ion Reefs is lane 7 → 3 enemies/encounter).
        let ion = &campaign[2];
        let non_boss = ion.encounters.iter().find(|e| !e.is_boss).unwrap();
        assert_eq!(
            non_boss.enemy_ships.len(),
            3,
            "lane 7 → 3 enemies per encounter"
        );
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
