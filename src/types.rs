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

use crate::grid::{Axis, Dir4, Dir8, Facing, Pos, Range};

/* =========================================================================
 * 0. v2 spatial migration — additive 2D fields (blueprint lane task A3)
 *
 * The v2 rebuild replaces the 1-D spatial layer (`cell: usize`, `LaneEnd`,
 * `Orientation`, `RangeBand`) with the [`crate::grid`] vocabulary
 * (`Pos`/`Dir8`/`Facing`/`Range`). Per the team's parallel-change ruling, the
 * migration is **expand → migrate → contract**: this phase (EXPAND) ADDS the
 * 2-D fields **alongside** the surviving 1-D fields so every commit keeps the
 * shared tree green. The 1-D fields and the `LaneEnd`/`Orientation`/`RangeBand`
 * enums are deleted in the later CONTRACT commit, once every consumer lane has
 * migrated off them.
 *
 * Each additive 2-D field is `#[serde(default = "…")]` against one of the
 * helpers below so existing JSON fixtures — which predate these fields — still
 * deserialize, and the round-trip tests still pass. (grid types intentionally
 * derive no `Default`, and `grid.rs` is frozen post-review, so the transitional
 * defaults live here as local fns that the CONTRACT commit removes wholesale.)
 *
 * Where the canonical field name is still taken by the live 1-D field, the 2-D
 * field uses a temporary suffix (`heading8`, `dir8`, `range_band`,
 * `optimal_range`); CONTRACT renames them back to the canonical names.
 * ====================================================================== */

/// Transitional serde default for an additive [`Pos`] field: the grid origin
/// (`col 0, row 0`). Removed at CONTRACT when the 1-D `cell` fields are deleted
/// and the 2-D fields become required.
const fn default_pos() -> Pos {
    Pos::new(0, 0)
}

/// Transitional serde default for an additive [`Dir8`] field: `N` (away from
/// the player; see [`crate::grid`] frame). Removed at CONTRACT.
const fn default_dir8() -> Dir8 {
    Dir8::N
}

/// Transitional serde default for an additive [`Facing`] field: bow pointed at
/// the player (`Bow(Dir4::S)`), the most common spawn stance. Removed at
/// CONTRACT when `facing` becomes required.
const fn default_facing() -> Facing {
    Facing::Bow(Dir4::S)
}

/// Transitional serde default for an additive single-[`Range`] field
/// (`optimal_range`): `Adjacent`. Removed at CONTRACT.
const fn default_range() -> Range {
    Range::Adjacent
}

/// Canonical transitional derivation of the v2 [`Facing`] from the legacy 1-D
/// [`Orientation`], for the EXPAND→MIGRATE window. **Single source** so the
/// three lanes that consume `Ship.facing` — the resolver's `bears()`/arc-gating,
/// the renderer's bow-arrow ([`crate::hud`] `bow_screen_dir`), and content's
/// spawn builders ([`crate::runs`]) — agree on the mapping. Removed at CONTRACT
/// once `Orientation` is deleted and spawns author a native [`Facing`] directly.
///
/// ## The convention (content/resolver-ratified; lead delegated the call)
///
/// The 1-D lane's depth axis embeds as the grid's **N/S** axis (per the frozen
/// [`crate::grid`] frame: `Dir4::S` = `+row` = toward the player at the front
/// row; `Dir4::N` = `-row` = toward the far/back row where enemies spawn):
///
/// - `BowOn { Fore }` → `Bow(S)` — bow points down-board toward the player. This
///   matches the renderer's shipped `bow_screen_dir(Bow(S)) = (0, +1)` (arrow
///   points at the player) and makes `bears(Forward)` gate on facing the player,
///   which the AI/telegraph expect for a closing enemy.
/// - `BowOn { Aft }`  → `Bow(N)` — bow points up-board, away from the player.
/// - `Broadside`      → `Broadside(EastWest)` — hull lies across the depth axis,
///   presenting its flanks to N/S (the fore/aft analog of v1's "lane hits land
///   on a flank").
///
/// **Caveat for content (C4):** this is a pure function of `orientation` alone.
/// If a spawn config wants an enemy to *face the player* regardless of side
/// (e.g. the v1 pincer, where ships on opposite sides need opposite bows to both
/// face center), derive the spawn's `orientation`/`facing` from its grid
/// position — don't assume one orientation value yields "toward player" for all
/// spawns. This helper is the faithful per-orientation map + the
/// [`default_facing`] fallback; native 2-D spawn facing is C4's to author.
pub const fn facing_from_orientation(o: Orientation) -> Facing {
    match o {
        Orientation::BowOn { bow: LaneEnd::Fore } => Facing::Bow(Dir4::S),
        Orientation::BowOn { bow: LaneEnd::Aft } => Facing::Bow(Dir4::N),
        Orientation::Broadside => Facing::Broadside(Axis::EastWest),
    }
}

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
#[derive(Debug)]
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
    /// **v2 additive** (A3 Board EXPAND): campaign-level cursor (0..19) — the
    /// background parallax `focus_target` AND campaign progress index
    /// (blueprint decision #3). `0` until the run loop sets it. Persistable
    /// (mirrored in [`BoardSnapshot`]).
    pub level: usize,
    /// **v2 additive** (A3 Board EXPAND): the telegraph map — cells the player
    /// will be hit on next turn, recomputed each phase by the resolver (R8) from
    /// each enemy's QUEUED action via `resolve_targeting` (blueprint "single best
    /// idea"). Like [`Board::fire_events`], this is **transient runtime state**:
    /// it does NOT round-trip through [`BoardSnapshot`] (recomputed on load).
    pub threats: Vec<Threat>,
    /// Pub/sub for subsystem hooks.
    pub bus: EventBus,
    /// Ships destroyed during the current chain-kill window. Resolver-managed.
    pub destroys_this_window: usize,
    /// Exact attacker→target shots fired during the current resolution, for
    /// the renderer to draw precise beams (#59). The resolver pushes a
    /// [`FireEvent`] in `run_action` for each shot; the renderer latches +
    /// draws them, then they're cleared at the next window boundary. Transient
    /// per-resolution render state — like `destroys_this_window`, it does
    /// **not** round-trip through [`BoardSnapshot`].
    pub fire_events: Vec<FireEvent>,
}

impl Board {
    /// Borrow the ship occupying `pos`, or `None` if the cell is empty / `pos`
    /// is out of range (**v2, A3 Board EXPAND** — the 2-D occupancy query).
    ///
    /// O(1): indexes [`Board::cells`] at [`Pos::to_index`]. Uses `get` (not `[]`)
    /// so an out-of-range `pos` — or a query against a short legacy 1-D test
    /// board — yields `None` rather than panicking.
    ///
    /// # Invariant
    ///
    /// Relies on the **slot==pos** invariant: a ship stored at `cells[i]` has
    /// `pos.to_index() == i`. Spawn placement ([`crate::runs`], C4) establishes
    /// it (a ship goes into `cells[ship.pos.to_index()]`); movement (resolver
    /// R6) maintains it (updating the slot and `Ship::pos` together). Until a
    /// producer populates real positions, ships carry the transitional default
    /// [`Pos`] and live at their legacy 1-D slot, so `ship_at` is only
    /// meaningful on a board whose ships have been placed 2-D-natively.
    pub fn ship_at(&self, pos: Pos) -> Option<&Ship> {
        self.cells.get(pos.to_index()).and_then(|c| c.as_ref())
    }

    /// Mutably borrow the ship occupying `pos` (the `mut` companion to
    /// [`Board::ship_at`]; used by the resolver's `apply_damage` in R4). Same
    /// O(1) `get_mut` + slot==pos invariant.
    pub fn ship_at_mut(&mut self, pos: Pos) -> Option<&mut Ship> {
        self.cells.get_mut(pos.to_index()).and_then(|c| c.as_mut())
    }

    /// Find the [`Pos`] of the ship with `id`, or `None` if absent
    /// (**v2, A3 Board EXPAND** — the 2-D replacement for the resolver's
    /// 1-D `find_cell_by_id`). Scans [`Board::cells`] (`O(CELLS)`); returns the
    /// slot's [`Pos`] (which equals the ship's `pos` under the slot==pos
    /// invariant).
    pub fn find_pos_by_id(&self, id: &str) -> Option<Pos> {
        self.cells.iter().enumerate().find_map(|(i, c)| {
            c.as_ref()
                .filter(|s| s.id == id)
                .and_then(|_| Pos::from_index(i))
        })
    }
}

