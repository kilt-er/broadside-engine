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
            LoadError::Io(e) => write!(f, "io error reading catalog: {e}"),
            LoadError::Parse(e) => write!(f, "parse error in catalog json: {e}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io(e) => Some(e),
            LoadError::Parse(e) => Some(e),
        }
    }
}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self { LoadError::Io(e) }
}
impl From<serde_json::Error> for LoadError {
    fn from(e: serde_json::Error) -> Self { LoadError::Parse(e) }
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
    if let Ok(c) = serde_json::from_slice::<Catalog>(bytes) {
        return Ok(c);
    }
    // Fallback: parse to a loose Value and run the canonical transformer.
    let v: serde_json::Value = serde_json::from_slice(bytes)?;
    Ok(crate::catalog_canonical::from_canonical_value(v)?)
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
/// back to its placeholder synthesizer (so a typo'd class_id degrades
/// gracefully instead of crashing the demo).
///
/// ## Field mapping (documented here because it isn't in `geometry.ts`)
///
/// - **hull / max_hull** ← `EnemyDef.hull`. (`hull5`, the Patrol-5 scaled
///   value, is left for a future patrol-tier scaler; the spawn's
///   `hp_override` still wins over both.)
/// - **mounts** ← `EnemyDef.weapons`. The canonical export lists weapons by
///   DISPLAY NAME ("Pulse Laser", "Tractor Beam"), the same drift the class
///   set1/set2 lists have (#82). Each name is resolved to an action id via a
///   display-name→id map built from `catalog.actions`; a weapon already in
///   id form passes through. The mount's `arc` is taken from the resolved
///   action's `targeting.requires_arc` (so a forward beam mounts Forward, a
///   broadside battery mounts BroadsideArc); arc-less actions (movement /
///   defensive — Afterburner, Brace, Blink Drive) default to `Forward` so
///   they still surface in the AI's fallback ladder. Unresolved weapon names
///   are skipped (logged) rather than mounted as a dangling id.
/// - **traits** ← `EnemyDef.traits`, mapped from canonical display strings
///   ("Burn-Hard", "Reactor Breach", "Pursuit", "Agile") to [`Trait`]
///   variants via [`trait_from_str`]. Unknown trait strings are skipped.
/// - **shield_profile** — a light enemy default (bow/port/starboard armour
///   1, stern 0): the soft-stern invariant the analysis doc rewards
///   flanking against. The boss (`boss_ship_for_spawn` in `runs.rs`) keeps
///   its own richer profile; this is the generic enemy shell.
/// - **heat_max 6**, heat 0, not locked out, empty queue/cooldowns/statuses.
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
    Some(ship_from_enemy_def_at_tier(catalog, def, spawn, patrol_tier))
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

    let hull = spawn.hp_override.unwrap_or_else(|| select_hull(def, patrol_tier));

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
fn select_hull(def: &EnemyDef, _patrol_tier: u8) -> i32 {
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
    ShieldProfile {
        bow: ShieldFace { armour: 1, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 1, charge: 0 },
        starboard: ShieldFace { armour: 1, charge: 0 },
    }
}

/// Build a lowercased display-name → action-id map from the catalog's
/// actions. Mirrors the lookup `catalog_canonical::transform_class` builds
/// for class set1/set2 normalization, but at synthesis time so it works
/// whether the catalog arrived via the canonical or the strict load path.
fn action_name_to_id(catalog: &Catalog) -> HashMap<String, String> {
    catalog
        .actions
        .iter()
        .map(|a| (a.name.to_lowercase(), a.id.clone()))
        .collect()
}

/// Resolve a weapon reference to an action id. A reference already in
/// snake_case id form (no spaces, lowercase + digits + underscore) passes
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
        .filter(|c| c.is_ascii_alphanumeric())
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
    /// the trickier serde shapes (tagged Effect, Orientation, RangeBand casing).
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
        assert_eq!(resolve_weapon_id("Pulse Laser", &m), Some("pulse_laser".into()));
        // Already an id -> passes through.
        assert_eq!(resolve_weapon_id("pulse_laser", &m), Some("pulse_laser".into()));
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
        assert!(ship.traits.contains(&Trait::Agile),
            "synthesized voidrunner must carry its Agile trait, got {:?}", ship.traits);
        // Mounts: both weapons resolved display-name -> id, Forward arc from
        // the action's requiresArc.
        let weapons: Vec<&str> = ship.mounts.iter().map(|m| m.weapon.as_str()).collect();
        assert_eq!(weapons, vec!["beam_cannon", "pulse_laser"],
            "weapons normalized to action ids in listed order");
        assert!(ship.mounts.iter().all(|m| m.arc == TArc::Forward),
            "both weapons are forward-arc per their action defs");
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
        assert_eq!(select_hull(&def, 5), 3, "dormant: will become hull5 (6) when wired");
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

        // monitor: hull 5, Pursuit, Pulse Laser.
        let monitor = enemy_ship_from_catalog(&cat, &spawn_at("monitor", 4))
            .expect("monitor in catalog");
        assert_eq!(monitor.hull, 5);
        assert!(monitor.traits.contains(&Trait::Pursuit),
            "monitor should carry Pursuit, got {:?}", monitor.traits);
        assert!(monitor.mounts.iter().any(|m| m.weapon == "pulse_laser"),
            "monitor should mount pulse_laser, got {:?}",
            monitor.mounts.iter().map(|m| &m.weapon).collect::<Vec<_>>());

        // voidrunner: Agile + beam_cannon + afterburner.
        let voidrunner = enemy_ship_from_catalog(&cat, &spawn_at("voidrunner", 6))
            .expect("voidrunner in catalog");
        assert!(voidrunner.traits.contains(&Trait::Agile),
            "voidrunner should carry Agile, got {:?}", voidrunner.traits);
        assert!(voidrunner.mounts.iter().any(|m| m.weapon == "beam_cannon"),
            "voidrunner should mount beam_cannon");

        // lancer: Burn-Hard.
        let lancer = enemy_ship_from_catalog(&cat, &spawn_at("lancer", 3))
            .expect("lancer in catalog");
        assert!(lancer.traits.contains(&Trait::BurnHard),
            "lancer should carry BurnHard, got {:?}", lancer.traits);
    }
}
