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

use crate::effects::{
    EffectCatalog, EffectKind as CatEffectKind, Explosion, ExplosionReflection, HitFlash,
    ParticleBurst, ShotBeam, TelegraphFire, Trail,
};
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

/// One transient effect with an eased 0→1 lifetime.
///
/// `age` advances every frame from [`CombatVfx::advance`]; `dur` is the total
/// time-on-pool including any leading `start_delay`. While `age < start_delay`
/// the effect is silent (not drawn, no visible progression); once past the
/// delay, `t()` runs 0→1 over the remaining `dur - start_delay` window. The
/// delay is what makes a multi-beam volley fire as a quick time-ordered
/// sequence (per-beam staggered `start_delay` in [`CombatVfx::observe`]) and an
/// explosion bloom AFTER its causing beam lands (delay = causing beam's
/// stagger + travel).
#[derive(Clone, Copy, Debug)]
struct Effect {
    kind: EffectKind,
    age: f32,
    dur: f32,
    /// Seconds of silence before the visible effect starts. `0.0` for the
    /// classic "spawn fires this frame" effects (hit flash, trail, queued
    /// glow, an unstaggered single-beam round).
    start_delay: f32,
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
    /// A telegraph icon "spending" as its enemy fires (#70). (#215 Bruce)
    /// REMOVED — the on-fire pop produced Bruce's "giant red blinking square"
    /// on small boards. The variant is preserved for editor-config schema
    /// compatibility (`TelegraphFire` struct lives in `effects.rs`), but
    /// nothing spawns this variant and the dispatch arm in `update` is a
    /// no-op. If a future redesign wants a discharge pop back, restore the
    /// spawn at `observe()` + the dispatch arm and tune the emitter small +
    /// on-grid.
    #[allow(dead_code)]
    TelegraphFire { pos: Pos },
    /// (#209 hook 4) Distance-delayed light bounce off a SURVIVING ship when a
    /// blast goes off elsewhere: `target_pos` is the surviving ship's cell;
    /// the per-instance seconds-of-silence BEFORE the glow appears (chebyshev
    /// distance × `delay_per_cell`) lives on the common [`Effect::start_delay`]
    /// field. The effect's total `dur` = `start_delay + life_secs`, so the
    /// pool keeps the effect alive long enough for the delayed glow to play.
    /// Drawn by `emit_reflection_glow`.
    ExplosionReflection { target_pos: Pos },
}

impl Effect {
    /// POST-DELAY lifetime fraction 0→1; clamped at 0 while `age < start_delay`
    /// (the effect is silent) and at 1 once the visible window is done. Callers
    /// should additionally check [`Self::visible`] to skip drawing during the
    /// silent lead-in (returning t=0 alone would still emit a "born" frame).
    fn t(&self) -> f32 {
        let visible_dur = (self.dur - self.start_delay).max(0.001);
        ((self.age - self.start_delay) / visible_dur).clamp(0.0, 1.0)
    }
    /// True once `age >= start_delay`: the visible window has begun.
    fn visible(&self) -> bool {
        self.age >= self.start_delay
    }
    fn alive(&self) -> bool {
        self.age < self.dur
    }
}

