//! Complete content + runtime type surface for the Broadside resolver.
//!
//! Mirrors `broadside-engine/engine/types.ts` (Section XIII of the analysis
//! doc). Pure types — no logic. The TypeScript engine is the canonical
//! reference; when this port and the TS disagree, the TS is right.
//!
//! ## Layout (matches the TS file's section comments)
//!
//! 1. Geometry — `LaneEnd`, `Orientation`, `HullZone`, `RangeBand`, `Arc`,
//!    `Faction`.
//! 2. Board — `Board`, `Hazard`.
//! 3. Ship — `Ship`, `ShieldFace`, `Mount`, `Status`, `StatusKind`, `Trait`.
//! 4. Action — `Action`, `ActionCost`, `Targeting`, `WeaponArchetype`,
//!    `TargetingPattern`.
//! 5. Effects — `Effect`, `MovementMode`.
//! 6. Ordnance — `Projectile`.
//! 7. Subsystems / event bus — `SubsystemDef`, `Hook`, `HookContext`.
//! 8. Catalog — `Catalog`, `EnemyDef`, plus content sub-record types.
//!
//! ## Serde conventions
//!
//! The JSON catalog is produced by the design-doc "Copy JSON" button and
//! is the lingua franca between TS and Rust. To keep the on-the-wire shape
//! identical:
//!
//! - `Orientation` is `#[serde(tag = "stance", rename_all = "camelCase")]`
//!   so `{ "stance": "bowOn", "bow": "fore" }` and `{ "stance": "broadside" }`
//!   both parse.
//! - `Effect` is `#[serde(tag = "kind")]` with variant names left in the
//!   TS `SCREAMING_SNAKE_CASE` form (`DAMAGE`, `APPLY_STATUS`, …).
//! - Most other enums use `#[serde(rename_all = "camelCase")]` to match TS
//!   string-literal unions like `"pointBlank" | "close" | "mid"`.
//! - `Hook` variant names are camelCase event names (`"onChainKill"`, …).
//!
//! ## Runtime vs. catalog split
//!
//! [`Board`] holds an `EventBus` and a destruction counter — both are runtime
//! collaborators, not catalog data, so [`Board`] does **not** derive serde.
//! [`SubsystemDef`] is the serde shape from the JSON; a separate `Subsystem`
//! runtime type (bundling a callback) will live alongside the resolver and
//! is owned by the content slice.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/* =========================================================================
 * 1. Geometry
 * ====================================================================== */

/// The two directions along the 1-D lane. `Fore` = toward higher cell index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LaneEnd {
    Fore,
    Aft,
}

/// Hull orientation. `BowOn` points the nose at one lane end (bow takes hits
/// from that side); `Broadside` turns the hull across the lane so both flanks
/// face it. This is the primary tactical axis of the design.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "stance", rename_all = "camelCase")]
pub enum Orientation {
    BowOn { bow: LaneEnd },
    Broadside,
}

/// Fixed armour zones welded to the hull. Strong bow, weak stern, medium flanks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HullZone {
    Bow,
    Stern,
    Port,
    Starboard,
}

/// Distance buckets. Every weapon has an optimal band and falls off outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangeBand {
    PointBlank,
    Close,
    Mid,
    Long,
    Extreme,
}

/// A weapon mount's firing arc, relative to the hull (not the lane).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Arc {
    Forward,
    BroadsideArc,
    Turret,
    Rear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Faction {
    Player,
    Enemy,
}

/* =========================================================================
 * 2. Board
 * ====================================================================== */

/// The 1-D battlefield. Live runtime state — holds the event bus and a
/// chain-kill window counter, neither of which round-trips through JSON,
/// so [`Board`] is intentionally **not** serde-derived.
///
/// `destroys_this_window` is incremented by the resolver each time a ship is
/// destroyed and reset to zero at well-defined window boundaries (start of
/// `executeQueue` and start of the ordnance phase). Two or more destroys in
/// the same window triggers `onChainKill`. Per the team coordination
/// decision, reset semantics live in the resolver, not here.
pub struct Board {
    /// Lane length. The TS uses 5, 7, or 9.
    pub size: usize,
    /// `cells[i]` is `Some(ship)` if a ship is at lane index `i`, else `None`.
    pub cells: Vec<Option<Ship>>,
    /// Live torpedoes / missiles travelling the lane.
    pub ordnance: Vec<Projectile>,
    /// Per-cell features (mines, drones, debris). Outer index matches `cells`.
    pub hazards: Vec<Vec<Hazard>>,
    /// 1..=7 global difficulty tier.
    pub patrol: u8,
    /// Pub/sub for subsystem hooks.
    pub bus: EventBus,
    /// Ships destroyed during the current chain-kill window. Resolver-managed.
    pub destroys_this_window: usize,
}

