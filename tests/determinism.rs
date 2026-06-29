//! Structural-determinism harness.
//!
//! The Broadside engine has **no RNG**: a grep across `src/` finds no
//! `rand` / `StdRng` / `thread_rng` / `gen_range`, and the AI
//! (`decide_enemy_action`) is a pure scoring function. So determinism here
//! is not about seeding randomness — it's a guard against a future change
//! introducing an *unordered* iteration whose order leaks into outcomes.
//!
//! The state most at risk is the `HashMap`-bearing fields: `Ship.cooldowns`
//! (and any future `extras`-style map). `HashMap`'s iteration order is not
//! stable across runs (it's randomized per-process by `SipHash`). If a code
//! path ever iterates a cooldown map (or any other `HashMap`) and lets that
//! order affect which action fires, which target is picked, or the order of
//! damage application, two byte-identical starting boards could diverge.
//! This file makes that failure observable: it runs N rounds twice from
//! cloned inputs and asserts the resulting state is byte-identical via
//! `BoardSnapshot` -> `serde_json`.
//!
//! Why `BoardSnapshot` and not the live `Board`: `Board` holds an
//! `EventBus` (callbacks, not serde-able) and a transient
//! `destroys_this_window` counter. `BoardSnapshot` is exactly the
//! persistable subset — `size / cells / ordnance / hazards / patrol` — and
//! derives `PartialEq`, so two snapshots compare structurally.
//!
//! ## Why structural `==`, NOT serialized-JSON-string equality
//!
//! An earlier draft fingerprinted each board by `serde_json::to_string`.
//! That immediately failed — but for a benign reason worth recording: the
//! resolved *game state* was identical across runs, but `Ship.cooldowns` is
//! a `HashMap<String, i32>`, and `serde_json` serializes a `HashMap` in its
//! iteration order, which Rust randomizes per-process via `SipHash`. So two
//! byte-identical boards produced JSON that differed only in cooldown-key
//! order (`{"a":0,"b":0}` vs `{"b":0,"a":0}`) — same map, different bytes.
//!
//! The lesson: **gameplay determinism and save-file byte-stability are two
//! different claims.** This file tests the first via `BoardSnapshot`'s
//! derived `PartialEq` (a `HashMap` compares by content, order-independent).
//! The second — that a JSON save of identical state is byte-identical — is
//! NOT true today because of the `HashMap` ordering, and the resolver/save
//! owner should know that (flagged to the lead). `serde_json` has a
//! `preserve_order`/`BTreeMap` route if byte-stable saves ever matter (e.g.
//! for save-file diffing or content hashing); it does not affect gameplay.
//!
//! Coverage target: a multi-ship, multi-faction board with queued actions,
//! live ordnance (both pre-seeded and freshly launched), pre-set statuses,
//! and non-empty cooldown maps — so the `HashMap`-bearing state is genuinely
//! exercised across the round, not just present.

use broadside_engine::resolve::{resolve_round, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, BoardSnapshot, Effect, EventBus, Faction, LaneEnd, Mount,
    Orientation, Projectile, RangeBand, ShieldFace, ShieldProfile, Ship, Status, StatusKind,
    Targeting, TargetingPattern, WeaponArchetype,
};
use std::collections::HashMap;

/* =========================================================================
 * Content — a small action set covering a beam, an ordnance launcher, and a
 * status applier, so the round touches several effect kinds.
 * ====================================================================== */

