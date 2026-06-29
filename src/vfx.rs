//! Combat juice — first-pass VFX plumbing (#51 / Phase D).
//!
//! Turns combat *events* into transient visuals: weapon-fire beams, ordnance
//! trails, hit flashes, destroy explosions, and the telegraphed-enemy-intent
//! cue. The LOOK here is deliberately first-pass placeholder-good (flat-colour
//! quads with eased fades); bruce iterates the art later, exactly like the ship
//! render. The durable value is the **plumbing**: event → effect → draw.
//!
//! ## How events are sourced — read-only state diff, never the resolver
//!
//! This module NEVER subscribes to the [`crate::types::EventBus`] and never
//! calls a resolver function. The reviewer's hard constraint is that VFX must
//! read combat events **read-only** and **not re-enter the resolver in a
//! callback** (the `EventBus` "no chained emit" invariant). We satisfy that *by
//! construction*: there is no callback at all. Instead [`CombatVfx::observe`]
//! diffs the current [`Board`] against the previous frame's snapshot and infers
//! what happened:
//! - a ship's `hull` dropped → hit flash (+ a beam from its nearest live
//!   opponent toward it),
//! - a ship `id` vanished → destroy explosion at its last cell,
//! - an ordnance moved/appeared → a fading trail along its path,
//! - an enemy holds a queued action → a telegraph cue above it.
//!
//! Pure state + math, no GPU and no `wgpu` types, so the whole module is
//! unit-testable headless. The caller ([`crate::hud`] / the bin) advances
//! lifetimes each frame and asks for the draw commands.

use crate::effects::{Explosion, HitFlash, ParticleBurst, ShotBeam, TelegraphFire, Trail};
use crate::gfx::{DrawCommand, SpriteInstance};
use crate::grid::Pos;
use crate::projector::{grid_cell_quad, ProjectorConfig};
use crate::types::{Board, Faction};
use crate::{atlas, types::Ship};
use std::collections::HashMap;
use std::sync::OnceLock;

// Effect lifetimes, colours, and curve params are no longer hardcoded here: they
// live in [`VfxConfig`] (sourced from [`crate::effects`]), so the VFX editor can
// author them as data. The shared default ([`default_vfx_config`]) reproduces the
// previous constants EXACTLY (the `effects::*` `Default`s == the old literals), so
// the game look is unchanged until a config is edited.
// The telegraph cue is not event-transient — it pulses while the intent is
// queued — so it has no lifetime; it's emitted live from the current board.

/// One transient effect with an eased 0→1 lifetime (`age / dur`).
#[derive(Clone, Copy, Debug)]
struct Effect {
    kind: EffectKind,
    age: f32,
    dur: f32,
}

#[derive(Clone, Copy, Debug)]
enum EffectKind {
    /// EXACT fired shot (#59): the resolver's per-round `FireEvent`, drawn as a
    /// styled beam attacker→target. `thickness` + `dur` come from the weapon
    /// archetype, `color` from the firing faction; a miss (`dim`) renders
    /// fainter. Replaces the old guessed nearest-opponent beam.
    ///
    /// (#201 bug 2) `from_pos` / `to_pos` are 2-D `Pos` (cell coords on the
    /// unified board), not the legacy 1-D flat indices. The emit helpers
    /// project them through the live `ProjectorConfig` via `grid_cell_quad` so
    /// the beam endpoints land on the correct 2-D cells.
    ShotBeam {
        from_pos: Pos,
        to_pos: Pos,
        color: [f32; 3],
        thickness: f32,
        dim: bool,
    },
    /// Expanding flash centred on a cell (a ship taking a hit).
    HitFlash { pos: Pos },
    /// Expanding ring + debris at a cell (a ship destroyed).
    Explosion { pos: Pos },
    /// Fading streak between two cells (ordnance step).
    Trail {
        from_pos: Pos,
        to_pos: Pos,
        color: [f32; 3],
    },
    /// A telegraph icon "spending" as its enemy fires (#70): a quick expanding
    /// red pop at the telegraph slot above the cell, so the player sees the
    /// readied action discharge rather than silently becoming the next intent.
    TelegraphFire { pos: Pos },
}

impl Effect {
    /// Lifetime fraction 0→1; >=1 means expired.
    fn t(&self) -> f32 {
        (self.age / self.dur).clamp(0.0, 1.0)
    }
    fn alive(&self) -> bool {
        self.age < self.dur
    }
}

/// Per-frame snapshot of the combat-relevant board state, used to diff the next
/// frame against. Cheap: a few small maps keyed by ship / projectile id.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    /// ship id → (hull, 2-D pos, faction). (#201 bug 2 migration: was 1-D `cell`.)
    ships: HashMap<String, (i32, Pos, Faction)>,
    /// projectile id → 2-D pos. (#201 bug 2 migration: was 1-D `cell`.)
    ordnance: HashMap<String, Pos>,
    /// enemy ship id → its telegraphed (queue head) action id, if any. Used to
    /// detect a FIRE: with the resolver's fire-then-decide model (#67/#162), an
    /// enemy's queue head changes the instant it spends its telegraphed action,
    /// so a head change = "this enemy just fired" — the signal for the
    /// shot-beam + telegraph-pop (#70), independent of whether the player's
    /// hull actually dropped (a shielded/missed shot still visibly fires).
    enemy_intent: HashMap<String, String>,
    /// A cheap signature of this frame's `board.fire_events` (#59). The resolver
    /// clears+repopulates the list each resolve round; the SAME list then
    /// persists across the many redraw frames until the next round. We latch the
    /// exact-shot beams once per round by spawning only when the signature
    /// CHANGES from the previous frame's — so a 2-event round followed by another
    /// 2-event round still re-fires (a bare count would miss that).
    fire_sig: u64,
}

impl Snapshot {
    fn of(board: &Board) -> Self {
        let mut ships = HashMap::new();
        let mut enemy_intent = HashMap::new();
        for s in board.cells.iter().flatten() {
            ships.insert(s.id.clone(), (s.hull, s.pos, s.faction));
            if s.faction == Faction::Enemy {
                if let Some(head) = s.queue.first() {
                    enemy_intent.insert(s.id.clone(), head.clone());
                }
            }
        }
        let mut ordnance = HashMap::new();
        for p in &board.ordnance {
            ordnance.insert(p.id.clone(), p.pos);
        }
        Self {
            ships,
            ordnance,
            enemy_intent,
            fire_sig: fire_events_sig(board),
        }
    }
}

