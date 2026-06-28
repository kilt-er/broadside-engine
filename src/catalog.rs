//! Content catalog loader. Reads `assets/broadside.catalog.json` (the JSON
//! exported by the analysis doc's "Copy JSON" button) into the typed surface
//! declared in [`crate::types`].
//!
//! The JSON shape is canonical — see `broadside-engine/engine/types.ts` and
//! the `Catalog` definition there. This module only exposes thin convenience
//! wrappers; type-driven deserialization handles the rest.
//!
//! # Catalog → Content seam
//!
//! [`crate::types::Catalog::actions`] is a `Vec<Action>` to match the JSON
//! wire shape (a JSON array). The TS resolver's `Content.actions` is
//! `Record<string, Action>`, i.e. an `id -> Action` map. The resolver crate
//! should build a `HashMap<String, Action>` from `catalog.actions` **once
//! at startup** and own that as its `Content` struct — not re-scan the
//! `Vec` per fire, and not duplicate-store the map on `Catalog`. This
//! file is the load shape; the resolver crate defines the runtime indexing.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::types::{
    Arc as TArc, Catalog, EnemyDef, Faction, Mount, ShieldFace, ShieldProfile, Ship, ShipSpawn,
    Trait,
};

/// Errors loading a catalog from disk.
///
/// Marked `#[non_exhaustive]` so downstream `match`es get a warning when a
/// new variant (e.g. a `BadSchema(String)` validation failure) is added.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error reading catalog: {e}"),
            Self::Parse(e) => write!(f, "parse error in catalog json: {e}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
        }
    }
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for LoadError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e)
    }
}

/// Read the catalog JSON file at `path` and decode it into a [`Catalog`].
///
/// Tries the strict shape first (the engine's native format); on parse
/// failure falls back to the canonical / design-doc-export shape via
/// [`crate::catalog_canonical::from_canonical_value`]. Either shape lands
/// in the same `Catalog` struct after this call — the resolver doesn't
/// see the format split.
///
/// The auto-detect path means tester's `tests/catalog_smoke.rs` and the
/// demo bin's startup loader both accept the canonical JSON bruce
/// exported from the analysis doc without per-caller plumbing.
pub fn load_from_path(path: impl AsRef<Path>) -> Result<Catalog, LoadError> {
    let bytes = fs::read(path)?;
    load_from_bytes(&bytes)
}

/// Decode an in-memory JSON byte slice (useful for embedded test fixtures).
/// Same auto-detect dispatch as [`load_from_path`].
pub fn load_from_bytes(bytes: &[u8]) -> Result<Catalog, LoadError> {
    // Strict shape first — fast path for engine-emitted JSON.
    let mut catalog = if let Ok(c) = serde_json::from_slice::<Catalog>(bytes) {
        c
    } else {
        // Fallback: parse to a loose Value and run the canonical transformer.
        let v: serde_json::Value = serde_json::from_slice(bytes)?;
        crate::catalog_canonical::from_canonical_value(v)?
    };
    // v2 (#28): guarantee every action has 2-D Range bands, on BOTH load paths.
    // The canonical transformer derives them inline; a STRICT-shape catalog that
    // carries only the 1-D `band` would otherwise deserialize `range_band` EMPTY
    // (its serde default) → `resolve_targeting_2d`'s `in_band` over an empty set
    // is ALWAYS false → no weapon fires in 2-D. This post-parse pass derives the
    // 2-D bands from the 1-D `band` for any action still missing them, so the
    // silent-inert bug can't recur regardless of which shape was loaded.
    normalize_2d_bands(&mut catalog);
    // #177: cap every enemy weapon's per-shot DAMAGE to ENEMY_DAMAGE_CAP (except
    // very-long-cooldown alpha abilities) so the player isn't killed in ~2 hits.
    // Runs on BOTH load shapes here (the single chokepoint), so every caller gets
    // the capped enemies + capped action defs for free via `install_catalog_actions`.
    cap_enemy_weapon_damage(&mut catalog);
    Ok(catalog)
}

/// Fill each action's 2-D [`crate::grid::Range`] bands from its 1-D
/// [`crate::types::RangeBand`] when they're absent (#28; widened #81). Idempotent:
/// actions that already carry a non-empty `range_band` (e.g. a future CONTRACT-era
/// catalog with explicit 2-D bands) are left untouched.
///
/// ## 1-D band → 2-D Range SET (#81 — the playable widening)
///
/// The catalog gives each weapon a SINGLE 1-D band (`"close"`, `"mid"`, …). The
/// original #28 derive mapped that to a single 2-D Range cell, so a weapon fired
/// in exactly ONE Chebyshev ring (a `close` beam only at distance 2, never
/// adjacent) — too narrow to actually play. #81 expands each 1-D band into a
/// CONTIGUOUS 2-D set that mirrors its firing intent, while preserving the
/// over-extension deadzone (decision #7):
///
/// | 1-D band     | 2-D set            | note                                    |
/// |--------------|--------------------|-----------------------------------------|
/// | `PointBlank` | `[Adjacent]`       | touching only                           |
/// | `Close`      | `[Adjacent, Near]` | fights from touching out to near        |
/// | `Mid`        | `[Near, Far]`      | reaches out, NOT point-blank            |
/// | `Long`       | `[Far]`            | long-only — DEADZONE (inert if closed)  |
/// | `Extreme`    | `[Far]`            | long-only — DEADZONE                     |
///
/// Each set is a SUPERSET of the original single cell (only rings ADDED, never
/// removed), so any test asserting a weapon fires at its old ring still holds.
/// `Long`/`Extreme` stay `Far`-only so a player who closes onto a long-range gun
/// still makes it inert — the #7 over-extension threat is unharmed.
///
/// `optimal_range` is the NEAREST ring of the resulting set (smallest Chebyshev),
/// the sensible "ideal engagement distance" for a telegraph; band-falloff is
/// absolute-by-distance now (R4) so `optimal_range` no longer drives damage.
///
/// MUST stay in lockstep with `catalog_canonical::derive_range_2d_set` (the other
/// load path) — both produce identical sets so strict + canonical catalogs agree.
fn normalize_2d_bands(catalog: &mut Catalog) {
    for action in &mut catalog.actions {
        let t = &mut action.targeting;
        if t.range_band.is_empty() {
            // Union the per-1-D-band sets (dedup-preserving), in nearest-first order.
            let mut seen = Vec::new();
            for &b in &t.band {
                for r in expand_band_2d(b) {
                    if !seen.contains(&r) {
                        seen.push(r);
                    }
                }
            }
            // Optimal: the nearest ring of the optimal band's expansion.
            t.optimal_range = expand_band_2d(t.optimal_band)
                .first()
                .copied()
                .unwrap_or(crate::grid::Range::Far);
            t.range_band = seen;
        }
    }
}

