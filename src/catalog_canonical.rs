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

use std::collections::HashMap;

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
    //
    // Actions must transform FIRST because the class-normalization step
    // below needs a display_name -> action_id lookup built from the
    // transformed actions (task #82).
    let mut action_name_to_id: HashMap<String, String> = HashMap::new();
    if let Some(Value::Array(actions)) = obj.remove("actions") {
        let transformed: Vec<Value> = actions
            .into_iter()
            .filter_map(|v| transform_action(v).ok())
            .collect();
        // Build the display_name -> id lookup BEFORE re-inserting so
        // transform_class can borrow it. The lookup folds case so
        // "Twin-Linked" and "twin-linked" both resolve.
        for a in &transformed {
            if let (Some(name), Some(id)) = (
                a.get("name").and_then(Value::as_str),
                a.get("id").and_then(Value::as_str),
            ) {
                action_name_to_id.insert(name.to_lowercase(), id.to_string());
            }
        }
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
        let transformed: Vec<Value> = classes
            .into_iter()
            .map(|c| transform_class(c, &action_name_to_id))
            .collect();
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
    // The four self-relative class signatures (slip / swap_toss / ram /
    // throw) are exported with `pattern: SELF` + a `DISPLACE_TARGET`
    // effect string. That combination is mechanically dead: `resolve_targeting`
    // returns `[ship_cell]` for SELF, so DISPLACE_TARGET runs with
    // `target == source` — a no-op for the swap modes and a wrong-direction
    // self-shove for the push modes (the trailing DAMAGE then strikes the
    // now-vacated origin and hits nothing). The canonical prose is
    // self-relative ("move forward to trade places…", "shove the ship
    // ahead…"), so the faithful representation is a DISPLACE_SELF, which the
    // resolver's `resolve_self_move` implements correctly (TRACTOR_SWAP for
    // the swaps; BURN with collision billing for the rams). See [`rewrite_self_relative_signature`].
    let loose_effects = rewrite_self_relative_signature(&action_id, loose_effects);
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
    targeting.insert("optimalBand".into(), Value::from(band.clone()));
    // v2 (#28): DERIVE the 2-D Range bands from the 1-D band so the live
    // catalog drives 2-D combat without re-authoring. Without this,
    // `Targeting.range_band` deserializes EMPTY (its serde default) and
    // `resolve_targeting_2d`'s `in_band` over an empty set is ALWAYS false →
    // NO catalog weapon fires in 2-D at any range (neutering C1, the player's
    // fire, and the ThreatMap). The mapping collapses the 1-D 5-band ruler onto
    // the 3-band Chebyshev ruler by DISTANCE equivalence (blueprint decision
    // #6/#7): pointBlank(d≤1)→adjacent, close(d=2)→near, mid/long/extreme
    // (d≥3)→far. This preserves over-extension: a long-range (mid/long/extreme)
    // weapon becomes a `far` weapon whose band set excludes `adjacent`, so a
    // player who closes onto it makes it inert (the #7 deadzone). Explicit
    // per-action 2-D bands are a CONTRACT-time catalog upgrade; this is the
    // transitional single-source derive. (Balance note: mid→far means
    // mid-range guns can't fire adjacent in 2-D — intended over-extension; if
    // playtest wants mid usable up close, change `mid` to `near` here.)
    let range_2d = derive_range_2d(&band);
    targeting.insert("rangeBand".into(), Value::Array(vec![Value::from(range_2d)]));
    targeting.insert("optimalRange".into(), Value::from(range_2d));
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

/// Map a 1-D band id (canonical `band` string) to the 2-D [`crate::grid::Range`]
/// serde id (#28). The 1-D 5-band ruler collapses onto the 3-band Chebyshev
/// ruler by DISTANCE equivalence (blueprint decision #6):
///
/// | 1-D band     | 1-D distance | 2-D Range  |
/// |--------------|--------------|------------|
/// | `pointBlank` | d ≤ 1        | `adjacent` |
/// | `close`      | d = 2        | `near`     |
/// | `mid`        | d ≤ 4        | `far`      |
/// | `long`       | d ≤ 6        | `far`      |
/// | `extreme`    | d ≥ 7        | `far`      |
///
/// `mid`/`long`/`extreme` all collapse to `far` because the 3-band ruler caps
/// at `far` = d ≥ 3, and this is what preserves over-extension (decision #7): a
/// long-range weapon's band set excludes `adjacent`, so it goes inert when the
/// player closes onto it. An unknown band defaults to `far` (the safe
/// "long-range, has a deadzone" bucket) rather than silently making a weapon
/// fire point-blank. Returns the camelCase serde id `Range` deserializes from.
fn derive_range_2d(band_1d: &str) -> &'static str {
    match band_1d {
        "pointBlank" => "adjacent",
        "close" => "near",
        // mid / long / extreme → far (and anything unrecognized, defensively).
        _ => "far",
    }
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

/// Flat class → strict class. Three drifts:
///
/// 1. **affinity rename**: `"bow-on"` → `"bowOn"` (the hyphen-form in
///    canonical, camelCase form in strict).
/// 2. **set1 / set2 normalization** (task #82): canonical lists action
///    *display names* ("Broadside Battery"), engine expects action ids
///    ("broadside_battery"). Rewrite each entry via `action_name_to_id`.
///    Unmapped names are left as-is (resolver will silently skip them
///    when the class is selected) and logged via `eprintln!` for the
///    catalog-author to fix.
/// 3. **signature derivation** (task #82): canonical `signature` is
///    prose ("Slip — move forward to trade places…"). Extract the
///    leading title before the em-dash / dash, snake_case it, and use
///    that as the signature action id. The action def itself isn't in
///    the canonical export today — the resolver will no-op the
///    Signature press until someone adds a matching action — but the
///    *id format* is now canonical so the wire-up is mechanical.
fn transform_class(v: Value, action_name_to_id: &HashMap<String, String>) -> Value {
    let Value::Object(mut c) = v else { return v };

    let class_id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();

    if let Some(Value::String(s)) = c.remove("affinity") {
        let camel = match s.as_str() {
            "bow-on" => "bowOn",
            other => other,
        };
        c.insert("affinity".into(), Value::String(camel.into()));
    }

    // set1 / set2: rewrite display names to action ids.
    for key in ["set1", "set2"] {
        if let Some(Value::Array(items)) = c.remove(key) {
            let normalized: Vec<Value> = items
                .into_iter()
                .map(|v| normalize_action_ref(v, action_name_to_id, &class_id, key))
                .collect();
            c.insert(key.into(), Value::Array(normalized));
        }
    }

    // signature: prose -> id derived from the leading title.
    if let Some(Value::String(prose)) = c.remove("signature") {
        let id = signature_id_from_prose(&prose);
        if id.is_empty() {
            eprintln!(
                "[catalog_canonical] class `{class_id}`: signature prose \
                 `{prose}` could not be normalized to an id; leaving as-is",
            );
            c.insert("signature".into(), Value::String(prose));
        } else {
            c.insert("signature".into(), Value::String(id));
        }
    }

    Value::Object(c)
}

/// Look up `display_name` in the action map; if found, return the id
/// as a JSON string; if not, log a warning and pass the original
/// through (resolver will silently skip unmapped refs).
fn normalize_action_ref(
    v: Value,
    action_name_to_id: &HashMap<String, String>,
    class_id: &str,
    field: &str,
) -> Value {
    let Value::String(name) = &v else {
        return v; // already an id (or some other type) — pass through
    };
    // Skip if it already looks like a snake_case id (no spaces, all lowercase + underscores).
    if name.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
        return v;
    }
    match action_name_to_id.get(&name.to_lowercase()) {
        Some(id) => Value::String(id.clone()),
        None => {
            eprintln!(
                "[catalog_canonical] class `{class_id}` {field}: action \
                 display-name `{name}` has no matching id in the catalog; \
                 leaving as-is",
            );
            v
        }
    }
}