/// Tunable VFX parameters, bundled so [`CombatVfx`] and [`ParticlePool`] can be
/// driven by authored data instead of module constants. Each field is one of the
/// [`crate::effects`] per-family structs; their `Default`s reproduce the original
/// `vfx.rs` constants exactly, so a default `VfxConfig` is behavior-identical to
/// the pre-data look. The VFX editor builds one from an
/// [`crate::effects::EffectCatalog`] and injects it via [`CombatVfx::with_config`]
/// / [`ParticlePool::with_config`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VfxConfig {
    /// Fired-shot beam style (per-archetype table, faction tints, travel/fade).
    pub shot_beam: ShotBeam,
    /// Hit-spark flash params.
    pub hit_flash: HitFlash,
    /// Destruction explosion params (shell / core / ignition).
    pub explosion: Explosion,
    /// Ordnance-trail params.
    pub trail: Trail,
    /// Telegraph-discharge pop params (also tints the steady telegraph cue).
    pub telegraph_fire: TelegraphFire,
    /// Screen-space particle-burst params.
    pub particle_burst: ParticleBurst,
}

impl VfxConfig {
    /// Build a [`VfxConfig`] by overlaying authored entries from an
    /// [`crate::effects::EffectCatalog`] on top of [`Self::default`]. Entries
    /// are looked up by the stable id constants in [`crate::effects`]
    /// (`ID_SHOT_BEAM`, `ID_HIT_FLASH`, …) so the editor's `to_catalog` and
    /// this loader cannot drift. Variant mismatches (e.g. a `ShotBeam` id
    /// carrying a `HitFlash` payload) are ignored, not silently misassigned;
    /// missing ids keep their defaults. Combined with the schema's
    /// `#[serde(default)]` this makes partial / hand-edited JSON safe —
    /// anything absent stays at the game's stock look.
    #[must_use]
    pub fn from_catalog(cat: &crate::effects::EffectCatalog) -> Self {
        use crate::effects::{
            EffectKind, ID_EXPLOSION, ID_HIT_FLASH, ID_PARTICLE_BURST, ID_SHOT_BEAM,
            ID_TELEGRAPH_FIRE, ID_TRAIL,
        };
        let mut cfg = Self::default();
        for def in &cat.effects {
            match (def.id.as_str(), &def.kind) {
                (ID_SHOT_BEAM, EffectKind::ShotBeam(v)) => cfg.shot_beam = v.clone(),
                (ID_HIT_FLASH, EffectKind::HitFlash(v)) => cfg.hit_flash = v.clone(),
                (ID_EXPLOSION, EffectKind::Explosion(v)) => cfg.explosion = v.clone(),
                (ID_TRAIL, EffectKind::Trail(v)) => cfg.trail = v.clone(),
                (ID_TELEGRAPH_FIRE, EffectKind::TelegraphFire(v)) => cfg.telegraph_fire = v.clone(),
                (ID_PARTICLE_BURST, EffectKind::ParticleBurst(v)) => cfg.particle_burst = v.clone(),
                _ => {}
            }
        }
        cfg
    }
}

/// The process-wide default [`VfxConfig`] — the SINGLE SOURCE the game reads.
/// Both [`CombatVfx::default`] and the 2-D scene compositor
/// ([`crate::hud::push_fire_2d`], via [`archetype_beam_style`] /
/// [`faction_beam_tint`]) resolve their styling from this, so the windowed `vfx`
/// beams and the live 2-D beams cannot diverge. The VFX editor overrides on a
/// per-[`CombatVfx`] instance via [`CombatVfx::with_config`] (it previews through
/// `CombatVfx`, not the hud path), so there is no divergence in practice.
pub(crate) fn default_vfx_config() -> &'static VfxConfig {
    static DEFAULT: OnceLock<VfxConfig> = OnceLock::new();
    DEFAULT.get_or_init(VfxConfig::default)
}

/// Live combat VFX state: the active transient effects + the previous frame's
/// snapshot for diffing + the tunable [`VfxConfig`]. Render-owned; the bin
/// advances it each frame.
#[derive(Default, Debug)]
pub struct CombatVfx {
    effects: Vec<Effect>,
    prev: Option<Snapshot>,
    /// Look/timing params. Defaults to [`VfxConfig::default`] (== the old
    /// constants); the VFX editor overrides via [`Self::with_config`].
    cfg: VfxConfig,
    /// (#209 hook 1) Wall-clock seconds the pool has been alive, advanced by
    /// [`Self::advance`]. Drives the READY-glow pulse phase for any ship with a
    /// non-empty queue (so the glow oscillates at a constant rate independent
    /// of which effects are alive). Read-only outside [`Self::emit`].
    anim_clock: f32,
}