/// A 4-damage forward beam, optimal Close, fires PointBlank/Close/Mid.
fn pulse_laser() -> Action {
    Action {
        id: "pulse_laser".into(),
        name: "Pulse Laser".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost {
            heat: 1,
            cooldown_max: 1,
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

/// A forward launcher that spawns a "torpedo" projectile. Exercises the
/// `SPAWN_ORDNANCE` -> `Content::spawn_projectile` -> ordnance-advance path.
fn launch_torpedo() -> Action {
    Action {
        id: "launch_torpedo".into(),
        name: "Launch Torpedo".into(),
        archetype: WeaponArchetype::Ordnance,
        cost: ActionCost {
            heat: 2,
            cooldown_max: 2,
            advances_turn: true,
        },
        targeting: Targeting {
            range_band: vec![
                broadside_engine::grid::Range::Adjacent,
                broadside_engine::grid::Range::Near,
                broadside_engine::grid::Range::Far,
            ],
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::ORDNANCE,
            band: vec![RangeBand::Close, RangeBand::Mid, RangeBand::Long],
            optimal_band: RangeBand::Mid,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::SPAWN_ORDNANCE {
            projectile: "torpedo".into(),
        }],
        r#mod: None,
        icon: None,
    }
}

/// A forward beam that applies `HullBreach` (damage-over-time) to its target,
/// so the round mutates `Ship.statuses` as well as hull.
fn ion_lance() -> Action {
    Action {
        id: "ion_lance".into(),
        name: "Ion Lance".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost {
            heat: 1,
            cooldown_max: 1,
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
            ],
            optimal_band: RangeBand::Mid,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::APPLY_STATUS {
            status: StatusKind::HullBreach,
            duration: 3,
        }],
        r#mod: None,
        icon: None,
    }
}

struct DetContent {
    actions: HashMap<String, Action>,
}
impl Content for DetContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }
    fn spawn_projectile(&self, kind: &str, owner: &Ship) -> Projectile {
        // Deterministic by construction: id is owner+kind+cell, no counter
        // or clock. A torpedo heading down-lane toward lower cells (the
        // player sits at 0, enemies fore of it).
        Projectile {
            id: format!("{}:{}:{}", owner.id, kind, owner.cell),
            kind: kind.into(),
            cell: owner.cell,
            pos: broadside_engine::grid::Pos::new(0, 0),
            heading: LaneEnd::Fore,
            heading8: broadside_engine::grid::Dir8::N,
            speed: 1,
            hull: 2,
            payload: vec![Effect::DAMAGE {
                amount: 3,
                band_falloff: Some(false),
            }],
            owner_faction: owner.faction,
        }
    }
}

fn content() -> DetContent {
    let mut actions = HashMap::new();
    for a in [pulse_laser(), launch_torpedo(), ion_lance()] {
        actions.insert(a.id.clone(), a);
    }
    DetContent { actions }
}

/* =========================================================================
 * Fixtures.
 * ====================================================================== */

