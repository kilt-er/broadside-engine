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

/// Build a fresh [`Board`] for the encounter on the *default* 5×4 grid (v2 C4).
/// Existing callers keep the default — this is a thin wrapper over
/// [`build_encounter_board_with_dims`] passing [`crate::grid::Dims::default`].
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
/// **INVARIANT (A) — slot == `pos.to_index_in(dims)`.** Every ship is stored at
/// `cells[ship.pos.to_index_in(dims)]` with `ship.pos` set to its real grid
/// [`Pos`], so [`Board::ship_at`] called with `pos` returns
/// `cells[pos.to_index_in(dims)]`. The resolver's R3 ray-walk and R4
/// `apply_damage` depend on this; a ship whose slot and `pos` disagree is
/// invisible to `ship_at`. A spawn whose `pos` collides with an already-placed
/// ship (or the player) is skipped — placeholder/generated sectors are
/// collision-free by construction, but a hand-authored sector with a duplicate
/// cell won't corrupt the board.
///
/// Hazards on the encounter populate `board.hazards` at their 2-D [`Hazard::pos`].
pub fn build_encounter_board<F>(encounter: &EncounterDef, player: Ship, class_to_ship: F) -> Board
where
    F: FnMut(&ShipSpawn) -> Option<Ship>,
{
    // #199b: route through `_with_dims` using the encounter's own `dims` field
    // — for the legacy 5x4 path that's `Dims::default()` (via the type's
    // `#[serde(default = "default_dims")]`); for the random-encounter-size
    // flip, this is the per-encounter rolled `Dims` from `generate_sector`.
    // Every existing call site picks up the rolled dims for free.
    build_encounter_board_with_dims(encounter, player, encounter.dims, class_to_ship)
}