impl CombatVfx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build with an authored [`VfxConfig`] (the VFX editor's path). The default
    /// constructor uses [`VfxConfig::default`], behavior-identical to the
    /// pre-data constants.
    #[must_use]
    pub fn with_config(cfg: VfxConfig) -> Self {
        Self {
            cfg,
            ..Self::default()
        }
    }

    /// Swap the active [`VfxConfig`] in place (the VFX editor's live-tune path).
    /// Already-spawned effects continue with their existing styling; subsequently
    /// spawned effects pick up the new config — exactly what authoring wants
    /// (drag a slider, the next `observe`-triggered effect uses the new value).
    // setter, const-marginal: VfxConfig owns heap data so the destructor of
    // the swapped-out value cannot be evaluated at compile-time (E0493).
    #[allow(clippy::missing_const_for_fn)]
    pub fn set_config(&mut self, cfg: VfxConfig) {
        self.cfg = cfg;
    }

    /// Diff `board` against the previous frame and spawn effects for the
    /// changes. Read-only over `board`. Call once per frame BEFORE [`advance`].
    pub fn observe(&mut self, board: &Board) {
        let cur = Snapshot::of(board);
        // Capture the previous frame's fire signature BEFORE `take()` empties it.
        let prev_sig = self.prev.as_ref().map_or(0, |p| p.fire_sig);
        if let Some(prev) = self.prev.take() {
            self.diff(&prev, &cur);
        }
        // EXACT fired shots (#59): latch the resolver's per-round FireEvent list
        // into styled ShotBeam effects. Read-only — the resolver owns
        // clear+repopulate; we COPY and animate with our own fade timers, never
        // mutate `board.fire_events`. Spawn once per round: only when this
        // frame's fire-event signature differs from the previous frame's (the
        // list persists across redraws until the next round repopulates it).
        if !board.fire_events.is_empty() && cur.fire_sig != prev_sig {
            for fe in &board.fire_events {
                let (thickness, dur) = archetype_beam_style(&self.cfg.shot_beam, fe.archetype);
                self.spawn(
                    EffectKind::ShotBeam {
                        from_pos: fe.from_pos,
                        to_pos: fe.to_pos,
                        color: faction_beam_tint(&self.cfg.shot_beam, fe.attacker_faction),
                        thickness,
                        dim: !fe.hit,
                    },
                    dur,
                );
            }
        }
        self.prev = Some(cur);
    }

    fn diff(&mut self, prev: &Snapshot, cur: &Snapshot) {
        // Ships: hull-drop → hit flash; vanished → explosion.
        for (id, &(prev_hull, prev_pos, _prev_faction)) in &prev.ships {
            match cur.ships.get(id) {
                Some(&(cur_hull, cur_pos, _)) => {
                    if cur_hull < prev_hull {
                        // Hull drop → the IMPACT (flash). The shot LINE itself now
                        // comes from the resolver's exact FireEvent (#59,
                        // ShotBeam in observe), not a guessed nearest-opponent
                        // beam — so we no longer fabricate an attacker here.
                        self.spawn(
                            EffectKind::HitFlash { pos: cur_pos },
                            self.cfg.hit_flash.life_secs,
                        );
                    }
                }
                None => {
                    // Ship gone this frame → destroyed at its last known cell.
                    self.spawn(
                        EffectKind::Explosion { pos: prev_pos },
                        self.cfg.explosion.life_secs,
                    );
                }
            }
        }
        // Telegraphed enemy FIRE (#70): an enemy whose queue head CHANGED since
        // last frame just spent its telegraphed action (fire-then-decide). Pop
        // the telegraph slot so the readied icon visibly DISCHARGES rather than
        // silently rolling to the next intent. The shot LINE is no longer drawn
        // here — the resolver's exact FireEvent (#59) draws the precise
        // attacker→target beam; this keeps only the slot-discharge pop.
        for (id, prev_head) in &prev.enemy_intent {
            // Enemy must still be alive this frame (a destroyed enemy is an
            // explosion, not a fire).
            let Some(&(_, cur_pos, _)) = cur.ships.get(id) else {
                continue;
            };
            let fired = match cur.enemy_intent.get(id) {
                Some(cur_head) => cur_head != prev_head, // swapped to next intent
                None => true,                            // queue emptied → spent
            };
            if fired {
                self.spawn(
                    EffectKind::TelegraphFire { pos: cur_pos },
                    self.cfg.telegraph_fire.life_secs,
                );
            }
        }
        // Ordnance: a projectile that moved leaves a trail along its step.
        for (id, &cur_pos) in &cur.ordnance {
            if let Some(&prev_pos) = prev.ordnance.get(id) {
                if prev_pos != cur_pos {
                    self.spawn(
                        EffectKind::Trail {
                            from_pos: prev_pos,
                            to_pos: cur_pos,
                            color: self.cfg.trail.color.0,
                        },
                        self.cfg.trail.life_secs,
                    );
                }
            }
        }
    }

    fn spawn(&mut self, kind: EffectKind, dur: f32) {
        self.effects.push(Effect {
            kind,
            age: 0.0,
            dur,
        });
    }

    /// Advance all effect lifetimes by `dt` seconds and drop expired ones.
    /// Returns `true` while any effect is still alive (so the caller keeps the
    /// redraw loop running until the juice settles).
    pub fn advance(&mut self, dt: f32) -> bool {
        for e in &mut self.effects {
            e.age += dt;
        }
        self.effects.retain(Effect::alive);
        // (#209 hook 1) Tick the wall-clock for the READY-glow pulse phase
        // independent of effect lifetimes — the glow has to pulse smoothly
        // while the queue is held even when no transient effect is alive.
        self.anim_clock += dt;
        !self.effects.is_empty()
    }

    /// True while any transient effect is active (redraw-keepalive helper).
    pub const fn is_active(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Emit draw commands for every active transient effect + the live
    /// telegraph cues (read from `board`). Append to `out`; ordered so juice
    /// sits above the ships but below modal overlays (the caller controls
    /// where in the command stream this runs).
    /// (#201 bug 2) Emit every live effect into `out`, projected through the
    /// LIVE 2-D [`ProjectorConfig`] so endpoints land on the correct cell quads
    /// (was previously the 1-D `LaneGeometry`, which mapped flat-index cells
    /// along a single horizontal line and never reached the unified board).
    /// Called from the bin's frame compose alongside `particles.emit` /
    /// `exhaust.emit`, after `observe(board)` + `advance(dt)`.
    pub fn emit(&self, out: &mut Vec<DrawCommand>, board: &Board, cfg: &ProjectorConfig) {
        for e in &self.effects {
            match e.kind {
                EffectKind::HitFlash { pos } => {
                    emit_flash(out, cfg, pos, e.t(), &self.cfg.hit_flash);
                }
                EffectKind::Explosion { pos } => {
                    emit_explosion(out, cfg, pos, e.t(), &self.cfg.explosion);
                }
                EffectKind::Trail {
                    from_pos,
                    to_pos,
                    color,
                } => emit_beam(out, cfg, from_pos, to_pos, color, e.t(), &self.cfg.trail),
                EffectKind::TelegraphFire { pos } => {
                    emit_telegraph_fire(out, cfg, pos, e.t(), &self.cfg.telegraph_fire);
                }
                EffectKind::ShotBeam {
                    from_pos,
                    to_pos,
                    color,
                    thickness,
                    dim,
                } => emit_shot_beam(
                    out,
                    cfg,
                    from_pos,
                    to_pos,
                    color,
                    thickness,
                    dim,
                    e.t(),
                    &self.cfg.shot_beam,
                ),
            }
        }
        // Telegraph: live cue above any enemy holding a queued action.
        // (#209 hook 1) READY-glow: pulsing aura around ANY ship (player +
        // enemy) with a non-empty queue — Bruce's "weapons charge while queued"
        // cue. Player-symmetric, where the existing telegraph cue above is
        // intentionally enemy-only (player has their own queue HUD strip).
        for s in board.cells.iter().flatten() {
            if !s.queue.is_empty() {
                emit_ready_glow(out, cfg, s, self.anim_clock, &self.cfg.telegraph_fire);
            }
            if s.faction == Faction::Enemy && !s.queue.is_empty() {
                emit_telegraph(out, cfg, s, &self.cfg.telegraph_fire);
            }
        }
    }
}

/// A cheap order-sensitive signature of `board.fire_events` (#59), so the latch
/// can tell "new round's shots" from "same round, redrawn again". Folds each
/// event's endpoints + archetype + faction + hit into a rolling hash. Distinct
/// rounds with identical shot lists hash the same (harmless — identical shots
/// would look the same anyway); the realistic case (different cells/weapons each
/// round) changes the hash so the beams re-fire.
fn fire_events_sig(board: &Board) -> u64 {
    let mut h: u64 = 1_469_598_103_934_665_603; // FNV-1a offset
    let mut fold = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(1_099_511_628_211);
    };
    for fe in &board.fire_events {
        fold(fe.from_cell as u64);
        fold(fe.to_cell as u64);
        fold(fe.archetype as u64);
        fold(fe.attacker_faction as u64);
        fold(u64::from(fe.hit));
    }
    fold(0xFF00 ^ board.fire_events.len() as u64);
    h
}

