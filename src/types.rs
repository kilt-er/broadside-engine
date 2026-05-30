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
//!
//! ## Numeric types
//!
//! TypeScript `number` (f64) is mapped to:
//! - `i32` for signed game quantities (`hull`, `heat`, `damage`, `distance`,
//!   `cooldown`, `armour`, `charge`) — these go negative mid-calculation
//!   (e.g. `target.hull -= dmg` is checked against `<= 0`).
//! - `usize` for cell indices into [`Board::cells`] (so `Vec` access is
//!   panic-or-index, not casted).
//! - `u32` for non-negative counts ([`Projectile::speed`], catalog meta
//!   lane sizes).
//! - `u8` for the patrol tier (1..=7).

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
    /// Fixed-to-hull defensive layout. The TS uses `Record<HullZone,
    /// ShieldFace>`, which is total — every zone is a required key. We mirror
    /// that with a named-field struct ([`ShieldProfile`]) so a catalog
    /// missing one of the four zones fails at parse rather than panicking
    /// later in the resolver on a `HashMap::get(...).unwrap()`. The JSON
    /// wire shape is identical to the TS object (`{ bow, stern, port,
    /// starboard }`).
    #[serde(rename = "shieldProfile")]
    pub shield_profile: ShieldProfile,
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

/// Total mapping from each [`HullZone`] to its defensive face. All four
/// zones are mandatory — this matches the TS `Record<HullZone, ShieldFace>`,
/// which is total in the TS type system. The JSON shape is the same
/// camelCase-keyed object the TS emits (`{ bow, stern, port, starboard }`),
/// so catalogs round-trip byte-for-byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShieldProfile {
    pub bow: ShieldFace,
    pub stern: ShieldFace,
    pub port: ShieldFace,
    pub starboard: ShieldFace,
}

impl ShieldProfile {
    /// Borrow a face by zone. Mirrors TS `shieldProfile[zone]`.
    pub fn face(&self, zone: HullZone) -> &ShieldFace {
        match zone {
            HullZone::Bow => &self.bow,
            HullZone::Stern => &self.stern,
            HullZone::Port => &self.port,
            HullZone::Starboard => &self.starboard,
        }
    }

    /// Mutably borrow a face by zone. Used by `absorbShield` to decrement
    /// `charge` after a hit.
    pub fn face_mut(&mut self, zone: HullZone) -> &mut ShieldFace {
        match zone {
            HullZone::Bow => &mut self.bow,
            HullZone::Stern => &mut self.stern,
            HullZone::Port => &mut self.port,
            HullZone::Starboard => &mut self.starboard,
        }
    }
}

impl std::ops::Index<HullZone> for ShieldProfile {
    type Output = ShieldFace;
    fn index(&self, zone: HullZone) -> &ShieldFace { self.face(zone) }
}

impl std::ops::IndexMut<HullZone> for ShieldProfile {
    fn index_mut(&mut self, zone: HullZone) -> &mut ShieldFace { self.face_mut(zone) }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Hull zone the status applies to. Present in the TS interface but
    /// currently unread by `resolve.ts` — `shieldsUp` is tracked via
    /// [`ShieldFace::charge`] instead, and the other `StatusKind`s ignore
    /// `face`. Mirrored here for catalog-shape parity; flagged as dead
    /// weight pending confirmation from resolver / content that nobody
    /// plans to start reading it.
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
        /// Three-valued: `None` (field absent) and `Some(true)` both apply
        /// band falloff; **band falloff is bypassed iff `Some(false)`**. Mirror
        /// of TS `bandFalloff?: boolean` at `types.ts:137`; the resolver
        /// predicate at `resolve.ts:143` is strict-equal to `false`, so the
        /// idiomatic Rust port is
        /// `matches!(band_falloff, Some(false))` — **not**
        /// `!band_falloff.unwrap_or(true)`, which collapses absent and `true`
        /// correctly but reads as if `None` is the "skip falloff" case. Also:
        /// the predicate is `effects.some(...)` in TS, so ONE damage effect
        /// on the action with `bandFalloff: false` disables falloff for the
        /// whole `applyDamage` call, not per-effect.
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
        /// Currently TODO in `resolve.ts` (`applyEffect` case at line 226);
        /// mirrored here for catalog parity. Content slice owns the actual
        /// semantics when board-wide effects land.
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
    /// after `n` salvage runs. TS shape is `number | null` (not `?:`), so
    /// `None` must round-trip as JSON `null` rather than being omitted —
    /// no `skip_serializing_if` here. `#[serde(default)]` is kept defensively
    /// in case a future catalog version drops the key entirely.
    #[serde(rename = "unlockSalvage", default)]
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

/// Context handed to a hook callback. Strongly-typed core plus an `extras`
/// bag for the TS `[k: string]: unknown` overflow. Not serialized — bus
/// state is purely runtime.
///
/// `source_cell` / `target_cell` are **lane indices into [`Board::cells`]**,
/// not raw pointers. Subscribers look up ships via the board; this mirrors
/// how every other engine path accesses ships (they live in `Board.cells`
/// keyed by their `cell` field) and avoids the aliasing UB that raw
/// `*mut Ship` would invite when the callback also has `&mut Board`. A ship
/// that was destroyed mid-callback will read as `None` at its old cell —
/// callers must handle that.
///
/// Every `bus.emit(...)` in `resolve.ts` passes `board`, so it is held by
/// reference rather than `Option`.
pub struct HookContext<'b> {
    pub board: &'b mut Board,
    pub source_cell: Option<usize>,
    pub target_cell: Option<usize>,
    pub amount: Option<i32>,
    pub extras: HashMap<String, serde_json::Value>,
}

