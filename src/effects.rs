//! VFX effect data — the serde schema that turns the hardcoded constants in
//! [`crate::vfx`] into authored DATA.
//!
//! This is the **shared bridge** for the Broadside VFX editor (the standalone
//! `broadside_vfx_editor` app): both the game runtime and the editor read the
//! SAME [`EffectCatalog`] JSON, so an effect tuned in the editor is exactly the
//! effect the game plays. Defining it here, in the engine crate, keeps a single
//! source of truth (the same pattern as [`crate::ship_design`] / [`crate::save`]
//! / [`crate::types`] — all data-only serde living in the engine).
//!
//! ## Pure data, non-gated
//!
//! This module is **not** behind the `render` feature: it is plain serde with no
//! `wgpu`, no GPU, no [`crate::gfx`] types — so it compiles on the default
//! (logic-only) build and the editor can read it without pulling the render
//! stack. It also has **no dependency on [`crate::vfx`]**, so it stands alone;
//! wiring `vfx` to *read* an [`EffectDef`] (instead of its module constants) is a
//! separate, later integration step.
//!
//! ## Behavior-identical by default
//!
//! Every parameter mirrors a literal currently hardcoded in `vfx.rs`, and every
//! [`Default`] impl reproduces that literal **exactly**. So a catalog built from
//! defaults — or an empty / partial JSON file (every field is
//! `#[serde(default)]`) — yields precisely today's look. The data layer changes
//! the game's visuals only once someone edits a value.
//!
//! ## Schema shape
//!
//! - [`EffectCatalog`] — the top-level file: a list of named [`EffectDef`]s.
//! - [`EffectDef`] — a stable `id` (the key the game/editor look effects up by)
//!   plus the per-family [`EffectKind`].
//! - [`EffectKind`] — an internally-tagged (`#[serde(tag = "kind")]`) enum over
//!   the six effect families the `vfx` pool produces today: [`ShotBeam`],
//!   [`HitFlash`], [`Explosion`], [`Trail`], [`TelegraphFire`], [`ParticleBurst`].

use serde::{Deserialize, Serialize};

use crate::types::WeaponArchetype;

/// An RGB color, serialized as a bare 3-element array (`[r, g, b]`). Newtype +
/// `#[serde(transparent)]` so the JSON wire shape is just the array (matching
/// the [`crate::ship_design::Point2`] convention), while the Rust type stays
/// distinct from a raw `[f32; 3]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rgb(pub [f32; 3]);

/// An RGBA color, serialized as a bare 4-element array (`[r, g, b, a]`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rgba(pub [f32; 4]);

/// The top-level effect catalog — what an editor save / game asset file is.
///
/// A flat list of named effects; the game and editor look an effect up by its
/// [`EffectDef::id`]. Round-trips to/from JSON via [`Self::from_json_str`] /
/// [`Self::to_json_string`].
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct EffectCatalog {
    /// Every authored effect, keyed by `id` at lookup time.
    #[serde(default)]
    pub effects: Vec<EffectDef>,
}

impl EffectCatalog {
    /// Parse a catalog from a JSON string (an editor save / bundled asset).
    pub fn from_json_str(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serialize the catalog to pretty JSON (what the editor writes on save).
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Find an effect by its `id`, if present.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&EffectDef> {
        self.effects.iter().find(|e| e.id == id)
    }

    /// Read a catalog from `path`. Returns:
    /// - `Ok(Some(cat))` when the file parses,
    /// - `Ok(None)` when the file does not exist (the game's "no authored
    ///   tunings, use defaults" case),
    /// - `Err(_)` with a `String` describing the io or decode failure.
    ///
    /// Counterpart to `broadside_vfx_editor`'s save: the editor writes via
    /// [`Self::to_json_string`] + atomic `fs::rename`; the game reads here.
    pub fn load_from_disk(path: impl AsRef<std::path::Path>) -> Result<Option<Self>, String> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }
        let s =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let cat = Self::from_json_str(&s).map_err(|e| format!("decode {}: {e}", path.display()))?;
        Ok(Some(cat))
    }
}

/// Stable [`EffectDef::id`] keys the editor writes + the game reads back. Shared
/// constants so `broadside_vfx_editor`'s `Params::to_catalog` and the engine's
/// `VfxConfig::from_catalog` cannot drift on either side. One id per
/// [`EffectKind`] variant.
pub const ID_SHOT_BEAM: &str = "shot_beam";
pub const ID_HIT_FLASH: &str = "hit_flash";
pub const ID_EXPLOSION: &str = "explosion";
pub const ID_TRAIL: &str = "trail";
pub const ID_TELEGRAPH_FIRE: &str = "telegraph_fire";
pub const ID_PARTICLE_BURST: &str = "particle_burst";
/// (#209 hook 4) Distance-delayed light bounce off a surviving ship when a blast
/// goes off. One id per [`EffectKind::ExplosionReflection`].
pub const ID_EXPLOSION_REFLECTION: &str = "explosion_reflection";
/// (#217) Composed effect — a Sequence "combines" other authored effects into
/// one named timeline. Unlike the per-family ids above this is a DISCRIMINATOR
/// only: every Sequence effect has a USER-chosen [`EffectDef::id`] (the name
/// the game looks up via [`crate::vfx::CombatVfx::play_sequence`]); the
/// constant here just names the variant for schema-aware tooling that wants
/// to query "is this a Sequence?".
pub const ID_SEQUENCE: &str = "sequence";

/// One authored effect: a stable lookup `id` plus its per-family parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectDef {
    /// Stable identifier the game and editor reference (e.g. `"player_beam"`).
    pub id: String,
    /// The effect family + its tunable parameters.
    #[serde(flatten)]
    pub kind: EffectKind,
}

