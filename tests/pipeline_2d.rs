//! LIVE damage-pipeline ORDER guards — `resolve::apply_damage_2d`.
//!
//! ## Why this file exists (the gap it closes)
//!
//! `resolve.rs` carries TWO damage functions:
//!
//! - [`broadside_engine::resolve::apply_damage_2d`] — the **live** path. Every
//!   real shot routes here: `fire_player_queue` -> `apply_effect`'s DAMAGE arm,
//!   ordnance impact (`advance_projectile_2d`), and displacement collisions. It
//!   runs the #103/#104 combat model: INTEGER band falloff (penalty `[0,1,2]`),
//!   then subsystem modifiers, then target-lock `x2`, then a per-face depleting
//!   SHIELD POOL (`charge` soaks the hit, overflow reaches hull), then hull.
//! - `apply_damage` (the 1-D original) — **dead for live play**, kept only until
//!   the A3 CONTRACT deletes the 1-D world. It still uses the OLD FLOAT falloff
//!   curve (`floor(raw * 0.66)` …) and FLAT armour subtraction.
//!
//! The existing pipeline-order coverage — `tests/pipeline.rs` and the inline
//! `resolve::tests::apply_damage_*` / `apply_modifiers_runs_before_target_lock`
//! — all drive the **dead** float `apply_damage`. They pass on legacy float math
//! and would NOT catch a regression in the LIVE integer+pool pipeline (e.g.
//! someone reordering target-lock vs modifiers, or applying the shield before the
//! lock, or reverting the integer falloff back to the float curve). You can
//! hand-break the live order and that suite stays green.
//!
//! This file isolates the **live** `apply_damage_2d` and pins each step of its
//! order with hand-computed integer expected values. `canary.rs` already proves
//! the pool deplete/regen END-TO-END through `resolve_round`; this is the
//! complementary UNIT-level order proof (one assertion per pipeline property).
//!
//! ## Fixture conventions (so the expected numbers are non-magic)
//!
//! All ships are built with [`common::ship_2d`] (invariant A: `cell ==
//! pos.to_index()`). `facing_zone` (confirmed exhaustively in
//! `geometry2d::tests`): a `Bow(N)` ship hit by an attacker to its NORTH takes
//! the hit on its **Bow** (`incoming_from == bow dir`); an attacker to its
//! SOUTH lands on the **Stern**. We place attacker/target on a column and pick
//! the bow so the shot lands on the face under test. `band_falloff: Some(false)`
//! disables falloff for the shield/lock tests so the only arithmetic is
//! pool-soak + lock; the dedicated falloff test (#4) leaves it on.

use broadside_engine::grid::{Dir4, Facing, Pos, Range};
use broadside_engine::resolve::{apply_damage_2d, Content};
use broadside_engine::types::{
    Action, ActionCost, Arc, Board, Effect, Faction, Projectile, RangeBand, ShieldFace,
    ShieldProfile, Ship, Status, StatusKind, Targeting, TargetingPattern, WeaponArchetype,
};

mod common;
use common::{board_2d, naked_shields, ship_2d};

/* =========================================================================
 * Content impls — a passthrough and a fixed-modifier, to pin the ORDER of
 * the subsystem-modifier step relative to target-lock.
 * ====================================================================== */

/// Content whose `action` lookup is unused (we call `apply_damage_2d`
/// directly with a hand-built weapon) and whose `damage_modifier` defaults to
/// `0` — a clean passthrough so a test sees ONLY falloff + lock + shield.
struct NoMod;
impl Content for NoMod {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("pipeline_2d tests don't spawn ordnance via Content");
    }
}

/// Content that adds a FIXED `+n` subsystem modifier to every hit. Lets us
/// observe whether the modifier is applied BEFORE the target-lock doubling
/// (the canonical order: `2 * (raw_falloff + n)`, not `2*raw_falloff + n`).
struct FixedMod(i32);
impl Content for FixedMod {
    fn action(&self, _id: &str) -> Option<&Action> {
        None
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("pipeline_2d tests don't spawn ordnance via Content");
    }
    fn damage_modifier(&self, _attacker: &Ship, _band: Range, _board: &Board) -> i32 {
        self.0
    }
}

/* =========================================================================
 * Helpers.
 * ====================================================================== */

/// A forward beam dealing `amount` raw. `falloff` toggles the band-falloff
/// flag on its single DAMAGE effect (Some(false) disables falloff for the
/// whole call, matching the action-level predicate).
fn beam(amount: i32, falloff: bool) -> Action {
    Action {
        id: "beam".into(),
        name: "beam".into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 0, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: vec![Range::Adjacent, Range::Near, Range::Far],
            optimal_range: Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid, RangeBand::Long, RangeBand::Extreme],
            optimal_band: RangeBand::PointBlank,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![Effect::DAMAGE { amount, band_falloff: Some(falloff) }],
        r#mod: None,
        icon: None,
    }
}