/// Per-beam stagger interval for a same-frame volley (#216). The resolver
/// commits a turn and `board.fire_events` holds the player's beams AND every
/// enemy's beams in one list — they all show up in [`CombatVfx::observe`] in
/// the same frame. Spawning them all at t=0 fires them in lockstep ("all at
/// once"); the fix is a per-event `start_delay = index * ENEMY_BEAT_SECS`,
/// preserving the resolver's insertion order (= the order the AI made the
/// shots happen). 0.12s reads as a quick rat-a-tat rather than a long lockout
/// — a 3-shot volley settles in under half a second.
const ENEMY_BEAT_SECS: f32 = 0.12;

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
    /// (#209 hook 4) Distance-delayed explosion-reflection params.
    pub explosion_reflection: ExplosionReflection,
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
            EffectKind, ID_EXPLOSION, ID_EXPLOSION_REFLECTION, ID_HIT_FLASH, ID_PARTICLE_BURST,
            ID_SHOT_BEAM, ID_TELEGRAPH_FIRE, ID_TRAIL,
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
                (ID_EXPLOSION_REFLECTION, EffectKind::ExplosionReflection(v)) => {
                    cfg.explosion_reflection = v.clone();
                }
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

    /// (#326) Drop every live effect + the diff snapshot. Called by the bin at
    /// a per-encounter boundary (warp-end board swap) so a previous encounter's
    /// residual `Explosion` / `HitFlash` / `ShotBeam` effects can't bleed into
    /// the next board. Mirrors the other per-encounter pool clears in the bin
    /// (`kill_bursts`, `particles`, `exhaust`). Resetting `prev` too means the
    /// next `observe` on the fresh board seeds from scratch (no phantom
    /// "everyone vanished" explosion cascade at swap time).
    pub fn clear(&mut self) {
        self.effects.clear();
        self.prev = None;
    }

    /// Diff `board` against the previous frame and spawn effects for the
    /// changes. Read-only over `board`. Call once per frame BEFORE [`advance`].
    pub fn observe(&mut self, board: &Board) {
        let cur = Snapshot::of(board);
        // Capture the previous frame's fire signature BEFORE `take()` empties it.
        let prev_sig = self.prev.as_ref().map_or(0, |p| p.fire_sig);
        // (#216) Build a per-target-cell delay map for THIS round's volley so
        // `diff` can delay an Explosion until its causing beam visually lands.
        // Key = causing beam's target Pos; value = (stagger_delay, travel_secs).
        // Only populated when this is a NEW round (fire_sig changed); otherwise
        // the list is yesterday's news and no delay is attributable to it.
        let mut kill_delays: HashMap<Pos, (f32, f32)> = HashMap::new();
        if !board.fire_events.is_empty() && cur.fire_sig != prev_sig {
            for (i, fe) in board.fire_events.iter().enumerate() {
                // EARLIEST hit on a cell wins: if two beams target the same
                // cell this round the explosion follows the first arrival,
                // not the last (later beams just splash into the wreckage).
                if kill_delays.contains_key(&fe.to_pos) {
                    continue;
                }
                let (_, life) = archetype_beam_style(&self.cfg.shot_beam, fe.archetype);
                let stagger = (i as f32) * ENEMY_BEAT_SECS;
                let travel = life * self.cfg.shot_beam.travel_frac;
                kill_delays.insert(fe.to_pos, (stagger, travel));
            }
        }
        if let Some(prev) = self.prev.take() {
            self.diff(&prev, &cur, &kill_delays);
        }
        // EXACT fired shots (#59): latch the resolver's per-round FireEvent list
        // into styled ShotBeam effects. Read-only — the resolver owns
        // clear+repopulate; we COPY and animate with our own fade timers, never
        // mutate `board.fire_events`. Spawn once per round: only when this
        // frame's fire-event signature differs from the previous frame's (the
        // list persists across redraws until the next round repopulates it).
        //
        // (#216) STAGGER same-frame beams by INSERTION ORDER: assigning each a
        // `start_delay = index * ENEMY_BEAT_SECS` turns a same-frame volley from
        // a single lockstep flash into a quick time-ordered sequence — the
        // resolver's insertion order IS the order the shots conceptually
        // happened, so the visual reads as "shots fire in sequence, not at
        // once."
        if !board.fire_events.is_empty() && cur.fire_sig != prev_sig {
            for (i, fe) in board.fire_events.iter().enumerate() {
                let (thickness, life) = archetype_beam_style(&self.cfg.shot_beam, fe.archetype);
                let start_delay = (i as f32) * ENEMY_BEAT_SECS;
                self.spawn_delayed(
                    EffectKind::ShotBeam {
                        from_pos: fe.from_pos,
                        to_pos: fe.to_pos,
                        color: faction_beam_tint(&self.cfg.shot_beam, fe.attacker_faction),
                        thickness,
                        dim: !fe.hit,
                    },
                    life,
                    start_delay,
                );
            }
        }
        self.prev = Some(cur);
    }

    /// (#217) Play a composed Sequence effect by id. Looks up the
    /// [`crate::effects::EffectKind::Sequence`] in `catalog`, then for each
    /// [`crate::effects::SequenceStep`] resolves the step's `id` to its base
    /// effect in the SAME catalog and spawns it onto the pool with
    /// `start_delay = step.delay_secs` (folded into the same machinery the
    /// `observe` volley stagger uses).
    ///
    /// `anchor` is the cell point-effects use (`HitFlash` / `Explosion` /
    /// `ExplosionReflection` / `ParticleBurst` centre). `target` is the second
    /// endpoint for line-effects (`ShotBeam` / `Trail`); if `None`, those
    /// steps fall back to `anchor` (a zero-length beam — handy for editor
    /// previews that don't have an attacker→target context).
    ///
    /// `particles`, if `Some`, also seeds any [`ParticleBurst`] steps onto
    /// the screen-space [`ParticlePool`] (so a Sequence that mixes pool and
    /// burst effects plays end-to-end). Pass `None` if you only want the
    /// `CombatVfx`-pool side (the editor preview case may not have a particle
    /// pool wired yet). The burst's `start_delay` is honored via the new
    /// `Particle.start_delay` field, mirroring the `Effect.start_delay`
    /// machinery that drives the combat-vfx pool.
    ///
    /// Returns the number of steps that resolved and spawned. Unresolved step
    /// ids (typo / catalog mismatch) and nested Sequence steps are logged
    /// and skipped without aborting the rest of the timeline. Calling with a
    /// non-Sequence `sequence_id` (or unknown id) returns `0` and spawns
    /// nothing.
    pub fn play_sequence(
        &mut self,
        catalog: &EffectCatalog,
        sequence_id: &str,
        anchor: Pos,
        target: Option<Pos>,
        mut particles: Option<&mut ParticlePool>,
    ) -> usize {
        let Some(def) = catalog.get(sequence_id) else {
            log::warn!("vfx::play_sequence: unknown id {sequence_id}");
            return 0;
        };
        let CatEffectKind::Sequence(seq) = &def.kind else {
            log::warn!(
                "vfx::play_sequence: id {sequence_id} is not a Sequence ({:?})",
                std::mem::discriminant(&def.kind)
            );
            return 0;
        };
        let to = target.unwrap_or(anchor);
        let mut scheduled = 0usize;
        for step in &seq.steps {
            let Some(step_def) = catalog.get(&step.id) else {
                log::warn!(
                    "vfx::play_sequence: step id '{}' unresolved in catalog (skipped)",
                    step.id
                );
                continue;
            };
            let delay = step.delay_secs.max(0.0);
            match &step_def.kind {
                CatEffectKind::ShotBeam(sb) => {
                    // Resolve to a default Beam archetype, player tint — the
                    // editor-driven preview doesn't carry attacker context;
                    // for runtime use, the caller can wrap play_sequence in
                    // their own helper that picks a tint.
                    let (thickness, life) =
                        archetype_beam_style(sb, crate::types::WeaponArchetype::Beam);
                    self.spawn_delayed(
                        EffectKind::ShotBeam {
                            from_pos: anchor,
                            to_pos: to,
                            color: sb.player_tint.0,
                            thickness,
                            dim: false,
                        },
                        life,
                        delay,
                    );
                    scheduled += 1;
                }
                CatEffectKind::HitFlash(hf) => {
                    self.spawn_delayed(EffectKind::HitFlash { pos: anchor }, hf.life_secs, delay);
                    scheduled += 1;
                }
                CatEffectKind::Explosion(ex) => {
                    self.spawn_delayed(EffectKind::Explosion { pos: anchor }, ex.life_secs, delay);
                    scheduled += 1;
                }
                CatEffectKind::Trail(t) => {
                    self.spawn_delayed(
                        EffectKind::Trail {
                            from_pos: anchor,
                            to_pos: to,
                            color: t.color.0,
                        },
                        t.life_secs,
                        delay,
                    );
                    scheduled += 1;
                }
                CatEffectKind::TelegraphFire(_) => {
                    // (#215) The TelegraphFire dispatch is a no-op in emit;
                    // spawning one is harmless but useless. Skip to keep the
                    // pool tight.
                }
                CatEffectKind::ExplosionReflection(er) => {
                    self.spawn_delayed(
                        EffectKind::ExplosionReflection { target_pos: anchor },
                        er.life_secs,
                        delay,
                    );
                    scheduled += 1;
                }
                CatEffectKind::ParticleBurst(pb) => {
                    if let Some(pool) = particles.as_deref_mut() {
                        // Project the anchor cell into screen space so the
                        // pool's screen-space integrator has a sensible
                        // centre. The caller is responsible for the
                        // ProjectorConfig — for now we use a default; a
                        // future overload can take it as an arg.
                        pool.spawn_burst_with(pb, [0.0, 0.0], delay);
                        scheduled += 1;
                    }
                }
                CatEffectKind::Sequence(_) => {
                    // No recursion (loop risk + unclear timing semantics).
                    log::warn!(
                        "vfx::play_sequence: nested Sequence step '{}' skipped (no recursion)",
                        step.id
                    );
                }
            }
        }
        scheduled
    }

    fn diff(&mut self, prev: &Snapshot, cur: &Snapshot, kill_delays: &HashMap<Pos, (f32, f32)>) {
        // Ships: hull-drop → hit flash; vanished → explosion.
        for (id, &(prev_hull, prev_pos, _prev_faction)) in &prev.ships {
            // (#209 hook 4) Both arms heavyweight (Some = hit flash; None =
            // explosion + reflection-spawn loop) → match reads clearer than
            // an if-let chain. Clippy's single-pattern heuristic misfires.
            #[allow(clippy::single_match)]
            #[allow(clippy::single_match_else)]
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
                    // (#216) EXPLOSION-AFTER-STRIKE: if THIS round's volley
                    // contains a beam targeting this ship's cell, the explosion
                    // is the consequence of that beam arriving — delay the
                    // bloom by `stagger + travel` so it kicks off the instant
                    // the beam visually lands, never concurrent with it.
                    // No matching beam (e.g. ordnance kill, self-destruct) →
                    // explosion fires immediately as before.
                    let blast_delay = kill_delays
                        .get(&prev_pos)
                        .map_or(0.0, |&(stagger, travel)| stagger + travel);
                    self.spawn_delayed(
                        EffectKind::Explosion { pos: prev_pos },
                        self.cfg.explosion.life_secs,
                        blast_delay,
                    );
                    // (#209 hook 4) Distance-delayed light bounce: for every
                    // ship STILL ALIVE this frame, spawn one ExplosionReflection
                    // with start_delay = chebyshev(blast, target) ×
                    // delay_per_cell, ADDITIVE on top of `blast_delay` so the
                    // reflection only starts radiating once the blast itself
                    // has begun. The pool keeps each alive for
                    // total_delay + life_secs so the delayed glow plays out.
                    // Chebyshev (max abs delta) matches how a player reads
                    // distance on a square grid (a diagonal-2 cell is "2 away",
                    // not √2). Skips the dying ship itself by construction
                    // (its id isn't in cur.ships).
                    //
                    // (#321 Bruce ruling 2026-07-01) Bruce: "the glow should be
                    // on the surface of the ship... changing the color of the
                    // surface in response to reflecting the light of the
                    // explosion." The FLAT CELL glow this cascade drives via
                    // `emit_reflection_glow` is the wrong mechanism — the real
                    // reflection is the loft shader's per-normal point-light
                    // driven by `brightest_explosion_light` (the hull surface
                    // itself tints, cf. #291). Gate the cascade on
                    // `peak_alpha > 0` so a zero-alpha default (the new
                    // ExplosionReflection default) never allocates a pool slot
                    // — spawn skipped by construction, no work, no wasted
                    // capacity. Setting peak_alpha > 0 in a catalog or via the
                    // editor re-enables the OLD cell-floor glow (kept as an
                    // opt-in path for future editor experiments).
                    if self.cfg.explosion_reflection.peak_alpha > 0.0 {
                        for (other_id, &(_, other_pos, _)) in &cur.ships {
                            if other_id == id {
                                continue;
                            }
                            let dx = (prev_pos.col as i32 - other_pos.col as i32).unsigned_abs();
                            let dy = (prev_pos.row as i32 - other_pos.row as i32).unsigned_abs();
                            let d = dx.max(dy) as f32;
                            let refl_delay =
                                blast_delay + d * self.cfg.explosion_reflection.delay_per_cell;
                            self.spawn_delayed(
                                EffectKind::ExplosionReflection {
                                    target_pos: other_pos,
                                },
                                self.cfg.explosion_reflection.life_secs,
                                refl_delay,
                            );
                        }
                    }
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
            let Some(&(_, _cur_pos, _)) = cur.ships.get(id) else {
                continue;
            };
            // (#215 Bruce) The on-fire TelegraphFire POP (an expanding red
            // square at the enemy cell when their queued head changes) was
            // creating the "giant red blinking square" Bruce kept seeing
            // — duplicated the HUD threat-outline cue (push_threats_2d on
            // c0caf7a, single source, dims-clamped) and visibly overpowered
            // the actual fire beam + impact spark on commit. Stop spawning.
            // The fire beam (emit_shot_beam) + impact spark
            // (hud::push_fire_2d) are the surviving "shot fired" cues.
            let _fired = match cur.enemy_intent.get(id) {
                Some(cur_head) => cur_head != prev_head,
                None => true,
            };
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
        self.spawn_delayed(kind, dur, 0.0);
    }

    /// Spawn an effect with `start_delay` seconds of silent lead-in. Total
    /// pool-time = `start_delay + visible_life`; pass `visible_life` as `dur`
    /// (the caller's perspective: "how long the effect plays") + the delay,
    /// so the visible window survives intact past the lead-in. Used to
    /// stagger same-frame beam volleys + delay explosions until the causing
    /// beam lands.
    fn spawn_delayed(&mut self, kind: EffectKind, visible_life: f32, start_delay: f32) {
        self.effects.push(Effect {
            kind,
            age: 0.0,
            dur: visible_life + start_delay,
            start_delay,
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

    /// (#291) Sample the LIVE `Explosion` effects and return the brightest one
    /// as a [`crate::loft_gpu::ExplosionLight`] for the loft shader's
    /// dynamic-point-light contribution. Returns `None` when no Explosion is
    /// alive (and visible — silent lead-in counts as no light), so the loft
    /// pipeline falls back to its byte-identical-default key+fill+ambient look.
    ///
    /// The "brightest" is `max(intensity = (1.0 - t) * peak_alpha * shell_alpha)`
    /// of the still-visible Explosions: a blast at t=0 lights the strongest,
    /// one near expiry barely lights at all. If multiple explode same-frame
    /// (rare — same kill never produces two) the closest-to-peak wins. The
    /// world-position is the cell centre via [`crate::projector::cell_world_center`]
    /// so it lines up with the hull `model` matrices the unified pass uses.
    ///
    /// The light's radius is sized to the configured `Explosion.peak_px`
    /// scaled by a fixed world-pixel ratio so a blast lights neighbouring
    /// cells (default 1.5 cell-widths of reach, tunable by Bruce later).
    /// Colour is `shell_color` (the warm orange front), so the bounce reads
    /// as a real reflection of the blast hue, not a generic white.
    #[must_use]
    pub fn brightest_explosion_light(
        &self,
        cfg: &ProjectorConfig,
    ) -> Option<crate::loft_gpu::ExplosionLight> {
        use crate::loft_gpu::ExplosionLight;
        let exp_cfg = &self.cfg.explosion;
        // World-units-per-cell for the radius mapping. The unified projector
        // packs cells into world space with one cell ≈ one world-unit edge
        // (see crate::projector::GRID_CELL_SCALE); the radius is the falloff
        // SCALE (1/(1+(d/r)²)). Sized large enough to reach a 4×3 board at
        // the cinematic camera (~6 world-units corner-to-corner) with a
        // visible contribution past the 8-band posterize threshold (each
        // band ≈ 0.125 in linear-RGB, so the cumulative
        // albedo×ndotl×intensity×falloff must exceed that to move a pixel).
        let cells_of_reach: f32 = 6.0;
        // Intensity multiplier into the shader. The Explosion shell_alpha is
        // ~0.8 default; multiplied through (1.0 - t) the visible window peaks
        // at ~0.8 and tapers to 0. The loft posterize discards anything below
        // a band step, so amplify the authored intensity into the per-pixel
        // shader contribution. 4.5× is the headroom Bruce can dial down by
        // eye later — for now the bias is "show the light is there".
        let intensity_scale: f32 = 4.5;
        let mut best: Option<(f32, &Effect, Pos)> = None;
        for e in &self.effects {
            let EffectKind::Explosion { pos } = e.kind else {
                continue;
            };
            if !e.visible() {
                continue;
            }
            let t = e.t();
            // Same fade curve emit_explosion uses for the shell layer: linear
            // (1 - t) × peak_alpha. Read from the live VfxConfig so the
            // editor's intensity dial drives both the visible bloom and the
            // hull bounce in lock-step.
            let intensity = (1.0 - t) * exp_cfg.shell_alpha;
            if intensity <= 0.0 {
                continue;
            }
            if best.is_none_or(|(b, _, _)| intensity > b) {
                best = Some((intensity, e, pos));
            }
        }
        let (intensity, _e, pos) = best?;
        let wc = crate::projector::cell_world_center(pos, cfg);
        Some(ExplosionLight {
            pos_world: [wc[0], wc[1], wc[2]],
            radius_world: cells_of_reach,
            color: exp_cfg.shell_color.0,
            intensity: intensity * intensity_scale,
        })
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
            // (#216) Silent lead-in: an effect with a positive `start_delay`
            // (staggered beam, post-strike explosion, distance-delayed
            // reflection) is alive but NOT drawn until its delay elapses. Same
            // gate for every variant, so spawn-order can't introduce a stray
            // "born at t=0" flash.
            if !e.visible() {
                continue;
            }
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
                // (#215 Bruce) The TelegraphFire pop is REMOVED — duplicated
                // the HUD threat outline cue + projected huge on small boards
                // (Bruce's "giant red blinking square"). The variant still
                // exists for editor-config schema compat; the dispatch is a
                // no-op so any leftover spawn (none today; observe() above no
                // longer spawns it) renders nothing.
                EffectKind::TelegraphFire { .. } => {}
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
                EffectKind::ExplosionReflection { target_pos } => {
                    emit_reflection_glow(
                        out,
                        cfg,
                        target_pos,
                        e.t(),
                        &self.cfg.explosion_reflection,
                    );
                }
            }
        }
        // (#209 hook 1) READY-glow per-mount markers: small per-queued-mount
        // markers seated at hull-relative offsets on any ship with a non-empty
        // queue (was a cell-center red square — Bruce's "floating square above
        // the ship," reshaped at 7748894). Player-symmetric so the player's
        // queued weapons read the same as enemies'.
        //
        // (#215 Bruce) The per-enemy steady `emit_telegraph` underglow (a
        // 12×12 red marker above each queued enemy) is REMOVED — duplicated
        // the HUD threat outline cue (push_threats_2d, dims-clamped + outline-
        // only since 4e214d6) and projected as a giant blinking square on
        // small boards (Bruce's "giant red BLINKING square"). The HUD threat
        // outline is now the SOLE enemy-intent cue.
        for s in board.cells.iter().flatten() {
            if !s.queue.is_empty() {
                emit_ready_glow(out, cfg, s, self.anim_clock, &self.cfg.telegraph_fire);
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
        // (#209 hook 5) MUZZLE FLASH: a one-shot over-bright pop at the firing
        // cell during the very first slice of travel — sells the "shot left the
        // muzzle" instant. Square quad at `a`, sized in pixels (so it reads at
        // any cell scale), fades over the first 30% of the travel phase. Goes
        // through emit_flash for free reuse of the easing — borrow the
        // ShotBeam color so player/enemy muzzles match their beam tint.
        let muzzle_t = (prog / 0.3).clamp(0.0, 1.0);
        if muzzle_t < 1.0 {
            let mflash_size = thickness * 4.0;
            let mflash_alpha = (1.0 - muzzle_t) * (base_alpha + 0.10).min(1.0);
            out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
                a,
                [mflash_size / 2.0, mflash_size / 2.0],
                [color[0], color[1], color[2], mflash_alpha],
                uv,
            )));
        }
    } else {
        // STRIKE + FADE: full beam, fading + thinning over the remaining life.
        // (#209 hook 5) Thickness now peaks at the strike instant then tapers,
        // not linear — `1 + 0.5*(1-f)^2 - 0.6f` reads as "punchy strike, then
        // settles" instead of slow uniform thinning. Bounded so a single
        // mis-tuned life_secs can't blow the half-size buffer.
        let f = ((t - travel_frac) / (1.0 - travel_frac)).clamp(0.0, 1.0);
        let strike_mul = (1.0 + 0.5 * (1.0 - f) * (1.0 - f) - 0.6 * f).clamp(0.3, 1.6);
        seg(a, b, thickness * strike_mul, (1.0 - f) * base_alpha);
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

/// (#178 Bruce) A real-time EXPANDING explosion, composited from three eased
/// ROUND quads over the effect's wall-clock life `t` (0→1, driven by the pool's
/// per-frame `advance(dt)` — NOT the turn beat). Bruce: "an explosion can run in
/// real time", not a static pop.
///
/// (#301 Bruce) Routes the three layers through the `PARTICLE_CIRCLE` atlas
/// silhouette instead of `SOLID_WHITE`, so the bloom reads as a ROUND burst
/// not an axis-aligned square. Bruce's "giant orange square" was this — the
/// quad geometry is unchanged (still square sprite primitives, since wgpu draws
/// rectangles), but the sampled silhouette is now a filled circle so the
/// rasterised result is a disc.
///
/// The three layers: an EXPANDING orange SHELL (the blast front — grows from a
/// small disc toward `~peak` while fading, ease-out so it bursts then settles; the
/// "expanding" Bruce called for); a HOT yellow CORE (smaller, shrinks + fades ~2×
/// faster than the shell, so the blast reads hottest at the middle early); and a
/// brief white IGNITION FLASH (over-bright, gone by ~t=0.25, sells the detonation
/// instant). `t` advances on real seconds, so the whole thing plays out over
/// `EXPLOSION_SECS` regardless of how the turn resolves — the `ParticlePool` burst
/// the bin seeds on the same kill layers debris on top.
/// (2026-07-01) Single-source `ShapeKind` → atlas-cell dispatch used by
/// both `emit_explosion` (the bloom silhouette) and `ParticlePool::emit`
/// (per-particle silhouette). Keeping ONE function guarantees the editor's
/// shape dropdown reads the same way regardless of where the shape is
/// consumed — an explosion with `shape: star5` and a burst particle with
/// `shape: star5` sample the identical atlas cell.
///
/// Square re-uses `SOLID_WHITE` (no extra atlas cost). Every other variant
/// resolves to a procedurally-drawn `PARTICLE_*` silhouette (see
/// `atlas.rs`).
const fn shape_cell(shape: crate::effects::ShapeKind) -> (u32, u32) {
    use crate::effects::ShapeKind as S;
    match shape {
        S::Square => atlas::SOLID_WHITE,
        S::Circle => atlas::PARTICLE_CIRCLE,
        S::Triangle => atlas::PARTICLE_TRIANGLE,
        S::Line => atlas::PARTICLE_LINE,
        S::Ring => atlas::PARTICLE_RING,
        S::HollowSquare => atlas::PARTICLE_HOLLOW_SQUARE,
        S::Diamond => atlas::PARTICLE_DIAMOND,
        S::Hexagon => atlas::PARTICLE_HEXAGON,
        S::Star4 => atlas::PARTICLE_STAR4,
        S::Star5 => atlas::PARTICLE_STAR5,
        S::Plus => atlas::PARTICLE_PLUS,
        S::X => atlas::PARTICLE_X,
        S::Crescent => atlas::PARTICLE_CRESCENT,
    }
}

fn emit_explosion(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    pos: Pos,
    t: f32,
    cfg: &Explosion,
) {
    let p = grid_cell_quad(pos, cfg_proj).center;
    let peak = cfg.peak_px;
    // ease-out (fast then settle) for growth; linear-ish fades per layer.
    let ease_out = 1.0 - (1.0 - t) * (1.0 - t);

    // (#218, 2026-07-01) Loop over the effective shape layers. When `shapes` is
    // empty, `effective_layers()` yields one Circle layer (backward-compat with
    // the pre-#218 single-shape path). When `shapes` is non-empty, each layer
    // contributes independent silhouette / rotation / alpha / scale_mul.
    //
    // Each layer draws the SAME shell/core/flash t-driven envelope using its
    // own atlas cell + rotation. The per-layer `alpha` multiplies the
    // t-driven alpha; `scale_mul` multiplies the computed size.
    for layer in cfg.effective_layers() {
        let bloom_uvs = atlas::cell_uvs(shape_cell(layer.shape));
        let rot = layer.rotation_deg.to_radians();
        let la = layer.alpha;
        let ls = layer.scale_mul;

        // Inline helper: emit one billboard quad with per-layer rotation.
        let mut quad = |size: f32, rgba: [f32; 4]| {
            let half = size * ls * 0.5;
            let (uv_min, uv_max) = bloom_uvs;
            out.push(DrawCommand::Sprite(SpriteInstance {
                pos: p,
                half_size: [half, half],
                color: [rgba[0], rgba[1], rgba[2], rgba[3] * la],
                uv_min,
                uv_max,
                rotation_rad: rot,
                _pad: [0.0; 3],
            }));
        };

        // 2) Expanding shell — grows 0.25→1.1 of peak, fades over the whole life.
        let shell = cfg.shell_color.0;
        let shell_size = peak * (cfg.shell_grow_base + cfg.shell_grow_span * ease_out);
        let shell_alpha = (1.0 - t) * cfg.shell_alpha;
        if shell_alpha > 0.0 {
            quad(shell_size, [shell[0], shell[1], shell[2], shell_alpha]);
        }
        // 3) Hot core — smaller, fades by ~t=0.55.
        let core = cfg.core_color.0;
        let core_life = (t / cfg.core_life_frac).clamp(0.0, 1.0);
        if core_life < 1.0 {
            let core_size = peak * 0.5 * (0.5 + 0.5 * ease_out);
            let core_alpha = (1.0 - core_life) * cfg.core_alpha;
            quad(core_size, [core[0], core[1], core[2], core_alpha]);
        }
        // 1) Ignition flash — gone by ~t=0.25.
        let fl = cfg.flash_color.0;
        let flash_life = (t / cfg.flash_life_frac).clamp(0.0, 1.0);
        if flash_life < 1.0 {
            let flash_size = peak * (0.4 + 0.3 * flash_life);
            let flash_alpha = (1.0 - flash_life) * cfg.flash_alpha;
            quad(flash_size, [fl[0], fl[1], fl[2], flash_alpha]);
        }
    }
}

/// (#218) Public thin wrapper around the private `emit_explosion` — exposed
/// exclusively for headless capture bins (e.g. `BROADSIDE_SHAPE_STACKER=1`).
/// Production code must go through `CombatVfx`; this fn is not part of the
/// stable API (breaking changes are fine).
#[doc(hidden)]
pub fn emit_explosion_pub(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &crate::projector::ProjectorConfig,
    pos: crate::grid::Pos,
    t: f32,
    cfg: &crate::effects::Explosion,
) {
    emit_explosion(out, cfg_proj, pos, t, cfg);
}

// (#215 Bruce) `emit_telegraph` (the steady per-queued-enemy red marker that
// floated above the hull) and `emit_telegraph_fire` (the expanding red pop
// on enemy fire) are REMOVED. Together they produced Bruce's "giant red
// blinking square" — the steady marker pulsed each new turn (= blink) and
// projected huge on small boards; the on-fire pop expanded over the ship.
// Both duplicated the HUD threat outline (push_threats_2d at hud.rs:1148,
// outline-only since 4e214d6, dims-clamped since 01cb79e), which is now the
// SOLE enemy-intent cue. The actual shot still renders via the shot beam
// (emit_shot_beam) + the impact spark (hud::push_fire_2d).
//
// The `TelegraphFire` schema struct is retained (editor config compat); the
// EffectKind::TelegraphFire dispatch arm in `update` is a no-op. If a
// future redesign wants a discharge pop back, restore one tiny on-grid
// sprite here.

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
    // (#215 Bruce) PER-MOUNT MARKERS — was a single cell-center red square the
    // size of half the cell ("a red square floating above the enemy" / "a red
    // square around the ship"). Bruce's design ask: "small red squares AROUND
    // WHERE THE WEAPONS ARE ON THE SHIPS — not a massive red floating square."
    // Draw one small marker per QUEUED mount, projected to a per-mount
    // hull-relative offset by `Arc` (Forward = bow, Rear = stern, BroadsideArc
    // = ports, Turret = stacked at the hull centre). Hull is in the lower half
    // of the cell trapezoid (the loft seats the hull at the cell's near edge),
    // so offsets are anchored to the cell's near-edge mid-point + a fraction
    // of the cell's near width.
    use crate::types::Arc as ShipArc;
    if ship.queue.is_empty() || ship.mounts.is_empty() {
        return;
    }
    let q = grid_cell_quad(ship.pos, cfg_proj);
    let pulse = 0.55 + 0.45 * (anim_clock * std::f32::consts::TAU * READY_GLOW_HZ).sin();
    // Cell-quad near edge (bottom on screen) — corners [top-left, top-right,
    // bottom-right, bottom-left]. Hull seats here.
    let near_w = (q.corners[2][0] - q.corners[3][0]).abs();
    let near_l = q.corners[3];
    let near_r = q.corners[2];
    let near_mid_x = (near_l[0] + near_r[0]) * 0.5;
    let near_mid_y = (near_l[1] + near_r[1]) * 0.5;
    // (team-lead 2026-06-29) Marker size: tiny — ~2.5px BASE × `depth_scale` so
    // a front-row marker reads as a small dot on the hull, far cells shrink to
    // a single pixel by perspective. Floor at 1px so back-row mounts still
    // draw something visible. Was `near_w * 0.06` clamped to [2..8] which
    // ran ~5-8px on 5x4 and pegged at 8px on 2x2 (cell near_w grows on small
    // boards) — Bruce read the 8px ceiling as "small red squares around the
    // ship" being still too big.
    let marker_half = (2.5 * q.depth_scale).max(1.0);
    // (team-lead 2026-06-29) Force RED for the ready-glow regardless of the
    // editor's TelegraphFire.color (per Bruce's literal "small red squares"
    // ask). The editor color stays available for the discharge pop if/when
    // that's revived; the in-flight ready cue is always red.
    let alpha = cfg.color.0[3] * pulse;
    let color = [0.95, 0.30, 0.30, alpha];
    // Per-mount hull offset, expressed as (dx_frac, dy_frac) of the near-edge
    // half-width, anchored at the near-edge midpoint. The hull occupies roughly
    // the cell's lower-mid area; offsets push markers toward the hull's bow /
    // stern / flanks.
    //   Forward     = bow:   up-screen (negative y) toward the cell's far edge
    //   Rear        = stern: at the near edge (where the hull seats)
    //   BroadsideArc = flanks: split left + right around the hull centre
    //   Turret      = no clear single mount; place near the hull centre
    // y is in screen pixels; cell-quad's near-to-far span = near_l.y - top edge.
    // Use a fraction of the near-edge width for y too so the offset shrinks
    // with depth (q.depth_scale folds in via near_w).
    let half_w = near_w * 0.5;
    // Cycle BroadsideArc mounts between port/starboard so a ship with multiple
    // broadside mounts shows distinct markers, not a stack.
    let mut bs_idx = 0usize;
    for (i, mount) in ship.mounts.iter().enumerate() {
        // Only draw a marker if THIS mount has its action id in the queue
        // (= the weapon Bruce is queueing-to-fire). A queued weapon that
        // isn't a mount-action (synth move, vent etc.) doesn't get a marker.
        let queued = ship.queue.iter().any(|a| a == &mount.weapon);
        if !queued {
            continue;
        }
        let (ox, oy) = match mount.arc {
            ShipArc::Forward => (0.0, -half_w * 0.55), // bow
            ShipArc::Rear => (0.0, half_w * 0.10),     // stern (near edge)
            ShipArc::BroadsideArc => {
                let side = if bs_idx.is_multiple_of(2) { -1.0 } else { 1.0 };
                bs_idx += 1;
                (side * half_w * 0.40, -half_w * 0.20)
            }
            ShipArc::Turret => {
                // Stack offset so multiple turrets don't overplot.
                let n = (i as f32) * 0.12;
                (n - half_w * 0.10, -half_w * 0.25)
            }
        };
        let cx = near_mid_x + ox;
        let cy = near_mid_y + oy;
        out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
            [cx, cy],
            [marker_half, marker_half],
            color,
            atlas::cell_uvs(atlas::SOLID_WHITE),
        )));
    }
}

