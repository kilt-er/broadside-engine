//! Multi-cell boss (#214) RENDER-side invariants — locks the contract that
//! the loft render path
//!   (a) DEDUPES the tail-mirror slot so a 1×2 Pair boss emits ONE hull
//!       quad rather than two (the boss's `Ship` clone sits in both
//!       primary and tail slots; a naive iteration would paint twice and
//!       Z-fight),
//!   (b) seats that single hull at the MIDPOINT of primary + tail cells,
//!   (c) scales it 2× via `hull_scale_mul` so the rendered hull spans the
//!       entire 1×2 footprint.
//!
//! These tests are RENDER-side — they validate the dedup filter + midpoint
//! math at the data level (no GPU required). The actual draw-call emission
//! happens only when a `SpriteRegistry::loft_kind(...)` returns `Some(...)`,
//! which requires an uploaded mesh and the unified camera enabled — a GPU
//! environment we can't conjure in `cargo test`. Instead we exercise the
//! pure helpers the render path uses:
//!   - the `dims`-aware tail-mirror skip in `compose_scene_2d_tweened`
//!     (replicated here as the same expression used in `hud.rs`),
//!   - the midpoint `cell_frac` calculation,
//!   - the `Ship::footprint()` + `Ship::tail` shape (architect's contract).
//!
//! Each test seats a fresh Pair boss via `place_capital_pair` (the
//! authoritative content-half boss-placer) on a 5×4 board and inspects
//! the resulting `Board.cells` invariants.

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::grid::{Dims, Dir4, Facing, Pos};
use broadside_engine::runs::place_capital_pair;
use broadside_engine::types::{Arc, Board, EventBus, Faction, LaneEnd, Mount, Orientation, Ship};
use std::collections::HashMap;

/* =========================================================================
 * Fixtures
 * ====================================================================== */

