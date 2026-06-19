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

use broadside_engine::grid::{Dir4, Facing, Pos};
use broadside_engine::resolve::{find_player_id, resolve_round, run_world_phase, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, EventBus, Faction, LaneEnd, Mount, Orientation,
    Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Targeting, TargetingPattern, Trait,
    WeaponArchetype,
};
use std::collections::HashMap;

/// Shared 2-D invariant-A fixture builders. The pulse_laser heat tests below are
/// migrated onto these (now that #28 derives 2-D Range bands from the 1-D catalog
/// band, so the live pulse_laser fires in 2-D); the rest of this file still uses
/// the local 1-D ship()/board() until their 2-D rewrite — tracks #22.
mod common;

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

const fn frigate_shields() -> ShieldProfile {
    ShieldProfile {
        bow: ShieldFace {
            armour: 2,
            charge: 0,
        },
        stern: ShieldFace {
            armour: 0,
            charge: 0,
        },
        port: ShieldFace {
            armour: 1,
            charge: 0,
        },
        starboard: ShieldFace {
            armour: 1,
            charge: 0,
        },
    }
}

const fn naked_shields() -> ShieldProfile {
    ShieldProfile {
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
    }
}

/// A ship with one forward-arc mount loaded with `weapon`.
fn ship(id: &str, faction: Faction, cell: usize, hull: i32, bow: LaneEnd, weapon: &str) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 12,
        locked_out: false,
        shield_profile: frigate_shields(),
        mounts: vec![Mount {
            id: format!("{id}-m1"),
            arc: Arc::Forward,
            weapon: weapon.into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// A forward beam: `amount` raw, optimal `PointBlank`, fires PB/Close/Mid,
/// Forward-arc only. No falloff so adjacent shots land full.
fn beam(id: &str, amount: i32) -> Action {
    Action {
        id: id.into(),
        name: id.into(),
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
            band: vec![
                RangeBand::PointBlank,
                RangeBand::Close,
                RangeBand::Mid,
                RangeBand::Long,
                RangeBand::Extreme,
            ],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE {
            amount,
            band_falloff: Some(false),
        }],
        r#mod: None,
        icon: None,
    }
}

/// Content serving two beams by id ("`pc_beam`" for the player, "`ai_beam`" for
/// enemies). `spawn_projectile` is unused (these scenarios fire beams).
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
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

fn enemies_left(b: &Board) -> usize {
    b.cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .count()
}

/* =========================================================================
 * 1. The combat loop terminates in a player win.
 * ====================================================================== */

// #22 2-D: player front-centre (2,3) Bow(N), forward gun bears N up column 2.
// Two armed enemies on the column ahead at (2,2) and (2,1), both Bow(S) so their
// forward guns bear back down-column on the player (they shoot back via the AI).
// Player hits hard (8 raw, naked enemies => one shot each); enemies hit soft
// (1 raw) so the player out-trades them and the loop ends in a win.
#[test]
fn combat_loop_player_clears_two_armed_enemies() {
    let player = common::ship_2d(
        "player",
        Faction::Player,
        Pos::new(2, 3),
        30,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    let mut e1 = common::ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(2, 2),
        4,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let mut e2 = common::ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(2, 1),
        4,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    e1.shield_profile = naked_shields();
    e2.shield_profile = naked_shields();

    let mut b = common::board_2d(vec![player, e1, e2]);
    let content = CombatContent {
        player_beam: beam("pc_beam", 8),
        ai_beam: beam("ai_beam", 1),
    };

    // Drive rounds: each round the player queues its beam, then resolve_round
    // fires the queue + runs the world phase (AI + end-of-turn). Bounded.
    let mut rounds = 0;
    while enemies_left(&b) > 0 && find_player_id(&b).is_some() && rounds < 32 {
        if let Some(pid) = find_player_id(&b) {
            // Queue the player's beam for this round.
            if let Some(cell) = b
                .cells
                .iter()
                .position(|c| c.as_ref().is_some_and(|s| s.id == pid))
            {
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

// #22 2-D: a 2-hull player at (2,3) Bow(N) versus two hard-hitting armed enemies
// at (2,2)/(2,1) Bow(S) that bear back down the column. The player never queues a
// shot (it just sits), so the board kills it. We assert the player's cell goes
// empty and find_player_id returns None — the bin's lose signal.
#[test]
fn combat_loop_player_death_clears_cell_and_is_detectable() {
    let player = common::ship_2d(
        "player",
        Faction::Player,
        Pos::new(2, 3),
        2,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    let e1 = common::ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(2, 2),
        20,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let e2 = common::ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(2, 1),
        20,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let player_idx = Pos::new(2, 3).to_index();
    let mut b = common::board_2d(vec![player, e1, e2]);
    // AI beam hits hard enough to punch through the bow (armour 2) — 6 raw.
    let content = CombatContent {
        player_beam: beam("pc_beam", 8),
        ai_beam: beam("ai_beam", 6),
    };

    let mut rounds = 0;
    while find_player_id(&b).is_some() && rounds < 32 {
        // Player does NOT queue — just yields to the world phase.
        run_world_phase(&mut b, &content);
        rounds += 1;
    }

    assert!(
        rounds < 32,
        "the board should kill the idle player within the bound"
    );
    assert!(
        find_player_id(&b).is_none(),
        "dead player is detectable via find_player_id == None"
    );
    assert!(
        b.cells[player_idx].is_none(),
        "the player's cell is cleared on death"
    );
}

/* =========================================================================
 * 3. Edge-board robustness — no panic / underflow.
 * ====================================================================== */

#[test]
fn combat_loop_edge_boards_do_not_panic() {
    let content = CombatContent {
        player_beam: beam("pc_beam", 8),
        ai_beam: beam("ai_beam", 3),
    };

    // (a) Player at the AFT edge (cell 0) firing aft-bearing nothing, enemy
    //     at the FORE edge. Exercises cell-0 aft probes + fore-edge stepping.
    {
        let mut player = ship("player", Faction::Player, 0, 20, LaneEnd::Aft, "pc_beam");
        player.queue.push("pc_beam".into());
        let e = ship("e", Faction::Enemy, 6, 6, LaneEnd::Aft, "ai_beam");
        let mut b = board(7, vec![Some(player), None, None, None, None, None, Some(e)]);
        // Must not panic regardless of who can bear.
        resolve_round(&mut b, &content);
    }

    // (b) Fully-packed lane: every cell occupied, player at cell 0. Stresses
    //     targeting / movement / splash bounds with no free cells.
    {
        let mut cells: Vec<Option<Ship>> = Vec::new();
        cells.push(Some(ship(
            "player",
            Faction::Player,
            0,
            20,
            LaneEnd::Fore,
            "pc_beam",
        )));
        for i in 1..7 {
            cells.push(Some(ship(
                &format!("e{i}"),
                Faction::Enemy,
                i,
                6,
                LaneEnd::Aft,
                "ai_beam",
            )));
        }
        let mut b = board(7, cells);
        if let Some(s) = b.cells[0].as_mut() {
            s.queue.push("pc_beam".into());
        }
        resolve_round(&mut b, &content);
    }

    // (c) Empty player queue + lone enemy: the world phase must no-op
    //     cleanly (enemy AI may fire or fall back, end-of-turn ticks).
    {
        let player = ship("player", Faction::Player, 3, 20, LaneEnd::Fore, "pc_beam");
        let e = ship("e", Faction::Enemy, 4, 6, LaneEnd::Aft, "ai_beam");
        let mut b = board(7, vec![None, None, None, Some(player), Some(e), None, None]);
        // No queue pushed. resolve_round fires an empty player queue, then
        // the world phase. Must not panic.
        resolve_round(&mut b, &content);
    }

    // (d) Single-cell board with a lone player — degenerate bounds.
    {
        let player = ship("player", Faction::Player, 0, 10, LaneEnd::Fore, "pc_beam");
        let mut b = board(1, vec![Some(player)]);
        if let Some(s) = b.cells[0].as_mut() {
            s.queue.push("pc_beam".into());
        }
        resolve_round(&mut b, &content);
        // Lone player with no enemies: still on the board, nothing to hit.
        assert!(
            find_player_id(&b).is_some(),
            "lone player survives a no-target round"
        );
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
    let mut b = board(7, vec![Some(player), None, None, Some(e), None, None, None]);
    let content = CombatContent {
        player_beam: beam("pc_beam", 2),
        ai_beam: beam("ai_beam", 2),
    };

    for _ in 0..10 {
        if let Some(pid) = find_player_id(&b) {
            if let Some(cell) = b
                .cells
                .iter()
                .position(|c| c.as_ref().is_some_and(|s| s.id == pid))
            {
                if let Some(s) = b.cells[cell].as_mut() {
                    s.queue.push("pc_beam".into());
                }
            }
        }
        resolve_round(&mut b, &content);

        // Invariant: every occupant's self-cell equals its slot index.
        for (idx, slot) in b.cells.iter().enumerate() {
            if let Some(s) = slot {
                assert_eq!(
                    s.cell, idx,
                    "ship {} reports cell {} but sits at slot {}",
                    s.id, s.cell, idx
                );
            }
        }
        // Invariant: ordnance never references an out-of-range cell.
        for p in &b.ordnance {
            assert!(
                p.cell < b.size,
                "ordnance {} at out-of-range cell {}",
                p.id,
                p.cell
            );
        }
    }
}

/* =========================================================================
 * 5. Telegraph-one-turn-ahead (#67): an enemy's NEXT action is visible in
 *    its queue between player inputs, and it's what fires next phase.
 * ====================================================================== */

// #22 2-D: one armed enemy at (2,1) Bow(S) bears down column 2 on the player at
// (2,3) (distance 2 = Near, in band). High hull on both so nobody dies and we can
// observe the telegraph (#67 fire-then-decide) across multiple world phases.
#[test]
fn telegraph_persists_in_enemy_queue_between_world_phases() {
    use broadside_engine::resolve::run_world_phase;

    let mut player = common::ship_2d(
        "player",
        Faction::Player,
        Pos::new(2, 3),
        99,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields(); // so the 3-dmg telegraph lands on hull, not armour
    let e = common::ship_2d(
        "e",
        Faction::Enemy,
        Pos::new(2, 1),
        99,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let e_idx = Pos::new(2, 1).to_index();
    let player_idx = Pos::new(2, 3).to_index();
    let mut b = common::board_2d(vec![player, e]);
    let content = CombatContent {
        player_beam: beam("pc_beam", 1),
        ai_beam: beam("ai_beam", 3),
    };

    // Before any phase, the enemy has telegraphed nothing.
    assert!(
        b.cells[e_idx].as_ref().unwrap().queue.is_empty(),
        "enemy starts with an empty (un-telegraphed) queue",
    );

    // One world phase: fire-then-decide. The enemy fires its (empty) queue —
    // a no-op — then DECIDES and telegraphs its next action, left un-fired.
    run_world_phase(&mut b, &content);

    let q1 = b.cells[e_idx].as_ref().unwrap().queue.clone();
    assert_eq!(
        q1,
        vec!["ai_beam".to_string()],
        "#67: after a world phase the enemy's NEXT action is telegraphed (visible) in its queue, not fired-and-cleared",
    );
    // The player took no damage yet — the telegraphed shot has NOT fired.
    assert_eq!(
        b.cells[player_idx].as_ref().unwrap().hull,
        99,
        "the telegraphed shot is intent only — it has not dealt damage this phase",
    );

    // Next world phase: the telegraphed ai_beam FIRES (player loses hull),
    // then the enemy re-telegraphs for the following phase.
    run_world_phase(&mut b, &content);
    assert!(
        b.cells[player_idx].as_ref().unwrap().hull < 99,
        "#67: the previously-telegraphed action fires on the NEXT world phase",
    );
    assert_eq!(
        b.cells[e_idx].as_ref().unwrap().queue,
        vec!["ai_beam".to_string()],
        "the enemy re-telegraphs its next action after firing — the queue stays populated",
    );
}

/// #71: an in-band, bearing enemy FIRES and HOLDS position instead of
/// marching past its firing range. Regression for bruce's "enemies march in
/// a line, never shoot, die" — the #68 close-move had over-corrected so that
/// covered-end enemies maneuvered forever instead of firing.
// #22 + C1 (now landed): #71 fires-and-holds, on a 2-D column. Player at (2,3)
// Bow(N) naked; e1 at (2,1) Bow(S) is distance 2 = Near = IN the narrow ai_beam's
// band, so it must HOLD at (2,1) and FIRE down the column rather than march into
// the player. e2 at (2,0) Bow(S) sits behind e1 on the same column (the live
// "same lane-end" spawn shape) — its forward ray hits e1 first, so it can't fire
// the player and maneuvers; the assertion is about e1 holding + the player taking
// damage. (C1 routes decide_enemy_action through resolve_targeting_2d, closing
// the V4 desync that gated this.)
#[test]
fn enemy_fires_and_holds_when_in_band_does_not_march() {
    use broadside_engine::resolve::run_world_phase;
    // Narrow-band ai weapon (PB/Close/Mid 1-D; the live pulse_laser shape).
    let mut narrow = beam("ai_beam", 2);
    narrow.targeting.band = vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid];
    narrow.targeting.optimal_band = RangeBand::Close;
    let mut player = common::ship_2d(
        "player",
        Faction::Player,
        Pos::new(2, 3),
        99,
        Facing::Bow(Dir4::N),
        Arc::Forward,
        "pc_beam",
    );
    player.shield_profile = naked_shields(); // so hits land on hull (observable)
    let e1 = common::ship_2d(
        "e1",
        Faction::Enemy,
        Pos::new(2, 1),
        99,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let e2 = common::ship_2d(
        "e2",
        Faction::Enemy,
        Pos::new(2, 0),
        99,
        Facing::Bow(Dir4::S),
        Arc::Forward,
        "ai_beam",
    );
    let e1_start = Pos::new(2, 1).to_index();
    let mut b = common::board_2d(vec![player, e1, e2]);
    let content = CombatContent {
        player_beam: beam("pc_beam", 1),
        ai_beam: narrow,
    };

    let hull_before = b
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == "player")
        .unwrap()
        .hull;
    for _ in 0..6 {
        run_world_phase(&mut b, &content);
    }
    let e1_cell = b
        .cells
        .iter()
        .position(|c| c.as_ref().is_some_and(|s| s.id == "e1"));
    let hull_after = b
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == "player")
        .map(|s| s.hull);

    // e1 was in band at (2,1) from the start: it must HOLD there and FIRE,
    // not march toward/into the player.
    assert_eq!(
        e1_cell,
        Some(e1_start),
        "an in-band enemy holds its firing position, it does not march"
    );
    assert!(
        hull_after.unwrap() < hull_before,
        "#71: an in-band bearing enemy actually FIRES (player hull drops); got {hull_after:?} from {hull_before}",
    );
}

/* =========================================================================
 * 6. #73 heat-gate — sustained pulse_laser fire overheats; a burst is free.
 *
 * Bruce's spam fix (Option B): pulse_laser keeps cd 0 (always available) but
 * heat 1 → 2, so with the resolver's -1/turn dissipation and heat_max 6,
 * sustained single-fire climbs into lockout while a short burst stays free.
 * This drives the REAL catalog pulse_laser through the REAL resolver, so it
 * locks BOTH the catalog value (a future export can't silently drop heat back
 * to 1) AND the resolver's accumulate→lockout→cool curve. Content's lane: the
 * catalog asset + the playability of the value it sets.
 * ====================================================================== */

/// Content serving exactly one weapon by id (the real catalog `pulse_laser`).
struct OneWeapon {
    id: String,
    action: Action,
}
impl Content for OneWeapon {
    fn action(&self, id: &str) -> Option<&Action> {
        (id == self.id).then_some(&self.action)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("heat-gate scenario fires a beam, not ordnance");
    }
}

/// Load the live `pulse_laser` Action from the committed catalog asset.
/// Returns `None` if the asset is absent (some CI checkouts) so the test can
/// skip rather than fail.
fn live_pulse_laser() -> Option<Action> {
    let path = std::path::Path::new("assets/broadside.catalog.json");
    if !path.exists() {
        return None;
    }
    let cat = broadside_engine::catalog::load_from_path(path).expect("catalog loads");
    cat.actions.into_iter().find(|a| a.id == "pulse_laser")
}

/// Fire the player's `pulse_laser` once, then apply end-of-turn heat dissipation
/// — the player's per-turn heat cycle. Returns the player's (heat, `locked_out`)
/// after the turn.
///
/// We fire via the REAL resolver (`fire_player_queue` → `run_action` does the
/// heat += cost.heat + lockout-at-heat_max bookkeeping) so the accumulate side
/// is genuine, then apply the canonical -1/turn dissipation DIRECTLY to the
/// player (mirroring resolve.rs:611 `heat = (heat-1).max(0)` + unlock when
/// `heat < heat_max`). We deliberately do NOT call `run_world_phase`: that
/// would also run the dummy's enemy AI, whose close-move slides the target out
/// of the `pulse_laser`'s [Close] band (distance 2) so the player can no longer
/// bear — which would silently stop the fire and defeat the heat test. Pinning
/// the target stationary isolates the heat curve, which is what we're verifying.
fn fire_once_then_cool(b: &mut Board, content: &dyn OneWeaponLike) -> (i32, bool) {
    let pid = find_player_id(b).expect("player present");
    let pcell = b
        .cells
        .iter()
        .position(|c| c.as_ref().is_some_and(|s| s.id == pid))
        .unwrap();
    // Queue the shot only if not locked out (a locked ship can't fire — the
    // resolver's lockout gate at resolve.rs:407 would no-op it anyway).
    if !b.cells[pcell].as_ref().unwrap().locked_out {
        b.cells[pcell]
            .as_mut()
            .unwrap()
            .queue
            .push("pulse_laser".into());
    }
    broadside_engine::resolve::fire_player_queue(&pid, b, content.as_content());
    // Capture lockout at PEAK (post-fire, pre-dissipation): firing is what
    // trips it, and the canonical -1 EOT cooling immediately drops heat back
    // below heat_max and clears the flag (resolve.rs:611-613). The peak is the
    // "did this shot overheat me" signal the test cares about.
    let locked_at_peak = b
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == pid)
        .is_some_and(|s| s.locked_out);
    // Canonical end-of-turn dissipation, applied only to the player (no world
    // phase → the target never maneuvers out of band).
    if let Some(c) = b
        .cells
        .iter()
        .position(|c| c.as_ref().is_some_and(|s| s.id == pid))
    {
        if let Some(s) = b.cells[c].as_mut() {
            s.heat = (s.heat - 1).max(0);
            if s.heat < s.heat_max {
                s.locked_out = false;
            }
        }
    }
    let heat = b
        .cells
        .iter()
        .flatten()
        .find(|s| s.id == pid)
        .map_or(0, |s| s.heat);
    (heat, locked_at_peak)
}

/// Tiny trait so the helper can take the concrete `OneWeapon` by reference while
/// still handing the resolver a `&dyn Content`.
trait OneWeaponLike {
    fn as_content(&self) -> &dyn Content;
}
impl OneWeaponLike for OneWeapon {
    fn as_content(&self) -> &dyn Content {
        self
    }
}

#[test]
fn pulse_laser_sustained_fire_overheats_into_lockout() {
    let Some(pulse) = live_pulse_laser() else {
        eprintln!("[heat-gate test] catalog asset absent; skipping");
        return;
    };
    // Sanity: the catalog value the spam-fix depends on. If a future export
    // drops this back to 1, THIS is the assertion that catches it.
    assert_eq!(
        pulse.cost.heat, 2,
        "#73: pulse_laser heat must be 2 (the spam-gate value)"
    );
    assert_eq!(
        pulse.cost.cooldown_max, 0,
        "#73: pulse_laser stays cd 0 (bruce's baseline-shot constraint)"
    );

    // v2 (#22 restore, unblocked by #28): REAL 2-D fixture. #28 derives the 2-D
    // band from the 1-D catalog band — pulse_laser is "close" → Near → fires at
    // Chebyshev distance 2. So player at (0,0) Bow(E) (forward gun bears East) +
    // dummy at (2,0): distance 2 = Near, in band → the shot bears and spends
    // heat. heat_max 6 (the canonical default the curve is tuned to). Same #73
    // heat-gate assertion, now on an invariant-A board (the stale pos-(0,0)
    // fixture couldn't target).
    let mut player = common::ship_2d(
        "p",
        Faction::Player,
        Pos::new(0, 0),
        99,
        Facing::Bow(Dir4::E),
        Arc::Forward,
        "pulse_laser",
    );
    player.heat_max = 6;
    player.heat = 0;
    // Anchored + weaponless dummy: can't fire, won't maneuver → holds (2,0)
    // (Near band) every turn, a stable firing target.
    let mut dummy = common::dummy_2d(
        "d",
        Faction::Enemy,
        Pos::new(2, 0),
        99,
        Facing::Bow(Dir4::W),
    );
    dummy.traits = vec![Trait::Anchored];
    let mut b = common::board_2d(vec![player, dummy]);
    let content = OneWeapon {
        id: "pulse_laser".into(),
        action: pulse,
    };

    // Per-turn: +2 heat on fire, -1 on EOT → net +1/turn. Heat after turn N:
    // T1=1, T2=2, T3=3, T4=4, T5 fires at 4→6 = LOCKOUT (then EOT →5).
    let mut locked_turn = None;
    for turn in 1..=5 {
        let (_heat, locked) = fire_once_then_cool(&mut b, &content);
        if locked && locked_turn.is_none() {
            locked_turn = Some(turn);
        }
    }
    // Sustained fire is NOT infinite — it overheats. With heat 2 / max 6 the
    // 5th sustained shot trips lockout (it pushed heat to 6 before EOT cooled
    // it to 5). "~5-6 shots then forced vent," as bruce asked.
    assert_eq!(
        locked_turn,
        Some(5),
        "#73: sustained pulse_laser fire overheats into lockout on the 5th shot (not infinite spam)",
    );

    // After lockout, the ship can't fire; passive cooling (-1/turn) brings
    // heat back below max and clears the lock so firing resumes — the
    // "vent then resume" loop. Idle a few turns (no fire) and confirm.
    for _ in 0..6 {
        run_world_phase(&mut b, content.as_content());
    }
    let p = b.cells.iter().flatten().find(|s| s.id == "p").unwrap();
    assert!(
        !p.locked_out,
        "#73: passive cooling clears the lockout so the ship can fire again"
    );
    assert_eq!(p.heat, 0, "idle cooling drains heat back to 0");
}

#[test]
fn pulse_laser_three_shot_alpha_locks_out_instantly() {
    let Some(pulse) = live_pulse_laser() else {
        eprintln!("[heat-gate test] catalog asset absent; skipping");
        return;
    };
    // Three pulse_laser shots queued in ONE turn = 3 × heat 2 = +6 = heat_max,
    // which trips lockout immediately (a 3-laser broadside alpha overheats on
    // the spot, before any dissipation). Hull-99 dummy so nobody dies.
    // v2 (#22 restore, unblocked by #28): REAL 2-D fixture — player (0,0) Bow(E),
    // dummy (2,0) = Near band (pulse_laser "close" → Near, dist 2), so each shot
    // bears and spends heat.
    let mut player = common::ship_2d(
        "p",
        Faction::Player,
        Pos::new(0, 0),
        99,
        Facing::Bow(Dir4::E),
        Arc::Forward,
        "pulse_laser",
    );
    player.heat_max = 6;
    player.heat = 0;
    player.queue = vec![
        "pulse_laser".into(),
        "pulse_laser".into(),
        "pulse_laser".into(),
    ];
    let dummy = common::dummy_2d(
        "d",
        Faction::Enemy,
        Pos::new(2, 0),
        99,
        Facing::Bow(Dir4::W),
    );
    let mut b = common::board_2d(vec![player, dummy]);
    let content = OneWeapon {
        id: "pulse_laser".into(),
        action: pulse,
    };

    broadside_engine::resolve::fire_player_queue("p", &mut b, &content);

    let p = b.cells.iter().flatten().find(|s| s.id == "p").unwrap();
    assert!(
        p.heat >= p.heat_max,
        "3 × heat-2 alpha reaches heat_max ({} >= {})",
        p.heat,
        p.heat_max
    );
    assert!(
        p.locked_out,
        "#73: a 3-laser alpha (+6) locks out instantly"
    );
}