impl<'b> HookContext<'b> {
    /// Minimal constructor — the most common emit shape (`board` only).
    pub fn new(board: &'b mut Board) -> Self {
        Self {
            board,
            source_cell: None,
            target_cell: None,
            amount: None,
            extras: HashMap::new(),
        }
    }
}

/// Synchronous pub/sub. The TS bus is `{ on(hook, fn), emit(hook, ctx) }`;
/// this is the Rust mirror.
///
/// Storage is one [`Vec`] of boxed closures per [`Hook`], indexed by the hook
/// discriminant. `emit` temporarily swaps the relevant vec out, runs each
/// callback against the [`HookContext`] (which holds `&mut Board`), and swaps
/// the vec back in — that two-step keeps Rust's aliasing rules happy when a
/// callback wants to reach back into the board through the context. A callback
/// CANNOT recursively register or invoke the same hook mid-emit; doing so
/// would silently no-op because the storage is empty during that window.
/// Resolver code never re-emits the same hook from inside a callback so this
/// is fine in practice.
///
/// Closures are `FnMut` to let subsystem state (e.g. counters) accumulate.
/// They are NOT `Send + Sync`; the renderer slice cannot move a [`Board`]
/// across threads without revisiting that.
pub struct EventBus {
    subscribers: [Vec<Box<dyn FnMut(&mut HookContext)>>; HOOK_COUNT],
}

/// Count of [`Hook`] variants. Keep in sync with the enum; the [`EventBus`]
/// storage array is sized from this constant so a missed update fails to
/// compile rather than silently dropping a hook.
const HOOK_COUNT: usize = 11;

impl Default for EventBus {
    fn default() -> Self {
        Self {
            // `Vec` is not `Copy` so we can't use the `[Vec::new(); N]` form;
            // build the array explicitly.
            subscribers: std::array::from_fn(|_| Vec::new()),
        }
    }
}

impl EventBus {
    /// Map a [`Hook`] to its slot in the [`EventBus::subscribers`] array.
    /// Order is the declaration order of the [`Hook`] enum.
    fn slot(hook: Hook) -> usize {
        match hook {
            Hook::Passive => 0,
            Hook::OnChainKill => 1,
            Hook::OnTurnEnd => 2,
            Hook::OnVent => 3,
            Hook::OnWaveStart => 4,
            Hook::OnHeatThreshold => 5,
            Hook::OnDamageDealt => 6,
            Hook::OnDamageTaken => 7,
            Hook::OnHeal => 8,
            Hook::OnReorient => 9,
            Hook::OnLethal => 10,
        }
    }

    /// Register a callback for `hook`. Mirrors TS `bus.on(hook, fn)`.
    pub fn on<F>(&mut self, hook: Hook, f: F)
    where
        F: FnMut(&mut HookContext) + 'static,
    {
        self.subscribers[Self::slot(hook)].push(Box::new(f));
    }

    /// Fire every callback registered for `hook` against `ctx`. Mirrors TS
    /// `bus.emit(hook, ctx)`. See the struct docstring for the swap-out
    /// pattern this uses to avoid aliasing UB.
    pub fn emit(&mut self, hook: Hook, ctx: &mut HookContext) {
        let slot = Self::slot(hook);
        let mut taken = std::mem::take(&mut self.subscribers[slot]);
        for cb in &mut taken {
            cb(ctx);
        }
        // Restore. If a callback (re-entrantly) registered new subscribers for
        // the SAME hook during the emit window, those land in
        // `self.subscribers[slot]`; merge them in front so the original
        // subscribers fire first on the NEXT emit.
        let appended = std::mem::take(&mut self.subscribers[slot]);
        taken.extend(appended);
        self.subscribers[slot] = taken;
    }
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModDef {
    pub id: String,
    pub name: String,
    /// Cooldown in turns.
    pub cd: i32,
    pub desc: String,
}

/// A status definition entry in the catalog (separate from the runtime
/// `Status` instance). TS shape is `{ id, name, effect, origin }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDef {
    pub id: String,
    pub name: String,
    pub effect: String,
    pub origin: String,
}

