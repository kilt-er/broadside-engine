//! 2-D enemy-AI integration suite — the migration of the 14 `ai_*` tests
//! (blueprint tester lane; task #33).
//!
//! C1 moved `decide_enemy_action` into `src/ai.rs` and made the AI a 2-D ladder
//! (decide → fire-if-bears → else close → else reorient/vent), reading
//! `ship.pos`/`ship.facing` over the grid. The 14 `ai_*` tests that lived in
//! `resolve.rs`'s `#[cfg(test)]` mod were 1-D fixtures (`pos = (0,0)` for every
//! ship), so the 2-D AI saw co-located ships and 7 of them broke. This file is
//! their 2-D home: `decide_enemy_action` is **public**, the assertions are on
//! the **public** `board.cells[].queue`, and the synthetic-move constants are
//! public — so the tests migrate to an external `tests/` file with zero loss and
//! NO `src/ai.rs` (content C2) / `resolve.rs` (resolver) collision. The old 7
//! `#[ignore]`d copies in `resolve.rs` are deleted in a separate R7-gated cleanup.
//!
//! Fixtures use the shared invariant-A `board_2d`/`ship_2d` (tests/common): a
//! ship sits at `cells[pos.to_index()]` with a real bearing `facing`, so a
//! Forward mount actually bears (cardinal-exact, per the geometry2d arc model).
//! Frame: row 0 is the back (enemy) row, row ROWS-1 the front (player); a
//! Bow(W)/Bow(E) enemy on row 0 bears along the row.

mod common;

use broadside_engine::ai::decide_enemy_action;
use broadside_engine::grid::{Dir4, Facing, Pos};
use broadside_engine::input::SYNTHETIC_MOVE_DOWN;
use broadside_engine::resolve::Content;
use broadside_engine::types::{
    Action, ActionCost, Arc, Faction, Mount, Projectile, RangeBand, Ship, Targeting,
    TargetingPattern, WeaponArchetype,
};
use common::{board_2d, ship_2d};
use std::collections::HashMap;

/* =========================================================================
 * Content fixture + weapon builders (recreated from resolve.rs's ai_* helpers).
 * ====================================================================== */

/// Serves weapon `Action`s by id. `spawn_projectile` is unused (these AI
/// scenarios queue beams / synthetic moves, never ordnance).
struct AiContent {
    actions: HashMap<String, Action>,
}
impl Content for AiContent {
    fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }
    fn spawn_projectile(&self, _: &str, _: &Ship) -> Projectile {
        panic!("ai scenarios don't fire ordnance");
    }
}

fn content(actions: &[Action]) -> AiContent {
    AiContent { actions: actions.iter().map(|a| (a.id.clone(), a.clone())).collect() }
}

/// A Forward-arc beam with explicit 2-D + 1-D bands. `raw` damage; `bands_2d`
/// are the 2-D `Range`s it may fire at; optimal Adjacent. Mirrors the catalog
/// shape so `decide_enemy_action`'s in-band gate (`in_band` over `range_band`)
/// sees a real allowed set.
fn beam(id: &str, raw: i32, bands_2d: Vec<broadside_engine::grid::Range>) -> Action {
    Action {
        id: id.into(),
        name: id.into(),
        archetype: WeaponArchetype::Beam,
        cost: ActionCost { heat: 1, cooldown_max: 0, advances_turn: true },
        targeting: Targeting {
            range_band: bands_2d,
            optimal_range: broadside_engine::grid::Range::Adjacent,
            pattern: TargetingPattern::BEAM,
            band: vec![RangeBand::PointBlank, RangeBand::Close, RangeBand::Mid],
            optimal_band: RangeBand::Close,
            requires_arc: Some(Arc::Forward),
            facing_relative: true,
            hits_all: false,
        },
        effects: vec![broadside_engine::types::Effect::DAMAGE { amount: raw, band_falloff: Some(false) }],
        r#mod: None,
        icon: None,
    }
}

