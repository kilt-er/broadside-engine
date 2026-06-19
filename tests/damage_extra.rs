//! Net-new destroy / chain-window assertions (content spec C2 + D1 + D3).
//!
//! Most of content's C/D series is already covered:
//! - C1 (detect_chain threshold) — src/resolve.rs inline
//!   `detect_chain_fires_at_two_destroys_in_one_window`
//! - C3 (per-destroy chain increment) — tests/event_chain.rs
//!   `cascading_reactor_breaches_chain_correctly`
//! - D2 (ReactorBreach splash + OnLethal ordering, armour-0 neighbours) —
//!   tests/event_chain.rs `reactor_breach_splashes_neighbour_then_emits_lethal`
//!
//! This file adds only the genuinely-uncovered ones, so it does NOT duplicate
//! the above:
//! - **D1** — `destroy` clears the cell to `None` and emits exactly one
//!   `OnLethal` for that cell, for a PLAIN (non-ReactorBreach) ship.
//! - **D3** — ReactorBreach splash is **shield-mediated**: a neighbour with
//!   facing-zone armour 1 takes `2 - 1 = 1`, proving the 2-point splash routes
//!   through `apply_damage` / `absorb_shield`, NOT a raw `hull -= 2`.
//! - **C2** — the chain-kill window is reset to 0 on entry to a fresh
//!   `apply_instant_action` pass (a stale count from a prior window can't leak
//!   a phantom chain).
//! - **E4 (HeatSink floor)** — content flagged uncertainty on the exact
//!   low-heat arithmetic. `subsystems.rs` covers heat 4→2 / 5→2-stacked /
//!   lockout-clear (all well above 0), but NOT the floor: HeatSink must not
//!   pull heat negative. This pins `(heat - extra).max(0)` at heat 0 and 1.

use broadside_engine::resolve::{apply_instant_action, destroy, Content};
use broadside_engine::subsystems::{on_turn_end_for, Installations, HEAT_SINK};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, Hook, HookContext, LaneEnd, Mount,
    Orientation, Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting,
    TargetingPattern, Trait, WeaponArchetype,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

struct NoContent;
impl Content for NoContent {
    fn action(&self, _: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        unreachable!("damage_extra tests never spawn ordnance");
    }
}