/* ---- draw helpers (flat-colour quads via SOLID_WHITE) --------------------- */

/// A beam / trail: a thin rectangle from `from`→`to` along the lane, fading out
/// over its lifetime. Rendered as a rotated `SpriteInstance`.
fn emit_beam(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    from_pos: Pos,
    to_pos: Pos,
    color: [f32; 3],
    t: f32,
    cfg: &Trail,
) {
    // (#201 bug 2) 2-D-correct endpoints: project each cell's screen-space
    // centre through the live unified camera, so the trail spans the actual
    // attacker→target cells on the perspective grid.
    let a = grid_cell_quad(from_pos, cfg_proj).center;
    let b = grid_cell_quad(to_pos, cfg_proj).center;
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = dx.hypot(dy).max(1.0);
    let cx = f32::midpoint(a[0], b[0]);
    let cy = f32::midpoint(a[1], b[1]);
    let alpha = (1.0 - t) * cfg.alpha; // fade out
    let thickness = cfg.thickness * (1.0 - t * 0.5); // thins slightly as it fades
    out.push(DrawCommand::Sprite(SpriteInstance {
        pos: [cx, cy],
        half_size: [len / 2.0, thickness / 2.0],
        color: [color[0], color[1], color[2], alpha],
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        rotation_rad: dy.atan2(dx),
        _pad: [0.0; 3],
    }));
}

/* ---- #59 exact attacker→target beam styling ------------------------------
 *
 * Per-archetype + per-faction styling for the resolver's per-round FireEvent
 * list. `observe` latches each `FireEvent { from_cell, to_cell, archetype,
 * attacker_faction, hit }` into a `ShotBeam` effect styled via
 * `archetype_beam_style` and tinted via `faction_beam_tint` (a miss —
 * `hit == false` — drawn dimmer in `emit_shot_beam`), giving the EXACT
 * attacker→target line instead of the old guessed nearest-opponent beam. The
 * resolver owns clear+repopulate each round; we COPY read-only and animate with
 * our own fade timers, never mutating `board.fire_events`. ----------------- */

/// Visual style for a fired shot, by weapon archetype: `(thickness, life_secs)`.
/// Reads the per-archetype table from `cfg` (data); falls back to the [`ShotBeam`]
/// default Beam row if an archetype is somehow absent (the default config never
/// is). `pub(crate)` so the 2-D scene compositor ([`crate::hud::push_fire_2d`])
/// styles its fire beams from the SAME config (single source) — pass it
/// [`default_vfx_config`]'s `shot_beam`.
pub(crate) fn archetype_beam_style(cfg: &ShotBeam, a: crate::types::WeaponArchetype) -> (f32, f32) {
    cfg.per_archetype
        .iter()
        .find(|b| b.archetype == a)
        .map_or((2.5, 0.20), |b| (b.thickness, b.life_secs))
}

/// Beam tint by the FIRING faction: enemy shots red, player shots cyan, so the
/// player can read at a glance who is shooting whom. Reads tints from `cfg`
/// (data); `pub(crate)` for the same single-source reason as
/// [`archetype_beam_style`].
pub(crate) const fn faction_beam_tint(cfg: &ShotBeam, f: Faction) -> [f32; 3] {
    match f {
        Faction::Enemy => cfg.enemy_tint.0,
        Faction::Player => cfg.player_tint.0,
    }
}

/// (#178 Bruce) Draw an EXACT fired shot (#59) as a REAL-TIME ANIMATED beam over
/// its wall-clock life `t` (0→1), not an instant-appear bolt.
///
/// Two phases. TRAVEL (`t` < `TRAVEL_FRAC`): a bright bolt races attacker→target —
/// the drawn segment runs from the muzzle to a HEAD that eases `a`→`b`, with a
/// brighter leading tip, so the shot visibly crosses the lane. STRIKE+FADE (`t` ≥
/// `TRAVEL_FRAC`): the head has arrived, so the full attacker→target beam is drawn
/// and fades + thins out over the remaining life. Archetype `thickness` + faction
/// `color` as before; a miss (`dim`) renders at reduced alpha ("fired but didn't
/// connect"). All on wall-clock, so the bolt crosses over real seconds regardless
/// of how the turn resolves.
#[allow(clippy::too_many_arguments)]
fn emit_shot_beam(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    from_pos: Pos,
    to_pos: Pos,
    color: [f32; 3],
    thickness: f32,
    dim: bool,
    t: f32,
    cfg: &ShotBeam,
) {
    // Fraction of the beam's life spent in the TRAVEL phase (bolt crossing the
    // lane); the rest is the strike + fade. From data (`ShotBeam.travel_frac`).
    let travel_frac = cfg.travel_frac;

    // (#201 bug 2) 2-D-correct endpoints via grid_cell_quad — the bolt travels
    // between the actual attacker / target cells on the unified perspective
    // grid (not the legacy 1-D LaneGeometry centerline).
    let a = grid_cell_quad(from_pos, cfg_proj).center;
    let b = grid_cell_quad(to_pos, cfg_proj).center;
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let rot = dy.atan2(dx);
    let base_alpha = if dim { cfg.miss_alpha } else { cfg.hit_alpha };
    let uv = atlas::cell_uvs(atlas::SOLID_WHITE);
    let mut seg = |p0: [f32; 2], p1: [f32; 2], th: f32, alpha: f32| {
        let len = (p1[0] - p0[0]).hypot(p1[1] - p0[1]).max(1.0);
        out.push(DrawCommand::Sprite(SpriteInstance {
            pos: [f32::midpoint(p0[0], p1[0]), f32::midpoint(p0[1], p1[1])],
            half_size: [len / 2.0, th / 2.0],
            color: [color[0], color[1], color[2], alpha],
            uv_min: uv.0,
            uv_max: uv.1,
            rotation_rad: rot,
            _pad: [0.0; 3],
        }));
    };

    if t < travel_frac {
        // TRAVEL: head eases muzzle→target; draw muzzle→head as the bolt body, plus
        // a brighter leading tip so the shot reads as a fast-moving round.
        let prog = (t / travel_frac).clamp(0.0, 1.0);
        let ease = 1.0 - (1.0 - prog) * (1.0 - prog); // ease-out
        let head = [a[0] + dx * ease, a[1] + dy * ease];
        seg(a, head, thickness, base_alpha);
        // Bright leading tip: a short over-bright stub at the head.
        let tip = cfg.tip_len_frac;
        let tail = [
            a[0] + dx * (ease - tip).max(0.0),
            a[1] + dy * (ease - tip).max(0.0),
        ];
        seg(
            tail,
            head,
            thickness * cfg.tip_thickness_mul,
            (base_alpha + 0.05).min(1.0),
        );
    } else {
        // STRIKE + FADE: full beam, fading + thinning over the remaining life.
        let f = ((t - travel_frac) / (1.0 - travel_frac)).clamp(0.0, 1.0);
        seg(a, b, thickness * (1.0 - f * 0.45), (1.0 - f) * base_alpha);
    }
}