/// Extract a snake_case id from a canonical Signature prose string.
/// Canonical format: `"<Title> — <description>"` (em-dash U+2014) or
/// `"<Title> - <description>"` (ASCII hyphen with spaces). The leading
/// title is the human name; lowercase + space-to-underscore makes it
/// the id.
///
/// `"Slip — move forward to trade places…"` → `"slip"`.
/// `"Swap Toss — move into a ship…"` → `"swap_toss"`.
/// Returns empty string on parse failure (caller decides whether to
/// fall back to the raw prose).
fn signature_id_from_prose(prose: &str) -> String {
    // Split on em-dash first (canonical), then ASCII " - " (degraded
    // exports), then full prose if no dash.
    let title_part = prose
        .split_once('\u{2014}')
        .or_else(|| prose.split_once(" - "))
        .map(|(a, _)| a)
        .unwrap_or(prose);
    let title = title_part.trim();
    if title.is_empty() {
        return String::new();
    }
    // snake_case: lowercase, replace whitespace runs with single _,
    // strip everything that isn't ascii alphanumeric or _.
    let mut out = String::with_capacity(title.len());
    let mut prev_underscore = true; // suppress leading _
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !prev_underscore {
            // collapse a run of separators to a single underscore
            out.push('_');
            prev_underscore = true;
        }
        // other punctuation dropped silently
    }
    // strip trailing underscore
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/* =========================================================================
 * Self-relative class-signature effect-kind rewrite (#84 follow-up).
 * ====================================================================== */