/// A ship with a per-face-configurable shield profile so D3 can route splash
/// onto a non-zero-armour zone.
fn ship_with_armour(
    id: &str,
    cell: usize,
    hull: i32,
    bow_armour: i32,
    stern_armour: i32,
    traits: Vec<Trait>,
) -> Ship {
    Ship {
        id: id.into(),
        faction: Faction::Enemy,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
            bow: ShieldFace {
                armour: bow_armour,
                charge: 0,
            },
            stern: ShieldFace {
                armour: stern_armour,
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
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits,
        klass: None,
    }
}

fn board(size: usize, cells: Vec<Option<Ship>>) -> Board {
    assert_eq!(cells.len(), size);
    Board {
        size,
        cells,
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

/// Wire an OnLethal recorder; returns the shared cell-log.
fn record_lethal(board: &mut Board) -> Rc<RefCell<Vec<usize>>> {
    let log: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let l = Rc::clone(&log);
    board.bus.on(Hook::OnLethal, move |ctx: &mut HookContext| {
        if let Some(c) = ctx.target_cell {
            l.borrow_mut().push(c);
        }
    });
    log
}

/* =========================================================================
 * D1 — destroy clears the cell and emits exactly one OnLethal.
 * ====================================================================== */

#[test]
fn d1_destroy_clears_the_cell_and_emits_one_lethal() {
    let victim = ship_with_armour("v", 2, 5, 0, 0, vec![]); // plain, no ReactorBreach
    let mut b = board(5, vec![None, None, Some(victim), None, None]);
    let lethal = record_lethal(&mut b);

    destroy(2, &mut b, &NoContent);

    assert!(
        b.cells[2].is_none(),
        "destroyed ship's cell is cleared to None"
    );
    assert_eq!(
        *lethal.borrow(),
        vec![2],
        "exactly one OnLethal emit, for the destroyed cell"
    );
    assert_eq!(
        b.destroys_this_window, 1,
        "destroy increments the chain-window counter once"
    );
}

#[test]
fn d1_plain_ship_death_does_not_splash_neighbours() {
    // Contrast with D2/D3: a non-ReactorBreach death must NOT touch neighbours.
    let left = ship_with_armour("l", 1, 5, 0, 0, vec![]);
    let victim = ship_with_armour("v", 2, 5, 0, 0, vec![]);
    let right = ship_with_armour("r", 3, 5, 0, 0, vec![]);
    let mut b = board(5, vec![None, Some(left), Some(victim), Some(right), None]);

    destroy(2, &mut b, &NoContent);

    assert_eq!(
        b.cells[1].as_ref().expect("left alive").hull,
        5,
        "no splash without ReactorBreach (left)"
    );
    assert_eq!(
        b.cells[3].as_ref().expect("right alive").hull,
        5,
        "no splash without ReactorBreach (right)"
    );
}

/* =========================================================================
 * D3 — ReactorBreach splash is shield-mediated, not raw hull subtraction.
 * ====================================================================== */

#[test]
fn d3_reactor_breach_splash_is_reduced_by_neighbour_armour() {
    // v@2 has ReactorBreach. Splash deals 2 to each neighbour THROUGH the
    // damage pipeline. The neighbours face bow=Fore, and the splash arrives
    // from cell 2 — for the LEFT neighbour (cell 1) the hit comes from the
    // Fore direction (2 > 1) => its BOW zone; for the RIGHT neighbour (cell 3)
    // it comes from the Aft direction (2 < 3) => its STERN zone.
    //
    // Give the left neighbour bow armour 1 (takes 2-1=1) and the right
    // neighbour stern armour 0 (takes the full 2). If splash were a raw
    // `hull -= 2`, the left neighbour would drop to 3 instead of 4 — so the
    // 4 vs 3 distinction is exactly the "routes through absorb_shield" proof.
    let left = ship_with_armour("l", 1, 5, /*bow*/ 1, /*stern*/ 0, vec![]);
    let victim = ship_with_armour("v", 2, 1, 0, 0, vec![Trait::ReactorBreach]);
    let right = ship_with_armour("r", 3, 5, /*bow*/ 0, /*stern*/ 0, vec![]);
    let mut b = board(5, vec![None, Some(left), Some(victim), Some(right), None]);

    destroy(2, &mut b, &NoContent);

    assert_eq!(
        b.cells[1].as_ref().expect("left survives").hull,
        4,
        "left neighbour's bow armour 1 reduces the 2 splash to 1 (5 -> 4) — splash is shield-mediated",
    );
    assert_eq!(
        b.cells[3].as_ref().expect("right survives").hull,
        3,
        "right neighbour's armour-0 stern takes the full 2 splash (5 -> 3)",
    );
}

/* =========================================================================
 * C2 — the chain-kill window resets on entry to apply_instant_action.
 * ====================================================================== */

/// An action that deals no damage (so it kills nothing) but still runs the
/// full instant-action pass — used to prove the entry-reset zeroes a stale
/// window count even when this pass destroys nothing.
fn inert_action() -> Action {
    Action {
        id: "_inert".into(),
        name: "Inert".into(),
        archetype: WeaponArchetype::Defensive,
        cost: ActionCost {
            heat: 0,
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
            pattern: TargetingPattern::SELF,
            band: vec![RangeBand::PointBlank],
            optimal_band: RangeBand::PointBlank,
            requires_arc: None,
            facing_relative: false,
            hits_all: false,
        },
        effects: vec![Effect::VENT_HEAT {
            amount: 0,
            recharge_cooldowns: None,
        }],
        r#mod: None,
        icon: None,
    }
}

#[test]
fn c2_chain_window_resets_on_instant_action_entry() {
    let mut actor = ship_with_armour("p", 0, 10, 0, 0, vec![]);
    actor.faction = Faction::Player;
    actor.mounts = vec![Mount {
        id: "m1".into(),
        arc: Arc::Forward,
        weapon: "_inert".into(),
    }];
    let mut b = board(5, vec![Some(actor), None, None, None, None]);

    // Simulate a stale count left over from a prior window.
    b.destroys_this_window = 4;

    apply_instant_action("p", &inert_action(), &mut b, &NoContent);

    assert_eq!(
        b.destroys_this_window, 0,
        "apply_instant_action zeroes the chain-window counter on entry, so a stale \
         count from a prior pass can't manufacture a phantom chain",
    );
}

/* =========================================================================
 * E4 — HeatSink dissipation floors at 0 (never goes negative).
 *
 * subsystems.rs covers the above-zero arithmetic (heat 4->2, 5->2 stacked,
 * lockout-clear). This pins the floor that content flagged uncertainty on:
 * on_turn_end_for applies `(heat - extra).max(0)`, so HeatSink on a ship
 * already at/near 0 heat clamps rather than underflowing.
 * ====================================================================== */

#[test]
fn e4_heat_sink_floors_dissipation_at_zero() {
    // Ship at heat 0 with one HeatSink: extra dissipation 1, floored => 0.
    let mut s = ship_with_armour("p", 0, 10, 0, 0, vec![]);
    s.faction = Faction::Player;
    s.heat = 0;
    let mut b = board(3, vec![Some(s), None, None]);
    let mut installs = Installations::new();
    installs.install("p", HEAT_SINK);

    on_turn_end_for(&installs, &mut b);

    assert_eq!(
        b.cells[0].as_ref().expect("ship alive").heat,
        0,
        "HeatSink on a 0-heat ship floors at 0, never negative",
    );
}

#[test]
fn e4_two_heat_sinks_on_one_heat_ship_floor_at_zero_not_minus_one() {
    // heat 1, two HeatSinks => extra 2; (1 - 2).max(0) == 0, not -1.
    let mut s = ship_with_armour("p", 0, 10, 0, 0, vec![]);
    s.faction = Faction::Player;
    s.heat = 1;
    let mut b = board(3, vec![Some(s), None, None]);
    let mut installs = Installations::new();
    installs.install("p", HEAT_SINK);
    installs.install("p", HEAT_SINK);

    on_turn_end_for(&installs, &mut b);

    assert_eq!(
        b.cells[0].as_ref().expect("ship alive").heat,
        0,
        "stacked HeatSinks overshooting available heat clamp to 0, not negative",
    );
}
