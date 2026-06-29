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
        tail: None,
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
        cols: broadside_engine::grid::COLS,
        rows: broadside_engine::grid::ROWS,
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
    // Id-dedup (#214 boss): a 1×2 Pair boss has the same Ship clone in
    // two slots; count unique ship ids, not occupied cells.
    let mut seen = std::collections::HashSet::new();
    b.cells
        .iter()
        .flatten()
        .filter(|s| s.faction == Faction::Enemy)
        .filter(|s| seen.insert(s.id.clone()))
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

/// Content serving a set of weapons by id (from the live catalog). Used by the
/// multi-weapon overheat test, which fires DIFFERENT weapons in one turn.
struct MultiWeapon {
    actions: HashMap<String, Action>,
}
impl Content for MultiWeapon {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("multi-weapon heat scenario fires beams, not ordnance");
    }
}

/// Load one live action by id from the committed catalog asset (None if absent).
fn live_action(id: &str) -> Option<Action> {
    let path = std::path::Path::new("assets/broadside.catalog.json");
    if !path.exists() {
        return None;
    }
    let cat = broadside_engine::catalog::load_from_path(path).expect("catalog loads");
    cat.actions.into_iter().find(|a| a.id == id)
}

#[test]
fn multi_weapon_alpha_overheats_into_lockout_then_recovers() {
    // #184 RE-POINT (supersedes the old #73 single-pulse heat-spam premise):
    // pulse_laser is now cd 2, so ONE weapon can no longer spam itself into
    // overheat. Heat->lockout is still a real mechanic, reached by firing
    // DIFFERENT weapons in one turn. This keeps the heat / lockout / recover
    // coverage alive on the live catalog path.
    //
    // pulse_laser (heat 2, band close -> [Adjacent, Near]) + beam_cannon (heat 2,
    // band mid -> [Near, Far]) BOTH bear at distance 2 (Near). A 2-mount ship
    // firing both in one turn spends 2 + 2 = 4 heat. With heat_max 4 that hits
    // the lockout line exactly: queue both, fire both, the ship locks out. Then,
    // idle (no fire) -> passive cooling (-1/turn) drops heat below max and clears
    // the lock, so the ship can fire again ("overheat -> forced cool -> resume").
    let (Some(pulse), Some(beam)) = (live_action("pulse_laser"), live_action("beam_cannon")) else {
        eprintln!("[heat-gate test] catalog asset absent; skipping");
        return;
    };
    assert_eq!(pulse.cost.heat, 2, "pulse_laser heat 2");
    assert_eq!(beam.cost.heat, 2, "beam_cannon heat 2");

    // REAL 2-D fixture (invariant A): player (0,0) Bow(E) with TWO forward mounts;
    // dummy at (2,0) = distance 2 (Near), where BOTH weapons bear. Anchored +
    // weaponless dummy holds the cell and never fires back.
    let mut player = common::ship_2d(
        "p",
        Faction::Player,
        Pos::new(0, 0),
        99,
        Facing::Bow(Dir4::E),
        Arc::Forward,
        "pulse_laser",
    );
    // Second mount: beam_cannon, same forward arc.
    player.mounts.push(Mount {
        id: "p-m2".into(),
        arc: Arc::Forward,
        weapon: "beam_cannon".into(),
    });
    player.heat_max = 4;
    player.heat = 0;
    let mut dummy = common::dummy_2d(
        "d",
        Faction::Enemy,
        Pos::new(2, 0),
        99,
        Facing::Bow(Dir4::W),
    );
    dummy.traits = vec![Trait::Anchored];
    let mut b = common::board_2d(vec![player, dummy]);
    let content = MultiWeapon {
        actions: HashMap::from([("pulse_laser".into(), pulse), ("beam_cannon".into(), beam)]),
    };

    // Queue BOTH weapons and fire them in ONE turn. Fire only (no end_of_turn) so
    // we read the PEAK heat: pulse (heat 0 -> 2) then beam_cannon (2 -> 4 = max),
    // which trips lockout on the spot.
    if let Some(c) = b
        .cells
        .iter()
        .position(|c| c.as_ref().is_some_and(|s| s.id == "p"))
    {
        let s = b.cells[c].as_mut().unwrap();
        s.queue = vec!["pulse_laser".into(), "beam_cannon".into()];
    }
    broadside_engine::resolve::fire_player_queue("p", &mut b, &content);
    let p = b.cells.iter().flatten().find(|s| s.id == "p").unwrap();
    assert_eq!(
        p.heat, 4,
        "two different weapons (heat 2 + 2) spend 4 heat in one turn"
    );
    assert!(
        p.locked_out,
        "a 2-weapon alpha reaching heat_max (4) locks the ship out"
    );

    // Recovery: idle turns (no fire) cool -1/turn and clear the lock once heat
    // drops below heat_max -- the "overheat then resume" loop.
    for _ in 0..6 {
        run_world_phase(&mut b, &content);
    }
    let p = b.cells.iter().flatten().find(|s| s.id == "p").unwrap();
    assert!(
        !p.locked_out,
        "passive cooling clears the lockout so the ship can fire again"
    );
    assert_eq!(p.heat, 0, "idle cooling drains heat back to 0");
}

#[test]
fn pulse_laser_same_weapon_cannot_fire_three_times_in_one_turn() {
    let Some(pulse) = live_pulse_laser() else {
        eprintln!("[heat-gate test] catalog asset absent; skipping");
        return;
    };
    // #184 (supersedes the old #73 cd-0 "3-laser alpha" premise): pulse_laser is
    // now cd 2 (load-and-fire). Queuing the SAME weapon three times in one turn
    // no longer fires three times — the FIRST shot sets the cooldown and the
    // run_action fire-gate (resolve.rs ~596) blocks the 2nd + 3rd in that same
    // turn. So only ONE shot lands: heat climbs by exactly 2 (not 6), the ship
    // does NOT lock out, and the cooldown is left at cooldown_max (2). This is
    // the regression guard that one weapon cannot be spammed within a turn.
    // (You can still alpha by queuing DIFFERENT weapons; that path is unchanged.)
    assert_eq!(
        pulse.cost.cooldown_max, 2,
        "#184: pulse_laser is cd 2 (load-and-fire); guards against same-turn spam"
    );
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
    // Only the first of the three queued shots fired: +2 heat, well under max.
    assert_eq!(
        p.heat, 2,
        "only ONE pulse fired (cooldown gate blocked the 2nd + 3rd same-turn): heat 0 + 2"
    );
    assert!(
        !p.locked_out,
        "#184: one pulse (+2) is far from heat_max (6) — no instant lockout, because the weapon can't triple-fire in a turn"
    );
    assert_eq!(
        p.cooldowns.get("pulse_laser").copied(),
        Some(2),
        "#184: the single shot left pulse_laser on cooldown (cooldown_max 2)"
    );
}