/// Expand one 1-D [`RangeBand`] into the contiguous 2-D [`crate::grid::Range`] set
/// it should fire across (#81), NEAREST-first. See [`normalize_2d_bands`] for the
/// table + rationale; kept in lockstep with
/// `catalog_canonical::derive_range_2d_set`.
pub(crate) fn expand_band_2d(b: crate::types::RangeBand) -> Vec<crate::grid::Range> {
    use crate::grid::Range::{Adjacent, Far, Near};
    use crate::types::RangeBand;
    match b {
        RangeBand::PointBlank => vec![Adjacent],
        RangeBand::Close => vec![Adjacent, Near],
        RangeBand::Mid => vec![Near, Far],
        RangeBand::Long | RangeBand::Extreme => vec![Far],
    }
}

/* =========================================================================
 * #177 — enemy per-shot damage cap (with a very-long-cooldown exception).
 *
 * Bruce ruling (refined): enemy weapons should deal AT MOST 1-2 damage PER SHOT,
 * EXCEPT very-long-cooldown abilities (a big telegraphed alpha hit may exceed
 * the cap). The player was dying in ~2 hits.
 *
 * The per-shot number a gun deals is its `Effect::DAMAGE` amount run through the
 * integer `band_falloff` (>=1) minus shield soak — falloff only ever REDUCES the
 * authored amount, so clamping the authored amount at <=2 caps the per-shot at
 * <=2. The DAMAGE amount lives on the SHARED catalog action (`pulse_laser` -> 3,
 * `beam_cannon` -> 4, `broadside_battery` -> 5 via the loader's heat-derived
 * `inflate_effect`), and those ids are ALSO the PLAYER's weapons — so the amount
 * can't be lowered on the shared action without nerfing the player.
 *
 * Fix: at load, give every NON-CAPITAL enemy a PRIVATE capped copy of any
 * direct-DAMAGE weapon whose amount exceeds the cap AND whose cooldown is below
 * the "very-long" threshold. The copy keeps the base action's name / archetype /
 * targeting / arc / cost (so the HUD + AI behave identically) and only differs in
 * `id` (`<base>__enemy`) and the DAMAGE amount (clamped to [`ENEMY_DAMAGE_CAP`]).
 * The enemy's weapon ref is rewritten to the capped id; the player never
 * references it, so the player's loadout is untouched.
 *
 * Exceptions (left at full damage, base id):
 * - **capitals / bosses** (Bruce ruling: bosses should hit harder; their threat
 *   is the long-cd specials anyway): any `enemies[]` entry whose id also appears
 *   in `catalog.capitals[]` (e.g. the Citadel `warlord`, which is both a regular-
 *   roster entry and a capital) is skipped entirely. The real bosses are anyway
 *   synthesized by `runs::boss_ship_for_spawn` (hand-built mounts, NOT
 *   `enemy_ship_from_catalog`), so they never flow through this pass — this guard
 *   just makes the "non-capital only" scope explicit + data-driven for the
 *   dual-listed entry.
 * - **very-long-cooldown weapons** (`cd >= `[`ENEMY_DAMAGE_CAP_CD_EXEMPT`]): the
 *   alpha-strike archetype (e.g. `railgun_broadside` cd 6, `particle_lance` cd 5)
 *   — a rare, telegraphed big hit is the intended counter-pressure, not the
 *   "dying to chip" problem. Standard low/mid-cd guns (`pulse_laser` cd 0,
 *   `beam_cannon` cd 3, `broadside_battery` cd 4) are capped.
 * - **ordnance** (e.g. Missile Salvo / Torpedo): carry their damage on the
 *   spawned projectile (`input::spawn_ordnance`), not the launcher action, so the
 *   launcher has no direct DAMAGE to cap; the projectile damage is a separate
 *   table (already low for the starter ordnance). Out of scope for this pass.
 * ====================================================================== */

/// The most damage a standard (non-exempt) enemy weapon may deal per shot (#177,
/// Bruce ruling: "1-2, max, per shot"). Bruce-tunable.
const ENEMY_DAMAGE_CAP: i32 = 2;

/// Cooldown (`Action.cost.cooldown_max`) at or above which a weapon is EXEMPT
/// from [`ENEMY_DAMAGE_CAP`] — a "very-long-cooldown ability" that may deal a
/// big telegraphed hit (#177 exception). Chosen so the alpha-strike guns
/// (`particle_lance` cd 5, `railgun_broadside` cd 6) keep full damage while the
/// sustained/standard guns (`pulse_laser` cd 0, `point_defense` cd 2,
/// `beam_cannon` cd 3, `broadside_battery`/`scatter_laser` cd 4) are capped.
/// Bruce-tunable.
const ENEMY_DAMAGE_CAP_CD_EXEMPT: i32 = 5;

/// Suffix marking a private, damage-capped weapon variant minted for an enemy
/// (#177). The variant keeps the base action's display name, so it must be
/// resolved by ID only — [`action_name_to_id`] excludes any id carrying this
/// suffix so the variant never wins a display-name lookup.
const ENEMY_CAPPED_SUFFIX: &str = "__enemy";

/// `true` if `id` is a private enemy-capped weapon variant ([`ENEMY_CAPPED_SUFFIX`]).
fn is_enemy_capped_id(id: &str) -> bool {
    id.ends_with(ENEMY_CAPPED_SUFFIX)
}

