//! End-to-end combat-resolution integration test.
//!
//! The unit slices in `src/resolve.rs` exercise each combat function in
//! isolation (one `apply_damage`, one `fire_player_queue`, one
//! `decide_enemy_action`). `tests/run_loop.rs` proves the *run/sector*
//! progression machinery holds over a played-through campaign, but it
//! deliberately fields inert targets so the playthrough stays deterministic
//! and tests the loop, not the AI.
//!
//! This file proves the **combat core** holds when you drive a full
//! multi-round encounter through the real resolver with *live, armed*
//! enemies: player commits a queue, then [`run_world_phase`] advances
//! ordnance, runs each enemy through its AI ([`decide_enemy_action`] fills
//! the queue, [`fire_player_queue`] fires it), and ticks end-of-turn — round
//! after round — until one side is gone.
//!
//! What no single-module unit test can claim:
//!
//! 1. **The loop terminates.** A realistic board played round-after-round
//!    reaches a clean end state (all enemies destroyed OR the player
//!    destroyed) within a bounded number of rounds — it does not deadlock,
//!    livelock, or run forever.
//! 2. **Player death clears the cell + is detectable.** When the board kills
//!    the player, the player's cell goes empty and [`find_player_id`] returns
//!    `None` — the signal the bin's lose-branch keys off.
//! 3. **Last-enemy-down is detectable.** A win leaves zero `Faction::Enemy`
//!    ships on the board — the signal the bin's win-branch keys off.
//! 4. **No panic / underflow on edge boards.** Driving the resolver with
//!    ships at cell 0 and the fore edge, a packed lane, and empty queues
//!    must not panic (lane-index under/overflow in targeting, movement,
//!    ordnance, or splash is the class of bug this guards).

use broadside_engine::resolve::{find_player_id, resolve_round, run_world_phase, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, LaneEnd, Mount, Orientation,
    Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting, TargetingPattern,
    WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

fn frigate_shields() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace { armour: 2, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 1, charge: 0 },
        starboard: ShieldFace { armour: 1, charge: 0 },
    }
}

fn naked_shields() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace { armour: 0, charge: 0 },
        stern: ShieldFace { armour: 0, charge: 0 },
        port: ShieldFace { armour: 0, charge: 0 },
        starboard: ShieldFace { armour: 0, charge: 0 },
    }
}

/// A ship with one forward-arc mount loaded with `weapon`.
fn ship(id: &str, faction: Faction, cell: usize, hull: i32, bow: LaneEnd, weapon: &str) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        orientation: Orientation::BowOn { bow },
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 12,
        locked_out: false,
        shield_profile: frigate_shields(),
        mounts: vec![Mount { id: format!("{id}-m1"), arc: Arc::Forward, weapon: weapon.into() }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// A forward beam: `amount` raw, optimal PointBlank, fires PB/Close/Mid,
/// Forward-arc only. No falloff so adjacent shots land full.
fn beam(id: &str, amount: i32) -> Action {
    Action {
        id: id.into(),
        name: id.into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid, RangeBand::Long, RangeBand::Extreme],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount, band_falloff: Some(false) }],
        r#mod: None,
        icon: None,
    }
}

/// Content serving two beams by id ("pc_beam" for the player, "ai_beam" for
/// enemies). spawn_projectile is unused (these scenarios fire beams).
struct CombatContent {
    player_beam: Action,
    ai_beam: Action,
}
impl Content for CombatContent {
    fn action(&self, id: &str) -> Option<&Action> {
        match id {
            "pc_beam" => Some(&self.player_beam),
            "ai_beam" => Some(&self.ai_beam),
            _ => None,
        }
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("combat-loop scenarios don't fire ordnance");
    }
}

fn board(size: usize, cells: Vec<Option<Ship>>) -> Board {
    Board {
        size,
        cells,
        ordnance: Vec::new(),
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
    }
}

fn enemies_left(b: &Board) -> usize {
    b.cells.iter().flatten().filter(|s| s.faction == Faction::Enemy).count()
}

/* =========================================================================
 * 1. The combat loop terminates in a player win.
 * ====================================================================== */