/// A cell-resident hazard: mine, drone, or debris field. Applies `payload`
/// to anything that enters the cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hazard {
    pub id: String,
    pub kind: HazardKind,
    pub cell: usize,
    pub payload: Vec<Effect>,
    /// Optional lifespan in turns; `None` = persistent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HazardKind {
    Mine,
    Drone,
    Debris,
}

/* =========================================================================
 * 3. Ship
 * ====================================================================== */

/// Player and enemy ships share this shape; `faction` distinguishes them.
/// Mirrors the TS `Ship` interface verbatim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ship {
    pub id: String,
    pub faction: Faction,
    pub cell: usize,
    pub orientation: Orientation,
    pub hull: i32,
    #[serde(rename = "maxHull")]
    pub max_hull: i32,
    pub heat: i32,
    /// Crossing this triggers lockout.
    #[serde(rename = "heatMax")]
    pub heat_max: i32,
    #[serde(rename = "lockedOut")]
    pub locked_out: bool,
    /// Fixed-to-hull defensive layout. Keys are [`HullZone`]; matches the TS
    /// `Record<HullZone, ShieldFace>`. The JSON shape is an object keyed by
    /// the camelCase hull-zone names.
    #[serde(rename = "shieldProfile")]
    pub shield_profile: HashMap<HullZone, ShieldFace>,
    /// Fixed weapon hardpoints.
    pub mounts: Vec<Mount>,
    /// Action ids the player has queued; fires bottom to top.
    pub queue: Vec<String>,
    /// `actionId -> turns remaining`. Matches the TS `Record<string, number>`.
    pub cooldowns: HashMap<String, i32>,
    pub statuses: Vec<Status>,
    pub traits: Vec<Trait>,
    /// Optional class id; dispatches the ship's Signature action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub klass: Option<String>,
}

/// A hull zone's defence. `armour` is permanent directional reduction (bow
/// high, stern ~0); `charge` is consumable shield (from Brace etc.) that
/// negates a hit and is decremented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShieldFace {
    pub armour: i32,
    pub charge: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mount {
    pub id: String,
    pub arc: Arc,
    /// Action id of the weapon mounted here.
    pub weapon: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Status {
    pub kind: StatusKind,
    pub duration: i32,
    /// Hull zone the status applies to, when relevant (e.g. directional shields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face: Option<HullZone>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusKind {
    /// Poison analogue: damage over time.
    HullBreach,
    /// Frozen analogue: skip turn(s).
    SystemsOffline,
    /// Curse analogue: next incoming hit doubled.
    TargetLock,
    /// A held shield charge.
    ShieldsUp,
}

/// Ship traits. Spelled in TitleCase in the TS string union, so we keep that
/// on the wire and let Rust variant names match 1:1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Trait {
    Pursuit,
    Agile,
    ReactorBreach,
    BurnHard,
    Anchored,
    EliteAgile,
    EliteAnchored,
    TwinLinked,
    ReactiveShield,
    Voidtouched,
}

/* =========================================================================
 * 4. Action
 * ====================================================================== */