/// The default test weapon: a "close/near" pulse_laser — fires Adjacent+Near
/// (the over-extension deadzone keeps it OUT of Far), raw 4.
fn pulse_laser() -> Action {
    beam(
        "pulse_laser",
        4,
        vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near],
    )
}

/// Read the enemy's queue after a decide.
fn queue_of(board: &broadside_engine::types::Board, pos: Pos) -> Vec<String> {
    board.cells[pos.to_index()].as_ref().unwrap().queue.clone()
}

/* =========================================================================
 * FIRE / SELECTION tests (deterministic — nailed without the runner).
 *
 * In 2-D, an enemy bears with a Forward mount when its bow cardinal points AT
 * the player (player on the bow ray) and the target is in the weapon's band.
 * Player on the FRONT row (row ROWS-1 = 3); enemy on row 0 facing S (toward the
 * player, +row). Same column → the enemy's Bow(S) Forward arc bears down-column.
 * ====================================================================== */

#[test]
fn ai_queues_threatening_action_when_bears() {
    // Enemy at (2,1) Bow(S) bears down-column on the player at (2,3): distance 2
    // = Near, in pulse_laser's band → it queues the attack.
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 1), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    let mut board = board_2d(vec![player, enemy]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 1).to_index(), &mut board, &c);
    assert_eq!(
        queue_of(&board, Pos::new(2, 1)),
        vec!["pulse_laser".to_string()],
        "an enemy whose Forward arc bears on the player (Near band) queues the attack",
    );
}

#[test]
fn ai_picks_highest_raw_bearing_weapon() {
    // Two mounts both bear; the AI picks the higher-raw one ("heavy" raw 6 over
    // pulse_laser raw 4).
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 20, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let mut enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 2), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    enemy.mounts = vec![
        Mount { id: "m1".into(), arc: Arc::Forward, weapon: "pulse_laser".into() },
        Mount { id: "m2".into(), arc: Arc::Forward, weapon: "heavy".into() },
    ];
    let mut board = board_2d(vec![player, enemy]);
    let heavy = beam("heavy", 6, vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far]);
    let c = content(&[pulse_laser(), heavy]);
    decide_enemy_action(Pos::new(2, 2).to_index(), &mut board, &c);
    assert_eq!(
        queue_of(&board, Pos::new(2, 2)),
        vec!["heavy".to_string()],
        "among bearing options the AI picks the highest raw damage",
    );
}

#[test]
fn ai_skips_friendly_fire_only_target() {
    // The enemy's only Forward target is an ALLY (another enemy) on its ray —
    // the friendly-fire filter rejects firing; with the player not on the ray it
    // must NOT queue the attack.
    let player = ship_2d("p", Faction::Player, Pos::new(0, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let ally = ship_2d("ally", Faction::Enemy, Pos::new(2, 2), 5, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    // Shooter at (2,1) Bow(S): its Forward ray down column 2 hits the ALLY at
    // (2,2) first, not the player (who is in column 0).
    let shooter = ship_2d("e", Faction::Enemy, Pos::new(2, 1), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    let mut board = board_2d(vec![player, ally, shooter]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 1).to_index(), &mut board, &c);
    let q = queue_of(&board, Pos::new(2, 1));
    assert!(
        !q.contains(&"pulse_laser".to_string()),
        "friendly-fire-only target must not be fired on; got {q:?}",
    );
}

#[test]
fn ai_respects_lockout_only_queues_zero_heat() {
    // A locked-out enemy can't fire its heat-1 weapon; with no zero-heat
    // fallback its queue stays empty (it does not fire).
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let mut enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 2), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    enemy.locked_out = true;
    let mut board = board_2d(vec![player, enemy]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 2).to_index(), &mut board, &c);
    assert!(
        !queue_of(&board, Pos::new(2, 2)).contains(&"pulse_laser".to_string()),
        "a locked-out enemy must not queue its heat-bearing weapon",
    );
}

#[test]
fn ai_allows_action_that_lands_exactly_one_over_heat_max() {
    // The AI tolerates overheating by exactly 1 (heat_max + 1 is allowed). "warm"
    // raw 5 pushes heat to heat_max+1; it should still be queued.
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 20, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let mut enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 2), 5, Facing::Bow(Dir4::S), Arc::Forward, "warm");
    enemy.heat_max = 2;
    enemy.heat = 1; // +warm's heat (1 via beam default? — set explicitly below)
    let mut board = board_2d(vec![player, enemy]);
    // warm: a heat-2 beam so heat 1 -> 3 = heat_max(2)+1, the tolerated overshoot.
    let mut warm = beam("warm", 5, vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near]);
    warm.cost.heat = 2;
    let c = content(&[warm]);
    decide_enemy_action(Pos::new(2, 2).to_index(), &mut board, &c);
    assert_eq!(
        queue_of(&board, Pos::new(2, 2)),
        vec!["warm".to_string()],
        "AI tolerates overheating by exactly 1 (heat_max + 1 allowed)",
    );
}