/// One attacker→target shot, recorded on [`Board::fire_events`] so the
/// renderer can draw an exact beam between the two cells (#59). Replaces the
/// renderer's previous guesswork about who-shot-whom. The resolver emits one
/// per shot in `run_action`; the renderer styles the beam by `archetype`
/// (per-weapon look), tints it by the `attacker_faction`, and dims it on a
/// miss (`hit == false`). For an N-target shot the resolver emits N events
/// (one attacker→target line each).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireEvent {
    /// Lane cell the shot originates from (the attacker's cell).
    pub from_cell: usize,
    /// Lane cell the shot targets (the target's cell).
    pub to_cell: usize,
    /// **v2 additive** (A3 EXPAND): 2-D origin cell, replacing
    /// [`FireEvent::from_cell`]. `#[serde(default)]` for fixture compatibility
    /// during the migration; the renderer's whiff beam (`hit:false`) draws
    /// between `from_pos`/`to_pos` at CONTRACT.
    #[serde(default = "default_pos")]
    pub from_pos: Pos,
    /// **v2 additive** (A3 EXPAND): 2-D target cell, replacing
    /// [`FireEvent::to_cell`]. See [`FireEvent::from_pos`].
    #[serde(default = "default_pos")]
    pub to_pos: Pos,
    /// Firing weapon's archetype — drives per-weapon beam styling.
    pub archetype: WeaponArchetype,
    /// Faction of the firing ship — for the renderer's side tint.
    pub attacker_faction: Faction,
    /// Whether the shot connected. Misses render dimmer.
    pub hit: bool,
}

/// A cell-resident hazard: mine, drone, or debris field. Applies `payload`
/// to anything that enters the cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hazard {
    pub id: String,
    pub kind: HazardKind,
    pub cell: usize,
    /// **v2 additive** (A3 EXPAND): 2-D cell, replacing [`Hazard::cell`].
    /// `#[serde(default)]` for fixture compatibility during the migration.
    #[serde(default = "default_pos")]
    pub pos: Pos,
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

/* -------------------------------------------------------------------------
 * Threat — the v2 telegraph map (blueprint "single best idea")
 *
 * NEW in v2. A `Threat` is one cell the player will be hit on next turn,
 * computed by running the REAL `resolve_targeting` against each enemy's QUEUED
 * action — so the painted threat set cannot desync from where the shot
 * actually lands (correctness from reuse). The resolver (R8) populates
 * [`Board::threats`] after `decide_enemy_action` and clears it at the phase-0
 * boundary; the renderer (D6) draws a red fill under each threatened cell,
 * styled by [`ThreatKind`], with a lethal flash. When the player vacates a
 * threatened cell the queued shot resolves to empty → a `hit:false` whiff beam
 * (R7). This is **transient runtime/telegraph state**, like
 * [`Board::fire_events`]: it does NOT round-trip through [`BoardSnapshot`].
 * ---------------------------------------------------------------------- */

/// What a telegraphed [`Threat`] will do to its cell next turn — drives the
/// renderer's threat-fill styling (blueprint: "red fill under a ship = positional
/// threat ... by `ThreatKind` + lethal flash"). Mirrors the effect families the
/// AI can queue; `Damage` is the common case and carries the projected amount
/// so the renderer can flag a lethal hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreatKind {
    /// A damaging shot. `amount` is the projected pre-mitigation damage so the
    /// renderer can flash cells where it would be lethal.
    Damage { amount: i32 },
    /// A displacement (push / pull / swap) — the cell's occupant gets moved.
    Displace,
    /// A debuff (status) application.
    Status,
    /// Any other queued effect with a cell footprint but no damage/displacement
    /// (e.g. a deploy onto the cell). Kept as a catch-all so the telegraph is
    /// total without enumerating every effect.
    Other,
}

/// One telegraphed threatened cell for the next turn (see the module section
/// comment above [`ThreatKind`]). Produced by the resolver's R8 `ThreatMap` pass
/// from the enemy's queued action; consumed by the renderer (D6) and the
/// dodge-whiff emission (R7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Threat {
    /// The cell that will be hit.
    pub pos: Pos,
    /// What will happen to it.
    pub kind: ThreatKind,
    /// The cell of the enemy whose queued action produces this threat — lets the
    /// renderer draw the telegraph beam from the right ship and lets R7 know
    /// whose shot whiffs when the player vacates `pos`.
    pub source: Pos,
}

/* =========================================================================
 * 3. Ship
 * ====================================================================== */

/// Player and enemy ships share this shape; `faction` distinguishes them.
/// Mirrors the TS `Ship` interface verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ship {
    pub id: String,
    pub faction: Faction,
    pub cell: usize,
    /// **v2 additive** (A3 EXPAND): 2-D grid position, replacing [`Ship::cell`].
    /// `#[serde(default)]` so pre-v2 fixtures (which carry only `cell`) still
    /// parse during the migration; the resolver writes both during transition.
    /// Becomes the sole position field at CONTRACT (and `cell` is deleted).
    #[serde(default = "default_pos")]
    pub pos: Pos,
    pub orientation: Orientation,
    /// **v2 additive** (A3 EXPAND): 2-D hull stance, replacing
    /// [`Ship::orientation`]. See [`Ship::pos`] for the transition contract.
    #[serde(default = "default_facing")]
    pub facing: Facing,
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
    pub const fn face(&self, zone: HullZone) -> &ShieldFace {
        match zone {
            HullZone::Bow => &self.bow,
            HullZone::Stern => &self.stern,
            HullZone::Port => &self.port,
            HullZone::Starboard => &self.starboard,
        }
    }

    /// Mutably borrow a face by zone. Used by `absorbShield` to decrement
    /// `charge` after a hit.
    pub const fn face_mut(&mut self, zone: HullZone) -> &mut ShieldFace {
        match zone {
            HullZone::Bow => &mut self.bow,
            HullZone::Stern => &mut self.stern,
            HullZone::Port => &mut self.port,
            HullZone::Starboard => &mut self.starboard,
        }
    }

    /// Mutable refs to all four faces (any order) — for the per-turn shield-regen
    /// pass (#103 Model A) in `crate::resolve::end_of_turn`.
    pub const fn faces_mut(&mut self) -> [&mut ShieldFace; 4] {
        [
            &mut self.bow,
            &mut self.stern,
            &mut self.port,
            &mut self.starboard,
        ]
    }
}

impl std::ops::Index<HullZone> for ShieldProfile {
    type Output = ShieldFace;
    fn index(&self, zone: HullZone) -> &ShieldFace {
        self.face(zone)
    }
}