/// Variable-board (#199) variant of [`build_encounter_board`]: build a fresh
/// [`Board`] on the supplied runtime `dims` grid. Existing 5×4 callers use the
/// thin wrapper above; the random-size encounter feature (separate later step)
/// will call this one directly with the rolled dims.
///
/// ## Per-shape behaviour
///
/// - Player goes to [`player_start_pos_in`] of `dims` (front-centre on `dims`),
///   bow N. The legacy 1-D `cell` is `pos.to_index_in(dims)` (NOT the default
///   `to_index()`) so a non-5-wide board still has a consistent legacy field.
/// - Each `ShipSpawn` is placed at its `pos` if `pos.in_bounds_in(dims)` AND
///   `pos != player_pos` — out-of-bounds spawns are dropped (e.g. a 5×4-shaped
///   encounter author placing at `(4,0)` on a 3×3 board), as are spawns that
///   collide with the player's front-centre cell. This is **defensive**: a
///   well-formed dim-aware encounter (built via
///   [`sample_encounter_spawns_with_dims`]) never produces an out-of-bounds
///   spawn for its own grid.
/// - `ship.cell` is **derived** from `spawn.pos.to_index_in(dims)` inside the
///   loop (reviewer rec, #199): the spawn's `cell` field is ignored for
///   placement so it can never go stale on a non-5 board. This is the
///   collapse-redundancy half of the variable-board feature — `ShipSpawn.cell`
///   stays in the type only for serde fixture compatibility.
/// - Hazards are placed at `hazard.pos` if in-bounds; out-of-bounds hazards are
///   silently dropped (same defensive policy as spawns).
///
/// The `Board.size`/`cols`/`rows` fields all carry `dims` so downstream
/// `board.dims()` reflects the actual grid.
pub fn build_encounter_board_with_dims<F>(
    encounter: &EncounterDef,
    mut player: Ship,
    dims: crate::grid::Dims,
    mut class_to_ship: F,
) -> Board
where
    F: FnMut(&ShipSpawn) -> Option<Ship>,
{
    // Backing Vecs sized to the runtime grid so the 2-D occupancy view
    // (Board::ship_at(pos) = cells[pos.to_index_in(dims)]) is valid over the
    // whole grid. The cell vector is dense (`dims.cell_count()` Nones) so the
    // resolver's `cells.iter()` scans cover every grid cell.
    let mut cells: Vec<Option<Ship>> = (0..dims.cell_count()).map(|_| None).collect();
    let mut hazards: Vec<Vec<crate::types::Hazard>> =
        (0..dims.cell_count()).map(|_| Vec::new()).collect();

    // Player at the front-centre cell of `dims`, bow pointed N (into the board,
    // toward the enemies). Normalize both the 2-D pos/facing and the legacy 1-D
    // cell/orientation. Invariant (A): slot == pos.to_index_in(dims). The
    // legacy `cell` is the dim-aware index (not `to_index()`), so a non-5 board
    // doesn't leave a stale 5-wide cell on the player.
    let player_pos = player_start_pos_in(dims);
    let player_idx = player_pos.to_index_in(dims);
    player.pos = player_pos;
    player.facing = player_spawn_facing();
    player.cell = player_idx;
    player.orientation = Orientation::BowOn { bow: LaneEnd::Fore };
    // Guard against a degenerate `Dims` (e.g. 0×0) yielding an out-of-range
    // index; on every real shape (rows>=1 && cols>=1) the player slot exists.
    if player_idx < cells.len() {
        cells[player_idx] = Some(player);
    }

    // Place each enemy spawn at its 2-D pos (invariant A). `cell` is DERIVED
    // from `pos.to_index_in(dims)` — the spawn's serialized `cell` field is
    // not consulted for placement, so it can't go stale on a non-5 board.
    for spawn in &encounter.enemy_ships {
        if !spawn.pos.in_bounds_in(dims) || spawn.pos == player_pos {
            // Off-grid (for THIS grid) or colliding with the player — skip.
            // TODO(broadside-content): log a debug! when an authored spawn is
            // dropped for out-of-grid (a 5×4-shaped encounter rolled onto a
            // smaller `dims`); silent today so a non-default-size encounter
            // doesn't surface as a spam in the canonical 5×4 campaign.
            continue;
        }
        let idx = spawn.pos.to_index_in(dims);
        if cells[idx].is_some() {
            continue; // a prior spawn already holds this cell
        }
        if let Some(mut ship) = class_to_ship(spawn) {
            // Force slot == pos.to_index_in(dims): trust the spawn's
            // authoritative 2-D pos/facing over whatever the builder defaulted,
            // and derive `cell` from `pos` (not from the spawn's `cell` field).
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

    // Drop hazards into their 2-D cells (dim-aware bounds).
    for h in &encounter.hazards {
        if h.pos.in_bounds_in(dims) {
            hazards[h.pos.to_index_in(dims)].push(h.clone());
        }
    }

    Board {
        // `size` is the legacy 1-D lane length, kept for the transition window;
        // 2-D placement uses the runtime grid. Report the grid width so any
        // remaining 1-D reader sees a sane lane spanning the columns.
        size: dims.cols,
        cols: dims.cols,
        rows: dims.rows,
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

/// The player's fixed 2-D start cell on the *default* 5×4 grid: front-centre
/// (`col = COLS/2`, the front row `ROWS-1`). Blueprint decision #8 — the player
/// anchors at the front and the rows ahead are pure dodge space.
///
/// For a runtime-sized grid use [`player_start_pos_in`].
pub const fn player_start_pos() -> crate::grid::Pos {
    crate::grid::Pos::new(crate::grid::COLS / 2, crate::grid::ROWS - 1)
}

/// The player's front-centre start cell on the runtime `dims` grid:
/// `(dims.center_col(), dims.front_row())`. The dim-aware mirror of
/// [`player_start_pos`]; on a default `Dims` the two agree byte-for-byte.
///
/// Variable-board feature (#199): every shape — 2×2, 3×3, 4×4, 5×4 — anchors the
/// player at front-centre on its own grid. On odd-width grids that is the exact
/// middle column; on even-width grids `center_col() == cols / 2` (slightly
/// right-of-centre), which keeps the same closed-form rule across all shapes.
#[must_use]
pub const fn player_start_pos_in(dims: crate::grid::Dims) -> crate::grid::Pos {
    crate::grid::Pos::new(dims.center_col(), dims.front_row())
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

/// Variable-board enemy cap (#199): the maximum number of enemies an encounter
/// should spawn onto a `dims`-sized board so the fight stays **legible +
/// winnable** on every pool shape (2×2 .. 5×4).
///
/// ## The formula (canonical — Bruce's ruling)
///
/// Capacity-by-shape trades geometric capacity for legibility. The width-minus-
/// one rule (always leave one back-row column as a clean dodge lane) is the
/// binding livability constraint; the 4-enemy ceiling is the same telegraph
/// limit `encounter_enemy_count`'s tier curve maxes at on the canonical 5×4.
///
/// 1. `back_capacity = (rows - 1) * cols` — actual back-row slots.
/// 2. `geometric_cap = back_capacity.min(4)` — never exceed the canonical
///    4-enemy ceiling.
/// 3. `narrow_cap = (cols - 1).max(1)` — leave at least one back-row column
///    as a clean dodge lane (cap == `cols - 1`), with a hard floor of 1 for
///    the `cols == 1` corner case.
/// 4. `cap = geometric_cap.min(narrow_cap)`, then `0` when `rows < 2` (no
///    back row at all → no enemies fit anywhere).
///
/// ## What the formula produces (the canonical table)
///
/// Each row is `back = (rows-1)*cols`, `geo = min(back, 4)`, `narrow =
/// max(cols-1, 1)`, `cap = min(geo, narrow)`:
///
/// | Shape | back | geo | narrow | **cap** |
/// |-------|------|-----|--------|---------|
/// | 2×2   |  2   |  2  |   1    | **1** |
/// | 3×2   |  3   |  3  |   2    | **2** |
/// | 4×2   |  4   |  4  |   3    | **3** |
/// | 5×2   |  5   |  4  |   4    | **4** |
/// | 2×3   |  4   |  4  |   1    | **1** |
/// | 3×3   |  6   |  4  |   2    | **2** |
/// | 4×3   |  8   |  4  |   3    | **3** |
/// | 5×3   | 10   |  4  |   4    | **4** |
/// | 2×4   |  6   |  4  |   1    | **1** |
/// | 3×4   |  9   |  4  |   2    | **2** |
/// | 4×4   | 12   |  4  |   3    | **3** |
/// | 5×4   | 15   |  4  |   4    | **4** |
///
/// `cols == 2` boards always cap at 1 (the `narrow_cap` floor). Shallow
/// boards (`rows == 2`) hit the cap via `narrow_cap` rather than `geo`, so
/// widening past 4 keeps shipping more enemies because the dodge-lane
/// constraint is the binding one — 5×2 fields 4 enemies on the single back
/// row, which is the max-pressure shape in the pool.
///
/// Returns `0` on `dims.rows < 2` (no back row) or `dims.cols == 0`. The
/// caller's `sample_encounter_spawns` / `build_encounter_board` further filter
/// out spawns that collide with the player or land off-grid.
#[must_use]
pub const fn max_enemies_in(dims: crate::grid::Dims) -> usize {
    if dims.rows < 2 || dims.cols == 0 {
        return 0;
    }
    let back_capacity = (dims.rows - 1) * dims.cols;
    let geometric_cap = if back_capacity > 4 { 4 } else { back_capacity };
    // Leave at least one column as a dodge lane; floor of 1 for cols==1.
    let narrow_cap = if dims.cols > 1 { dims.cols - 1 } else { 1 };
    if geometric_cap < narrow_cap {
        geometric_cap
    } else {
        narrow_cap
    }
}

/// Column order for fanning enemies across one back row on the *default* 5×4
/// grid: centre-out (`2, 1, 3, 0, 4` for `COLS == 5`) so small encounters
/// cluster toward the middle (in front of the front-centre player) and larger
/// ones fan to the edges. Deterministic and total over `0..COLS`.
///
/// For a runtime-sized grid use [`back_row_column_order_in`]. Test-only thin
/// wrapper kept for the legacy regression tests that pin the default-size
/// centre-out fan; production calls go through `_in`.
#[cfg(test)]
fn back_row_column_order() -> Vec<usize> {
    back_row_column_order_in(crate::grid::Dims::default())
}

/// Column order for fanning enemies across one back row on the runtime `dims`
/// grid: centre-out from `dims.center_col()`. Deterministic and total over
/// `0..dims.cols`. The dim-aware mirror of [`back_row_column_order`]; the two
/// agree at the default size. An even-width grid centres on `cols / 2` (the
/// `Dims::center_col` rule), so 4-wide is `2, 1, 3, 0` (slightly right-biased,
/// consistent with the odd-width rule).
fn back_row_column_order_in(dims: crate::grid::Dims) -> Vec<usize> {
    let cols = dims.cols;
    if cols == 0 {
        return Vec::new();
    }
    let mid = dims.center_col();
    let mut out = vec![mid];
    let mut k = 1usize;
    while out.len() < cols {
        if mid >= k {
            out.push(mid - k);
        }
        if mid + k < cols {
            out.push(mid + k);
        }
        k += 1;
    }
    out
}

/// The back-row [`crate::grid::Pos`] for the `i`-th enemy on the *default* 5×4
/// grid. For a runtime-sized grid use [`enemy_spawn_pos_in`]. Test-only thin
/// wrapper kept for the legacy default-size regression tests; production calls
/// go through `_in`.
#[cfg(test)]
fn enemy_spawn_pos(i: usize) -> Option<crate::grid::Pos> {
    enemy_spawn_pos_in(i, crate::grid::Dims::default())
}

/// The back-row [`crate::grid::Pos`] for the `i`-th enemy on the runtime `dims`
/// grid: fill the topmost back row across the centre-out column order, then the
/// next back row, … all the way down to (but excluding) the player's front row
/// `dims.front_row()`. Returns `None` once the back rows are exhausted, or for a
/// grid with `rows < 2` (no back rows at all — caller falls through to no
/// enemies).
///
/// The depth gradient (decision #8): top-row enemies are Far/Near from the
/// front-centre player, lower-row ones one band closer, so the player reads a
/// wall with depth and dodges laterally between threatened columns. The
/// centre-out fill (via [`back_row_column_order_in`]) means small encounters
/// cluster directly in front of the player and larger ones fan to the edges,
/// so a Far-band weapon can bear on the centre-of-mass on every shape.
fn enemy_spawn_pos_in(i: usize, dims: crate::grid::Dims) -> Option<crate::grid::Pos> {
    if dims.rows < 2 || dims.cols == 0 {
        return None; // no back rows at all (rows<=1) or zero-width grid
    }
    let order = back_row_column_order_in(dims);
    let per_row = order.len(); // == dims.cols
    let row = i / per_row;
    let col = order[i % per_row];
    // Enemies occupy the back rows only; never the player's front row
    // (`dims.front_row()`). Rows 0..front_row() are back; rows >= front_row()
    // are off-limits.
    if row >= dims.front_row() {
        return None;
    }
    Some(crate::grid::Pos::new(col, row))
}

/// Map a placeholder sector's 1-D lane `cell` onto a back-row 2-D [`Pos`] on
/// the *default* 5×4 grid (the placeholder sectors below author 1-D cells; this
/// re-keys them onto the grid). For a runtime-sized grid use
/// [`placeholder_cell_to_pos_in`].
fn placeholder_cell_to_pos(cell: usize) -> crate::grid::Pos {
    placeholder_cell_to_pos_in(cell, crate::grid::Dims::default())
}

/// Map a placeholder sector's 1-D lane `cell` onto a back-row 2-D [`Pos`] on
/// the runtime `dims` grid. Columns wrap across `dims.cols`; each full wrap
/// drops to the next back row, so a spread of cells fans across row 0 then row
/// 1 … . Bounded to the back rows — a cell that would overflow past the back
/// rows clamps to the last back row (defensive; placeholder cells stay small).
/// A `rows<2` grid clamps to row 0 with `col = cell % cols` (placement still
/// goes through `build_encounter_board`'s collision filter, which will then
/// drop the spawn since it lands on the player's row).
fn placeholder_cell_to_pos_in(cell: usize, dims: crate::grid::Dims) -> crate::grid::Pos {
    let cols = dims.cols.max(1); // guard the modulus on a degenerate 0-wide
    let col = cell % cols;
    // Last back row = front_row() - 1; saturating at 0 for a rows<2 grid so we
    // never produce a negative row. `dims.rows.saturating_sub(2)` is the max
    // valid back-row index (`front_row() - 1`) for rows>=2; saturates to 0 for
    // smaller grids.
    let last_back = dims.rows.saturating_sub(2);
    let row = (cell / cols).min(last_back);
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
        ..Default::default()
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
                ..Default::default()
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
/// fight. **#210 parallax-flow piece 1:** bumped 2 → 4 so each sector
/// runs as 4 rounds + 1 boss = 5 beats per level. Locks the round count
/// Bruce's parallax-progression cadence reads against (every round bumps
/// the parallax background one step; the boss closes the level). Bruce-
/// ratified; flagged here so further re-tuning is one-line.
///
/// Boss-at-end still holds at any count — [`generate_sector`] pushes the
/// capital encounter AFTER this loop, so the last encounter in the
/// returned sector is always the boss (when the sector has one).
pub const ENCOUNTERS_PER_SECTOR: u32 = 4;

/// **#199b reversible gate:** when `true`, [`generate_sector`] rolls a random
/// per-encounter [`Dims`] from the pool [`VARIABLE_ENCOUNTER_DIMS_POOL`] for
/// every non-boss encounter (and a larger floor-pool for the boss, see
/// [`VARIABLE_ENCOUNTER_BOSS_DIMS_POOL`]). When `false`, every encounter
/// stays on the canonical 5x4 grid — `EncounterDef::dims` is always
/// [`crate::grid::Dims::default`] and behaviour is byte-identical to the
/// pre-flip campaign. Bruce ratified the random-per-encounter variation as
/// the default, but the one-line flip back is preserved here so a bad
/// small-board playtest can be reverted without surgery.
pub const VARIABLE_ENCOUNTER_DIMS: bool = true;

/// **#199b** the random pool from which [`generate_sector`] rolls a non-boss
/// encounter's [`Dims`]. Sourced from the lead's brief (the variable-board
/// design pool); each shape is uniformly sampled per encounter so a
/// sector's 4 rounds read as 4 distinct arenas. Includes 5x4 (the canonical
/// default) so the rolled variety still occasionally feels familiar. The
/// per-shape winnability of every entry is locked by the #199 substrate
/// (per-shape spawn cap via [`max_enemies_in`] + tester's
/// `combat_per_shape` suite).
pub const VARIABLE_ENCOUNTER_DIMS_POOL: &[(usize, usize)] = &[
    (2, 2),
    (2, 3),
    (3, 2),
    (2, 4),
    (4, 2),
    (3, 3),
    (3, 4),
    (4, 3),
    (4, 4),
    (5, 4),
];

/// **#199b** the random pool for boss encounters — narrower than
/// [`VARIABLE_ENCOUNTER_DIMS_POOL`] so the climactic fight isn't cramped onto
/// a 2x2. Boss rounds floor at 4x3 (3 enemies + dodge lane on a 2-row-deep
/// back-field is the smallest shape that gives a single boss meaningful
/// stand-off). Each entry passes [`max_enemies_in`] >= 3 so a 3-mount armed
/// boss has room to manoeuvre. Bruce's call: "don't put a boss on a 2x2"; I
/// pick the exact floor here.
pub const VARIABLE_ENCOUNTER_BOSS_DIMS_POOL: &[(usize, usize)] = &[(4, 3), (4, 4), (5, 4)];

/// **#199b** roll a per-encounter [`Dims`] from `pool` using `seed`, the
/// existing campaign wang-hash PRNG so generation stays deterministic in
/// `(sector node, patrol tier, encounter index)`. No `rand` crate, no global
/// RNG — keeps the resolver/run-loop free of non-determinism the tests
/// would flake on (#111 guarantee). Returns [`crate::grid::Dims::default`]
/// if `pool` is empty (defensive — never reached, both pool consts are
/// non-empty by const).
fn roll_encounter_dims(pool: &[(usize, usize)], seed: u32) -> crate::grid::Dims {
    if pool.is_empty() {
        return crate::grid::Dims::default();
    }
    let pick = wang_hash(seed) as usize % pool.len();
    let (cols, rows) = pool[pick];
    crate::grid::Dims::new(cols, rows)
}

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
            // #199b: roll a per-encounter Dims when the flip is on; otherwise
            // stay on the canonical 5x4. The dims-roll seed is offset from
            // the spawn-sample seed so dims + spawns are independent (a size
            // change doesn't shift which classes get picked) — keeps
            // determinism transparent under future tuning. Sampler runs ON
            // the rolled dims so spawn `pos`s land inside `dims`, and the
            // per-shape count cap via `max_enemies_in(dims)` keeps small
            // boards uncrowded.
            let spawn_seed = base_seed.wrapping_add(e.wrapping_mul(0x1000_0001));
            let dims = if VARIABLE_ENCOUNTER_DIMS {
                roll_encounter_dims(
                    VARIABLE_ENCOUNTER_DIMS_POOL,
                    spawn_seed.wrapping_add(0x0D15_D115),
                )
            } else {
                crate::grid::Dims::default()
            };
            let enemy_ships =
                sample_encounter_spawns_with_dims(pool, sector_def.lane, count, spawn_seed, dims);
            if enemy_ships.is_empty() {
                continue;
            }
            encounters.push(EncounterDef {
                id: format!("{}_e{e}", sector_def.node),
                enemy_ships,
                hazards: Vec::new(),
                is_boss: false,
                dims,
            });
        }
    }

    // Capital boss encounter at sector end (if this sector has a capital).
    // #199b: bosses roll from the LARGER pool (floor 4x3) so no boss lands on
    // a 2x2 — Bruce's "don't cramp the boss" call. With the flip off, every
    // boss stays on 5x4.
    let boss_dims = if VARIABLE_ENCOUNTER_DIMS {
        roll_encounter_dims(
            VARIABLE_ENCOUNTER_BOSS_DIMS_POOL,
            base_seed.wrapping_add(0xB055_B055),
        )
    } else {
        crate::grid::Dims::default()
    };
    if let Some(boss) = sector_def
        .capital
        .as_ref()
        .and_then(|cap| capital_spawn_with_dims(cap, sector_def.lane, catalog, boss_dims))
    {
        encounters.push(EncounterDef {
            id: format!("{}_boss", sector_def.node),
            enemy_ships: vec![boss],
            hazards: Vec::new(),
            is_boss: true,
            dims: boss_dims,
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
///
/// Test-only thin wrapper since #199b — production [`generate_sector`] now
/// calls [`sample_encounter_spawns_with_dims`] directly with the rolled
/// per-encounter `Dims`. Kept under `#[cfg(test)] #[allow(dead_code)]` so a
/// future regression test pinning the centre-out fan at the canonical 5×4
/// can call this without an explicit Dims arg; currently no test uses it.
#[cfg(test)]
#[allow(dead_code)]
fn sample_encounter_spawns(pool: &SpawnPool, lane: u8, count: usize, seed: u32) -> Vec<ShipSpawn> {
    sample_encounter_spawns_with_dims(pool, lane, count, seed, crate::grid::Dims::default())
}

/// Variable-board (#199) variant of [`sample_encounter_spawns`]: fan
/// `count` enemies across the back rows of the runtime `dims` grid in
/// centre-out order, with the per-shape liveability cap from
/// [`max_enemies_in`] applied on top. `lane` is held in the signature for
/// caller parity but no longer drives distribution.
///
/// The result count is `count.min(max_enemies_in(dims))` — so a 2×2 board
/// never gets more than 1 enemy even if the caller asked for 4. Spawns
/// beyond the back-row capacity are simply dropped (`enemy_spawn_pos_in`
/// returns `None`), which is a defensive guard since the per-shape cap
/// already keeps the count within the rows.
pub(crate) fn sample_encounter_spawns_with_dims(
    pool: &SpawnPool,
    lane: u8,
    count: usize,
    seed: u32,
    dims: crate::grid::Dims,
) -> Vec<ShipSpawn> {
    let _ = lane;
    if pool.is_empty() {
        return Vec::new();
    }
    // Variable-board cap (#199): per-shape liveability ceiling. On the default
    // 5×4 grid `max_enemies_in == 4`, matching the legacy
    // `encounter_enemy_count` ceiling so existing behaviour is preserved; on
    // smaller shapes the cap shrinks per the table on [`max_enemies_in`].
    let n = count.min(max_enemies_in(dims));

    let mut spawns = Vec::with_capacity(n);
    for i in 0..n {
        let Some(pos) = enemy_spawn_pos_in(i, dims) else {
            break; // back rows exhausted (extra-defensive given the cap above)
        };
        let pick = wang_hash(seed.wrapping_add(i as u32)) as usize % pool.class_ids.len();
        let class_id = pool.class_ids[pick].clone();
        spawns.push(ShipSpawn {
            class_id,
            // Legacy 1-D cell for the transition window: dim-aware grid index.
            // Not load-bearing for placement (2-D `pos` is), just kept
            // non-stale on a non-5 board.
            cell: pos.to_index_in(dims),
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
///
/// Test-only thin wrapper since #199b — production [`generate_sector`] now
/// calls [`capital_spawn_with_dims`] directly with the boss's rolled `Dims`.
/// Kept under `#[cfg(test)] #[allow(dead_code)]` so a future regression
/// test pinning the canonical 5×4 boss placement can call it directly;
/// currently no test uses it.
#[cfg(test)]
#[allow(dead_code)]
fn capital_spawn(capital_name: &str, lane: u8, catalog: &Catalog) -> Option<ShipSpawn> {
    capital_spawn_with_dims(capital_name, lane, catalog, crate::grid::Dims::default())
}

/// Variable-board (#199) variant of [`capital_spawn`]: place the capital at the
/// back-row centre of the runtime `dims` grid — row 0 (the row furthest from
/// the player), `dims.center_col()`. On the default 5×4 this is `Pos(2, 0)`,
/// matching the legacy behavior; on a 2×2 grid it's `Pos(1, 0)` which
/// (importantly) is *not* the player's front-centre `(1, 1)`, so the boss has
/// somewhere to land. Returns `None` if the capital name isn't in the catalog,
/// OR if the grid has no back row (`dims.rows < 2`) — a capital encounter on a
/// 1-row board is geometrically degenerate (boss can't stand off the player).
pub(crate) fn capital_spawn_with_dims(
    capital_name: &str,
    lane: u8,
    catalog: &Catalog,
    dims: crate::grid::Dims,
) -> Option<ShipSpawn> {
    let known = catalog
        .capitals
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(capital_name));
    if !known {
        return None;
    }
    if dims.rows < 2 || dims.cols == 0 {
        return None; // no room for a stand-off boss on this shape
    }
    let _ = lane;
    let boss_pos = crate::grid::Pos::new(dims.center_col(), 0);
    Some(ShipSpawn {
        class_id: capital_name.to_string(),
        cell: boss_pos.to_index_in(dims),
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
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
        // smoke check.) #199b: each encounter has its own dims, so player
        // start + back-row test must use the encounter's dims, and the cap
        // (was lane-5 → 2) becomes per-shape `max_enemies_in(e.dims)`.
        for e in &sector.encounters[..sector.encounters.len() - 1] {
            assert!(!e.is_boss);
            assert!(!e.enemy_ships.is_empty());
            let ppos = player_start_pos_in(e.dims);
            for sp in &e.enemy_ships {
                assert_ne!(sp.pos, ppos, "enemies never spawn on the player cell");
                assert!(
                    sp.pos.row < e.dims.front_row(),
                    "enemies on the back rows of e.dims",
                );
                assert_eq!(
                    sp.facing,
                    enemy_spawn_facing(),
                    "enemies bow S toward the player"
                );
                assert_eq!(
                    sp.cell,
                    sp.pos.to_index_in(e.dims),
                    "invariant A: legacy cell tracks pos.to_index_in(e.dims)",
                );
                assert!(
                    pool.class_ids.contains(&sp.class_id),
                    "spawn {} drawn from the pool",
                    sp.class_id,
                );
            }
            // #199b per-shape cap: each rolled `e.dims` has its own
            // `max_enemies_in`. Lane-5 caller asks for 2 enemies, but on a
            // small rolled `dims` (e.g. 2×2) the cap is 1 — assert
            // `len <= min(lane_count, cap)`.
            let lane_count = encounter_enemy_count(cat.sectors[1].lane);
            let cap = max_enemies_in(e.dims);
            assert!(
                !e.enemy_ships.is_empty() && e.enemy_ships.len() <= lane_count.min(cap),
                "{} enemies on {:?} (lane_count {lane_count}, cap {cap})",
                e.enemy_ships.len(),
                e.dims,
            );
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
                ..Default::default()
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
        // Lane sizes feed `encounter_enemy_count` (Ion Reefs is lane 7 → 3),
        // but per-encounter shape cap (`max_enemies_in(e.dims)`, #199b) is
        // the binding ceiling. Assert `non_boss.len()` is in
        // `1..=min(3, max_enemies_in(e.dims))` so the test holds whether a
        // 5x4 was rolled (cap 4 → 3 enemies) or a smaller shape was (cap 1-3).
        let ion = &campaign[2];
        let non_boss = ion.encounters.iter().find(|e| !e.is_boss).unwrap();
        let lane_count = 3usize;
        let cap = max_enemies_in(non_boss.dims);
        let upper = lane_count.min(cap);
        assert!(
            !non_boss.enemy_ships.is_empty() && non_boss.enemy_ships.len() <= upper,
            "{} enemies on {:?} (lane_count {lane_count}, cap {cap}, upper {upper})",
            non_boss.enemy_ships.len(),
            non_boss.dims,
        );
    }

    /* =====================================================================
     * Variable-board (#199) coverage.
     *
     * Every test here exercises the dim-aware spawn surface
     * (`*_in(dims)` helpers + `build_encounter_board_with_dims`) across the
     * full pool of encounter shapes the random-size feature will roll from:
     *
     *   { 2x2, 3x2, 4x2, 5x2, 2x3, 3x3, 4x3, 5x3, 2x4, 3x4, 4x4, 5x4 }
     *
     * Property focus, not data: the design-critical invariants (player on
     * the board, enemies on the back rows, no collisions, at least one
     * dodge lane open) hold for every shape, not just the canonical 5x4.
     * ================================================================== */

    /// Every pool shape the variable-board feature is expected to support.
    /// Sourced from the lead's `#199` brief; kept here as the test source of
    /// truth so a shape change is a one-line edit.
    const POOL_SHAPES: &[(usize, usize)] = &[
        (2, 2),
        (3, 2),
        (4, 2),
        (5, 2),
        (2, 3),
        (3, 3),
        (4, 3),
        (5, 3),
        (2, 4),
        (3, 4),
        (4, 4),
        (5, 4),
    ];

    #[test]
    fn max_enemies_in_matches_the_design_table() {
        // CANONICAL TABLE (Bruce's ruling): each row is the *literal expected
        // output* of `max_enemies_in` on that shape, hard-coded here rather
        // than recomputed from the formula in-test. This pins the CONTRACT
        // (the public per-shape cap promise), not the implementation — a
        // future refactor that quietly changes the formula's output for any
        // shape is a visible break here. Mirrors the table on
        // `max_enemies_in` itself; if you tune one, tune the other.
        let cases: &[(usize, usize, usize)] = &[
            // (cols, rows, expected cap)
            (2, 2, 1),
            (3, 2, 2),
            (4, 2, 3),
            (5, 2, 4),
            (2, 3, 1),
            (3, 3, 2),
            (4, 3, 3),
            (5, 3, 4),
            (2, 4, 1),
            (3, 4, 2),
            (4, 4, 3),
            (5, 4, 4),
        ];
        for &(c, r, expected) in cases {
            let got = max_enemies_in(crate::grid::Dims::new(c, r));
            assert_eq!(
                got, expected,
                "{c}x{r}: canonical cap is {expected}, max_enemies_in returned {got}",
            );
        }
        // Sanity: 5x4 default keeps the legacy 4-enemy cap (behaviour-identical
        // — `Dims::default()` produces the same number as the 5x4 entry above).
        assert_eq!(
            max_enemies_in(crate::grid::Dims::default()),
            4,
            "default Dims preserves the legacy 4-enemy cap",
        );
        // Degenerate shapes return 0 (no back row, or zero-width grid).
        assert_eq!(max_enemies_in(crate::grid::Dims::new(0, 4)), 0);
        assert_eq!(max_enemies_in(crate::grid::Dims::new(4, 0)), 0);
        assert_eq!(
            max_enemies_in(crate::grid::Dims::new(4, 1)),
            0,
            "rows<2 has no back row",
        );
    }

    #[test]
    fn max_enemies_in_always_leaves_a_dodge_lane() {
        // For every legal shape (rows >= 2, cols >= 2): the cap must leave at
        // least one back-row column free for the player to dodge into.
        for &(c, r) in POOL_SHAPES {
            let cap = max_enemies_in(crate::grid::Dims::new(c, r));
            assert!(
                cap < c,
                "{c}x{r}: cap {cap} would saturate the back row's columns — no dodge lane left",
            );
        }
    }

    #[test]
    fn player_start_pos_in_default_matches_legacy_const_helper() {
        // Behaviour-identical lock: at default Dims the dim-aware variant must
        // produce the exact same Pos as the legacy const helper. A regression
        // here would silently shift every existing 5x4 encounter.
        assert_eq!(
            player_start_pos_in(crate::grid::Dims::default()),
            player_start_pos(),
        );
    }

    #[test]
    fn player_start_pos_in_lands_in_bounds_on_every_pool_shape() {
        for &(c, r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, r);
            let p = player_start_pos_in(d);
            assert!(
                p.in_bounds_in(d),
                "{c}x{r}: player start {p:?} off the grid"
            );
            // Front-centre rule: row is the front row, col is the centre col.
            assert_eq!(p.row, d.front_row(), "{c}x{r}: player not on front row");
            assert_eq!(p.col, d.center_col(), "{c}x{r}: player not on centre col");
        }
    }

    #[test]
    fn enemy_spawn_pos_in_default_matches_legacy_helper() {
        // Byte-equivalent on default Dims — the legacy `enemy_spawn_pos` is a
        // thin wrapper. Walks well past the cap so the None tail is checked.
        let d = crate::grid::Dims::default();
        for i in 0..(crate::grid::CELLS * 2) {
            assert_eq!(enemy_spawn_pos_in(i, d), enemy_spawn_pos(i), "slot {i}");
        }
    }

    #[test]
    fn enemy_spawn_pos_in_never_lands_on_the_player_or_off_grid() {
        // Across every pool shape, every Some(pos) is a back-row cell distinct
        // from the player's front-centre.
        for &(c, r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, r);
            let ppos = player_start_pos_in(d);
            for i in 0..(d.cell_count() * 2) {
                if let Some(pos) = enemy_spawn_pos_in(i, d) {
                    assert!(
                        pos.in_bounds_in(d),
                        "{c}x{r} slot {i}: {pos:?} off the grid",
                    );
                    assert!(
                        pos.row < d.front_row(),
                        "{c}x{r} slot {i}: {pos:?} on player's front row",
                    );
                    assert_ne!(pos, ppos, "{c}x{r} slot {i}: enemy on player cell");
                }
            }
        }
    }

    #[test]
    fn enemy_spawn_pos_in_back_row_slots_are_mutually_distinct() {
        // Each shape's back-row slots are a permutation: no two distinct
        // indices below the back-row capacity share a Pos.
        for &(c, r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, r);
            let back_slots = (d.rows - 1) * d.cols;
            let mut seen = std::collections::HashSet::new();
            for i in 0..back_slots {
                let pos = enemy_spawn_pos_in(i, d).expect("in-range slot is Some");
                assert!(seen.insert(pos), "{c}x{r}: slot {i} duplicated at {pos:?}");
            }
            assert_eq!(
                seen.len(),
                back_slots,
                "{c}x{r}: every back-row cell used exactly once",
            );
            assert_eq!(
                enemy_spawn_pos_in(back_slots, d),
                None,
                "{c}x{r}: first slot past the back rows is None",
            );
        }
    }

    #[test]
    fn placeholder_cell_to_pos_in_never_lands_on_the_player_row() {
        // For every pool shape, the placeholder re-key clamps to the back rows
        // — no 1-D cell, however large, ever lands on `front_row()`.
        for &(c, r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, r);
            for cell in 0..(d.cell_count() * 4) {
                let pos = placeholder_cell_to_pos_in(cell, d);
                assert!(pos.in_bounds_in(d), "{c}x{r} cell {cell}: {pos:?} off grid");
                assert!(
                    pos.row < d.front_row() || d.rows < 2,
                    "{c}x{r} cell {cell}: {pos:?} on the player's front row",
                );
            }
        }
    }

    #[test]
    fn back_row_column_order_in_default_matches_legacy_helper() {
        // Behaviour-identical lock at default size.
        assert_eq!(
            back_row_column_order_in(crate::grid::Dims::default()),
            back_row_column_order(),
        );
    }

    #[test]
    fn back_row_column_order_in_is_a_permutation_for_every_shape() {
        // Centre-out fan is a permutation of `0..cols` — every column hit
        // exactly once, no column dropped. Pinning this means a regression in
        // the centre-out walk (skipping a column on an even-width board) is a
        // visible break, not a silently-skewed encounter.
        for &(c, _r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, 4);
            let order = back_row_column_order_in(d);
            assert_eq!(order.len(), c, "{c}-wide: order length");
            let mut sorted = order.clone();
            sorted.sort_unstable();
            let expected: Vec<usize> = (0..c).collect();
            assert_eq!(sorted, expected, "{c}-wide: not a permutation of 0..cols");
            // First entry is always the centre column.
            assert_eq!(order[0], d.center_col(), "{c}-wide: first not centre");
        }
    }

    #[test]
    fn build_encounter_board_default_matches_with_dims_default() {
        // The 5x4 wrapper is exactly `build_encounter_board_with_dims(_, _,
        // Dims::default(), _)`. A non-empty encounter that exercises the
        // placement path (one enemy on the back row) MUST land at the same
        // cells through both APIs — the behaviour-identical lock on the
        // default callers.
        let enc = EncounterDef {
            id: "parity".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: crate::grid::Pos::new(1, 0).to_index(),
                pos: crate::grid::Pos::new(1, 0),
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                facing: enemy_spawn_facing(),
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
            ..Default::default()
        };
        let p1 = make_player(0, 10);
        let p2 = make_player(0, 10);
        let a = build_encounter_board(&enc, p1, |spawn| Some(fallback_ship_for_spawn(spawn)));
        let b = build_encounter_board_with_dims(&enc, p2, crate::grid::Dims::default(), |spawn| {
            Some(fallback_ship_for_spawn(spawn))
        });
        // Both boards have identical occupancy and shape.
        assert_eq!(a.cols, b.cols);
        assert_eq!(a.rows, b.rows);
        assert_eq!(a.size, b.size);
        assert_eq!(a.cells.len(), b.cells.len());
        assert_eq!(
            a.cells
                .iter()
                .map(|c| c.as_ref().map(|s| (s.faction, s.pos, s.cell)))
                .collect::<Vec<_>>(),
            b.cells
                .iter()
                .map(|c| c.as_ref().map(|s| (s.faction, s.pos, s.cell)))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn build_encounter_board_with_dims_2x2_spawns_one_enemy_off_the_player() {
        // The smallest shape: player at the front-centre of a 2x2, one back-row
        // cell remains in front of the player → exactly 1 enemy fits + must not
        // collide.
        let d = crate::grid::Dims::new(2, 2);
        let cap = max_enemies_in(d);
        assert_eq!(cap, 1, "2x2 enemy cap");
        let pos = enemy_spawn_pos_in(0, d).expect("at least one back-row slot exists on a 2x2");
        let enc = EncounterDef {
            id: "2x2".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: pos.to_index_in(d),
                pos,
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                facing: enemy_spawn_facing(),
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
            ..Default::default()
        };
        let board = build_encounter_board_with_dims(&enc, make_player(0, 10), d, |spawn| {
            Some(fallback_ship_for_spawn(spawn))
        });
        // Board shape carries through.
        assert_eq!((board.cols, board.rows), (2, 2));
        assert_eq!(board.cells.len(), 4);
        // Player at front-centre, NOT on the enemy cell.
        let ppos = player_start_pos_in(d);
        let player = board
            .ship_at(ppos)
            .expect("player on the front-centre cell");
        assert_eq!(player.faction, Faction::Player);
        assert_eq!(player.pos, ppos);
        assert_eq!(player.cell, ppos.to_index_in(d), "invariant A on a 2x2");
        // Enemy at its 2-D pos, slot==idx, distinct from the player's.
        assert_ne!(pos, ppos, "test setup: spawn pos != player");
        let enemy = board.ship_at(pos).expect("enemy on its back-row pos");
        assert_eq!(enemy.faction, Faction::Enemy);
        assert_eq!(enemy.pos, pos);
        assert_eq!(enemy.cell, pos.to_index_in(d), "invariant A: derived cell");
    }

    #[test]
    fn build_encounter_board_with_dims_3x3_full_squad_lands_distinct() {
        // 3x3: cap=2, fan two enemies across the back row.
        let d = crate::grid::Dims::new(3, 3);
        let cap = max_enemies_in(d);
        assert_eq!(cap, 2, "3x3 enemy cap");
        let spawns: Vec<ShipSpawn> = (0..cap)
            .map(|i| {
                let pos = enemy_spawn_pos_in(i, d).expect("in-range slot");
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: pos.to_index_in(d),
                    pos,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                }
            })
            .collect();
        let board = build_encounter_board_with_dims(
            &EncounterDef {
                id: "3x3".into(),
                enemy_ships: spawns.clone(),
                hazards: vec![],
                is_boss: false,
                ..Default::default()
            },
            make_player(0, 10),
            d,
            |spawn| Some(fallback_ship_for_spawn(spawn)),
        );
        assert_eq!((board.cols, board.rows), (3, 3));
        assert_eq!(board.cells.len(), 9);
        let ppos = player_start_pos_in(d);
        assert_eq!(board.ship_at(ppos).unwrap().faction, Faction::Player);
        // Every spawn placed at its pos, distinct from the player.
        let mut enemy_cells = std::collections::HashSet::new();
        for sp in &spawns {
            let s = board
                .ship_at(sp.pos)
                .unwrap_or_else(|| panic!("enemy missing on 3x3 at {:?}", sp.pos));
            assert_eq!(s.faction, Faction::Enemy);
            assert_eq!(s.pos, sp.pos);
            assert_eq!(s.cell, sp.pos.to_index_in(d));
            assert_ne!(sp.pos, ppos);
            assert!(enemy_cells.insert(sp.pos));
        }
    }

    #[test]
    fn build_encounter_board_with_dims_4x4_full_squad_lands_distinct() {
        // 4x4: cap=3, three enemies spread across the back rows.
        let d = crate::grid::Dims::new(4, 4);
        let cap = max_enemies_in(d);
        assert_eq!(cap, 3, "4x4 enemy cap");
        let spawns: Vec<ShipSpawn> = (0..cap)
            .map(|i| {
                let pos = enemy_spawn_pos_in(i, d).expect("in-range slot");
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: pos.to_index_in(d),
                    pos,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                }
            })
            .collect();
        let board = build_encounter_board_with_dims(
            &EncounterDef {
                id: "4x4".into(),
                enemy_ships: spawns.clone(),
                hazards: vec![],
                is_boss: false,
                ..Default::default()
            },
            make_player(0, 10),
            d,
            |spawn| Some(fallback_ship_for_spawn(spawn)),
        );
        assert_eq!((board.cols, board.rows), (4, 4));
        assert_eq!(board.cells.len(), 16);
        let ppos = player_start_pos_in(d);
        assert_eq!(board.ship_at(ppos).unwrap().faction, Faction::Player);
        // Player has at least one empty back-row cell to dodge to next turn.
        let mut free_back_cells = 0usize;
        for row in 0..d.front_row() {
            for col in 0..d.cols {
                let p = crate::grid::Pos::new(col, row);
                if board.ship_at(p).is_none() {
                    free_back_cells += 1;
                }
            }
        }
        assert!(
            free_back_cells >= 1,
            "4x4 with cap-{cap} squad should leave >=1 free back-row dodge cell, got {free_back_cells}",
        );
        // Every spawn placed, invariant A.
        for sp in &spawns {
            let s = board.ship_at(sp.pos).unwrap();
            assert_eq!(s.cell, sp.pos.to_index_in(d));
        }
    }

    #[test]
    fn build_encounter_board_with_dims_drops_out_of_grid_spawns() {
        // A spawn whose Pos is in-bounds on 5x4 but OFF the 3x3 grid is
        // silently dropped (defensive). The on-grid spawns still place. This
        // is the contract that lets a 5x4-shaped encounter author run on a
        // narrower roll without panicking.
        let d = crate::grid::Dims::new(3, 3);
        let on_grid = crate::grid::Pos::new(0, 0);
        let off_grid = crate::grid::Pos::new(4, 0); // valid on 5x4, off 3x3
        let enc = EncounterDef {
            id: "off-grid".into(),
            enemy_ships: vec![
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: on_grid.to_index_in(d),
                    pos: on_grid,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                },
                ShipSpawn {
                    class_id: "skiff".into(),
                    cell: 0,
                    pos: off_grid,
                    orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                    facing: enemy_spawn_facing(),
                    hp_override: None,
                },
            ],
            hazards: vec![],
            is_boss: false,
            ..Default::default()
        };
        let board = build_encounter_board_with_dims(&enc, make_player(0, 10), d, |spawn| {
            Some(fallback_ship_for_spawn(spawn))
        });
        // On-grid enemy landed.
        assert_eq!(board.ship_at(on_grid).unwrap().faction, Faction::Enemy);
        // Total enemies = 1 (the off-grid spawn was dropped).
        let n_enemies = board
            .cells
            .iter()
            .flatten()
            .filter(|s| s.faction == Faction::Enemy)
            .count();
        assert_eq!(n_enemies, 1, "off-grid spawn was dropped, not crashed");
    }

    #[test]
    fn build_encounter_board_with_dims_collapses_stale_ship_spawn_cell() {
        // Reviewer rec (#199): `ship.cell` is DERIVED from `pos.to_index_in(
        // dims)` — the spawn's `cell` field is NOT consulted for placement, so
        // a stale `cell` (e.g. a 5x4-keyed index on a 3x3 board) doesn't
        // corrupt the ship's slot. The ship lands at `pos`, and its `cell`
        // field matches that pos under the runtime dims.
        let d = crate::grid::Dims::new(3, 3);
        let pos = crate::grid::Pos::new(1, 1);
        let stale_cell = 19usize; // a valid 5x4 index that's nonsense on 3x3
        let enc = EncounterDef {
            id: "stale".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "skiff".into(),
                cell: stale_cell,
                pos,
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                facing: enemy_spawn_facing(),
                hp_override: None,
            }],
            hazards: vec![],
            is_boss: false,
            ..Default::default()
        };
        let board = build_encounter_board_with_dims(&enc, make_player(0, 10), d, |spawn| {
            Some(fallback_ship_for_spawn(spawn))
        });
        let enemy = board.ship_at(pos).expect("enemy at its 2-D pos");
        assert_eq!(
            enemy.cell,
            pos.to_index_in(d),
            "ship.cell derived from pos.to_index_in(dims), not from spawn.cell",
        );
        // The slot-by-stale-cell holds no ship — confirms placement ignored
        // the stale field.
        assert!(
            stale_cell >= board.cells.len()
                || board.cells[stale_cell.min(board.cells.len() - 1)]
                    .as_ref()
                    .is_none_or(|s| s.pos == pos)
        );
    }

    #[test]
    fn capital_spawn_with_dims_lands_at_back_centre_on_every_shape() {
        // Capital spawn: back-row centre of the runtime grid. Verified across
        // every pool shape with rows >= 2 (rows<2 returns None — no room for a
        // stand-off boss).
        let cat = gen_catalog();
        let cap_name = cat
            .capitals
            .first()
            .map(|c| c.name.clone())
            .expect("test catalog has at least one capital");
        for &(c, r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, r);
            let sp = capital_spawn_with_dims(&cap_name, 5, &cat, d)
                .unwrap_or_else(|| panic!("{c}x{r}: capital_spawn_with_dims returned None"));
            assert_eq!(sp.pos.col, d.center_col(), "{c}x{r}: capital not centred");
            assert_eq!(sp.pos.row, 0, "{c}x{r}: capital not on back row 0");
            assert_eq!(sp.cell, sp.pos.to_index_in(d), "{c}x{r}: cell derived");
            // Most importantly, NEVER the player's cell.
            assert_ne!(sp.pos, player_start_pos_in(d), "{c}x{r}: capital on player");
        }
        // Degenerate: 1-row board has no stand-off, no capital.
        assert!(
            capital_spawn_with_dims(&cap_name, 5, &cat, crate::grid::Dims::new(3, 1)).is_none()
        );
    }

    #[test]
    fn sample_encounter_spawns_with_dims_caps_at_max_enemies_in() {
        // A pool with one class + a high requested count must produce exactly
        // `max_enemies_in(dims)` spawns on every shape — the cap takes
        // precedence over the caller's `count`.
        let pool = SpawnPool {
            class_ids: vec!["skiff".to_string()],
        };
        for &(c, r) in POOL_SHAPES {
            let d = crate::grid::Dims::new(c, r);
            let spawns = sample_encounter_spawns_with_dims(&pool, 5, 999, 0xCAFE_BABE, d);
            assert_eq!(
                spawns.len(),
                max_enemies_in(d),
                "{c}x{r}: spawn count = max_enemies_in",
            );
            // Every spawn in-bounds and not on the player's cell.
            let ppos = player_start_pos_in(d);
            let mut seen = std::collections::HashSet::new();
            for sp in &spawns {
                assert!(sp.pos.in_bounds_in(d), "{c}x{r}: spawn off grid");
                assert_ne!(sp.pos, ppos, "{c}x{r}: spawn on player");
                assert!(seen.insert(sp.pos), "{c}x{r}: spawn collision");
            }
        }
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
