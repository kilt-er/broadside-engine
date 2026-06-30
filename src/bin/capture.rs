//! Headless offscreen capture — renders ONE frame of the real combat board to a
//! PNG, with no window or display. The permanent "give the team eyes" tool: an
//! AI agent (or anyone) can Read the PNG to SEE exactly what the renderer
//! produces, instead of needing a live window (which a headless/locked machine
//! can't provide).
//!
//! Run: `cargo run --bin capture --features render,runtime -- bugs/current_state.png`
//! With NO path arg it defaults to `<crate>/../bugs/current_state.png` (the
//! Broadside-root `bugs/` dir), resolved from `CARGO_MANIFEST_DIR` so it works
//! from ANY cwd — `cargo run --bin capture` argless lands the PNG in the right
//! place whether launched from the workspace root or the engine crate. A passed
//! path (relative or absolute) is used verbatim. The parent dir is created if
//! missing, so capture never fails with a "path not found" os-error.
//!
//! Renders through the SAME path the game uses — [`Gfx::new_headless`] builds the
//! identical sprite/polygon/loft pipelines + parallax background, the player gets
//! the real Aegis loft hull + enemies the CAD hull, and
//! [`hud::compose_scene_2d_with`] composes the scene — so the capture is a
//! faithful image of the live frame, then [`Gfx::capture_png`] reads the offscreen
//! back to disk (stripping the wgpu 256-byte row padding).
//!
//! The board matches the campaign spawn layout (player front-centre Bow(N),
//! enemies fanned centre-out across the back row Bow(S)) so the captured frame
//! shows the real bow-to-bow start — the orientation the team needs to see.

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::Gfx;
use broadside_engine::grid::{Dir4, Facing, Pos, COLS};
use broadside_engine::hud::compose_scene_2d_with;
use broadside_engine::projector::ProjectorConfig;
use broadside_engine::runs::{enemy_spawn_facing, player_spawn_facing, player_start_pos};
use broadside_engine::types::{Board, EventBus, Faction, LaneEnd, Mount, Orientation, Ship};