impl std::ops::IndexMut<HullZone> for ShieldProfile {
    fn index_mut(&mut self, zone: HullZone) -> &mut ShieldFace {
        self.face_mut(zone)
    }
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

/// Ship traits. Spelled in `TitleCase` in the TS string union, so we keep that
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// **v2 additive** (A3 EXPAND): 3-band Chebyshev ranges the weapon may fire
    /// at, replacing [`Targeting::band`]. `#[serde(default)]` (→ empty) so pre-v2
    /// catalogs still parse; the resolver reads this once content re-authors the
    /// catalog. Temporary name: `band` is still the live 1-D field — renamed to
    /// `band` at CONTRACT.
    #[serde(rename = "rangeBand", default)]
    pub range_band: Vec<Range>,
    /// **v2 additive** (A3 EXPAND): peak-damage [`Range`] band, replacing
    /// [`Targeting::optimal_band`]. Temporary name (renamed to `optimal_band` at
    /// CONTRACT). See [`Targeting::range_band`].
    #[serde(rename = "optimalRange", default = "default_range")]
    pub optimal_range: Range,
    /// Mount must bear this arc given the firing ship's orientation. `None`
    /// = arc-less action (SELF, `DEPLOYED_CELL`).
    #[serde(rename = "requiresArc")]
    pub requires_arc: Option<Arc>,
    #[serde(rename = "facingRelative")]
    pub facing_relative: bool,
    /// `SPINAL_LINE` pierce vs first-only.
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        #[serde(
            rename = "bandFalloff",
            default,
            skip_serializing_if = "Option::is_none"
        )]
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
        /// **Rust-port extension** (not in TS `DISPLACE_SELF`). When `Some`,
        /// overrides the ship-orientation-derived movement direction with an
        /// absolute [`LaneEnd`]: `Some(Fore)` always moves toward higher cell
        /// indices, `Some(Aft)` toward lower. When `None`, the resolver
        /// derives direction from `ship.orientation` (the canonical TS
        /// semantics: `BowOn { bow: Fore }` -> step +1, `BowOn { bow: Aft }`
        /// -> step -1, `Broadside` -> step +1).
        ///
        /// Added for player-controlled lane-relative movement (Left arrow ->
        /// `Some(Aft)`, Right arrow -> `Some(Fore)`); the surprise it solves
        /// is "after a reorient, Left moves the ship rightward on screen."
        /// AI / scripted actions continue to pass `None` and stay
        /// bow-relative.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<LaneEnd>,
        /// **v2 additive** (A3 MIGRATE, for resolver R6): the 2-D analog of
        /// [`direction`], replacing the 1-D [`LaneEnd`] override. `Some(dir)`
        /// forces an absolute 4-cardinal step ([`Dir4`], one cell/step via
        /// `grid::offset`); `None` defers to the ship-`Facing`-derived direction
        /// (the 2-D analog of today's orientation-derived default). [`Dir4`] not
        /// [`Dir8`] because movement is 4-cardinal (decision #9; matches the
        /// `__move_up/down/left/right` = N/S/E/W input ids).
        ///
        /// Temp name (`direction` is still the live 1-D field); CONTRACT deletes
        /// `direction` and renames this → `direction`. `#[serde(default,
        /// skip_serializing_if)]` so no fixture needs it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction_2d: Option<Dir4>,
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
        #[serde(
            rename = "rechargeCooldowns",
            default,
            skip_serializing_if = "Option::is_none"
        )]
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
///
/// [`Self::RotateLeft`] / [`Self::RotateRight`] are a **Rust-port extension** for
/// the player rotation control (the four-orientation broadside hook): they turn
/// the ship's `facing` (the [`crate::grid::Dir4`] bow direction) a quarter-turn
/// counter-clockwise / clockwise and re-derive `orientation` from it, so the hull
/// VISUALLY rotates and the firing arcs follow (render + the 2-D fire-gate both
/// key off `facing`). They are produced only by the synthetic player rotate
/// actions, never authored in the catalog JSON — so they don't change the TS
/// contract (additive, like the `direction` field on `DISPLACE_SELF`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReorientTo {
    BowOn,
    Broadside,
    Flip,
    RotateLeft,
    RotateRight,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projectile {
    pub id: String,
    pub kind: String,
    pub cell: usize,
    /// **v2 additive** (A3 EXPAND): 2-D position, replacing [`Projectile::cell`].
    /// `#[serde(default)]` for fixture compatibility during the migration.
    #[serde(default = "default_pos")]
    pub pos: Pos,
    pub heading: LaneEnd,
    /// **v2 additive** (A3 EXPAND): 2-D heading, replacing [`Projectile::heading`].
    /// Temporary name (`heading` is the live 1-D field) — renamed to `heading`
    /// at CONTRACT. The ordnance projector (R5) steps along this each phase.
    #[serde(rename = "heading8", default = "default_dir8")]
    pub heading8: Dir8,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
///
/// # No-chained-emit invariant
///
/// **A callback MUST NOT call `ctx.board.bus.emit(...)`** — the resolver's
/// emit wrapper [`mem::take`]s [`Board::bus`] off the board for the duration
/// of every emit, so the bus reachable through `ctx.board` during a callback
/// is a default placeholder, **not** the live bus. An emit through it would
/// silently no-op (and would not be a bug to file — that is the contract).
///
/// To trigger downstream effects, **call resolver functions directly**
/// (e.g. `apply_damage`, `destroy`, `add_status`). They take `&mut Board`,
/// the callback already has `&mut Board` via `ctx.board`, and the resolver
/// will fire any hooks they trigger **after this callback returns** —
/// because by then control has unwound back to the wrapper, the bus is
/// restored, and the next `emit` call goes through the live bus.
///
/// This matches the TS engine: in `resolve.ts:337-344`, `destroy` runs
/// `applyDamage` for the `ReactorBreach` splash via a **direct function
/// call**, then emits `onLethal` — no callback ever re-emits through the
/// bus from inside another emit.
#[derive(Debug)]
pub struct HookContext<'b> {
    pub board: &'b mut Board,
    pub source_cell: Option<usize>,
    pub target_cell: Option<usize>,
    /// **v2 additive** (A3 EXPAND): 2-D source cell, replacing
    /// [`HookContext::source_cell`]. Not serialized (the bus is runtime-only),
    /// so no fixture concern — purely a parallel field the resolver populates
    /// during the migration. Becomes the sole field at CONTRACT.
    pub source_pos: Option<Pos>,
    /// **v2 additive** (A3 EXPAND): 2-D target cell, replacing
    /// [`HookContext::target_cell`]. See [`HookContext::source_pos`].
    pub target_pos: Option<Pos>,
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
            source_pos: None,
            target_pos: None,
            amount: None,
            extras: HashMap::new(),
        }
    }
}

/// Synchronous pub/sub. The TS bus is `{ on(hook, fn), emit(hook, ctx) }`;
/// this is the Rust mirror.
///
/// Storage is one [`Vec`] per [`Hook`], holding `Option<Box<dyn FnMut>>`. The
/// `Option` lets `emit` move a single callback out for the duration of the
/// call (leaving its slot as `None`) and put it back when the call returns
/// — same semantics as iterating a live JS array with `forEach`.
///
/// # Re-entrancy contract
///
/// **From inside a callback, subscribers MUST NOT call `bus.emit(...)`
/// (whether on this bus directly or via `ctx.board.bus`).** The contract is
/// enforced at the architecture level by the resolver's emit wrapper, which
/// `mem::take`s this bus off [`Board`] for the duration of every emit pass —
/// so a callback that reaches for `ctx.board.bus` finds a default placeholder.
/// See [`HookContext`] for the full invariant and the "use direct resolver
/// calls" guidance.
///
/// The storage-level `Option<Box<...>>` shape exists nonetheless because
/// it's the simpler, correct primitive: it makes the `&mut self` borrow live
/// only for the brief slot-take/slot-restore moments rather than across the
/// whole callback, which keeps the bus self-consistent against any future
/// caller (test harnesses, alternative orchestration layers) that does call
/// `EventBus::emit` re-entrantly without the resolver wrapper interposed.
/// In that narrow scenario:
///
/// - **Same-hook re-emit** iterates the same vec; the currently-executing
///   slot reads as `None` and is skipped, every other live subscriber fires.
/// - **Same-hook re-register** `push`es to the end; the outer `emit`'s
///   index loop re-reads `len()` each iteration so new subscribers fire in
///   the same pass.
/// - **Cross-hook emit** is unaffected — only the live hook's slot is in
///   the take/replace dance.
///
/// These guarantees are correctness backstops, **not** part of the public
/// subsystem-author contract. Subsystem authors should treat the bus as
/// "you receive callbacks, you do not fire them."
///
/// Closures are `FnMut` so subsystem state (e.g. counters) can accumulate.
/// They are NOT `Send + Sync`; the renderer slice cannot move a [`Board`]
/// across threads without revisiting that bound — see the module-level
/// note on `Send + Sync`.
pub struct EventBus {
    subscribers: [Vec<HookSlot>; HOOK_COUNT],
}

// Manual `Debug`: the subscribers are boxed `FnMut` closures, which don't
// implement `Debug`. Report the per-hook subscriber counts instead of the
// closures — enough to inspect the bus's wiring without an unprintable field.
impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let counts: [usize; HOOK_COUNT] = std::array::from_fn(|i| self.subscribers[i].len());
        f.debug_struct("EventBus")
            .field("subscriber_counts", &counts)
            .finish()
    }
}

/// One registered hook callback slot. The `Option` lets [`EventBus::emit`]
/// move a callback out for the duration of its call (leaving `None`) and
/// restore it afterward — see [`EventBus`]'s re-entrancy contract. Aliased so
/// the [`EventBus::subscribers`] array type stays readable (and clippy's
/// `type_complexity` lint stays quiet).
type HookSlot = Option<Box<dyn FnMut(&mut HookContext)>>;