/// The six effect families produced by [`crate::vfx`] today, internally tagged
/// by a `"kind"` field so the JSON is self-describing and forward-extensible
/// (a new family is a new variant; old catalogs keep parsing). Each variant's
/// fields map 1:1 to constants currently hardcoded in `vfx.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EffectKind {
    /// An exact attacker→target fired shot (the resolver's per-round
    /// `FireEvent`, animated as a travelling-then-fading beam).
    ShotBeam(ShotBeam),
    /// Expanding hit-spark where a ship takes damage.
    HitFlash(HitFlash),
    /// Multi-layer expanding blast where a ship is destroyed.
    Explosion(Explosion),
    /// Fading streak along an ordnance step.
    Trail(Trail),
    /// The telegraph-slot discharge pop when a queued enemy action fires.
    TelegraphFire(TelegraphFire),
    /// A radial screen-space particle burst (e.g. debris on a kill).
    ParticleBurst(ParticleBurst),
    /// (#209 hook 4) Distance-delayed light bounce: when a ship explodes, every
    /// surviving ship's hull lights up briefly with a delay proportional to its
    /// chebyshev distance from the blast (fake light-travel-time). Editor
    /// authors `color` / `life_secs` / `peak_alpha` / `delay_per_cell` — the
    /// engine spawns one effect per surviving ship in the diff explosion
    /// branch.
    ExplosionReflection(ExplosionReflection),
    /// (#217) Sequence — a "combined" effect that PLAYS a fixed timeline of
    /// other authored effects, each at its own `delay_secs` offset. The editor
    /// writes one of these when the user ticks "include" on N base families
    /// and dials per-step delays; the game plays it via
    /// [`crate::vfx::CombatVfx::play_sequence`] (looks up each step's id in the
    /// same catalog and schedules it onto the pool with the staggered
    /// `start_delay`). A Sequence's [`EffectDef::id`] is USER-chosen (e.g.
    /// `"player_torpedo_kill"`); the `"kind": "Sequence"` discriminator marks
    /// it. Step ids that don't resolve (typo / catalog mismatch) are silently
    /// skipped — the rest of the timeline still plays.
    Sequence(SequenceDef),
}

/// Per-archetype beam style row — mirrors one arm of `vfx::archetype_beam_style`
/// `(thickness, life_secs)`, now data keyed by [`WeaponArchetype`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeamStyle {
    /// Which weapon archetype this row styles.
    pub archetype: WeaponArchetype,
    /// Beam half-thickness in virtual pixels.
    pub thickness: f32,
    /// Beam lifetime in seconds.
    pub life_secs: f32,
}

/// Fired-shot beam params (`vfx::ShotBeam` / `emit_shot_beam` +
/// `archetype_beam_style` + `faction_beam_tint`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotBeam {
    /// Per-archetype `(thickness, life)` table (the `archetype_beam_style` arms).
    #[serde(default = "default_beam_styles")]
    pub per_archetype: Vec<BeamStyle>,
    /// Tint for enemy-fired beams (`faction_beam_tint(Enemy)`).
    #[serde(default = "default_enemy_tint")]
    pub enemy_tint: Rgb,
    /// Tint for player-fired beams (`faction_beam_tint(Player)`).
    #[serde(default = "default_player_tint")]
    pub player_tint: Rgb,
    /// Fraction of life spent in the TRAVEL phase before STRIKE+FADE
    /// (`emit_shot_beam::TRAVEL_FRAC`).
    #[serde(default = "default_travel_frac")]
    pub travel_frac: f32,
    /// Base alpha for a hit (`emit_shot_beam` `base_alpha` when not `dim`).
    #[serde(default = "default_hit_alpha")]
    pub hit_alpha: f32,
    /// Base alpha for a miss (`dim`).
    #[serde(default = "default_miss_alpha")]
    pub miss_alpha: f32,
    /// Bright leading-tip length as a fraction of the beam (the `0.12` stub).
    #[serde(default = "default_tip_len_frac")]
    pub tip_len_frac: f32,
    /// Leading-tip thickness multiplier (the `1.4` over-bright stub).
    #[serde(default = "default_tip_thickness_mul")]
    pub tip_thickness_mul: f32,
}

/// Hit-flash params (`vfx::HIT_COLOR` + `emit_flash`, peak `16.0` at the call
/// site for the hit case).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HitFlash {
    /// Flash tint (`HIT_COLOR`).
    #[serde(default = "default_hit_color")]
    pub color: Rgb,
    /// Lifetime in seconds (`HIT_FLASH_SECS`).
    #[serde(default = "default_hit_flash_secs")]
    pub life_secs: f32,
    /// Peak size in virtual pixels (the `16.0` passed to `emit_flash`).
    #[serde(default = "default_hit_flash_peak")]
    pub peak_px: f32,
    /// Size grow base fraction (`emit_flash` `0.35`).
    #[serde(default = "default_flash_grow_base")]
    pub grow_base: f32,
    /// Size grow span fraction (`emit_flash` `0.65`).
    #[serde(default = "default_flash_grow_span")]
    pub grow_span: f32,
    /// Peak alpha (`emit_flash` `0.85`).
    #[serde(default = "default_flash_alpha")]
    pub alpha_peak: f32,
}

/// Explosion params (`vfx::EXPLOSION_COLOR` + `EXPLOSION_SECS` + `emit_explosion`
/// three eased layers: shell / core / ignition flash, peak `30.0`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Explosion {
    /// Lifetime in seconds (`EXPLOSION_SECS`).
    #[serde(default = "default_explosion_secs")]
    pub life_secs: f32,
    /// Peak size in virtual pixels (`emit_explosion` `peak`).
    #[serde(default = "default_explosion_peak")]
    pub peak_px: f32,
    /// Expanding shell tint (`EXPLOSION_COLOR`).
    #[serde(default = "default_explosion_color")]
    pub shell_color: Rgb,
    /// Shell grow base fraction (`0.25`).
    #[serde(default = "default_shell_grow_base")]
    pub shell_grow_base: f32,
    /// Shell grow span fraction (`0.85`).
    #[serde(default = "default_shell_grow_span")]
    pub shell_grow_span: f32,
    /// Shell peak alpha (`0.8`).
    #[serde(default = "default_shell_alpha")]
    pub shell_alpha: f32,
    /// Hot core tint (`[1.0, 0.85, 0.4]`).
    #[serde(default = "default_core_color")]
    pub core_color: Rgb,
    /// Core life as a fraction of the whole (the `0.55` cutoff).
    #[serde(default = "default_core_life_frac")]
    pub core_life_frac: f32,
    /// Core peak alpha (`0.9`).
    #[serde(default = "default_core_alpha")]
    pub core_alpha: f32,
    /// Ignition-flash tint (`[1.0, 0.97, 0.9]`).
    #[serde(default = "default_flash_color")]
    pub flash_color: Rgb,
    /// Ignition-flash life as a fraction of the whole (the `0.25` cutoff).
    #[serde(default = "default_ignition_life_frac")]
    pub flash_life_frac: f32,
    /// Ignition-flash peak alpha (`0.95`).
    #[serde(default = "default_ignition_alpha")]
    pub flash_alpha: f32,
    /// (2026-07-01) Per-explosion silhouette. Defaults to
    /// [`ShapeKind::Circle`] so unedited catalogs reproduce the pre-fix
    /// round bloom byte-for-byte (`emit_explosion` used to hardcode
    /// `PARTICLE_CIRCLE`). Editor can flip this to any of the extended
    /// shapes (square / diamond / ring / star4-5 / plus / x / crescent /
    /// hollow-square / hexagon / triangle / line) for authored effects.
    ///
    /// Legacy single-shape field: kept for backward-compat so old catalogs
    /// that only set `"shape": "diamond"` still work. When [`shapes`] is
    /// non-empty it takes precedence and this field is unused by the engine.
    /// New catalog authors should prefer [`shapes`].
    #[serde(default = "default_explosion_shape")]
    pub shape: ShapeKind,
    /// (#218, 2026-07-01) Multi-layer shape stack. When non-empty, the engine
    /// draws each layer independently with its own silhouette / rotation /
    /// alpha / `scale_mul` — producing compound shapes (e.g. three squares at
    /// 0°/45°/90° → an 8-point star ish). When empty the engine falls back to
    /// the legacy single [`shape`] field as a single axis-aligned layer.
    ///
    /// Wire format: `#[serde(default)]` so old catalogs with no `shapes` key
    /// deserialise to an empty vec and take the single-shape fallback path.
    #[serde(default)]
    pub shapes: Vec<ExplosionShapeLayer>,
}

