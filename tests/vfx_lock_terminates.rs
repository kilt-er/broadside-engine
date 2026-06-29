//! `CombatVfx::is_active()` MUST always terminate (no soft-lock guard).
//!
//! ## Why this exists
//!
//! Tracker `#209 hook 2` (`e24db79`) added `&& !self.vfx.is_active()` to the
//! `beat_playback` turn-advance unlock gate in `src/bin/broadside.rs`. The new
//! gate trades turn responsiveness for "the beam visibly lands before the
//! next turn fires" — but if `is_active()` ever stuck at `true` for an alive
//! board, the turn would NEVER advance and the game would soft-lock.
//!
//! This file asserts the invariant: no matter what combination of effect
//! families spawns from one `observe()` of a board diff, every `advance(dt)`
//! loop drains `is_active()` to `false` in BOUNDED time. Beat-pacing tuning
//! (Bruce's `life_secs` dials) can shift the exact duration, so the assert
//! bounds against a generous safety cap (≈ 2.0 s) — well above the longest
//! known effect lifetime — rather than an exact value.
//!
//! Sources of the upper bound (`src/effects.rs` defaults at 25877f2):
//!
//!   - `Explosion::life_secs` = 0.55 s
//!   - `ExplosionReflection` = `start_delay + life_secs` =
//!     (chebyshev × 0.08) + 0.45 = ≤ 4 × 0.08 + 0.45 = 0.77 s on a 5×4 board
//!   - `ShotBeam` per-archetype max = 0.40 s (Ordnance)
//!   - `HitFlash::life_secs` = 0.30 s
//!   - `Trail::life_secs` = 0.35 s
//!   - `TelegraphFire::life_secs` = 0.32 s
//!
//! So a single round's worth of simultaneous spawns drains in ≤ 0.77 s.
//! `SAFETY_CAP_SECS` is 2.0 s — over 2.5× the analytic max, so a future
//! `life_secs` retune (Bruce's dials) can move within that envelope without
//! flaking the test. If the cap is ever EXCEEDED, that's either a real
//! soft-lock bug OR Bruce has retuned an effect past 2 s (in which case
//! flag the test back to me, don't bump the cap silently — the cap is the
//! design contract that the turn unlocks "soon," not "eventually").
//!
//! ## What this DOESN'T test
//!
//! Continuously-respawning effects (e.g. a future loop that calls
//! `observe()` every advance frame on a permanently-changing board) would
//! keep `is_active()` true forever — but that's outside the live unlock
//! gate's path (`is_active()` is checked between turns, not within a
//! single advance loop). The guard here pins the per-turn drain time.

#![cfg(feature = "render")]

use broadside_engine::effects::{
    Explosion, ExplosionReflection, HitFlash, ShotBeam, TelegraphFire, Trail,
};
use broadside_engine::grid::{Dir4, Facing, Pos};
use broadside_engine::types::{
    Arc, Board, EventBus, Faction, FireEvent, LaneEnd, Mount, Orientation, ShieldFace,
    ShieldProfile, Ship, WeaponArchetype,
};
use broadside_engine::vfx::CombatVfx;
use std::collections::HashMap;

/// Safety upper bound on `is_active()` drain time (seconds). Set well above
/// the analytic ~0.77 s max so a per-effect `life_secs` retune (Bruce's
/// pacing dials) stays inside the envelope. A test failure on this cap
/// means EITHER a soft-lock regression OR a real retune that wants this
/// number bumped (flag, don't silently bump — see module doc).
const SAFETY_CAP_SECS: f32 = 2.0;

/// Per-step `dt` for the drain loop. 1 ms is small enough that any drift in
/// the advance-then-retain loop bookkeeping surfaces before `SAFETY_CAP_SECS`;
/// the loop bound is `SAFETY_CAP_SECS / DT_SECS` ≈ 2000 iterations, cheap.
const DT_SECS: f32 = 0.001;