#[test]
fn combat_loop_player_clears_two_armed_enemies() {
    // Player at cell 0, bow=fore (forward gun bears up-lane). Two armed
    // enemies at cells 1 and 2, both bow=aft so their forward guns bear
    // DOWN-lane on the player (they shoot back via the AI). Player hits hard
    // (8 raw, naked enemies => one shot each); enemies hit soft (1 raw) so
    // the player out-trades them and the loop ends in a win.
    let mut player = ship("player", Faction::Player, 0, 30, LaneEnd::Fore, "pc_beam");
    let mut e1 = ship("e1", Faction::Enemy, 1, 4, LaneEnd::Aft, "ai_beam");
    let mut e2 = ship("e2", Faction::Enemy, 2, 4, LaneEnd::Aft, "ai_beam");
    e1.shield_profile = naked_shields();
    e2.shield_profile = naked_shields();
    let _ = &mut player;

    let mut b = board(7, vec![
        Some(player), Some(e1), Some(e2), None, None, None, None,
    ]);
    let content = CombatContent { player_beam: beam("pc_beam", 8), ai_beam: beam("ai_beam", 1) };

    // Drive rounds: each round the player queues its beam, then resolve_round
    // fires the queue + runs the world phase (AI + end-of-turn). Bounded.
    let mut rounds = 0;
    while enemies_left(&b) > 0 && find_player_id(&b).is_some() && rounds < 32 {
        if let Some(pid) = find_player_id(&b) {
            // Queue the player's beam for this round.
            if let Some(cell) = b.cells.iter().position(|c| c.as_ref().map(|s| s.id == pid).unwrap_or(false)) {
                if let Some(s) = b.cells[cell].as_mut() {
                    s.queue.push("pc_beam".into());
                }
            }
        }
        resolve_round(&mut b, &content);
        rounds += 1;
    }

    assert!(rounds < 32, "combat loop must terminate, not run forever");
    assert_eq!(enemies_left(&b), 0, "player should clear both enemies");
    assert!(find_player_id(&b).is_some(), "player survives the win");
}

/* =========================================================================
 * 2. Player death clears the cell and is detectable.
 * ====================================================================== */

#[test]
fn combat_loop_player_death_clears_cell_and_is_detectable() {
    // A 2-hull player versus three hard-hitting armed enemies that all bear
    // on it. The player never queues a shot (it just sits), so the board
    // kills it. We assert the player's cell goes empty and find_player_id
    // returns None — the bin's lose signal.
    let player = ship("player", Faction::Player, 0, 2, LaneEnd::Fore, "pc_beam");
    let e1 = ship("e1", Faction::Enemy, 1, 20, LaneEnd::Aft, "ai_beam");
    let e2 = ship("e2", Faction::Enemy, 2, 20, LaneEnd::Aft, "ai_beam");
    let mut b = board(7, vec![
        Some(player), Some(e1), Some(e2), None, None, None, None,
    ]);
    // AI beam hits hard enough to punch through the bow (armour 2) — 6 raw.
    let content = CombatContent { player_beam: beam("pc_beam", 8), ai_beam: beam("ai_beam", 6) };

    let mut rounds = 0;
    while find_player_id(&b).is_some() && rounds < 32 {
        // Player does NOT queue — just yields to the world phase.
        run_world_phase(&mut b, &content);
        rounds += 1;
    }

    assert!(rounds < 32, "the board should kill the idle player within the bound");
    assert!(find_player_id(&b).is_none(), "dead player is detectable via find_player_id == None");
    assert!(b.cells[0].is_none(), "the player's cell is cleared on death");
}

/* =========================================================================
 * 3. Edge-board robustness — no panic / underflow.
 * ====================================================================== */