impl Explosion {
    /// The effective layer list for rendering: returns `shapes` if non-empty,
    /// otherwise synthesises a single default layer from the legacy `shape` field.
    /// This is the ONLY place both paths collapse to the same type — callers
    /// (`emit_explosion`) always iterate `effective_layers()` and never branch on
    /// `shapes.is_empty()` themselves.
    pub fn effective_layers(&self) -> impl Iterator<Item = ExplosionShapeLayer> + '_ {
        let single: Option<ExplosionShapeLayer> = if self.shapes.is_empty() {
            Some(ExplosionShapeLayer {
                shape: self.shape,
                ..ExplosionShapeLayer::default()
            })
        } else {
            None
        };
        self.shapes.iter().copied().chain(single)
    }
}

/// Ordnance-trail params (`vfx::TRAIL_COLOR` + `TRAIL_SECS` + `emit_beam`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trail {
    /// Trail tint (`TRAIL_COLOR`).
    #[serde(default = "default_trail_color")]
    pub color: Rgb,
    /// Lifetime in seconds (`TRAIL_SECS`).
    #[serde(default = "default_trail_secs")]
    pub life_secs: f32,
    /// Base thickness in virtual pixels (`emit_beam` `3.0`).
    #[serde(default = "default_trail_thickness")]
    pub thickness: f32,
    /// Peak alpha (`emit_beam` `0.9`).
    #[serde(default = "default_trail_alpha")]
    pub alpha: f32,
}

/// Telegraph-fire pop params (`vfx::TELEGRAPH_COLOR` + `TELEGRAPH_FIRE_SECS` +
/// `emit_telegraph_fire`, slot offset `-96`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TelegraphFire {
    /// Pop tint (`TELEGRAPH_COLOR`).
    #[serde(default = "default_telegraph_color")]
    pub color: Rgba,
    /// Lifetime in seconds (`TELEGRAPH_FIRE_SECS`).
    #[serde(default = "default_telegraph_fire_secs")]
    pub life_secs: f32,
    /// Vertical offset above the ship to the telegraph slot, virtual pixels
    /// (`emit_telegraph_fire` `-96.0`).
    #[serde(default = "default_telegraph_slot_offset")]
    pub slot_offset_px: f32,
    /// Size grow base fraction (`0.4`).
    #[serde(default = "default_telegraph_grow_base")]
    pub grow_base: f32,
    /// Size grow span fraction (`1.1`).
    #[serde(default = "default_telegraph_grow_span")]
    pub grow_span: f32,
    /// Peak alpha (`0.95`).
    #[serde(default = "default_telegraph_alpha")]
    pub alpha: f32,
}

/// Particle-burst params (`vfx::ParticlePool::spawn_burst` + `advance` drag).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParticleBurst {
    /// Number of particles to spawn (the burst `n`).
    #[serde(default = "default_burst_count")]
    pub count: u32,
    /// Burst tint (the burst `color`).
    #[serde(default = "default_burst_color")]
    pub color: Rgba,
    /// Base lifetime in seconds (the burst `dur`, before per-particle jitter).
    #[serde(default = "default_burst_life_secs")]
    pub life_secs: f32,
    /// Minimum radial speed, px/sec (`spawn_burst` `24.0`).
    #[serde(default = "default_burst_speed_min")]
    pub speed_min: f32,
    /// Maximum radial speed, px/sec (`24.0 + 70.0`).
    #[serde(default = "default_burst_speed_max")]
    pub speed_max: f32,
    /// Minimum birth half-size, virtual pixels (`spawn_burst` `2.0`).
    #[serde(default = "default_burst_size_min")]
    pub size_min: f32,
    /// Maximum birth half-size, virtual pixels (`2.0 + 3.0`).
    #[serde(default = "default_burst_size_max")]
    pub size_max: f32,
    /// Per-particle lifetime jitter span (`spawn_burst` dur `0.7 + 0.6`: a
    /// particle lives `dur * (0.7 ..= 1.3)`). Stored as `(base, span)` =
    /// `(0.7, 0.6)`.
    #[serde(default = "default_burst_dur_jitter")]
    pub dur_jitter: [f32; 2],
    /// Per-second velocity drag coefficient (`advance` `2.0` in `1 - 2*dt`).
    #[serde(default = "default_burst_drag")]
    pub drag: f32,
    /// (#217) Per-particle silhouette. Defaults to [`ShapeKind::Square`] so an
    /// unedited catalog reproduces today's look byte-for-byte.
    #[serde(default)]
    pub shape: ShapeKind,
    /// (#217) Minimum birth rotation in RADIANS — each particle's starting
    /// orientation is drawn deterministically from `[rotation_min,
    /// rotation_max]` (no RNG, same FNV fold as the radial spread). Defaults
    /// to `0.0` so unedited bursts look identical (the `SOLID_WHITE` square
    /// is rotation-invariant; non-square shapes start axis-aligned).
    #[serde(default = "default_burst_rotation_min")]
    pub rotation_min: f32,
    /// (#217) Maximum birth rotation in RADIANS. Set to ~`TAU` for "any
    /// orientation"; set equal to `rotation_min` for "every particle the same
    /// angle." Defaults to `0.0` (no spread).
    #[serde(default = "default_burst_rotation_max")]
    pub rotation_max: f32,
    /// (#217) Angular velocity in RADIANS PER SECOND — applied per particle
    /// per `advance(dt)`. Positive = CCW. Defaults to `0.0` (no spin) so
    /// unedited bursts are spin-free.
    #[serde(default = "default_burst_spin_rate")]
    pub spin_rate: f32,
}