/// (#209 hook 1) Pulse rate for the READY-glow aura — 1.5 Hz feels "alive but
/// not twitchy" per the design note. Reuses [`TelegraphFire`]'s alpha/colour
/// for now; if Bruce wants per-effect pulse-rate dialling we can add a
/// `TelegraphFire.ready_glow_hz` schema field later (one-line addition).
const READY_GLOW_HZ: f32 = 1.5;

/// (#209 hook 4) A distance-delayed reflection glow on a surviving ship's cell.
/// `local_t` is the post-delay 0→1 lifetime fraction (the caller subtracts
/// `start_delay` from the effect age + divides by `life_secs`). Eased
/// fade-in/fade-out: alpha = `peak_alpha · sin(π · t)`, so it ramps up then
/// settles symmetrically over the life. Drawn as a single `PARTICLE_CIRCLE`
/// quad at the cell centre, sized to the cell's near-edge width so the
/// highlight tracks live cell scale (#195) + camera zoom (#192) automatically.
///
/// (#321 render half 2026-06-30) Swap `SOLID_WHITE` → `PARTICLE_CIRCLE` to
/// match [`emit_explosion`]'s #301 fix: the reflection was the sibling layer
/// #301 missed, so surviving-ship cells still rendered a large axis-aligned
/// square instead of a round bloom. Same per-layer alpha/size/curve math;
/// only the atlas UV cell changed.
fn emit_reflection_glow(
    out: &mut Vec<DrawCommand>,
    cfg_proj: &ProjectorConfig,
    pos: Pos,
    local_t: f32,
    cfg: &ExplosionReflection,
) {
    let q = grid_cell_quad(pos, cfg_proj);
    let near_w = (q.corners[2][0] - q.corners[3][0]).abs();
    let size = near_w * 0.85;
    let alpha = cfg.peak_alpha * (local_t * std::f32::consts::PI).sin().max(0.0);
    if alpha <= 0.0 {
        return;
    }
    let c = cfg.color.0;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        q.center,
        [size / 2.0, size / 2.0],
        [c[0], c[1], c[2], alpha],
        atlas::cell_uvs(atlas::PARTICLE_CIRCLE),
    )));
}

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
///
/// (#217) `shape`, `rotation`, `spin_rate`, and `start_delay` are authored
/// per-burst from [`crate::effects::ParticleBurst`] and copied onto every
/// particle the burst spawns. `shape` picks the atlas silhouette in `emit`;
/// `rotation` is the current orientation in radians (advanced by `spin_rate *
/// dt` in `advance`); `start_delay` keeps the particle silent until age
/// elapses past it (mirrors [`Effect::start_delay`] on the combat-vfx side, so
/// a Sequence step can place a burst on the same timeline as a hit flash).
#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub age: f32,
    pub dur: f32,
    pub size: f32,
    pub color: [f32; 4],
    /// (#217) Silhouette atlas cell choice (Square/Circle/Triangle/Line).
    /// Defaults to `Square` for back-compat with the old `spawn_burst` path.
    pub shape: crate::effects::ShapeKind,
    /// (#217) Current orientation in radians. Drawn as `SpriteInstance.rotation_rad`.
    pub rotation: f32,
    /// (#217) Angular velocity radians/sec; `advance()` folds `rotation += spin_rate * dt`.
    pub spin_rate: f32,
    /// (#217) Silent lead-in in seconds — particle is alive in the pool but
    /// not drawn until `age >= start_delay`. Set from a Sequence step's
    /// `delay_secs`; classic bursts spawn with `0.0`.
    pub start_delay: f32,
}