/* =========================================================================
 * MANEUVER tests (7) — the C1-flipped ones. Best-guess 2-D fixtures; the exact
 * SYNTHETIC_MOVE direction the 2-D ladder picks is iterated against the RUNNER
 * in the build window (the assertion direction below is the working hypothesis:
 * an enemy NORTH of the player closes SOUTH = toward the player; with a
 * same-column setup that is SYNTHETIC_MOVE_DOWN, but the legacy tests expected
 * _LEFT on the 1-D lane — RECONCILE against C1's actual 2-D pick).
 *
 * NOTE (#33): these are drafted, NOT yet runner-verified. Marked with a TODO so
 * the build-window pass pins each direction. Kept compiling + asserting SOMETHING
 * real (a maneuver is queued, not empty) so they're not vacuous pre-iteration.
 * ====================================================================== */

/// Helper for the maneuver tests: an enemy that can't fire CLOSES toward the
/// player via the exact synthetic move. All maneuver fixtures put the enemy on a
/// back row directly NORTH of the front-row player (same column), so closing =
/// step SOUTH (+row, toward the player) = `SYNTHETIC_MOVE_DOWN` (verified against
/// the committed C1 ladder). Asserts the EXACT cardinal — a sharper claim than
/// "some move": a wrong direction (closing away / sideways) fails here.
fn assert_closes_toward_player(q: &[String], weapon: &str) {
    assert!(
        !q.contains(&weapon.to_string()),
        "enemy must NOT queue the out-of-band/illegal weapon; got {q:?}",
    );
    assert_eq!(
        q,
        &[SYNTHETIC_MOVE_DOWN.to_string()],
        "a can't-fire enemy directly north of the player closes SOUTH (toward it); got {q:?}",
    );
}

#[test]
fn ai_skips_out_of_band_action_and_closes() {
    // Enemy at (2,0) Bow(S), player at (2,3): distance 3 = Far. pulse_laser is
    // Adjacent+Near only (deadzone) → out of band → it must CLOSE toward the
    // player (south), not fire.
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 0), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    let mut board = board_2d(vec![player, enemy]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 0).to_index(), &mut board, &c);
    assert_closes_toward_player(&queue_of(&board, Pos::new(2, 0)), "pulse_laser");
}

#[test]
fn ai_closes_via_synthetic_move_when_cannot_fire() {
    // Enemy can't fire (out of band, Far) and has no movement mount → it reaches
    // for the synthetic move to close.
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 0), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    let mut board = board_2d(vec![player, enemy]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 0).to_index(), &mut board, &c);
    assert_closes_toward_player(&queue_of(&board, Pos::new(2, 0)), "pulse_laser");
}

#[test]
fn ai_falls_back_to_movement_when_nothing_bears() {
    // Enemy bow N (faces AWAY from the player to its south) → its Forward arc
    // doesn't bear → nothing fires → it maneuvers (reorient or close).
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 1), 5, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let mut board = board_2d(vec![player, enemy]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 1).to_index(), &mut board, &c);
    // Bow N can't bear the southward player; the C1 camp fix (0fea775) CLOSES
    // toward the player rather than camping empty — south = __move_down.
    assert_closes_toward_player(&queue_of(&board, Pos::new(2, 1)), "pulse_laser");
}

