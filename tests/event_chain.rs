//! The positive form of the no-chained-emit invariant.
//!
//! `tests/event_bus.rs::callback_emit_through_ctx_board_bus_is_a_noop`
//! pins the negative contract: a callback can't trigger downstream
//! effects by emitting through `ctx.board.bus`. The Right Way to chain
//! effects is to call resolver functions directly (`apply_damage`,
//! `destroy`, `add_status`); the resolver's wrapper emits the
//! downstream hooks AFTER the current callback returns.
//!
//! `destroy` at `resolve.rs:757-784` is the canonical example: when a
//! ReactorBreach ship dies, `destroy` calls `apply_damage` directly on
//! both neighbours (line 776), which itself emits `OnDamageTaken`, and
//! THEN emits `OnLethal` for the original target (line 781). The whole
//! chain runs through the wrapper's serial-emit pattern — no callback
//! ever fires another callback through the bus.
//!
//! This file proves the chain works end-to-end.

use broadside_engine::resolve::{destroy, Content};
use broadside_engine::types::{
    Action, Arc, Board, EventBus, Faction, Hook, HookContext, LaneEnd, Mount, Orientation,
    Projectile, ShieldFace, ShieldProfile, Ship, Trait,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

/// Empty content. `destroy` now takes `&dyn Content` so the ReactorBreach
/// splash routes its `apply_damage` calls through the full pipeline
/// including subsystem modifiers. Default `damage_modifier` returns 0, so
/// these tests' arithmetic is unchanged.
struct NoContent;
impl Content for NoContent {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        unreachable!("spawn_projectile not used in event_chain tests");
    }
}