/// A shield profile with a single face charged to `cap` (full pool) and the
/// rest empty. `which` selects the loaded face. Capacity (`armour`) == `cap`
/// so the pool starts FULL.
fn one_full_face(which: &str, cap: i32) -> ShieldProfile {
    let empty = ShieldFace { armour: 0, charge: 0 };
    let full = ShieldFace { armour: cap, charge: cap };
    let mut p = ShieldProfile { bow: empty, stern: empty, port: empty, starboard: empty };
    match which {
        "bow" => p.bow = full,
        "stern" => p.stern = full,
        "port" => p.port = full,
        "starboard" => p.starboard = full,
        _ => unreachable!("unknown face"),
    }
    p
}

/// Read a ship's hull by id.
fn hull(b: &Board, id: &str) -> i32 {
    b.cells.iter().flatten().find(|s| s.id == id).map(|s| s.hull).unwrap_or(i32::MIN)
}

/// Read a ship's bow-face shield charge by id.
fn bow_charge(b: &Board, id: &str) -> i32 {
    b.cells.iter().flatten().find(|s| s.id == id).unwrap().shield_profile.bow.charge
}

/* =========================================================================
 * 1. ORDER: subsystem modifier BEFORE target-lock doubling.
 * ====================================================================== */

/// The canonical pipeline (resolve.rs `apply_damage_2d`): falloff -> modifier
/// -> target-lock x2 -> shield -> hull. With a +1 modifier and a TargetLock on
/// a naked target, raw 4 (falloff off) becomes `2 * (4 + 1) = 10`. If the lock
/// were applied BEFORE the modifier it would be `2*4 + 1 = 9`. This is the
/// load-bearing order assertion on the LIVE fn (the dead `apply_damage` has its
/// own copy of this test; this one guards the path the game actually runs).
#[test]
fn apply_damage_2d_applies_modifier_before_target_lock() {
    // Attacker at (2,0) N of the target; target Bow(N) at (2,1) so the shot
    // from the north lands on the BOW. Naked shields so the post-lock damage
    // lands fully on hull (observable). Distance 1 = Adjacent (no falloff
    // anyway), and we also disable falloff to keep raw == 4.
    let attacker = ship_2d("atk", Faction::Player, Pos::new(2, 0), 50, Facing::Bow(Dir4::S), Arc::Forward, "beam");
    let mut target = ship_2d("tgt", Faction::Enemy, Pos::new(2, 1), 50, Facing::Bow(Dir4::N), Arc::Forward, "noop");
    target.shield_profile = naked_shields();
    target.statuses.push(Status { kind: StatusKind::TargetLock, duration: 5, face: None });
    let mut board = board_2d(vec![attacker, target]);
    let weapon = beam(4, false);

    apply_damage_2d(Pos::new(2, 1), 4, Pos::new(2, 0), &weapon, &mut board, &FixedMod(1));

    assert_eq!(
        hull(&board, "tgt"),
        40,
        "live pipeline: 2*(raw 4 + mod 1) = 10 off a 50 hull -> 40; \
         a hull of 41 would mean target-lock ran BEFORE the modifier",
    );
}

/* =========================================================================
 * 2. SHIELD: the strong bow POOL soaks, overflow reaches hull (NOT flat armour).
 * ====================================================================== */

/// #103/#104 Model A on the LIVE path: the hit face's `charge` POOL soaks the
/// hit down to 0 and the OVERFLOW reaches hull — it is NOT the dead 1-D flat
/// `max(0, dmg - armour)` subtraction. Bow pool 3 vs a raw-4 bow hit: soak 3,
/// pool -> 0, 1 overflows to hull. This is the model-swap canary: if someone
/// reverts `apply_damage_2d` to the 1-D `geometry::absorb_shield`, the flat
/// model would subtract armour without depleting `charge`, and BOTH the hull
/// and the post-hit pool value below would change.
#[test]
fn apply_damage_2d_strong_bow_pool_soaks_then_overflows_to_hull() {
    let attacker = ship_2d("atk", Faction::Player, Pos::new(2, 0), 50, Facing::Bow(Dir4::S), Arc::Forward, "beam");
    let mut target = ship_2d("tgt", Faction::Enemy, Pos::new(2, 1), 10, Facing::Bow(Dir4::N), Arc::Forward, "noop");
    target.shield_profile = one_full_face("bow", 3); // strong bow pool, rest empty
    let mut board = board_2d(vec![attacker, target]);
    let weapon = beam(4, false);

    apply_damage_2d(Pos::new(2, 1), 4, Pos::new(2, 0), &weapon, &mut board, &NoMod);

    assert_eq!(hull(&board, "tgt"), 9, "raw 4 vs bow pool 3: 1 overflows to hull (10 -> 9)");
    assert_eq!(bow_charge(&board, "tgt"), 0, "the bow pool is DEPLETED by the hit (charge 3 -> 0), not left intact by a flat subtract");
}