#[test]
fn ai_skips_action_on_cooldown_and_closes() {
    // The bearing weapon is on cooldown → can't fire → close toward the player.
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let mut enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 0), 5, Facing::Bow(Dir4::S), Arc::Forward, "pulse_laser");
    enemy.cooldowns.insert("pulse_laser".into(), 2);
    let mut board = board_2d(vec![player, enemy]);
    let c = content(&[pulse_laser()]);
    decide_enemy_action(Pos::new(2, 0).to_index(), &mut board, &c);
    assert_closes_toward_player(&queue_of(&board, Pos::new(2, 0)), "pulse_laser");
}

#[test]
fn ai_skips_action_that_overshoots_heat_budget_and_closes() {
    // Heat-constrained (not locked): firing would overshoot heat_max by >1, so
    // the AI declines and closes instead of over-committing.
    let player = ship_2d("p", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let mut enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 2), 5, Facing::Bow(Dir4::S), Arc::Forward, "bighot");
    enemy.heat_max = 2;
    enemy.heat = 2;
    let mut board = board_2d(vec![player, enemy]);
    // bighot: heat 5 → from heat 2 lands at 7, way over heat_max+1 = 3 → declined.
    let mut bighot = beam("bighot", 5, vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near]);
    bighot.cost.heat = 5;
    let c = content(&[bighot]);
    decide_enemy_action(Pos::new(2, 2).to_index(), &mut board, &c);
    assert_closes_toward_player(&queue_of(&board, Pos::new(2, 2)), "bighot");
}

// RECONCILED with the resolver (#33): `hits_all`-pierce-through-an-ally is a
// SPINAL_LINE behaviour, NOT a BEAM one. resolve_targeting_2d's BEAM branch is
// first-target-only by DESIGN and ignores `hits_all` (resolve.rs:1148-1161); only
// SPINAL_LINE honours `hits_all` to pierce every in-band occupant on the ray
// (resolve.rs:1163-1178). This matches the canonical 1-D test verbatim, whose own
// comment notes "pulse_laser is BEAM = first-target-only, so this scenario uses a
// synthetic piercing variant" and builds a SPINAL_LINE (resolve.rs:4281-4297).
// The earlier draft fired a BEAM with hits_all=true (a no-op flag for BEAM), so it
// degenerately stopped at the ally and closed. Fixed: the piercing weapon is now
// SPINAL_LINE, so the line genuinely threatens the player beyond the ally and the
// friendly-fire filter permits the shot. NOT a resolver bug — a fixture bug.
#[test]
fn ai_fires_through_ally_to_reach_player() {
    // An ally sits between the enemy and the player on the firing ray, but the
    // line ALSO threatens the player beyond → the friendly-fire filter permits
    // it (at least one cell on the ray is hostile). Enemy at (2,0) Bow(S), ally
    // at (2,1), player at (2,2): the down-column ray pierces both.
    let ally = ship_2d("ally", Faction::Enemy, Pos::new(2, 1), 5, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let player = ship_2d("p", Faction::Player, Pos::new(2, 2), 10, Facing::Bow(Dir4::N), Arc::Forward, "pulse_laser");
    let enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 0), 5, Facing::Bow(Dir4::S), Arc::Forward, "sweep");
    let mut board = board_2d(vec![ally, player, enemy]);
    // sweep: SPINAL_LINE + hits_all so the ray pierces the ally AND the player.
    let mut sweep = beam("sweep", 4, vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far]);
    sweep.targeting.pattern = TargetingPattern::SPINAL_LINE;
    sweep.targeting.hits_all = true;
    let c = content(&[sweep]);
    decide_enemy_action(Pos::new(2, 0).to_index(), &mut board, &c);
    assert_eq!(
        queue_of(&board, Pos::new(2, 0)),
        vec!["sweep".to_string()],
        "AI fires (SPINAL_LINE) through an ally when the line also threatens the player",
    );
}

