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

/// Build a `types::Ship` at `pos`/`facing` (mirrors the bin's make_ship: 2-D
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
fn capture_board(player_col: usize, player_facing: Facing) -> Board {
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    let place = |cells: &mut Vec<Option<Ship>>, s: Ship| {
        let idx = s.pos.to_index();
        cells[idx] = Some(s);
    };
    let start = player_start_pos();
    let ppos = Pos::new(player_col.min(COLS - 1), start.row);
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
    place(&mut cells, player);
    let mid = COLS / 2;
    place(&mut cells, make_ship("enemy-2", Faction::Enemy, Pos::new(mid, 0), enemy_spawn_facing()));
    place(&mut cells, make_ship("enemy-3", Faction::Enemy, Pos::new(mid - 1, 0), enemy_spawn_facing()));
    place(&mut cells, make_ship("enemy-5", Faction::Enemy, Pos::new(mid + 1, 0), enemy_spawn_facing()));

    Board {
        size: COLS,
        cells,
        ordnance: Vec::new(),
        hazards: (0..broadside_engine::grid::CELLS).map(|_| Vec::new()).collect(),
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

    // (#70) Install the player's faithful Aegis GLB exactly as the playable bin
    // does (broadside.rs install_player_glb) so the capture faithfully shows the
    // LIVE-3D player ship, not the flat-box placeholder. Enemies stay flat (no
    // enemy mesh installed), matching the live game.
    const AEGIS_GLB: &[u8] = include_bytes!("../../assets/ships/Aegis.glb");
    match gfx.install_player_glb(AEGIS_GLB) {
        Ok(()) => log::info!("capture: player Aegis hull installed from Aegis.glb"),
        Err(e) => log::warn!("capture: Aegis.glb import failed ({e}); player falls back to sprite/flat-box"),
    }

    // (#76) Optional BROADSIDE_SHIP_RES=N env cycles the SHIP loft res forward N
    // steps before the capture, so a headless shot can verify the live ship-res
    // change (160x100 -> 220x138 -> 320x200) renders correctly. Default 0 = the
    // baseline res. Mirrors the live `,`/`.` control via `Gfx::cycle_loft_res`.
    if let Ok(n) = std::env::var("BROADSIDE_SHIP_RES") {
        if let Ok(steps) = n.parse::<u32>() {
            for _ in 0..steps {
                let (w, h) = gfx.cycle_loft_res(true);
                log::info!("capture: cycled ship res -> {w}x{h}");
            }
        }
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
        Some("n") | Some("N") => Facing::Bow(Dir4::N),
        Some("s") | Some("S") => Facing::Bow(Dir4::S),
        Some("e") | Some("E") => Facing::Bow(Dir4::E),
        Some("w") | Some("W") => Facing::Bow(Dir4::W),
        _ => player_spawn_facing(),
    };
    let board = capture_board(player_col, player_facing);
    log::info!("capture: player at column {player_col}, facing {player_facing:?}");

    // (#70) Sync the player's loft pose so the loft pre-pass has a pose to render
    // (mirrors the playable bin's per-frame sync_loft_pose).
    for s in board.cells.iter().flatten() {
        gfx.sync_loft_pose(&s.id, s.orientation);
    }

    let cfg = ProjectorConfig::default();
    let commands = compose_scene_2d_with(&board, &cfg, &gfx);
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