/// (#209 hook 4) Distance-delayed light bounce. When a ship explodes, every
/// surviving ship lights up briefly with a delay proportional to its chebyshev
/// distance from the blast — a fake light-travel-time cue that makes the blast
/// feel like a real point source. `vfx.rs`'s `diff` explosion branch spawns
/// one effect per surviving ship; `emit_reflection_glow` draws it once the
/// per-instance delay elapses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplosionReflection {
    /// Bounce tint — defaults to a warm yellow-white so the reflection reads as
    /// light from the blast, not as a hit.
    #[serde(default = "default_reflection_color")]
    pub color: Rgb,
    /// Glow lifetime in seconds AFTER the per-instance delay elapses.
    #[serde(default = "default_reflection_life_secs")]
    pub life_secs: f32,
    /// Peak alpha at mid-life — subtle (default ~0.35), it's a light bounce,
    /// not a hit.
    #[serde(default = "default_reflection_peak_alpha")]
    pub peak_alpha: f32,
    /// Seconds of delay per cell of chebyshev distance from the blast. Default
    /// 0.08 s/cell ≈ 12 cells/sec "fake light speed" — slow enough to read on
    /// a 5x4 board, fast enough not to bore. Tunable.
    #[serde(default = "default_reflection_delay_per_cell")]
    pub delay_per_cell: f32,
}

/// (#217) The body of a [`EffectKind::Sequence`]: a list of steps that
/// reference OTHER effects in the same [`EffectCatalog`] by id, each scheduled
/// at its own offset.
///
/// Wire shape (JSON):
/// ```json
/// { "id": "player_torpedo_kill",
///   "kind": "Sequence",
///   "steps": [
///     { "id": "player_beam",   "delay_secs": 0.00 },
///     { "id": "torpedo_trail", "delay_secs": 0.05 },
///     { "id": "boom",          "delay_secs": 0.40 }
///   ] }
/// ```
///
/// Empty `steps` is a no-op (legal — useful as an editor scaffold). Order is
/// preserved (the editor's UI strip mirrors `steps`'s order). The
/// per-effect-family `start_delay` machinery in `vfx.rs` does the scheduling —
/// the Sequence is pure data, no runtime state of its own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SequenceDef {
    /// Ordered list of base-effect steps. Each step references another
    /// [`EffectDef::id`] in the SAME [`EffectCatalog`]; the editor renders
    /// these as a vertical timeline of included families with a per-row delay
    /// slider.
    #[serde(default)]
    pub steps: Vec<SequenceStep>,
}

/// (#217) One row in a [`SequenceDef`]: which authored effect to play + how
/// long to wait before playing it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SequenceStep {
    /// The [`EffectDef::id`] of the base effect to play at this step. The
    /// engine looks this id up in the same [`EffectCatalog`] passed to
    /// [`crate::vfx::CombatVfx::play_sequence`]; an unresolved id is logged
    /// and skipped (the rest of the timeline still plays — degrades
    /// gracefully past editor typos).
    #[serde(default)]
    pub id: String,
    /// Seconds of silence before this step's effect spawns, measured from the
    /// Sequence's t=0 (i.e. the call to `play_sequence`). The engine folds
    /// this into the spawned effect's `start_delay`; per-particle or per-beam
    /// existing delays are additive on top inside the spawned base effect.
    #[serde(default)]
    pub delay_secs: f32,
}

/// (#218, 2026-07-01) One layer in a multi-shape explosion stack. An
/// [`Explosion`] can carry any number of these (via [`Explosion::shapes`]); the
/// engine draws each layer independently, applying that layer's shape silhouette,
/// rotation, alpha multiplier, and size multiplier on top of the shared t-driven
/// shell/core/flash envelope.
///
/// Defaults produce a single axis-aligned Circle layer at full alpha and full
/// scale — so an [`Explosion`] constructed with `shapes: vec![Default::default()]`
/// is byte-identical to the pre-#218 single-shape path.
///
/// Wire format (JSON):
/// ```json
/// { "shape": "diamond", "rotation_deg": 45.0, "alpha": 0.8, "scale_mul": 1.0 }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplosionShapeLayer {
    /// Which atlas silhouette this layer uses.
    #[serde(default = "default_explosion_shape")]
    pub shape: ShapeKind,
    /// Rotation of this layer's silhouette sprite in degrees (0 = axis-aligned).
    /// Passed as `rotation_rad = rotation_deg.to_radians()` to the GPU.
    #[serde(default)]
    pub rotation_deg: f32,
    /// Alpha multiplier in `[0, 1]` applied on top of the t-driven alpha of each
    /// explosion sub-layer (shell/core/flash). `1.0` = full opacity.
    #[serde(default = "default_layer_alpha")]
    pub alpha: f32,
    /// Size multiplier applied to the computed size of each explosion sub-layer.
    /// `1.0` = normal size.
    #[serde(default = "default_layer_scale_mul")]
    pub scale_mul: f32,
}

const fn default_layer_alpha() -> f32 {
    1.0
}
const fn default_layer_scale_mul() -> f32 {
    1.0
}

impl Default for ExplosionShapeLayer {
    fn default() -> Self {
        Self {
            shape: default_explosion_shape(),
            rotation_deg: 0.0,
            alpha: default_layer_alpha(),
            scale_mul: default_layer_scale_mul(),
        }
    }
}

