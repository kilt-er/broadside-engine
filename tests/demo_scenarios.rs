//! Demo.ts as a Rust integration test.
//!
//! Port of `broadside-engine/demo.ts`. The two scenarios prove the headline
//! claim of the engine: **same weapon, same range, orientation alone
//! decides how much damage gets through.** That claim is the user-facing
//! contract of the directional-shield design.
//!
//! Inline tests in `src/resolve.rs` (`apply_damage_weak_stern_takes_post_falloff_hit`
//! and `apply_damage_strong_bow_soaks_to_zero`) prove the math at the
//! `apply_damage` boundary. This file proves the same math holds through
//! the FULL `resolve_round` pipeline — i.e., it exercises the action queue,
//! the arc gate, heat / cooldown accounting, the damage pipeline, and the
//! event bus emit ordering, against the same demo.ts scenarios.
//!
//! ## Reference snapshots
//!
//! - `demo.ts:62-79` — Scenario A (scout bow=fore, weak stern faces player)
//! - `demo.ts:81-87` — Scenario B (scout bow=aft, strong bow faces player)
//! - `demo.ts:89` — the headline claim that this file pins

use broadside_engine::grid::{Dir4, Facing, Pos};
use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, Hook, HookContext, Mount,
    Orientation, Projectile, RangeBand, Ship, Targeting, TargetingPattern, WeaponArchetype,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/* =========================================================================
 * Fixtures — direct mirror of demo.ts, ported to the 2-D grid.
 *
 * demo.ts is 1-D (a lane of cells); the v2 engine is 2-D (a 5x4 grid), and the
 * live firing/damage path reads `pos`/`facing` over the grid (R3/R4). So the
 * 1-D `cell`/`bow` shape is re-keyed onto real grid positions with real bearing
 * facings, upholding invariant A (`cell == pos.to_index()`). The HEADLINE claim
 * is unchanged and is what these tests pin: same weapon, same range, the scout's
 * orientation alone decides how much gets through.
 * ====================================================================== */