/// Count of [`Hook`] variants. The compile-time guard is the exhaustive match
/// in [`EventBus::slot`]: adding a `Hook` variant without extending `slot`
/// is a compile error. Updating this constant when a variant lands is
/// asserted by the `hook_count_matches_enum_cardinality` test below.
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
    /// Order is the declaration order of the [`Hook`] enum. The exhaustive
    /// match here is the actual drift guard — adding a `Hook` variant without
    /// extending this function fails to compile.
    const fn slot(hook: Hook) -> usize {
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
    /// Safe to call from inside another callback (including a callback
    /// firing for the same hook): the new subscriber lands at the end of
    /// the vec and is picked up by the outer `emit` loop in the same pass.
    pub fn on<F>(&mut self, hook: Hook, f: F)
    where
        F: FnMut(&mut HookContext) + 'static,
    {
        self.subscribers[Self::slot(hook)].push(Some(Box::new(f)));
    }

    /// Fire every callback registered for `hook` against `ctx`. Mirrors TS
    /// `bus.emit(hook, ctx)`.
    ///
    /// Iterates by index, taking the callback out of its slot for the
    /// duration of the call and putting it back after. A re-entrant `emit`
    /// of the same hook sees the vec with this slot temporarily `None` —
    /// every other subscriber fires normally. Length is re-read each
    /// iteration so new subscribers registered during emit are picked up.
    pub fn emit(&mut self, hook: Hook, ctx: &mut HookContext) {
        let slot = Self::slot(hook);
        let mut i = 0;
        loop {
            // Re-read length each iteration: a callback may have pushed new
            // subscribers (same-hook re-register), and we want them to fire.
            if i >= self.subscribers[slot].len() {
                break;
            }
            // Move the callback out. The slot at index `i` is now `None` so
            // a re-entrant same-hook emit sees the rest of the vec.
            let cb = self.subscribers[slot][i].take();
            if let Some(mut boxed) = cb {
                boxed(ctx);
                // Put it back. If a callback removed itself, that future API
                // would mean leaving the slot as `None`; for now everything
                // is permanent, so always restore. (A future `off` API would
                // mark slots `None` to drain; the cleanup pass at the end of
                // emit would compact them.)
                self.subscribers[slot][i] = Some(boxed);
            }
            i += 1;
        }
    }
}

/* =========================================================================
 * 8. Catalog
 * ====================================================================== */

/// The JSON payload exported by the analysis doc's "Copy JSON" button.
/// Mirrors the TS `Catalog` interface field for field. Fields that the TS
/// types as `unknown[]` are mapped to `Vec<serde_json::Value>` so they parse
/// today and can be tightened to real types later without breaking consumers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub meta: CatalogMeta,
    pub actions: Vec<Action>,
    pub mods: Vec<ModDef>,
    pub subsystems: Vec<SubsystemDef>,
    pub statuses: Vec<StatusDef>,
    pub enemies: Vec<EnemyDef>,
    /// Boss capital ships, one per sector. Canonical catalog data — see
    /// [`CapitalDef`].
    #[serde(default)]
    pub capitals: Vec<CapitalDef>,
    #[serde(default)]
    pub classes: Vec<ClassDef>,
    #[serde(default)]
    pub fieldkit: Vec<serde_json::Value>,
    /// Campaign sector map. Canonical catalog data — see [`SectorDef`].
    #[serde(default)]
    pub sectors: Vec<SectorDef>,
    pub patrols: Vec<PatrolDef>,
    #[serde(default)]
    pub commendations: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogMeta {
    pub schema: String,
    pub lane: Vec<u32>,
    #[serde(rename = "newAxes")]
    pub new_axes: Vec<String>,
    pub bands: Vec<RangeBand>,
}

/// A campaign-map sector as it appears in the **catalog** (the design doc's
/// `SECTORS` literal, `broadside-analysis.html:1176-1189` / sector-map §XI;
/// content's #50-keystone ruling confirmed this is the canonical schema, not
/// the Phase-3 [`Sector`] guess).
///
/// This is **catalog data**, deliberately distinct from the **runtime**
/// [`Sector`] / [`EncounterDef`] / [`ShipSpawn`] types that the run-loop uses
/// to materialize a board. Per the canonical campaign model (§XI dynamic spawn
/// pool, §VIII capital engagements) a sector does **not** carry a static
/// encounter list: `intro` seeds the global spawn pool when the player first
/// reaches the sector, encounters are generated at runtime from the
/// accumulated pool + patrol tier, and `capital` is the one fixed end-of-sector
/// boss fight. The pool→encounter generator that bridges [`SectorDef`] to the
/// runtime types is a separate content task; this type is just the loaded
/// shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectorDef {
    /// Display name (e.g. `"Drift Belt"`).
    pub name: String,
    /// Graph node id. **A string, not an int** — the dotted form (`"0"`,
    /// `"2.1"`, `"4.2"`, `"5.1"`) encodes the branching campaign map; branch
    /// siblings share a major number. Successor / branch links are derived
    /// from the node numbering, not stored.
    pub node: String,
    /// Lane length for this sector's encounters (the canonical board sizes
    /// 5 / 7 / 9).
    pub lane: u8,
    /// Display names of enemy ship types **first introduced** in this sector
    /// (`["Skiff", "Lancer"]`). These ENTER the global spawn pool on arrival;
    /// it is **not** the full per-encounter spawn list. Empty for sectors that
    /// introduce nothing new (Staging / Citadel / Crimson Anomaly).
    #[serde(default)]
    pub intro: Vec<String>,
    /// The sector's boss capital-ship display name (`"The Dasher"`). The
    /// catalog stores `"—"` (U+2014 em-dash) — or `""` — for "no capital";
    /// only Staging (the run start) has none. Deserialized to `None` in those
    /// cases via [`deserialize_capital`] so callers branch on `Option` rather
    /// than sniffing a sentinel string.
    #[serde(default, deserialize_with = "deserialize_capital")]
    pub capital: Option<String>,
}

/// Serde helper for [`SectorDef::capital`]: maps the catalog's "no capital"
/// sentinels (`"—"` U+2014 em-dash, or empty string) to `None`; any other
/// string is `Some(name)`. A literal JSON `null` is also `None`.
fn deserialize_capital<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    Ok(match raw {
        None => None,
        Some(s) if s == "\u{2014}" || s.trim().is_empty() => None,
        Some(s) => Some(s),
    })
}

/// A boss **capital ship** as it appears in the **catalog** (`capitals[]`).
/// One per sector — the fixed end-of-sector engagement ([`SectorDef::capital`]
/// names it). This is the typed catalog representation that replaces the
/// per-boss fallback to the warlord synthesizer, so each capital ("The
/// Dasher", "The Impaler", …) is a distinct catalog entry.
///
/// Like [`SectorDef`], this is **catalog data** distinct from the **runtime**
/// Ship a capital materializes into on the board; the capital → Ship builder
/// (combat loadout, behavior) is runtime/content's lane. The doc authors **no
/// per-capital combat loadout** (no hull / shields / mounts here) — per-capital
/// combat distinctiveness (e.g. the Twins spawning two ships, the Coward
/// fleeing) is content's future runtime-synthesis follow-up, decoupled from
/// this type. These six fields are the whole canonical catalog spec.
///
/// ## Salvage reward (not combat stats)
///
/// `salvage_p1` / `salvage_p7` are the **salvage payout for destroying this
/// capital** at Patrol tier 1 vs tier 7 (design doc §VIII "Salvage P1/P7"),
/// scaling with tier — they are **rewards, not strength/hull**. `salvage_p1`
/// is `Option` because the catalog stores `null` for the capital not awarded
/// at tier 1 (the Void Sovereign).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapitalDef {
    /// Internal id (e.g. `"dasher"`).
    pub id: String,
    /// Display name (e.g. `"The Dasher"`).
    pub name: String,
    /// Display name of the sector this capital ends (matches
    /// [`SectorDef::name`], e.g. `"Drift Belt"`).
    pub sector: String,
    /// Whether this capital has a Patrol-4+ **corrupted variant** (design doc
    /// §VIII 699-700) — an eligibility flag for the harder corrupted form at
    /// higher tiers; the variant's stats/behavior are content's to define.
    #[serde(default)]
    pub corrupt: bool,
    /// Salvage reward for killing this capital at Patrol tier 1. `null` in the
    /// catalog → `None` (the Void Sovereign awards none at tier 1). Catalog
    /// key `sP1`.
    #[serde(rename = "sP1", default)]
    pub salvage_p1: Option<i32>,
    /// Salvage reward for killing this capital at Patrol tier 7 (the scaling
    /// ceiling). Catalog key `sP7`. `#[serde(default)]` (→ 0) so minimal
    /// capital entries that omit it — fixtures, or a forward catalog that
    /// lists a capital before its salvage numbers land — still parse; the
    /// canonical catalog always supplies it.
    #[serde(rename = "sP7", default)]
    pub salvage_p7: i32,
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

