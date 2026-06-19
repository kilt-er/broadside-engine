//! TURN-BASED chess-loop rhythm guards (#126, `docs/design/CORE_GAMEPLAY_LOOP.md`).
//!
//! The canonical loop: **one player action = one world turn.** Each of the four
//! turn-actions (move / queue / dequeue-fire / wait) calls
//! [`broadside_engine::resolve::run_world_phase`] exactly ONCE, and that single
//! call advances every enemy by one action AND ticks **every** cooldown (player +
//! all enemies) down by exactly 1. (The field-kit cards 5/6/7 are the one FREE
//! action and do NOT advance the world — but that path lives in the bin/input
//! layer, not the resolver, so it is not exercised here.)
//!
//! `canary.rs` proves the rhythm BEHAVIOURALLY (an enemy moves -> telegraphs ->
//! fires, never on turn 1; an on-cooldown weapon isn't re-queued). This file adds
//! the DIRECT structural invariant the behaviour rests on: a single
//! `run_world_phase` ticks each ship's whole cooldown map by exactly 1, never 0
//! (the world didn't advance) and never 2 (double-tick). A regression in the tick
//! cadence — the kind that would make weapons recharge twice as fast or never —
//! is caught here at the source even if the behavioural tests happen to still
//! pass.

use broadside_engine::grid::{Dir4, Facing, Pos};
use broadside_engine::resolve::{run_world_phase, Content};
use broadside_engine::types::{Action, Board, Faction, Projectile, Ship};

mod common;
use common::{board_2d, ship_2d};

/// Content with no actions — `run_world_phase` only needs `action`/`spawn` for
/// firing, and these ships carry no queues, so the world phase is pure
/// end-of-turn bookkeeping (cooldown tick + heat dissipation).
struct Quiet;
impl Content for Quiet {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("turn_loop tests don't spawn ordnance");
    }
}

/// Set a cooldown entry on a ship by id.
fn set_cd(b: &mut Board, id: &str, weapon: &str, v: i32) {
    let s = b.cells.iter_mut().flatten().find(|s| s.id == id).unwrap();
    s.cooldowns.insert(weapon.to_string(), v);
}

/// Read a cooldown entry (0 if absent).
fn cd(b: &Board, id: &str, weapon: &str) -> i32 {
    b.cells
        .iter()
        .flatten()
        .find(|s| s.id == id)
        .and_then(|s| s.cooldowns.get(weapon).copied())
        .unwrap_or(0)
}

/// ONE `run_world_phase` ticks EVERY ship's positive cooldowns down by exactly
/// 1 — the structural heart of the chess loop ("one action = one turn = one
/// tick"). Player + two enemies, each with a different starting cooldown; after
/// a single phase each has dropped by exactly 1 (never 0, never 2). A 0 drop
/// would mean the world didn't advance; a 2 drop would mean a double-tick.
#[test]
fn one_world_phase_ticks_every_ship_cooldown_by_exactly_one() {
    // Bow-away, mountless ships: nobody can fire, so the phase is pure
    // bookkeeping and the cooldown deltas are unconfounded by combat.
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        30,
        Facing::Bow(Dir4::N),
        broadside_engine::types::Arc::Forward,
        "noop",
    );
    player.mounts.clear();
    let mut e1 = ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(0, 0),
        30,
        Facing::Bow(Dir4::N),
        broadside_engine::types::Arc::Forward,
        "noop",
    );
    e1.mounts.clear();
    let mut e2 = ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(4, 0),
        30,
        Facing::Bow(Dir4::N),
        broadside_engine::types::Arc::Forward,
        "noop",
    );
    e2.mounts.clear();
    let mut board = board_2d(vec![player, e1, e2]);

    set_cd(&mut board, "p", "w", 3);
    set_cd(&mut board, "e1", "w", 2);
    set_cd(&mut board, "e2", "w", 1);

    run_world_phase(&mut board, &Quiet);

    assert_eq!(
        cd(&board, "p", "w"),
        2,
        "player cooldown 3 -> 2 after exactly one world phase"
    );
    assert_eq!(
        cd(&board, "e1", "w"),
        1,
        "enemy1 cooldown 2 -> 1 after exactly one world phase"
    );
    assert_eq!(
        cd(&board, "e2", "w"),
        0,
        "enemy2 cooldown 1 -> 0 after exactly one world phase"
    );
}

/// A cooldown already at 0 does NOT go negative on a world phase (the tick is
/// floored). Guards against an unconditional `-= 1` that would underflow a
/// ready weapon into a negative cooldown.
#[test]
fn a_zero_cooldown_does_not_go_negative() {
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        30,
        Facing::Bow(Dir4::N),
        broadside_engine::types::Arc::Forward,
        "noop",
    );
    player.mounts.clear();
    let mut board = board_2d(vec![player]);
    set_cd(&mut board, "p", "w", 0);

    run_world_phase(&mut board, &Quiet);

    assert_eq!(
        cd(&board, "p", "w"),
        0,
        "a ready (0) cooldown stays 0, never underflows to -1"
    );
}

/// N world phases tick a cooldown down by N (until it floors at 0), then it
/// stays there. A cooldown of 2 reaches 0 after 2 phases and holds at 0 across
/// further phases — the recharge completes in exactly `cd` turns, the pacing the
/// chess loop depends on.
#[test]
fn cooldown_reaches_zero_after_exactly_n_phases_then_holds() {
    let mut player = ship_2d(
        "p",
        Faction::Player,
        Pos::new(2, 3),
        30,
        Facing::Bow(Dir4::N),
        broadside_engine::types::Arc::Forward,
        "noop",
    );
    player.mounts.clear();
    let mut board = board_2d(vec![player]);
    set_cd(&mut board, "p", "w", 2);

    run_world_phase(&mut board, &Quiet);
    assert_eq!(cd(&board, "p", "w"), 1, "after 1 phase: 2 -> 1");
    run_world_phase(&mut board, &Quiet);
    assert_eq!(
        cd(&board, "p", "w"),
        0,
        "after 2 phases: 1 -> 0 (recharge complete in exactly cd turns)"
    );
    run_world_phase(&mut board, &Quiet);
    assert_eq!(
        cd(&board, "p", "w"),
        0,
        "after 3 phases: holds at 0, does not go negative"
    );
}