/// Per-patrol-tier modifier metadata. TS shape is `{ n, mod }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatrolDef {
    pub n: u8,
    pub r#mod: String,
}

/// Definition of an enemy ship type as it appears in the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        // This is the demo.ts player shape, transliterated. ShieldProfile is
        // now a named-field struct (M1 fix) — completeness is mandatory.
        let s = Ship {
            id: "frigate".into(),
            faction: Faction::Player,
            cell: 0,
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            hull: 10, max_hull: 10,
            heat: 0, heat_max: 6, locked_out: false,
            shield_profile: ShieldProfile {
                bow:       ShieldFace { armour: 2, charge: 0 },
                stern:     ShieldFace { armour: 0, charge: 0 },
                port:      ShieldFace { armour: 1, charge: 0 },
                starboard: ShieldFace { armour: 1, charge: 0 },
            },
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

    #[test]
    fn shield_profile_rejects_missing_zone() {
        // M1: a catalog ship missing one of the four hull zones must fail at
        // parse, not silently produce a defaultable / partial map that the
        // resolver later panics on.
        let json = r#"{"bow":{"armour":2,"charge":0},"stern":{"armour":0,"charge":0},"port":{"armour":1,"charge":0}}"#;
        let r: Result<ShieldProfile, _> = serde_json::from_str(json);
        assert!(r.is_err(), "missing 'starboard' should reject");

        // Sanity: all four present parses fine.
        let json_ok = r#"{"bow":{"armour":2,"charge":0},"stern":{"armour":0,"charge":0},"port":{"armour":1,"charge":0},"starboard":{"armour":1,"charge":0}}"#;
        let ok: ShieldProfile = serde_json::from_str(json_ok).unwrap();
        assert_eq!(ok.bow.armour, 2);
        assert_eq!(ok[HullZone::Stern].armour, 0);
    }

    #[test]
    fn shield_profile_index_mut_decrements_charge() {
        // M1 helper: resolver mutates charge via the IndexMut impl.
        let mut sp = ShieldProfile {
            bow:       ShieldFace { armour: 0, charge: 1 },
            stern:     ShieldFace { armour: 0, charge: 0 },
            port:      ShieldFace { armour: 0, charge: 0 },
            starboard: ShieldFace { armour: 0, charge: 0 },
        };
        sp[HullZone::Bow].charge -= 1;
        assert_eq!(sp.bow.charge, 0);
    }

    #[test]
    fn subsystem_def_unlock_salvage_null_roundtrips() {
        // H4: TS uses `number | null`, NOT `?:`. None must serialize as JSON
        // null (not be omitted) so the catalog round-trips byte-stable.
        let s = SubsystemDef {
            id: "marksman".into(),
            name: "Marksman".into(),
            bay: SubsystemBay::Gunnery,
            hook: Hook::OnDamageDealt,
            cost: 2,
            unlock_salvage: None,
            level: 1,
            max_level: 3,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""unlockSalvage":null"#),
            "expected unlockSalvage:null in {json}",
        );
        let back: SubsystemDef = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);

        // A missing field also parses to None (defensive default), even though
        // a strictly-conforming TS catalog should always include null.
        let json_no_field = r#"{
            "id":"marksman","name":"Marksman","bay":"gunnery","hook":"onDamageDealt",
            "cost":2,"level":1,"maxLevel":3
        }"#;
        let parsed: SubsystemDef = serde_json::from_str(json_no_field).unwrap();
        assert_eq!(parsed.unlock_salvage, None);

        // And a Some(n) round-trips as a number.
        let s_some = SubsystemDef { unlock_salvage: Some(2), ..s };
        let json2 = serde_json::to_string(&s_some).unwrap();
        assert!(json2.contains(r#""unlockSalvage":2"#));
    }

    #[test]
    fn damage_band_falloff_predicate_semantics() {
        // H1: the predicate is "bypass falloff iff Some(false)". None and
        // Some(true) BOTH apply falloff. This test pins that down so a future
        // resolver port can't drift to `band_falloff.unwrap_or(true) == false`
        // or similar without one of these assertions breaking.
        let absent = Effect::DAMAGE { amount: 4, band_falloff: None };
        let on     = Effect::DAMAGE { amount: 4, band_falloff: Some(true) };
        let off    = Effect::DAMAGE { amount: 4, band_falloff: Some(false) };

        let bypass = |e: &Effect| matches!(e, Effect::DAMAGE { band_falloff: Some(false), .. });

        assert!(!bypass(&absent), "absent => apply falloff");
        assert!(!bypass(&on),     "Some(true) => apply falloff");
        assert!( bypass(&off),    "Some(false) => bypass falloff");
    }
}
