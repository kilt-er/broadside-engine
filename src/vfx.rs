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
//! callback** (the EventBus "no chained emit" invariant). We satisfy that *by
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

use crate::gfx::{DrawCommand, SpriteInstance};
use crate::perspective::{fractional_cell_to_screen, LaneGeometry};
use crate::types::{Board, Faction};
use crate::{atlas, types::Ship};
use std::collections::HashMap;

/// How long each effect lives, seconds. First-pass values; bruce tunes.
const HIT_FLASH_SECS: f32 = 0.30;
const EXPLOSION_SECS: f32 = 0.55;
const TRAIL_SECS: f32 = 0.35;
const TELEGRAPH_FIRE_SECS: f32 = 0.32;
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
    /// EXACT fired shot (#59): the resolver's per-round FireEvent, drawn as a
    /// styled beam attacker→target. `thickness` + `dur` come from the weapon
    /// archetype, `color` from the firing faction; a miss (`dim`) renders
    /// fainter. Replaces the old guessed nearest-opponent beam.
    ShotBeam {
        from_cell: f32,
        to_cell: f32,
        color: [f32; 3],
        thickness: f32,
        dim: bool,
    },
    /// Expanding flash centred on a cell (a ship taking a hit).
    HitFlash { cell: f32 },
    /// Expanding ring + debris at a cell (a ship destroyed).
    Explosion { cell: f32 },
    /// Fading streak between two cells (ordnance step).
    Trail {
        from_cell: f32,
        to_cell: f32,
        color: [f32; 3],
    },
    /// A telegraph icon "spending" as its enemy fires (#70): a quick expanding
    /// red pop at the telegraph slot above `cell`, so the player sees the
    /// readied action discharge rather than silently becoming the next intent.
    TelegraphFire { cell: f32 },
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
    /// ship id → (hull, cell, faction).
    ships: HashMap<String, (i32, usize, Faction)>,
    /// projectile id → cell.
    ordnance: HashMap<String, usize>,
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
            ships.insert(s.id.clone(), (s.hull, s.cell, s.faction));
            if s.faction == Faction::Enemy {
                if let Some(head) = s.queue.first() {
                    enemy_intent.insert(s.id.clone(), head.clone());
                }
            }
        }
        let mut ordnance = HashMap::new();
        for p in &board.ordnance {
            ordnance.insert(p.id.clone(), p.cell);
        }
        Self {
            ships,
            ordnance,
            enemy_intent,
            fire_sig: fire_events_sig(board),
        }
    }
}

/// Live combat VFX state: the active transient effects + the previous frame's
/// snapshot for diffing. Render-owned; the bin advances it each frame.
#[derive(Default)]
pub struct CombatVfx {
    effects: Vec<Effect>,
    prev: Option<Snapshot>,
}

/// Placeholder palette — readable flat tones; bruce refines.
const HIT_COLOR: [f32; 3] = [1.0, 0.86, 0.45]; // warm spark
const EXPLOSION_COLOR: [f32; 3] = [1.0, 0.55, 0.25]; // orange burst
const TRAIL_COLOR: [f32; 3] = [0.95, 0.70, 0.35]; // ordnance ember
const TELEGRAPH_COLOR: [f32; 4] = [0.95, 0.30, 0.30, 0.9]; // enemy-intent red

