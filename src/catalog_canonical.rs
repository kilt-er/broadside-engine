//! Canonical (design-doc) → strict (engine-runtime) catalog transformer.
//!
//! The analysis HTML's "Copy JSON" button emits a **flat** catalog shape:
//!
//! ```json
//! { "id": "pulse_laser", "heat": 1, "cd": 0, "band": "close",
//!   "pattern": "BEAM", "arc": "forward", "freeplay": false,
//!   "effects": ["DAMAGE"] }
//! ```
//!
//! The engine's strict types (mirroring TS) expect a **nested** shape:
//!
//! ```json
//! { "id": "pulse_laser",
//!   "cost":   { "heat": 1, "cooldownMax": 0, "advancesTurn": true },
//!   "targeting": { "pattern": "BEAM", "optimalBand": "close",
//!                  "band": ["close"], "requiresArc": "forward",
//!                  "facingRelative": true, "hitsAll": false },
//!   "effects": [{ "kind": "DAMAGE", "amount": 3 }] }
//! ```
//!
//! This module walks the loose `serde_json::Value` tree, infers the
//! missing fields with documented defaults, and produces a `Catalog`.
//! `Catalog` round-trips with the strict shape on the other side — the
//! resolver is unchanged.
//!
//! ## Inference rules (and where they live)
//!
//! - **`heat → cost.heat`**, **`cd → cost.cooldownMax`**,
//!   **`!freeplay → cost.advancesTurn`** are direct renames.
//! - **`band → targeting.optimalBand`**, **`pattern → targeting.pattern`**,
//!   **`arc → targeting.requiresArc`**: same — direct renames.
//! - **`targeting.band: [optimal_band]`**: the canonical shape doesn't
//!   list "all allowed bands" — only the optimal one — so the strict
//!   `band` array is seeded with the optimal band as its single member.
//!   That's the conservative read: weapons fire only at their optimal
//!   range under canonical data. Tune later when a real "allowed bands"
//!   field lands in the export.
//! - **`hits_all`**: SPINAL_LINE pierces only if explicitly true; default
//!   `false`.
//! - **`facingRelative: true`** for every transformed action (the
//!   canonical engine treats targeting as facing-relative by default).
//!
//! Bare-string effects are inflated using the action's archetype as a
//! hint — see [`inflate_effect`].
//!
//! ## Why `load_from_path` auto-detects, not a separate function
//!
//! Tester's `catalog_smoke` test hardcodes `load_from_path` and the demo
//! bin will too. Rather than make every caller pick between
//! `load_strict_from_path` / `load_canonical_from_path`, we let the
//! existing `load_from_path` try strict first and fall back to canonical
//! on parse error. The canonical export is the only loose shape we
//! expect today; future formats can extend the dispatch.

use serde_json::{Map, Value};

use crate::types::Catalog;

/* =========================================================================
 * Entry point — transform a raw JSON Value into a Catalog.
 * ====================================================================== */

/// Walk the loose canonical shape and produce a strict [`Catalog`].
/// Returns the same `serde_json::Error` kind any failed conversion would
/// surface — caller distinguishes IO from parse errors at the
/// [`crate::catalog::LoadError`] layer.
pub fn from_canonical_value(root: Value) -> Result<Catalog, serde_json::Error> {
    let mut obj = match root {
        Value::Object(o) => o,
        other => return serde_json::from_value(other), // not the canonical shape; let serde say so
    };

    // Transform each section in place. Sections that already match the
    // strict shape (statuses, enemies, mods, patrols, capitals, sectors,
    // fieldkit, commendations) pass through untouched. Sections with
    // structural drift (actions, subsystems, classes) get rewritten.
    if let Some(Value::Array(actions)) = obj.remove("actions") {
        let transformed: Vec<Value> = actions
            .into_iter()
            .filter_map(|v| transform_action(v).ok())
            .collect();
        obj.insert("actions".into(), Value::Array(transformed));
    }
    if let Some(Value::Array(subsystems)) = obj.remove("subsystems") {
        let transformed: Vec<Value> = subsystems
            .into_iter()
            .map(transform_subsystem)
            .collect();
        obj.insert("subsystems".into(), Value::Array(transformed));
    }
    if let Some(Value::Array(classes)) = obj.remove("classes") {
        let transformed: Vec<Value> = classes.into_iter().map(transform_class).collect();
        obj.insert("classes".into(), Value::Array(transformed));
    }

    // The canonical shape carries `archetypes` and `bays` top-level keys
    // (UI metadata). Strict `Catalog` doesn't have these — serde will
    // ignore unknown fields (no `deny_unknown_fields`) so we don't need
    // to strip them, but leaving the comment here for the next reader.

    let rebuilt = Value::Object(obj);
    serde_json::from_value(rebuilt)
}