/* =========================================================================
 * 3. SHIELD: the weak (empty-pool) stern bleeds the full hit to hull.
 * ====================================================================== */

/// The directional gradient the design rewards flanking against: a face with an
/// EMPTY pool passes the full hit straight to hull. Same target as #2 but hit
/// from the SOUTH (so the shot lands on the bow-N ship's STERN, which has a 0
/// pool here). Raw 4 -> full 4 to hull. (Distance (2,1)->(2,3) is 2 = Near, so
/// falloff is disabled to keep the arithmetic about the shield, not the band.)
#[test]
fn apply_damage_2d_weak_stern_bleeds_full_hit_to_hull() {
    let attacker = ship_2d("atk", Faction::Player, Pos::new(2, 3), 50, Facing::Bow(Dir4::N), Arc::Forward, "beam");
    let mut target = ship_2d("tgt", Faction::Enemy, Pos::new(2, 1), 10, Facing::Bow(Dir4::N), Arc::Forward, "noop");
    target.shield_profile = one_full_face("bow", 3); // bow charged, stern empty
    let mut board = board_2d(vec![attacker, target]);
    let weapon = beam(4, false);

    // Shot from the SOUTH lands on the bow-N ship's STERN (empty pool).
    apply_damage_2d(Pos::new(2, 1), 4, Pos::new(2, 3), &weapon, &mut board, &NoMod);

    assert_eq!(hull(&board, "tgt"), 6, "the empty stern pool bleeds the full raw 4 to hull (10 -> 6)");
    assert_eq!(bow_charge(&board, "tgt"), 3, "the untouched bow pool is unaffected by a stern hit");
}

/* =========================================================================
 * 4. FALLOFF: integer penalty per band on the LIVE path (NOT the old float).
 * ====================================================================== */

/// #104/#44 on the LIVE path, end-to-end (not just the pure `band_falloff`):
/// the falloff is the INTEGER penalty `[0,1,2]` for Adjacent/Near/Far, NOT the
/// old float curve `floor(raw * [1.0,0.6,0.3])`. Raw 6 at Far (Chebyshev 3)
/// lands `6 - 2 = 4`, NOT `floor(6*0.3) = 1`. Naked target so the post-falloff
/// value lands on hull directly. This is the regression that pins the #44 fix
/// (long-range weapons no longer floor to 1) into the live pipeline.
#[test]
fn apply_damage_2d_far_falloff_is_integer_penalty_not_float() {
    // Distance (2,3) -> (2,0) is Chebyshev 3 = Far. Falloff ENABLED (the point).
    let attacker = ship_2d("atk", Faction::Player, Pos::new(2, 3), 50, Facing::Bow(Dir4::N), Arc::Forward, "beam");
    let mut target = ship_2d("tgt", Faction::Enemy, Pos::new(2, 0), 20, Facing::Bow(Dir4::N), Arc::Forward, "noop");
    target.shield_profile = naked_shields();
    let mut board = board_2d(vec![attacker, target]);
    let weapon = beam(6, true); // falloff ON

    assert_eq!(broadside_engine::grid::distance(Pos::new(2, 3), Pos::new(2, 0)), 3, "sanity: the shot crosses Far (dist 3)");
    apply_damage_2d(Pos::new(2, 0), 6, Pos::new(2, 3), &weapon, &mut board, &NoMod);

    assert_eq!(
        hull(&board, "tgt"),
        16,
        "Far penalty is integer -2: raw 6 -> 4 lands (20 -> 16). \
         A hull of 19 would mean the old float curve floor(6*0.3)=1 came back",
    );
}

/* =========================================================================
 * 5. TARGET-LOCK is consumed exactly once.
 * ====================================================================== */