#[test]
fn combat_loop_edge_boards_do_not_panic() {
    let content = CombatContent { player_beam: beam("pc_beam", 8), ai_beam: beam("ai_beam", 3) };

    // (a) Player at the AFT edge (cell 0) firing aft-bearing nothing, enemy
    //     at the FORE edge. Exercises cell-0 aft probes + fore-edge stepping.
    {
        let mut player = ship("player", Faction::Player, 0, 20, LaneEnd::Aft, "pc_beam");
        player.queue.push("pc_beam".into());
        let e = ship("e", Faction::Enemy, 6, 6, LaneEnd::Aft, "ai_beam");
        let mut b = board(7, vec![
            Some(player), None, None, None, None, None, Some(e),
        ]);
        // Must not panic regardless of who can bear.
        resolve_round(&mut b, &content);
    }

    // (b) Fully-packed lane: every cell occupied, player at cell 0. Stresses
    //     targeting / movement / splash bounds with no free cells.
    {
        let mut cells: Vec<Option<Ship>> = Vec::new();
        cells.push(Some(ship("player", Faction::Player, 0, 20, LaneEnd::Fore, "pc_beam")));
        for i in 1..7 {
            cells.push(Some(ship(&format!("e{i}"), Faction::Enemy, i, 6, LaneEnd::Aft, "ai_beam")));
        }
        let mut b = board(7, cells);
        if let Some(s) = b.cells[0].as_mut() { s.queue.push("pc_beam".into()); }
        resolve_round(&mut b, &content);
    }

    // (c) Empty player queue + lone enemy: the world phase must no-op
    //     cleanly (enemy AI may fire or fall back, end-of-turn ticks).
    {
        let player = ship("player", Faction::Player, 3, 20, LaneEnd::Fore, "pc_beam");
        let e = ship("e", Faction::Enemy, 4, 6, LaneEnd::Aft, "ai_beam");
        let mut b = board(7, vec![
            None, None, None, Some(player), Some(e), None, None,
        ]);
        // No queue pushed. resolve_round fires an empty player queue, then
        // the world phase. Must not panic.
        resolve_round(&mut b, &content);
    }

    // (d) Single-cell board with a lone player — degenerate bounds.
    {
        let player = ship("player", Faction::Player, 0, 10, LaneEnd::Fore, "pc_beam");
        let mut b = board(1, vec![Some(player)]);
        if let Some(s) = b.cells[0].as_mut() { s.queue.push("pc_beam".into()); }
        resolve_round(&mut b, &content);
        // Lone player with no enemies: still on the board, nothing to hit.
        assert!(find_player_id(&b).is_some(), "lone player survives a no-target round");
    }
}

/* =========================================================================
 * 4. A multi-round exchange leaves a consistent board (no orphan state).
 * ====================================================================== */

#[test]
fn combat_loop_keeps_board_consistent_across_rounds() {
    // Player vs one tanky armed enemy. Run several rounds and assert that at
    // every step each ship's `cell` field matches its index in `cells` (no
    // drift between the occupant's self-reported cell and its slot — the
    // invariant movement/swap code must preserve).
    let mut player = ship("player", Faction::Player, 0, 40, LaneEnd::Fore, "pc_beam");
    player.shield_profile = naked_shields();
    let e = ship("e", Faction::Enemy, 3, 40, LaneEnd::Aft, "ai_beam");
    let mut b = board(7, vec![
        Some(player), None, None, Some(e), None, None, None,
    ]);
    let content = CombatContent { player_beam: beam("pc_beam", 2), ai_beam: beam("ai_beam", 2) };

    for _ in 0..10 {
        if let Some(pid) = find_player_id(&b) {
            if let Some(cell) = b.cells.iter().position(|c| c.as_ref().map(|s| s.id == pid).unwrap_or(false)) {
                if let Some(s) = b.cells[cell].as_mut() { s.queue.push("pc_beam".into()); }
            }
        }
        resolve_round(&mut b, &content);

        // Invariant: every occupant's self-cell equals its slot index.
        for (idx, slot) in b.cells.iter().enumerate() {
            if let Some(s) = slot {
                assert_eq!(s.cell, idx, "ship {} reports cell {} but sits at slot {}", s.id, s.cell, idx);
            }
        }
        // Invariant: ordnance never references an out-of-range cell.
        for p in &b.ordnance {
            assert!(p.cell < b.size, "ordnance {} at out-of-range cell {}", p.id, p.cell);
        }
    }
}
