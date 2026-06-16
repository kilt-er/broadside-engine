//! Headless offscreen capture — renders ONE frame of the real combat board to a
//! PNG, with no window or display. The permanent "give the team eyes" tool: an
//! AI agent (or anyone) can Read the PNG to SEE exactly what the renderer
//! produces, instead of needing a live window (which a headless/locked machine
//! can't provide).
//!
//! Run: `cargo run --bin capture --features render,runtime -- bugs/current_state.png`
//! (defaults to `bugs/current_state.png` if no path is given).
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

/// The campaign-layout board: player front-centre Bow(N), enemies centre-out
/// across the back row Bow(S) — the real bow-to-bow start.
fn capture_board() -> Board {
    let mut cells: Vec<Option<Ship>> = (0..broadside_engine::grid::CELLS).map(|_| None).collect();
    let place = |cells: &mut Vec<Option<Ship>>, s: Ship| {
        let idx = s.pos.to_index();
        cells[idx] = Some(s);
    };
    let mut player = make_ship("player", Faction::Player, player_start_pos(), player_spawn_facing());
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
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bugs/current_state.png".to_string());

    let mut gfx = pollster::block_on(Gfx::new_headless());

    // Install the real ship hulls (same as the playable bin): player = Aegis loft,
    // enemies = CAD. Fall back silently to the flat box if an install fails.
    const SHIP_GLB: &[u8] = include_bytes!("../../assets/ships/broadside-ship.glb");
    let _ = gfx.install_enemy_cad(SHIP_GLB);
    const SHIP_LIBRARY_V2: &[u8] =
        include_bytes!("../../assets/ships/broadside-ship-library_v2.json");
    // Loft the Aegis exactly as the bin does (minimal-field parse, robust to the
    // v2 settings schema), then install it as the player loft mesh.
    if let Some(mesh) = loft_aegis(SHIP_LIBRARY_V2) {
        gfx.install_player_loft_mesh(&mesh);
    }

    // Sync loft poses to each ship's orientation so the hulls render at their
    // stance (not the default), then compose + capture one frame.
    let board = capture_board();
    for s in board.cells.iter().flatten() {
        gfx.sync_loft_pose(&s.id, s.orientation);
    }
    gfx.advance_loft_poses(0.0);

    let cfg = ProjectorConfig::default();
    let commands = compose_scene_2d_with(&board, &cfg, &gfx);
    match gfx.capture_png(&commands, std::path::Path::new(&path)) {
        Ok(()) => log::info!("capture: wrote {path}"),
        Err(e) => {
            log::error!("capture failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Loft the "Aegis" ship from the v2 library to a HullMesh (minimal-field parse,
/// robust to the v2 editor settings schema — same approach as the bin).
fn loft_aegis(library_bytes: &[u8]) -> Option<broadside_engine::loft::HullMesh> {
    use broadside_engine::loft::{
        loft_from_profiles, LoftParams, DEFAULT_SEC_N, PLAYER_LOFT_HSCALE_BOOST,
    };
    use broadside_engine::ship_design::Point2;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Library {
        ships: Vec<LibShip>,
    }
    #[derive(Deserialize)]
    struct LibShip {
        name: String,
        design: LibDesign,
    }
    #[derive(Deserialize)]
    struct LibDesign {
        plan: Vec<[f64; 2]>,
        section: Vec<[f64; 2]>,
        #[serde(default, rename = "heightProfile")]
        height_profile: Option<Vec<[f64; 2]>>,
        settings: LibSettings,
    }
    #[derive(Deserialize)]
    struct LibSettings {
        stretch: f64,
        hscale: f64,
        #[serde(default)]
        secn: Option<usize>,
    }

    let library: Library = serde_json::from_slice(library_bytes).ok()?;
    let ship = library.ships.into_iter().find(|s| s.name == "Aegis")?;
    let d = ship.design;
    let to_pts = |v: Vec<[f64; 2]>| v.into_iter().map(Point2).collect::<Vec<_>>();
    let params = LoftParams {
        stretch: d.settings.stretch as f32,
        // #54: give the hull vertical mass for the steep in-game camera (the
        // game bin applies the same boost, so the capture stays faithful).
        hscale: d.settings.hscale as f32 * PLAYER_LOFT_HSCALE_BOOST,
        sec_n: d.settings.secn.unwrap_or(DEFAULT_SEC_N).max(3),
    };
    Some(loft_from_profiles(
        &to_pts(d.plan),
        &to_pts(d.section),
        d.height_profile.map(to_pts).as_deref(),
        params,
    ))
}