/* =========================================================================
 * Per-section transformers.
 * ====================================================================== */

/// Flat action → strict action. Returns the failure value as-is on
/// missing required fields so the outer `filter_map` can skip silently
/// — losing an action is better than failing the whole load.
fn transform_action(v: Value) -> Result<Value, &'static str> {
    let Value::Object(mut a) = v else {
        return Err("not an object");
    };

    // Required fields. If any are missing we punt (the canonical export
    // always has them).
    let heat = a.remove("heat").and_then(|v| v.as_i64()).ok_or("missing heat")?;
    let cd = a.remove("cd").and_then(|v| v.as_i64()).ok_or("missing cd")?;
    let freeplay = a.remove("freeplay").and_then(|v| v.as_bool()).unwrap_or(false);
    let band = a.remove("band").and_then(|v| v.as_str().map(String::from))
        .ok_or("missing band")?;
    let pattern = a.remove("pattern").and_then(|v| v.as_str().map(String::from))
        .ok_or("missing pattern")?;
    let arc = a.remove("arc"); // may be null / absent for arc-less actions
    let hits_all = a.remove("hits_all")
        .or_else(|| a.remove("hitsAll"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Inflate `effects: [<string>]` into Effect records.
    let archetype = a.get("archetype").and_then(|v| v.as_str()).unwrap_or("beam").to_string();
    let action_id = a.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let loose_effects = a.remove("effects").unwrap_or(Value::Array(vec![]));
    let new_effects = match loose_effects {
        Value::Array(items) => items
            .into_iter()
            .map(|e| inflate_effect(e, &archetype, heat as i32, &action_id))
            .collect(),
        other => vec![other],
    };
    a.insert("effects".into(), Value::Array(new_effects));

    // Build cost { heat, cooldownMax, advancesTurn }.
    let mut cost = Map::new();
    cost.insert("heat".into(), Value::from(heat));
    cost.insert("cooldownMax".into(), Value::from(cd));
    cost.insert("advancesTurn".into(), Value::from(!freeplay));
    a.insert("cost".into(), Value::Object(cost));

    // Build targeting { pattern, band: [band], optimalBand: band,
    //                   requiresArc, facingRelative, hitsAll }.
    let mut targeting = Map::new();
    targeting.insert("pattern".into(), Value::from(pattern));
    // The canonical shape gives only the optimal band; seed `band` with
    // a single entry so the resolver's "is this band allowed?" gate
    // still works without growing the catalog format. A later format
    // upgrade can widen to a real allowed-bands list.
    targeting.insert("band".into(), Value::Array(vec![Value::from(band.clone())]));
    targeting.insert("optimalBand".into(), Value::from(band));
    targeting.insert(
        "requiresArc".into(),
        match arc {
            Some(Value::String(s)) => Value::String(s),
            _ => Value::Null,
        },
    );
    targeting.insert("facingRelative".into(), Value::from(true));
    targeting.insert("hitsAll".into(), Value::from(hits_all));
    a.insert("targeting".into(), Value::Object(targeting));

    // Drop UI / canonical-only fields the strict shape doesn't care
    // about. `desc` stays nowhere on Action (it's UI-side) — strip.
    a.remove("desc");

    Ok(Value::Object(a))
}

/// Flat subsystem → strict subsystem. The two real drifts are
/// `unlock → unlockSalvage` (rename) and the missing `level` field
/// (canonical defaults to 1).
fn transform_subsystem(v: Value) -> Value {
    let Value::Object(mut s) = v else { return v };

    // unlock → unlockSalvage (rename, value preserved).
    if let Some(unlock) = s.remove("unlock") {
        s.insert("unlockSalvage".into(), unlock);
    }
    // level: 1 default (canonical omits, strict requires).
    s.entry("level").or_insert(Value::from(1));
    // Drop UI-only fields.
    s.remove("desc");

    Value::Object(s)
}

/// Flat class → strict class. Only drift is `affinity: "bow-on"` →
/// `"bowOn"` (the hyphen-form in canonical, camelCase form in strict).
/// Other affinity values (`"flexible"`, `"broadside"`) pass through.
fn transform_class(v: Value) -> Value {
    let Value::Object(mut c) = v else { return v };

    if let Some(Value::String(s)) = c.remove("affinity") {
        let camel = match s.as_str() {
            "bow-on" => "bowOn",
            other => other,
        };
        c.insert("affinity".into(), Value::String(camel.into()));
    }

    Value::Object(c)
}

/* =========================================================================
 * Effect inflation.
 *
 * The canonical export emits `effects: ["DAMAGE", "APPLY_STATUS"]` —
 * bare strings. The strict Effect enum is internally tagged on `kind`
 * with required per-variant fields. We inflate each bare string into a
 * minimal record using the action's archetype + heat as hints.
 *
 * Numeric inference is conservative — better to under-tune and let
 * playtesting flag the values than to bake "magic numbers from the
 * desc" into the loader. The lead's brief said "sensible defaults you
 * document in comments"; that's the spirit.
 * ====================================================================== */

/// Inflate one loose effect entry (string or already-an-object) into a
/// strict Effect-tagged JSON object.
fn inflate_effect(v: Value, archetype: &str, heat: i32, action_id: &str) -> Value {
    let kind = match &v {
        // Already in strict form — pass through.
        Value::Object(_) => return v,
        Value::String(s) => s.clone(),
        _ => return v, // unknown — let serde fail downstream
    };

    let mut m = Map::new();
    m.insert("kind".into(), Value::from(kind.clone()));

    match kind.as_str() {
        "DAMAGE" => {
            // Inference: heat is a tempo cost, and the canonical
            // archetypes split into roughly two tiers — direct-damage
            // (beam, broadside) and indirect (ordnance, control,
            // displacement, movement, defensive). Beam/broadside scale
            // with heat; the indirect archetypes contribute either a
            // small fixed amount (control/displacement) or zero (the
            // ordnance variant carries damage on the projectile, not
            // the launcher action).
            let amount = match archetype {
                "beam" | "broadside" => heat + 2,
                "ordnance" => 0,
                "displacement" | "control" => 2,
                _ => heat.max(1),
            };
            m.insert("amount".into(), Value::from(amount));
            // bandFalloff omitted — strict shape's `None` default
            // (apply falloff). Reviewer audit #67's pipeline order
            // means modifier subsystems still take effect.
        }
        "APPLY_STATUS" => {
            // Status defaults by archetype. The canonical descs aren't
            // machine-readable so we pick the analysis-doc-aligned
            // common case per family.
            let status = match archetype {
                // Heavy Torpedo's APPLY_STATUS in the canonical export is
                // the "systems-offline pulse" mentioned in its desc.
                "ordnance" => "systemsOffline",
                // Grav Snare-style: control / displacement applies the
                // freeze / lock half of the combo.
                "displacement" | "control" => "systemsOffline",
                // Beam mods most often apply hull-breach (Incendiary).
                _ => "hullBreach",
            };
            m.insert("status".into(), Value::String(status.into()));
            m.insert("duration".into(), Value::from(3));
        }
        "DISPLACE_TARGET" => {
            // mode chosen by action id keyword. Default push.
            let id_lower = action_id.to_lowercase();
            let mode = if id_lower.contains("tractor") && id_lower.contains("toss") {
                "swap"
            } else if id_lower.contains("tractor") || id_lower.contains("pull") {
                "pull"
            } else if id_lower.contains("repulsor")
                || id_lower.contains("push")
                || id_lower.contains("toss")
                || id_lower.contains("snare")
            {
                "push"
            } else {
                "push"
            };
            m.insert("mode".into(), Value::String(mode.into()));
            m.insert("distance".into(), Value::from(2));
        }
        "DISPLACE_SELF" => {
            m.insert("mode".into(), Value::String("THRUST".into()));
            m.insert("distance".into(), Value::from(1));
            // direction omitted -> serde defaults to None on the strict
            // shape (preserves orientation-relative semantics).
        }
        "REORIENT" => {
            m.insert("to".into(), Value::String("flip".into()));
        }
        "SPAWN_ORDNANCE" => {
            // Projectile kind defaults to the action id. The runtime
            // `Content::spawn_projectile` impl looks it up; if the demo
            // content doesn't know the kind it falls back to a 0-damage
            // dummy (see input.rs::DemoContent::spawn_projectile).
            m.insert("projectile".into(), Value::String(action_id.into()));
        }
        "VENT_HEAT" => {
            // Heat 3 is the canonical Vent value (the demo's
            // synthetic_vent uses the same number).
            m.insert("amount".into(), Value::from(3));
            m.insert("rechargeCooldowns".into(), Value::from(false));
        }
        "DEPLOY" => {
            // Default to mine; the `mine_layer` action is the canonical
            // example. Drone-deploying actions get re-inferred by their
            // id keyword.
            let hazard = if action_id.to_lowercase().contains("drone") {
                "drone"
            } else {
                "mine"
            };
            m.insert("hazard".into(), Value::String(hazard.into()));
        }
        "BOARD" => {
            // Note defaults to the action id so card-style BOARD effects
            // dispatch through `Content::apply_board_effect(action_id)`.
            m.insert("note".into(), Value::String(action_id.into()));
        }
        _ => {
            // Unknown effect kind — preserve as a strict Effect-tagged
            // record with just `kind`. serde will fail downstream if
            // the engine doesn't know it, surfacing the drift clearly.
        }
    }

    Value::Object(m)
}

/* =========================================================================
 * Tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Catalog, ClassAffinity, Effect};

    /// Single-action canonical shape parses cleanly into a strict Catalog.
    #[test]
    fn canonical_pulse_laser_parses() {
        let json = serde_json::json!({
            "meta": {
                "schema": "broadside-action-verb v1",
                "lane": [5, 7, 9],
                "newAxes": ["rangeBands","orientation","ordnanceEntities","heat"],
                "bands": ["pointBlank","close","mid","long","extreme"],
            },
            "actions": [{
                "id": "pulse_laser",
                "name": "Pulse Laser",
                "archetype": "beam",
                "heat": 1, "cd": 0,
                "band": "close", "pattern": "BEAM", "arc": "forward",
                "freeplay": false,
                "effects": ["DAMAGE"],
                "desc": "Baseline shot — first target in the forward arc.",
            }],
            "mods": [],
            "subsystems": [],
            "statuses": [],
            "enemies": [],
            "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        assert_eq!(cat.actions.len(), 1);
        let a = &cat.actions[0];
        assert_eq!(a.id, "pulse_laser");
        assert_eq!(a.cost.heat, 1);
        assert_eq!(a.cost.cooldown_max, 0);
        assert!(a.cost.advances_turn, "freeplay=false -> advancesTurn=true");
        assert_eq!(a.effects.len(), 1);
        match &a.effects[0] {
            Effect::DAMAGE { amount, .. } => {
                // beam + heat 1 -> amount 3 per the inflate_effect rule
                assert_eq!(*amount, 3);
            }
            _ => panic!("expected DAMAGE"),
        }
    }

    #[test]
    fn freeplay_true_yields_advances_turn_false() {
        let json = serde_json::json!({
            "meta": {
                "schema": "x", "lane": [5],
                "newAxes": [], "bands": ["close"],
            },
            "actions": [{
                "id": "vent", "name": "Vent", "archetype": "defensive",
                "heat": 0, "cd": 0,
                "band": "pointBlank", "pattern": "SELF",
                "freeplay": true,
                "effects": ["VENT_HEAT"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        assert!(!cat.actions[0].cost.advances_turn,
            "freeplay=true should map to advancesTurn=false");
    }

    #[test]
    fn class_affinity_bow_on_renames_to_camelcase() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
            "classes": [{
                "id": "ronin", "name": "Destroyer Ronin",
                "affinity": "bow-on",
                "set1": [], "set2": [],
                "signature": "Ram",
                "desc": "Bow-on bruiser.",
            }],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        assert_eq!(cat.classes[0].affinity, ClassAffinity::BowOn);
    }

    #[test]
    fn subsystem_unlock_renames_to_unlock_salvage_and_level_defaults() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [], "mods": [],
            "subsystems": [{
                "id": "marksman", "name": "Marksman",
                "bay": "gunnery", "hook": "passive",
                "cost": 15, "unlock": null, "maxLevel": 3,
                "desc": "+1 damage at long band.",
            }],
            "statuses": [], "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        assert_eq!(cat.subsystems[0].id, "marksman");
        assert_eq!(cat.subsystems[0].unlock_salvage, None);
        assert_eq!(cat.subsystems[0].level, 1, "missing level defaults to 1");
        assert_eq!(cat.subsystems[0].max_level, 3);
    }

    #[test]
    fn ordnance_apply_status_infers_systems_offline() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [{
                "id": "heavy_torpedo", "name": "Heavy Torpedo",
                "archetype": "ordnance",
                "heat": 5, "cd": 7, "band": "mid",
                "pattern": "ORDNANCE", "arc": "forward",
                "freeplay": false,
                "effects": ["SPAWN_ORDNANCE", "APPLY_STATUS"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        let effects = &cat.actions[0].effects;
        assert_eq!(effects.len(), 2);
        match &effects[1] {
            Effect::APPLY_STATUS { status, duration } => {
                assert_eq!(*status, crate::types::StatusKind::SystemsOffline);
                assert_eq!(*duration, 3);
            }
            other => panic!("expected APPLY_STATUS, got {other:?}"),
        }
    }

    #[test]
    fn tractor_beam_displace_infers_pull() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [{
                "id": "tractor_beam", "name": "Tractor Beam",
                "archetype": "displacement",
                "heat": 1, "cd": 4, "band": "mid",
                "pattern": "BEAM", "arc": "forward",
                "freeplay": false,
                "effects": ["DISPLACE_TARGET", "DAMAGE"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        let effects = &cat.actions[0].effects;
        match &effects[0] {
            Effect::DISPLACE_TARGET { mode, .. } => {
                assert_eq!(*mode, crate::types::DisplaceMode::Pull);
            }
            other => panic!("expected DISPLACE_TARGET, got {other:?}"),
        }
    }

    #[test]
    fn repulsor_displace_infers_push() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [{
                "id": "repulsor", "name": "Repulsor",
                "archetype": "displacement",
                "heat": 1, "cd": 5, "band": "close",
                "pattern": "BEAM", "arc": "forward",
                "freeplay": false,
                "effects": ["DISPLACE_TARGET"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        match &cat.actions[0].effects[0] {
            Effect::DISPLACE_TARGET { mode, .. } => {
                assert_eq!(*mode, crate::types::DisplaceMode::Push);
            }
            other => panic!("expected push, got {other:?}"),
        }
    }

    #[test]
    fn tractor_toss_infers_swap() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [{
                "id": "tractor_toss", "name": "Tractor Toss",
                "archetype": "displacement",
                "heat": 2, "cd": 7, "band": "close",
                "pattern": "BROADSIDE", "arc": "broadsideArc",
                "freeplay": true,
                "effects": ["DISPLACE_TARGET"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        match &cat.actions[0].effects[0] {
            Effect::DISPLACE_TARGET { mode, .. } => {
                assert_eq!(*mode, crate::types::DisplaceMode::Swap);
            }
            other => panic!("expected swap, got {other:?}"),
        }
    }

    /// The targeting `band` array seeds with the single optimal band —
    /// the resolver's "in-band?" gate will then accept only that band.
    /// Future format extensions can widen the array; this is the
    /// conservative starting point.
    #[test]
    fn targeting_band_seeds_with_optimal_only() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [{
                "id": "x", "name": "X", "archetype": "beam",
                "heat": 1, "cd": 0, "band": "mid",
                "pattern": "BEAM", "arc": "forward",
                "freeplay": false,
                "effects": ["DAMAGE"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        let t = &cat.actions[0].targeting;
        assert_eq!(t.optimal_band, crate::types::RangeBand::Mid);
        assert_eq!(t.band, vec![crate::types::RangeBand::Mid]);
    }

    /// Already-strict effects (objects) pass through inflation
    /// untouched. Future hybrid catalogs (some loose, some strict) are
    /// handled.
    #[test]
    fn already_strict_effect_passes_through() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [{
                "id": "x", "name": "X", "archetype": "beam",
                "heat": 1, "cd": 0, "band": "close",
                "pattern": "BEAM", "arc": "forward",
                "freeplay": false,
                "effects": [{ "kind": "DAMAGE", "amount": 99 }],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        match &cat.actions[0].effects[0] {
            Effect::DAMAGE { amount, .. } => assert_eq!(*amount, 99),
            _ => panic!("expected DAMAGE"),
        }
    }
}