impl CombatVfx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Diff `board` against the previous frame and spawn effects for the
    /// changes. Read-only over `board`. Call once per frame BEFORE [`advance`].
    pub fn observe(&mut self, board: &Board) {
        let cur = Snapshot::of(board);
        // Capture the previous frame's fire signature BEFORE `take()` empties it.
        let prev_sig = self.prev.as_ref().map(|p| p.fire_sig).unwrap_or(0);
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
                let (thickness, dur) = archetype_beam_style(fe.archetype);
                self.spawn(
                    EffectKind::ShotBeam {
                        from_cell: fe.from_cell as f32,
                        to_cell: fe.to_cell as f32,
                        color: faction_beam_tint(fe.attacker_faction),
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
        for (id, &(prev_hull, prev_cell, _prev_faction)) in &prev.ships {
            match cur.ships.get(id) {
                Some(&(cur_hull, cur_cell, _)) => {
                    if cur_hull < prev_hull {
                        // Hull drop → the IMPACT (flash). The shot LINE itself now
                        // comes from the resolver's exact FireEvent (#59,
                        // ShotBeam in observe), not a guessed nearest-opponent
                        // beam — so we no longer fabricate an attacker here.
                        let cell = cur_cell as f32;
                        self.spawn(EffectKind::HitFlash { cell }, HIT_FLASH_SECS);
                    }
                }
                None => {
                    // Ship gone this frame → destroyed at its last known cell.
                    self.spawn(
                        EffectKind::Explosion {
                            cell: prev_cell as f32,
                        },
                        EXPLOSION_SECS,
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
            let Some(&(_, cur_cell, _)) = cur.ships.get(id) else {
                continue;
            };
            let fired = match cur.enemy_intent.get(id) {
                Some(cur_head) => cur_head != prev_head, // swapped to next intent
                None => true,                            // queue emptied → spent
            };
            if fired {
                self.spawn(
                    EffectKind::TelegraphFire {
                        cell: cur_cell as f32,
                    },
                    TELEGRAPH_FIRE_SECS,
                );
            }
        }
        // Ordnance: a projectile that moved leaves a trail along its step.
        for (id, &cur_cell) in &cur.ordnance {
            if let Some(&prev_cell) = prev.ordnance.get(id) {
                if prev_cell != cur_cell {
                    self.spawn(
                        EffectKind::Trail {
                            from_cell: prev_cell as f32,
                            to_cell: cur_cell as f32,
                            color: TRAIL_COLOR,
                        },
                        TRAIL_SECS,
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
        self.effects.retain(|e| e.alive());
        !self.effects.is_empty()
    }

    /// True while any transient effect is active (redraw-keepalive helper).
    pub fn is_active(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Emit draw commands for every active transient effect + the live
    /// telegraph cues (read from `board`). Append to `out`; ordered so juice
    /// sits above the ships but below modal overlays (the caller controls
    /// where in the command stream this runs).
    pub fn emit(&self, out: &mut Vec<DrawCommand>, board: &Board, lane: &LaneGeometry) {
        for e in &self.effects {
            match e.kind {
                EffectKind::HitFlash { cell } => {
                    emit_flash(out, lane, cell, HIT_COLOR, e.t(), 16.0)
                }
                EffectKind::Explosion { cell } => {
                    emit_flash(out, lane, cell, EXPLOSION_COLOR, e.t(), 30.0)
                }
                EffectKind::Trail {
                    from_cell,
                    to_cell,
                    color,
                } => emit_beam(out, lane, from_cell, to_cell, color, e.t()),
                EffectKind::TelegraphFire { cell } => emit_telegraph_fire(out, lane, cell, e.t()),
                EffectKind::ShotBeam {
                    from_cell,
                    to_cell,
                    color,
                    thickness,
                    dim,
                } => emit_shot_beam(out, lane, from_cell, to_cell, color, thickness, dim, e.t()),
            }
        }
        // Telegraph: live cue above any enemy holding a queued action.
        for s in board.cells.iter().flatten() {
            if s.faction == Faction::Enemy && !s.queue.is_empty() {
                emit_telegraph(out, lane, s);
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
    let mut h: u64 = 1469598103934665603; // FNV-1a offset
    let mut fold = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(1099511628211);
    };
    for fe in &board.fire_events {
        fold(fe.from_cell as u64);
        fold(fe.to_cell as u64);
        fold(fe.archetype as u64);
        fold(fe.attacker_faction as u64);
        fold(fe.hit as u64);
    }
    fold(0xFF00 ^ board.fire_events.len() as u64);
    h
}

/* ---- draw helpers (flat-colour quads via SOLID_WHITE) --------------------- */

/// A beam / trail: a thin rectangle from `from`→`to` along the lane, fading out
/// over its lifetime. Rendered as a rotated SpriteInstance.
fn emit_beam(
    out: &mut Vec<DrawCommand>,
    lane: &LaneGeometry,
    from_cell: f32,
    to_cell: f32,
    color: [f32; 3],
    t: f32,
) {
    let a = fractional_cell_to_screen(from_cell, lane);
    let b = fractional_cell_to_screen(to_cell, lane);
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let cx = (a.x + b.x) / 2.0;
    let cy = (a.y + b.y) / 2.0;
    let alpha = (1.0 - t) * 0.9; // fade out
    let thickness = 3.0 * (1.0 - t * 0.5); // thins slightly as it fades
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
/// Cheap differentiation so a beam reads instant-and-thin, ordnance slow-and-fat,
/// a broadside short-and-wide, etc. Colour comes from the firing faction.
fn archetype_beam_style(a: crate::types::WeaponArchetype) -> (f32, f32) {
    use crate::types::WeaponArchetype as W;
    match a {
        W::Beam => (2.5, 0.20),      // instant thin bolt
        W::Ordnance => (4.5, 0.40),  // fat, lingering streak
        W::Broadside => (5.5, 0.26), // wide volley
        W::Control => (2.0, 0.30),   // thin, lingering tractor/lock
        W::Displacement => (3.0, 0.24),
        W::Movement => (2.0, 0.20),
        W::Defensive => (2.0, 0.20),
    }
}

/// Beam tint by the FIRING faction: enemy shots red, player shots cyan, so the
/// player can read at a glance who is shooting whom.
pub(crate) fn faction_beam_tint(f: Faction) -> [f32; 3] {
    match f {
        Faction::Enemy => [0.98, 0.34, 0.30], // hostile red
        Faction::Player => [0.40, 0.86, 1.0], // friendly cyan
    }
}

/// Draw an EXACT fired shot (#59): a styled beam attacker→target, given
/// archetype `thickness` and faction `color`, fading over its life. A miss
/// (`dim`) renders at reduced alpha so it reads as "fired but didn't connect".
#[allow(clippy::too_many_arguments)]
fn emit_shot_beam(
    out: &mut Vec<DrawCommand>,
    lane: &LaneGeometry,
    from_cell: f32,
    to_cell: f32,
    color: [f32; 3],
    thickness: f32,
    dim: bool,
    t: f32,
) {
    let a = fractional_cell_to_screen(from_cell, lane);
    let b = fractional_cell_to_screen(to_cell, lane);
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let cx = (a.x + b.x) / 2.0;
    let cy = (a.y + b.y) / 2.0;
    // Bright at birth, fading out; a connecting hit reads fuller than a miss.
    let base_alpha = if dim { 0.45 } else { 0.95 };
    let alpha = (1.0 - t) * base_alpha;
    let th = thickness * (1.0 - t * 0.4); // thins slightly as it fades
    out.push(DrawCommand::Sprite(SpriteInstance {
        pos: [cx, cy],
        half_size: [len / 2.0, th / 2.0],
        color: [color[0], color[1], color[2], alpha],
        uv_min: atlas::cell_uvs(atlas::SOLID_WHITE).0,
        uv_max: atlas::cell_uvs(atlas::SOLID_WHITE).1,
        rotation_rad: dy.atan2(dx),
        _pad: [0.0; 3],
    }));
}

/// A flash / explosion: an expanding, fading square centred on a cell.
fn emit_flash(
    out: &mut Vec<DrawCommand>,
    lane: &LaneGeometry,
    cell: f32,
    color: [f32; 3],
    t: f32,
    peak: f32,
) {
    let p = fractional_cell_to_screen(cell, lane);
    // Ease-out grow; fade over life.
    let size = peak * (0.35 + 0.65 * t);
    let alpha = (1.0 - t) * 0.85;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        [p.x, p.y],
        [size / 2.0, size / 2.0],
        [color[0], color[1], color[2], alpha],
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// Telegraph FIRE pop (#70): a quick expanding red flash at the telegraph slot
/// above the enemy (`lane.center_y - 96`, matching the hud telegraph stack), so
/// the readied action visibly DISCHARGES as the enemy fires rather than silently
/// rolling to the next intent. Grows + fades over its short life.
fn emit_telegraph_fire(out: &mut Vec<DrawCommand>, lane: &LaneGeometry, cell: f32, t: f32) {
    let p = fractional_cell_to_screen(cell, lane);
    let y = lane.center_y - 96.0;
    // Bright expanding ring-ish pop: a fast-growing, fast-fading square.
    let size = 18.0 * (0.4 + 1.1 * t);
    let alpha = (1.0 - t) * 0.95;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        [p.x, y],
        [size / 2.0, size / 2.0],
        [1.0, 0.42, 0.38, alpha],
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

/// Telegraph cue: a small red marker above an enemy holding a queued action,
/// signalling "this ship intends to act". First-pass = a chevron-ish bar; the
/// per-intent icon set is a later art pass.
fn emit_telegraph(out: &mut Vec<DrawCommand>, lane: &LaneGeometry, ship: &Ship) {
    let p = fractional_cell_to_screen(ship.cell as f32, lane);
    // Sit well above the ship silhouette.
    let y = lane.center_y - 96.0;
    out.push(DrawCommand::Sprite(SpriteInstance::axis_aligned(
        [p.x, y],
        [6.0, 6.0],
        TELEGRAPH_COLOR,
        atlas::cell_uvs(atlas::SOLID_WHITE),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EventBus, LaneEnd, Orientation, Projectile, ShieldFace, ShieldProfile};

    fn empty_board(size: usize) -> Board {
        Board {
            size,
            cells: (0..size).map(|_| None).collect(),
            ordnance: Vec::new(),
            hazards: (0..size).map(|_| Vec::new()).collect(),
            patrol: 1,
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
        // Past the explosion lifetime → cleared.
        let still = vfx.advance(EXPLOSION_SECS + 0.01);
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
        let lane = crate::perspective::DEFAULT_LANE;
        let mut board = empty_board(7);
        let mut e = ship("enemy", Faction::Enemy, 4, 5);
        e.queue.push("pulse_laser".into());
        board.cells[4] = Some(e);
        let vfx = CombatVfx::new();
        let mut out = Vec::new();
        vfx.emit(&mut out, &board, &lane);
        assert_eq!(out.len(), 1, "one telegraph cue for the queued enemy");
    }
}