/// A flash / hit-spark: an expanding, fading square centred on a cell. Colour /
/// peak size / grow curve / alpha come from [`HitFlash`] (data); the defaults
/// reproduce the prior `HIT_COLOR` + peak `16.0` + `0.35 + 0.65t` + `0.85`.
fn emit_flash(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    pos: Pos,
    t: f32,
    cfg: &HitFlash,
) {
    let p = grid_cell_quad(pos, cfg_proj).center;
    let color = cfg.color.0;
    // Ease-out grow; fade over life.
    let size = cfg.peak_px * (cfg.grow_base + cfg.grow_span * t);
    let alpha = (1.0 - t) * cfg.alpha_peak;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        p,
        [size / 2.0, size / 2.0],
        [color[0], color[1], color[2], alpha],
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// (#178 Bruce) A real-time EXPANDING explosion, composited from three eased flat
/// quads (`SOLID_WHITE`) over the effect's wall-clock life `t` (0→1, driven by the
/// pool's per-frame `advance(dt)` — NOT the turn beat). Bruce: "an explosion can
/// run in real time", not a static pop.
///
/// The three layers: an EXPANDING orange SHELL (the blast front — grows from a
/// small disc toward `~peak` while fading, ease-out so it bursts then settles; the
/// "expanding" Bruce called for); a HOT yellow CORE (smaller, shrinks + fades ~2×
/// faster than the shell, so the blast reads hottest at the middle early); and a
/// brief white IGNITION FLASH (over-bright, gone by ~t=0.25, sells the detonation
/// instant). `t` advances on real seconds, so the whole thing plays out over
/// `EXPLOSION_SECS` regardless of how the turn resolves — the `ParticlePool` burst
/// the bin seeds on the same kill layers debris on top.
fn emit_explosion(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    pos: Pos,
    t: f32,
    cfg: &Explosion,
) {
    let p = grid_cell_quad(pos, cfg_proj).center;
    let peak = cfg.peak_px;
    let mut quad = |size: f32, rgba: [f32; 4]| {
        out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
            p,
            [size * 0.5, size * 0.5],
            rgba,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        )));
    };
    // ease-out (fast then settle) for growth; linear-ish fades per layer.
    let ease_out = 1.0 - (1.0 - t) * (1.0 - t);

    // 2) Expanding orange shell — grows 0.25→1.1 of peak, fades over the whole life.
    let shell = cfg.shell_color.0;
    let shell_size = peak * (cfg.shell_grow_base + cfg.shell_grow_span * ease_out);
    let shell_alpha = (1.0 - t) * cfg.shell_alpha;
    if shell_alpha > 0.0 {
        quad(shell_size, [shell[0], shell[1], shell[2], shell_alpha]);
    }
    // 3) Hot yellow core — smaller, shrinks + fades by ~t=0.55 (2x the shell's rate).
    let core = cfg.core_color.0;
    let core_life = (t / cfg.core_life_frac).clamp(0.0, 1.0);
    if core_life < 1.0 {
        let core_size = peak * 0.5 * (0.5 + 0.5 * ease_out);
        let core_alpha = (1.0 - core_life) * cfg.core_alpha;
        quad(core_size, [core[0], core[1], core[2], core_alpha]);
    }
    // 1) White ignition flash — over-bright spike, gone by ~t=0.25.
    let fl = cfg.flash_color.0;
    let flash_life = (t / cfg.flash_life_frac).clamp(0.0, 1.0);
    if flash_life < 1.0 {
        let flash_size = peak * (0.4 + 0.3 * flash_life);
        let flash_alpha = (1.0 - flash_life) * cfg.flash_alpha;
        quad(flash_size, [fl[0], fl[1], fl[2], flash_alpha]);
    }
}