/// A ship with a configurable mount loadout, a non-trivial cooldown map, and
/// optional statuses. The cooldown map is the whole point — it's the
/// `HashMap` whose iteration order must never leak into outcomes.
#[allow(clippy::too_many_arguments)] // a deliberately explicit test fixture; bundling these into a struct would obscure the per-test setup
fn ship(
    id: &str,
    faction: Faction,
    cell: usize,
    hull: i32,
    bow: LaneEnd,
    mounts: Vec<Mount>,
    cooldowns: &[(&str, i32)],
    statuses: Vec<Status>,
) -> Ship {
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
        heat_max: 8,
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
        mounts,
        queue: Vec::new(),
        cooldowns: cooldowns.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        statuses,
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

fn mount(id: &str, weapon: &str) -> Mount {
    Mount {
        id: id.into(),
        arc: Arc::Forward,
        weapon: weapon.into(),
    }
}

/// A 7-cell board exercising every HashMap-bearing path:
/// - player at 0 (bow=Aft so its forward arc bears up-lane on the enemies),
///   queued to fire a beam AND launch a torpedo, with a populated cooldown
///   map (multiple keys -> multiple `HashMap` entries to iterate).
/// - two enemies at 2 and 4 with their own cooldown maps and a pre-set
///   `HullBreach` status, so the AI scoring + status tick + per-ship cooldown
///   bookkeeping all run.
/// - one pre-seeded live projectile already on the lane, so the ordnance
///   advance/impact path runs from round 1 (independent of any launch).
fn busy_board() -> Board {
    let mut player = ship(
        "player",
        Faction::Player,
        0,
        12,
        LaneEnd::Aft,
        vec![mount("m1", "pulse_laser"), mount("m2", "launch_torpedo")],
        &[("pulse_laser", 0), ("launch_torpedo", 0), ("ion_lance", 0)],
        Vec::new(),
    );
    player.queue = vec!["pulse_laser".into(), "launch_torpedo".into()];

    // Enemies face the player (bow=Fore points down-lane toward cell 0), and
    // carry the ion_lance so decide_enemy_action has something to score and
    // applies a status when it fires.
    let enemy_a = ship(
        "enemy-a",
        Faction::Enemy,
        2,
        6,
        LaneEnd::Fore,
        vec![mount("e1", "ion_lance")],
        &[("ion_lance", 0)],
        vec![Status {
            kind: StatusKind::HullBreach,
            duration: 2,
            face: None,
        }],
    );
    let enemy_b = ship(
        "enemy-b",
        Faction::Enemy,
        4,
        6,
        LaneEnd::Fore,
        vec![mount("e1", "ion_lance")],
        &[("ion_lance", 1)], // mid-cooldown: a non-zero HashMap entry
        Vec::new(),
    );

    // A pre-seeded enemy torpedo already inbound toward the player.
    let inbound = Projectile {
        id: "seed-torpedo".into(),
        kind: "torpedo".into(),
        cell: 5,
        pos: broadside_engine::grid::Pos::new(0, 0),
        heading: LaneEnd::Aft, // toward lower cells / the player
        heading8: broadside_engine::grid::Dir8::N,
        speed: 1,
        hull: 2,
        payload: vec![Effect::DAMAGE {
            amount: 2,
            band_falloff: Some(false),
        }],
        owner_faction: Faction::Enemy,
    };

    Board {
        size: 7,
        cols: broadside_engine::grid::COLS,
        rows: broadside_engine::grid::ROWS,
        cells: vec![
            Some(player),
            None,
            Some(enemy_a),
            None,
            Some(enemy_b),
            None,
            None,
        ],
        ordnance: vec![inbound],
        hazards: (0..7).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: vec![],
    }
}

/// The persistable state of a board, captured for structural comparison.
/// `BoardSnapshot` derives `PartialEq`, and its `Ship.cooldowns` `HashMap`
/// compares by content — so this fingerprint is order-independent (see the
/// module doc on why string comparison would be wrong).
fn fingerprint(board: &Board) -> BoardSnapshot {
    BoardSnapshot::from(board)
}

/// Run `rounds` `resolve_rounds` on a fresh `busy_board`, re-arming the player
/// each round so the queue-driven paths keep firing, and return the
/// per-round fingerprints (index 0 = after round 1).
fn play(rounds: usize) -> Vec<BoardSnapshot> {
    let c = content();
    let mut board = busy_board();
    let mut prints = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        // Re-arm the player so the beam + launcher keep exercising the
        // queue, cooldown, and ordnance paths every round (the queue clears
        // after each resolve).
        if let Some(p) = board.cells[0].as_mut() {
            if p.faction == Faction::Player {
                p.queue = vec!["pulse_laser".into(), "launch_torpedo".into()];
            }
        }
        resolve_round(&mut board, &c);
        prints.push(fingerprint(&board));
    }
    prints
}

const ROUNDS: usize = 8;

/* =========================================================================
 * Tests.
 * ====================================================================== */

/// The core claim: two independent playthroughs from identical starting
/// inputs produce structurally-identical state after each of N rounds. If
/// any code path lets `HashMap` iteration order (or any other nondeterminism)
/// affect *outcomes*, the snapshots diverge at the round it first bites — and
/// the per-round comparison localizes exactly where. (Comparison is
/// structural via `BoardSnapshot`'s `PartialEq`, deliberately NOT serialized
/// bytes — see the module doc.)
#[test]
fn two_runs_from_identical_inputs_match_structurally_each_round() {
    let run1 = play(ROUNDS);
    let run2 = play(ROUNDS);

    assert_eq!(run1.len(), ROUNDS);
    assert_eq!(run2.len(), ROUNDS);

    for (i, (a, b)) in run1.iter().zip(run2.iter()).enumerate() {
        assert_eq!(
            a,
            b,
            "board state diverged at round {} — two runs from identical \
             inputs produced different state. The engine has no RNG, so a \
             divergence means an unordered iteration (e.g. a HashMap) leaked \
             into the round outcome.",
            i + 1,
        );
    }
}

/// Re-running the SAME starting board many times must always land on the
/// same final fingerprint. This is the cross-process guard: `HashMap` seed
/// randomization is per-process, so within one process the order is fixed —
/// but iterating the map in a way that depends on insertion-order vs
/// hash-order can still differ between two separately-built maps. Building
/// the board fresh each iteration (new `HashMaps`) catches that.
#[test]
fn repeated_independent_playthroughs_share_one_final_state() {
    let baseline = play(ROUNDS).pop().expect("ROUNDS > 0");
    for attempt in 0..12 {
        let got = play(ROUNDS).pop().expect("ROUNDS > 0");
        assert_eq!(
            got, baseline,
            "playthrough #{attempt} produced a different final state than the \
             baseline — nondeterminism across independently-built boards",
        );
    }
}

