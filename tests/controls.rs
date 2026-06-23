//! Phase 1 controls — end-to-end integration tests.
//!
//! `apply_intent` lives in `src/bin/broadside.rs` (it's bin-side glue
//! gated by `required-features = ["render", "runtime"]`), so this file
//! can't import it directly. Inline tests in the bin's own `mod tests`
//! cover the bin-side translation (keycode -> key, queue append, commit
//! delegates to `resolve_round`, restart rebuilds the board).
//!
//! This file pins the LIB-LEVEL contract that `apply_intent` ultimately
//! relies on: an Intent translates to an action id (via
//! `input::intent_to_action_id`), the id appends to the player's queue,
//! and `resolve_round` executes the queue. We drive the queue +
//! `resolve_round` directly with the synthetic action ids exposed by
//! `broadside_engine::input` — the same code path the bin walks via
//! `apply_intent`.
//!
//! Scenarios covered (team-lead's Phase 1 spec):
//!
//! 1. `CommitTurn` on empty queue -> no-op `resolve_round` (only EOT tick:
//!    heat decremented, cooldowns ticked, no damage emits)
//! 2. `QueueAction(pulse_laser)` -> `CommitTurn` -> action fires once
//! 3. Three `MoveLeft` -> `CommitTurn` -> ship moves up to 3 cells, clamped
//!    at the edge (with the `resolve_self_move` stub: thrust in the ship's
//!    bow direction, so `MoveRight` on bow=Fore moves Fore; clamping is at
//!    board edge OR at the first occupied cell on the path)
//! 4. Vent -> `CommitTurn` -> heat 0, `locked_out` cleared
//! 5. `ReorientFlip` -> `CommitTurn` -> orientation flipped
//! 6. Synthetic actions have heat 0 and `cooldown_max` 0 — pinned at the
//!    engine boundary by running them with non-zero starting heat and
//!    asserting heat is unchanged (modulo the EOT -1 dissipation)