/// Telegraph FIRE pop (#70): a quick expanding red flash at the telegraph slot
/// above the enemy (`lane.center_y + slot_offset_px`, matching the hud telegraph
/// stack), so the readied action visibly DISCHARGES as the enemy fires rather
/// than silently rolling to the next intent. Grows + fades over its short life.
/// Slot offset / grow curve / alpha come from [`TelegraphFire`] (data). NOTE: the
/// pop's own tint `[1.0, 0.42, 0.38]` stays a literal — it is a DISTINCT colour
/// from the steady-cue tint (`TelegraphFire.color`, == the old `TELEGRAPH_COLOR`)
/// and the schema has one colour field, so keeping the pop literal preserves the
/// exact prior look. A future schema rev can add a `pop_color` field if desired.
fn emit_telegraph_fire(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    pos: Pos,
    t: f32,
    cfg: &TelegraphFire,
) {
    // (#201 bug 2) 2-D-correct anchor: use the cell's screen-space centre +
    // the configured slot offset, instead of the legacy lane.center_y constant.
    // The offset is now applied relative to the cell's projected y, so the pop
    // floats above the firing enemy's actual cell on the perspective grid.
    let p = grid_cell_quad(pos, cfg_proj).center;
    let y = p[1] + cfg.slot_offset_px;
    // Bright expanding ring-ish pop: a fast-growing, fast-fading square.
    let size = 18.0 * (cfg.grow_base + cfg.grow_span * t);
    let alpha = (1.0 - t) * cfg.alpha;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        [p[0], y],
        [size / 2.0, size / 2.0],
        [1.0, 0.42, 0.38, alpha],
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// Telegraph cue: a small red marker above an enemy holding a queued action,
/// signalling "this ship intends to act". First-pass = a chevron-ish bar; the
/// per-intent icon set is a later art pass. Tint + slot offset from
/// [`TelegraphFire`] (data; `color` == the old `TELEGRAPH_COLOR`).
fn emit_telegraph(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    ship: &Ship,
    cfg: &TelegraphFire,
) {
    // (#201 bug 2) Use the ship's 2-D cell centre + slot offset relative to
    // its projected y, so the cue floats above the actual ship on the grid
    // (not at the legacy lane-centerline constant).
    let p = grid_cell_quad(ship.pos, cfg_proj).center;
    let y = p[1] + cfg.slot_offset_px;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        [p[0], y],
        [6.0, 6.0],
        cfg.color.0,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// (#209 hook 1) READY-glow: a low, pulsing aura around any ship currently
/// holding a queued action — Bruce's "weapons charge while queued" cue. Drawn
/// HUGGING the cell quad (size = ~55% of the near-edge width), pulse phase
/// driven by `anim_clock` (wall-clock secs from [`CombatVfx::anim_clock`]).
/// Player-symmetric — runs for ANY faction with a non-empty queue.
///
/// Reuses [`TelegraphFire`]'s `color` + `alpha` so the editor's existing
/// telegraph slider tab tunes the ready-glow without new schema fields (alpha
/// is dimmed to 0.6× here so the glow reads as STEADY charge, distinct from
/// the discharge pop). A future `TelegraphFire.ready_glow_hz` schema field
/// lets the editor author the pulse rate too; today it's the constant
/// [`READY_GLOW_HZ`].
fn emit_ready_glow(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    ship: &Ship,
    anim_clock: f32,
    cfg: &TelegraphFire,
) {
    let q = grid_cell_quad(ship.pos, cfg_proj);
    let pulse = 0.55 + 0.45 * (anim_clock * std::f32::consts::TAU * READY_GLOW_HZ).sin();
    // Use the cell-quad's wider (bottom = near) edge as the size baseline so
    // the glow tracks live cell scale (#195) + camera zoom (#192) + perspective
    // automatically. Corners are [top-left, top-right, bottom-right, bottom-left];
    // bottom width = corners[2].x - corners[3].x.
    let near_w = (q.corners[2][0] - q.corners[3][0]).abs();
    let size = near_w * 0.55;
    let alpha = cfg.color.0[3] * pulse * 0.6;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        q.center,
        [size, size],
        [cfg.color.0[0], cfg.color.0[1], cfg.color.0[2], alpha],
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// (#209 hook 1) Pulse rate for the READY-glow aura — 1.5 Hz feels "alive but
/// not twitchy" per the design note. Reuses [`TelegraphFire`]'s alpha/colour
/// for now; if Bruce wants per-effect pulse-rate dialling we can add a
/// `TelegraphFire.ready_glow_hz` schema field later (one-line addition).
const READY_GLOW_HZ: f32 = 1.5;

/* =============================================================================
 * Procedural particle pool (#119) — SCREEN-SPACE, for the live 2-D board.
 *
 * The legacy `CombatVfx` effects above are 1-D LaneGeometry-positioned and aren't
 * on the live 2-D render path. The particle pool is the opposite: it works in
 * SCREEN SPACE so the caller (the bin) spawns a burst at a PROJECTED cell
 * position (via the projector it already holds for the 2-D compose) and the pool
 * integrates + emits SpriteInstances straight into the frame's draw list. Pure
 * math, no GPU types — headless-unit-testable like the rest of the module.
 *
 * Phase 1 use: a ship-death EXPLOSION burst, triggered at the bin's kill
 * detection. Later phases (muzzle flash, impact debris) reuse the same pool.
 * ========================================================================== */

/// One live particle in [`ParticlePool`]. `pos`/`vel` are SCREEN-SPACE (virtual
/// pixels and px/sec); `age`/`dur` drive the 0→1 lifetime; `size` is the birth
/// half-extent (shrinks with age); `color` is RGBA at birth (alpha fades).
#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub age: f32,
    pub dur: f32,
    pub size: f32,
    pub color: [f32; 4],
}

impl Particle {
    fn t(&self) -> f32 {
        (self.age / self.dur).clamp(0.0, 1.0)
    }
    fn alive(&self) -> bool {
        self.age < self.dur
    }
}

/// A small screen-space particle pool. The bin holds one, `spawn_burst`s at a
/// projected position on a combat event, `advance`s it each frame, and `emit`s
/// the live particles into the draw list. Capacity-free (Vec) but particles
/// self-expire, so it stays small in practice.
#[derive(Clone, Debug, Default)]
pub struct ParticlePool {
    particles: Vec<Particle>,
    /// Rolling seed for the deterministic spread (no RNG dep) — folded per spawn.
    seed: u64,
    /// Burst params (speeds/sizes/jitter/drag). Defaults reproduce the prior
    /// hardcodes; the VFX editor overrides via [`Self::with_config`].
    cfg: ParticleBurst,
}

impl ParticlePool {
    pub fn new() -> Self {
        Self {
            particles: Vec::new(),
            seed: 0x9E37_79B9_7F4A_7C15,
            cfg: ParticleBurst::default(),
        }
    }

    /// Build with authored [`ParticleBurst`] params (the VFX editor's path). The
    /// default constructor reproduces the prior hardcoded spread.
    #[must_use]
    pub fn with_config(cfg: ParticleBurst) -> Self {
        Self { cfg, ..Self::new() }
    }

    /// Swap the active [`ParticleBurst`] config in place (live-tune). Already-
    /// spawned particles continue with their existing speeds/sizes; subsequently
    /// spawned bursts use the new values.
    pub const fn set_config(&mut self, cfg: ParticleBurst) {
        self.cfg = cfg;
    }

    /// Seed `n` particles at `center` (screen space) flying outward with a
    /// DETERMINISTIC radial spread (FNV-style fold of a per-particle counter, the
    /// same no-RNG approach as `fire_events_sig`), so the burst is reproducible
    /// for headless capture/tests. `color` is the burst tint; `dur` its lifetime
    /// (seconds). Speed/size ranges + lifetime jitter come from [`ParticleBurst`]
    /// (data); the defaults reproduce the prior `24 + 0..70` speed, `2 + 0..3`
    /// size, `0.7 + 0..0.6` jitter.
    pub fn spawn_burst(&mut self, center: [f32; 2], n: u32, color: [f32; 4], dur: f32) {
        let (spd_min, spd_span) = (self.cfg.speed_min, self.cfg.speed_max - self.cfg.speed_min);
        let (sz_min, sz_span) = (self.cfg.size_min, self.cfg.size_max - self.cfg.size_min);
        let (jit_base, jit_span) = (self.cfg.dur_jitter[0], self.cfg.dur_jitter[1]);
        for i in 0..n {
            // FNV-1a fold of (seed, i) → three independent-ish [0,1) values.
            let mut h: u64 = 1_469_598_103_934_665_603;
            let mut fold = |v: u64| {
                h ^= v;
                h = h.wrapping_mul(1_099_511_628_211);
                ((h >> 11) & 0xFFFF) as f32 / 65535.0
            };
            let a01 = fold(self.seed ^ u64::from(i));
            let spd01 = fold(0xA1 ^ u64::from(i));
            let sz01 = fold(0xB2 ^ u64::from(i));
            let angle = a01 * std::f32::consts::TAU;
            let speed = spd_min + spd01 * spd_span; // px/sec, radial
            self.particles.push(Particle {
                pos: center,
                vel: [angle.cos() * speed, angle.sin() * speed],
                age: 0.0,
                dur: dur * (jit_base + sz01 * jit_span), // staggered lifetimes
                size: sz_min + sz01 * sz_span,
                color,
            });
        }
        // Advance the seed so successive bursts differ.
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
    }

    /// Integrate `pos += vel*dt`, age every particle by `dt`, drop the expired.
    /// Returns true while any particle is still alive (so the caller can keep
    /// requesting redraws). A light drag bleeds the velocity so the spray settles.
    pub fn advance(&mut self, dt: f32) -> bool {
        let drag_k = self.cfg.drag;
        for p in &mut self.particles {
            p.pos[0] += p.vel[0] * dt;
            p.pos[1] += p.vel[1] * dt;
            let drag = (1.0 - drag_k * dt).clamp(0.0, 1.0);
            p.vel[0] *= drag;
            p.vel[1] *= drag;
            p.age += dt;
        }
        self.particles.retain(Particle::alive);
        !self.particles.is_empty()
    }

    /// Push one SOLID_WHITE-tinted [`SpriteInstance`] per live particle: alpha =
    /// (1 − t) of the birth alpha, half-size shrinking with t (mirrors
    /// `emit_flash`). No-op when the pool is empty.
    pub fn emit(&self, out: &mut Vec<DrawCommand>) {
        for p in &self.particles {
            let t = p.t();
            let alpha = (1.0 - t) * p.color[3];
            if alpha <= 0.0 {
                continue;
            }
            let hs = (p.size * (1.0 - 0.6 * t)).max(0.5);
            out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
                p.pos,
                [hs, hs],
                [p.color[0], p.color[1], p.color[2], alpha],
                atlas::cell_uvs(atlas::SOLID_WHITE),
            )));
        }
    }

    /// Drop all particles (e.g. on restart so a fresh board shows no stale spray).
    pub fn clear(&mut self) {
        self.particles.clear();
    }

    /// Live particle count — for tests / debug.
    pub const fn len(&self) -> usize {
        self.particles.len()
    }

    /// True when no particles are live.
    pub const fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EventBus, LaneEnd, Orientation, Projectile, ShieldFace, ShieldProfile};

    fn empty_board(size: usize) -> Board {
        Board {
            size,
            cols: crate::grid::COLS,
            rows: crate::grid::ROWS,
            cells: (0..size).map(|_| None).collect(),
            ordnance: Vec::new(),
            hazards: (0..size).map(|_| Vec::new()).collect(),
            patrol: 1,
            level: 0,
            threats: Vec::new(),
            bus: EventBus::default(),
            destroys_this_window: 0,
            fire_events: vec![],
        }
    }

    fn ship(id: &str, faction: Faction, cell: usize, hull: i32) -> Ship {
        Ship {
            id: id.into(),
            faction,
            cell,
            pos: crate::grid::Pos::new(0, 0),
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
            facing: crate::grid::Facing::Bow(crate::grid::Dir4::S),
            hull,
            max_hull: 5,
            heat: 0,
            heat_max: 6,
            locked_out: false,
            shield_profile: ShieldProfile {
                bow: ShieldFace {
                    armour: 0,
                    charge: 0,
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
            },
            mounts: Vec::new(),
            queue: Vec::new(),
            cooldowns: std::collections::HashMap::new(),
            statuses: Vec::new(),
            traits: Vec::new(),
            klass: None,
        }
    }

    #[test]
    fn first_observe_spawns_nothing() {
        // No prev snapshot → no diff → no effects (just records the baseline).
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        vfx.observe(&board);
        assert!(
            !vfx.is_active(),
            "first frame establishes baseline, no effects"
        );
    }

    #[test]
    fn hull_drop_spawns_hit_flash_only() {
        // #59: the shot LINE now comes from the resolver's FireEvent, so a hull
        // drop spawns ONLY the impact flash here (no guessed nearest-opponent
        // beam anymore).
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        board.cells[3] = Some(ship("enemy", Faction::Enemy, 3, 5));
        vfx.observe(&board); // baseline
                             // Enemy takes a hit.
        board.cells[3].as_mut().unwrap().hull = 3;
        vfx.observe(&board);
        assert_eq!(vfx.effects.len(), 1, "hull drop = one hit flash only");
    }

    #[test]
    fn fire_event_spawns_exact_shot_beam() {
        // The consumer latches each per-round FireEvent into a styled ShotBeam.
        use crate::types::{FireEvent, WeaponArchetype};
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        board.cells[4] = Some(ship("enemy", Faction::Enemy, 4, 5));
        vfx.observe(&board); // baseline, no fire_events
        assert!(!vfx.is_active(), "no shots yet");
        // Resolver populates two shots this round (e.g. a broadside hitting two).
        board.fire_events = vec![
            FireEvent {
                from_cell: 4,
                to_cell: 0,
                from_pos: crate::grid::Pos::new(0, 0),
                to_pos: crate::grid::Pos::new(0, 0),
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Enemy,
                hit: true,
            },
            FireEvent {
                from_cell: 0,
                to_cell: 4,
                from_pos: crate::grid::Pos::new(0, 0),
                to_pos: crate::grid::Pos::new(0, 0),
                archetype: WeaponArchetype::Ordnance,
                attacker_faction: Faction::Player,
                hit: false,
            },
        ];
        vfx.observe(&board);
        let shots = vfx
            .effects
            .iter()
            .filter(|e| matches!(e.kind, EffectKind::ShotBeam { .. }))
            .count();
        assert_eq!(shots, 2, "two FireEvents → two exact shot beams");
        // Same list persisting next frame must NOT re-spawn (latch once/round).
        vfx.advance(0.001);
        let before = vfx.effects.len();
        vfx.observe(&board);
        let after = vfx
            .effects
            .iter()
            .filter(|e| matches!(e.kind, EffectKind::ShotBeam { .. }))
            .count();
        assert_eq!(
            after, before,
            "persisting fire_events must not re-spawn beams (signature unchanged)"
        );
    }

    #[test]
    fn vanished_ship_spawns_explosion() {
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[2] = Some(ship("doomed", Faction::Enemy, 2, 1));
        vfx.observe(&board); // baseline
        board.cells[2] = None; // destroyed
        vfx.observe(&board);
        assert_eq!(vfx.effects.len(), 1, "one explosion");
    }

    #[test]
    fn ordnance_step_spawns_trail() {
        // (#201 bug 2 migration) Snapshot keys on `proj.pos` (2-D); a move
        // requires the pos to change, not just the legacy 1-D cell index.
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        let mut proj = Projectile {
            id: "t1".into(),
            kind: "torpedo".into(),
            cell: 1,
            pos: crate::grid::Pos::new(0, 0),
            heading: LaneEnd::Fore,
            heading8: crate::grid::Dir8::N,
            speed: 1,
            hull: 1,
            payload: Vec::new(),
            owner_faction: Faction::Player,
        };
        board.ordnance.push(proj.clone());
        vfx.observe(&board); // baseline
        proj.cell = 2;
        proj.pos = crate::grid::Pos::new(0, 1);
        board.ordnance[0] = proj;
        vfx.observe(&board);
        assert_eq!(vfx.effects.len(), 1, "one ordnance trail");
    }

    #[test]
    fn advance_expires_effects() {
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[2] = Some(ship("doomed", Faction::Enemy, 2, 1));
        vfx.observe(&board);
        board.cells[2] = None;
        vfx.observe(&board);
        assert!(vfx.is_active());
        // Past the explosion lifetime → cleared. Lifetime now comes from the
        // (default) VfxConfig, which reproduces the old EXPLOSION_SECS (0.55).
        let still = vfx.advance(crate::effects::Explosion::default().life_secs + 0.01);
        assert!(!still);
        assert!(!vfx.is_active());
    }

    #[test]
    fn enemy_intent_change_spawns_fire_pop() {
        // With fire-then-decide, an enemy's queue head changes when it fires.
        // That should spawn the TelegraphFire POP (the icon discharge). The shot
        // LINE itself now comes from the resolver's FireEvent (#59), not here, so
        // the intent change alone spawns exactly the one pop.
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        let mut e = ship("enemy", Faction::Enemy, 4, 5);
        e.queue.push("beam_a".into());
        board.cells[4] = Some(e);
        vfx.observe(&board); // baseline: intent = beam_a
                             // Enemy fires → queue head rolls to its NEXT intent.
        board.cells[4].as_mut().unwrap().queue = vec!["beam_b".into()];
        vfx.observe(&board);
        assert_eq!(
            vfx.effects.len(),
            1,
            "intent change should spawn exactly the fire pop"
        );
    }

    #[test]
    fn enemy_intent_unchanged_spawns_nothing() {
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        let mut e = ship("enemy", Faction::Enemy, 4, 5);
        e.queue.push("beam_a".into());
        board.cells[4] = Some(e);
        vfx.observe(&board);
        vfx.observe(&board); // same intent, no change
        assert!(
            !vfx.is_active(),
            "a steady telegraph must not spawn a fire effect"
        );
    }

    #[test]
    fn telegraph_emits_for_enemy_with_queue() {
        let cfg = crate::projector::ProjectorConfig::for_scene(480.0, 270.0);
        let mut board = empty_board(7);
        let mut e = ship("enemy", Faction::Enemy, 4, 5);
        e.queue.push("pulse_laser".into());
        board.cells[4] = Some(e);
        let vfx = CombatVfx::new();
        let mut out = Vec::new();
        vfx.emit(&mut out, &board, &cfg);
        // (#209 hook 1) Queued ship now also gets a READY-glow aura, so a
        // queued enemy emits TWO cues: the steady glow (any-faction) + the
        // enemy-only telegraph chevron. The original telegraph contract is
        // preserved in the second draw.
        assert_eq!(
            out.len(),
            2,
            "ready-glow + telegraph cue for the queued enemy"
        );
    }

    // ---- (#119) particle pool ----

    #[test]
    fn particle_burst_spawns_n_and_emits_one_sprite_each() {
        let mut pool = ParticlePool::new();
        pool.spawn_burst([100.0, 50.0], 12, [1.0, 0.6, 0.2, 1.0], 0.5);
        assert_eq!(pool.len(), 12, "spawn_burst seeds exactly N particles");
        let mut out = Vec::new();
        pool.emit(&mut out);
        assert_eq!(
            out.len(),
            12,
            "one live SpriteInstance per particle at birth"
        );
        // All born at the burst centre.
        for c in &out {
            if let DrawCommand::Sprite(s) = c {
                assert_eq!(s.pos, [100.0, 50.0], "particles spawn at the burst centre");
            }
        }
    }

    #[test]
    fn particle_advance_moves_ages_and_expires() {
        let mut pool = ParticlePool::new();
        pool.spawn_burst([0.0, 0.0], 8, [1.0, 1.0, 1.0, 1.0], 0.4);
        // After a step, particles have moved off-centre (radial velocity).
        assert!(pool.advance(0.05), "still alive mid-life");
        let mut out = Vec::new();
        pool.emit(&mut out);
        let moved = out
            .iter()
            .any(|c| matches!(c, DrawCommand::Sprite(s) if s.pos != [0.0, 0.0]));
        assert!(moved, "advance integrates pos += vel*dt");
        // Past the max lifetime, every particle expires.
        let alive = pool.advance(10.0);
        assert!(
            !alive && pool.is_empty(),
            "all particles expire past their dur"
        );
    }

    #[test]
    fn particle_burst_is_deterministic() {
        // Same seed sequence → identical first burst (no RNG; reproducible for
        // headless capture).
        let mut a = ParticlePool::new();
        let mut b = ParticlePool::new();
        a.spawn_burst([10.0, 10.0], 6, [1.0, 0.5, 0.5, 1.0], 0.5);
        b.spawn_burst([10.0, 10.0], 6, [1.0, 0.5, 0.5, 1.0], 0.5);
        let (mut oa, mut ob) = (Vec::new(), Vec::new());
        a.emit(&mut oa);
        b.emit(&mut ob);
        assert_eq!(oa.len(), ob.len());
        for (ca, cb) in oa.iter().zip(ob.iter()) {
            if let (DrawCommand::Sprite(sa), DrawCommand::Sprite(sb)) = (ca, cb) {
                assert_eq!(sa.half_size, sb.half_size, "deterministic particle sizes");
            }
        }
    }

    #[test]
    fn particle_clear_drops_all() {
        let mut pool = ParticlePool::new();
        pool.spawn_burst([0.0, 0.0], 5, [1.0; 4], 0.5);
        pool.clear();
        assert!(pool.is_empty(), "clear drops every particle");
    }
}