/// Give EVERY enemy private, damage-capped copies of their over-cap, non-exempt
/// direct-DAMAGE weapons (#177). See the module section comment above for the why
/// + the exception list.
///
/// For each NON-CAPITAL enemy weapon that resolves to a catalog action carrying a
/// direct `Effect::DAMAGE` whose amount exceeds [`ENEMY_DAMAGE_CAP`] AND whose
/// `cooldown_max` is below [`ENEMY_DAMAGE_CAP_CD_EXEMPT`]:
/// 1. ensure a capped clone exists in `catalog.actions` under id `<base>__enemy`
///    (idempotent — shared across enemies that mount the same base weapon), and
/// 2. rewrite the enemy's weapon ref to that capped id.
///
/// Capital-tier `enemies[]` entries (id present in `catalog.capitals[]`), weapons
/// that already deal `<= cap`, those carrying no direct DAMAGE (ordnance /
/// movement / defensive), and cooldown-exempt weapons are left pointing at the
/// base id: no capped copy is made, so the catalog isn't littered with no-op
/// variants.
fn cap_enemy_weapon_damage(catalog: &mut Catalog) {
    // Resolve weapon refs (display name OR id) to action ids up front, using an
    // immutable snapshot of the name->id map (built before we start pushing the
    // capped variants, which only add new ids and never shadow a base name).
    let name_to_id = action_name_to_id(catalog);

    // Capital ids (owned, so the borrow ends before the `&mut catalog.enemies`
    // loop). An `enemies[]` entry that is ALSO a capital (the dual-listed Citadel
    // warlord) is exempt — bosses hit harder (Bruce ruling).
    let capital_ids: std::collections::HashSet<String> =
        catalog.capitals.iter().map(|c| c.id.clone()).collect();

    // Collect the capped action defs to append after we finish reading + rewriting
    // `enemies` (can't push into `catalog.actions` while iterating the same
    // catalog immutably for the base-action lookup). Keyed by capped id so the
    // same base weapon shared by two enemies yields ONE variant.
    let mut capped_defs: HashMap<String, crate::types::Action> = HashMap::new();

    for def in &mut catalog.enemies {
        // Capitals/bosses keep full damage — their threat is intended.
        if capital_ids.contains(&def.id) {
            continue;
        }
        for weapon in &mut def.weapons {
            let Some(base_id) = resolve_weapon_id(weapon, &name_to_id) else {
                continue; // unresolved name — synthesis already logs + skips it
            };
            // Find the base action's direct DAMAGE (if any). Look in the catalog
            // first; if the base id isn't a catalog action (e.g. a player-only
            // DemoContent weapon), skip — the cap is a catalog-data step.
            let Some(base_action) = catalog.actions.iter().find(|a| a.id == base_id) else {
                continue;
            };
            let Some(base_dmg) = direct_damage_amount(base_action) else {
                continue; // no direct DAMAGE (ordnance / utility) — nothing to cap
            };
            if base_dmg <= ENEMY_DAMAGE_CAP {
                continue; // already within the cap — leave the ref on the base id
            }
            // Very-long-cooldown alpha weapons are EXEMPT: a big telegraphed hit
            // is intended, not the chip-death problem.
            if base_action.cost.cooldown_max >= ENEMY_DAMAGE_CAP_CD_EXEMPT {
                continue;
            }
            // Need a capped variant. Build it once (idempotent across enemies).
            let capped_id = format!("{base_id}{ENEMY_CAPPED_SUFFIX}");
            capped_defs.entry(capped_id.clone()).or_insert_with(|| {
                let mut a = base_action.clone();
                a.id.clone_from(&capped_id);
                cap_direct_damage(&mut a, ENEMY_DAMAGE_CAP);
                a
            });
            // Point the enemy's mount at the capped id (snake_case, so the later
            // `resolve_weapon_id` at synthesis passes it through unchanged).
            *weapon = capped_id;
        }
    }

    // Append the new capped actions (skip any id a catalog already defines, so a
    // future explicit `*__enemy` entry in the export wins over the synthesized
    // one). insert-if-absent semantics, mirroring `install_catalog_actions`.
    for (id, action) in capped_defs {
        if !catalog.actions.iter().any(|a| a.id == id) {
            catalog.actions.push(action);
        }
    }
}

/// The amount of an action's FIRST direct [`Effect::DAMAGE`], or `None` if the
/// action deals no direct damage (ordnance carries it on the projectile;
/// movement / defensive carry none). Mirrors the `action_damage` direct-DAMAGE
/// read used by the bin's tile readout.
fn direct_damage_amount(action: &crate::types::Action) -> Option<i32> {
    action.effects.iter().find_map(|e| match e {
        crate::types::Effect::DAMAGE { amount, .. } => Some(*amount),
        _ => None,
    })
}

/// Clamp every [`Effect::DAMAGE`] amount on `action` to at most `cap`, preserving
/// each effect's `band_falloff`. (An action has at most one DAMAGE today, but the
/// loop is total in case a future weapon stacks two.)
fn cap_direct_damage(action: &mut crate::types::Action, cap: i32) {
    for e in &mut action.effects {
        if let crate::types::Effect::DAMAGE { amount, .. } = e {
            *amount = (*amount).min(cap);
        }
    }
}

/* =========================================================================
 * Catalog-driven enemy synthesis.
 *
 * The demo bin previously fielded every spawn through a minimal fallback
 * (hull 3, one Forward pulse_laser, no traits). That made every enemy
 * mechanically identical and — crucially — left them trait-less, so the
 * AI's trait nudges in `decide_enemy_action` (Pursuit / BurnHard / Agile)
 * never fired. This module turns a `ShipSpawn` into a `Ship` using the
 * catalog's `enemies[]` definitions, so a `skiff` is hull 3 with a pulse
 * laser, a `monitor` is hull 5 with Pursuit, a `voidrunner` is hull 5 with
 * Agile + a Beam Cannon, etc. — each enemy behaves per its canonical
 * identity.
 * ====================================================================== */

/// Build an enemy [`Ship`] for `spawn` from the catalog's `enemies[]`
/// definition matching `spawn.class_id`. Returns `None` if no enemy with
/// that id exists in the catalog — the bin's spawn closure can then fall
/// back to its placeholder synthesizer (so a typo'd `class_id` degrades
/// gracefully instead of crashing the demo).
///
/// ## Field mapping (documented here because it isn't in `geometry.ts`)
///
/// - **hull / `max_hull`** ← `EnemyDef.hull`. (`hull5`, the Patrol-5 scaled
///   value, is left for a future patrol-tier scaler; the spawn's
///   `hp_override` still wins over both.)
/// - **mounts** ← `EnemyDef.weapons`. The canonical export lists weapons by
///   DISPLAY NAME ("Pulse Laser", "Tractor Beam"), the same drift the class
///   set1/set2 lists have (#82). Each name is resolved to an action id via a
///   display-name→id map built from `catalog.actions`; a weapon already in
///   id form passes through. The mount's `arc` is taken from the resolved
///   action's `targeting.requires_arc` (so a forward beam mounts Forward, a
///   broadside battery mounts `BroadsideArc`); arc-less actions (movement /
///   defensive — Afterburner, Brace, Blink Drive) default to `Forward` so
///   they still surface in the AI's fallback ladder. Unresolved weapon names
///   are skipped (logged) rather than mounted as a dangling id.
/// - **traits** ← `EnemyDef.traits`, mapped from canonical display strings
///   ("Burn-Hard", "Reactor Breach", "Pursuit", "Agile") to [`Trait`]
///   variants via [`trait_from_str`]. Unknown trait strings are skipped.
/// - **`shield_profile`** — a light enemy default (bow/port/starboard armour
///   1, stern 0): the soft-stern invariant the analysis doc rewards
///   flanking against. The boss (`boss_ship_for_spawn` in `runs.rs`) keeps
///   its own richer profile; this is the generic enemy shell.
/// - **`heat_max` 6**, heat 0, not locked out, empty queue/cooldowns/statuses.
///
/// `orientation` and `hp_override` come from the spawn (the encounter
/// author's call), matching [`crate::runs::build_encounter_board`]'s
/// existing contract.
///
/// Tier-1 entry point — see [`enemy_ship_from_catalog_at_tier`] for the
/// patrol-tier-aware form. Equivalent to passing `patrol_tier = 1`.
pub fn enemy_ship_from_catalog(catalog: &Catalog, spawn: &ShipSpawn) -> Option<Ship> {
    enemy_ship_from_catalog_at_tier(catalog, spawn, 1)
}