/// (#217) Per-particle silhouette for [`ParticleBurst`]. Was implicitly
/// `Square` (the `SOLID_WHITE` atlas cell); the editor now lets the user
/// author the silhouette per-burst. `Square` is the default so an
/// unedited catalog reproduces today's look byte-for-byte.
///
/// Wire shape: `#[serde(rename_all = "snake_case")]` → JSON values are
/// `"square"` / `"circle"` / `"triangle"` / `"line"`. The atlas cell each
/// shape resolves to is the engine's lookup (`SOLID_WHITE` for square; the
/// editor doesn't need to know which atlas cell).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    /// `SOLID_WHITE` filled square — today's look.
    #[default]
    Square,
    /// Filled circle silhouette (round soft-edge dot).
    Circle,
    /// Equilateral triangle silhouette (pointing along `+rotation_rad`).
    Triangle,
    /// Thin line segment (uses the burst's `size` for thickness and a
    /// fixed-ratio length; `rotation_rad` orients the line).
    Line,
    /* ---- extended shape kit (2026-07-01) --------------------------------
     * Broader palette for authored explosions + particle bursts. Each new
     * variant has a matching `PARTICLE_*` atlas cell drawn procedurally in
     * `atlas.rs` and is dispatched in BOTH `emit_explosion` (the bloom) AND
     * `ParticlePool::emit` (per-particle silhouette).
     * ------------------------------------------------------------------- */
    /// Hollow circle outline (~4 px stroke). Reads as a shockwave rim /
    /// blast expanding ring.
    Ring,
    /// Hollow axis-aligned box outline (~4 px stroke). Reads as a scanline
    /// / holo pip.
    HollowSquare,
    /// Filled square rotated 45° (a solid diamond silhouette). Reads as a
    /// crystal shard / lozenge.
    Diamond,
    /// Filled flat-top hexagon silhouette. Reads as a shielded / systems
    /// pip.
    Hexagon,
    /// 4-pointed cardinal star silhouette (a sharp `+` with tapered arms).
    /// Reads as a lens flare / cardinal burst.
    Star4,
    /// Classic 5-pointed star silhouette. Reads as a hero / capital
    /// destruction mark.
    Star5,
    /// Cardinal `+` cross silhouette (horizontal + vertical bar). Reads as
    /// a med / repair pip.
    Plus,
    /// Diagonal `X` cross silhouette. Reads as a hit mark / negation.
    X,
    /// Crescent silhouette (waxing crescent, opens to the right in the
    /// canonical pose; `rotation_rad` reorients).
    Crescent,
}

/* ---- defaults: the EXACT constants currently in vfx.rs ---------------------
 *
 * Each `default_*` fn returns the literal hardcoded in `crate::vfx` today, so a
 * catalog of defaults (or partial/empty JSON) reproduces the current look
 * byte-for-byte. serde needs a path-callable fn per `#[serde(default = "..")]`
 * field; these are those. Keep them in lock-step with `vfx.rs` until the
 * param-lifting step makes `vfx` read these instead (then `vfx` becomes the
 * consumer and this is the single source).
 */

// -- ShotBeam --
fn default_beam_styles() -> Vec<BeamStyle> {
    // Mirrors vfx::archetype_beam_style, one row per WeaponArchetype.
    use WeaponArchetype as W;
    vec![
        BeamStyle {
            archetype: W::Beam,
            thickness: 2.5,
            life_secs: 0.20,
        },
        BeamStyle {
            archetype: W::Ordnance,
            thickness: 4.5,
            life_secs: 0.40,
        },
        BeamStyle {
            archetype: W::Broadside,
            thickness: 5.5,
            life_secs: 0.26,
        },
        BeamStyle {
            archetype: W::Control,
            thickness: 2.0,
            life_secs: 0.30,
        },
        BeamStyle {
            archetype: W::Displacement,
            thickness: 3.0,
            life_secs: 0.24,
        },
        BeamStyle {
            archetype: W::Movement,
            thickness: 2.0,
            life_secs: 0.20,
        },
        BeamStyle {
            archetype: W::Defensive,
            thickness: 2.0,
            life_secs: 0.20,
        },
    ]
}
const fn default_enemy_tint() -> Rgb {
    Rgb([0.98, 0.34, 0.30])
}
const fn default_player_tint() -> Rgb {
    Rgb([0.40, 0.86, 1.0])
}
const fn default_travel_frac() -> f32 {
    0.4
}
const fn default_hit_alpha() -> f32 {
    0.95
}
const fn default_miss_alpha() -> f32 {
    0.45
}
const fn default_tip_len_frac() -> f32 {
    0.12
}
const fn default_tip_thickness_mul() -> f32 {
    1.4
}

// -- HitFlash --
const fn default_hit_color() -> Rgb {
    Rgb([1.0, 0.86, 0.45])
}
const fn default_hit_flash_secs() -> f32 {
    0.30
}
const fn default_hit_flash_peak() -> f32 {
    16.0
}
const fn default_flash_grow_base() -> f32 {
    0.35
}
const fn default_flash_grow_span() -> f32 {
    0.65
}
const fn default_flash_alpha() -> f32 {
    0.85
}

// -- Explosion --
/// (2026-07-01) Serde default for `Explosion.shape`: [`ShapeKind::Circle`]
/// so an unedited catalog reproduces the round bloom `emit_explosion` used
/// to hardcode via `PARTICLE_CIRCLE` — byte-identical to pre-fix output.
const fn default_explosion_shape() -> ShapeKind {
    ShapeKind::Circle
}
const fn default_explosion_secs() -> f32 {
    0.55
}
const fn default_explosion_peak() -> f32 {
    30.0
}
const fn default_explosion_color() -> Rgb {
    Rgb([1.0, 0.55, 0.25])
}
const fn default_shell_grow_base() -> f32 {
    0.25
}
const fn default_shell_grow_span() -> f32 {
    0.85
}
const fn default_shell_alpha() -> f32 {
    0.8
}
const fn default_core_color() -> Rgb {
    Rgb([1.0, 0.85, 0.4])
}
const fn default_core_life_frac() -> f32 {
    0.55
}
const fn default_core_alpha() -> f32 {
    0.9
}
const fn default_flash_color() -> Rgb {
    Rgb([1.0, 0.97, 0.9])
}
const fn default_ignition_life_frac() -> f32 {
    0.25
}
const fn default_ignition_alpha() -> f32 {
    0.95
}

// -- Trail --
const fn default_trail_color() -> Rgb {
    Rgb([0.95, 0.70, 0.35])
}
const fn default_trail_secs() -> f32 {
    0.35
}
const fn default_trail_thickness() -> f32 {
    3.0
}
const fn default_trail_alpha() -> f32 {
    0.9
}