impl Particle {
    fn t(&self) -> f32 {
        (self.age / self.dur).clamp(0.0, 1.0)
    }
    fn alive(&self) -> bool {
        self.age < self.dur
    }
    fn visible(&self) -> bool {
        self.age >= self.start_delay
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
        self.spawn_burst_inner(center, n, color, dur, &self.cfg.clone(), 0.0);
    }

    /// (#217) Spawn a burst seeded from `cfg` (shape / rotation range / spin
    /// rate / count / lifetime / colour), with an optional `start_delay` of
    /// silent lead-in. Used by [`CombatVfx::play_sequence`] to drive a
    /// Sequence-timed particle step on the same timeline as the rest of the
    /// composed effect. The classic [`Self::spawn_burst`] path stays
    /// byte-identical (it routes through this with `start_delay = 0.0` and the
    /// pool's existing `cfg`).
    pub fn spawn_burst_with(&mut self, cfg: &ParticleBurst, center: [f32; 2], start_delay: f32) {
        let dur = cfg.life_secs;
        let color = cfg.color.0;
        let n = cfg.count;
        self.spawn_burst_inner(center, n, color, dur, cfg, start_delay.max(0.0));
    }

    fn spawn_burst_inner(
        &mut self,
        center: [f32; 2],
        n: u32,
        color: [f32; 4],
        dur: f32,
        cfg: &ParticleBurst,
        start_delay: f32,
    ) {
        let (spd_min, spd_span) = (cfg.speed_min, cfg.speed_max - cfg.speed_min);
        let (sz_min, sz_span) = (cfg.size_min, cfg.size_max - cfg.size_min);
        let (jit_base, jit_span) = (cfg.dur_jitter[0], cfg.dur_jitter[1]);
        let rot_min = cfg.rotation_min;
        let rot_span = cfg.rotation_max - cfg.rotation_min;
        let shape = cfg.shape;
        let spin = cfg.spin_rate;
        for i in 0..n {
            // FNV-1a fold of (seed, i) → independent-ish [0,1) values.
            let mut h: u64 = 1_469_598_103_934_665_603;
            let mut fold = |v: u64| {
                h ^= v;
                h = h.wrapping_mul(1_099_511_628_211);
                ((h >> 11) & 0xFFFF) as f32 / 65535.0
            };
            let a01 = fold(self.seed ^ u64::from(i));
            let spd01 = fold(0xA1 ^ u64::from(i));
            let sz01 = fold(0xB2 ^ u64::from(i));
            // (#217) Separate FNV fold for the per-particle rotation pick so
            // a non-zero rotation range doesn't perturb the existing
            // size/speed picks → unedited bursts (rotation_min == rotation_max
            // == 0.0) stay byte-identical to today's deterministic spread.
            let rot01 = fold(0xC3 ^ u64::from(i));
            let angle = a01 * std::f32::consts::TAU;
            let speed = spd_min + spd01 * spd_span; // px/sec, radial
                                                    // Particle lives for (dur * jitter) AFTER the silent lead-in,
                                                    // so total pool-time = start_delay + dur*jitter (mirrors
                                                    // Effect.dur on the combat-vfx side).
            let visible_life = dur * (jit_base + sz01 * jit_span);
            self.particles.push(Particle {
                pos: center,
                vel: [angle.cos() * speed, angle.sin() * speed],
                age: 0.0,
                dur: visible_life + start_delay,
                size: sz_min + sz01 * sz_span,
                color,
                shape,
                rotation: rot_min + rot01 * rot_span,
                spin_rate: spin,
                start_delay,
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
            // (#217) Fold the per-particle spin into the current rotation.
            // Zero spin_rate (the default) is a no-op so unedited bursts stay
            // byte-identical.
            p.rotation += p.spin_rate * dt;
        }
        self.particles.retain(Particle::alive);
        !self.particles.is_empty()
    }

    /// Push one [`SpriteInstance`] per live particle. Alpha = (1 − t) of the
    /// birth alpha; half-size shrinks with t (mirrors `emit_flash`).
    /// (#217) Atlas cell is picked by [`Particle::shape`] — `Square` keeps
    /// `SOLID_WHITE` for byte-identity with the pre-Shape pool;
    /// `Circle`/`Triangle`/`Line` resolve to their dedicated silhouettes.
    /// The per-particle `rotation` drives `SpriteInstance.rotation_rad`, so a
    /// non-zero `spin_rate` or `rotation_min..max` range visibly turns the
    /// sprite. Particles still in their `start_delay` lead-in are skipped.
    /// No-op when the pool is empty.
    pub fn emit(&self, out: &mut Vec<DrawCommand>) {
        for p in &self.particles {
            if !p.visible() {
                continue;
            }
            let t = p.t();
            let alpha = (1.0 - t) * p.color[3];
            if alpha <= 0.0 {
                continue;
            }
            let hs = (p.size * (1.0 - 0.6 * t)).max(0.5);
            // (#217, 2026-07-01) Route ShapeKind through the single-source
            // `shape_cell` dispatch shared with `emit_explosion`, so a new
            // ShapeKind variant works both places from one match arm.
            // Line is the only aspect-non-square shape — its 6/32 cell
            // aspect wants Y scaled by ~0.20 so a horizontal-rotation Line
            // reads distinct; everything else is square-aspect at `hs`.
            let cell = shape_cell(p.shape);
            let half_size = if matches!(p.shape, crate::effects::ShapeKind::Line) {
                [hs, hs * 0.20]
            } else {
                [hs, hs]
            };
            let (uv_min, uv_max) = atlas::cell_uvs(cell);
            out.push(DrawCommand::Sprite(SpriteInstance {
                pos: p.pos,
                half_size,
                color: [p.color[0], p.color[1], p.color[2], alpha],
                uv_min,
                uv_max,
                rotation_rad: p.rotation,
                _pad: [0.0; 3],
            }));
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
            tail: None,
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
    fn same_frame_volley_is_staggered_by_insertion_order() {
        // (#216) Multiple same-frame FireEvents = enemy volley → each shot
        // must carry an INCREASING start_delay (idx * ENEMY_BEAT_SECS),
        // preserving the resolver's insertion order, so the beams animate as a
        // quick time-ordered sequence and never lockstep at t=0.
        use crate::types::{FireEvent, WeaponArchetype};
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        board.cells[4] = Some(ship("enemy", Faction::Enemy, 4, 5));
        vfx.observe(&board); // baseline
        board.fire_events = vec![
            FireEvent {
                from_cell: 4,
                to_cell: 0,
                from_pos: Pos::new(0, 0),
                to_pos: Pos::new(0, 0),
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Enemy,
                hit: true,
            },
            FireEvent {
                from_cell: 4,
                to_cell: 0,
                from_pos: Pos::new(0, 0),
                to_pos: Pos::new(0, 0),
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Enemy,
                hit: true,
            },
            FireEvent {
                from_cell: 4,
                to_cell: 0,
                from_pos: Pos::new(0, 0),
                to_pos: Pos::new(0, 0),
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Enemy,
                hit: true,
            },
        ];
        vfx.observe(&board);
        let shots: Vec<&Effect> = vfx
            .effects
            .iter()
            .filter(|e| matches!(e.kind, EffectKind::ShotBeam { .. }))
            .collect();
        assert_eq!(shots.len(), 3);
        // start_delays = [0, ENEMY_BEAT_SECS, 2*ENEMY_BEAT_SECS] in insertion order.
        assert!((shots[0].start_delay - 0.0).abs() < 1e-6);
        assert!((shots[1].start_delay - ENEMY_BEAT_SECS).abs() < 1e-6);
        assert!((shots[2].start_delay - 2.0 * ENEMY_BEAT_SECS).abs() < 1e-6);
        // The later shots must be SILENT at age 0 (visible() == false): proves
        // they don't render in lockstep with shot[0].
        assert!(shots[0].visible());
        assert!(!shots[1].visible());
        assert!(!shots[2].visible());
    }

    #[test]
    fn explosion_delays_until_causing_beam_lands() {
        // (#216) An Explosion spawned the same frame as the killing beam must
        // start AFTER the beam visibly arrives: start_delay = beam stagger +
        // beam travel time. Otherwise the bloom plays concurrent with the
        // beam's TRAVEL phase, reading as "instant pop."
        use crate::types::{FireEvent, WeaponArchetype};
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        board.cells[4] = Some(ship("doomed", Faction::Enemy, 4, 1));
        vfx.observe(&board); // baseline
                             // Killing shot: the SECOND event (so stagger = 1 * ENEMY_BEAT_SECS)
                             // tagged at the dying ship's cell (default Pos(0,0) per ship()).
        board.fire_events = vec![
            FireEvent {
                from_cell: 0,
                to_cell: 1, // unrelated
                from_pos: Pos::new(0, 0),
                to_pos: Pos::new(1, 1), // unrelated cell
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Player,
                hit: true,
            },
            FireEvent {
                from_cell: 0,
                to_cell: 4,
                from_pos: Pos::new(0, 0),
                to_pos: Pos::new(0, 0), // matches doomed.pos (default)
                archetype: WeaponArchetype::Beam,
                attacker_faction: Faction::Player,
                hit: true,
            },
        ];
        board.cells[4] = None; // doomed destroyed this frame
        vfx.observe(&board);
        let explosion = vfx
            .effects
            .iter()
            .find(|e| matches!(e.kind, EffectKind::Explosion { .. }))
            .expect("explosion spawned for the destroyed ship");
        // Causing beam = event[1]: stagger = ENEMY_BEAT_SECS; travel =
        // life_secs * travel_frac (from the default ShotBeam config).
        let beam_cfg = &VfxConfig::default().shot_beam;
        let (_, life) = archetype_beam_style(beam_cfg, crate::types::WeaponArchetype::Beam);
        let expected = ENEMY_BEAT_SECS + life * beam_cfg.travel_frac;
        assert!(
            (explosion.start_delay - expected).abs() < 1e-5,
            "explosion start_delay {} should equal stagger+travel {}",
            explosion.start_delay,
            expected
        );
        assert!(!explosion.visible(), "explosion is SILENT at age 0");
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
    fn emit_explosion_uses_round_bloom_atlas_cell() {
        // (#301 Bruce) The explosion's three bloom layers (shell + core +
        // ignition flash) must sample the PARTICLE_CIRCLE atlas silhouette,
        // not SOLID_WHITE, so the rasterised burst reads as a ROUND bloom
        // instead of the axis-aligned square Bruce kept seeing. If a future
        // edit flips the UVs back to SOLID_WHITE this test loud-fails
        // immediately rather than waiting for an eyeball capture pass.
        let cfg = crate::projector::ProjectorConfig::for_scene(480.0, 270.0);
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[2] = Some(ship("doomed", Faction::Enemy, 2, 1));
        vfx.observe(&board);
        board.cells[2] = None;
        vfx.observe(&board);
        // Advance into the middle of the explosion life so all three layers
        // (flash, core, shell) are visible.
        vfx.advance(0.15);
        let mut out = Vec::new();
        vfx.emit(&mut out, &board, &cfg);
        let circle_uv = atlas::cell_uvs(atlas::PARTICLE_CIRCLE);
        let any_round = out.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s) if s.uv_min == circle_uv.0 && s.uv_max == circle_uv.1)
        });
        assert!(
            any_round,
            "emit_explosion must sample PARTICLE_CIRCLE atlas cell so the bloom reads as ROUND"
        );
        // None of the explosion's three bloom layers should still sample
        // SOLID_WHITE — that's the square Bruce kept seeing. The ready-glow
        // pulse also uses SOLID_WHITE but spawns on QUEUED enemies (none
        // here), so any SOLID_WHITE sprite AT the killed cell is a leak.
        let square_uv = atlas::cell_uvs(atlas::SOLID_WHITE);
        let kill_center =
            crate::projector::grid_cell_quad(crate::grid::Pos::new(0, 0), &cfg).center;
        let leaked = out.iter().any(|c| {
            matches!(c, DrawCommand::Sprite(s)
                if s.uv_min == square_uv.0
                    && s.uv_max == square_uv.1
                    && (s.pos[0] - kill_center[0]).abs() < 1.0
                    && (s.pos[1] - kill_center[1]).abs() < 1.0)
        });
        assert!(
            !leaked,
            "no SOLID_WHITE square bloom layers should fire at the killed cell"
        );
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
    fn enemy_intent_change_spawns_no_pop() {
        // (#215) The TelegraphFire on-fire POP is REMOVED — it duplicated the
        // HUD threat outline and painted a giant red square on small boards.
        // The shot LINE comes from the resolver's FireEvent (#59); the HUD
        // outline is the queued-intent cue. An intent change alone now spawns
        // nothing on the vfx pool.
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
        assert!(
            !vfx.is_active(),
            "intent change alone spawns nothing (TelegraphFire pop stripped in #215)"
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
    fn ready_glow_emits_per_queued_mount() {
        // (#215) The steady per-enemy telegraph chevron is REMOVED (HUD threat
        // outline is now the queued-intent cue). The READY-glow (`#209 hook 1`)
        // survives BUT is drawn per QUEUED MOUNT, so a queued ship with no
        // mount whose `weapon` matches the queue entry emits nothing — the
        // marker only appears where the weapon physically sits on the hull.
        // Verify the queue-with-no-matching-mount case: zero draws.
        let cfg = crate::projector::ProjectorConfig::for_scene(480.0, 270.0);
        let mut board = empty_board(7);
        let mut e = ship("enemy", Faction::Enemy, 4, 5);
        e.queue.push("pulse_laser".into());
        board.cells[4] = Some(e);
        let vfx = CombatVfx::new();
        let mut out = Vec::new();
        vfx.emit(&mut out, &board, &cfg);
        assert!(
            out.is_empty(),
            "queued enemy with no matching mount → no per-mount marker; HUD owns the threat outline"
        );
    }

    // ---- (#217) Sequence playback + per-particle shapes ----

    #[test]
    fn play_sequence_schedules_steps_with_staggered_delays() {
        // The composed-effect contract: each step spawns a base effect on the
        // pool with start_delay = step.delay_secs (folded into the existing
        // visible() gate). Steps with unresolved ids are skipped without
        // aborting the rest of the timeline.
        use crate::effects::{
            EffectCatalog, EffectDef, EffectKind as CK, Explosion, HitFlash, SequenceDef,
            SequenceStep,
        };
        let cat = EffectCatalog {
            effects: vec![
                EffectDef {
                    id: "spark".into(),
                    kind: CK::HitFlash(HitFlash::default()),
                },
                EffectDef {
                    id: "boom".into(),
                    kind: CK::Explosion(Explosion::default()),
                },
                EffectDef {
                    id: "kill".into(),
                    kind: CK::Sequence(SequenceDef {
                        steps: vec![
                            SequenceStep {
                                id: "spark".into(),
                                delay_secs: 0.0,
                            },
                            SequenceStep {
                                id: "boom".into(),
                                delay_secs: 0.25,
                            },
                            SequenceStep {
                                id: "missing".into(), // unresolved, must be skipped
                                delay_secs: 0.10,
                            },
                        ],
                    }),
                },
            ],
        };
        let mut vfx = CombatVfx::new();
        let scheduled = vfx.play_sequence(&cat, "kill", Pos::new(0, 0), None, None);
        assert_eq!(scheduled, 2, "spark + boom resolved; missing skipped");
        let kinds: Vec<_> = vfx.effects.iter().map(|e| e.start_delay).collect();
        assert!(kinds.contains(&0.0), "spark immediate");
        assert!(
            kinds.iter().any(|d| (d - 0.25).abs() < 1e-6),
            "boom delayed by step.delay_secs"
        );
        // Spark is visible at age 0; boom must not be.
        let spark = vfx
            .effects
            .iter()
            .find(|e| matches!(e.kind, EffectKind::HitFlash { .. }))
            .unwrap();
        let boom = vfx
            .effects
            .iter()
            .find(|e| matches!(e.kind, EffectKind::Explosion { .. }))
            .unwrap();
        assert!(spark.visible());
        assert!(!boom.visible());
    }

    #[test]
    fn play_sequence_rejects_unknown_id_or_wrong_kind() {
        // Calling on a missing id returns 0 + spawns nothing; calling on a
        // non-Sequence id returns 0 too (the editor only writes Sequence into
        // the play_sequence path; misuse must NOT spawn ghost effects).
        use crate::effects::{EffectCatalog, EffectDef, EffectKind as CK, HitFlash};
        let cat = EffectCatalog {
            effects: vec![EffectDef {
                id: "spark".into(),
                kind: CK::HitFlash(HitFlash::default()),
            }],
        };
        let mut vfx = CombatVfx::new();
        assert_eq!(
            vfx.play_sequence(&cat, "missing", Pos::new(0, 0), None, None),
            0
        );
        assert!(vfx.effects.is_empty());
        assert_eq!(
            vfx.play_sequence(&cat, "spark", Pos::new(0, 0), None, None),
            0,
            "non-Sequence id must not schedule anything"
        );
        assert!(vfx.effects.is_empty());
    }

    #[test]
    fn play_sequence_drives_particle_pool() {
        // ParticleBurst steps route through ParticlePool::spawn_burst_with.
        use crate::effects::{
            EffectCatalog, EffectDef, EffectKind as CK, ParticleBurst, SequenceDef, SequenceStep,
        };
        let cat = EffectCatalog {
            effects: vec![
                EffectDef {
                    id: "spray".into(),
                    kind: CK::ParticleBurst(ParticleBurst {
                        count: 4,
                        ..Default::default()
                    }),
                },
                EffectDef {
                    id: "spritz".into(),
                    kind: CK::Sequence(SequenceDef {
                        steps: vec![SequenceStep {
                            id: "spray".into(),
                            delay_secs: 0.0,
                        }],
                    }),
                },
            ],
        };
        let mut vfx = CombatVfx::new();
        let mut pool = ParticlePool::new();
        let scheduled = vfx.play_sequence(&cat, "spritz", Pos::new(0, 0), None, Some(&mut pool));
        assert_eq!(scheduled, 1);
        assert_eq!(pool.len(), 4, "particle burst seeded N particles");
    }

    #[test]
    fn particle_shape_drives_atlas_cell() {
        // (#217) A burst configured with ShapeKind::Triangle emits sprites
        // pointing at the PARTICLE_TRIANGLE atlas cell, distinct from
        // SOLID_WHITE — proves the shape selection routes through emit.
        let mut pool = ParticlePool::with_config(crate::effects::ParticleBurst {
            count: 6,
            shape: crate::effects::ShapeKind::Triangle,
            ..Default::default()
        });
        pool.spawn_burst([0.0, 0.0], 6, [1.0; 4], 0.5);
        let mut out = Vec::new();
        pool.emit(&mut out);
        let tri_uv = atlas::cell_uvs(atlas::PARTICLE_TRIANGLE);
        let white_uv = atlas::cell_uvs(atlas::SOLID_WHITE);
        assert!(out.iter().all(|c| match c {
            DrawCommand::Sprite(s) => s.uv_min == tri_uv.0 && s.uv_max == tri_uv.1,
            _ => false,
        }));
        assert_ne!(tri_uv, white_uv, "triangle silhouette has its own cell");
    }

    #[test]
    fn particle_spin_rotates_over_time() {
        // (#217) A non-zero spin_rate advances per-particle rotation each
        // frame; integrated rotation ~= spin_rate * dt.
        let mut pool = ParticlePool::with_config(crate::effects::ParticleBurst {
            count: 1,
            spin_rate: std::f32::consts::PI, // 180°/sec
            ..Default::default()
        });
        pool.spawn_burst([0.0, 0.0], 1, [1.0; 4], 1.0);
        pool.advance(0.5);
        let mut out = Vec::new();
        pool.emit(&mut out);
        if let DrawCommand::Sprite(s) = &out[0] {
            assert!(
                (s.rotation_rad - std::f32::consts::FRAC_PI_2).abs() < 1e-4,
                "0.5s * PI rad/s = PI/2, got {}",
                s.rotation_rad
            );
        } else {
            panic!("expected Sprite");
        }
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

    // (#218) emit_explosion layer-count tests --------------------------------

    /// Single-layer (legacy path): `emit_explosion` with empty `shapes` emits the
    /// same sprite count as it always did — 3 quads at t=0 (shell + core + flash
    /// are all live at t=0).
    #[test]
    fn emit_explosion_single_layer_emits_three_quads_at_t0() {
        use crate::effects::Explosion;
        let cfg_proj = crate::projector::ProjectorConfig::default();
        let ex = Explosion::default(); // shapes: empty → falls back to single Circle
        let mut out = Vec::new();
        emit_explosion(&mut out, &cfg_proj, crate::grid::Pos::new(2, 2), 0.0, &ex);
        assert_eq!(out.len(), 3, "shell + core + flash = 3 quads at t=0");
    }

    /// Three-layer stack: `emit_explosion` emits 3× the quads vs single-layer
    /// (each of 3 layers draws shell+core+flash at t=0).
    #[test]
    fn emit_explosion_three_layers_emit_triple_quads() {
        use crate::effects::{Explosion, ExplosionShapeLayer, ShapeKind};
        let cfg_proj = crate::projector::ProjectorConfig::default();
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
                scale_mul: 1.0,
            },
        ];
        let ex = Explosion {
            shapes: layers,
            ..Explosion::default()
        };
        let mut out = Vec::new();
        emit_explosion(&mut out, &cfg_proj, crate::grid::Pos::new(2, 2), 0.0, &ex);
        // 3 layers × 3 quads each = 9
        assert_eq!(
            out.len(),
            9,
            "3 layers × (shell+core+flash) = 9 quads at t=0"
        );
    }

    /// Per-layer rotation is applied: layer at 45° has `rotation_rad` ≈ π/4 on sprites.
    #[test]
    fn emit_explosion_layer_rotation_reaches_sprite() {
        use crate::effects::{Explosion, ExplosionShapeLayer, ShapeKind};
        let cfg_proj = crate::projector::ProjectorConfig::default();
        let ex = Explosion {
            shapes: vec![ExplosionShapeLayer {
                shape: ShapeKind::Diamond,
                rotation_deg: 45.0,
                alpha: 1.0,
                scale_mul: 1.0,
            }],
            ..Explosion::default()
        };
        let mut out = Vec::new();
        emit_explosion(&mut out, &cfg_proj, crate::grid::Pos::new(2, 2), 0.0, &ex);
        assert!(!out.is_empty());
        for cmd in &out {
            if let DrawCommand::Sprite(s) = cmd {
                assert!(
                    (s.rotation_rad - std::f32::consts::FRAC_PI_4).abs() < 1e-4,
                    "expected π/4 rotation, got {}",
                    s.rotation_rad
                );
            }
        }
    }

    /// Per-layer alpha multiplier reduces the sprite alpha vs a full-alpha layer.
    #[test]
    fn emit_explosion_layer_alpha_multiplies_sprite_alpha() {
        use crate::effects::{Explosion, ExplosionShapeLayer, ShapeKind};
        let cfg_proj = crate::projector::ProjectorConfig::default();
        let full_alpha_ex = Explosion {
            shapes: vec![ExplosionShapeLayer {
                shape: ShapeKind::Circle,
                rotation_deg: 0.0,
                alpha: 1.0,
                scale_mul: 1.0,
            }],
            ..Explosion::default()
        };
        let half_alpha_ex = Explosion {
            shapes: vec![ExplosionShapeLayer {
                shape: ShapeKind::Circle,
                rotation_deg: 0.0,
                alpha: 0.5,
                scale_mul: 1.0,
            }],
            ..Explosion::default()
        };
        let pos = crate::grid::Pos::new(2, 2);
        let mut full_out = Vec::new();
        let mut half_out = Vec::new();
        emit_explosion(&mut full_out, &cfg_proj, pos, 0.0, &full_alpha_ex);
        emit_explosion(&mut half_out, &cfg_proj, pos, 0.0, &half_alpha_ex);
        // Compare first sprite's alpha channel (index 3).
        if let (DrawCommand::Sprite(sf), DrawCommand::Sprite(sh)) = (&full_out[0], &half_out[0]) {
            assert!(
                sh.color[3] < sf.color[3],
                "half-alpha layer must produce lower sprite alpha: full={:.3} half={:.3}",
                sf.color[3],
                sh.color[3]
            );
        } else {
            panic!("expected Sprite commands");
        }
    }
}