/// Patrol-tier-aware enemy synthesis. `patrol_tier` is the
/// [`crate::types::Sector::patrol_tier`] of the encounter being built.
///
/// **Difficulty-tier seam (not yet consumed).** The canonical data carries a
/// `hull5` field — the enemy's effective hull at Patrol 5+ — and the design
/// escalates difficulty by patrol tier. That mechanic has no consumer yet
/// (the demo escalates via enemy count + traits), so `patrol_tier` is
/// currently threaded but not used for stat math: [`select_hull`] returns the
/// base `hull` at every tier today. The parameter exists so that wiring
/// tier-scaling later (`patrol_tier ≥ 5 → hull5`, plus `patrol_tier →
/// Board.patrol` at the encounter-builder level) is a small change inside
/// [`select_hull`] rather than a signature-breaking retrofit across every
/// caller. Flagged by reviewer's audit as dormant; deliberately left as a
/// seam per the lead.
pub fn enemy_ship_from_catalog_at_tier(
    catalog: &Catalog,
    spawn: &ShipSpawn,
    patrol_tier: u8,
) -> Option<Ship> {
    let def = catalog.enemies.iter().find(|e| e.id == spawn.class_id)?;
    Some(ship_from_enemy_def_at_tier(
        catalog,
        def,
        spawn,
        patrol_tier,
    ))
}

/// Materialize a [`Ship`] from a specific [`EnemyDef`] + spawn at Patrol
/// tier 1. Split from [`enemy_ship_from_catalog`] so tests can drive a
/// hand-built `EnemyDef` without a whole catalog. See
/// [`ship_from_enemy_def_at_tier`] for the tier-aware form.
pub fn ship_from_enemy_def(catalog: &Catalog, def: &EnemyDef, spawn: &ShipSpawn) -> Ship {
    ship_from_enemy_def_at_tier(catalog, def, spawn, 1)
}

/// Tier-aware materialization. `patrol_tier` flows in through the
/// difficulty-tier seam documented on [`enemy_ship_from_catalog_at_tier`];
/// it currently only reaches [`select_hull`], which ignores it pending the
/// scheduled tier-scaling work.
pub fn ship_from_enemy_def_at_tier(
    catalog: &Catalog,
    def: &EnemyDef,
    spawn: &ShipSpawn,
    patrol_tier: u8,
) -> Ship {
    let name_to_id = action_name_to_id(catalog);

    let mounts: Vec<Mount> = def
        .weapons
        .iter()
        .enumerate()
        .filter_map(|(i, weapon)| {
            let action_id = resolve_weapon_id(weapon, &name_to_id);
            let Some(action_id) = action_id else {
                eprintln!(
                    "[catalog] enemy `{}`: weapon `{weapon}` has no matching action id; skipping mount",
                    def.id,
                );
                return None;
            };
            let arc = catalog
                .actions
                .iter()
                .find(|a| a.id == action_id)
                .and_then(|a| a.targeting.requires_arc)
                .unwrap_or(TArc::Forward);
            Some(Mount {
                id: format!("m{}", i + 1),
                arc,
                weapon: action_id,
            })
        })
        .collect();

    let traits: Vec<Trait> = def
        .traits
        .iter()
        .filter_map(|t| trait_from_str(t))
        .collect();

    let hull = spawn
        .hp_override
        .unwrap_or_else(|| select_hull(def, patrol_tier));

    Ship {
        id: format!("{}@{}", def.id, spawn.cell),
        faction: Faction::Enemy,
        cell: spawn.cell,
        // v2 (A3 EXPAND): carry the spawn's 2-D pos/facing through. Both default
        // until content's spawn-gen (C4) sets real grid coordinates.
        pos: spawn.pos,
        orientation: spawn.orientation,
        facing: spawn.facing,
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: enemy_shield_default(),
        mounts,
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits,
        klass: Some(def.id.clone()),
    }
}

/// Select an enemy's effective hull for a patrol tier — the difficulty-tier
/// seam. **Currently returns `def.hull` at every tier.** When tier-scaling is
/// scheduled, this is the single place to switch to `def.hull5` at
/// `patrol_tier >= 5` (the canonical Patrol-5 escalation); every caller
/// already threads `patrol_tier` here, so no signature changes are needed
/// then. Kept as a named fn (rather than inlining `def.hull`) precisely so
/// that future change is one edit.
const fn select_hull(def: &EnemyDef, _patrol_tier: u8) -> i32 {
    // TODO(broadside-content, difficulty-tier scaling): when wired, return
    //   if patrol_tier >= 5 { def.hull5 } else { def.hull }
    // and wire patrol_tier -> Board.patrol at the encounter-builder level.
    // Reviewer's audit flagged Sector.patrol_tier + hull5 as dormant; this
    // is the consumer-to-be.
    def.hull
}

/// Generic enemy shield: light all-round armour with a soft stern, the
/// flank-me-from-behind invariant the analysis doc rewards. Distinct from
/// [`crate::geometry::default_shield_profile`] (the player default) so a
/// future tuning pass can diverge enemy vs player armour without touching
/// the player path.
fn enemy_shield_default() -> ShieldProfile {
    // #103 Model A: `armour` is the per-face shield CAPACITY, `charge` the live
    // pool (start FULL). Light enemy default: cap 2 bow + flanks, soft stern 0
    // (the flank-me-from-behind invariant — a stern hit goes straight to hull).
    // Bruce-tunable.
    let face = |cap: i32| ShieldFace {
        armour: cap,
        charge: cap,
    };
    ShieldProfile {
        bow: face(2),
        stern: face(0),
        port: face(2),
        starboard: face(2),
    }
}