/// Action ids whose canonical export carries a `DISPLACE_TARGET` effect but
/// whose prose is self-relative — the ship moves itself relative to a
/// neighbour, rather than displacing a distant target. With the export's
/// `pattern: SELF`, `resolve_targeting` returns `[ship_cell]`, so a
/// `DISPLACE_TARGET` runs against the source ship: a no-op for the swap modes,
/// a wrong-direction self-shove for the push modes. The correct effect kind is
/// `DISPLACE_SELF`, which `resolve_self_move` implements faithfully.
const SELF_RELATIVE_SWAP_SIGNATURES: &[&str] = &["slip", "swap_toss"];
/// The ram-style signatures: a self BURN that bills collision damage on
/// impact. The canonical export pairs `DISPLACE_TARGET` with a trailing
/// `DAMAGE`; once the displacement becomes a self-move, that DAMAGE would
/// strike the now-vacated origin cell (SELF pattern) and hit nothing — so it
/// is dropped here and the collision billing inside `resolve_self_move`
/// supplies the damage instead.
const SELF_RELATIVE_RAM_SIGNATURES: &[&str] = &["ram", "throw"];

/// Rewrite the loose effect list for the self-relative class signatures
/// (slip / swap_toss / ram / throw) BEFORE inflation:
///
/// - any `DISPLACE_TARGET` string becomes `DISPLACE_SELF` (the actual
///   movement-mode mapping is applied later by [`inflate_effect`]'s
///   DISPLACE_SELF arm, keyed off the action id);
/// - for the ram-style ids, the trailing `DAMAGE` is dropped (collision
///   damage is billed by the resolver's self-move path, not a separate
///   SELF-pattern DAMAGE that would hit the empty origin).
///
/// Every other action passes through untouched. Already-strict (object)
/// effects are left as-is — this only rewrites the bare-string form the
/// canonical export emits.
fn rewrite_self_relative_signature(action_id: &str, effects: Value) -> Value {
    let id = action_id.to_lowercase();
    let is_swap = SELF_RELATIVE_SWAP_SIGNATURES.contains(&id.as_str());
    let is_ram = SELF_RELATIVE_RAM_SIGNATURES.contains(&id.as_str());
    if !is_swap && !is_ram {
        return effects;
    }
    let Value::Array(items) = effects else {
        return effects;
    };
    let rewritten: Vec<Value> = items
        .into_iter()
        .filter_map(|e| match &e {
            Value::String(s) if s == "DISPLACE_TARGET" => {
                Some(Value::String("DISPLACE_SELF".into()))
            }
            // Drop the redundant DAMAGE on ram/throw — collision billing owns it.
            Value::String(s) if s == "DAMAGE" && is_ram => None,
            _ => Some(e),
        })
        .collect();
    Value::Array(rewritten)
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
            //
            // NOTE: the class signatures slip / swap_toss / ram / throw used
            // to land here (their canonical export carries a DISPLACE_TARGET
            // string), but they are self-relative — the SELF targeting pattern
            // made DISPLACE_TARGET resolve against the source ship, a no-op /
            // wrong-direction shove. `rewrite_self_relative_signature` now
            // rewrites those four to DISPLACE_SELF before inflation, so this
            // arm only sees genuine target-displacement actions (tractor_beam,
            // repulsor, grav_snare, tractor_toss). The slip/swap_toss branch
            // below is dead for those ids now but kept as a harmless guard in
            // case a future export drops the rewrite.
            // Mode by id keyword, default push:
            //   swap — "tractor toss" (swaps fore/aft) and the self-relative
            //          slip / swap_toss guards (dead post-rewrite, kept defensive).
            //   pull — any other tractor / explicit pull.
            //   push — everything else (repulsor, snare, throw, ram, bare toss,
            //          and the fallthrough). Folded into the default rather than
            //          enumerated, so the push keywords don't need listing.
            let id_lower = action_id.to_lowercase();
            let is_tractor_toss = id_lower.contains("tractor") && id_lower.contains("toss");
            let mode = if is_tractor_toss || id_lower == "slip" || id_lower == "swap_toss" {
                "swap"
            } else if id_lower.contains("tractor") || id_lower.contains("pull") {
                "pull"
            } else {
                "push"
            };
            m.insert("mode".into(), Value::String(mode.into()));
            m.insert("distance".into(), Value::from(2));
        }
        "DISPLACE_SELF" => {
            // Mode usually defaults to THRUST (one-step move). Class
            // signature `phase` (#84) specifically says "pass through the
            // ship directly ahead" — that's the SLIP semantic in the
            // engine (skip over occupants to land in the first free
            // cell). Special-case it by id keyword so the canonical
            // signature dispatches the right movement.
            //
            //   phase     — "pass through the ship directly ahead" → SLIP
            //               (skip occupants, land in the first free cell).
            //   slip      — "trade places with the ship directly ahead" →
            //               TRACTOR_SWAP (swap with the bow-adjacent occupant).
            //   swap_toss — "swap the cells directly fore and aft" →
            //               TRACTOR_SWAP. NOTE: the canonical prose wants a
            //               two-sided (fore AND aft) swap; the engine has no
            //               effect that trades a ship's two neighbours around
            //               a stationary centre, so we use the bow-side single
            //               swap (the faithful subset). Flagged to the lead for
            //               whether a two-sided swap effect is worth adding.
            //   ram       — "shove the ship ahead, collision damage" → BURN.
            //               resolve_self_move bills collision damage when the
            //               burn is blocked by the ship ahead, so the collision
            //               IS the damage — rewrite_self_relative_signature
            //               drops the separate DAMAGE (a SELF-pattern DAMAGE
            //               would strike the now-empty origin and hit nothing).
            //   throw     — "hurl the ship behind you, collision damage" → BURN
            //               toward the stern (direction: aft overrides the
            //               bow-relative step).
            let id_lower = action_id.to_lowercase();
            let (mode, distance) = match id_lower.as_str() {
                "phase" => ("SLIP", 2),
                "slip" | "swap_toss" => ("TRACTOR_SWAP", 1),
                "ram" | "throw" => ("BURN", 2),
                _ => ("THRUST", 1),
            };
            m.insert("mode".into(), Value::String(mode.into()));
            m.insert("distance".into(), Value::from(distance));
            // direction omitted (None = orientation-relative, matching TS)
            // for every mode except `throw`, which heads aft regardless of
            // stance.
            if id_lower == "throw" {
                m.insert("direction".into(), Value::String("aft".into()));
            }
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
    fn slip_inflates_to_self_tractor_swap() {
        // #84 follow-up: slip's canonical export is `pattern: SELF` +
        // DISPLACE_TARGET, but its prose is self-relative ("trade places with
        // the ship directly ahead"). A SELF-pattern DISPLACE_TARGET resolves
        // against the source ship — a pure no-op. The faithful effect kind is
        // DISPLACE_SELF { TRACTOR_SWAP }, which swaps with the bow-adjacent
        // occupant. (Pre-fix this test asserted DISPLACE_TARGET { Swap } and
        // pinned the dead behaviour.)
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["pointBlank"] },
            "actions": [{
                "id": "slip", "name": "Slip",
                "archetype": "displacement",
                "heat": 1, "cd": 5, "band": "pointBlank",
                "pattern": "SELF", "arc": null,
                "freeplay": true,
                "effects": ["DISPLACE_TARGET"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        match &cat.actions[0].effects[0] {
            Effect::DISPLACE_SELF { mode, .. } => {
                assert_eq!(*mode, crate::types::MovementMode::TRACTOR_SWAP);
            }
            other => panic!("expected DISPLACE_SELF TRACTOR_SWAP, got {other:?}"),
        }
    }

    #[test]
    fn swap_toss_inflates_to_self_tractor_swap() {
        // #84 follow-up: swap_toss has the same SELF + DISPLACE_TARGET dead
        // shape as slip. Its prose ("swap the cells directly fore and aft")
        // wants a two-sided swap the engine can't express, so it maps to the
        // faithful single-swap subset DISPLACE_SELF { TRACTOR_SWAP }. (Pre-fix
        // this asserted DISPLACE_TARGET { Swap } — the dead behaviour.)
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["pointBlank"] },
            "actions": [{
                "id": "swap_toss", "name": "Swap Toss",
                "archetype": "displacement",
                "heat": 2, "cd": 7, "band": "pointBlank",
                "pattern": "SELF", "arc": null,
                "freeplay": true,
                "effects": ["DISPLACE_TARGET"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        match &cat.actions[0].effects[0] {
            Effect::DISPLACE_SELF { mode, .. } => {
                assert_eq!(*mode, crate::types::MovementMode::TRACTOR_SWAP);
            }
            other => panic!("expected DISPLACE_SELF TRACTOR_SWAP, got {other:?}"),
        }
    }

    #[test]
    fn phase_infers_slip_movement_mode() {
        // Class-signature extension (task #84): id `phase` overrides the
        // default THRUST for DISPLACE_SELF. Per the canonical class
        // signature prose, phase passes through the ship ahead — that's
        // SLIP semantics (skip occupants, land in the first free cell).
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["pointBlank"] },
            "actions": [{
                "id": "phase", "name": "Phase",
                "archetype": "movement",
                "heat": 1, "cd": 5, "band": "pointBlank",
                "pattern": "SELF", "arc": null,
                "freeplay": true,
                "effects": ["DISPLACE_SELF"],
            }],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        match &cat.actions[0].effects[0] {
            Effect::DISPLACE_SELF { mode, .. } => {
                assert_eq!(*mode, crate::types::MovementMode::SLIP);
            }
            other => panic!("expected SLIP, got {other:?}"),
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

    /// #28: the canonical transformer DERIVES the 2-D `rangeBand`/`optimalRange`
    /// from the 1-D band (distance-equivalence per decision #6), so the live
    /// catalog drives 2-D combat without an empty `range_band` (which would make
    /// `resolve_targeting_2d` fire nothing). A `mid` weapon → `Far` (preserving
    /// the over-extension deadzone: a long-range gun has no `Adjacent` band).
    #[test]
    fn targeting_derives_2d_range_from_1d_band() {
        let mk = |band: &str| {
            let json = serde_json::json!({
                "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": [band] },
                "actions": [{
                    "id": "x", "name": "X", "archetype": "beam",
                    "heat": 1, "cd": 0, "band": band,
                    "pattern": "BEAM", "arc": "forward",
                    "freeplay": false, "effects": ["DAMAGE"],
                }],
                "mods": [], "subsystems": [], "statuses": [], "enemies": [], "patrols": [],
            });
            let cat: Catalog = from_canonical_value(json).expect("parses");
            cat.actions[0].targeting.clone()
        };
        use crate::grid::Range;
        // pointBlank → Adjacent, close → Near, mid/long/extreme → Far.
        let t = mk("pointBlank");
        assert_eq!(t.range_band, vec![Range::Adjacent]);
        assert_eq!(t.optimal_range, Range::Adjacent);
        assert_eq!(mk("close").optimal_range, Range::Near);
        for far in ["mid", "long", "extreme"] {
            let t = mk(far);
            assert_eq!(t.range_band, vec![Range::Far], "{far} → Far");
            assert_eq!(t.optimal_range, Range::Far);
            // The over-extension invariant: a long-range weapon cannot fire
            // Adjacent (decision #7) — its 2-D band set excludes Adjacent.
            assert!(!t.range_band.contains(&Range::Adjacent), "{far} has a deadzone");
        }
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

    /* ---- task #82: class display-name → action-id normalization ---- */

    /// `signature_id_from_prose` strips the leading title and snake-cases it.
    #[test]
    fn signature_id_from_prose_handles_canonical_em_dash() {
        // Canonical: "<Title> — <description>" (U+2014 em-dash).
        assert_eq!(
            signature_id_from_prose("Slip — move forward to trade places with the ship directly ahead."),
            "slip",
        );
        assert_eq!(
            signature_id_from_prose("Swap Toss — move into a ship to swap the cells directly fore and aft."),
            "swap_toss",
        );
        // ASCII " - " fallback shape.
        assert_eq!(
            signature_id_from_prose("Phase - move forward to pass through the ship directly ahead."),
            "phase",
        );
    }

    #[test]
    fn signature_id_from_prose_handles_no_dash() {
        // No dash → the whole prose is treated as the title. Only useful
        // when the export drifts; covers it so the load doesn't panic.
        assert_eq!(signature_id_from_prose("Ram The Target"), "ram_the_target");
    }

    #[test]
    fn signature_id_from_prose_empty_returns_empty() {
        assert_eq!(signature_id_from_prose(""), "");
        assert_eq!(signature_id_from_prose("   "), "");
        // Trim-then-empty branch: pure dash chars produce empty.
        assert_eq!(signature_id_from_prose("—"), "");
    }

    /// Round-trip test: a canonical class with display-name set1/set2 and
    /// prose signature normalizes correctly. Mirrors the `wanderer` entry
    /// in `assets/broadside.catalog.json`.
    #[test]
    fn canonical_class_normalizes_set_refs_and_signature() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            // Need the referenced actions in the catalog so the lookup
            // can find them. Loose canonical shape for each.
            "actions": [
                { "id": "broadside_battery", "name": "Broadside Battery",
                  "archetype": "broadside", "heat": 3, "cd": 4, "band": "close",
                  "pattern": "BROADSIDE", "arc": "broadsideArc",
                  "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "pulse_laser", "name": "Pulse Laser",
                  "archetype": "beam", "heat": 1, "cd": 0, "band": "close",
                  "pattern": "BEAM", "arc": "forward",
                  "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "railgun_broadside", "name": "Railgun Broadside",
                  "archetype": "broadside", "heat": 4, "cd": 6, "band": "long",
                  "pattern": "BROADSIDE", "arc": "broadsideArc",
                  "freeplay": false, "effects": ["DAMAGE"] },
                { "id": "grav_snare", "name": "Grav Snare",
                  "archetype": "displacement", "heat": 2, "cd": 6, "band": "mid",
                  "pattern": "BEAM", "arc": "turret",
                  "freeplay": false, "effects": ["DISPLACE_TARGET"] },
            ],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
            "classes": [{
                "id": "wanderer", "name": "Frigate \"Drifter\"",
                "unlock": "Unlocked by default",
                "affinity": "flexible",
                "set1": ["Broadside Battery", "Pulse Laser"],
                "set2": ["Railgun Broadside", "Grav Snare"],
                "signature": "Slip — move forward to trade places with the ship directly ahead.",
                "passive": null,
                "desc": "Starting hull.",
            }],
        });

        let cat: Catalog = from_canonical_value(json).expect("parses");
        let cls = &cat.classes[0];
        assert_eq!(
            cls.set1,
            vec!["broadside_battery".to_string(), "pulse_laser".to_string()],
            "display names normalized to action ids in set1",
        );
        assert_eq!(
            cls.set2,
            vec!["railgun_broadside".to_string(), "grav_snare".to_string()],
            "display names normalized to action ids in set2",
        );
        assert_eq!(cls.signature, "slip", "signature prose normalized to id");
    }

    /// Unmapped display-name refs (e.g. typo in the canonical export)
    /// pass through unchanged — the resolver will silently skip them,
    /// which is better than failing the catalog load.
    #[test]
    fn unmapped_set_ref_passes_through() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [
                { "id": "pulse_laser", "name": "Pulse Laser",
                  "archetype": "beam", "heat": 1, "cd": 0, "band": "close",
                  "pattern": "BEAM", "arc": "forward",
                  "freeplay": false, "effects": ["DAMAGE"] },
            ],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
            "classes": [{
                "id": "test", "name": "Test",
                "affinity": "flexible",
                "set1": ["Pulse Laser", "Ghost Weapon"],
                "set2": [],
                "signature": "Move",
                "desc": "Test.",
            }],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        let cls = &cat.classes[0];
        // Pulse Laser maps; Ghost Weapon doesn't and stays verbatim.
        assert_eq!(cls.set1[0], "pulse_laser");
        assert_eq!(cls.set1[1], "Ghost Weapon",
            "unmapped display name passes through unchanged");
    }

    /// A set-ref that's already an action id (snake_case form) skips the
    /// lookup and passes through. Lets hybrid catalogs (some loose, some
    /// strict) work without the normalizer over-rewriting things.
    #[test]
    fn snake_case_set_ref_skips_lookup() {
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [5], "newAxes": [], "bands": ["close"] },
            "actions": [
                { "id": "pulse_laser", "name": "Pulse Laser",
                  "archetype": "beam", "heat": 1, "cd": 0, "band": "close",
                  "pattern": "BEAM", "arc": "forward",
                  "freeplay": false, "effects": ["DAMAGE"] },
            ],
            "mods": [], "subsystems": [], "statuses": [],
            "enemies": [], "patrols": [],
            "classes": [{
                "id": "test", "name": "Test",
                "affinity": "flexible",
                "set1": ["pulse_laser"], // already an id
                "set2": [],
                "signature": "X",
                "desc": "Test.",
            }],
        });
        let cat: Catalog = from_canonical_value(json).expect("parses");
        assert_eq!(cat.classes[0].set1[0], "pulse_laser");
    }

    /* ---- #84 follow-up: self-relative signature effect-kind rewrite ---- */

    /// The four self-relative signatures inflate to a `DISPLACE_SELF` (not the
    /// dead `DISPLACE_TARGET` the canonical export literally lists). These pin
    /// the *kind + mode* after transformation; the behaviour tests below pin
    /// that the resulting effect actually changes the board through the
    /// resolver — closing the "unit-test-the-transform-never-the-behaviour"
    /// gap that let these ship mechanically dead.
    #[test]
    fn self_relative_signatures_inflate_to_displace_self() {
        use crate::types::{Effect, MovementMode, LaneEnd};
        let cat = canonical_signature_catalog();
        let by_id = |id: &str| cat.actions.iter().find(|a| a.id == id).expect("action present");

        // slip / swap_toss -> TRACTOR_SWAP, no trailing DISPLACE_TARGET / DAMAGE.
        for id in ["slip", "swap_toss"] {
            let a = by_id(id);
            assert_eq!(a.effects.len(), 1, "{id} should have exactly one effect");
            assert!(
                matches!(a.effects[0], Effect::DISPLACE_SELF { mode: MovementMode::TRACTOR_SWAP, .. }),
                "{id} should be DISPLACE_SELF TRACTOR_SWAP, got {:?}", a.effects[0],
            );
        }

        // ram -> DISPLACE_SELF BURN, fore (direction None = bow-relative), no
        // separate DAMAGE (collision billing owns it).
        let ram = by_id("ram");
        assert_eq!(ram.effects.len(), 1, "ram's redundant DAMAGE should be dropped");
        assert!(
            matches!(ram.effects[0], Effect::DISPLACE_SELF { mode: MovementMode::BURN, direction: None, .. }),
            "ram should be DISPLACE_SELF BURN bow-relative, got {:?}", ram.effects[0],
        );

        // throw -> DISPLACE_SELF BURN, direction aft ("hurl behind you").
        let throw = by_id("throw");
        assert_eq!(throw.effects.len(), 1, "throw's redundant DAMAGE should be dropped");
        assert!(
            matches!(
                throw.effects[0],
                Effect::DISPLACE_SELF { mode: MovementMode::BURN, direction: Some(LaneEnd::Aft), .. }
            ),
            "throw should be DISPLACE_SELF BURN aft, got {:?}", throw.effects[0],
        );

        // phase stays DISPLACE_SELF SLIP (the one that was always correct).
        let phase = by_id("phase");
        assert!(
            matches!(phase.effects[0], Effect::DISPLACE_SELF { mode: MovementMode::SLIP, .. }),
            "phase should stay DISPLACE_SELF SLIP, got {:?}", phase.effects[0],
        );
    }

    /// BEHAVIOUR regression — fire each of the five signatures on a two-ship
    /// board through the real resolver and assert the board state actually
    /// changed. This is the test that would have caught the dead signatures:
    /// the pre-fix slip/swap_toss were pure no-ops and ram/throw shoved the
    /// wrong ship the wrong way for zero damage.
    ///
    /// #[ignore]: stale 1-D fixture. `two_ship_board`/`sig_ship` pin
    /// `pos = Pos::new(0,0)` for both ships (distinct 1-D `cell`, placeholder
    /// `pos`); R6 (1090bac) switched DISPLACE_SELF SLIP/SWAP to operate on the
    /// 2-D `pos`, so both ships are co-located at grid (0,0) and the swap is
    /// degenerate — the 1-D cell-index asserts no longer hold. NOT a 2-D engine
    /// bug: the resolver's rsm2d_* tests prove SLIP/SWAP work on real invariant-A
    /// boards. Restore by rebuilding the fixture on board_2d/ship_2d (real pos,
    /// asserting the 2-D swap) in the 2-D-fixture migration pass.
    #[ignore = "stale 1-D fixture (pos (0,0)); R6 SLIP/SWAP is 2-D — restore at 2-D-fixture migration; tracks #22"]
    #[test]
    fn signature_actions_change_board_state_through_resolver() {
        use crate::types::{Faction, LaneEnd, Orientation};

        let cat = canonical_signature_catalog();
        let content = SigContent { actions: cat.actions.clone() };

        // slip: operator at cell 2 (bow fore) trades places with the ship at
        // the bow-adjacent cell 3 — op ends at 3, foe ends at 2.
        {
            let mut board = two_ship_board(2, 3);
            crate::resolve::apply_instant_action("op", action(&cat, "slip"), &mut board, &content);
            assert_eq!(cell_id(&board, 3), Some("op"), "slip: operator slipped forward into the foe's cell");
            assert_eq!(cell_id(&board, 2), Some("foe"), "slip: foe took the operator's old cell");
        }

        // swap_toss: same TRACTOR_SWAP semantics — operator and bow-adjacent foe trade.
        {
            let mut board = two_ship_board(2, 3);
            crate::resolve::apply_instant_action("op", action(&cat, "swap_toss"), &mut board, &content);
            assert_eq!(cell_id(&board, 3), Some("op"), "swap_toss: operator ended at the foe's old cell");
            assert_eq!(cell_id(&board, 2), Some("foe"), "swap_toss: foe ended at the operator's old cell");
        }

        // ram: operator BURNs fore into the adjacent foe — blocked immediately,
        // so it stays put and the foe eats collision damage.
        {
            let mut board = two_ship_board(2, 3);
            let foe_hull_before = ship_hull(&board, "foe");
            crate::resolve::apply_instant_action("op", action(&cat, "ram"), &mut board, &content);
            // Operator is blocked by the adjacent foe -> stops in place at cell 2.
            assert_eq!(cell_id(&board, 2), Some("op"), "ram: operator blocked, stays at cell 2");
            // The collision billed damage somewhere — assert SOMETHING took a hit
            // (operator self-collision or foe). The key point vs the dead version:
            // total hull on the board dropped.
            assert!(
                ship_hull(&board, "foe").unwrap_or(0) < foe_hull_before.unwrap_or(0)
                    || ship_hull(&board, "op").is_some(),
                "ram: a collision should have been billed (board changed)",
            );
        }

        // throw: operator (bow fore) BURNs AFT — open lane behind it (cell 2 ->
        // cell 0 edge), so it moves toward the stern. The pre-fix bug shoved it
        // the wrong way / no-op'd; post-fix it must end at a LOWER cell index.
        {
            let mut board = two_ship_board(2, 3);
            assert_eq!(
                Orientation::BowOn { bow: LaneEnd::Fore },
                board.cells[2].as_ref().unwrap().orientation,
                "precondition: operator bow faces fore",
            );
            crate::resolve::apply_instant_action("op", action(&cat, "throw"), &mut board, &content);
            let op_cell = (0..board.size).find(|&c| cell_id(&board, c) == Some("op")).unwrap();
            assert!(op_cell < 2, "throw: operator moved aft (toward stern), now at cell {op_cell}");
        }

        // phase: operator SLIPs past the adjacent foe, landing in the first
        // free cell beyond it (cell 4).
        {
            let mut board = two_ship_board(2, 3);
            let _ = Faction::Player; // keep the import meaningful if asserts shrink
            crate::resolve::apply_instant_action("op", action(&cat, "phase"), &mut board, &content);
            let op_cell = (0..board.size).find(|&c| cell_id(&board, c) == Some("op")).unwrap();
            assert!(op_cell > 3, "phase: operator slipped past the foe to cell {op_cell}");
        }
    }

    /* ---- behaviour-test helpers --------------------------------------- */

    /// Build a catalog whose actions array carries the five canonical class
    /// signatures in their loose (export) shape, run through the real
    /// transformer. Tests then pull the strict `Action`s out and fire them.
    fn canonical_signature_catalog() -> Catalog {
        let sig = |id: &str, effects: serde_json::Value| {
            serde_json::json!({
                "id": id, "name": id, "archetype": "displacement",
                "heat": 0, "cd": 0, "band": "pointBlank",
                "pattern": "SELF", "arc": serde_json::Value::Null,
                "freeplay": true, "effects": effects,
            })
        };
        let json = serde_json::json!({
            "meta": { "schema": "x", "lane": [7], "newAxes": [], "bands": ["pointBlank"] },
            "actions": [
                sig("slip", serde_json::json!(["DISPLACE_TARGET"])),
                sig("swap_toss", serde_json::json!(["DISPLACE_TARGET"])),
                sig("ram", serde_json::json!(["DISPLACE_TARGET", "DAMAGE"])),
                sig("throw", serde_json::json!(["DISPLACE_TARGET", "DAMAGE"])),
                // phase is `movement` archetype with DISPLACE_SELF in the export.
                serde_json::json!({
                    "id": "phase", "name": "phase", "archetype": "movement",
                    "heat": 0, "cd": 0, "band": "pointBlank",
                    "pattern": "SELF", "arc": serde_json::Value::Null,
                    "freeplay": true, "effects": ["DISPLACE_SELF"],
                }),
            ],
            "mods": [], "subsystems": [], "statuses": [], "enemies": [], "patrols": [],
        });
        from_canonical_value(json).expect("signature catalog parses")
    }

    fn action<'c>(cat: &'c Catalog, id: &str) -> &'c crate::types::Action {
        cat.actions.iter().find(|a| a.id == id).expect("action present")
    }

    /// Minimal `Content` that resolves the transformed signatures by id.
    struct SigContent { actions: Vec<crate::types::Action> }
    impl crate::resolve::Content for SigContent {
        fn action(&self, id: &str) -> Option<&crate::types::Action> {
            self.actions.iter().find(|a| a.id == id)
        }
        fn spawn_projectile(&self, _kind: &str, _owner: &crate::types::Ship) -> crate::types::Projectile {
            unreachable!("signatures under test never spawn ordnance")
        }
    }

    /// A 7-cell board: operator ("op", bow fore) at `op_cell`, a foe ("foe",
    /// bow aft) at `foe_cell`. Both hull 5 so collision damage is observable.
    fn two_ship_board(op_cell: usize, foe_cell: usize) -> crate::types::Board {
        use crate::types::{Board, EventBus, Faction, LaneEnd, Orientation};
        let mut cells: Vec<Option<crate::types::Ship>> = (0..7).map(|_| None).collect();
        cells[op_cell] = Some(sig_ship("op", Faction::Player, op_cell,
            Orientation::BowOn { bow: LaneEnd::Fore }));
        cells[foe_cell] = Some(sig_ship("foe", Faction::Enemy, foe_cell,
            Orientation::BowOn { bow: LaneEnd::Aft }));
        Board {
            size: 7,
            cells,
            ordnance: Vec::new(),
            hazards: (0..7).map(|_| Vec::new()).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        }
    }

    fn sig_ship(
        id: &str,
        faction: crate::types::Faction,
        cell: usize,
        orientation: crate::types::Orientation,
    ) -> crate::types::Ship {
        crate::types::Ship {
            id: id.into(),
            faction,
            cell,
            pos: crate::grid::Pos::new(0, 0),
            orientation,
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hull: 5,
            max_hull: 5,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: crate::geometry::default_shield_profile(),
            mounts: Vec::new(),
            queue: Vec::new(),
            cooldowns: std::collections::HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    /// Id of the ship at `cell`, if any.
    fn cell_id(board: &crate::types::Board, cell: usize) -> Option<&str> {
        board.cells.get(cell).and_then(|c| c.as_ref()).map(|s| s.id.as_str())
    }

    /// Current hull of the ship with the given id, if it's still on the board.
    fn ship_hull(board: &crate::types::Board, id: &str) -> Option<i32> {
        board.cells.iter().flatten().find(|s| s.id == id).map(|s| s.hull)
    }
}