/// Build a minimal 5×4 `Board` matching the inline `empty_board` shape in
/// `src/vfx.rs::tests` (the harness those tests use). Cell count is the
/// default-grid `cell_count` (20) so the dim-aware path stays consistent;
/// `hazards` Vec parallels cells; `ordnance` / `threats` / `fire_events` empty.
fn empty_default_board() -> Board {
    let dims = broadside_engine::grid::Dims::default();
    let n = dims.cell_count();
    Board {
        size: n,
        cols: dims.cols,
        rows: dims.rows,
        cells: (0..n).map(|_| None).collect(),
        ordnance: Vec::new(),
        hazards: (0..n).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

/// A bare hull at `pos` / `hull` / `facing`. Mirrors the inline `ship`
/// helper in `src/vfx.rs::tests` — naked shields, no mounts, no statuses.
/// `cell` is set to `pos.to_index_in(dims)` for invariant A.
fn naked_ship(id: &str, faction: Faction, pos: Pos, hull: i32, facing: Facing) -> Ship {
    let dims = broadside_engine::grid::Dims::default();
    Ship {
        id: id.into(),
        faction,
        cell: pos.to_index_in(dims),
        pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Fore },
        facing,
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
            id: "m0".into(),
            arc: Arc::Forward,
            weapon: String::new(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// Place `ship` on `board` at `pos.to_index_in(dims)`. Mirrors how
/// `build_encounter_board_with_dims` seats ships — invariant A holds for
/// `Board::ship_at(pos)`.
fn place(board: &mut Board, pos: Pos, ship: Ship) {
    let idx = pos.to_index_in(board.dims());
    board.cells[idx] = Some(ship);
}

/// Drain a `CombatVfx` by `advance(DT_SECS)` until `!is_active()`, capped at
/// `SAFETY_CAP_SECS`. Returns the elapsed seconds. Panics if the cap is
/// reached — the panic message names the soft-lock contract and the count
/// of live effects (so a flake on a future `life_secs` retune surfaces
/// exactly which family overran).
fn drain_to_inactive(vfx: &mut CombatVfx, label: &str) -> f32 {
    let max_steps = (SAFETY_CAP_SECS / DT_SECS) as u32;
    for step in 0..max_steps {
        if !vfx.advance(DT_SECS) {
            assert!(
                !vfx.is_active(),
                "{label}: advance returned false but is_active() still true \
                 at step {step} (~{:.3}s) — internal inconsistency",
                step as f32 * DT_SECS,
            );
            return step as f32 * DT_SECS;
        }
    }
    panic!(
        "{label}: CombatVfx never drained inside SAFETY_CAP_SECS={SAFETY_CAP_SECS}s. \
         is_active() still true → IMPACT-lock would soft-lock the turn here. \
         Either a real regression (effect retain loop not dropping expired \
         effects) OR Bruce retuned an effect past {SAFETY_CAP_SECS}s — flag \
         the cap, don't silently bump."
    );
}

/* =========================================================================
 * Single-family terminations
 * ====================================================================== */

#[test]
fn hit_flash_drains_within_safety_cap() {
    // Hull drop on an enemy → one HitFlash. Smallest spawnable effect,
    // drains in ~life_secs (0.30 s default).
    let mut vfx = CombatVfx::new();
    let mut board = empty_default_board();
    place(
        &mut board,
        Pos::new(0, 3),
        naked_ship(
            "player",
            Faction::Player,
            Pos::new(0, 3),
            5,
            Facing::Bow(Dir4::N),
        ),
    );
    place(
        &mut board,
        Pos::new(0, 0),
        naked_ship(
            "enemy",
            Faction::Enemy,
            Pos::new(0, 0),
            5,
            Facing::Bow(Dir4::S),
        ),
    );
    vfx.observe(&board); // baseline
    let enemy_idx = Pos::new(0, 0).to_index_in(board.dims());
    board.cells[enemy_idx].as_mut().unwrap().hull = 3;
    vfx.observe(&board);
    assert!(
        vfx.is_active(),
        "hull drop should spawn at least one effect"
    );
    let elapsed = drain_to_inactive(&mut vfx, "HitFlash drain");
    // Sanity: never returns negative / NaN, and stays within the cap.
    assert!(
        (0.0..=SAFETY_CAP_SECS).contains(&elapsed),
        "HitFlash drain time {elapsed} out of bounds [0, {SAFETY_CAP_SECS}]",
    );
}

#[test]
fn explosion_plus_reflections_drain_within_safety_cap() {
    // The longest single-family spawn: destruction of one enemy with
    // SURVIVING witnesses → one Explosion (0.55 s) + one
    // ExplosionReflection per survivor (delay_per_cell × chebyshev + life_secs).
    // Maximal chebyshev distance on the 5×4 grid is 4, so max
    // reflection dur = 4×0.08 + 0.45 = 0.77 s — the analytic upper bound
    // the SAFETY_CAP_SECS envelope is sized for.
    let mut vfx = CombatVfx::new();
    let mut board = empty_default_board();
    // Player at front-centre, doomed enemy at the OPPOSITE corner so
    // chebyshev(blast, player) = max possible on the default grid.
    let player_pos = Pos::new(2, 3);
    let doomed_pos = Pos::new(0, 0);
    place(
        &mut board,
        player_pos,
        naked_ship(
            "player",
            Faction::Player,
            player_pos,
            5,
            Facing::Bow(Dir4::N),
        ),
    );
    place(
        &mut board,
        doomed_pos,
        naked_ship(
            "doomed",
            Faction::Enemy,
            doomed_pos,
            5,
            Facing::Bow(Dir4::S),
        ),
    );
    // Also seat a witness at the other far corner so two reflections
    // spawn (one at the player, one at the far-corner witness). Both share
    // the maximal-chebyshev pathway.
    let witness_pos = Pos::new(4, 3);
    place(
        &mut board,
        witness_pos,
        naked_ship(
            "witness",
            Faction::Enemy,
            witness_pos,
            5,
            Facing::Bow(Dir4::S),
        ),
    );
    vfx.observe(&board); // baseline
                         // Doomed vanishes this frame.
    let doomed_idx = doomed_pos.to_index_in(board.dims());
    board.cells[doomed_idx] = None;
    vfx.observe(&board);
    assert!(
        vfx.is_active(),
        "destruction should spawn at least one effect"
    );
    let elapsed = drain_to_inactive(&mut vfx, "Explosion + reflections drain");
    assert!(
        (0.0..=SAFETY_CAP_SECS).contains(&elapsed),
        "Explosion+reflections drain time {elapsed} out of bounds [0, {SAFETY_CAP_SECS}]",
    );
    // Tighter lower bound: the explosion itself takes ~0.55 s + reflection
    // start_delay 4×0.08=0.32 + 0.45 life = 0.77 s; the drain MUST be at
    // least the longest individual effect's lifetime (drain isn't zero).
    let explosion_secs = Explosion::default().life_secs;
    let refl = ExplosionReflection::default();
    let max_chebyshev = 4.0; // (0,0) -> (4,3)
    let analytic_min = explosion_secs.max(max_chebyshev * refl.delay_per_cell + refl.life_secs);
    assert!(
        elapsed >= analytic_min * 0.9, // 10% slack for dt-step quantisation
        "drain returned implausibly early: {elapsed}s < ~{analytic_min}s (analytic min); \
         the retain loop may have been dropping effects before they expired",
    );
}

#[test]
fn shot_beam_drains_within_safety_cap() {
    // A FireEvent latched as a ShotBeam — the slowest archetype (Ordnance,
    // 0.40 s) is the bound here.
    let mut vfx = CombatVfx::new();
    let mut board = empty_default_board();
    let attacker_pos = Pos::new(0, 0);
    let target_pos = Pos::new(0, 3);
    place(
        &mut board,
        attacker_pos,
        naked_ship(
            "enemy",
            Faction::Enemy,
            attacker_pos,
            5,
            Facing::Bow(Dir4::S),
        ),
    );
    place(
        &mut board,
        target_pos,
        naked_ship(
            "player",
            Faction::Player,
            target_pos,
            5,
            Facing::Bow(Dir4::N),
        ),
    );
    vfx.observe(&board); // baseline (no fire_events yet)
    board.fire_events = vec![FireEvent {
        from_cell: attacker_pos.to_index_in(board.dims()),
        to_cell: target_pos.to_index_in(board.dims()),
        from_pos: attacker_pos,
        to_pos: target_pos,
        archetype: WeaponArchetype::Ordnance,
        attacker_faction: Faction::Enemy,
        hit: true,
    }];
    vfx.observe(&board);
    assert!(vfx.is_active(), "FireEvent should spawn a ShotBeam");
    let elapsed = drain_to_inactive(&mut vfx, "ShotBeam drain");
    assert!(
        (0.0..=SAFETY_CAP_SECS).contains(&elapsed),
        "ShotBeam drain time {elapsed} out of bounds [0, {SAFETY_CAP_SECS}]",
    );
}

/* =========================================================================
 * Worst case — every effect family spawned simultaneously
 * ====================================================================== */

#[test]
fn worst_case_simultaneous_spawn_drains_within_safety_cap() {
    // Hit-flash on a survivor + Explosion + reflections + TelegraphFire pop +
    // a ShotBeam ALL in one observe() — the IMPACT-lock has to drain every
    // family in finite time on the same frame. This is the "everything
    // happens at once" stress case the soft-lock guard ultimately protects.
    // (Ordnance/Trail covered separately by the inline vfx tests in
    // `src/vfx.rs::tests`; the bound here is set by Explosion + reflections,
    // not by Trail's 0.35 s — adding a Projectile here would just bloat the
    // fixture without raising the drain ceiling.)
    let mut vfx = CombatVfx::new();
    let mut board = empty_default_board();
    let player_pos = Pos::new(2, 3);
    let doomed_pos = Pos::new(0, 0);
    let wounded_pos = Pos::new(4, 0);
    let mut wounded = naked_ship(
        "wounded",
        Faction::Enemy,
        wounded_pos,
        5,
        Facing::Bow(Dir4::S),
    );
    wounded.queue.push("intent_a".into()); // teed-up telegraph
    place(
        &mut board,
        player_pos,
        naked_ship(
            "player",
            Faction::Player,
            player_pos,
            5,
            Facing::Bow(Dir4::N),
        ),
    );
    place(
        &mut board,
        doomed_pos,
        naked_ship(
            "doomed",
            Faction::Enemy,
            doomed_pos,
            5,
            Facing::Bow(Dir4::S),
        ),
    );
    place(&mut board, wounded_pos, wounded);
    vfx.observe(&board); // baseline

    // One observe() that triggers four families at once:
    //   - doomed → None: Explosion + reflections (toward player + wounded)
    //   - wounded hull drop: HitFlash
    //   - wounded queue head rolls "intent_a" → "intent_b": TelegraphFire pop
    //   - new FireEvent: ShotBeam
    let dims = board.dims();
    let doomed_idx = doomed_pos.to_index_in(dims);
    let wounded_idx = wounded_pos.to_index_in(dims);
    board.cells[doomed_idx] = None;
    {
        let w = board.cells[wounded_idx].as_mut().unwrap();
        w.hull = 3;
        w.queue = vec!["intent_b".into()];
    }
    board.fire_events = vec![FireEvent {
        from_cell: wounded_idx,
        to_cell: player_pos.to_index_in(dims),
        from_pos: wounded_pos,
        to_pos: player_pos,
        archetype: WeaponArchetype::Ordnance,
        attacker_faction: Faction::Enemy,
        hit: true,
    }];
    vfx.observe(&board);
    assert!(
        vfx.is_active(),
        "every-family worst-case observe should spawn something"
    );
    let elapsed = drain_to_inactive(&mut vfx, "worst-case simultaneous drain");
    assert!(
        (0.0..=SAFETY_CAP_SECS).contains(&elapsed),
        "worst-case drain time {elapsed} out of bounds [0, {SAFETY_CAP_SECS}]",
    );
}

/* =========================================================================
 * Sanity ribbon: empty pool already inactive
 * ====================================================================== */

#[test]
fn empty_pool_is_inactive_and_drains_immediately() {
    // The other direction of the contract: a CombatVfx with NO observe()
    // (or with only baseline observes) starts inactive, and advance() on
    // an empty pool keeps it inactive. The unlock gate sees `!is_active()`
    // immediately and the turn advances — the OPPOSITE soft-lock would be
    // an empty pool falsely reporting active.
    let mut vfx = CombatVfx::new();
    assert!(!vfx.is_active(), "fresh CombatVfx is not active");
    let still_active = vfx.advance(DT_SECS);
    assert!(!still_active, "advance() on empty pool returns false");
    assert!(!vfx.is_active(), "still inactive after advance");
    // Baseline observe (no diff) leaves the pool empty.
    let board = empty_default_board();
    vfx.observe(&board);
    assert!(
        !vfx.is_active(),
        "first observe (no diff yet) does not spawn"
    );
}

/* =========================================================================
 * Pin the analytic bounds — flag a retune that crosses SAFETY_CAP_SECS
 * ====================================================================== */

#[test]
fn safety_cap_envelopes_every_known_life_secs() {
    // Documents the bounds source in code: every effect family's default
    // life_secs (the "longest" each family can run on a fresh spawn) is
    // strictly below SAFETY_CAP_SECS. Locks the design contract — a future
    // retune that pushes one family past 2 s would break this assert
    // BEFORE breaking the drain tests, surfacing the choice to bump the
    // cap as an explicit decision rather than a silent flake.
    let hit = HitFlash::default().life_secs;
    let expl = Explosion::default().life_secs;
    let trail = Trail::default().life_secs;
    let tel = TelegraphFire::default().life_secs;
    let refl = ExplosionReflection::default();
    // ShotBeam is per-archetype; pick the longest in the default table
    // (Ordnance = 0.40 s per archetype_beam_style at the time of writing).
    // Read it from the ShotBeam::default's per_archetype list to keep the
    // assert in sync with the data.
    let max_beam = ShotBeam::default()
        .per_archetype
        .iter()
        .map(|b| b.life_secs)
        .fold(0.0_f32, f32::max);
    // Reflection's worst case on a default-Dims board: chebyshev 4 cells.
    let max_reflection = 4.0 * refl.delay_per_cell + refl.life_secs;
    let envelope: [(f32, &str); 6] = [
        (hit, "HitFlash"),
        (expl, "Explosion"),
        (trail, "Trail"),
        (tel, "TelegraphFire"),
        (max_beam, "ShotBeam (max archetype)"),
        (max_reflection, "ExplosionReflection (max chebyshev)"),
    ];
    for (secs, name) in envelope {
        assert!(
            secs < SAFETY_CAP_SECS,
            "{name} life_secs={secs} >= SAFETY_CAP_SECS={SAFETY_CAP_SECS}: a retune \
             crossed the soft-lock-guard envelope. Either lower the family or \
             flag the cap for a bump (do NOT silently raise — the cap is the \
             contract that the turn unlocks within a fixed window).",
        );
    }
}