/// Build a lowercased display-name → action-id map from the catalog's
/// actions. Mirrors the lookup `catalog_canonical::transform_class` builds
/// for class set1/set2 normalization, but at synthesis time so it works
/// whether the catalog arrived via the canonical or the strict load path.
///
/// #177: the private `__enemy` damage-capped variants (added by
/// [`cap_enemy_weapon_damage`]) deliberately keep the BASE display name (so the
/// HUD still reads "Pulse Laser"), so they would COLLIDE on the name key with
/// their base action. They are resolved by ID only (the enemies' `weapons` were
/// rewritten to the capped id), never by display name, so they are excluded here
/// — otherwise the last-inserted variant would win the name key and silently
/// re-map a display-name lookup to the capped copy (and, on a second load pass,
/// double-suffix the variant).
fn action_name_to_id(catalog: &Catalog) -> HashMap<String, String> {
    catalog
        .actions
        .iter()
        .filter(|a| !is_enemy_capped_id(&a.id))
        .map(|a| (a.name.to_lowercase(), a.id.clone()))
        .collect()
}

/// Resolve a weapon reference to an action id. A reference already in
/// `snake_case` id form (no spaces, lowercase + digits + underscore) passes
/// through; otherwise it's looked up as a display name. `None` if neither
/// resolves.
fn resolve_weapon_id(weapon: &str, name_to_id: &HashMap<String, String>) -> Option<String> {
    if weapon
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Some(weapon.to_string());
    }
    name_to_id.get(&weapon.to_lowercase()).cloned()
}