/// The universal Action — weapon, system, maneuver, or ordnance launcher.
/// Lookups happen by id through the catalog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub archetype: WeaponArchetype,
    pub cost: ActionCost,
    pub targeting: Targeting,
    pub effects: Vec<Effect>,
    /// At most one weapon mod, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#mod: Option<String>,
    /// SVG glyph or sprite URL — renderer can swap freely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionCost {
    pub heat: i32,
    #[serde(rename = "cooldownMax")]
    pub cooldown_max: i32,
    /// `false` = free-fire, doesn't advance the turn.
    #[serde(rename = "advancesTurn")]
    pub advances_turn: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Targeting {
    pub pattern: TargetingPattern,
    /// Bands the weapon is allowed to fire at.
    pub band: Vec<RangeBand>,
    /// Peak-damage band; outside it band falloff applies.
    #[serde(rename = "optimalBand")]
    pub optimal_band: RangeBand,
    /// Mount must bear this arc given the firing ship's orientation. `None`
    /// = arc-less action (SELF, DEPLOYED_CELL).
    #[serde(rename = "requiresArc")]
    pub requires_arc: Option<Arc>,
    #[serde(rename = "facingRelative")]
    pub facing_relative: bool,
    /// SPINAL_LINE pierce vs first-only.
    #[serde(rename = "hitsAll")]
    pub hits_all: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WeaponArchetype {
    Beam,
    Ordnance,
    Broadside,
    Displacement,
    Control,
    Movement,
    Defensive,
}

/// The eight resolveTargeting branches. TS uses `SCREAMING_SNAKE_CASE`
/// literals; we mirror exactly so the JSON wire shape is identical and so
/// `grep TargetingPattern::SPINAL_LINE` finds the same token across both
/// ports.
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TargetingPattern {
    POINT_BLANK,
    SPINAL_LINE,
    BEAM,
    BROADSIDE,
    BLAST,
    ORDNANCE,
    SELF,
    DEPLOYED_CELL,
}

/* =========================================================================
 * 5. Effects
 * ====================================================================== */

/// The verb payload an action emits. Internally tagged on `kind` so JSON
/// of the form `{ "kind": "DAMAGE", "amount": 4 }` deserializes directly.
/// Variant names preserved in `SCREAMING_SNAKE_CASE` to match the TS literals
/// — keeps `grep Effect::DAMAGE` working uniformly across both ports.
#[allow(non_camel_case_types)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Effect {
    DAMAGE {
        amount: i32,
        /// Defaults to `true` (resolver applies band falloff) when absent.
        #[serde(rename = "bandFalloff", default, skip_serializing_if = "Option::is_none")]
        band_falloff: Option<bool>,
    },
    APPLY_STATUS {
        status: StatusKind,
        duration: i32,
    },
    DISPLACE_TARGET {
        mode: DisplaceMode,
        distance: i32,
    },
    DISPLACE_SELF {
        mode: MovementMode,
        distance: i32,
    },
    REORIENT {
        to: ReorientTo,
    },
    SPAWN_ORDNANCE {
        /// Projectile kind id; resolver looks it up via `Content::spawnProjectile`.
        projectile: String,
    },
    VENT_HEAT {
        amount: i32,
        #[serde(rename = "rechargeCooldowns", default, skip_serializing_if = "Option::is_none")]
        recharge_cooldowns: Option<bool>,
    },
    DEPLOY {
        hazard: DeployHazardKind,
    },
    BOARD {
        note: String,
    },
}

/// Variants of `DISPLACE_TARGET`. TS uses lowercase literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplaceMode {
    Push,
    Pull,
    Swap,
}

/// Variants of `REORIENT.to`. TS uses lowercase literals plus `"bowOn"`/
/// `"broadside"` matching the orientation tag values, and `"flip"` for the
/// stance-preserving inversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReorientTo {
    BowOn,
    Broadside,
    Flip,
}

/// `DEPLOY` only spawns mines or drones (not debris). Mirrors the TS union
/// `"mine" | "drone"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeployHazardKind {
    Mine,
    Drone,
}

/// Self-movement modes. Listed `SCREAMING_SNAKE_CASE` in the TS to match
/// the action-system convention; mirrored here for grep parity.
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MovementMode {
    THRUST,
    BURN,
    SLIP,
    JUMP,
    TRACTOR_SWAP,
}

/* =========================================================================
 * 6. Ordnance entity
 * ====================================================================== */

/// A torpedo or missile travelling the lane. Spawned by `SPAWN_ORDNANCE`
/// effects; advanced by the ordnance phase; can be shot down by point-defense.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Projectile {
    pub id: String,
    pub kind: String,
    pub cell: usize,
    pub heading: LaneEnd,
    /// Cells advanced per turn.
    pub speed: u32,
    /// Hull points — point-defense damages this until the projectile breaks up.
    pub hull: i32,
    /// Applied on impact.
    pub payload: Vec<Effect>,
    #[serde(rename = "ownerFaction")]
    pub owner_faction: Faction,
}

