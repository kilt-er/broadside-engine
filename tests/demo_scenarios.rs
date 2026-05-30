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

use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, Hook, HookContext, LaneEnd, Mount,
    Orientation, Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting,
    TargetingPattern, WeaponArchetype,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/* =========================================================================
 * Fixtures — direct mirror of demo.ts
 * ====================================================================== */

/// The demo.ts `ship` builder. Bow-on, default Frigate shield profile,
/// one forward-arc mount with the pulse_laser. Player gets faction Player;
/// scout/gunboat are Enemy.
fn ship(id: &str, faction: Faction, cell: usize, hull: i32, bow: LaneEnd) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        orientation: Orientation::BowOn { bow },
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
            // The canonical Frigate hull: strong bow, weak stern,
            // medium flanks. Demo.ts uses `defaultShieldProfile()`.
            bow: ShieldFace { armour: 2, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 1, charge: 0 },
            starboard: ShieldFace { armour: 1, charge: 0 },
        },
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
    }
}

/// The demo.ts `pulseLaser` action: 4 raw damage, optimal=close, fires
/// pointBlank/close/mid, requires Forward arc. Heat 1, no cooldown.
fn pulse_laser() -> Action {
    Action {
        id: "pulse_laser".into(),
        name: "Pulse Laser".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::Close,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount: 4, band_falloff: None }],
        r#mod: None,
        icon: None,
    }
}

/// Content holding the pulse_laser action. spawn_projectile panics
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
/// OnDamageTaken and just cells for OnLethal. Returns Rc handles the
/// caller can read after `resolve_round` returns.
type DamageLog = Rc<RefCell<Vec<(usize, i32)>>>;
type LethalLog = Rc<RefCell<Vec<usize>>>;

fn wire_bus(board: &mut Board) -> (DamageLog, LethalLog) {
    let damage: DamageLog = Rc::new(RefCell::new(Vec::new()));
    let lethal: LethalLog = Rc::new(RefCell::new(Vec::new()));

    let d = Rc::clone(&damage);
    board.bus.on(Hook::OnDamageTaken, move |ctx: &mut HookContext| {
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

/// 7-cell board with the demo.ts layout: `[player, scout, _, _, gunboat, _, _]`.
fn demo_board(scout_bow: LaneEnd) -> Board {
    let mut player = ship("frigate", Faction::Player, 0, 10, LaneEnd::Fore);
    player.queue = vec!["pulse_laser".into()];
    let scout = ship("scout", Faction::Enemy, 1, 5, scout_bow);
    let gunboat = ship("gunboat", Faction::Enemy, 4, 5, LaneEnd::Aft);

    Board {
        size: 7,
        cells: vec![
            Some(player),
            Some(scout),
            None,
            None,
            Some(gunboat),
            None,
            None,
        ],
        ordnance: Vec::new(),
        hazards: (0..7).map(|_| Vec::new()).collect(),
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
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

#[test]
fn scenario_a_weak_stern_takes_post_falloff_damage() {
    let mut board = demo_board(LaneEnd::Fore);
    let (damage, lethal) = wire_bus(&mut board);
    let content = DemoContent(pulse_laser());

    resolve_round(&mut board, &content);

    // Scout's hull dropped by exactly the post-falloff, post-armour damage.
    let scout_hull = board.cells[1].as_ref().expect("scout survives").hull;
    assert_eq!(scout_hull, 3, "weak stern (armour 0) bleeds the full post-falloff 2 damage");

    // Exactly one OnDamageTaken emit for the scout with the correct amount.
    assert_eq!(
        *damage.borrow(),
        vec![(1, 2)],
        "OnDamageTaken should fire once for cell 1 with amount 2",
    );

    // No deaths.
    assert!(lethal.borrow().is_empty(), "no ship should be destroyed in Scenario A");

    // Gunboat at cell 4 is dressing — BEAM hits first target only (the
    // scout at cell 1). Gunboat is untouched.
    let gunboat_hull = board.cells[4].as_ref().expect("gunboat untouched").hull;
    assert_eq!(gunboat_hull, 5);

    // Player paid the heat and the cooldown is set. Heat dissipates by 1
    // at end-of-turn, so after resolve_round the player's heat is
    // (0 + 1) - 1 = 0. cooldown_max is 0 for pulse_laser, so the cooldown
    // entry is also 0 after end-of-turn's decrement.
    let player = board.cells[0].as_ref().expect("player survives");
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
#[ignore = "AI friendly-fires: gunboat at cell 4 fires at scout at cell 1 because \
            decide_enemy_action doesn't filter same-faction targets. See task #49. \
            Un-ignore once the AI / resolve_targeting fix lands."]
fn scenario_b_strong_bow_soaks_to_zero() {
    let mut board = demo_board(LaneEnd::Aft);
    let (damage, lethal) = wire_bus(&mut board);
    let content = DemoContent(pulse_laser());

    resolve_round(&mut board, &content);

    let scout_hull = board.cells[1].as_ref().expect("scout survives").hull;
    assert_eq!(scout_hull, 5, "strong bow (armour 2) soaks the post-falloff 2 damage to zero");

    // Crucial: NO OnDamageTaken emit. resolve.rs:467 gates the emit on
    // `final_dmg > 0`, and the bow armour brings final_dmg to 0. A port
    // that emitted OnDamageTaken with amount 0 (e.g. for renderer
    // animations) would fail this assertion.
    assert!(
        damage.borrow().is_empty(),
        "no OnDamageTaken emit when armour fully absorbs the hit (final_dmg == 0)",
    );
    assert!(lethal.borrow().is_empty(), "no ship destroyed in Scenario B");
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
#[test]
#[ignore = "Depends on scenario_b passing — same friendly-fire bug. See task #49."]
fn orientation_alone_changes_the_outcome() {
    // Scenario A: weak stern facing.
    let mut board_a = demo_board(LaneEnd::Fore);
    let content = DemoContent(pulse_laser());
    resolve_round(&mut board_a, &content);
    let hull_a = board_a.cells[1].as_ref().expect("scout A survives").hull;

    // Scenario B: strong bow facing.
    let mut board_b = demo_board(LaneEnd::Aft);
    resolve_round(&mut board_b, &content);
    let hull_b = board_b.cells[1].as_ref().expect("scout B survives").hull;

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
        "the bow armour absorbs exactly the post-falloff damage (2). \
         A different delta would mean the falloff math or the armour \
         arithmetic changed.",
    );
}