fn naked_ship_with_traits(
    id: &str,
    faction: Faction,
    cell: usize,
    hull: i32,
    traits: Vec<Trait>,
) -> Ship {
    Ship {
        id: id.into(),
        faction,
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
        mounts: vec![Mount {
            id: "m1".into(),
            arc: Arc::Forward,
            weapon: "_".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits,
        klass: None,
    }
}

fn empty_board(size: usize, ships: Vec<Option<Ship>>) -> Board {
    assert_eq!(ships.len(), size);
    Board {
        size,
        cells: ships,
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

/* =========================================================================
 * The serial-emit pattern through resolver functions
 * ====================================================================== */

/// ReactorBreach + neighbour splash chains correctly. The chain is:
///
/// 1. Test calls `destroy(1, ...)` directly.
/// 2. `destroy` takes the ReactorBreach ship out of cell 1.
/// 3. `destroy` calls `apply_damage(2, 2, ...)` on the right neighbour —
///    a DIRECT function call, not a bus emit. apply_damage runs the
///    damage pipeline and emits `OnDamageTaken` through the live bus
///    (the wrapper has restored it by this point because `destroy`'s
///    `OnLethal` emit hasn't started yet).
/// 4. apply_damage emits `OnDamageTaken { target_cell: 2, amount: 2 }`.
///    The neighbour's hull drops to 8.
/// 5. `destroy` continues, emits `OnLethal { target_cell: 1 }`.
///
/// Observable order: OnDamageTaken (cell 2, +2) BEFORE OnLethal (cell 1).
/// That ordering is the "callback effects chain after the current
/// callback returns" property — the same property that the negative
/// canary in tests/event_bus.rs guards.
#[test]
fn reactor_breach_splashes_neighbour_then_emits_lethal() {
    let breacher = naked_ship_with_traits(
        "breacher",
        Faction::Enemy,
        1,
        2, // hull 2 — it's about to die, but we're calling destroy() directly
        vec![Trait::ReactorBreach],
    );
    // Neighbour at cell 2, hull 10, no traits. With zero armour, the raw 2
    // splash from `dummy_weapon` (band_falloff: false) lands as-is.
    let neighbour = naked_ship_with_traits("neighbour", Faction::Enemy, 2, 10, vec![]);
    let mut board = empty_board(
        7,
        vec![
            None,
            Some(breacher),
            Some(neighbour),
            None,
            None,
            None,
            None,
        ],
    );

    // Recording subscribers.
    let damage_log: Rc<RefCell<Vec<(usize, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    let lethal_log: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let event_order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

    let damage_log_inner = Rc::clone(&damage_log);
    let order_inner_d = Rc::clone(&event_order);
    board
        .bus
        .on(Hook::OnDamageTaken, move |ctx: &mut HookContext| {
            if let (Some(c), Some(a)) = (ctx.target_cell, ctx.amount) {
                damage_log_inner.borrow_mut().push((c, a));
            }
            order_inner_d.borrow_mut().push("damage");
        });

    let lethal_log_inner = Rc::clone(&lethal_log);
    let order_inner_l = Rc::clone(&event_order);
    board.bus.on(Hook::OnLethal, move |ctx: &mut HookContext| {
        if let Some(c) = ctx.target_cell {
            lethal_log_inner.borrow_mut().push(c);
        }
        order_inner_l.borrow_mut().push("lethal");
    });

    // Trigger.
    destroy(1, &mut board, &NoContent);

    // Damage chain: one OnDamageTaken for cell 2 with amount 2.
    // (No splash to cell 0 because cell 0 is None.)
    assert_eq!(
        *damage_log.borrow(),
        vec![(2, 2)],
        "ReactorBreach should deal 2 splash to the right neighbour exactly once",
    );

    // Neighbour hull dropped by exactly the splash amount.
    let neighbour_hull = board.cells[2]
        .as_ref()
        .expect("neighbour survives 2 splash")
        .hull;
    assert_eq!(neighbour_hull, 8);

    // OnLethal fired for the breacher's original cell.
    assert_eq!(*lethal_log.borrow(), vec![1]);

    // ORDERING: the splash damage emit happens BEFORE the breacher's
    // OnLethal emit, because destroy() does the splash via a direct
    // apply_damage call (which finishes, including its OnDamageTaken
    // emit) before reaching the OnLethal emit at the end of destroy().
    // This is the chain-through-functions pattern; if anyone ever
    // reordered destroy() to emit OnLethal first and splash second, the
    // observable event order would flip and this assertion would catch it.
    assert_eq!(
        *event_order.borrow(),
        vec!["damage", "lethal"],
        "splash OnDamageTaken must fire before breacher's OnLethal",
    );

    // The breacher's cell is cleared.
    assert!(
        board.cells[1].is_none(),
        "breacher should be removed from the lane"
    );

    // destroys_this_window incremented by exactly one — destroy()
    // increments BEFORE the splash, and the neighbour survives, so no
    // second increment fires.
    assert_eq!(board.destroys_this_window, 1);
}

/// Cascading destruction: if a ReactorBreach kills the neighbour, the
/// neighbour's destroy() runs too. With a hull-2 ReactorBreach blowing a
/// hull-2 neighbour also with ReactorBreach, you get a chain reaction
/// — the neighbour's splash hits ITS far neighbour, and so on.
///
/// Pinning this confirms the chain doesn't infinite-loop and that
/// destroys_this_window counts the cascade correctly.
#[test]
fn cascading_reactor_breaches_chain_correctly() {
    // Cells: 0=empty, 1=breacher(2hp), 2=tiny breacher (2hp), 3=neighbour(10hp).
    // breacher(1) splashes 2 -> tiny(2) takes 2 damage, dies (hp 2->0),
    // tiny's destroy() runs, tiny splashes 2 -> breacher(1) is already
    // None so no hit there; tiny splashes 2 -> neighbour(3) takes 2,
    // survives at hp 8.
    let breacher =
        naked_ship_with_traits("breacher", Faction::Enemy, 1, 2, vec![Trait::ReactorBreach]);
    let tiny = naked_ship_with_traits("tiny", Faction::Enemy, 2, 2, vec![Trait::ReactorBreach]);
    let neighbour = naked_ship_with_traits("neighbour", Faction::Enemy, 3, 10, vec![]);
    let mut board = empty_board(
        7,
        vec![
            None,
            Some(breacher),
            Some(tiny),
            Some(neighbour),
            None,
            None,
            None,
        ],
    );

    let lethal_log: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
    let lethal_log_inner = Rc::clone(&lethal_log);
    board.bus.on(Hook::OnLethal, move |ctx: &mut HookContext| {
        if let Some(c) = ctx.target_cell {
            lethal_log_inner.borrow_mut().push(c);
        }
    });

    // Per reviewer's follow-up: also subscribe to OnDamageTaken so the
    // FULL event order across the cascade is observed, not just the
    // lethal log. A port that reordered destroy() to emit OnLethal
    // BEFORE the splash would still produce the same lethal log and
    // final hulls, but the event_order would flip from
    // `[damage(2), damage(3), lethal(2), lethal(1)]` to
    // `[damage(2), lethal(2), damage(3), lethal(1)]` — caught here.
    let event_order: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let order_d = Rc::clone(&event_order);
    board
        .bus
        .on(Hook::OnDamageTaken, move |ctx: &mut HookContext| {
            if let Some(c) = ctx.target_cell {
                order_d.borrow_mut().push(format!("damage({c})"));
            }
        });
    let order_l = Rc::clone(&event_order);
    board.bus.on(Hook::OnLethal, move |ctx: &mut HookContext| {
        if let Some(c) = ctx.target_cell {
            order_l.borrow_mut().push(format!("lethal({c})"));
        }
    });

    destroy(1, &mut board, &NoContent);

    // Both ReactorBreach ships died: cell 1 (the original) and cell 2
    // (the one killed by the splash).
    assert!(board.cells[1].is_none());
    assert!(board.cells[2].is_none());
    // Neighbour at cell 3 survives the second splash (2 damage on 10
    // hull = 8 remaining).
    assert_eq!(board.cells[3].as_ref().expect("neighbour survives").hull, 8);

    // OnLethal fired for cell 2 (tiny) FIRST — because tiny's destroy()
    // is invoked from inside breacher's apply_damage call, which
    // completes (including tiny's OnLethal emit) before breacher's own
    // destroy() reaches its own OnLethal emit.
    assert_eq!(
        *lethal_log.borrow(),
        vec![2, 1],
        "tiny's OnLethal fires before breacher's (DFS chain through destroy)",
    );

    // Full event order across the cascade. The shape derives from
    // `destroy()`'s body at resolve.rs:757-784: splash THEN OnLethal.
    // Walking the trace:
    //   1. destroy(1) splashes 2 -> apply_damage(2) -> damage(2) emit, tiny dies
    //   2. apply_damage(2) calls destroy(2)
    //   3. destroy(2) splashes 2 -> apply_damage(3) -> damage(3) emit
    //      (neighbour survives at 8 hp; no further chain)
    //   4. destroy(2) emits OnLethal(2)
    //   5. control unwinds to destroy(1); destroy(1) emits OnLethal(1)
    // Final: [damage(2), damage(3), lethal(2), lethal(1)].
    assert_eq!(
        *event_order.borrow(),
        vec!["damage(2)", "damage(3)", "lethal(2)", "lethal(1)"],
        "full event-order through the cascade. If a port moves destroy()'s \
         OnLethal emit before the splash, this becomes \
         [damage(2), lethal(2), damage(3), lethal(1)]",
    );

    // Chain-window counter saw both deaths.
    assert_eq!(board.destroys_this_window, 2);
}