/* =========================================================================
 * 7. Subsystems / event bus
 * ====================================================================== */

/// Serde-friendly catalog shape of a subsystem (the TS `Omit<Subsystem, "apply">`).
/// The runtime `Subsystem` type — which binds this definition to a Rust
/// callback — lives next to the content slice; for now the resolver and the
/// catalog only need the data side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubsystemDef {
    pub id: String,
    pub name: String,
    pub bay: SubsystemBay,
    pub hook: Hook,
    pub cost: i32,
    /// `null` in the JSON => available from the start. `Some(n)` => unlocks
    /// after `n` salvage runs.
    #[serde(rename = "unlockSalvage", default, skip_serializing_if = "Option::is_none")]
    pub unlock_salvage: Option<i32>,
    pub level: i32,
    #[serde(rename = "maxLevel")]
    pub max_level: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubsystemBay {
    Gunnery,
    Helm,
    Engineering,
    Tactical,
    General,
    Astrogation,
}

/// Event-bus hook the subsystem subscribes to. Variant names are the
/// `"onFoo"` strings used in the TS bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Hook {
    Passive,
    OnChainKill,
    OnTurnEnd,
    OnVent,
    OnWaveStart,
    OnHeatThreshold,
    OnDamageDealt,
    OnDamageTaken,
    OnHeal,
    OnReorient,
    OnLethal,
}

/// Context handed to a hook callback. Keeps a strongly-typed core and a
/// free-form `extras` bag for the TS `[k: string]: unknown` overflow. Not
/// serialized — bus state is purely runtime.
#[derive(Default)]
pub struct HookContext<'b> {
    pub board: Option<&'b mut Board>,
    pub source: Option<*mut Ship>,
    pub target: Option<*mut Ship>,
    pub amount: Option<i32>,
    pub extras: HashMap<String, serde_json::Value>,
}

/// Synchronous pub/sub. Concrete impl owned by the resolver / content slice;
/// the type alias lives here so [`Board`] can hold one.
///
/// The TS bus is `{ on(hook, fn), emit(hook, ctx) }`. In Rust the resolver
/// will define a struct of `Vec<Box<dyn Fn(&mut HookContext)>>` per [`Hook`]
/// (or an indexed array of `Vec`s). For now the alias is a unit type so
/// [`Board`] compiles; resolver replaces it when it lands.
#[derive(Default)]
pub struct EventBus {
    /// Placeholder. The real shape is defined by the resolver crate; this
    /// keeps `Board` instantiable in tests and from `Catalog` for now.
    _private: (),
}

/* =========================================================================
 * 8. Catalog
 * ====================================================================== */

/// The JSON payload exported by the analysis doc's "Copy JSON" button.
/// Mirrors the TS `Catalog` interface field for field. Fields that the TS
/// types as `unknown[]` are mapped to `Vec<serde_json::Value>` so they parse
/// today and can be tightened to real types later without breaking consumers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub meta: CatalogMeta,
    pub actions: Vec<Action>,
    pub mods: Vec<ModDef>,
    pub subsystems: Vec<SubsystemDef>,
    pub statuses: Vec<StatusDef>,
    pub enemies: Vec<EnemyDef>,
    #[serde(default)]
    pub capitals: Vec<serde_json::Value>,
    #[serde(default)]
    pub classes: Vec<serde_json::Value>,
    #[serde(default)]
    pub fieldkit: Vec<serde_json::Value>,
    #[serde(default)]
    pub sectors: Vec<serde_json::Value>,
    pub patrols: Vec<PatrolDef>,
    #[serde(default)]
    pub commendations: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogMeta {
    pub schema: String,
    pub lane: Vec<u32>,
    #[serde(rename = "newAxes")]
    pub new_axes: Vec<String>,
    pub bands: Vec<RangeBand>,
}

/// A weapon mod (`Action.mod` is its id). The TS shape is `{ id, name, cd, desc }`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModDef {
    pub id: String,
    pub name: String,
    /// Cooldown in turns.
    pub cd: i32,
    pub desc: String,
}