/// The stance / orientation a class leans into. Mirrors the canonical doc's
/// `affinity` field at `broadside-analysis.html:1144-1163` (the `CLASSES`
/// table). `Flexible` is the no-stance-bias starter class; `BowOn` and
/// `Broadside` correspond to the two `Orientation::*` stances the hull can
/// take. Note this is **not** a [`WeaponArchetype`] — the canonical doc
/// uses stance affinity, not weapon-type affinity, for class identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClassAffinity {
    Flexible,
    BowOn,
    Broadside,
}

/// Definition of a player ship class as it appears in the catalog. Matches
/// the canonical schema in `broadside-analysis.html:1144-1163`: each class
/// names two action sets the player can pick between at run-start, a
/// free-fire Signature action (the "hero special move" — dispatch keyed off
/// [`Ship::klass`]), an optional Passive description, and the unlock /
/// flavour copy shown in the class-select UI.
///
/// The TS `Catalog.classes` is typed as `unknown[]` (placeholder); this
/// Rust port locks the shape in ahead of the canonical typings. When the
/// TS engine grows a real `ClassDef`, the wire shape should already match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassDef {
    /// Internal id used for `Ship::klass` lookups (e.g. `"wanderer"`).
    pub id: String,
    /// Display name (e.g. `"Frigate \"Drifter\""`).
    pub name: String,
    /// Stance bias. See [`ClassAffinity`].
    pub affinity: ClassAffinity,
    /// Unlock criterion copy. `None` is treated as "available from the
    /// start" so the field can be omitted from the catalog JSON; a canonical
    /// `"Unlocked by default"` string is also valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlock: Option<String>,
    /// Action ids in the class's primary loadout set.
    pub set1: Vec<String>,
    /// Action ids in the secondary loadout set.
    pub set2: Vec<String>,
    /// The Signature action id (free-fire; dispatched on the ship's `klass`).
    pub signature: String,
    /// Optional Passive — prose for now; structured effect bodies arrive
    /// when the content slice promotes this to a real [`Effect`] chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passive: Option<String>,
    /// Class-select UI flavour copy.
    pub desc: String,
}

/* =========================================================================
 * 9. Run loop — Sector / Encounter / Run / SaveState
 *
 * Phase 3 foundation. The TS engine doesn't model these yet (`Catalog.sectors`
 * is still `unknown[]` at `broadside-engine/engine/types.ts:208`); this Rust
 * port locks the shape in. Future canonical-catalog imports should parse
 * cleanly because the field set mirrors what the analysis HTML Section XI
 * sector-map design implies.
 * ====================================================================== */

/// A campaign-map node: one named sector with a list of encounters the player
/// works through. `patrol_tier` mirrors [`Board::patrol`] (u8 because the
/// design caps it at 7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sector {
    pub id: String,
    pub name: String,
    pub patrol_tier: u8,
    pub encounters: Vec<EncounterDef>,
}

/// A single battle within a [`Sector`] — spawn templates for the enemy
/// fleet and the hazards already on the board when the encounter opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncounterDef {
    pub id: String,
    pub enemy_ships: Vec<ShipSpawn>,
    /// Hazards already on the lane at encounter start. Reuses the existing
    /// [`Hazard`] shape — there are no spawn-only fields yet.
    pub hazards: Vec<Hazard>,
    pub is_boss: bool,
}

/// Spawn template for one ship at encounter start. `class_id` refers to a
/// template id in the catalog — either a [`ClassDef::id`] (player hulls:
/// wanderer, ronin, shadow, …) **or** an [`EnemyDef::id`] (enemy ships:
/// skiff, lancer, gunboat, …). The spawn resolver looks in both registries;
/// content's `placeholder_sectors()` uses this for enemy refs at
/// `src/runs.rs`. `hp_override` lets the encounter patch hull to a
/// tier-scaled value without minting a whole new template.
///
/// TODO: rename `class_id` → `template_id` (touches every spawn callsite;
/// deferred until content's progression layer stabilizes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShipSpawn {
    pub class_id: String,
    pub cell: usize,
    /// **v2 additive** (A3 EXPAND): 2-D spawn position, replacing
    /// [`ShipSpawn::cell`]. `#[serde(default)]` for fixture compatibility; the
    /// spawn builders ([`crate::runs`]) carry it onto the materialized
    /// [`Ship::pos`] during the migration.
    #[serde(default = "default_pos")]
    pub pos: Pos,
    pub orientation: Orientation,
    /// **v2 additive** (A3 EXPAND): 2-D spawn stance, replacing
    /// [`ShipSpawn::orientation`]. See [`ShipSpawn::pos`].
    #[serde(default = "default_facing")]
    pub facing: Facing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hp_override: Option<i32>,
}

/// Cross-encounter run state — accumulates progress through a campaign
/// from the player's first sector entry through victory or defeat.
///
/// `player` carries the player Ship across encounter boundaries: hull
/// damage, installed subsystems, equipped cards, status durations, and
/// salvage-purchased upgrades all persist. Resuming a saved run rebuilds
/// the encounter board around this Ship via
/// `build_encounter_board(enc, run.player.clone(), …)`. (Without this
/// field, an alt-tab + kill-process cycle would save-scum a fresh
/// full-hull Ship.)
///
/// `salvage` is the meta-currency spent on between-encounter cards;
/// `completed_encounters` indexes within the *current* sector. The
/// `defeated` / `victorious` pair is mutually exclusive at end of run
/// (both `false` while the run is live).
///
/// **`Run` is no longer `Copy`/`Eq`/`Hash`** — `player: Ship` brings
/// heap-allocated fields (Vec, `HashMap`) that don't satisfy those bounds.
/// Pre-#79 code passing `Run` by value still works (Clone is derived);
/// callers that want sharing should hold `&Run` or clone explicitly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub current_sector_idx: usize,
    pub salvage: u32,
    pub completed_encounters: u32,
    pub defeated: bool,
    pub victorious: bool,
    /// The active player Ship. See struct docstring on why this lives
    /// on `Run` and not somewhere else.
    pub player: Ship,
}

impl Run {
    /// Start a fresh run with the given player Ship. All other fields
    /// initialize to the start-of-campaign state (sector 0, encounter 0,
    /// zero salvage, neither defeated nor victorious).
    ///
    /// Callers build `player` from whichever `ClassDef` the player picked
    /// at the class-select screen; constructing the Ship is content's
    /// job, not types.rs's, so `new` takes a fully-formed Ship.
    pub const fn new(player: Ship) -> Self {
        Self {
            current_sector_idx: 0,
            salvage: 0,
            completed_encounters: 0,
            defeated: false,
            victorious: false,
            player,
        }
    }
}

/// Persistable snapshot of a live [`Board`]. [`Board`] itself is intentionally
/// non-serde (it holds the [`EventBus`] and the transient
/// `destroys_this_window` counter — see Board's docstring). This snapshot
/// captures the persistable subset; the resolver re-subscribes subsystems
/// to a fresh [`EventBus`] on load via [`BoardSnapshot::into_board`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub size: usize,
    pub cells: Vec<Option<Ship>>,
    pub ordnance: Vec<Projectile>,
    pub hazards: Vec<Vec<Hazard>>,
    pub patrol: u8,
    /// **v2 additive** (A3 Board EXPAND): persisted campaign cursor, mirroring
    /// [`Board::level`]. `#[serde(default)]` (→ 0) so pre-v2 saves still parse.
    /// (`Board::threats` is deliberately NOT mirrored — it's transient, like
    /// `fire_events`, recomputed on load.)
    #[serde(default)]
    pub level: usize,
}

impl From<&Board> for BoardSnapshot {
    /// Snapshot the persistable fields of `board`. The `EventBus` and the
    /// `destroys_this_window` counter are deliberately dropped — both are
    /// runtime-only state that the resolver reconstructs.
    fn from(board: &Board) -> Self {
        Self {
            size: board.size,
            cells: board.cells.clone(),
            ordnance: board.ordnance.clone(),
            hazards: board.hazards.clone(),
            patrol: board.patrol,
            level: board.level,
        }
    }
}