/// Guard the guard: the board must actually CHANGE over the run. A
/// determinism test that passes because nothing ever happens is worthless —
/// if a refactor made `resolve_round` a no-op, the two-runs test would still
/// pass trivially. Assert the state after round 1 differs from the initial
/// state, and the final state differs from round 1.
#[test]
fn the_board_actually_evolves_so_the_determinism_claim_is_meaningful() {
    let initial = fingerprint(&busy_board());
    let prints = play(ROUNDS);

    assert_ne!(
        initial, prints[0],
        "round 1 must mutate the board (damage, ordnance advance, status \
         tick, cooldown decrement) — otherwise the determinism claim is \
         vacuous",
    );
    assert_ne!(
        prints[0],
        prints[ROUNDS - 1],
        "the board must keep evolving across the run, not freeze after \
         round 1",
    );
}

/// Cooldown maps are the headline `HashMap` risk. Assert that the player's
/// cooldown map — which has multiple keys, the classic place an unordered
/// iteration could hide — round-trips identically across two runs. This is a
/// narrower, more legible assertion than the whole-board fingerprint: it
/// fails loudly and specifically if cooldown bookkeeping ever goes
/// order-dependent.
#[test]
fn player_cooldown_map_is_identical_across_runs() {
    let c = content();

    let cooldowns_after = |rounds: usize| -> std::collections::BTreeMap<String, i32> {
        let mut board = busy_board();
        for _ in 0..rounds {
            if let Some(p) = board.cells[0].as_mut() {
                p.queue = vec!["pulse_laser".into(), "launch_torpedo".into()];
            }
            resolve_round(&mut board, &c);
        }
        // Collect into a BTreeMap so the comparison itself is order-stable;
        // the test is about the *contents* matching, not the iteration order
        // of the HashMap.
        board.cells[0]
            .as_ref()
            .expect("player survives")
            .cooldowns
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    };

    assert_eq!(
        cooldowns_after(ROUNDS),
        cooldowns_after(ROUNDS),
        "player cooldown map differs between two identical runs",
    );
}

/// Pin the gameplay-vs-save-bytes distinction the module doc describes, so
/// the finding can't silently regress in either direction.
///
/// The board state is deterministic (asserted via structural `==` above),
/// but a JSON SAVE of two structurally-identical boards is NOT guaranteed
/// byte-identical, because `Ship.cooldowns` is a `HashMap` and `serde_json`
/// emits map keys in (per-process-randomized) iteration order. This test
/// documents that reality: structural equality holds, and the two snapshots
/// parse back to equal values, but we do NOT assert their serialized bytes
/// are equal — because they legitimately may not be.
///
/// If the save layer ever needs byte-stable output (save-file diffing,
/// content-hash dedup, deterministic test fixtures), the fix is a
/// BTreeMap-backed cooldowns field or `serde_json`'s `preserve_order`/sorted
/// serialization — NOT a gameplay change. Flagged to the resolver/save owner.
#[test]
fn structurally_equal_boards_round_trip_but_save_bytes_need_not_match() {
    let a = play(ROUNDS).pop().expect("ROUNDS > 0");
    let b = play(ROUNDS).pop().expect("ROUNDS > 0");

    // Gameplay determinism: the two snapshots are structurally equal.
    assert_eq!(a, b, "structural state must be identical across runs");

    // And each serialized form parses back to a value equal to itself —
    // serde round-trips losslessly regardless of key order.
    let a_json = serde_json::to_string(&a).expect("snapshot serializes");
    let a_back: BoardSnapshot = serde_json::from_str(&a_json).expect("snapshot deserializes");
    assert_eq!(a, a_back, "BoardSnapshot survives a serde round-trip");

    // We deliberately do NOT assert `serde_json::to_string(&a) ==
    // serde_json::to_string(&b)` — the HashMap cooldowns map can serialize
    // its keys in a different order across the two independently-built
    // boards even though the maps are equal. That's the save-byte-stability
    // caveat, not a gameplay bug.
}
