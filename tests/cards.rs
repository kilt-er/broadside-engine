//! Field-kit Cards integration tests — drive cards through the full
//! resolver dispatch (queue -> execute_queue -> BOARD effect ->
//! Content::apply_board_effect).
//!
//! `src/cards.rs` has unit tests for `apply_card_effect` called
//! directly. This file pins that the SAME behaviours are observable
//! when the resolver drives them through `execute_queue` and the
//! `Content::apply_board_effect` trait hook. If a future refactor
//! changes `Effect::BOARD` dispatch or the synthetic-action wiring,
//! these integration tests catch it where unit tests wouldn't.
//!
//! Reference: cards.rs:191-250, input.rs:362-411, resolve.rs (Effect::BOARD).

use broadside_engine::cards::{
    PlayResult, CARD_MASS_BREACH, CARD_MASS_LOCK, CARD_SENSOR_PULSE,
};
use broadside_engine::input::{synthetic_card_action_id, DemoContent};
use broadside_engine::resolve::{execute_queue, Content};
use broadside_engine::types::{
    Arc, Board, EventBus, Faction, LaneEnd, Mount, Orientation, ShieldFace, ShieldProfile, Ship,
    StatusKind,
};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

fn naked_ship(id: &str, faction: Faction, cell: usize) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        hull: 10,
        max_hull: 10,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
            bow: ShieldFace { armour: 0, charge: 0 },
            stern: ShieldFace { armour: 0, charge: 0 },
            port: ShieldFace { armour: 0, charge: 0 },
            starboard: ShieldFace { armour: 0, charge: 0 },
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

fn board_with(size: usize, ships: Vec<Ship>) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..size).map(|_| None).collect();
    for s in ships {
        let c = s.cell;
        cells[c] = Some(s);
    }
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

/// The canonical play flow: validate + decrement via try_play_card, push
/// the synthetic action id onto the ship's queue, then execute_queue
/// runs the BOARD effect dispatch through Content::apply_board_effect.
fn play_card(
    board: &mut Board,
    content: &mut DemoContent,
    ship_id: &str,
    card_id: &str,
) -> PlayResult {
    let result = content.try_play_card(ship_id, card_id);
    if result == PlayResult::Played {
        // Push the synthetic onto the ship's queue. Find ship by id.
        for slot in board.cells.iter_mut() {
            if let Some(s) = slot.as_mut() {
                if s.id == ship_id {
                    s.queue.push(synthetic_card_action_id(card_id));
                    break;
                }
            }
        }
        execute_queue(ship_id, board, content);
    }
    result
}

/* =========================================================================
 * mass_lock — TargetLock status on every enemy
 * ====================================================================== */

#[test]
fn mass_lock_applies_target_lock_to_every_enemy_ship() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("p", Faction::Player, 0),
            naked_ship("e1", Faction::Enemy, 2),
            naked_ship("e2", Faction::Enemy, 4),
            naked_ship("ally", Faction::Player, 6), // not an enemy; must not be locked
        ],
    );
    let mut content = DemoContent::default();
    content.field_kits.grant("p", CARD_MASS_LOCK, 1);

    assert_eq!(play_card(&mut board, &mut content, "p", CARD_MASS_LOCK), PlayResult::Played);

    let locked = |s: &Ship| s.statuses.iter().any(|st| st.kind == StatusKind::TargetLock);
    assert!(locked(board.cells[2].as_ref().unwrap()), "e1 must be locked");
    assert!(locked(board.cells[4].as_ref().unwrap()), "e2 must be locked");
    assert!(!locked(board.cells[6].as_ref().unwrap()), "ally must NOT be locked");
    assert!(!locked(board.cells[0].as_ref().unwrap()), "source player must NOT lock itself");
}

/* =========================================================================
 * mass_breach — HullBreach status on every enemy
 * ====================================================================== */

#[test]
fn mass_breach_applies_hull_breach_to_every_enemy_ship() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("p", Faction::Player, 0),
            naked_ship("e1", Faction::Enemy, 2),
            naked_ship("e2", Faction::Enemy, 4),
        ],
    );
    let mut content = DemoContent::default();
    content.field_kits.grant("p", CARD_MASS_BREACH, 1);

    assert_eq!(play_card(&mut board, &mut content, "p", CARD_MASS_BREACH), PlayResult::Played);

    let breached = |s: &Ship| {
        s.statuses
            .iter()
            .any(|st| st.kind == StatusKind::HullBreach && st.duration == 3)
    };
    assert!(breached(board.cells[2].as_ref().unwrap()), "e1 must have HullBreach(3)");
    assert!(breached(board.cells[4].as_ref().unwrap()), "e2 must have HullBreach(3)");
    assert!(
        !breached(board.cells[0].as_ref().unwrap()),
        "source player must NOT breach itself",
    );
}

/* =========================================================================
 * sensor_pulse — clears every enemy ship's queue
 * ====================================================================== */