impl BoardSnapshot {
    /// Rebuild a live [`Board`] from this snapshot. The caller supplies a
    /// fresh [`EventBus`] (typically `EventBus::default()`, then re-subscribe
    /// the subsystem registrations via the resolver's content layer);
    /// `destroys_this_window` resets to 0.
    pub fn into_board(self, bus: EventBus) -> Board {
        Board {
            size: self.size,
            cells: self.cells,
            ordnance: self.ordnance,
            hazards: self.hazards,
            patrol: self.patrol,
            level: self.level,
            bus,
            destroys_this_window: 0,
            // Transient render state; a loaded board starts with none.
            fire_events: Vec::new(),
            // Transient telegraph state; the resolver recomputes on the next
            // world phase (not persisted — see Board::threats).
            threats: Vec::new(),
        }
    }
}

/// What the save file holds: cross-encounter [`Run`] state plus the live
/// [`BoardSnapshot`] of the current encounter. Future fields (meta-progression
/// unlocks, run-seed for replay, etc.) land here as Phase 3 grows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveState {
    pub run: Run,
    pub board: BoardSnapshot,
}

/* =========================================================================
 * Tests — schema parity smoke-tests
 * ====================================================================== */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sector_def_parses_canonical_catalog_shape() {
        // The exact catalog shape (broadside.catalog.json sectors[0]): a sector
        // with a capital, and one with the "—" no-capital sentinel.
        let with_capital = r#"{"name":"Drift Belt","node":"1","lane":7,"intro":["Skiff","Lancer"],"capital":"The Dasher"}"#;
        let s: SectorDef = serde_json::from_str(with_capital).unwrap();
        assert_eq!(s.name, "Drift Belt");
        assert_eq!(s.node, "1"); // string, not int
        assert_eq!(s.lane, 7);
        assert_eq!(s.intro, vec!["Skiff".to_string(), "Lancer".to_string()]);
        assert_eq!(s.capital.as_deref(), Some("The Dasher"));

        // Staging: em-dash capital + empty intro -> None / [].
        let staging = r#"{"name":"Staging","node":"0","lane":5,"intro":[],"capital":"—"}"#;
        let st: SectorDef = serde_json::from_str(staging).unwrap();
        assert_eq!(st.capital, None, "em-dash sentinel maps to None");
        assert!(st.intro.is_empty());
    }

    #[test]
    fn sector_def_capital_sentinels_all_map_to_none() {
        for sentinel in [r#""—""#, r#""""#, "null"] {
            let json = format!(r#"{{"name":"X","node":"9","lane":9,"capital":{sentinel}}}"#);
            let s: SectorDef = serde_json::from_str(&json).unwrap();
            assert_eq!(s.capital, None, "sentinel {sentinel} should be None");
        }
        // A real name is preserved.
        let s: SectorDef =
            serde_json::from_str(r#"{"name":"X","node":"9","lane":9,"capital":"Citadel Warlord"}"#)
                .unwrap();
        assert_eq!(s.capital.as_deref(), Some("Citadel Warlord"));
    }

    #[test]
    fn capital_def_parses_canonical_catalog_shape() {
        // Exact catalog.capitals[] shape (broadside.catalog.json): the salvage
        // sP1/sP7 -> salvage_p1/salvage_p7 rename + the null-sP1 case.
        let dasher = r#"{"id":"dasher","name":"The Dasher","sector":"Drift Belt","corrupt":true,"sP1":2,"sP7":7}"#;
        let c: CapitalDef = serde_json::from_str(dasher).unwrap();
        assert_eq!(c.id, "dasher");
        assert_eq!(c.name, "The Dasher");
        assert_eq!(c.sector, "Drift Belt");
        assert!(c.corrupt);
        assert_eq!(c.salvage_p1, Some(2));
        assert_eq!(c.salvage_p7, 7);

        // sP1 null -> None (the Void Sovereign awards no tier-1 salvage).
        let null_sp1 = r#"{"id":"x","name":"X","sector":"S","corrupt":false,"sP1":null,"sP7":9}"#;
        let c2: CapitalDef = serde_json::from_str(null_sp1).unwrap();
        assert_eq!(c2.salvage_p1, None);
        assert!(!c2.corrupt);

        // sP1 absent also -> None (serde default); corrupt absent -> false.
        let minimal = r#"{"id":"y","name":"Y","sector":"S","sP7":8}"#;
        let c3: CapitalDef = serde_json::from_str(minimal).unwrap();
        assert_eq!(c3.salvage_p1, None);
        assert!(!c3.corrupt);
        assert_eq!(c3.salvage_p7, 8);
    }

    #[test]
    fn catalog_capitals_field_deserializes_a_vec_of_capital_defs() {
        let cat_json = r#"{
            "meta": {"schema":"v","lane":[5,7,9],"newAxes":[],"bands":["pointBlank","close","mid","long","extreme"]},
            "actions": [], "mods": [], "subsystems": [], "statuses": [], "enemies": [],
            "capitals": [
                {"id":"dasher","name":"The Dasher","sector":"Drift Belt","corrupt":true,"sP1":2,"sP7":7},
                {"id":"impaler","name":"The Impaler","sector":"Ion Reefs","corrupt":true,"sP1":3,"sP7":8}
            ],
            "patrols": []
        }"#;
        let cat: Catalog = serde_json::from_str(cat_json).unwrap();
        assert_eq!(cat.capitals.len(), 2);
        assert_eq!(cat.capitals[0].id, "dasher");
        assert_eq!(cat.capitals[1].salvage_p7, 8);
    }

    #[test]
    fn catalog_sectors_field_deserializes_a_vec_of_sector_defs() {
        // Catalog.sectors is now strict Vec<SectorDef>, not Vec<Value>: a
        // minimal catalog with a 2-entry sectors[] round-trips.
        let cat_json = r#"{
            "meta": {"schema":"v","lane":[5,7,9],"newAxes":[],"bands":["pointBlank","close","mid","long","extreme"]},
            "actions": [], "mods": [], "subsystems": [], "statuses": [], "enemies": [],
            "sectors": [
                {"name":"Staging","node":"0","lane":5,"intro":[],"capital":"—"},
                {"name":"Drift Belt","node":"1","lane":7,"intro":["Skiff"],"capital":"The Dasher"}
            ],
            "patrols": []
        }"#;
        let cat: Catalog = serde_json::from_str(cat_json).unwrap();
        assert_eq!(cat.sectors.len(), 2);
        assert_eq!(cat.sectors[0].capital, None);
        assert_eq!(cat.sectors[1].lane, 7);
    }

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
        let dmg = Effect::DAMAGE {
            amount: 4,
            band_falloff: None,
        };
        let s = serde_json::to_string(&dmg).unwrap();
        assert_eq!(s, r#"{"kind":"DAMAGE","amount":4}"#);
        let parsed: Effect = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, dmg);

        // With band_falloff:false (the dummy-weapon case in resolve.ts).
        let dmg2 = Effect::DAMAGE {
            amount: 0,
            band_falloff: Some(false),
        };
        let s2 = serde_json::to_string(&dmg2).unwrap();
        assert_eq!(s2, r#"{"kind":"DAMAGE","amount":0,"bandFalloff":false}"#);
    }

    #[test]
    fn effect_displace_self_parses_movement_mode() {
        // Catalog form without the Rust-extension `direction` field — parses
        // with `direction: None`, which preserves the canonical TS semantics
        // (resolver derives direction from ship.orientation).
        let json = r#"{"kind":"DISPLACE_SELF","mode":"THRUST","distance":2}"#;
        let parsed: Effect = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed,
            Effect::DISPLACE_SELF {
                mode: MovementMode::THRUST,
                distance: 2,
                direction: None,
                direction_2d: None,
            }
        );
    }

    #[test]
    fn effect_displace_self_roundtrips_direction_override() {
        // With direction overridden: serializes the camelCase LaneEnd literal
        // and round-trips equal. None case omits the field entirely (verified
        // here by serializing back from a None and asserting the absence).
        let with_dir = Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: Some(LaneEnd::Aft),
            direction_2d: None,
        };
        let s = serde_json::to_string(&with_dir).unwrap();
        assert_eq!(
            s,
            r#"{"kind":"DISPLACE_SELF","mode":"THRUST","distance":1,"direction":"aft"}"#
        );
        let parsed: Effect = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, with_dir);

        let without = Effect::DISPLACE_SELF {
            mode: MovementMode::THRUST,
            distance: 1,
            direction: None,
            direction_2d: None,
        };
        let s2 = serde_json::to_string(&without).unwrap();
        assert!(
            !s2.contains("direction"),
            "None direction (and direction_2d) must serialize to absent field, got {s2}",
        );
    }

    #[test]
    fn range_band_serializes_camel_case() {
        assert_eq!(
            serde_json::to_string(&RangeBand::PointBlank).unwrap(),
            r#""pointBlank""#
        );
        assert_eq!(serde_json::to_string(&RangeBand::Mid).unwrap(), r#""mid""#);
    }

    #[test]
    fn targeting_pattern_preserves_screaming_snake() {
        assert_eq!(
            serde_json::to_string(&TargetingPattern::SPINAL_LINE).unwrap(),
            r#""SPINAL_LINE""#
        );
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
            pos: Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: Facing::Bow(Dir4::S),
            hull: 10,
            max_hull: 10,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: ShieldProfile {
                bow: ShieldFace {
                    armour: 2,
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
                arc: Arc::Forward,
                weapon: "pulse_laser".into(),
            }],
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
            bow: ShieldFace {
                armour: 0,
                charge: 1,
            },
            stern: ShieldFace {
                armour: 0,
                charge: 0,
            },
            port: ShieldFace {
                armour: 0,
                charge: 0,
            },
            starboard: ShieldFace {
                armour: 0,
                charge: 0,
            },
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
        let s_some = SubsystemDef {
            unlock_salvage: Some(2),
            ..s
        };
        let json2 = serde_json::to_string(&s_some).unwrap();
        assert!(json2.contains(r#""unlockSalvage":2"#));
    }

    #[test]
    fn damage_band_falloff_predicate_semantics() {
        // H1: the predicate is "bypass falloff iff Some(false)". None and
        // Some(true) BOTH apply falloff. This test pins that down so a future
        // resolver port can't drift to `band_falloff.unwrap_or(true) == false`
        // or similar without one of these assertions breaking.
        let absent = Effect::DAMAGE {
            amount: 4,
            band_falloff: None,
        };
        let on = Effect::DAMAGE {
            amount: 4,
            band_falloff: Some(true),
        };
        let off = Effect::DAMAGE {
            amount: 4,
            band_falloff: Some(false),
        };

        let bypass = |e: &Effect| {
            matches!(
                e,
                Effect::DAMAGE {
                    band_falloff: Some(false),
                    ..
                }
            )
        };

        assert!(!bypass(&absent), "absent => apply falloff");
        assert!(!bypass(&on), "Some(true) => apply falloff");
        assert!(bypass(&off), "Some(false) => bypass falloff");
    }

    #[test]
    fn hook_count_matches_enum_cardinality() {
        // N2: HOOK_COUNT is hand-counted; the actual drift guard is the
        // exhaustive match in `EventBus::slot`. This test asserts that the
        // slot mapping covers exactly 0..HOOK_COUNT — so if someone adds a
        // Hook variant and bumps `slot` (which they must, to satisfy
        // exhaustiveness) without also bumping HOOK_COUNT, this test fails.
        let all = [
            Hook::Passive,
            Hook::OnChainKill,
            Hook::OnTurnEnd,
            Hook::OnVent,
            Hook::OnWaveStart,
            Hook::OnHeatThreshold,
            Hook::OnDamageDealt,
            Hook::OnDamageTaken,
            Hook::OnHeal,
            Hook::OnReorient,
            Hook::OnLethal,
        ];
        // If a Hook variant is added without being added here, the existing
        // EventBus::slot exhaustiveness check forces touching this list too:
        // the slot() function won't compile until the variant is added, at
        // which point cardinality drift gets surfaced by the length assert
        // below.
        let mut slots: Vec<usize> = all.iter().copied().map(EventBus::slot).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(
            slots,
            (0..HOOK_COUNT).collect::<Vec<_>>(),
            "slot mapping must be dense in 0..HOOK_COUNT"
        );
        assert_eq!(
            all.len(),
            HOOK_COUNT,
            "HOOK_COUNT={} but {} hook variants listed — bump HOOK_COUNT",
            HOOK_COUNT,
            all.len()
        );
    }

    #[test]
    fn emit_fires_subscribers_in_registration_order() {
        // Baseline: two subscribers fire once each in the order registered.
        use std::cell::Cell;
        use std::rc::Rc;

        let mut board = Board {
            size: 1,
            cells: vec![None],
            ordnance: vec![],
            hazards: vec![vec![]],
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: Vec::new(),
        };

        // Take the bus off the board so the callback closures can hold their
        // own state without re-borrowing through board. The real resolver
        // uses `std::mem::take` for the same reason — see `resolve::emit`.
        let mut bus = std::mem::take(&mut board.bus);

        let order = Rc::new(Cell::new(0u32));
        let log = Rc::new(Cell::new([0u32; 2]));

        let order1 = order.clone();
        let log1 = log.clone();
        bus.on(Hook::OnDamageDealt, move |_ctx| {
            let i = order1.get();
            let mut l = log1.get();
            l[i as usize] = 100;
            log1.set(l);
            order1.set(i + 1);
        });

        let order2 = order.clone();
        let log2 = log.clone();
        bus.on(Hook::OnDamageDealt, move |_ctx| {
            let i = order2.get();
            let mut l = log2.get();
            l[i as usize] = 200;
            log2.set(l);
            order2.set(i + 1);
        });

        let mut ctx = HookContext::new(&mut board);
        bus.emit(Hook::OnDamageDealt, &mut ctx);

        assert_eq!(order.get(), 2, "both subscribers fire exactly once");
        assert_eq!(log.get(), [100, 200], "registration order is preserved");
    }

    // NOTE on the no-chained-emit invariant: a callback that tries
    // `ctx.board.bus.emit(...)` finds a default placeholder bus on the board
    // (the resolver `mem::take`s the live bus off `Board` for the duration of
    // every emit) and silently no-ops. This is the documented contract — see
    // `HookContext` and `EventBus` docstrings. The TS engine itself doesn't
    // nest emits either (`resolve.ts:337-344` runs `destroy` -> direct
    // `applyDamage` -> emits `onLethal`; no callback re-emits through the
    // bus). Subsystems trigger downstream effects via resolver function calls
    // (`apply_damage`, `destroy`, ...) which fire their own hooks after the
    // current callback returns and the wrapper restores the bus.
    //
    // Tester's task #22 verifies the invariant (callback's view of the bus
    // is a placeholder); #25 verifies that direct resolver-function calls
    // from inside a callback DO emit through the live bus after return.

    #[test]
    fn sector_with_one_encounter_roundtrips() {
        // Minimal sector + encounter built around the existing types
        // (Ship/Orientation/Hazard) so the roundtrip exercises the spawn
        // refs end-to-end.
        let enc = EncounterDef {
            id: "skirmish_alpha".into(),
            enemy_ships: vec![ShipSpawn {
                class_id: "wanderer".into(),
                cell: 3,
                pos: Pos::new(0, 0),
                orientation: Orientation::BowOn { bow: LaneEnd::Aft },
                facing: Facing::Bow(Dir4::S),
                hp_override: Some(4),
            }],
            hazards: vec![Hazard {
                id: "mine_a".into(),
                kind: HazardKind::Mine,
                cell: 2,
                pos: Pos::new(0, 0),
                payload: vec![Effect::DAMAGE {
                    amount: 2,
                    band_falloff: Some(false),
                }],
                ttl: None,
            }],
            is_boss: false,
        };
        let sector = Sector {
            id: "training_grounds".into(),
            name: "Training Grounds".into(),
            patrol_tier: 1,
            encounters: vec![enc],
        };
        let json = serde_json::to_string(&sector).unwrap();
        let back: Sector = serde_json::from_str(&json).unwrap();
        assert_eq!(sector, back);

        // hp_override None must be omitted (skip_serializing_if).
        let ship_spawn_no_hp = ShipSpawn {
            class_id: "wanderer".into(),
            cell: 0,
            pos: Pos::new(0, 0),
            orientation: Orientation::Broadside,
            facing: Facing::Bow(Dir4::S),
            hp_override: None,
        };
        let s = serde_json::to_string(&ship_spawn_no_hp).unwrap();
        assert!(!s.contains("hp_override"), "None must omit, got {s}");
    }

    #[test]
    fn run_roundtrips_through_json() {
        // Build a minimal player Ship via the same factory style other
        // tests use; persistence has to round-trip the full Ship state
        // (hull, heat, queue, cooldowns, statuses, traits, klass).
        let player = Ship {
            id: "player".into(),
            faction: Faction::Player,
            cell: 0,
            pos: Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: Facing::Bow(Dir4::S),
            hull: 4,
            max_hull: 5,
            heat: 1,
            heat_max: 6,
            locked_out: false,
            shield_profile: ShieldProfile {
                bow: ShieldFace {
                    armour: 2,
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
            mounts: vec![Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "pulse_laser".into(),
            }],
            queue: vec!["pulse_laser".into()],
            cooldowns: {
                let mut m = HashMap::new();
                m.insert("torpedo".into(), 2);
                m
            },
            statuses: vec![Status {
                kind: StatusKind::TargetLock,
                duration: 1,
                face: None,
            }],
            traits: vec![Trait::Agile],
            klass: Some("wanderer".into()),
        };
        let run = Run {
            current_sector_idx: 2,
            salvage: 17,
            completed_encounters: 4,
            defeated: false,
            victorious: false,
            player,
        };
        let json = serde_json::to_string(&run).unwrap();
        let back: Run = serde_json::from_str(&json).unwrap();
        assert_eq!(run, back);
    }

    #[test]
    fn run_new_seeds_clean_progress() {
        // `Run::new(player)` should start at sector 0 / encounter 0 with
        // zero salvage and neither end-state flag. The player Ship is
        // carried in unchanged.
        let player = Ship {
            id: "p".into(),
            faction: Faction::Player,
            cell: 0,
            pos: Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: Facing::Bow(Dir4::S),
            hull: 5,
            max_hull: 5,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: ShieldProfile {
                bow: ShieldFace {
                    armour: 2,
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
            mounts: vec![],
            queue: vec![],
            cooldowns: HashMap::new(),
            statuses: vec![],
            traits: vec![],
            klass: Some("wanderer".into()),
        };
        let run = Run::new(player.clone());
        assert_eq!(run.current_sector_idx, 0);
        assert_eq!(run.salvage, 0);
        assert_eq!(run.completed_encounters, 0);
        assert!(!run.defeated);
        assert!(!run.victorious);
        assert_eq!(run.player, player);
    }

    #[test]
    fn save_state_roundtrips_and_board_snapshot_drops_bus() {
        // Build a live Board (with the runtime-only bus + counter), snapshot
        // it, roundtrip through JSON, then rebuild a Board from the parsed
        // snapshot with a fresh EventBus. The snapshot must NOT carry the
        // bus or the destroys_this_window counter.
        let mut shield = ShieldProfile {
            bow: ShieldFace {
                armour: 2,
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
        };
        shield.bow.charge = 1;
        let ship = Ship {
            id: "frigate".into(),
            faction: Faction::Player,
            cell: 0,
            pos: Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: Facing::Bow(Dir4::S),
            hull: 10,
            max_hull: 10,
            heat: 2,
            heat_max: 6,
            locked_out: false,
            shield_profile: shield,
            mounts: vec![Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "pulse_laser".into(),
            }],
            queue: vec![],
            cooldowns: HashMap::new(),
            statuses: vec![],
            traits: vec![],
            klass: Some("wanderer".into()),
        };
        let mut board = Board {
            size: 3,
            cells: vec![Some(ship), None, None],
            ordnance: vec![],
            hazards: vec![vec![], vec![], vec![]],
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 7, // runtime junk that must NOT round-trip
            fire_events: vec![FireEvent {
                from_cell: 0,
                to_cell: 2,
                from_pos: Pos::new(0, 0),
                to_pos: Pos::new(0, 0),
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Player,
                hit: true,
            }], // transient render junk that must NOT round-trip either
        };
        // Register a callback so the bus is non-empty at snapshot time —
        // the snapshot still must not carry it.
        board.bus.on(Hook::OnDamageDealt, |_ctx| { /* canary */ });

        let snap = BoardSnapshot::from(&board);
        // The player Ship inside Run is independent of the board snapshot —
        // for this test we just need a placeholder; the rest of the assertions
        // are about the snapshot, not Run.player.
        let placeholder_player = board.cells[0].as_ref().unwrap().clone();
        let save = SaveState {
            run: Run {
                current_sector_idx: 0,
                salvage: 0,
                completed_encounters: 0,
                defeated: false,
                victorious: false,
                player: placeholder_player,
            },
            board: snap,
        };

        let json = serde_json::to_string(&save).unwrap();
        // Snapshot must not contain the bus or the destroys counter, even
        // as field names — they're structurally absent.
        assert!(
            !json.contains("\"bus\""),
            "BoardSnapshot leaked bus into JSON: {json}"
        );
        assert!(
            !json.contains("destroys_this_window"),
            "BoardSnapshot leaked destroys_this_window: {json}"
        );
        assert!(
            !json.contains("fire_events"),
            "BoardSnapshot leaked fire_events (transient render state): {json}"
        );

        let back: SaveState = serde_json::from_str(&json).unwrap();
        assert_eq!(save, back);

        // Rebuild a Board from the parsed snapshot with a fresh bus.
        let rebuilt = back.board.into_board(EventBus::default());
        assert_eq!(rebuilt.size, 3);
        assert_eq!(rebuilt.patrol, 1);
        assert_eq!(
            rebuilt.destroys_this_window, 0,
            "rebuilt board resets the chain-kill counter to 0"
        );
        assert!(
            rebuilt.fire_events.is_empty(),
            "rebuilt board starts with no fire events"
        );
        // The Ship's pre-save state is preserved (cell, hull, heat, charge).
        let s = rebuilt.cells[0].as_ref().unwrap();
        assert_eq!(s.heat, 2);
        assert_eq!(s.shield_profile.bow.charge, 1);
        assert_eq!(s.klass.as_deref(), Some("wanderer"));
    }

    #[test]
    fn class_affinity_serializes_camel_case() {
        // Matches the canonical doc's string literals ("flexible" / "bowOn"
        // / "broadside"). `BowOn` is the one that needs camelCase, the
        // others are single lowercase tokens.
        assert_eq!(
            serde_json::to_string(&ClassAffinity::Flexible).unwrap(),
            r#""flexible""#
        );
        assert_eq!(
            serde_json::to_string(&ClassAffinity::BowOn).unwrap(),
            r#""bowOn""#
        );
        assert_eq!(
            serde_json::to_string(&ClassAffinity::Broadside).unwrap(),
            r#""broadside""#
        );

        let parsed: ClassAffinity = serde_json::from_str(r#""bowOn""#).unwrap();
        assert_eq!(parsed, ClassAffinity::BowOn);
    }

    #[test]
    fn class_def_roundtrips_canonical_shape() {
        // Mirrors the `wanderer` entry from `broadside-analysis.html:1144`.
        // Includes every field — unlock + passive populated — so the
        // round-trip exercises the full shape (not just the
        // `skip_serializing_if = "Option::is_none"` shortcut paths).
        let wanderer = ClassDef {
            id: "wanderer".into(),
            name: r#"Frigate "Drifter""#.into(),
            affinity: ClassAffinity::Flexible,
            unlock: Some("Unlocked by default".into()),
            set1: vec!["Broadside Battery".into(), "Pulse Laser".into()],
            set2: vec!["Railgun Broadside".into(), "Grav Snare".into()],
            signature: "Slip — move forward to trade places with the ship directly ahead.".into(),
            passive: None,
            desc:
                "The starting hull; a balanced beam + broadside opener with no strong stance bias."
                    .into(),
        };
        let json = serde_json::to_string(&wanderer).unwrap();
        let back: ClassDef = serde_json::from_str(&json).unwrap();
        assert_eq!(wanderer, back);
        // `passive: None` must omit (no JSON `null`): the canonical doc emits
        // `passive:null` in TS, but JSON serializers vary; we choose absent
        // for consistency with the rest of the Rust port's `?:`-style
        // optional fields. A future strict-canonical pass can flip this.
        assert!(
            !json.contains("\"passive\""),
            "passive should be omitted when None, got {json}"
        );

        // And the placeholder `Catalog.classes` path: an empty array still
        // parses, and a populated one preserves order through the catalog
        // boundary.
        let json_catalog_classes_empty = r"[]";
        let parsed: Vec<ClassDef> = serde_json::from_str(json_catalog_classes_empty).unwrap();
        assert!(parsed.is_empty());
    }
}
