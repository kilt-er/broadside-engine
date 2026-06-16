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
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bugs/current_state.png".to_string());

    let mut gfx = pollster::block_on(Gfx::new_headless());

    // (#66) Load the BAKED ship sprites exactly as the playable bin does
    // (try_load_ship_sprites at broadside.rs:1085) so the capture faithfully shows
    // the contract sprite path (editor-baked PNGs, drawn UNLIT) — NOT the dropped
    // runtime loft. Aegis sprites live in assets/sprites/aegis_*.png.
    let loaded = gfx.try_load_ship_sprites(std::path::Path::new("assets"));
    log::info!("capture: loaded {loaded} ship sprite(s)");

    let board = capture_board();

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