/// A demo ship at a real 2-D `pos` with bearing `facing`. Default Frigate shield
/// profile, one forward-arc `pulse_laser` mount. Upholds invariant A.
fn ship(id: &str, faction: Faction, pos: Pos, hull: i32, facing: Facing) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell: pos.to_index(),
        pos,
        orientation: Orientation::BowOn {
            bow: broadside_engine::types::LaneEnd::Fore,
        },
        facing,
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        // The canonical Frigate per-face SHIELD pool (#103 Model A): strong bow
        // (cap 4), soft stern (cap 1), medium flanks (cap 3), pools start FULL.
        // `charge` is the live depleting pool; `armour` the capacity.
        shield_profile: broadside_engine::geometry2d::default_shield_profile(),
        mounts: vec![Mount {
            id: "m1".into(),
            arc: Arc::Forward,
            weapon: "pulse_laser".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

/// The demo.ts `pulseLaser` action: 4 raw damage, optimal=close, fires
/// pointBlank/close/mid, requires Forward arc. Heat 1, no cooldown.
fn pulse_laser() -> Action {
    Action {
        id: "pulse_laser".into(),
        name: "Pulse Laser".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost {
            heat: 1,
            cooldown_max: 0,
            advances_turn: true,
        },
        targeting: Targeting {
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
                broadside_engine::grid::Range::Far,
            ],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::Close,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE {
            amount: 4,
            band_falloff: None,
        }],
        r#mod: None,
        icon: None,
    }
}

/// Content holding the `pulse_laser` action. `spawn_projectile` panics
/// because the demo scenarios don't fire ordnance.
struct DemoContent(Action);
impl Content for DemoContent {
    fn action(&self, id: &str) -> Option<&Action> {
        (id == "pulse_laser").then_some(&self.0)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("demo scenarios don't fire ordnance");
    }
}

/// Wire a recording bus that captures (cell, amount) pairs for
/// `OnDamageTaken` and just cells for `OnLethal`. Returns Rc handles the
/// caller can read after `resolve_round` returns.
type DamageLog = Rc<RefCell<Vec<(usize, i32)>>>;
type LethalLog = Rc<RefCell<Vec<usize>>>;

fn wire_bus(board: &mut Board) -> (DamageLog, LethalLog) {
    let damage: DamageLog = Rc::new(RefCell::new(Vec::new()));
    let lethal: LethalLog = Rc::new(RefCell::new(Vec::new()));

    let d = Rc::clone(&damage);
    board
        .bus
        .on(Hook::OnDamageTaken, move |ctx: &mut HookContext| {
            if let (Some(c), Some(a)) = (ctx.target_cell, ctx.amount) {
                d.borrow_mut().push((c, a));
            }
        });
    let l = Rc::clone(&lethal);
    board.bus.on(Hook::OnLethal, move |ctx: &mut HookContext| {
        if let Some(c) = ctx.target_cell {
            l.borrow_mut().push(c);
        }
    });

    (damage, lethal)
}

/// The demo.ts layout on the 2-D grid. The player frigate fires N up column 2;
/// the scout sits two cells N (distance 2 = Near) so the shot bears and the
/// falloff curve is exercised; the gunboat is dressing one column over (off the
/// firing ray, so the first-target-only BEAM never touches it).
///
/// `scout_facing` is the load-bearing variable: the shot arrives FROM the south
/// (the player is S of the scout), so `Bow(N)` turns the scout's weak STERN to
/// the incoming fire (Scenario A) and `Bow(S)` turns its strong BOW to it
/// (Scenario B). Everything else is held constant.
///
/// Cell indices (row-major, COLS=5): player (2,3)=17, scout (2,1)=7,
/// gunboat (4,0)=4.
fn demo_board(scout_facing: Facing) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();

    let mut player = ship(
        "frigate",
        Faction::Player,
        Pos::new(2, 3),
        10,
        Facing::Bow(Dir4::N),
    );
    player.queue = vec!["pulse_laser".into()];
    let scout = ship("scout", Faction::Enemy, Pos::new(2, 1), 5, scout_facing);
    let gunboat = ship(
        "gunboat",
        Faction::Enemy,
        Pos::new(4, 0),
        5,
        Facing::Bow(Dir4::S),
    );

    // Capture each cell index before moving the ship into the slot (invariant A:
    // slot == pos.to_index()).
    let (pi, si, gi) = (
        player.pos.to_index(),
        scout.pos.to_index(),
        gunboat.pos.to_index(),
    );
    cells[pi] = Some(player);
    cells[si] = Some(scout);
    cells[gi] = Some(gunboat);

    Board {
        size: broadside_engine::grid::COLS,
        cols: broadside_engine::grid::COLS,
        rows: broadside_engine::grid::ROWS,
        cells,
        ordnance: Vec::new(),
        hazards: (0..broadside_engine::grid::CELLS)
            .map(|_| Vec::new())
            .collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

/* =========================================================================
 * Scenario A — weak stern faces the player
 *
 * scout bow=fore -> bow points forward, stern faces the attacker at cell 0.
 * Distance 1 = pointBlank; pulse_laser optimal=close (delta 1) ->
 * factor 0.66 -> floor(4 * 0.66) = 2. Stern armour 0 -> 2 lands.
 * Scout hull 5 - 2 = 3.
 *
 * Mirrors demo.ts:64-79.
 * ====================================================================== */

// 2-D port (#22) + #103/#104 shield-pool model: scout at (2,1) with bow N so its
// weak STERN faces the player firing from the south at (2,3). Distance 2 = Near
// -> INTEGER falloff 4 - 1 = 3. The stern pool (cap 1, starts full) soaks 1; the
// remaining 2 overflows to hull. Scout 5 -> 3.
#[test]
fn scenario_a_weak_stern_takes_post_falloff_damage() {
    let mut board = demo_board(Facing::Bow(Dir4::N));
    let (damage, lethal) = wire_bus(&mut board);
    let content = DemoContent(pulse_laser());

    resolve_round(&mut board, &content);

    // Scout's hull dropped by exactly the post-falloff, post-armour damage.
    let scout_idx = Pos::new(2, 1).to_index();
    let scout_hull = board.cells[scout_idx]
        .as_ref()
        .expect("scout survives")
        .hull;
    assert_eq!(
        scout_hull, 3,
        "soft stern pool (cap 1) soaks 1 of the falloff-3 hit; 2 overflows to hull"
    );

    // Exactly one OnDamageTaken emit for the scout (cell index 7) with amount 2.
    assert_eq!(
        *damage.borrow(),
        vec![(scout_idx, 2)],
        "OnDamageTaken should fire once for the scout cell with amount 2",
    );

    // No deaths.
    assert!(
        lethal.borrow().is_empty(),
        "no ship should be destroyed in Scenario A"
    );

    // Gunboat one column over is dressing — BEAM hits the first target on the
    // firing ray (the scout up column 2). Gunboat is untouched.
    let gunboat_hull = board.cells[Pos::new(4, 0).to_index()]
        .as_ref()
        .expect("gunboat untouched")
        .hull;
    assert_eq!(gunboat_hull, 5);

    // Player paid the heat and the cooldown is set. Heat dissipates by 1
    // at end-of-turn, so after resolve_round the player's heat is
    // (0 + 1) - 1 = 0. cooldown_max is 0 for pulse_laser, so the cooldown
    // entry is also 0 after end-of-turn's decrement.
    let player = board.cells[Pos::new(2, 3).to_index()]
        .as_ref()
        .expect("player survives");
    assert_eq!(player.heat, 0, "heat 0 + 1 fired - 1 EOT dissipation = 0");
    assert_eq!(player.cooldowns.get("pulse_laser").copied(), Some(0));
    assert!(player.queue.is_empty(), "queue cleared after resolve_round");
}

/* =========================================================================
 * Scenario B — strong bow faces the player
 *
 * scout bow=aft -> bow points BACKWARD along the lane, so the bow faces
 * the attacker at cell 0. Distance 1 = pointBlank; falloff brings 4 down
 * to 2 (same as A); bow armour 2 -> max(0, 2 - 2) = 0 lands. Scout hull
 * stays at 5.
 *
 * Mirrors demo.ts:81-87. Same shot, same range — orientation alone
 * decided the outcome.
 * ====================================================================== */

#[test]
fn scenario_b_strong_bow_soaks_to_zero() {
    // 2-D port (#22) + #103/#104 shield-pool model: scout at (2,1) with bow S so
    // its strong BOW faces the player firing from the south. Distance 2 = Near ->
    // INTEGER falloff 4 - 1 = 3; the bow pool (cap 4, starts full) soaks all 3,
    // so 0 reaches hull. Scout stays 5. NOTE: pre-migration this test passed
    // VACUOUSLY (the 1-D fixture stacked ships at (0,0) so no shot connected and
    // the scout was untouched for the wrong reason); now the shot genuinely lands
    // and the bow shield pool genuinely soaks it.
    let mut board = demo_board(Facing::Bow(Dir4::S));
    let (damage, lethal) = wire_bus(&mut board);
    let content = DemoContent(pulse_laser());

    resolve_round(&mut board, &content);

    let scout_hull = board.cells[Pos::new(2, 1).to_index()]
        .as_ref()
        .expect("scout survives")
        .hull;
    assert_eq!(
        scout_hull, 5,
        "strong bow pool (cap 4) soaks the falloff-3 hit to zero"
    );

    // Crucial: NO OnDamageTaken emit. resolve.rs:467 gates the emit on
    // `final_dmg > 0`, and the bow armour brings final_dmg to 0. A port
    // that emitted OnDamageTaken with amount 0 (e.g. for renderer
    // animations) would fail this assertion.
    assert!(
        damage.borrow().is_empty(),
        "no OnDamageTaken emit when armour fully absorbs the hit (final_dmg == 0)",
    );
    assert!(
        lethal.borrow().is_empty(),
        "no ship destroyed in Scenario B"
    );
}

/* =========================================================================
 * The headline claim — same weapon, same range, orientation decides
 * ====================================================================== */

/// Run both scenarios back-to-back and assert the scout-hull DELTA
/// between them. The whole point of the directional-shield design is
/// that this delta is observable and reproducible: weak-stern hit > 0,
/// strong-bow hit == 0, with everything else (weapon, range, attacker
/// position, lane state) held constant.
///
/// If a future port broke `facing_zone` (e.g. swapped the bow/stern
/// mapping), `scenario_a` would still land 2 damage (because falloff
/// math is independent of zone) but it would land on the BOW instead
/// of the STERN, and the strong-bow armour would absorb it to 0 — and
/// `scenario_b` would conversely route to the stern and lose 2 hull.
/// The delta would invert. This test catches that.
// 2-D port (#22): the foundational demo.ts Scenario A/B contrast on the grid.
// Same weapon, same Near range, the scout's facing (bow N stern-to-attacker vs
// bow S bow-to-attacker) alone decides the outcome. A broken facing_zone (e.g. a
// swapped bow/stern mapping) inverts the delta and reddens this.
#[test]
fn orientation_alone_changes_the_outcome() {
    let scout_idx = Pos::new(2, 1).to_index();
    // Scenario A: weak stern faces the attacker (scout bow N).
    let mut board_a = demo_board(Facing::Bow(Dir4::N));
    let content = DemoContent(pulse_laser());
    resolve_round(&mut board_a, &content);
    let hull_a = board_a.cells[scout_idx]
        .as_ref()
        .expect("scout A survives")
        .hull;

    // Scenario B: strong bow faces the attacker (scout bow S).
    let mut board_b = demo_board(Facing::Bow(Dir4::S));
    resolve_round(&mut board_b, &content);
    let hull_b = board_b.cells[scout_idx]
        .as_ref()
        .expect("scout B survives")
        .hull;

    assert_eq!(hull_a, 3, "A: stern facing -> 2 damage lands");
    assert_eq!(hull_b, 5, "B: bow facing -> 2 damage soaked");
    assert!(
        hull_a < hull_b,
        "the headline demo.ts claim: same weapon, same range, weak-stern \
         scenario takes MORE damage than the strong-bow scenario. \
         hull_a == hull_b would mean facing_zone returned the same zone \
         for both stances (broken bow/stern routing).",
    );
    assert_eq!(
        hull_b - hull_a,
        2,
        "the bow shield pool absorbs more of the falloff hit than the soft \
         stern (delta 2). A different delta would mean the falloff math or \
         the shield-pool arithmetic changed.",
    );
}