/// A fresh 5×4 board with no ships (no hazards, no ordnance, no threats).
/// Mirrors what `compose_scene_2d_tweened` reads in production.
fn empty_5x4_board() -> Board {
    let dims = Dims::default(); // 5×4
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

/// A bare-bones boss `Ship` (no mounts; the render tests don't fire). The
/// boss's pos/cell/tail get overwritten by `place_capital_pair`.
fn boss_template(facing: Facing, hull: i32) -> Ship {
    Ship {
        id: "boss".into(),
        faction: Faction::Enemy,
        cell: 0,
        pos: Pos::new(0, 0),
        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
        facing,
        hull,
        max_hull: hull,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: vec![Mount {
            id: "b-m1".into(),
            arc: Arc::Forward,
            weapon: "_noop".into(),
        }],
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

/// A bare-bones single-cell enemy `Ship` (no mounts; render-only).
fn single_template(id: &str, pos: Pos) -> Ship {
    Ship {
        id: id.into(),
        faction: Faction::Enemy,
        cell: pos.to_index(),
        pos,
        orientation: Orientation::BowOn { bow: LaneEnd::Aft },
        facing: Facing::Bow(Dir4::S),
        hull: 3,
        max_hull: 3,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: Vec::new(),
        queue: Vec::new(),
        cooldowns: HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
        tail: None,
    }
}

/// The dedup filter `compose_scene_2d_tweened` uses to skip tail-mirror
/// slots. Returns the ships that would emit a `LoftShipInstance`, in
/// linear-index order (same order as the production iterator).
fn dedup_ships(board: &Board) -> Vec<&Ship> {
    let dims = board.dims();
    board
        .cells
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let s = slot.as_ref()?;
            if s.tail.is_some() && i != s.pos.to_index_in(dims) {
                return None; // tail mirror — skip
            }
            Some(s)
        })
        .collect()
}

/// Midpoint `cell_frac` the render path computes for a Pair boss whose
/// primary is at `pos` and tail at `ship.tail.unwrap()`. Single-cell ships
/// (`tail == None`) pass through with `cell_frac == [pos.col, pos.row]`.
fn boss_cell_frac(ship: &Ship) -> ([f32; 2], f32) {
    let base = [ship.pos.col as f32, ship.pos.row as f32];
    if let Some(tail) = ship.tail {
        let mid_col = (base[0] + tail.col as f32) * 0.5;
        let mid_row = (base[1] + tail.row as f32) * 0.5;
        ([mid_col, mid_row], 2.0_f32)
    } else {
        (base, 1.0_f32)
    }
}

/* =========================================================================
 * (a) Tail-mirror dedup
 * ====================================================================== */

#[test]
fn tail_mirror_slot_is_dropped_so_boss_emits_one_hull_quad() {
    let mut board = empty_5x4_board();
    let primary = Pos::new(2, 1);
    let boss = boss_template(Facing::Bow(Dir4::S), 10);

    // place_capital_pair seats the boss at `primary` and clones it into
    // the bow-direction tail (S → row+1 → (2, 2)).
    let placed = place_capital_pair(&mut board, boss, primary);
    assert!(
        placed,
        "boss must place as 1×2 on an empty 5×4 board with row below free"
    );

    // Both slots are occupied by the SAME ship id (the mirror clone).
    let dims = board.dims();
    let primary_idx = primary.to_index_in(dims);
    let tail_pos = Pos::new(2, 2);
    let tail_idx = tail_pos.to_index_in(dims);
    assert_eq!(
        board.cells[primary_idx].as_ref().map(|s| s.id.as_str()),
        Some("boss"),
        "primary slot holds the boss"
    );
    assert_eq!(
        board.cells[tail_idx].as_ref().map(|s| s.id.as_str()),
        Some("boss"),
        "tail slot holds the boss mirror clone"
    );

    // The render dedup filter drops the tail-mirror slot → exactly ONE
    // ship to draw for the boss. Verify by id-count.
    let drawn = dedup_ships(&board);
    let boss_emits = drawn.iter().filter(|s| s.id == "boss").count();
    assert_eq!(
        boss_emits, 1,
        "boss must emit exactly ONE LoftShip quad — the tail-mirror skip \
         in compose_scene_2d_tweened drops the duplicate"
    );
}

/* =========================================================================
 * (b) Midpoint cell_frac
 * ====================================================================== */

#[test]
fn boss_renders_at_midpoint_of_primary_and_tail_cells() {
    let mut board = empty_5x4_board();
    let primary = Pos::new(2, 1);
    let boss = boss_template(Facing::Bow(Dir4::S), 10);
    assert!(place_capital_pair(&mut board, boss, primary));

    // Pull the primary-slot ship (the surviving entry after dedup).
    let drawn = dedup_ships(&board);
    let boss_ship = drawn
        .iter()
        .find(|s| s.id == "boss")
        .expect("boss survives the dedup filter");
    let (cell_frac, scale_mul) = boss_cell_frac(boss_ship);

    // Primary (2, 1) + tail (2, 2) → midpoint (2.0, 1.5). The
    // boss-2cell render seats the hull on the seam between the two cells
    // so it visually spans both.
    assert_eq!(cell_frac, [2.0, 1.5], "cell_frac at the seam midpoint");
    assert!(
        (scale_mul - 2.0).abs() < f32::EPSILON,
        "Pair boss scales 2× so the hull spans both cells; got {scale_mul}"
    );
}

/* =========================================================================
 * (c) Single-cell ships are byte-identical to pre-#214
 * ====================================================================== */

#[test]
fn single_cell_ships_render_unchanged_and_at_their_own_cell() {
    let mut board = empty_5x4_board();
    let dims = board.dims();
    // Seat two single-cell enemies in their own slots — no boss this time.
    let a_pos = Pos::new(0, 0);
    let b_pos = Pos::new(4, 3);
    board.cells[a_pos.to_index_in(dims)] = Some(single_template("enemy-a", a_pos));
    board.cells[b_pos.to_index_in(dims)] = Some(single_template("enemy-b", b_pos));

    let drawn = dedup_ships(&board);
    assert_eq!(
        drawn.len(),
        2,
        "two singletons → two drawn ships (no dedup applies)"
    );

    for s in &drawn {
        let (cell_frac, scale_mul) = boss_cell_frac(s);
        assert_eq!(
            cell_frac,
            [s.pos.col as f32, s.pos.row as f32],
            "single-cell ship's cell_frac == its own cell — byte-identical \
             to pre-#214 render for {}",
            s.id
        );
        assert!(
            (scale_mul - 1.0).abs() < f32::EPSILON,
            "single-cell ship scales 1× — byte-identical to pre-#214; got \
             {scale_mul} for {}",
            s.id
        );
    }
}

/* =========================================================================
 * (d) Architect contract: Ship::footprint reports both cells
 * ====================================================================== */

#[test]
fn boss_footprint_is_primary_then_tail_in_that_order() {
    let mut board = empty_5x4_board();
    let primary = Pos::new(1, 1);
    let boss = boss_template(Facing::Bow(Dir4::S), 10);
    assert!(place_capital_pair(&mut board, boss, primary));

    // find_pos_by_id returns the primary, then footprint enumerates both.
    let primary_pos = board.find_pos_by_id("boss").expect("boss is on the board");
    assert_eq!(primary_pos, primary, "find_pos_by_id returns the primary");

    let primary_ship = board
        .ship_at(primary_pos)
        .expect("primary slot holds the boss");
    let fp = primary_ship.footprint();
    assert_eq!(
        fp,
        vec![primary, Pos::new(1, 2)],
        "footprint is [primary, tail] with the tail forward of the bow"
    );
}

/* =========================================================================
 * (e) Dedup is keyed on (tail.is_some() && slot != primary): the PRIMARY
 *     slot always survives, even if the tail slot's index sorts earlier.
 * ====================================================================== */

#[test]
fn primary_slot_always_survives_even_when_tail_index_is_smaller() {
    // Seat a boss whose tail's linear index is SMALLER than its primary's:
    // pick `Bow(N)` so the bow-forward tail is row-1 (smaller row → smaller
    // linear index than the primary).
    let mut board = empty_5x4_board();
    let primary = Pos::new(2, 2);
    let mut boss = boss_template(Facing::Bow(Dir4::N), 10);
    boss.orientation = Orientation::BowOn { bow: LaneEnd::Fore };
    let placed = place_capital_pair(&mut board, boss, primary);
    assert!(placed, "Bow(N) tail at (2, 1) — that cell is empty");

    let dims = board.dims();
    let primary_idx = primary.to_index_in(dims);
    let tail_pos = Pos::new(2, 1);
    let tail_idx = tail_pos.to_index_in(dims);
    assert!(
        tail_idx < primary_idx,
        "test sanity: tail-cell linear index ({tail_idx}) precedes \
         primary's ({primary_idx})"
    );

    // The PRIMARY survives the dedup; the tail mirror is dropped — even
    // though the iteration hits the tail slot first.
    let drawn = dedup_ships(&board);
    let boss_entries: Vec<_> = drawn.iter().filter(|s| s.id == "boss").collect();
    assert_eq!(
        boss_entries.len(),
        1,
        "exactly one boss survives the dedup filter"
    );
    // The surviving entry must have `pos == primary` (the primary slot).
    // The slot==pos invariant the architect documented in
    // `place_capital_pair`: the surviving entry's pos field is the
    // PRIMARY's pos in BOTH slots, but only the primary slot's index
    // equals `pos.to_index_in(dims)`.
    let surviving = boss_entries[0];
    assert_eq!(
        surviving.pos, primary,
        "the surviving slot is the primary, not the tail mirror"
    );
}