#[test]
fn ai_pursuit_bonus_flips_pick_toward_the_player_hitting_action() {
    // The `Pursuit` +2 is CONDITIONAL on hitting the player, so it races a
    // higher-raw shot that does NOT hit the player. Faithful 2-D port of the
    // canonical isolation (resolve.rs ai_pursuit_bonus_*): the board holds TWO
    // player-faction ships — the real player (found FIRST = lowest cell index,
    // so player_pos) and an allied player-faction ship — and the enemy has two
    // OPPOSED arcs so each beam bears a different way.
    //
    // Enemy at (2,2) Bow(S): Forward bears S (+row), Rear bears N (-row).
    //   - real player at (2,0) [index 2, the lowest -> player_pos], hit by the
    //     REAR "weak" gun (raw 2): score 10(hit) + 2 - 0  (+2 Pursuit).
    //   - ally (player-faction) at (2,3) [index 17], hit by the FORWARD "strong"
    //     gun (raw 13): score 13 (no +10, no +2 — it doesn't hit player_pos).
    // Without Pursuit: weak 12 < strong 13 -> strong. With Pursuit: weak 14 >
    // strong 13 -> weak. So the +2 is decisive. (Contrived to isolate the term,
    // exactly as the canonical test tunes its raw gap; deleting `if pursuit &&
    // hits_player` flips the pick back to "strong" and reddens this.)
    let player = ship_2d("p", Faction::Player, Pos::new(2, 0), 10, Facing::Bow(Dir4::S), Arc::Forward, "weak");
    let ally = ship_2d("ally", Faction::Player, Pos::new(2, 3), 10, Facing::Bow(Dir4::N), Arc::Forward, "weak");
    let mut enemy = ship_2d("e", Faction::Enemy, Pos::new(2, 2), 5, Facing::Bow(Dir4::S), Arc::Forward, "weak");
    enemy.heat_max = 10; // generous so neither shot trips the heat gate
    enemy.traits = vec![broadside_engine::types::Trait::Pursuit];
    enemy.mounts = vec![
        Mount { id: "m1".into(), arc: Arc::Rear, weapon: "weak".into() }, // bears N -> player (2,0)
        Mount { id: "m2".into(), arc: Arc::Forward, weapon: "strong".into() }, // bears S -> ally (2,3)
    ];
    // Index sanity: the real player must be the FIRST player-faction cell so the
    // AI reads it as player_pos (lower cell index than the ally).
    assert!(Pos::new(2, 0).to_index() < Pos::new(2, 3).to_index(), "real player precedes ally in scan order");
    let mut board = board_2d(vec![player, ally, enemy]);
    let mut weak = beam("weak", 2, vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far]);
    weak.targeting.requires_arc = Some(Arc::Rear);
    weak.cost.heat = 0;
    let mut strong = beam("strong", 13, vec![broadside_engine::grid::Range::Adjacent, broadside_engine::grid::Range::Near, broadside_engine::grid::Range::Far]);
    strong.targeting.requires_arc = Some(Arc::Forward);
    strong.cost.heat = 0;
    let c = content(&[weak, strong]);
    decide_enemy_action(Pos::new(2, 2).to_index(), &mut board, &c);
    assert_eq!(
        queue_of(&board, Pos::new(2, 2)),
        vec!["weak".to_string()],
        "Pursuit's +2 flips the pick to the player-hitting shot over a higher-raw non-player shot",
    );
}

// (#76 deleted ai_prefers_diversifying_threat; #71's ai_fires_on_a_covered_end
// is a fire test already covered by ai_queues_threatening_action_when_bears in
// 2-D — folded, not duplicated. ai_burn_hard_trait_picks_the_hot_action and
// ai_allows_action_that_lands_exactly_one_over_heat_max cover the heat ladder.)