/// A status definition entry in the catalog (separate from the runtime
/// `Status` instance). TS shape is `{ id, name, effect, origin }`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StatusDef {
    pub id: String,
    pub name: String,
    pub effect: String,
    pub origin: String,
}

/// Per-patrol-tier modifier metadata. TS shape is `{ n, mod }`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PatrolDef {
    pub n: u8,
    pub r#mod: String,
}

/// Definition of an enemy ship type as it appears in the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnemyDef {
    pub id: String,
    pub name: String,
    pub hull: i32,
    /// Effective hull at Patrol 5+.
    pub hull5: i32,
    pub traits: Vec<String>,
    pub sector: String,
    pub weapons: Vec<String>,
}

/* =========================================================================
 * Tests — schema parity smoke-tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_roundtrips_through_ts_shape() {
        // Bow-on with bow=fore: the most common runtime shape.
        let bow_on = Orientation::BowOn { bow: LaneEnd::Fore };
        let s = serde_json::to_string(&bow_on).unwrap();
        assert_eq!(s, r#"{"stance":"bowOn","bow":"fore"}"#);
        let parsed: Orientation = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, bow_on);

        // Broadside: tag-only.
        let bs = Orientation::Broadside;
        let s2 = serde_json::to_string(&bs).unwrap();
        assert_eq!(s2, r#"{"stance":"broadside"}"#);
    }

    #[test]
    fn effect_damage_roundtrips_with_optional_band_falloff() {
        // With band_falloff omitted (the common case).
        let dmg = Effect::DAMAGE { amount: 4, band_falloff: None };
        let s = serde_json::to_string(&dmg).unwrap();
        assert_eq!(s, r#"{"kind":"DAMAGE","amount":4}"#);
        let parsed: Effect = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, dmg);

        // With band_falloff:false (the dummy-weapon case in resolve.ts).
        let dmg2 = Effect::DAMAGE { amount: 0, band_falloff: Some(false) };
        let s2 = serde_json::to_string(&dmg2).unwrap();
        assert_eq!(s2, r#"{"kind":"DAMAGE","amount":0,"bandFalloff":false}"#);
    }

    #[test]
    fn effect_displace_self_parses_movement_mode() {
        let json = r#"{"kind":"DISPLACE_SELF","mode":"THRUST","distance":2}"#;
        let parsed: Effect = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, Effect::DISPLACE_SELF { mode: MovementMode::THRUST, distance: 2 });
    }

    #[test]
    fn range_band_serializes_camel_case() {
        assert_eq!(serde_json::to_string(&RangeBand::PointBlank).unwrap(), r#""pointBlank""#);
        assert_eq!(serde_json::to_string(&RangeBand::Mid).unwrap(), r#""mid""#);
    }

    #[test]
    fn targeting_pattern_preserves_screaming_snake() {
        assert_eq!(serde_json::to_string(&TargetingPattern::SPINAL_LINE).unwrap(), r#""SPINAL_LINE""#);
        let p: TargetingPattern = serde_json::from_str(r#""POINT_BLANK""#).unwrap();
        assert_eq!(p, TargetingPattern::POINT_BLANK);
    }

    #[test]
    fn ship_roundtrips_with_pulse_laser_demo_shape() {
        // This is the demo.ts player shape, transliterated.
        let mut shield = HashMap::new();
        shield.insert(HullZone::Bow,       ShieldFace { armour: 2, charge: 0 });
        shield.insert(HullZone::Stern,     ShieldFace { armour: 0, charge: 0 });
        shield.insert(HullZone::Port,      ShieldFace { armour: 1, charge: 0 });
        shield.insert(HullZone::Starboard, ShieldFace { armour: 1, charge: 0 });

        let s = Ship {
            id: "frigate".into(),
            faction: Faction::Player,
            cell: 0,
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            hull: 10, max_hull: 10,
            heat: 0, heat_max: 6, locked_out: false,
            shield_profile: shield,
            mounts: vec![Mount { id: "m1".into(), arc: Arc::Forward, weapon: "pulse_laser".into() }],
            queue: vec!["pulse_laser".into()],
            cooldowns: HashMap::new(),
            statuses: vec![],
            traits: vec![],
            klass: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Ship = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