/// Map a canonical trait display string to a [`Trait`] variant. The
/// canonical export uses Title-Case-with-hyphens ("Burn-Hard", "Reactor
/// Breach"); the strict enum is camelCase-serialized. This is the bridge.
/// Unknown strings return `None` (skipped at the call site) so a future
/// catalog trait doesn't crash the load — it just won't apply until a
/// matching arm is added here.
fn trait_from_str(s: &str) -> Option<Trait> {
    // Normalize to a spaceless lowercase key so "Burn-Hard", "burn_hard",
    // and "Burn Hard" all resolve.
    let key: String = s
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match key.as_str() {
        "pursuit" => Some(Trait::Pursuit),
        "agile" => Some(Trait::Agile),
        "reactorbreach" => Some(Trait::ReactorBreach),
        "burnhard" => Some(Trait::BurnHard),
        "anchored" => Some(Trait::Anchored),
        "eliteagile" => Some(Trait::EliteAgile),
        "eliteanchored" => Some(Trait::EliteAnchored),
        "twinlinked" => Some(Trait::TwinLinked),
        "reactiveshield" => Some(Trait::ReactiveShield),
        "voidtouched" => Some(Trait::Voidtouched),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimum-viable catalog that should round-trip: enough fields to exercise
    /// the trickier serde shapes (tagged Effect, Orientation, `RangeBand` casing).
    /// Empty arrays for the placeholder sections (`capitals` etc.) are accepted
    /// via `#[serde(default)]` on the Catalog struct.
    const MINIMAL_CATALOG_JSON: &str = r#"
{
  "meta": {
    "schema": "broadside.v0",
    "lane": [0,1,2,3,4,5,6],
    "newAxes": ["range","orientation","ordnance","heat"],
    "bands": ["pointBlank","close","mid","long","extreme"]
  },
  "actions": [
    {
      "id": "pulse_laser",
      "name": "Pulse Laser",
      "archetype": "beam",
      "cost": { "heat": 1, "cooldownMax": 0, "advancesTurn": true },
      "targeting": {
        "pattern": "BEAM",
        "band": ["pointBlank","close","mid"],
        "optimalBand": "close",
        "requiresArc": "forward",
        "facingRelative": true,
        "hitsAll": false
      },
      "effects": [
        { "kind": "DAMAGE", "amount": 4 }
      ]
    }
  ],
  "mods": [],
  "subsystems": [],
  "statuses": [],
  "enemies": [],
  "patrols": [{ "n": 1, "mod": "baseline" }]
}
"#;

    #[test]
    fn loads_minimal_catalog() {
        let cat = load_from_bytes(MINIMAL_CATALOG_JSON.as_bytes()).expect("parses");
        assert_eq!(cat.meta.schema, "broadside.v0");
        assert_eq!(cat.actions.len(), 1);
        assert_eq!(cat.actions[0].id, "pulse_laser");
    }

    #[test]
    fn placeholder_sections_default_to_empty_when_absent() {
        // Reviewer m4 + m3 follow-up: confirm `#[serde(default)]` on the
        // placeholder `unknown[]`-typed sections actually omits cleanly.
        // MINIMAL_CATALOG_JSON intentionally omits capitals/classes/fieldkit/
        // sectors/commendations entirely; if the defaults regress, this test
        // will fail with `missing field` parse errors.
        let cat = load_from_bytes(MINIMAL_CATALOG_JSON.as_bytes()).expect("parses");
        assert!(cat.capitals.is_empty());
        assert!(cat.classes.is_empty());
        assert!(cat.fieldkit.is_empty());
        assert!(cat.sectors.is_empty());
        assert!(cat.commendations.is_empty());
    }

    /* ---- catalog-driven enemy synthesis (#115) --------------------- */

    use crate::types::{LaneEnd, Orientation};

    fn spawn_at(class_id: &str, cell: usize) -> ShipSpawn {
        ShipSpawn {
            class_id: class_id.into(),
            cell,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Aft },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hp_override: None,
        }
    }

    #[test]
    fn trait_from_str_maps_canonical_display_strings() {
        // The exact display strings the canonical export uses.
        assert_eq!(trait_from_str("Burn-Hard"), Some(Trait::BurnHard));
        assert_eq!(trait_from_str("Reactor Breach"), Some(Trait::ReactorBreach));
        assert_eq!(trait_from_str("Pursuit"), Some(Trait::Pursuit));
        assert_eq!(trait_from_str("Agile"), Some(Trait::Agile));
        // Tolerant of casing / separator drift.
        assert_eq!(trait_from_str("burn_hard"), Some(Trait::BurnHard));
        assert_eq!(trait_from_str("reactorBreach"), Some(Trait::ReactorBreach));
        // Unknown -> None (skipped, not a crash).
        assert_eq!(trait_from_str("Phlogiston"), None);
    }

    #[test]
    fn resolve_weapon_id_handles_display_names_and_ids() {
        let mut m = HashMap::new();
        m.insert("pulse laser".to_string(), "pulse_laser".to_string());
        // Display name -> id.
        assert_eq!(
            resolve_weapon_id("Pulse Laser", &m),
            Some("pulse_laser".into())
        );
        // Already an id -> passes through.
        assert_eq!(
            resolve_weapon_id("pulse_laser", &m),
            Some("pulse_laser".into())
        );
        // Unknown display name -> None.
        assert_eq!(resolve_weapon_id("Ghost Gun", &m), None);
    }

    #[test]
    fn synthesized_enemy_carries_catalog_traits_and_mounts() {
        // The headline #115 guarantee: a spawned canonical enemy fields its
        // real traits + mounts (not the trait-less fallback). Use a small
        // hand-built catalog so the test is independent of the asset file.
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [
                { "id": "pulse_laser", "name": "Pulse Laser", "archetype": "beam",
                  "heat": 1, "cd": 0, "band": "close", "pattern": "BEAM",
                  "arc": "forward", "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "beam_cannon", "name": "Beam Cannon", "archetype": "beam",
                  "heat": 2, "cd": 3, "band": "mid", "pattern": "BEAM",
                  "arc": "forward", "freeplay": false, "effects": ["DAMAGE"] },
            ],
            "mods": [], "subsystems": [], "statuses": [],
            // Canonical enemies[] shape: weapons by display name, traits as
            // hyphenated display strings.
            "enemies": [
                { "id": "voidrunner", "name": "Voidrunner", "hull": 5, "hull5": 7,
                  "traits": ["Agile"], "sector": "Inner Keeps",
                  "weapons": ["Beam Cannon", "Pulse Laser"] },
            ],
            "patrols": [],
        });
        let cat: Catalog = crate::catalog_canonical::from_canonical_value(json).expect("parses");

        let ship = enemy_ship_from_catalog(&cat, &spawn_at("voidrunner", 4))
            .expect("voidrunner is in the catalog");

        // Identity + placement.
        assert_eq!(ship.faction, Faction::Enemy);
        assert_eq!(ship.cell, 4);
        assert_eq!(ship.klass.as_deref(), Some("voidrunner"));
        // Hull from the EnemyDef.
        assert_eq!(ship.hull, 5);
        assert_eq!(ship.max_hull, 5);
        // Trait mapped from "Agile" -> Trait::Agile (the AI nudge that was
        // dead under the fallback synthesizer).
        assert!(
            ship.traits.contains(&Trait::Agile),
            "synthesized voidrunner must carry its Agile trait, got {:?}",
            ship.traits
        );
        // Mounts: both weapons resolved display-name -> id, Forward arc from
        // the action's requiresArc.
        let weapons: Vec<&str> = ship.mounts.iter().map(|m| m.weapon.as_str()).collect();
        assert_eq!(
            weapons,
            vec!["beam_cannon", "pulse_laser"],
            "weapons normalized to action ids in listed order"
        );
        assert!(
            ship.mounts.iter().all(|m| m.arc == TArc::Forward),
            "both weapons are forward-arc per their action defs"
        );
    }

    #[test]
    fn synthesized_enemy_honors_hp_override() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [
                { "id": "pulse_laser", "name": "Pulse Laser", "archetype": "beam",
                  "heat": 1, "cd": 0, "band": "close", "pattern": "BEAM",
                  "arc": "forward", "freeplay": false, "effects": ["DAMAGE"] },
            ],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [
                { "id": "skiff", "name": "Skiff", "hull": 3, "hull5": 4,
                  "traits": [], "sector": "Drift Belt", "weapons": ["Pulse Laser"] },
            ],
            "patrols": [],
        });
        let cat: Catalog = crate::catalog_canonical::from_canonical_value(json).expect("parses");
        let mut spawn = spawn_at("skiff", 2);
        spawn.hp_override = Some(9);
        let ship = enemy_ship_from_catalog(&cat, &spawn).unwrap();
        assert_eq!(ship.hull, 9, "hp_override wins over EnemyDef.hull");
        assert_eq!(ship.max_hull, 9);
    }

    #[test]
    fn unknown_class_id_returns_none() {
        let cat = load_from_bytes(MINIMAL_CATALOG_JSON.as_bytes()).expect("parses");
        // MINIMAL_CATALOG_JSON has no enemies[]; any class_id misses.
        assert!(enemy_ship_from_catalog(&cat, &spawn_at("nonexistent", 1)).is_none());
    }

    #[test]
    fn patrol_tier_seam_threads_through_without_changing_hull_yet() {
        // The difficulty-tier seam: patrol_tier is accepted at every entry
        // point but `select_hull` ignores it today (hull5-at-patrol-5 is
        // dormant pending the scheduled tier-scaling work). This pins the
        // current "tier in, base hull out" behaviour so the eventual
        // tier-math change has a test to flip rather than a silent
        // behaviour drift. EnemyDef has hull 3 / hull5 6; at every tier we
        // still expect 3 until the seam is wired.
        let def = EnemyDef {
            id: "scaler".into(),
            name: "Scaler".into(),
            hull: 3,
            hull5: 6,
            traits: vec![],
            sector: "Test".into(),
            weapons: vec![],
        };
        let cat = load_from_bytes(MINIMAL_CATALOG_JSON.as_bytes()).expect("parses");
        for tier in [1u8, 3, 5, 9] {
            let ship = ship_from_enemy_def_at_tier(&cat, &def, &spawn_at("scaler", 2), tier);
            assert_eq!(
                ship.hull, 3,
                "tier {tier}: select_hull still returns base hull (seam present, math dormant)",
            );
        }
        // select_hull is the single switch point for the future change.
        assert_eq!(select_hull(&def, 1), 3);
        assert_eq!(
            select_hull(&def, 5),
            3,
            "dormant: will become hull5 (6) when wired"
        );
    }

    #[test]
    fn real_catalog_synthesizes_canonical_enemies_with_traits() {
        // End-to-end against the actual exported asset: confirms the real
        // enemies[] display-name + trait-string shapes synthesize correctly.
        // If the asset is absent (some CI checkouts), skip rather than fail.
        let path = std::path::Path::new("assets/broadside.catalog.json");
        if !path.exists() {
            eprintln!("[catalog test] asset absent; skipping real-catalog enemy synthesis check");
            return;
        }
        let cat = load_from_path(path).expect("real catalog loads");

        // Helper: an enemy mounts the weapon `base` (possibly via its #177 capped
        // `__enemy` variant — both resolve from the same display name).
        let mounts_weapon = |ship: &Ship, base: &str| {
            ship.mounts
                .iter()
                .any(|m| m.weapon == base || m.weapon == format!("{base}{ENEMY_CAPPED_SUFFIX}"))
        };

        // monitor: hull 5, Pursuit, Pulse Laser (capped to pulse_laser__enemy by #177).
        let monitor =
            enemy_ship_from_catalog(&cat, &spawn_at("monitor", 4)).expect("monitor in catalog");
        assert_eq!(monitor.hull, 5);
        assert!(
            monitor.traits.contains(&Trait::Pursuit),
            "monitor should carry Pursuit, got {:?}",
            monitor.traits
        );
        assert!(
            mounts_weapon(&monitor, "pulse_laser"),
            "monitor should mount pulse_laser (or its #177 __enemy cap), got {:?}",
            monitor.mounts.iter().map(|m| &m.weapon).collect::<Vec<_>>()
        );

        // voidrunner: Agile + beam_cannon (capped) + afterburner.
        let voidrunner = enemy_ship_from_catalog(&cat, &spawn_at("voidrunner", 6))
            .expect("voidrunner in catalog");
        assert!(
            voidrunner.traits.contains(&Trait::Agile),
            "voidrunner should carry Agile, got {:?}",
            voidrunner.traits
        );
        assert!(
            mounts_weapon(&voidrunner, "beam_cannon"),
            "voidrunner should mount beam_cannon (or its #177 __enemy cap)"
        );

        // lancer: Burn-Hard.
        let lancer =
            enemy_ship_from_catalog(&cat, &spawn_at("lancer", 3)).expect("lancer in catalog");
        assert!(
            lancer.traits.contains(&Trait::BurnHard),
            "lancer should carry BurnHard, got {:?}",
            lancer.traits
        );
    }

    /// #28/#81 GUARANTEE: every TARGETING (firing) weapon in the REAL exported
    /// catalog has a NON-EMPTY 2-D `range_band` after load — so no catalog weapon
    /// is silently inert in 2-D (`resolve_targeting_2d`'s `in_band` over an empty
    /// set is always false). This is the regression that catches a future catalog
    /// re-export reverting to the bare 1-D `band` (the empty-`range_band` failure
    /// the bin hit). SELF-pattern actions (movement/defensive — thrusters, slip,
    /// brace, vent) don't target board cells, so an empty band there is harmless
    /// and excluded. Skips if the asset is absent (some CI checkouts).
    #[test]
    fn real_catalog_every_firing_weapon_has_a_2d_band() {
        use crate::types::TargetingPattern;
        let path = std::path::Path::new("assets/broadside.catalog.json");
        if !path.exists() {
            eprintln!("[catalog test] asset absent; skipping 2-D band coverage check");
            return;
        }
        let cat = load_from_path(path).expect("real catalog loads");

        // Patterns that actually pick board cells (and thus need a band to fire).
        // SELF / DEPLOYED_CELL / ORDNANCE don't gate on the target's band the same
        // way (SELF hits the actor; DEPLOYED_CELL/ORDNANCE place/spawn ahead), but
        // the derive populates them anyway — we assert the firing patterns here.
        let firing = |p: TargetingPattern| {
            matches!(
                p,
                TargetingPattern::BEAM
                    | TargetingPattern::POINT_BLANK
                    | TargetingPattern::BROADSIDE
                    | TargetingPattern::SPINAL_LINE
                    | TargetingPattern::BLAST
            )
        };
        let mut checked = 0;
        for a in &cat.actions {
            if firing(a.targeting.pattern) {
                assert!(
                    !a.targeting.range_band.is_empty(),
                    "firing weapon `{}` ({:?}) has an EMPTY 2-D range_band — it would never fire in 2-D",
                    a.id, a.targeting.pattern,
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 5,
            "expected several firing weapons in the catalog, checked {checked}"
        );

        // Spot-check the #81 widened sets on representative weapons.
        use crate::grid::Range;
        let band_of = |id: &str| {
            cat.actions
                .iter()
                .find(|a| a.id == id)
                .unwrap_or_else(|| panic!("`{id}` in catalog"))
                .targeting
                .range_band
                .clone()
        };
        // close beam fires touching AND near (was: near-only).
        assert_eq!(
            band_of("pulse_laser"),
            vec![Range::Adjacent, Range::Near],
            "#81: a `close` weapon fires Adjacent+Near"
        );
        // mid beam reaches Near AND Far, never point-blank.
        assert_eq!(
            band_of("beam_cannon"),
            vec![Range::Near, Range::Far],
            "#81: a `mid` weapon fires Near+Far"
        );
        // long broadside stays Far-only (the over-extension deadzone, #7).
        assert_eq!(
            band_of("railgun_broadside"),
            vec![Range::Far],
            "#81: a `long` weapon is Far-only (deadzone preserved)"
        );
    }

    /* ---- #177: enemy per-shot damage cap (with cd exemption) ------------ */

    /// A canonical-shape catalog with one enemy mounting standard low/mid-cd guns
    /// (gunboat: Beam Cannon cd3/dmg4 + Broadside Battery cd4/dmg5) and one
    /// mounting a very-long-cd alpha gun PLUS a cheap gun (monitor: Pulse Laser
    /// cd0/dmg3 + Railgun Broadside cd6/dmg6). Loaded via `load_from_bytes` so the
    /// post-parse `cap_enemy_weapon_damage` pass runs (the embedded
    /// `from_canonical_value` tests above deliberately do NOT run it).
    fn cap_test_catalog_bytes() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [
                { "id": "pulse_laser", "name": "Pulse Laser", "archetype": "beam",
                  "heat": 1, "cd": 0, "band": "close", "pattern": "BEAM",
                  "arc": "forward", "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "beam_cannon", "name": "Beam Cannon", "archetype": "beam",
                  "heat": 2, "cd": 3, "band": "mid", "pattern": "BEAM",
                  "arc": "forward", "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "broadside_battery", "name": "Broadside Battery", "archetype": "broadside",
                  "heat": 3, "cd": 4, "band": "close", "pattern": "BROADSIDE",
                  "arc": "broadsideArc", "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "railgun_broadside", "name": "Railgun Broadside", "archetype": "broadside",
                  "heat": 4, "cd": 6, "band": "long", "pattern": "BROADSIDE",
                  "arc": "broadsideArc", "freeplay": false, "effects": ["DAMAGE"] },
            ],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [
                { "id": "gunboat", "name": "Gunboat", "hull": 4, "hull5": 5,
                  "traits": [], "sector": "Ion Reefs",
                  "weapons": ["Beam Cannon", "Broadside Battery"] },
                { "id": "monitor", "name": "Monitor", "hull": 5, "hull5": 7,
                  "traits": ["Pursuit"], "sector": "Spindle Port",
                  "weapons": ["Pulse Laser", "Railgun Broadside"] },
                // Dual-listed: a regular-roster entry whose id is ALSO a capital
                // (mirrors the real Citadel "warlord"). Must be EXEMPT from the cap.
                { "id": "warlord", "name": "Citadel Warlord", "hull": 14, "hull5": 16,
                  "traits": ["Reactor Breach"], "sector": "Inner Keeps",
                  "weapons": ["Beam Cannon"] },
            ],
            "capitals": [
                { "id": "warlord", "name": "The Warlord", "sector": "Inner Keeps",
                  "corrupt": true, "sP1": 5, "sP7": 11 },
            ],
            "patrols": [],
        }))
        .expect("catalog json serializes")
    }

    /// The direct DAMAGE of the action a synthesized mount resolves to, in the
    /// loaded `cat`. `None` if the mount's weapon id isn't a catalog action or
    /// the action has no direct DAMAGE.
    fn mount_damage(cat: &Catalog, mount_weapon_id: &str) -> Option<i32> {
        cat.actions
            .iter()
            .find(|a| a.id == mount_weapon_id)
            .and_then(direct_damage_amount)
    }

    #[test]
    fn enemy_standard_weapons_capped_at_two() {
        // Standard low/mid-cd enemy guns are capped per shot (#177): gunboat's
        // Beam Cannon (cd3) 4->2 and Broadside Battery (cd4) 5->2, monitor's Pulse
        // Laser (cd0) 3->2. Each capped mount points at the private `__enemy`
        // variant (base name/arc preserved).
        let cat = load_from_bytes(&cap_test_catalog_bytes()).expect("catalog loads");

        for (enemy, cell) in [("gunboat", 2usize), ("monitor", 4)] {
            let ship = enemy_ship_from_catalog(&cat, &spawn_at(enemy, cell))
                .unwrap_or_else(|| panic!("{enemy} in catalog"));
            for m in &ship.mounts {
                // Only the capped (non-exempt, over-cap) mounts carry the suffix;
                // the exempt railgun keeps its base id and is checked separately.
                if m.weapon.ends_with(ENEMY_CAPPED_SUFFIX) {
                    let dmg = mount_damage(&cat, &m.weapon).unwrap_or_else(|| {
                        panic!("{enemy} mount `{}` is a DAMAGE action", m.weapon)
                    });
                    assert!(
                        dmg <= ENEMY_DAMAGE_CAP,
                        "{enemy} weapon `{}` must deal <= {ENEMY_DAMAGE_CAP}, got {dmg}",
                        m.weapon,
                    );
                }
            }
        }

        // Concretely: every standard gun resolved to its capped variant at <= 2.
        assert_eq!(mount_damage(&cat, "beam_cannon__enemy"), Some(2));
        assert_eq!(mount_damage(&cat, "broadside_battery__enemy"), Some(2));
        assert_eq!(mount_damage(&cat, "pulse_laser__enemy"), Some(2));
    }

    #[test]
    fn enemy_very_long_cooldown_weapon_is_exempt() {
        // The cd-exemption (#177): monitor's Railgun Broadside (cd 6 >=
        // ENEMY_DAMAGE_CAP_CD_EXEMPT) keeps FULL damage (heat 4 -> 6) and its BASE
        // id — a big telegraphed alpha hit is intended, not the chip-death problem.
        let cat = load_from_bytes(&cap_test_catalog_bytes()).expect("catalog loads");
        let monitor =
            enemy_ship_from_catalog(&cat, &spawn_at("monitor", 4)).expect("monitor in catalog");

        let rail = monitor
            .mounts
            .iter()
            .find(|m| m.weapon == "railgun_broadside")
            .expect("monitor mounts railgun_broadside at its BASE id (cd-exempt, not capped)");
        assert_eq!(
            mount_damage(&cat, &rail.weapon),
            Some(6),
            "a very-long-cooldown weapon keeps full damage (heat 4 -> 6)",
        );
        // No capped variant was minted for the exempt weapon.
        assert!(
            cat.actions
                .iter()
                .all(|a| a.id != format!("railgun_broadside{ENEMY_CAPPED_SUFFIX}")),
            "no __enemy variant should exist for the cd-exempt railgun",
        );
    }

    #[test]
    fn capital_enemy_weapons_are_exempt() {
        // Bruce ruling (#177): capitals/bosses keep full damage. The dual-listed
        // `warlord` (in both enemies[] and capitals[]) mounts Beam Cannon (cd3,
        // dmg4 — a STANDARD gun that WOULD be capped on a regular enemy), but as a
        // capital it stays at full 4 and its BASE id (no `__enemy` variant).
        let cat = load_from_bytes(&cap_test_catalog_bytes()).expect("catalog loads");
        let warlord =
            enemy_ship_from_catalog(&cat, &spawn_at("warlord", 2)).expect("warlord in catalog");

        let bc = warlord
            .mounts
            .iter()
            .find(|m| m.weapon == "beam_cannon")
            .expect("capital warlord keeps beam_cannon at its BASE id (not capped)");
        assert_eq!(
            mount_damage(&cat, &bc.weapon),
            Some(4),
            "a capital's standard gun keeps full damage (beam_cannon 4, uncapped)",
        );
    }

    #[test]
    fn capping_enemy_does_not_touch_the_shared_base_weapon() {
        // The base `broadside_battery` / `beam_cannon` / `pulse_laser` actions (the
        // PLAYER's weapons share these ids) must keep their full damage — the cap
        // only adds private `__enemy` variants. This is the "do NOT touch player
        // weapons" guarantee made structural.
        let cat = load_from_bytes(&cap_test_catalog_bytes()).expect("catalog loads");
        assert_eq!(
            mount_damage(&cat, "broadside_battery"),
            Some(5),
            "base broadside_battery (player weapon) unchanged at 5",
        );
        assert_eq!(
            mount_damage(&cat, "beam_cannon"),
            Some(4),
            "base beam_cannon (player weapon) unchanged at 4",
        );
        assert_eq!(
            mount_damage(&cat, "pulse_laser"),
            Some(3),
            "base pulse_laser (player weapon) unchanged at 3",
        );
    }

    #[test]
    fn capped_enemy_variant_keeps_base_name_and_arc() {
        // The HUD reads the action's `name` + the mount's `arc`; the capped variant
        // must mirror the base so the enemy panel still says "Broadside Battery"
        // and the broadside still bears on its flanks.
        let cat = load_from_bytes(&cap_test_catalog_bytes()).expect("catalog loads");
        let base = cat
            .actions
            .iter()
            .find(|a| a.id == "broadside_battery")
            .expect("base present");
        let capped = cat
            .actions
            .iter()
            .find(|a| a.id == format!("broadside_battery{ENEMY_CAPPED_SUFFIX}"))
            .expect("capped variant present");
        assert_eq!(capped.name, base.name, "capped keeps the display name");
        assert_eq!(
            capped.archetype, base.archetype,
            "capped keeps the archetype"
        );
        assert_eq!(
            capped.targeting.requires_arc, base.targeting.requires_arc,
            "capped keeps the arc (still a broadside)",
        );
        assert_eq!(
            capped.targeting.range_band, base.targeting.range_band,
            "capped keeps the firing bands",
        );
        assert_eq!(capped.cost, base.cost, "capped keeps heat/cooldown cost");
    }
}