// -- TelegraphFire --
const fn default_telegraph_color() -> Rgba {
    Rgba([0.95, 0.30, 0.30, 0.9])
}
const fn default_telegraph_fire_secs() -> f32 {
    0.32
}
const fn default_telegraph_slot_offset() -> f32 {
    -96.0
}
const fn default_telegraph_grow_base() -> f32 {
    0.4
}
const fn default_telegraph_grow_span() -> f32 {
    1.1
}
const fn default_telegraph_alpha() -> f32 {
    0.95
}

// -- ParticleBurst --
const fn default_burst_count() -> u32 {
    22
}
const fn default_burst_color() -> Rgba {
    Rgba([1.0, 0.72, 0.32, 1.0])
}
const fn default_burst_life_secs() -> f32 {
    0.55
}
const fn default_burst_speed_min() -> f32 {
    24.0
}
const fn default_burst_speed_max() -> f32 {
    94.0
}
const fn default_burst_size_min() -> f32 {
    2.0
}
const fn default_burst_size_max() -> f32 {
    5.0
}
const fn default_burst_dur_jitter() -> [f32; 2] {
    [0.7, 0.6]
}
const fn default_burst_drag() -> f32 {
    2.0
}
// (#217) Particle rotation defaults — `0.0` for all three so unedited bursts
// stay byte-identical (the SOLID_WHITE square is rotation-invariant anyway).
const fn default_burst_rotation_min() -> f32 {
    0.0
}
const fn default_burst_rotation_max() -> f32 {
    0.0
}
const fn default_burst_spin_rate() -> f32 {
    0.0
}

// -- ExplosionReflection (#209 hook 4) --
const fn default_reflection_color() -> Rgb {
    // Warm yellow-white — light bounce, not a hit.
    Rgb([1.0, 0.85, 0.55])
}
const fn default_reflection_life_secs() -> f32 {
    0.45
}
const fn default_reflection_peak_alpha() -> f32 {
    // (#321 Bruce ruling 2026-07-01) Default OFF: Bruce ruled the
    // ExplosionReflection cell-floor glow is the wrong mechanism -- the real
    // reflection is the hull SURFACE tinting per-normal via the loft shader's
    // ExplosionLight (#291), already wired live in the bin. `peak_alpha == 0`
    // short-circuits both the auto-cascade spawn in `CombatVfx::observe` and
    // the sprite emit in `emit_reflection_glow`, so no floor square renders
    // anymore. Any editor catalog that authors `peak_alpha > 0` re-enables
    // the old cell glow as an opt-in path -- the machinery stays but the
    // default is quiet. Was 0.35 pre-#321.
    0.0
}
const fn default_reflection_delay_per_cell() -> f32 {
    0.08
}

// Default impls delegate to the same fns so `EffectKind` variants built in code
// match the serde-default path exactly.
impl Default for ShotBeam {
    fn default() -> Self {
        Self {
            per_archetype: default_beam_styles(),
            enemy_tint: default_enemy_tint(),
            player_tint: default_player_tint(),
            travel_frac: default_travel_frac(),
            hit_alpha: default_hit_alpha(),
            miss_alpha: default_miss_alpha(),
            tip_len_frac: default_tip_len_frac(),
            tip_thickness_mul: default_tip_thickness_mul(),
        }
    }
}
impl Default for HitFlash {
    fn default() -> Self {
        Self {
            color: default_hit_color(),
            life_secs: default_hit_flash_secs(),
            peak_px: default_hit_flash_peak(),
            grow_base: default_flash_grow_base(),
            grow_span: default_flash_grow_span(),
            alpha_peak: default_flash_alpha(),
        }
    }
}
impl Default for Explosion {
    fn default() -> Self {
        Self {
            life_secs: default_explosion_secs(),
            peak_px: default_explosion_peak(),
            shell_color: default_explosion_color(),
            shell_grow_base: default_shell_grow_base(),
            shell_grow_span: default_shell_grow_span(),
            shell_alpha: default_shell_alpha(),
            core_color: default_core_color(),
            core_life_frac: default_core_life_frac(),
            core_alpha: default_core_alpha(),
            flash_color: default_flash_color(),
            flash_life_frac: default_ignition_life_frac(),
            flash_alpha: default_ignition_alpha(),
            shape: default_explosion_shape(),
            shapes: Vec::new(),
        }
    }
}
impl Default for Trail {
    fn default() -> Self {
        Self {
            color: default_trail_color(),
            life_secs: default_trail_secs(),
            thickness: default_trail_thickness(),
            alpha: default_trail_alpha(),
        }
    }
}
impl Default for TelegraphFire {
    fn default() -> Self {
        Self {
            color: default_telegraph_color(),
            life_secs: default_telegraph_fire_secs(),
            slot_offset_px: default_telegraph_slot_offset(),
            grow_base: default_telegraph_grow_base(),
            grow_span: default_telegraph_grow_span(),
            alpha: default_telegraph_alpha(),
        }
    }
}
impl Default for ParticleBurst {
    fn default() -> Self {
        Self {
            count: default_burst_count(),
            color: default_burst_color(),
            life_secs: default_burst_life_secs(),
            speed_min: default_burst_speed_min(),
            speed_max: default_burst_speed_max(),
            size_min: default_burst_size_min(),
            size_max: default_burst_size_max(),
            dur_jitter: default_burst_dur_jitter(),
            drag: default_burst_drag(),
            shape: ShapeKind::Square,
            rotation_min: default_burst_rotation_min(),
            rotation_max: default_burst_rotation_max(),
            spin_rate: default_burst_spin_rate(),
        }
    }
}