use broadside_engine::input::{
    intent_to_action_id, key_to_intent, DemoContent, Intent, Key, SYNTHETIC_MOVE_LEFT,
    SYNTHETIC_MOVE_RIGHT, SYNTHETIC_REORIENT_FLIP, SYNTHETIC_VENT,
};
use broadside_engine::resolve::resolve_round;
use broadside_engine::types::{
    Arc, Board, EventBus, Faction, Hook, HookContext, LaneEnd, Mount, Orientation, ShieldFace,
    ShieldProfile, Ship,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

/// Player at `cell`, bow=Fore, two Forward mounts (`pulse_laser` + torpedo)
/// matching the demo binary's `player_ship` factory at
/// `bin/broadside.rs:168-179`. Default shield profile.
fn player_ship(cell: usize) -> Ship {
    Ship {
        id: "player".into(),
        faction: Faction::Player,
        cell,
        pos: broadside_engine::grid::Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::S),
        hull: 10,
        max_hull: 10,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: ShieldProfile {
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
        },
        mounts: vec![
            Mount {
                id: "m1".into(),
                arc: Arc::Forward,
                weapon: "pulse_laser".into(),
            },
            Mount {
                id: "m2".into(),
                arc: Arc::Forward,
                weapon: "torpedo".into(),
            },
        ],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// 7-cell board with the player at cell 0 and no enemies. No enemies
/// means the enemy phase is a no-op (no AI scoring, no friendly fire from
/// task #49) — keeps these tests focused on player-input mechanics.
fn solo_board() -> Board {
    let player = player_ship(0);
    let mut cells: Vec<Option<Ship>> = (0..7).map(|_| None).collect();
    cells[0] = Some(player);
    Board {
        size: 7,
        cells,
        ordnance: Vec::new(),
        hazards: (0..7).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

/// Mirror of the bin's `apply_intent` glue, lifted into the test so we
/// don't depend on the bin's render-feature-gated module. Translates an
/// Intent into a queue mutation OR a resolver call, depending on the
/// variant. Returns true if board state changed (matching the bin's
/// signature so future test cases can assert on the return value).
#[allow(clippy::needless_pass_by_value)] // mirrors the bin's `apply_intent` signature on purpose
fn apply_intent_lib(intent: Intent, board: &mut Board, content: &DemoContent) -> bool {
    match intent {
        Intent::CommitTurn => {
            resolve_round(board, content);
            true
        }
        Intent::Restart => {
            *board = solo_board();
            true
        }
        _ => {
            let Some(id) = intent_to_action_id(&intent) else {
                return false;
            };
            let Some(player_cell) = board
                .cells
                .iter()
                .position(|c| matches!(c, Some(s) if s.faction == Faction::Player))
            else {
                return false;
            };
            if let Some(ship) = board.cells[player_cell].as_mut() {
                ship.queue.push(id.to_string());
                true
            } else {
                false
            }
        }
    }
}

/// Wire a recording bus capturing (cell, amount) for `OnDamageTaken` — used
/// by tests that assert "no shots fired" (empty queue) or "exactly one
/// shot landed".
fn wire_damage_log(board: &mut Board) -> Rc<RefCell<Vec<(usize, i32)>>> {
    let log: Rc<RefCell<Vec<(usize, i32)>>> = Rc::new(RefCell::new(Vec::new()));
    let inner = Rc::clone(&log);
    board
        .bus
        .on(Hook::OnDamageTaken, move |ctx: &mut HookContext| {
            if let (Some(c), Some(a)) = (ctx.target_cell, ctx.amount) {
                inner.borrow_mut().push((c, a));
            }
        });
    log
}

/* =========================================================================
 * 1. CommitTurn on empty queue -> only EOT tick observable
 * ====================================================================== */

#[test]
fn commit_turn_on_empty_queue_runs_only_end_of_turn() {
    let mut board = solo_board();
    // Pre-charge the player with some heat + a stale cooldown so EOT
    // bookkeeping is observable.
    if let Some(p) = board.cells[0].as_mut() {
        p.heat = 3;
        p.cooldowns.insert("pulse_laser".into(), 2);
    }
    let log = wire_damage_log(&mut board);
    let content = DemoContent::default();

    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    let p = board.cells[0].as_ref().unwrap();
    // EOT: heat -= 1, cooldowns -= 1, no firings.
    assert_eq!(
        p.heat, 2,
        "empty-queue commit should still dissipate 1 heat at EOT"
    );
    assert_eq!(
        p.cooldowns.get("pulse_laser").copied(),
        Some(1),
        "cooldown should tick down even with empty queue",
    );
    assert!(p.queue.is_empty(), "no queue items added");
    assert!(
        log.borrow().is_empty(),
        "empty queue must produce zero OnDamageTaken emits",
    );
}

/* =========================================================================
 * 2. QueueAction(pulse_laser) -> CommitTurn -> fires once
 * ====================================================================== */

#[test]
fn queue_pulse_laser_then_commit_fires_once_against_a_target() {
    // v2 (#40 restore) + #104 integer falloff: REAL 2-D fixture. solo_board's
    // player is at pos (0,0); turn its bow EAST so the Forward gun bears down row
    // 0, and place a zero-shield target at (2,0) = Near band (pulse_laser "close"
    // -> Near, dist 2 per #28). Cell index 2 = Pos(2,0).to_index() (invariant A).
    // The 2-D damage is the INTEGER falloff raw - 1 at Near = 4 - 1 = 3, onto the
    // empty stern pool (charge 0 -> nothing soaked). Does NOT touch solo_board or
    // the movement tests (their lone player is unchanged; the target is local).
    let mut board = solo_board();
    if let Some(p) = board.cells[0].as_mut() {
        p.facing = broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::E);
    }
    let target_pos = broadside_engine::grid::Pos::new(2, 0);
    let target = Ship {
        id: "target".into(),
        faction: Faction::Enemy,
        cell: target_pos.to_index(),
        pos: target_pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        // Bow West: nose toward the player at (0,0), so the weak stern faces the
        // incoming East-bound shot (armour 0 → full post-falloff damage lands).
        facing: broadside_engine::grid::Facing::Bow(broadside_engine::grid::Dir4::W),
        hull: 10,
        max_hull: 10,
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
        mounts: vec![],
        queue: vec![],
        cooldowns: HashMap::new(),
        statuses: vec![],
        traits: vec![],
        klass: None,
    };
    board.cells[target_pos.to_index()] = Some(target);

    let log = wire_damage_log(&mut board);
    let content = DemoContent::default();

    apply_intent_lib(
        Intent::QueueAction("pulse_laser".into()),
        &mut board,
        &content,
    );
    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    // Exactly one OnDamageTaken on the target's cell, with the 2-D integer
    // post-falloff amount (#104): raw 4 - 1 (Near penalty) = 3, onto an empty
    // stern pool so all 3 reaches hull.
    assert_eq!(
        *log.borrow(),
        vec![(target_pos.to_index(), 3)],
        "exactly one OnDamageTaken emit for the target cell with the integer post-falloff 3 damage",
    );
    let p = board.cells[0].as_ref().unwrap();
    assert!(
        p.queue.is_empty(),
        "queue should be drained after resolve_round"
    );
}

/* =========================================================================
 * 3. Three thrusts -> CommitTurn -> ship moves up to 3 cells
 *
 * Per task #50, Effect::DISPLACE_SELF now carries a `direction:
 * Option<LaneEnd>` field. `synthetic_move_left` encodes Some(Aft) and
 * `synthetic_move_right` encodes Some(Fore), so MoveLeft and MoveRight
 * are LANE-RELATIVE (independent of bow direction) — the player Left
 * key reliably moves toward cell 0, Right toward `board.size - 1`.
 *
 * Per task #52, execute_queue tracks the ship by id rather than by a
 * fixed cell index, so a queued sequence of DISPLACE_SELFs all execute
 * even as the ship moves between cells.
 * ====================================================================== */

/// (#167 no-strafe) Three `MoveRight` thrusts queued, one `CommitTurn` drains
/// the queue. The `solo_board` player faces `Bow(S)` (forward axis = N/S), so
/// `MoveRight` = `Dir4::E` is PERPENDICULAR = a lateral strafe. The resolver's
/// no-strafe gate REJECTS each one (no-op), so the player stays at its starting
/// cell even though all three actions ran. The queue still drains (the actions
/// execute; they just decline to move a lateral step).
#[test]
fn three_lateral_thrusts_then_commit_are_gated_player_unmoved() {
    let mut board = solo_board();
    let content = DemoContent::default();

    apply_intent_lib(Intent::MoveRight, &mut board, &content);
    apply_intent_lib(Intent::MoveRight, &mut board, &content);
    apply_intent_lib(Intent::MoveRight, &mut board, &content);

    // Pre-commit: three synthetic action ids in the queue.
    {
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.queue.len(), 3, "three thrusts should queue three actions");
        assert!(
            p.queue.iter().all(|id| id == SYNTHETIC_MOVE_RIGHT),
            "all three should be the move_right synthetic",
        );
    }

    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    // Lateral move gated -> the player never left cell 0.
    let p = board.cells[0]
        .as_ref()
        .expect("player still at starting cell");
    assert_eq!(p.faction, Faction::Player);
    assert!(p.queue.is_empty(), "queue drained after commit");
    // And no phantom move to col 3 (cell 3 stays empty).
    assert!(
        board.cells[3].is_none(),
        "lateral strafe rejected — player did NOT advance to cell 3"
    );
}

/// (#167 no-strafe) Repeated lateral move intents never advance the ship — so
/// they also can never overshoot the board edge. Player at cell 5 facing
/// `Bow(S)` (forward axis = N/S); three `MoveRight` (= `Dir4::E`) thrusts are
/// each PERPENDICULAR = a lateral strafe, which the resolver's no-strafe gate
/// rejects (no-op). The player stays at cell 5 — no movement, no overshoot, no
/// panic. (Forward-direction edge clamping is covered by the resolver's own
/// `rsm2d_*` mode tests; it can no longer be reached via `MoveRight`, which is
/// lateral under tank controls.)
#[test]
fn lateral_thrusts_never_advance_so_never_overshoot_edge() {
    let mut board = solo_board();
    // Move the player to cell 5 manually (bypassing intents for the setup).
    let player = board.cells[0].take().unwrap();
    board.cells[5] = Some(Ship { cell: 5, ..player });

    let content = DemoContent::default();
    apply_intent_lib(Intent::MoveRight, &mut board, &content);
    apply_intent_lib(Intent::MoveRight, &mut board, &content);
    apply_intent_lib(Intent::MoveRight, &mut board, &content);
    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    // Lateral MoveRight is gated -> the player never left cell 5.
    assert!(
        board.cells[5]
            .as_ref()
            .is_some_and(|s| s.faction == Faction::Player),
        "lateral strafe rejected — player stays at cell 5 (no overshoot)",
    );
    assert!(
        board.cells[6].is_none(),
        "no advance to cell 6 — the move was gated, not clamped"
    );
    assert_eq!(board.size, 7);
}

/* =========================================================================
 * 4. Vent -> CommitTurn -> heat 0, locked_out cleared
 * ====================================================================== */

#[test]
fn vent_then_commit_resets_heat_and_clears_lockout() {
    let mut board = solo_board();
    if let Some(p) = board.cells[0].as_mut() {
        p.heat = 6;
        p.locked_out = true;
        p.cooldowns.insert("pulse_laser".into(), 2);
    }
    let content = DemoContent::default();

    apply_intent_lib(Intent::Vent, &mut board, &content);
    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    let p = board.cells[0].as_ref().unwrap();
    // Synthetic vent: heat -= 3 (6 -> 3), locked_out cleared, cooldowns
    // recharged to 0. Then EOT: heat -= 1 (3 -> 2), cooldowns tick at 0
    // unchanged.
    assert_eq!(p.heat, 2, "6 - 3 (vent) - 1 (EOT) = 2");
    assert!(!p.locked_out, "vent must clear lockout");
    assert_eq!(
        p.cooldowns.get("pulse_laser").copied(),
        Some(0),
        "recharge_cooldowns: true resets pulse_laser to 0; EOT does not decrement past 0",
    );
}

/* =========================================================================
 * 5. ReorientFlip -> CommitTurn -> orientation flipped
 * ====================================================================== */

#[test]
fn reorient_flip_then_commit_swaps_bow_direction() {
    let mut board = solo_board();
    {
        let p = board.cells[0].as_ref().unwrap();
        assert_eq!(p.orientation, Orientation::BowOn { bow: LaneEnd::Fore });
    }
    let content = DemoContent::default();

    apply_intent_lib(Intent::ReorientFlip, &mut board, &content);
    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    let p = board.cells[0].as_ref().unwrap();
    assert_eq!(
        p.orientation,
        Orientation::BowOn { bow: LaneEnd::Aft },
        "Flip should swap bow=Fore -> bow=Aft",
    );
}

#[test]
fn reorient_flip_twice_returns_to_original_orientation() {
    let mut board = solo_board();
    let content = DemoContent::default();

    apply_intent_lib(Intent::ReorientFlip, &mut board, &content);
    apply_intent_lib(Intent::ReorientFlip, &mut board, &content);
    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    let p = board.cells[0].as_ref().unwrap();
    assert_eq!(
        p.orientation,
        Orientation::BowOn { bow: LaneEnd::Fore },
        "Two flips in one turn should restore the original orientation",
    );
}

/* =========================================================================
 * 6. Synthetic actions are cost-free at the resolver boundary
 *
 * input.rs:591-605 inline-asserts the Action structs have heat: 0 and
 * cooldown_max: 0. This test exercises the same property dynamically:
 * with non-zero starting heat and one of each synthetic queued, heat
 * after CommitTurn should reflect ONLY the EOT -1 dissipation, not the
 * synthetic's cost.
 * ====================================================================== */

#[test]
fn synthetic_actions_dont_advance_heat_or_set_cooldown() {
    let mut board = solo_board();
    if let Some(p) = board.cells[0].as_mut() {
        p.heat = 3;
    }
    let content = DemoContent::default();

    // Queue one MoveRight + one ReorientFlip. Both execute in one commit.
    // (#167 no-strafe: the demo player faces Bow(S), so the lateral MoveRight
    // is gated to a no-op — the ship does NOT relocate — but the action still
    // RUNS, so its cost bookkeeping applies all the same.) Property under test:
    // heat is unchanged (modulo the EOT -1 dissipation) and both synthetic
    // cooldowns are set to 0 by execute_queue, regardless of the gated move.
    apply_intent_lib(Intent::MoveRight, &mut board, &content);
    apply_intent_lib(Intent::ReorientFlip, &mut board, &content);
    apply_intent_lib(Intent::CommitTurn, &mut board, &content);

    // Look up the player wherever it is — the gated MoveRight left it in place,
    // and the find() is robust to either outcome.
    let p = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.faction == Faction::Player)
        .expect("player survives the round");
    // Both synthetics ran; each is heat 0 cost 0. EOT decrements heat
    // by 1. Starting heat 3 -> 2.
    assert_eq!(
        p.heat, 2,
        "synthetics must not add heat; only EOT -1 applies"
    );
    // Cooldowns: each synthetic sets cooldowns[id] = 0 unconditionally
    // (cooldown_max == 0). EOT does not decrement past 0. Both synthetic
    // ids are present with value 0.
    assert_eq!(p.cooldowns.get(SYNTHETIC_MOVE_RIGHT).copied(), Some(0));
    assert_eq!(p.cooldowns.get(SYNTHETIC_REORIENT_FLIP).copied(), Some(0));
}

/* =========================================================================
 * 7. Synthetic ids parity check — pin the public surface
 *
 * Architect's inline test asserts the synthetic ids start with `__`. This
 * is the integration-layer mirror so a Cargo.toml feature gate or a
 * crate rename can't accidentally hide the ids from the public surface
 * tests/controls.rs depends on.
 * ====================================================================== */

#[test]
fn synthetic_action_ids_are_lib_public() {
    // The fact that these compile against the lib API is itself the test —
    // if any of these stops being `pub`, this file fails to build.
    let _ = SYNTHETIC_MOVE_LEFT;
    let _ = SYNTHETIC_MOVE_RIGHT;
    let _ = SYNTHETIC_REORIENT_FLIP;
    let _ = SYNTHETIC_VENT;
    // And the contract: all four are `__`-prefixed.
    for id in [
        SYNTHETIC_MOVE_LEFT,
        SYNTHETIC_MOVE_RIGHT,
        SYNTHETIC_REORIENT_FLIP,
        SYNTHETIC_VENT,
    ] {
        assert!(
            id.starts_with("__"),
            "synthetic id {id} must be __-prefixed"
        );
    }
}

/* =========================================================================
 * 8. key_to_intent table contract — every canonical key in one place
 *
 * Architect's inline tests in src/input.rs exhaustively cover each key
 * one-by-one. This integration-layer test puts every binding in a single
 * table so a future refactor that drops a key from the mapping shows up
 * as one obvious failure with all 10 bindings side-by-side. Per
 * team-lead's "pins content's mapping table as a contract."
 * ====================================================================== */

#[test]
fn key_to_intent_table_pins_every_canonical_binding() {
    // A player with three mounts so D1/D2/D3 all resolve to a weapon id.
    // Mount weapons are distinct strings so we can assert the exact id
    // returned by key_to_intent for each digit.
    let mut player = player_ship(0);
    player.mounts = vec![
        Mount {
            id: "m1".into(),
            arc: Arc::Forward,
            weapon: "weapon_a".into(),
        },
        Mount {
            id: "m2".into(),
            arc: Arc::Forward,
            weapon: "weapon_b".into(),
        },
        Mount {
            id: "m3".into(),
            arc: Arc::Forward,
            weapon: "weapon_c".into(),
        },
    ];
    let content = DemoContent::default();

    // The canonical key->intent table. If a future PR drops a row, this fails
    // with the offending key visible.
    //
    // (#165 tank controls) Left/Right ROTATE (no strafe) — same intents as Q/E.
    // This row was a pre-#165 strafe leftover (`Left -> MoveLeft`); corrected here
    // to the shipped behavior (input.rs `key_to_intent` returns RotateLeft/Right,
    // and the inline `key_to_intent_is_tank_controls` unit test already pins it).
    // Forward/reverse now live on Up/Down and are facing-relative, so they aren't
    // in this fixed-intent table.
    let cases: &[(Key, Intent)] = &[
        (Key::Left, Intent::RotateLeft),
        (Key::Right, Intent::RotateRight),
        (Key::Tab, Intent::ReorientFlip),
        (Key::V, Intent::Vent),
        (Key::D1, Intent::QueueAction("weapon_a".into())),
        (Key::D2, Intent::QueueAction("weapon_b".into())),
        (Key::D3, Intent::QueueAction("weapon_c".into())),
        (Key::R, Intent::CommitTurn),
        (Key::Space, Intent::CommitTurn),
        (Key::Enter, Intent::Restart),
    ];
    for (key, want) in cases {
        let got = key_to_intent(*key, &player, &content);
        assert_eq!(
            got.as_ref(),
            Some(want),
            "key_to_intent({key:?}) returned {got:?}, expected {want:?}",
        );
    }
}
