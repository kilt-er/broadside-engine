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
const BEAM_SECS: f32 = 0.22;
const HIT_FLASH_SECS: f32 = 0.30;
const EXPLOSION_SECS: f32 = 0.55;
const TRAIL_SECS: f32 = 0.35;
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
    /// Straight beam between two lane cells (attacker → target).
    Beam {
        from_cell: f32,
        to_cell: f32,
        color: [f32; 3],
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
}

impl Snapshot {
    fn of(board: &Board) -> Self {
        let mut ships = HashMap::new();
        for s in board.cells.iter().flatten() {
            ships.insert(s.id.clone(), (s.hull, s.cell, s.faction));
        }
        let mut ordnance = HashMap::new();
        for p in &board.ordnance {
            ordnance.insert(p.id.clone(), p.cell);
        }
        Self { ships, ordnance }
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
const BEAM_COLOR: [f32; 3] = [0.40, 0.86, 1.0]; // cyan bolt
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
        if let Some(prev) = self.prev.take() {
            self.diff(&prev, &cur, board);
        }
        self.prev = Some(cur);
    }

    fn diff(&mut self, prev: &Snapshot, cur: &Snapshot, board: &Board) {
        // Ships: hull-drop → hit flash + beam; vanished → explosion.
        for (id, &(prev_hull, prev_cell, prev_faction)) in &prev.ships {
            match cur.ships.get(id) {
                Some(&(cur_hull, cur_cell, _)) => {
                    if cur_hull < prev_hull {
                        let cell = cur_cell as f32;
                        self.spawn(EffectKind::HitFlash { cell }, HIT_FLASH_SECS);
                        // Beam from the nearest live OPPOSING ship toward the
                        // ship that was hit (first-pass pairing heuristic — the
                        // resolver doesn't hand us attacker→target yet).
                        if let Some(from) = nearest_opponent_cell(board, prev_faction, cur_cell) {
                            self.spawn(
                                EffectKind::Beam {
                                    from_cell: from,
                                    to_cell: cell,
                                    color: BEAM_COLOR,
                                },
                                BEAM_SECS,
                            );
                        }
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
                EffectKind::Beam {
                    from_cell,
                    to_cell,
                    color,
                } => emit_beam(out, lane, from_cell, to_cell, color, e.t()),
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

/// Nearest live ship of the opposite faction to `cell`, as a fractional cell.
fn nearest_opponent_cell(board: &Board, hit_faction: Faction, cell: usize) -> Option<f32> {
    board
        .cells
        .iter()
        .flatten()
        .filter(|s| s.faction != hit_faction)
        .min_by_key(|s| (s.cell as i64 - cell as i64).abs())
        .map(|s| s.cell as f32)
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
        }
    }

    fn ship(id: &str, faction: Faction, cell: usize, hull: i32) -> Ship {
        Ship {
            id: id.into(),
            faction,
            cell,
            orientation: Orientation::BowOn { bow: LaneEnd::Fore },
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
    fn hull_drop_spawns_hit_flash_and_beam() {
        let mut vfx = CombatVfx::new();
        let mut board = empty_board(7);
        board.cells[0] = Some(ship("player", Faction::Player, 0, 5));
        board.cells[3] = Some(ship("enemy", Faction::Enemy, 3, 5));
        vfx.observe(&board); // baseline
                             // Enemy takes a hit.
        board.cells[3].as_mut().unwrap().hull = 3;
        vfx.observe(&board);
        // Hit flash on the enemy + a beam from the player (its only opponent).
        assert_eq!(vfx.effects.len(), 2, "hit flash + beam");
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
            heading: LaneEnd::Fore,
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