impl Default for ExplosionReflection {
    fn default() -> Self {
        Self {
            color: default_reflection_color(),
            life_secs: default_reflection_life_secs(),
            peak_alpha: default_reflection_peak_alpha(),
            delay_per_cell: default_reflection_delay_per_cell(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_catalog_round_trips() {
        let cat = EffectCatalog::default();
        let json = cat.to_json_string().unwrap();
        let back = EffectCatalog::from_json_str(&json).unwrap();
        assert_eq!(cat, back);
        assert!(back.effects.is_empty());
    }

    #[test]
    fn defaults_match_vfx_constants() {
        // The whole point of the schema: defaults reproduce today's vfx.rs
        // literals exactly, so an unedited catalog is behavior-identical.
        let beam = ShotBeam::default();
        assert_eq!(beam.enemy_tint, Rgb([0.98, 0.34, 0.30]));
        assert_eq!(beam.player_tint, Rgb([0.40, 0.86, 1.0]));
        assert!((beam.travel_frac - 0.4).abs() < f32::EPSILON);
        // Beam table has one row per WeaponArchetype (7).
        assert_eq!(beam.per_archetype.len(), 7);
        let ord = beam
            .per_archetype
            .iter()
            .find(|b| b.archetype == WeaponArchetype::Ordnance)
            .unwrap();
        assert!((ord.thickness - 4.5).abs() < f32::EPSILON);
        assert!((ord.life_secs - 0.40).abs() < f32::EPSILON);

        assert_eq!(HitFlash::default().color, Rgb([1.0, 0.86, 0.45]));
        assert!((HitFlash::default().life_secs - 0.30).abs() < f32::EPSILON);

        let ex = Explosion::default();
        assert_eq!(ex.shell_color, Rgb([1.0, 0.55, 0.25]));
        assert!((ex.peak_px - 30.0).abs() < f32::EPSILON);

        assert_eq!(Trail::default().color, Rgb([0.95, 0.70, 0.35]));
        assert_eq!(
            TelegraphFire::default().color,
            Rgba([0.95, 0.30, 0.30, 0.9])
        );

        let burst = ParticleBurst::default();
        assert_eq!(burst.count, 22);
        assert_eq!(burst.color, Rgba([1.0, 0.72, 0.32, 1.0]));
    }

    #[test]
    fn effect_def_serializes_with_kind_tag() {
        let def = EffectDef {
            id: "player_beam".into(),
            kind: EffectKind::ShotBeam(ShotBeam::default()),
        };
        let json = serde_json::to_string(&def).unwrap();
        // Internally tagged: the "kind" discriminator + the flattened id.
        assert!(json.contains("\"kind\":\"ShotBeam\""));
        assert!(json.contains("\"id\":\"player_beam\""));
        let back: EffectDef = serde_json::from_str(&json).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn partial_json_fills_defaults() {
        // A catalog entry that omits most params should fill them from defaults
        // (every field is #[serde(default)]), so partial authoring is safe.
        let json = r#"{ "effects": [ { "id": "spark", "kind": "HitFlash" } ] }"#;
        let cat = EffectCatalog::from_json_str(json).unwrap();
        let def = cat.get("spark").unwrap();
        match &def.kind {
            EffectKind::HitFlash(h) => {
                assert_eq!(h.color, Rgb([1.0, 0.86, 0.45]), "omitted color = default");
                assert!((h.peak_px - 16.0).abs() < f32::EPSILON);
            }
            other => panic!("expected HitFlash, got {other:?}"),
        }
    }

    #[test]
    fn sequence_round_trips_through_json() {
        // (#217) The composed-effect schema: a Sequence with named steps round-
        // trips end-to-end. The editor writes one of these when the user ticks
        // "include" on base families + dials per-step delays.
        let cat = EffectCatalog {
            effects: vec![
                EffectDef {
                    id: "spark".into(),
                    kind: EffectKind::HitFlash(HitFlash::default()),
                },
                EffectDef {
                    id: "boom".into(),
                    kind: EffectKind::Explosion(Explosion::default()),
                },
                EffectDef {
                    id: "player_kill".into(),
                    kind: EffectKind::Sequence(SequenceDef {
                        steps: vec![
                            SequenceStep {
                                id: "spark".into(),
                                delay_secs: 0.0,
                            },
                            SequenceStep {
                                id: "boom".into(),
                                delay_secs: 0.20,
                            },
                        ],
                    }),
                },
            ],
        };
        let json = cat.to_json_string().unwrap();
        assert!(
            json.contains("\"Sequence\""),
            "Sequence variant tag missing from JSON: {json}"
        );
        let back = EffectCatalog::from_json_str(&json).unwrap();
        assert_eq!(cat, back);
        match &back.get("player_kill").unwrap().kind {
            EffectKind::Sequence(s) => {
                assert_eq!(s.steps.len(), 2);
                assert_eq!(s.steps[0].id, "spark");
                assert!((s.steps[1].delay_secs - 0.20).abs() < f32::EPSILON);
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn shape_kind_default_is_square_for_back_compat() {
        // (#217) Unedited ParticleBurst → ShapeKind::Square so today's look
        // is byte-identical. Partial JSON missing `shape` also yields Square.
        assert_eq!(ParticleBurst::default().shape, ShapeKind::Square);
        let json = r#"{ "effects": [ { "id": "p", "kind": "ParticleBurst" } ] }"#;
        let cat = EffectCatalog::from_json_str(json).unwrap();
        if let EffectKind::ParticleBurst(p) = &cat.get("p").unwrap().kind {
            assert_eq!(p.shape, ShapeKind::Square);
            assert!((p.rotation_min - 0.0).abs() < f32::EPSILON);
            assert!((p.rotation_max - 0.0).abs() < f32::EPSILON);
            assert!((p.spin_rate - 0.0).abs() < f32::EPSILON);
        } else {
            panic!("expected ParticleBurst");
        }
    }

    #[test]
    fn shape_kind_wire_format_is_snake_case() {
        // (#217) Editor will read/write JSON values "square" / "circle" /
        // "triangle" / "line" — keep the wire format stable for round-trip.
        // (2026-07-01) Extended shape kit variants also use snake_case tags.
        for (variant, expected) in [
            (ShapeKind::Square, "\"square\""),
            (ShapeKind::Circle, "\"circle\""),
            (ShapeKind::Triangle, "\"triangle\""),
            (ShapeKind::Line, "\"line\""),
            (ShapeKind::Ring, "\"ring\""),
            (ShapeKind::HollowSquare, "\"hollow_square\""),
            (ShapeKind::Diamond, "\"diamond\""),
            (ShapeKind::Hexagon, "\"hexagon\""),
            (ShapeKind::Star4, "\"star4\""),
            (ShapeKind::Star5, "\"star5\""),
            (ShapeKind::Plus, "\"plus\""),
            (ShapeKind::X, "\"x\""),
            (ShapeKind::Crescent, "\"crescent\""),
        ] {
            assert_eq!(serde_json::to_string(&variant).unwrap(), expected);
            let back: ShapeKind = serde_json::from_str(expected).unwrap();
            assert_eq!(back, variant);
        }
    }

    /// (2026-07-01) `Explosion.shape` defaults to Circle so an unedited
    /// catalog reproduces the round bloom `emit_explosion` hardcoded before
    /// the shape-kit landed.
    #[test]
    fn explosion_shape_defaults_to_circle() {
        assert_eq!(Explosion::default().shape, ShapeKind::Circle);
        // A catalog entry without an explicit `shape` field round-trips as
        // Circle (not Square, the ParticleBurst default).
        let json = r#"{ "effects": [ { "id": "e", "kind": "Explosion" } ] }"#;
        let cat = EffectCatalog::from_json_str(json).unwrap();
        if let EffectKind::Explosion(ex) = &cat.get("e").unwrap().kind {
            assert_eq!(ex.shape, ShapeKind::Circle);
        } else {
            panic!("expected Explosion");
        }
    }

    /// (2026-07-01) Every extended `ShapeKind` variant round-trips through an
    /// `Explosion` catalog entry (JSON serialize + deserialize).
    #[test]
    fn explosion_shape_extended_round_trips() {
        let shapes = [
            ShapeKind::Ring,
            ShapeKind::HollowSquare,
            ShapeKind::Diamond,
            ShapeKind::Hexagon,
            ShapeKind::Star4,
            ShapeKind::Star5,
            ShapeKind::Plus,
            ShapeKind::X,
            ShapeKind::Crescent,
        ];
        for shape in shapes {
            let ex = Explosion {
                shape,
                ..Explosion::default()
            };
            let def = EffectDef {
                id: "test".into(),
                kind: EffectKind::Explosion(ex),
            };
            let json = serde_json::to_string(&def).unwrap();
            let back: EffectDef = serde_json::from_str(&json).unwrap();
            if let EffectKind::Explosion(ex_back) = back.kind {
                assert_eq!(
                    ex_back.shape, shape,
                    "round-trip failed for ShapeKind::{shape:?}"
                );
            } else {
                panic!("expected Explosion for {shape:?}");
            }
        }
    }

    #[test]
    fn catalog_lookup_by_id() {
        let cat = EffectCatalog {
            effects: vec![
                EffectDef {
                    id: "a".into(),
                    kind: EffectKind::Trail(Trail::default()),
                },
                EffectDef {
                    id: "b".into(),
                    kind: EffectKind::Explosion(Explosion::default()),
                },
            ],
        };
        assert!(cat.get("a").is_some());
        assert!(cat.get("b").is_some());
        assert!(cat.get("missing").is_none());
    }

    // (#218) Shape-stacker tests -------------------------------------------

    /// `ExplosionShapeLayer::default()` is Circle, rotation 0, alpha 1, scale 1.
    #[test]
    fn explosion_shape_layer_default_values() {
        let layer = ExplosionShapeLayer::default();
        assert_eq!(layer.shape, ShapeKind::Circle);
        assert!((layer.rotation_deg - 0.0).abs() < f32::EPSILON);
        assert!((layer.alpha - 1.0).abs() < f32::EPSILON);
        assert!((layer.scale_mul - 1.0).abs() < f32::EPSILON);
    }

    /// A 3-layer stack round-trips through JSON without loss.
    #[test]
    fn explosion_shape_layer_stack_round_trips() {
        let layers = vec![
            ExplosionShapeLayer {
                shape: ShapeKind::Square,
                rotation_deg: 0.0,
                alpha: 1.0,
                scale_mul: 1.0,
            },
            ExplosionShapeLayer {
                shape: ShapeKind::Square,
                rotation_deg: 45.0,
                alpha: 0.7,
                scale_mul: 1.0,
            },
            ExplosionShapeLayer {
                shape: ShapeKind::Square,
                rotation_deg: 90.0,
                alpha: 0.5,
                scale_mul: 0.9,
            },
        ];
        let ex = Explosion {
            shapes: layers,
            ..Explosion::default()
        };
        let def = EffectDef {
            id: "stacked".into(),
            kind: EffectKind::Explosion(ex),
        };
        let json = serde_json::to_string(&def).unwrap();
        let back: EffectDef = serde_json::from_str(&json).unwrap();
        if let EffectKind::Explosion(ex_back) = back.kind {
            assert_eq!(ex_back.shapes.len(), 3);
            assert!((ex_back.shapes[1].rotation_deg - 45.0).abs() < f32::EPSILON);
            assert!((ex_back.shapes[1].alpha - 0.7).abs() < f32::EPSILON);
            assert!((ex_back.shapes[2].rotation_deg - 90.0).abs() < f32::EPSILON);
            assert!((ex_back.shapes[2].scale_mul - 0.9).abs() < f32::EPSILON);
        } else {
            panic!("expected Explosion");
        }
    }

    /// `effective_layers()` with empty `shapes` yields the single legacy `shape` layer.
    #[test]
    fn effective_layers_empty_shapes_uses_legacy_shape() {
        let ex = Explosion {
            shape: ShapeKind::Diamond,
            shapes: vec![],
            ..Explosion::default()
        };
        let layers: Vec<_> = ex.effective_layers().collect();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].shape, ShapeKind::Diamond);
        assert!((layers[0].rotation_deg - 0.0).abs() < f32::EPSILON);
        assert!((layers[0].alpha - 1.0).abs() < f32::EPSILON);
    }

    /// `effective_layers()` with a non-empty `shapes` yields exactly those layers.
    #[test]
    fn effective_layers_nonempty_shapes_ignores_legacy_field() {
        let layers = vec![
            ExplosionShapeLayer {
                shape: ShapeKind::Ring,
                rotation_deg: 10.0,
                alpha: 0.8,
                scale_mul: 1.0,
            },
            ExplosionShapeLayer {
                shape: ShapeKind::Star4,
                rotation_deg: 20.0,
                alpha: 0.6,
                scale_mul: 1.1,
            },
        ];
        // Set legacy `shape` to something different to prove it's ignored.
        let ex = Explosion {
            shape: ShapeKind::Square,
            shapes: layers,
            ..Explosion::default()
        };
        let out: Vec<_> = ex.effective_layers().collect();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].shape, ShapeKind::Ring);
        assert_eq!(out[1].shape, ShapeKind::Star4);
    }

    /// Old catalog JSON with no `shapes` key round-trips as empty vec (backward compat).
    #[test]
    fn explosion_missing_shapes_key_deserialises_as_empty() {
        let json = r#"{ "effects": [ { "id": "boom", "kind": "Explosion" } ] }"#;
        let cat = EffectCatalog::from_json_str(json).unwrap();
        if let EffectKind::Explosion(ex) = &cat.get("boom").unwrap().kind {
            assert!(
                ex.shapes.is_empty(),
                "missing `shapes` key must yield empty vec"
            );
        } else {
            panic!("expected Explosion");
        }
    }
}