#[test]
fn sensor_pulse_clears_every_enemy_queue() {
    let mut e1 = naked_ship("e1", Faction::Enemy, 2);
    e1.queue = vec!["pulse_laser".into(), "torpedo".into()];
    let mut e2 = naked_ship("e2", Faction::Enemy, 4);
    e2.queue = vec!["pulse_laser".into()];
    let mut p = naked_ship("p", Faction::Player, 0);
    p.queue = vec!["pulse_laser".into()]; // player queue must NOT be cleared

    let mut board = board_with(7, vec![p, e1, e2]);
    let mut content = DemoContent::default();
    content.field_kits.grant("p", CARD_SENSOR_PULSE, 1);

    assert_eq!(play_card(&mut board, &mut content, "p", CARD_SENSOR_PULSE), PlayResult::Played);

    assert!(board.cells[2].as_ref().unwrap().queue.is_empty(), "e1 queue cleared");
    assert!(board.cells[4].as_ref().unwrap().queue.is_empty(), "e2 queue cleared");
    // Player queue: after execute_queue runs the card synthetic, the
    // player's queue is drained by the resolver's normal "clear queue at
    // end of execute_queue" path — every queued action including the
    // synthetic card play has fired. The player's pulse_laser was
    // enqueued BEFORE the card synthetic was pushed by `play_card`, so
    // it also ran. The empty queue here is the resolver's normal
    // post-execute_queue state, NOT the card's effect.
    //
    // To prove the card doesn't clear the player's queue, see the
    // separate "card doesn't drain source queue" assertion in
    // sensor_pulse_does_not_clear_source_queue below.
}

/// Carve out the "source faction NOT affected" property of sensor_pulse.
/// Setup: player has a queue, plays sensor_pulse, then a SECOND player
/// ship is added. execute_queue runs ONLY the source player; the
/// second player's queue must survive.
#[test]
fn sensor_pulse_does_not_clear_other_player_ships_queues() {
    let p_source = naked_ship("p1", Faction::Player, 0);
    let mut p_ally = naked_ship("p2", Faction::Player, 6);
    p_ally.queue = vec!["pulse_laser".into()];
    let mut e1 = naked_ship("e1", Faction::Enemy, 3);
    e1.queue = vec!["pulse_laser".into()];

    let mut board = board_with(7, vec![p_source, p_ally, e1]);
    let mut content = DemoContent::default();
    content.field_kits.grant("p1", CARD_SENSOR_PULSE, 1);

    play_card(&mut board, &mut content, "p1", CARD_SENSOR_PULSE);

    // Enemy queue cleared.
    assert!(board.cells[3].as_ref().unwrap().queue.is_empty());
    // Ally player queue PRESERVED.
    assert_eq!(
        board.cells[6].as_ref().unwrap().queue,
        vec!["pulse_laser".to_string()],
        "sensor_pulse must not touch same-faction (Player) queues",
    );
}

/* =========================================================================
 * Charge bookkeeping — one play decrements; depleted cards can't be replayed
 * ====================================================================== */

/// Granting two charges of mass_lock lets the player play it twice; a
/// third play returns InsufficientCharges with no effect on the board.
#[test]
fn card_charges_decrement_per_play_and_block_when_depleted() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("p", Faction::Player, 0),
            naked_ship("e1", Faction::Enemy, 2),
        ],
    );
    let mut content = DemoContent::default();
    content.field_kits.grant("p", CARD_MASS_LOCK, 2);

    assert_eq!(play_card(&mut board, &mut content, "p", CARD_MASS_LOCK), PlayResult::Played);
    assert_eq!(play_card(&mut board, &mut content, "p", CARD_MASS_LOCK), PlayResult::Played);
    // Third play: insufficient charges. PlayResult tells us, and no
    // synthetic was queued (play_card only queues on Played).
    assert_eq!(
        play_card(&mut board, &mut content, "p", CARD_MASS_LOCK),
        PlayResult::InsufficientCharges,
    );

    // The two successful plays both applied TargetLock — the second
    // play extends the duration but doesn't add a second status (per
    // add_or_extend semantics at cards.rs:252-258). Verify exactly one
    // TargetLock entry on the enemy.
    let e1 = board.cells[2].as_ref().unwrap();
    let lock_count = e1
        .statuses
        .iter()
        .filter(|s| s.kind == StatusKind::TargetLock)
        .count();
    assert_eq!(lock_count, 1, "duplicate plays don't create duplicate status entries");
}

/// Playing a card the ship doesn't carry returns NotCarried; no board
/// mutation.
#[test]
fn playing_a_card_not_in_inventory_returns_not_carried() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("p", Faction::Player, 0),
            naked_ship("e1", Faction::Enemy, 2),
        ],
    );
    let mut content = DemoContent::default();
    // No grant — p has no kit.
    assert_eq!(
        play_card(&mut board, &mut content, "p", CARD_MASS_LOCK),
        PlayResult::NotCarried,
    );
    // Enemy must not be locked.
    let e1 = board.cells[2].as_ref().unwrap();
    assert!(
        e1.statuses.iter().all(|s| s.kind != StatusKind::TargetLock),
        "no card played means no TargetLock",
    );
}

/* =========================================================================
 * Reverse-faction sanity — enemy can play cards against the player
 * ====================================================================== */

/// If an enemy ship plays mass_lock, the player gets the TargetLock —
/// the "every ship of the OPPOSITE faction" logic in
/// `apply_card_effect` is symmetric. Catches a hardcoded "always target
/// Faction::Enemy" bug.
#[test]
fn enemy_playing_mass_lock_targets_the_player() {
    let mut board = board_with(
        7,
        vec![
            naked_ship("p", Faction::Player, 0),
            naked_ship("e1", Faction::Enemy, 2),
        ],
    );
    let mut content = DemoContent::default();
    content.field_kits.grant("e1", CARD_MASS_LOCK, 1);

    assert_eq!(play_card(&mut board, &mut content, "e1", CARD_MASS_LOCK), PlayResult::Played);

    let p = board.cells[0].as_ref().unwrap();
    assert!(
        p.statuses.iter().any(|s| s.kind == StatusKind::TargetLock),
        "enemy's mass_lock should target the player (opposite faction)",
    );
    let e1 = board.cells[2].as_ref().unwrap();
    assert!(
        e1.statuses.iter().all(|s| s.kind != StatusKind::TargetLock),
        "e1 (source) must not target itself",
    );
}