/// Build a `types::Ship` at `pos`/`facing` (mirrors the bin's `make_ship`: 2-D
/// pos/facing drive the render; legacy 1-D cell/orientation kept consistent).
fn make_ship(id: &str, faction: Faction, pos: Pos, facing: Facing) -> Ship {
    let orientation = match facing {
        Facing::Bow(Dir4::N) => Orientation::BowOn { bow: LaneEnd::Fore },
        Facing::Bow(Dir4::S) => Orientation::BowOn { bow: LaneEnd::Aft },
        _ => Orientation::Broadside,
    };
    Ship {
        id: id.to_string(),
        faction,
        cell: pos.to_index(),
        pos,
        orientation,
        facing,
        hull: 5,
        max_hull: 5,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: vec![Mount {
            id: "m1".into(),
            arc: broadside_engine::types::Arc::Forward,
            weapon: "pulse_laser".into(),
        }],
        queue: Vec::new(),
        cooldowns: std::collections::HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

/// The campaign-layout board: player front row Bow(N) at `player_col`, enemies
/// centre-out across the back row Bow(S) — the real bow-to-bow start. `player_col`
/// lets the capture place the player OFF-CENTER (e.g. col 0/4) to expose
/// lane-dependent pose bugs that a centred (col 2, zero lane-yaw) shot masks.
/// (#213 item 4 proof) Build a capture board at the canonical 5x4 dims.
fn capture_board(player_col: usize, player_row: usize, player_facing: Facing) -> Board {
    capture_board_with_dims(
        broadside_engine::grid::Dims::new(COLS, broadside_engine::grid::ROWS),
        player_col,
        player_row,
        player_facing,
    )
}

/// (#213 item 4 proof) Build a capture board at ARBITRARY [`Dims`] so the
/// capture path can prove variable-board rendering. Player + back-row enemies
/// are clamped + spread to fit the requested shape so a 3x3 / 4x2 / 2x4 / etc.
/// captures render without out-of-bounds spawns. Mirrors the live bin's
/// `build_encounter_board_with_dims` behavior at a smaller scale.
fn capture_board_with_dims(
    dims: broadside_engine::grid::Dims,
    player_col: usize,
    player_row: usize,
    player_facing: Facing,
) -> Board {
    let cell_count = dims.cols * dims.rows;
    let mut cells: Vec<Option<Ship>> = (0..cell_count).map(|_| None).collect();
    let place = |cells: &mut Vec<Option<Ship>>, s: Ship, dims: broadside_engine::grid::Dims| {
        let idx = s.pos.row * dims.cols + s.pos.col;
        if idx < cells.len() {
            cells[idx] = Some(s);
        }
    };
    let ppos = Pos::new(
        player_col.min(dims.cols.saturating_sub(1)),
        player_row.min(dims.rows.saturating_sub(1)),
    );
    // (#70) Player facing is an ARG so the capture reproduces a MOVED/REORIENTED
    // live player (the old frozen spawn-facing masked the chase-cam pose bug).
    // Post-decouple the loft hull no longer rotates with facing — so a capture
    // at the same column should look identical across facings (that's the fix);
    // the arg lets us PROVE it + match any live case.
    let mut player = make_ship("player", Faction::Player, ppos, player_facing);
    // (#66) class "aegis" so the baked aegis_* sprites are selected (the renderer
    // keys sprite_path on klass); the bin sets the player class likewise.
    player.klass = Some("aegis".to_string());
    player.shield_profile.bow.charge = 2;
    player.shield_profile.port.charge = 1;
    // (#191 Bruce) Match the live campaign boot player's 3 mounts so the bottom
    // HUD card strip renders all 3 weapon slots (1/2/3), not just 1. The live
    // bin builds mounts m1=pulse_laser + m2=torpedo + m3=broadside_battery in
    // bin/broadside.rs:706. Without this the capture under-draws the menu (one
    // tile vs the live six) and the menu-over-ship clip stays invisible to the
    // capture harness — the bug Bruce saw in 192545.
    player.mounts = vec![
        broadside_engine::types::Mount {
            id: "m1".into(),
            arc: broadside_engine::types::Arc::Forward,
            weapon: "pulse_laser".into(),
        },
        broadside_engine::types::Mount {
            id: "m2".into(),
            arc: broadside_engine::types::Arc::Forward,
            weapon: "torpedo".into(),
        },
        broadside_engine::types::Mount {
            id: "m3".into(),
            arc: broadside_engine::types::Arc::BroadsideArc,
            weapon: "broadside_battery".into(),
        },
    ];
    place(&mut cells, player, dims);
    // (#213 item 4) Lay enemies along the BACK row (row 0) at clamped columns
    // so a narrow board (cols<3) doesn't spawn out-of-bounds. Centre + two
    // flanking lanes, all clamped into [0..cols).
    let mid = dims.cols / 2;
    let lhs = mid.saturating_sub(1);
    let rhs = (mid + 1).min(dims.cols.saturating_sub(1));
    place(
        &mut cells,
        make_ship(
            "enemy-2",
            Faction::Enemy,
            Pos::new(mid, 0),
            enemy_spawn_facing(),
        ),
        dims,
    );
    if lhs != mid {
        place(
            &mut cells,
            make_ship(
                "enemy-3",
                Faction::Enemy,
                Pos::new(lhs, 0),
                enemy_spawn_facing(),
            ),
            dims,
        );
    }
    if rhs != mid {
        place(
            &mut cells,
            make_ship(
                "enemy-5",
                Faction::Enemy,
                Pos::new(rhs, 0),
                enemy_spawn_facing(),
            ),
            dims,
        );
    }

    Board {
        size: dims.cols,
        cols: dims.cols,
        rows: dims.rows,
        cells,
        ordnance: Vec::new(),
        hazards: (0..cell_count).map(|_| Vec::new()).collect(),
        patrol: 1,
        level: 0,
        threats: Vec::new(),
        bus: EventBus::default(),
        destroys_this_window: 0,
        fire_events: Vec::new(),
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // A passed path (relative or absolute) is used verbatim; with no arg, default
    // to the Broadside-root `bugs/` dir resolved from the crate manifest so the
    // capture lands in the right place from ANY cwd (the bin's cwd is the engine
    // crate, which has no `bugs/` subdir — that was the os-error-3 on the argless
    // run). `CARGO_MANIFEST_DIR` is `<…>/Broadside/engine`; `../bugs` is the dir
    // every agent + the lead Reads.
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../bugs/current_state.png")
            .to_string_lossy()
            .into_owned()
    });

    let mut gfx = pollster::block_on(Gfx::new_headless());

    // Load the baked ship sprites the playable bin loads (fallback path).
    let loaded = gfx.try_load_ship_sprites(std::path::Path::new("assets"));
    log::info!("capture: loaded {loaded} ship sprite(s)");

    // (#149/#187) Install the player's GLB exactly as the playable bin does
    // (broadside.rs install_player_glb) so the capture faithfully shows the LIVE-3D
    // player ship. The player hull is broadside-ship_03.glb (Bruce's new hero);
    // flip_prow=false (Bruce confirmed the un-flipped pose is bow-forward), matching the
    // live bin — the tip-width heuristic had mis-flagged it.
    const PLAYER_GLB: &[u8] = include_bytes!("../../assets/ships/broadside-ship_03.glb");
    match gfx.install_player_glb(PLAYER_GLB, false) {
        Ok(()) => {
            log::info!("capture: player hull installed from broadside-ship_03.glb (no flip)");
        }
        Err(e) => log::warn!(
            "capture: broadside-ship_03.glb import failed ({e}); player falls back to sprite/flat-box"
        ),
    }
    // (#163/#187) ENEMY FLEET = a MIX of broadside-ship_02.glb (EnemyLoft) + the old
    // player hull broadside-ship_01.glb (EnemyLoftB), both enemy-tinted, matching the
    // live bin so the capture shows both enemy classes (loft_kind picks per-id).
    const ENEMY_GLB: &[u8] = include_bytes!("../../assets/ships/broadside-ship_02.glb");
    match gfx.install_enemy_glb(ENEMY_GLB) {
        Ok(()) => log::info!("capture: enemy hull A installed from broadside-ship_02.glb"),
        Err(e) => {
            log::warn!(
                "capture: broadside-ship_02.glb import failed ({e}); enemies fall back to CAD/2D"
            );
        }
    }
    const ENEMY_GLB_B: &[u8] = include_bytes!("../../assets/ships/broadside-ship_01.glb");
    match gfx.install_enemy_glb_b(ENEMY_GLB_B, false) {
        Ok(()) => log::info!("capture: enemy hull B installed from broadside-ship_01.glb"),
        Err(e) => {
            log::warn!(
                "capture: broadside-ship_01.glb import failed ({e}); enemy fleet uses the single hull"
            );
        }
    }

    // (#76) Optional BROADSIDE_SHIP_RES=N env cycles the SHIP loft res forward N
    // steps before the capture, so a headless shot can verify the live ship-res
    // change (160x100 -> 220x138 -> 320x200 -> 480x300) renders correctly. Default
    // 0 = the baseline res. Mirrors the live `,`/`.` control via
    // `Gfx::cycle_loft_res`.
    if let Ok(n) = std::env::var("BROADSIDE_SHIP_RES") {
        if let Ok(steps) = n.parse::<u32>() {
            for _ in 0..steps {
                let (w, h) = gfx.cycle_loft_res(true);
                log::info!("capture: cycled ship res -> {w}x{h}");
            }
        }
    }

    // (#76) Optional BROADSIDE_SCENE_RES=N env cycles the WHOLE-SCENE (offscreen)
    // res forward N steps before the capture (320x180 -> 480x270 -> 640x360),
    // mirroring the live `;`/`'` control via `Gfx::cycle_scene_res`. Default 0 =
    // the 480x270 baseline, which the pixel-identity gate diffs against the
    // pre-scene-res reference PNG. The projector `cfg` below is built AFTER this so
    // it reprojects to whatever scene size we land on.
    if let Ok(n) = std::env::var("BROADSIDE_SCENE_RES") {
        if let Ok(steps) = n.parse::<u32>() {
            for _ in 0..steps {
                let (w, h) = gfx.cycle_scene_res(true);
                log::info!("capture: cycled scene res -> {w}x{h}");
            }
        }
    }

    // (#139) Optional BROADSIDE_GRID_PITCH=N steps the grid pitch toward top-down
    // before the capture (mirrors the live `G` key), so the headless shots show the
    // constant-depth re-pitch at steps 0 / mid / near-top-down. The capture's `cfg`
    // below applies grid_pitch_t() so the grid/cells/ordnance reproject.
    if let Ok(n) = std::env::var("BROADSIDE_GRID_PITCH") {
        if let Ok(steps) = n.parse::<u32>() {
            for _ in 0..steps {
                let s = broadside_engine::gfx::cycle_grid_pitch();
                log::info!("capture: grid pitch -> step {s}");
            }
        }
    }
    // (#190) Optional BROADSIDE_SHIP_SCALE=F sets the live UNIFIED_SHIP_SCALE to
    // F (float, clamped into [UNIFIED_SHIP_SCALE_MIN, UNIFIED_SHIP_SCALE_MAX]) so
    // the headless shot verifies any value the live `[` / `]` keys can dial.
    // Implemented as a single adjust_ship_scale(F - current) so the clamp logic
    // and the atomic update path are the same as the live key handler.
    if let Ok(v) = std::env::var("BROADSIDE_SHIP_SCALE") {
        if let Ok(target) = v.parse::<f32>() {
            let cur = broadside_engine::gfx::unified_ship_scale();
            let s = broadside_engine::gfx::adjust_ship_scale(target - cur);
            log::info!("capture: unified ship scale -> {s:.2}");
        }
    }
    // (#192) Optional BROADSIDE_CAM_DIST=F sets the live UNIFIED_CAM_DIST to F
    // (clamped into [UNIFIED_CAM_DIST_MIN, UNIFIED_CAM_DIST_MAX]) so the headless
    // shot verifies any value the `-` / `=` keys can dial. Mirrors the
    // BROADSIDE_SHIP_SCALE pattern — single adjust_cam_dist(F - current) routes
    // through the same clamp + atomic store the live key handler uses.
    if let Ok(v) = std::env::var("BROADSIDE_CAM_DIST") {
        if let Ok(target) = v.parse::<f32>() {
            let cur = broadside_engine::gfx::unified_cam_dist();
            let d = broadside_engine::gfx::adjust_cam_dist(target - cur);
            log::info!("capture: unified cam dist -> {d:.2}");
        }
    }
    // (#195) Optional BROADSIDE_GRID_CELL_SCALE=F sets the live grid cell-size
    // multiplier to F (clamped into the gfx min/max). Mirrors the cam-dist /
    // ship-scale envs — single adjust_grid_cell_scale(F - current) routes
    // through the same clamp + atomic store as the live `K` / `L` keys.
    if let Ok(v) = std::env::var("BROADSIDE_GRID_CELL_SCALE") {
        if let Ok(target) = v.parse::<f32>() {
            let cur = broadside_engine::gfx::unified_grid_cell_scale();
            let s = broadside_engine::gfx::adjust_grid_cell_scale(target - cur);
            log::info!("capture: unified grid cell scale -> {s:.2}");
        }
    }
    // (#198) BROADSIDE_ANCHOR_CENTERED=1 sets the vertical anchor mode to
    // CENTERED (Mode B) so a headless capture can verify the centered pose;
    // unset / 0 keeps the default snap-to-menu (Mode A). Mirrors the M cycle.
    if std::env::var("BROADSIDE_ANCHOR_CENTERED").is_ok_and(|v| v != "0") {
        broadside_engine::gfx::set_anchor_mode_centered(true);
        log::info!("capture: anchor mode -> CTR (centered)");
    }
    // (#207) BROADSIDE_LATERAL_X=F sets the live lateral pan offset (world
    // units, signed) so headless captures verify the parallax-style edge-lane
    // shift. The live bin EASES the offset toward a board-derived target
    // every frame; here we jam it directly to the value Bruce wants to
    // observe (single-frame harness has no wall-clock ease).
    if let Ok(v) = std::env::var("BROADSIDE_LATERAL_X") {
        if let Ok(x) = v.parse::<f32>() {
            broadside_engine::gfx::set_unified_lateral_x_offset(x);
            log::info!("capture: lateral pan offset -> {x:.2} world units");
        }
    }
    // (#215 Bruce hittable-cells toggle) BROADSIDE_HITTABLE=0 forces the
    // hittable-cells overlay OFF for a clean-board capture (default is ON to
    // match the live bin). Useful for proving the toggle gates the overlay.
    if std::env::var("BROADSIDE_HITTABLE").is_ok_and(|v| v == "0") {
        // The atomic defaults to true; flip it off here.
        if broadside_engine::gfx::hittable_cells_enabled() {
            broadside_engine::gfx::toggle_hittable_cells();
        }
        log::info!("capture: hittable cells OFF (BROADSIDE_HITTABLE=0)");
    }
    // (#140/#142/#151) Optional grid-mode env (mirrors the `T` cycle), so the pitch-arc
    // shots show each mode. BROADSIDE_GRID_CONTINUOUS=1 -> continuous-straight (mode 3);
    // BROADSIDE_GRID_STRAIGHT=1 -> stretch-straight stepped (mode 2);
    // BROADSIDE_GRID_STRETCH=1 -> stretch-curved (mode 1); none -> drawbridge (mode 0).
    // Most-specific wins. Cycle the live GRID_MODE to the target.
    let target_mode: u32 = if std::env::var("BROADSIDE_GRID_CONTINUOUS").is_ok_and(|v| v != "0") {
        3
    } else if std::env::var("BROADSIDE_GRID_STRAIGHT").is_ok_and(|v| v != "0") {
        2
    } else {
        u32::from(std::env::var("BROADSIDE_GRID_STRETCH").is_ok_and(|v| v != "0"))
    };
    while broadside_engine::gfx::grid_mode() != target_mode {
        broadside_engine::gfx::cycle_grid_mode();
    }
    if target_mode != 0 {
        log::info!(
            "capture: grid mode -> {} ({})",
            target_mode,
            broadside_engine::gfx::grid_mode_tag()
        );
    }

    // Optional 2nd arg = player column (0..COLS-1) so the capture can place the
    // player OFF-CENTER to expose lane-dependent pose bugs (a centred shot at the
    // zero-lane-yaw col 2 masks a mirrored lane-aim sign). Defaults to the
    // campaign spawn column.
    let player_col = std::env::args()
        .nth(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| player_start_pos().col);
    // Optional 3rd arg = player FACING (n/s/e/w) so the capture reproduces a
    // REORIENTED live player (the frozen spawn-facing masked the chase-cam pose
    // bug). Default = the campaign spawn facing.
    let player_facing = match std::env::args().nth(3).as_deref() {
        Some("n" | "N") => Facing::Bow(Dir4::N),
        Some("s" | "S") => Facing::Bow(Dir4::S),
        Some("e" | "E") => Facing::Bow(Dir4::E),
        Some("w" | "W") => Facing::Bow(Dir4::W),
        _ => player_spawn_facing(),
    };
    // Optional 4th arg = player ROW (0..ROWS-1) so the capture can place the
    // player at a MOVED-FORWARD/BACK cell (row 0 = far/back, row 3 = front
    // spawn) — reproduces what a forward (N) move actually draws, which a
    // frozen-spawn-row capture can't show. Default = the campaign spawn row.
    let player_row = std::env::args()
        .nth(4)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| player_start_pos().row);
    // (#213 item 4 capture proof) BROADSIDE_DIMS=CxR env override builds the
    // capture board at variable [`Dims`] so a single capture run can prove
    // non-5x4 rendering. Format: BROADSIDE_DIMS=3x3 / 4x2 / 2x4 / etc. Falls
    // back to the canonical 5x4 when unset / malformed.
    let dims = std::env::var("BROADSIDE_DIMS")
        .ok()
        .and_then(|s| {
            let lower = s.to_ascii_lowercase();
            let mut it = lower.split('x');
            let c = it.next()?.parse::<usize>().ok()?;
            let r = it.next()?.parse::<usize>().ok()?;
            if c == 0 || r == 0 {
                return None;
            }
            Some(broadside_engine::grid::Dims::new(c, r))
        })
        .unwrap_or_else(|| broadside_engine::grid::Dims::new(COLS, broadside_engine::grid::ROWS));
    let mut board = capture_board_with_dims(dims, player_col, player_row, player_facing);
    // (#215) Publish the live dims to gfx's atomics so the GPU loft pass
    // (render_unified_fleet) projects ships at the same dims the HUD grid
    // uses. Mirrors the playable bin's scene_projector_for_board call.
    broadside_engine::gfx::set_live_grid_dims(dims.cols, dims.rows);
    log::info!(
        "capture: player at col {player_col} row {player_row}, facing {player_facing:?}, dims {}x{}",
        dims.cols,
        dims.rows,
    );

    // (#90 yellow-square repro) Optional BROADSIDE_QUEUE_DEMO=1 queues the player's
    // mount weapon (what pressing `1` does) so the queue-driven overlays render —
    // reproduces Bruce's "yellow square covers the ship when I queue an ability".
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        if let Some(p) = board
            .cells
            .iter_mut()
            .flatten()
            .find(|s| s.faction == Faction::Player)
        {
            let w = p
                .mounts
                .first()
                .map(|m| m.weapon.clone())
                .unwrap_or_default();
            // Queue it three times — match Bruce queuing several abilities.
            for _ in 0..3 {
                p.queue.push(w.clone());
            }
            log::info!(
                "capture: QUEUE demo — player queued '{w}' x{}",
                p.queue.len()
            );
        }
        // (#129) Give the enemies a REVEALED queue so the top-left ENEMY INFO panel
        // shows queue icons (the live path fills enemy.queue when the AI telegraphs;
        // here we inject it so the static capture shows the learned-queue state). Knock
        // one enemy's hull + shields down so the panel's hull/shield bars read PARTIAL.
        for (n, e) in board
            .cells
            .iter_mut()
            .flatten()
            .filter(|s| s.faction == Faction::Enemy)
            .enumerate()
        {
            let w = e
                .mounts
                .first()
                .map(|m| m.weapon.clone())
                .unwrap_or_default();
            if !w.is_empty() {
                // First enemy telegraphs two shots; second telegraphs one — varied queue.
                let count = if n == 0 { 2 } else { 1 };
                for _ in 0..count {
                    e.queue.push(w.clone());
                }
            }
            if n == 0 {
                e.hull = (e.max_hull / 2).max(1); // partial hull bar
                e.shield_profile.bow.charge = 0; // partial shield bar
            }
        }
        log::info!("capture: QUEUE demo — injected enemy revealed-queues for the info panel");
        // The LIVE queue runs the enemy world-phase (apply_intent → run_world_phase),
        // which paints board.threats (enemy intent). A capture's manual queue-push
        // does NOT, so simulate it: an enemy threat on the PLAYER's NEAR cell — the
        // largest cell quad on screen — to test whether the threat FILL is the
        // field-covering warm square Bruce sees (#78 was a red-slab version of this).
        use broadside_engine::types::{Threat, ThreatKind};
        let ppos = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map_or_else(|| Pos::new(2, 3), |s| s.pos);
        board.threats.push(Threat {
            pos: ppos,
            kind: ThreatKind::Damage { amount: 9 }, // ≥ hull ⇒ LETHAL fill (brightest)
            source: Pos::new(COLS / 2, 0),
        });
        log::info!("capture: QUEUE demo — injected a LETHAL threat on the player cell {ppos:?}");
        // (#132) Inject an in-flight PLAYER torpedo mid-lane so the capture shows
        // push_ordnance_2d drawing it travelling (the live bin spawns it on a torpedo
        // commit; here we place one mid-flight between the player and the enemies).
        use broadside_engine::grid::Dir8;
        use broadside_engine::types::{Effect, Projectile};
        let tpos = Pos::new(ppos.col, ppos.row.saturating_sub(1));
        board.ordnance.push(Projectile {
            id: "demo-torpedo".into(),
            kind: "torpedo".into(),
            cell: tpos.to_index(),
            pos: tpos,
            heading: broadside_engine::types::LaneEnd::Fore,
            heading8: Dir8::N,
            speed: 1,
            hull: 1,
            payload: vec![Effect::DAMAGE {
                amount: 6,
                band_falloff: Some(false),
            }],
            owner_faction: Faction::Player,
        });
        log::info!("capture: #132 injected in-flight player torpedo at {tpos:?}");
    }

    // (#90) Optional BROADSIDE_VFX_DEMO=1 injects sample combat RESULTS so the
    // headless capture can verify the new fire/impact/destruction VFX: a player→
    // enemy HIT beam + impact, an enemy→player MISS beam (dimmer), and one enemy
    // marked at zero hull (destruction burst). Off by default so normal captures
    // (incl. the pixel-identity gate) are unaffected.
    let mut vfx_demo_kill: Option<Pos> = None;
    if std::env::var("BROADSIDE_VFX_DEMO").is_ok_and(|v| v != "0") {
        use broadside_engine::types::{FireEvent, WeaponArchetype};
        let ppos = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map_or_else(|| Pos::new(2, 3), |s| s.pos);
        let mid = COLS / 2;
        let tgt = Pos::new(mid, 0); // centre back-row enemy
        board.fire_events.push(FireEvent {
            from_cell: ppos.to_index(),
            to_cell: tgt.to_index(),
            from_pos: ppos,
            to_pos: tgt,
            archetype: WeaponArchetype::Beam,
            attacker_faction: Faction::Player,
            hit: true,
        });
        board.fire_events.push(FireEvent {
            from_cell: tgt.to_index(),
            to_cell: ppos.to_index(),
            from_pos: tgt,
            to_pos: ppos,
            archetype: WeaponArchetype::Beam,
            attacker_faction: Faction::Enemy,
            hit: false, // a miss — renders dimmer
        });
        // (#216) Two MORE enemy beams from OTHER enemy cells, so the volley is
        // 4 shots in INSERTION ORDER (1 player + 3 enemy). With staggered
        // start_delays the capture proves the beams DON'T fire in lockstep:
        // at a given wall-clock the earlier-indexed beams have advanced
        // through their travel/strike phases while the later ones are still
        // silent or just-launched.
        for (r, c) in [(0usize, 0usize), (0usize, COLS.saturating_sub(1))] {
            let p = Pos::new(c, r);
            if board
                .cells
                .get(p.to_index())
                .and_then(|s| s.as_ref())
                .is_some_and(|s| s.faction == Faction::Enemy)
            {
                board.fire_events.push(FireEvent {
                    from_cell: p.to_index(),
                    to_cell: ppos.to_index(),
                    from_pos: p,
                    to_pos: ppos,
                    archetype: WeaponArchetype::Beam,
                    attacker_faction: Faction::Enemy,
                    hit: true,
                });
            }
        }
        // REMOVE the struck enemy (mirrors the resolver's destroy() take()), then
        // remember its cell so we append a kill-burst to the commands below — the
        // faithful post-kill frame (the bin drives the burst off a prev-vs-current
        // id diff, which a single static capture can't reproduce).
        if let Some(slot) = board.cells.get_mut(tgt.to_index()) {
            *slot = None;
        }
        vfx_demo_kill = Some(tgt);
        // (#101) Knock a SURVIVING enemy down to half hull BEFORE compose, so the
        // hull bar renders PARTIAL — then we flash it (full intensity) after compose
        // to show the damage-flash + min-size bar legibility win.
        if let Some(victim) = board
            .cells
            .iter_mut()
            .flatten()
            .find(|s| s.faction == Faction::Enemy)
        {
            if victim.max_hull > 0 {
                victim.hull = (victim.max_hull / 2).max(1);
            }
            // (#107) Half-charge the victim's shields so its lane SHIELD bar reads
            // partial under the hull bar.
            for f in [
                &mut victim.shield_profile.bow,
                &mut victim.shield_profile.stern,
                &mut victim.shield_profile.port,
                &mut victim.shield_profile.starboard,
            ] {
                f.charge = (f.armour + 1) / 2;
            }
        }
        // (#107) Half-charge the PLAYER's shields too so the bottom-HUD SHIELD bar
        // (below HULL) reads partial in the capture.
        if let Some(p) = board
            .cells
            .iter_mut()
            .flatten()
            .find(|s| s.faction == Faction::Player)
        {
            for f in [
                &mut p.shield_profile.bow,
                &mut p.shield_profile.stern,
                &mut p.shield_profile.port,
                &mut p.shield_profile.starboard,
            ] {
                f.charge = (f.armour + 1) / 2;
            }
        }
        log::info!("capture: VFX demo — player HIT + enemy MISS + a kill (burst) at {tgt:?}");
    }

    // (#70) Sync the player's loft pose so the loft pre-pass has a pose to render
    // (mirrors the playable bin's per-frame sync_loft_pose).
    for s in board.cells.iter().flatten() {
        gfx.sync_loft_pose(&s.id, s.orientation);
    }

    // (#76) Project to the LIVE scene size (default 480x270 == ProjectorConfig
    // ::default(); a BROADSIDE_SCENE_RES cycle above reprojects to the new canvas).
    // (#139/#140/#142) Apply the live pitch step in the ACTIVE grid mode — mirrors the
    // bin's scene_projector(): 0 drawbridge (with_pitch), 1 stretch-curved (with_stretch),
    // 2 stretch-straight stepped (with_stretch_straight), 3 stretch-continuous
    // (with_stretch_continuous). The mode + pitch globals were set above by
    // BROADSIDE_GRID_PITCH / _STRETCH / _STRAIGHT / _CONTINUOUS.
    let base = ProjectorConfig::for_scene(
        broadside_engine::gfx::scene_w() as f32,
        broadside_engine::gfx::scene_h() as f32,
    );
    let pitch_t = broadside_engine::gfx::grid_pitch_t();
    // (UNIFY / #84) The unified real-perspective camera is the default — gfx's
    // global UNIFIED flag boots `true`, so the capture mirrors the live bin which
    // honours the same default via `scene_projector_cfg`. `BROADSIDE_UNIFIED=0`
    // forces the legacy fan path (the `U` key off case) for A/B; any other value
    // (including unset) keeps the unified default. The legacy-mode set_unified(false)
    // also propagates to gfx's loft loop so its ship pass matches.
    let unified_on = !std::env::var("BROADSIDE_UNIFIED").is_ok_and(|v| v == "0");
    broadside_engine::gfx::set_unified(unified_on);
    let cfg_no_dims = if unified_on {
        base.with_unified(pitch_t)
    } else {
        match broadside_engine::gfx::grid_mode() {
            1 => base.with_stretch(pitch_t),
            2 => base.with_stretch_straight(pitch_t),
            3 => base.with_stretch_continuous(pitch_t),
            _ => base.with_pitch(pitch_t),
        }
    };
    // (#213 item 4 / #199b) Mirror the live bin: chain `.with_dims(board.dims())`
    // so the playable grid wireframe + every projector-derived overlay lay out
    // at the captured board's variable encounter shape (rather than 5x4).
    let board_dims = board.dims();
    let cfg = cfg_no_dims.with_dims(board_dims.cols, board_dims.rows);
    // (CINEMATIC REBUILD phase a 2026-06-30) Pre-read BROADSIDE_WARP_T so we
    // can build the right Tween2d below — the player warp tween needs the
    // (z_offset, tint_alpha) AND the player VisualShip2d override in lockstep.
    let warp_t_pre: Option<f32> = std::env::var("BROADSIDE_WARP_T")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 1.0));
    // (warp rebuild 5/N — faithful capture 2026-06-30) When BROADSIDE_WARP_T
    // is set, simulate the live LATE-SWAP Transitioning state faithfully:
    //   - The "playable" board (= `board`) is the POST-FIGHT n state:
    //     the player only; all n enemies were destroyed in run_action by
    //     the time the round-clear gate fires. Drop the static back-row
    //     enemies that capture_board seeded so the playable plane during
    //     the warp matches what the live bin shows (player only, no
    //     ghost LoftShips bypassing the destructive fade).
    //   - Build a `pending_board` that mirrors the live bin's n+1 board:
    //     same dims, the canonical spawn (player front-center +
    //     enemies on back row), so the at-depth preview can source its
    //     enemy positions + IDs from here. Use `capture_board_with_dims`
    //     for the canonical layout (matches what
    //     `App::build_current_board` produces post-advance).
    //   - Pose-sync the pending board's enemies down below alongside the
    //     live player so the at-depth LoftShips find a pose + render
    //     (without this they'd silently skip in render_unified_fleet).
    let pending_board: Option<Board> = if warp_t_pre.is_some() {
        // The pending board uses the canonical (player front-center,
        // enemies back row) spawn at the same dims as the live capture
        // board — matches what App::build_current_board() outputs.
        // Player facing Bow(N) at front-center is the runs::
        // player_start_pos() / player_spawn_facing() canonical state.
        use broadside_engine::runs::player_start_pos_in;
        let p_pos = player_start_pos_in(board_dims);
        let pending = capture_board_with_dims(
            board_dims,
            p_pos.col,
            p_pos.row,
            broadside_engine::grid::Facing::Bow(Dir4::N),
        );
        // Strip the live (n) board's enemies — post-fight n has only the
        // player. The destructive fade applied below now correctly
        // covers the playable plane: only the player LoftShip survives
        // (LoftShip is fade-exempt by design, hero-hull rule). No
        // ghost enemy hulls.
        for cell in board.cells.iter_mut() {
            if let Some(s) = cell {
                if s.faction == Faction::Enemy {
                    *cell = None;
                }
            }
        }
        log::info!(
            "capture: faithful warp — stripped n enemies from live board; pending n+1 board built ({} enemies)",
            pending
                .cells
                .iter()
                .flatten()
                .filter(|s| s.faction == Faction::Enemy)
                .count()
        );
        Some(pending)
    } else {
        None
    };
    // (warp rebuild 5/N — faithful capture 2026-06-30) Pose-sync the
    // pending (n+1) board's enemies so the at-depth LoftShips emitted
    // below have a registered pose + actually render. Mirrors the live
    // bin's Transitioning sync (broadside.rs near sync_loft_pose loop).
    // Without this the at-depth hulls silently skip in
    // render_unified_fleet on the None-pose branch.
    if let Some(pending) = pending_board.as_ref() {
        for s in pending.cells.iter().flatten() {
            if s.faction == Faction::Enemy {
                gfx.sync_loft_pose(&s.id, s.orientation);
            }
        }
    }
    // (CINEMATIC REBUILD phase a 2026-06-30) PURE RENDER-TIME PLAYER TWEEN
    // ON CAPTURE — when BROADSIDE_WARP_T is set AND BROADSIDE_WARP_PRIOR_COL/
    // ROW are provided, override the player's VisualShip2d.cell_frac so the
    // t-strip SHOWS the player moving across the 5 frames. Without this the
    // capture rendered the player at its static board cell regardless of t,
    // masking the pure-render-time tween. Mirrors the bin's
    // cinematic_player_cell_frac calculation exactly (same easing, same
    // PLAYER_WARP_FASTNESS midpoint).
    let player_tween = warp_t_pre.and_then(|t_total| {
        let prior_col = std::env::var("BROADSIDE_WARP_PRIOR_COL")
            .ok()?
            .parse::<usize>()
            .ok()?;
        let prior_row = std::env::var("BROADSIDE_WARP_PRIOR_ROW")
            .ok()?
            .parse::<usize>()
            .ok()?;
        // PLAYER_WARP_FASTNESS = 0.5 — matches bin/broadside.rs.
        let inner_t = (t_total / 0.5).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - inner_t) * (1.0 - inner_t);
        let player_pos = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)?;
        let from_col = prior_col.min(board_dims.cols.saturating_sub(1));
        let from_row = prior_row.min(board_dims.rows.saturating_sub(1));
        let col_f = from_col as f32 + (player_pos.col as f32 - from_col as f32) * eased;
        let row_f = from_row as f32 + (player_pos.row as f32 - from_row as f32) * eased;
        Some((from_col, from_row, eased, col_f, row_f))
    });
    // (#291) BROADSIDE_EXPLOSION_LIGHT_T=<secs> seeds a synthetic Explosion
    // effect at the centre back-row cell + advances the vfx pool to the given
    // wall-clock, then reads CombatVfx::brightest_explosion_light(cfg) and
    // pushes it to gfx — so the loft pass that runs next renders the hulls
    // with the real per-surface-normal dynamic reflection. Off by default;
    // the live game wires this from the per-frame vfx state in the bin (out
    // of scope for #291 — the proof is in this capture path).
    if let Some(t_secs) = std::env::var("BROADSIDE_EXPLOSION_LIGHT_T")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        use broadside_engine::types::{FireEvent, WeaponArchetype};
        // Place the synthetic blast at the centre back-row cell so it lights
        // the player (front-centre) from above-and-behind for a clear
        // per-normal response.
        let blast_pos = Pos::new(board_dims.cols / 2, 0);
        // Snapshot a board with the blast cell EMPTY (post-destroy) and one
        // with it occupied; observe both to make the diff spawn the
        // Explosion the same way the live resolver path does.
        let before = capture_board_with_dims(board_dims, player_col, player_row, player_facing);
        // The capture_board_with_dims layout already places enemies at
        // (mid,0)/(lhs,0)/(rhs,0); `after` drops the centre enemy so the
        // observe-diff spawns the Explosion at that cell.
        let mut after = capture_board_with_dims(board_dims, player_col, player_row, player_facing);
        after.fire_events.push(FireEvent {
            from_cell: 0,
            to_cell: blast_pos.to_index(),
            from_pos: Pos::new(player_col, player_row),
            to_pos: blast_pos,
            archetype: WeaponArchetype::Beam,
            attacker_faction: Faction::Player,
            hit: true,
        });
        if let Some(slot) = after.cells.get_mut(blast_pos.to_index()) {
            *slot = None;
        }
        let mut light_vfx = broadside_engine::vfx::CombatVfx::new();
        light_vfx.observe(&before);
        light_vfx.observe(&after); // enemy gone → spawns Explosion at blast_pos
        light_vfx.advance(t_secs.max(0.0));
        if let Some(l) = light_vfx.brightest_explosion_light(&cfg) {
            log::info!(
                "capture: #291 explosion light pos={:?} radius={} color={:?} intensity={:.3}",
                l.pos_world,
                l.radius_world,
                l.color,
                l.intensity
            );
            gfx.set_loft_explosion_light(Some(l));
        } else {
            log::info!("capture: #291 no live explosion at t={t_secs}s (expired or silent)");
            gfx.set_loft_explosion_light(None);
        }
    }
    let mut commands = if let Some((from_col, from_row, eased, col_f, row_f)) = player_tween {
        use broadside_engine::hud::{Tween2d, VisualShip2d};
        use broadside_engine::projector::grid_cell_quad;
        let mut tw = Tween2d::default();
        if let Some(player) = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
        {
            let from_q = grid_cell_quad(Pos::new(from_col, from_row), &cfg);
            let to_q = grid_cell_quad(player.pos, &cfg);
            let lerped_q = broadside_engine::hud::lerp_cell_quad(&from_q, &to_q, eased);
            tw.visual.insert(
                player.id.clone(),
                VisualShip2d {
                    center: lerped_q.center,
                    near_edge_y: lerped_q.corners[3][1],
                    near_edge_width: lerped_q.near_edge_width(),
                    depth_scale: lerped_q.depth_scale,
                    facing_yaw_deg: broadside_engine::hud::loft_facing_ground_yaw(player.facing),
                    cell_frac: [col_f, row_f],
                    kickback: [0.0, 0.0],
                },
            );
            log::info!(
                "capture: warp player tween cell_frac=[{col_f:.2}, {row_f:.2}] (prior=[{from_col},{from_row}] → current={:?})",
                player.pos
            );
        }
        broadside_engine::hud::compose_scene_2d_tweened(&board, &cfg, &gfx, &tw, 0.0)
    } else {
        compose_scene_2d_with(&board, &cfg, &gfx)
    };
    // (#213 t-sampled warp knob) BROADSIDE_WARP_T=<0.0..=1.0> renders the
    // round-change cinematic AT a specific t inside DemoState::Transitioning,
    // so a temporal animation can be verified WITHOUT a winit redraw loop.
    // Bruce's #213 transition was shipped "done" but live-broken because
    // every prior verify was a STILL boot frame — t=0, before any phase
    // animates. This knob mirrors the live bin's per-phase render logic at
    // the requested t:
    //   1. phase_from_progress(t) → (CinematicPhase, sub)
    //   2. If Fade: alpha-multiply every Sprite + Polygon in the composed
    //      scene by (1 - sub) so the outgoing grid visibly clears.
    //   3. The upcoming preview's z_offset + tint_alpha lerp per phase (the
    //      block below reads warp_t to override the dial-driven values).
    // Strip a 5-frame sequence (t=0, .25, .5, .75, 1.0) and Bruce / lead
    // see what each phase actually produces vs spec.
    // (CINEMATIC REBUILD phase a 2026-06-30) Reuse the pre-read warp_t_pre
    // value from before the compose_scene call — both code paths (player
    // tween override + fade/preview lerp) must read the SAME t per frame.
    let warp_t: Option<f32> = warp_t_pre;
    if let Some(t) = warp_t {
        let (phase, sub) = broadside_engine::gfx::phase_from_progress(t);
        // (#213 fade + CINEMATIC REBUILD phase b 2026-06-30) DESTRUCTIVE
        // outgoing-grid fade — mirrors the bin's per-phase multiplier:
        // Fade eases 1→0, phases 2-5 stay at 0. The pre-rebuild capture
        // only ran the multiply during Fade (matching the bin's bug), so
        // a t=0.5 strip frame showed the outgoing grid back at alpha=1 —
        // masking the "overlapping grids" Bruce was seeing live. Now both
        // bin + capture share the same destructive behavior. Only Sprite +
        // Polygon commands fade; LoftShip + TexturedShip (the player hero
        // hull) are intact per Bruce's "player never leaves screen" rule.
        let mul = match phase {
            broadside_engine::gfx::CinematicPhase::Fade => (1.0 - sub).clamp(0.0, 1.0),
            broadside_engine::gfx::CinematicPhase::Approach
            | broadside_engine::gfx::CinematicPhase::Warp
            | broadside_engine::gfx::CinematicPhase::Snap
            | broadside_engine::gfx::CinematicPhase::Settle => 0.0,
        };
        if mul < 1.0 {
            for cmd in &mut commands {
                match cmd {
                    broadside_engine::gfx::DrawCommand::Sprite(s) => {
                        s.color[3] *= mul;
                    }
                    broadside_engine::gfx::DrawCommand::Polygon(p) => {
                        p.color[3] *= mul;
                    }
                    broadside_engine::gfx::DrawCommand::TexturedShip(_)
                    | broadside_engine::gfx::DrawCommand::LoftShip(_) => {}
                }
            }
        }
        log::info!("capture: warp t={t:.2} → phase {phase:?} sub={sub:.2} mul={mul:.2}");
    }
    // (#213/#P7) Mirror the live bin's persistent at-depth preview so headless
    // capture reads the SAME view Bruce will see at boot. Stand-in spawns
    // span multiple rows so the capture shows markers across the upcoming
    // board's full depth; the live bin pulls real EncounterDef::enemy_ships
    // from the campaign cursor. Reads the live preview Z + tint dials so a
    // headless snapshot reflects Bruce's currently-dialled values.
    {
        // (#213 preview centering proof) BROADSIDE_PREVIEW_DIMS=CxR overrides
        // the preview's dims independently of the playable board's dims, so a
        // capture run can prove the preview centers correctly even when the
        // current + upcoming boards have DIFFERENT widths. Default = canonical
        // 5x4 (matches the prior hardcoded path).
        let preview_dims = std::env::var("BROADSIDE_PREVIEW_DIMS")
            .ok()
            .and_then(|s| {
                let lower = s.to_ascii_lowercase();
                let mut it = lower.split('x');
                let c = it.next()?.parse::<usize>().ok()?;
                let r = it.next()?.parse::<usize>().ok()?;
                if c == 0 || r == 0 {
                    return None;
                }
                Some((c, r))
            })
            .unwrap_or((COLS, broadside_engine::grid::ROWS));
        // Spawns clamped into the preview's dims so the stand-in markers
        // always fall in-board even on tiny preview shapes.
        let stand_in_spawns: Vec<Pos> = [(1, 0), (0, 0), (preview_dims.0 - 1, 0)]
            .into_iter()
            .filter_map(|(c, r)| {
                if c < preview_dims.0 && r < preview_dims.1 {
                    Some(Pos::new(c, r))
                } else {
                    None
                }
            })
            .collect();
        // (#213 t-knob + CINEMATIC REBUILD phase c 2026-06-30) When
        // BROADSIDE_WARP_T is set, drive the preview's (z_offset, tint_alpha)
        // through the same per-phase anchor lerp as the bin's
        // preview_seam_lerp at broadside.rs (the helper near the warp
        // consts). The pre-rebuild capture mirrored the bin's late-phase
        // bug (z=rest*0.2 + a≈0.92 — close but NOT z=0/a=1), so the t=1.0
        // strip frame showed the preview at near-final but with a visible
        // gap from the playable plane. Now both bin + capture land the
        // preview at EXACTLY (0, 1) by the START of Settle and HOLD there
        // — so the t=1.0 frame is byte-equivalent to a Playing-state
        // frame at the same camera, making the demo-state swap invisible.
        let rest_z = broadside_engine::gfx::preview_z_offset();
        let rest_a = broadside_engine::gfx::preview_tint_alpha();
        let (z_offset, tint_alpha) = match warp_t {
            Some(t) => {
                let (phase, sub) = broadside_engine::gfx::phase_from_progress(t);
                // Per-phase START anchors (sub=0). Settle anchor = (0, 1).
                let approach_start_z = rest_z;
                let approach_start_a = rest_a;
                let warp_start_z = rest_z * 0.6;
                let warp_start_a = rest_a + (1.0 - rest_a) * 0.30;
                let snap_start_z = rest_z * 0.25;
                let snap_start_a = rest_a + (1.0 - rest_a) * 0.65;
                let settle_start_z = 0.0;
                let settle_start_a = 1.0;
                let eased = sub * sub;
                let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
                match phase {
                    broadside_engine::gfx::CinematicPhase::Fade => (rest_z, rest_a),
                    broadside_engine::gfx::CinematicPhase::Approach => (
                        lerp(approach_start_z, warp_start_z, eased),
                        lerp(approach_start_a, warp_start_a, eased).clamp(0.0, 1.0),
                    ),
                    broadside_engine::gfx::CinematicPhase::Warp => (
                        lerp(warp_start_z, snap_start_z, eased),
                        lerp(warp_start_a, snap_start_a, eased).clamp(0.0, 1.0),
                    ),
                    broadside_engine::gfx::CinematicPhase::Snap => (
                        lerp(snap_start_z, settle_start_z, eased),
                        lerp(snap_start_a, settle_start_a, eased).clamp(0.0, 1.0),
                    ),
                    broadside_engine::gfx::CinematicPhase::Settle => {
                        // HOLD at (0, 1) — t=1.0 seam frame.
                        let _ = eased;
                        (settle_start_z, settle_start_a)
                    }
                }
            }
            None => (rest_z, rest_a),
        };
        // (STABILIZE 2026-06-29) The live bin no longer draws this at-
        // depth preview (team-lead directive: strip the warp cinematic to
        // a clean cut). Capture omits it by default to match the live
        // look. Re-enable with BROADSIDE_PREVIEW=1 when working on the
        // cinematic rebuild (BROADSIDE_WARP_T capture still works for
        // t-sampling the preview's approach).
        if std::env::var("BROADSIDE_PREVIEW").is_ok_and(|v| v != "0") || warp_t.is_some() {
            // (CINEMATIC REBUILD phase d 2026-06-30) When warp_t is set, emit
            // the at-depth REAL LOFT HULLS path matching the live bin's
            // cinematic — same `prepend_upcoming_board_with_loft_2d` call,
            // synthetic stand-in IDs in the form `{class_id}@{cell}` (the
            // canonical Ship.id format from runs::*_for_spawn) so the
            // unified pass can find the enemy loft mesh. Without warp_t
            // (the legacy BROADSIDE_PREVIEW=1 sentinel) keep the flat-
            // triangle markers so existing capture flows are unchanged.
            if warp_t.is_some() {
                // (warp rebuild 5/N — faithful capture 2026-06-30) Source
                // the at-depth preview from the PENDING n+1 board's real
                // enemies — same Ship.id format `class_id@cell` that
                // build_encounter_board produces, so the loft pose
                // registered above by ID lookup hits. The pre-rebuild
                // capture used synthetic `preview-enemy@N` IDs that never
                // matched any pose-registered ship — the at-depth hulls
                // either skipped (when no enemy mesh was installed for
                // those synthetic IDs) or fell back to the wrong pose;
                // either way it didn't reflect what the live bin renders.
                // Now the capture's at-depth preview is byte-equivalent
                // to what the live bin emits during Transitioning.
                let (preview_spawns, preview_ids, p_cols, p_rows) =
                    if let Some(pending) = pending_board.as_ref() {
                        let pd = pending.dims();
                        let mut spawns: Vec<Pos> = Vec::new();
                        let mut ids: Vec<String> = Vec::new();
                        for s in pending.cells.iter().flatten() {
                            if s.faction == Faction::Enemy {
                                spawns.push(s.pos);
                                ids.push(s.id.clone());
                            }
                        }
                        (spawns, ids, pd.cols, pd.rows)
                    } else {
                        let synth: Vec<String> = stand_in_spawns
                            .iter()
                            .map(|p| format!("preview-enemy@{}", p.to_index()))
                            .collect();
                        (
                            stand_in_spawns.clone(),
                            synth,
                            preview_dims.0,
                            preview_dims.1,
                        )
                    };
                broadside_engine::hud::prepend_upcoming_board_with_loft_2d(
                    &mut commands,
                    &cfg,
                    z_offset,
                    p_cols,
                    p_rows,
                    &preview_ids,
                    &preview_spawns,
                    &gfx,
                    tint_alpha,
                );
            } else {
                broadside_engine::hud::prepend_upcoming_board_2d(
                    &mut commands,
                    &cfg,
                    z_offset,
                    preview_dims.0,
                    preview_dims.1,
                    &stand_in_spawns,
                    false,
                    tint_alpha,
                );
            }
        }
    }
    // (team-lead 2026-06-29) READY-glow proof: when a queue is present (the
    // QUEUE_DEMO branch above queued the player + enemy mount weapons), run
    // one CombatVfx pass so emit_ready_glow paints its small per-mount red
    // dots on each queued ship. Mirrors the live bin's per-redraw vfx pass.
    // Without this the headless capture would skip the in-encounter ready
    // cue entirely; with it the capture is faithful proof that the giant
    // cell-center red square is gone + replaced by small hull dots.
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        let mut ready_vfx = broadside_engine::vfx::CombatVfx::new();
        // populate latches without spawning fire/explosion events
        ready_vfx.observe(&board);
        // anim_clock = 0.0 inside the freshly-constructed pool, so the pulse
        // factor 0.55 + 0.45 * sin(0) = 0.55 — visible but not at peak. Fine
        // for the static proof.
        ready_vfx.emit(&mut commands, &board, &cfg);
        log::info!("capture: ready-glow vfx pass over the queued board");
    }
    // (#127) SALVAGE readout — the live bin draws this in its Playing overlay (not
    // inside compose_scene_2d), so append it here with a representative value so the
    // capture shows its NEW bottom-left position under the HULL/SHLD bars.
    broadside_engine::hud::push_salvage_hud(&mut commands, 137);
    // (#134) Debug readouts — also bin-overlay-only; append so the capture shows
    // their NEW bottom-right position (POS/FACE + SHIP/SCENE res).
    if let Some(p) = board
        .cells
        .iter()
        .flatten()
        .find(|s| s.faction == Faction::Player)
    {
        broadside_engine::hud::push_player_readout(&mut commands, p.pos, p.facing);
    }
    broadside_engine::hud::push_res_readout(&mut commands, gfx.loft_res(), gfx.scene_res());
    // (#188 lead) LIVE-FIDELITY HUD layers — the live bin (bin/broadside.rs Playing
    // overlay) ALWAYS draws these three bottom/edge bands every frame regardless of
    // any queued state: ability tiles strip at the bottom-center, queue panel
    // top-right, enemy info panel top-left. Without them the capture's PLAYABLE
    // area is wrong (no bottom-band reserved → near-row hull "looks fine" headless
    // but clips under the live HUD strip). Build representative tiles from the
    // player's mounts so the strip is REAL, not env-gated demo data. Mirrors what
    // build_ship_tiles + push_ability_tiles_2d do in the live bin.
    {
        use broadside_engine::hud::{AbilityIcon, AbilityTile};
        let mut tiles: Vec<AbilityTile> = if let Some(p) = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
        {
            p.mounts
                .iter()
                .enumerate()
                .map(|(i, _m)| AbilityTile {
                    slot: (b'1' + i as u8) as char,
                    icon: AbilityIcon::Beam,
                    damage: 3,
                    range: 1,
                    cooldown: 0,
                    cooldown_max: 0,
                    queued_index: None,
                    can_fire: true,
                    arc: Some('F'),
                })
                .collect()
        } else {
            Vec::new()
        };
        // (#191 Bruce) Add the 3 field-kit card slots 5/6/7 so the capture's
        // bottom strip is the FULL 6-tile menu the live bin renders (3 weapon
        // mounts + 3 cards). build_ship_tiles + the live App always push all 6
        // when cards are loaded; the capture has no Content registry to read
        // real cards, so synthesise placeholder tiles at the same slots/sizes
        // so push_ability_tiles_2d lays out the SAME 6-cell width Bruce sees.
        for slot in ['5', '6', '7'] {
            tiles.push(AbilityTile {
                slot,
                icon: AbilityIcon::Defensive,
                damage: 0,
                range: 0,
                cooldown: 0,
                cooldown_max: 0,
                queued_index: None,
                can_fire: true,
                arc: None,
            });
        }
        broadside_engine::hud::push_ability_tiles_2d(&mut commands, &tiles);
        broadside_engine::hud::push_player_queue_panel_2d(&mut commands, &tiles);
        broadside_engine::hud::push_enemy_info_panel_2d(&mut commands, &board);
    }
    // (Bruce debug) BROADSIDE_ANGLE_OVERLAY=1 draws the per-ship PITCH/ROLL/YAW
    // labels (the bin's `O` toggle) so a headless capture can verify orientation
    // numerically alongside the pixels.
    if std::env::var("BROADSIDE_ANGLE_OVERLAY").is_ok_and(|v| v != "0") {
        broadside_engine::hud::push_ship_angle_overlay(&mut commands, &board, &cfg);
    }
    // (#215 Bruce debug) BROADSIDE_CELL_NUMBERS=1 paints "r,c" on every REAL grid
    // cell (the bin's `H` toggle) so a headless capture can prove which on-screen
    // squares are real cells vs overlays/UI.
    if std::env::var("BROADSIDE_CELL_NUMBERS").is_ok_and(|v| v != "0") {
        broadside_engine::hud::push_cell_numbers_2d(&mut commands, &cfg);
    }
    // (#196) BROADSIDE_CONTROLS_POPUP=1 forces the centered controls popup on
    // (the bin's `F1` toggle) so headless captures can verify the panel layout.
    if std::env::var("BROADSIDE_CONTROLS_POPUP").is_ok_and(|v| v != "0") {
        broadside_engine::gfx::set_controls_popup(true);
    }
    broadside_engine::hud::push_controls_popup(&mut commands);
    // (#90) VFX-demo kill burst at the destroyed enemy's cell — the bin emits this
    // off its prev-vs-current id diff each frame; here we append it directly so the
    // headless demo shows the post-kill burst.
    if let Some(killed) = vfx_demo_kill {
        broadside_engine::hud::push_destruction_at(&mut commands, &[killed], &cfg);
        // (#119) Procedural explosion particle burst at the killed cell — the bin
        // seeds this on its kill detection; here we seed + advance it a few frames
        // so the single static capture catches the spray MID-flight (spread out),
        // not all stacked at the centre.
        {
            let c = broadside_engine::projector::grid_cell_quad(killed, &cfg).center;
            let mut pool = broadside_engine::vfx::ParticlePool::new();
            pool.spawn_burst(c, 22, [1.0, 0.72, 0.32, 1.0], 0.55);
            for _ in 0..6 {
                pool.advance(1.0 / 60.0);
            }
            pool.emit(&mut commands);
            log::info!("capture: #119 explosion particle burst at killed cell {killed:?}");
            // (#282) BROADSIDE_VFX_DEMO_SEQ=1 round-trips a JSON catalog that
            // mirrors what the broadside_vfx_editor a4c28f9 export produces:
            // a "Sequence" EffectDef listing steps with delay_secs PLUS a
            // ParticleBurst with non-default shape + rotation_min/max +
            // spin_rate. We load via EffectCatalog::from_json_str (the same
            // path the game reads at boot), then play the sequence via
            // CombatVfx::play_sequence and seed the burst via
            // ParticlePool::spawn_burst_with reading the loaded config.
            // This proves the WHOLE editor → engine → in-game render chain,
            // not just an inline Rust struct. Disabled by default so the
            // pre-existing demo capture stays identical.
            //
            // The JSON below is shaped exactly like the editor's export
            // (schema landed in 4f39978): internally-tagged "kind"
            // discriminator, snake_case ShapeKind values, every optional
            // field present so we round-trip the full surface.
            if std::env::var("BROADSIDE_VFX_DEMO_SEQ").is_ok_and(|v| v != "0") {
                use broadside_engine::effects::{EffectCatalog, EffectKind, ShapeKind};
                let player_pos = board
                    .cells
                    .iter()
                    .flatten()
                    .find(|s| s.faction == Faction::Player)
                    .map_or_else(|| Pos::new(2, 3), |s| s.pos);
                let editor_json = r#"{
                    "effects": [
                        { "id": "player_beam",
                          "kind": "ShotBeam" },
                        { "id": "spark",
                          "kind": "HitFlash" },
                        { "id": "boom",
                          "kind": "Explosion" },
                        { "id": "shaped_debris",
                          "kind": "ParticleBurst",
                          "count": 24,
                          "color": [0.35, 0.95, 1.0, 1.0],
                          "life_secs": 0.65,
                          "speed_min": 30.0,
                          "speed_max": 110.0,
                          "size_min": 5.0,
                          "size_max": 9.0,
                          "shape": "triangle",
                          "rotation_min": 0.0,
                          "rotation_max": 6.2831855,
                          "spin_rate": 3.0 },
                        { "id": "kill_combo",
                          "kind": "Sequence",
                          "steps": [
                            { "id": "player_beam",   "delay_secs": 0.0 },
                            { "id": "spark",         "delay_secs": 0.05 },
                            { "id": "boom",          "delay_secs": 0.20 }
                          ] }
                    ]
                }"#;
                let cat = EffectCatalog::from_json_str(editor_json)
                    .expect("editor-shaped JSON round-trips into EffectCatalog");
                // ROUND-TRIP ASSERTIONS — fail loud if the engine's serde
                // silently dropped any of the new shape/rotation/spin fields.
                let burst_def = cat
                    .get("shaped_debris")
                    .expect("ParticleBurst id 'shaped_debris' present after load");
                let burst_cfg = match &burst_def.kind {
                    EffectKind::ParticleBurst(p) => p.clone(),
                    other => panic!("expected ParticleBurst, got {other:?}"),
                };
                assert_eq!(
                    burst_cfg.shape,
                    ShapeKind::Triangle,
                    "shape=triangle round-trips (not silently dropped to Square)"
                );
                assert!(
                    (burst_cfg.spin_rate - 3.0).abs() < 1e-5,
                    "spin_rate round-trips, got {}",
                    burst_cfg.spin_rate
                );
                assert!(
                    (burst_cfg.rotation_max - std::f32::consts::TAU).abs() < 1e-3,
                    "rotation_max round-trips, got {}",
                    burst_cfg.rotation_max
                );
                log::info!(
                    "capture: #282 catalog loaded; shaped_debris.shape={:?} spin_rate={} rotation_max={}",
                    burst_cfg.shape, burst_cfg.spin_rate, burst_cfg.rotation_max
                );
                // Sequence playback through the loaded catalog.
                let mut seq_vfx = broadside_engine::vfx::CombatVfx::new();
                let scheduled =
                    seq_vfx.play_sequence(&cat, "kill_combo", killed, Some(player_pos), None);
                assert_eq!(scheduled, 3, "all 3 sequence steps resolved + scheduled");
                log::info!("capture: #282 Sequence scheduled {scheduled} steps");
                let seq_t: f32 = std::env::var("BROADSIDE_VFX_DEMO_SEQ_T")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.30);
                seq_vfx.advance(seq_t);
                seq_vfx.emit(&mut commands, &board, &cfg);
                // Shaped-particle burst FROM the loaded catalog's ParticleBurst
                // def — this is the round-trip that proves the editor's shape
                // export actually reaches the in-game render. Centre it on the
                // player so the t-strip shows triangle silhouettes at varying
                // rotation as `seq_t` advances past spawn.
                let mut tri_pool = broadside_engine::vfx::ParticlePool::new();
                let tri_center =
                    broadside_engine::projector::grid_cell_quad(player_pos, &cfg).center;
                tri_pool.spawn_burst_with(&burst_cfg, tri_center, 0.0);
                // Advance the particle pool by the same `seq_t` knob so the
                // t-strip shows the rotation+spin progression alongside the
                // sequence steps. (spin_rate=3 rad/s × t shows visible turn.)
                let steps = ((seq_t * 60.0) as u32).max(1);
                for _ in 0..steps {
                    tri_pool.advance(1.0 / 60.0);
                }
                tri_pool.emit(&mut commands);
                log::info!(
                    "capture: #282 round-tripped shaped burst (Triangle, spin 3 rad/s) at {player_pos:?}"
                );
            }
            // (#178) Drive the REAL wall-clock CombatVfx explosion at mid-life so the
            // capture shows emit_explosion's expanding multi-layer blast (the live
            // death path). observe(before) latches the enemy present; observe(after =
            // current `board`, enemy already removed above) spawns the Explosion;
            // advance ~0.18s puts it mid-expansion; emit over the same lane the bin uses.
            {
                // `before` = a fresh campaign board (all enemies present, incl. the
                // centre back-row one at `killed`); `board` already had it removed by
                // the VFX-demo kill above. observe(before)->observe(board) is exactly
                // the live vanish diff that spawns the Explosion.
                let before = capture_board(player_col, player_row, player_facing);
                let mut demo_vfx = broadside_engine::vfx::CombatVfx::new();
                demo_vfx.observe(&before);
                demo_vfx.observe(&board); // enemy gone -> spawns Explosion; fire_events -> ShotBeams
                                          // (#216) Override the demo-vfx wall-clock via env var so a
                                          // SEQUENCE of captures at different t values proves the
                                          // staggered-volley animation. Default 0.08 keeps the old
                                          // single-frame look unchanged. ENEMY_BEAT_SECS=0.12 → t=0.04
                                          // shows only beam[0]; t=0.20 shows beams[0..2]; t=0.45 lands
                                          // the explosion bloom AFTER its causing beam.
                let advance_secs: f32 = std::env::var("BROADSIDE_VFX_DEMO_T")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.08);
                demo_vfx.advance(advance_secs); // (#216) staggered, vfx-driven wall-clock
                demo_vfx.emit(&mut commands, &board, &cfg);
                log::info!("capture: #178 CombatVfx explosion + travelling beam at {killed:?}");
            }
        }
        // (#101) Flash the hull bar of the SURVIVING enemy we knocked to half hull
        // (above, before compose, so its bar already renders PARTIAL) at full
        // intensity — the moment-of-hit pop — so the capture shows the new
        // damage-flash + min-size bar (the "I don't see the enemy health bar
        // dropping" legibility win).
        if let Some(victim) = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Enemy)
        {
            broadside_engine::hud::push_hull_flash_2d(&mut commands, victim, 1.0, &cfg);
            // (#106) Floating damage number above the same enemy (demo amount).
            broadside_engine::hud::push_damage_number_2d(&mut commands, victim, 3, 1.0, &cfg);
            log::info!(
                "capture: #101/#106 hull-flash + damage number on surviving enemy {} at {:?}",
                victim.id,
                victim.pos
            );
        }
    }
    // (#98/#100) With QUEUE_DEMO, append a representative ability-tile row so the
    // headless shot shows the layout (damage # top-left, key # bottom-right,
    // cooldown ticks) AND the #100 cues. The capture has no Content, so hand-build
    // tiles in varied states:
    //   * slot 1 = QUEUED + bears: amber border + order badge "2", no veil;
    //   * slot 2 = resting, on cooldown (grey ticks), not queued;
    //   * slot 3 = QUEUED but CAN'T BEAR (Bruce's press-3 bug): amber border +
    //     order badge "1" + dark veil + red slash = "won't fire from here";
    //   * slot 5 = resting, ready (white border), not queued.
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        use broadside_engine::hud::{AbilityIcon, AbilityTile};
        let tiles = vec![
            AbilityTile {
                slot: '1',
                icon: AbilityIcon::Beam,
                damage: 3,
                range: 1,
                cooldown: 0,
                cooldown_max: 0,
                queued_index: Some(1),
                can_fire: true,
                arc: Some('F'),
            },
            AbilityTile {
                slot: '2',
                icon: AbilityIcon::Ordnance,
                damage: 6,
                range: 3,
                cooldown: 0,
                cooldown_max: 0,
                queued_index: None,
                can_fire: false,
                arc: Some('F'),
            },
            AbilityTile {
                slot: '3',
                icon: AbilityIcon::Defensive,
                damage: 5,
                range: 2,
                cooldown: 0,
                cooldown_max: 3,
                queued_index: Some(0),
                can_fire: false,
                arc: Some('B'),
            },
            AbilityTile {
                slot: '5',
                icon: AbilityIcon::Defensive,
                damage: 0,
                range: 0,
                cooldown: 0,
                cooldown_max: 0,
                queued_index: None,
                can_fire: true,
                arc: None,
            },
        ];
        broadside_engine::hud::push_ability_tiles_2d(&mut commands, &tiles);
        // (#128) Player QUEUE panel (top-right) from the same demo tiles — slots 1+3
        // are queued (index 1 + 0), so the panel shows them in fire order and the
        // hand tiles 1+3 hollow out. Lets the capture show the hand->queue move.
        broadside_engine::hud::push_player_queue_panel_2d(&mut commands, &tiles);
        // (#129) Enemy INFO panel (top-left) from the board's enemies — shows each
        // enemy's hull + shield + the revealed queue injected above.
        broadside_engine::hud::push_enemy_info_panel_2d(&mut commands, &board);
        // (#122) Player targeting telegraph demo: show the cyan preview of where a
        // queued weapon would strike. The live bin resolves the cells via
        // resolve_targeting_2d; here we hand-pick a target cell forward of the
        // player so the headless shot shows the cyan target outline + aim line.
        if let Some(ppos) = board
            .cells
            .iter()
            .flatten()
            .find(|s| s.faction == Faction::Player)
            .map(|s| s.pos)
        {
            // A cell two rows up-lane (toward the enemies) as the demo target.
            let tgt = Pos::new(ppos.col, ppos.row.saturating_sub(2));
            broadside_engine::hud::push_player_targeting_2d(&mut commands, ppos, &[tgt], &cfg);
            log::info!("capture: #122 player targeting telegraph demo {ppos:?} -> {tgt:?}");
        }
    }
    // (#90 yellow-square repro) With QUEUE_DEMO, dump any LARGE draw command (a
    // sprite half-size > 60px or a polygon spanning > 120px in either axis) + its
    // colour — pinpoints the giant fill covering the ship/field without eyeballing.
    if std::env::var("BROADSIDE_QUEUE_DEMO").is_ok_and(|v| v != "0") {
        use broadside_engine::gfx::DrawCommand;
        // A "yellow-ish" tint: high R+G, low B (gold/amber/warm-white).
        let yellowish = |c: &[f32; 4]| c[0] > 0.7 && c[1] > 0.6 && c[2] < 0.6 && c[3] > 0.2;
        for (i, c) in commands.iter().enumerate() {
            match c {
                DrawCommand::Sprite(s) if yellowish(&s.color) => {
                    log::info!(
                        "YELLOW sprite #{i}: pos {:?} half {:?} color {:?}",
                        s.pos,
                        s.half_size,
                        s.color
                    );
                }
                DrawCommand::Sprite(s) if s.half_size[0] > 40.0 || s.half_size[1] > 40.0 => {
                    log::info!(
                        "BIG sprite #{i}: pos {:?} half {:?} color {:?}",
                        s.pos,
                        s.half_size,
                        s.color
                    );
                }
                DrawCommand::Polygon(p) if yellowish(&p.color) => {
                    log::info!(
                        "YELLOW polygon #{i}: p0 {:?} p2 {:?} color {:?}",
                        p.p0,
                        p.p2,
                        p.color
                    );
                }
                DrawCommand::Polygon(p) => {
                    let wspan = (p.p1[0] - p.p0[0]).abs().max((p.p2[0] - p.p3[0]).abs());
                    let hspan = (p.p3[1] - p.p0[1]).abs().max((p.p2[1] - p.p1[1]).abs());
                    if wspan > 120.0 || hspan > 120.0 {
                        log::info!(
                            "BIG polygon #{i}: p0 {:?} p2 {:?} (span {wspan:.0}x{hspan:.0}) color {:?}",
                            p.p0, p.p2, p.color
                        );
                    }
                }
                _ => {}
            }
        }
        log::info!("capture: {} total draw commands", commands.len());
    }
    // Ensure the output dir exists so the save never trips os-error-3 on a fresh
    // checkout / unusual cwd (the default `bugs/` dir, or any custom path).
    if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::warn!("capture: could not create {}: {e}", parent.display());
            }
        }
    }
    match gfx.capture_png(&commands, std::path::Path::new(&path)) {
        Ok(()) => log::info!("capture: wrote {path}"),
        Err(e) => {
            log::error!("capture failed: {e}");
            std::process::exit(1);
        }
    }
}