/// Target-lock doubles ONE hit and is then removed: the first shot doubles, the
/// second (no lock left) does not. Two raw-2 shots onto a naked bow: 1st -> 4,
/// 2nd -> 2, total 6 off a 20 hull -> 14. If the lock were not consumed, both
/// would double (4 + 4 = 8 -> 12).
#[test]
fn apply_damage_2d_target_lock_doubles_only_the_first_hit() {
    let attacker = ship_2d("atk", Faction::Player, Pos::new(2, 0), 50, Facing::Bow(Dir4::S), Arc::Forward, "beam");
    let mut target = ship_2d("tgt", Faction::Enemy, Pos::new(2, 1), 20, Facing::Bow(Dir4::N), Arc::Forward, "noop");
    target.shield_profile = naked_shields();
    target.statuses.push(Status { kind: StatusKind::TargetLock, duration: 5, face: None });
    let mut board = board_2d(vec![attacker, target]);
    let weapon = beam(2, false);

    apply_damage_2d(Pos::new(2, 1), 2, Pos::new(2, 0), &weapon, &mut board, &NoMod);
    assert_eq!(hull(&board, "tgt"), 16, "first shot is doubled by the lock: 2*2 = 4 (20 -> 16)");

    apply_damage_2d(Pos::new(2, 1), 2, Pos::new(2, 0), &weapon, &mut board, &NoMod);
    assert_eq!(hull(&board, "tgt"), 14, "second shot is NOT doubled (lock already consumed): 2 (16 -> 14)");
}

/* =========================================================================
 * 6. ORDNANCE DAMAGE TIMING (#132 class): deterministic per-world-phase.
 *
 * Bruce's "damage a turn late" was a render-visibility bug (the in-flight
 * torpedo drew nothing), NOT a logic-timing bug — the impact timing IS
 * deterministic. This guard pins that timing: a speed-1 torpedo M cells from
 * its target deals ZERO damage for M-1 world phases, then its full payload on
 * phase M (the phase it reaches the occupant). Drives the LIVE world path
 * (`run_world_phase` -> `advance_ordnance` -> `advance_projectile_2d`).
 * ====================================================================== */

/// A pre-seeded enemy torpedo, speed 1, heading N up column 2 toward a player.
/// We assert the exact phase its damage lands so a future change to the
/// ordnance step cadence (e.g. an off-by-one in `advance_projectile_2d`, or
/// double-stepping) is caught.
#[test]
fn ordnance_damage_lands_on_the_exact_world_phase_it_reaches_the_target() {
    use broadside_engine::resolve::run_world_phase;

    // Player at (2,0) Bow(S), naked so the impact is observable on hull. A
    // mountless enemy at (2,3) so the ONLY thing that happens across phases is
    // the torpedo advancing (no enemy fire, no AI shot to confound the timing).
    let mut player = ship_2d("p", Faction::Player, Pos::new(2, 0), 30, Facing::Bow(Dir4::S), Arc::Forward, "noop");
    player.shield_profile = naked_shields();
    player.mounts.clear();
    let mut shooter = ship_2d("e", Faction::Enemy, Pos::new(2, 3), 30, Facing::Bow(Dir4::N), Arc::Forward, "noop");
    shooter.mounts.clear(); // never fires; pure spectator so timing is the torpedo's alone
    let mut board = board_2d(vec![player, shooter]);

    // Seed a torpedo at (2,3) heading N (toward the player up column 2), speed
    // 1, payload raw 5 (falloff-off via the payload flag; ordnance impact never
    // applies band falloff anyway). Distance (2,3)->(2,0) is 3 cells, so it
    // reaches the player on the 3rd world phase.
    board.ordnance.push(Projectile {
        id: "torp".into(),
        kind: "torpedo".into(),
        cell: Pos::new(2, 3).to_index(),
        pos: Pos::new(2, 3),
        heading: broadside_engine::types::LaneEnd::Aft,
        heading8: broadside_engine::grid::Dir8::N,
        speed: 1,
        hull: 2,
        payload: vec![Effect::DAMAGE { amount: 5, band_falloff: Some(false) }],
        owner_faction: Faction::Enemy,
    });

    // Phase 1: torpedo (2,3) -> (2,2). Not yet at the player; no damage.
    run_world_phase(&mut board, &NoMod);
    assert_eq!(hull(&board, "p"), 30, "phase 1: torpedo still in flight (at 2,2), player undamaged");
    assert_eq!(board.ordnance.len(), 1, "phase 1: torpedo still live");

    // Phase 2: torpedo (2,2) -> (2,1). Still in flight; no damage.
    run_world_phase(&mut board, &NoMod);
    assert_eq!(hull(&board, "p"), 30, "phase 2: torpedo still in flight (at 2,1), player undamaged");
    assert_eq!(board.ordnance.len(), 1, "phase 2: torpedo still live");

    // Phase 3: torpedo (2,1) -> (2,0) = the player. Payload lands NOW: raw 5 on
    // the naked hull (30 -> 25), and the torpedo is consumed.
    run_world_phase(&mut board, &NoMod);
    assert_eq!(hull(&board, "p"), 25, "phase 3: torpedo reaches the player and deals its full 5 (deterministic timing)");
    assert_eq!(board.ordnance.len(), 0, "phase 3: the torpedo is consumed on impact");
}
