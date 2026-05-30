//! Runnable demo: opens a window and renders the render-example.ts Broadside
//! scene.
//!
//! Scene (mirrors `_drive_pull/ship-view-decision/render-example.ts`):
//! - 7-cell lane (`perspective::DEFAULT_LANE`).
//! - Player frigate at cell 0, bow pointing fore.
//! - Enemy frigates at cells 2 (broadside), 3 (bow-on aft), 5 (bow-on fore),
//!   and 6 (bow-on fore).
//! - One torpedo en-route at fractional cell 4.0, heading fore.
//! - Player carries one bow shield charge (visible as a teal pip) and two
//!   queued actions, so the queue glyphs render.
//!
//! Run with:
//!
//! ```bash
//! cargo run --bin broadside --features render,runtime
//! ```

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use broadside_engine::geometry::default_shield_profile;
use broadside_engine::gfx::{Gfx, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::hud;
use broadside_engine::perspective::{LaneGeometry, DEFAULT_LANE};
use broadside_engine::types::{
    Arc as TArc, Board, EventBus, Faction, LaneEnd, Mount, Orientation, Projectile, Ship,
};

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    board: Board,
    lane: LaneGeometry,
}

impl App {
    fn new() -> Self {
        // DEFAULT_LANE is tuned for a 660×240 viewport; the engine's virtual
        // canvas is 1320×480 (2× of that, for integer-scale crispness). Scale
        // the lane uniformly so it covers the full canvas instead of just the
        // top-left quadrant.
        let lane = DEFAULT_LANE.scaled((VIRTUAL_W as f32) / 660.0);
        Self {
            window: None,
            gfx: None,
            board: render_example_board(),
            lane,
        }
    }
}

/// Mirrors the board state hard-coded in `render-example.ts` so the Rust
/// demo and the TypeScript SVG output draw the same scene.
fn render_example_board() -> Board {
    let size = 7usize;
    let mut cells: Vec<Option<Ship>> = (0..size).map(|_| None).collect();

    cells[0] = Some(make_ship("player", Faction::Player, 0, Orientation::BowOn { bow: LaneEnd::Fore }));
    cells[2] = Some(make_ship("enemy-2", Faction::Enemy, 2, Orientation::Broadside));
    cells[3] = Some(make_ship("enemy-3", Faction::Enemy, 3, Orientation::BowOn { bow: LaneEnd::Aft }));
    cells[5] = Some(make_ship("enemy-5", Faction::Enemy, 5, Orientation::BowOn { bow: LaneEnd::Fore }));
    cells[6] = Some(make_ship("enemy-6", Faction::Enemy, 6, Orientation::BowOn { bow: LaneEnd::Fore }));

    // Player frigate: one bow shield charge, two queued actions (so queue
    // glyphs render). Heat at 2/6 so the heat bar shows a partial fill.
    if let Some(player) = cells[0].as_mut() {
        player.shield_profile.bow.charge = 1;
        player.queue = vec!["pulse_laser".into(), "torpedo".into()];
        player.mounts = vec![
            Mount { id: "m1".into(), arc: TArc::Forward, weapon: "pulse_laser".into() },
            Mount { id: "m2".into(), arc: TArc::Forward, weapon: "torpedo".into() },
        ];
        player.heat = 2;
    }

    let ordnance = vec![Projectile {
        id: "torp-1".into(),
        kind: "torpedo".into(),
        cell: 4,
        heading: LaneEnd::Fore,
        speed: 1,
        hull: 1,
        payload: Vec::new(),
        owner_faction: Faction::Player,
    }];

    Board {
        size,
        cells,
        ordnance,
        hazards: (0..size).map(|_| Vec::new()).collect(),
        patrol: 1,
        bus: EventBus::default(),
        destroys_this_window: 0,
    }
}

fn make_ship(id: &str, faction: Faction, cell: usize, orientation: Orientation) -> Ship {
    Ship {
        id: id.into(),
        faction,
        cell,
        orientation,
        hull: 5,
        max_hull: 5,
        heat: 0,
        heat_max: 6,
        locked_out: false,
        shield_profile: default_shield_profile(),
        mounts: Vec::new(),
        queue: Vec::new(),
        cooldowns: std::collections::HashMap::new(),
        statuses: Vec::new(),
        traits: Vec::new(),
        klass: None,
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Window opens at the virtual canvas size; the blit pipeline picks
        // the largest integer scale and letterboxes any leftover area.
        let attrs = Window::default_attributes()
            .with_title("Broadside")
            .with_inner_size(winit::dpi::LogicalSize::new(
                VIRTUAL_W as f64,
                VIRTUAL_H as f64,
            ));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let gfx = pollster::block_on(Gfx::new(window.clone()));
        self.window = Some(window);
        self.gfx = Some(gfx);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gfx) = self.gfx.as_mut() else { return; };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => gfx.resize(size),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(KeyCode::Escape) = event.physical_key {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                let instances = hud::compose_scene(&self.board, &self.lane);
                match gfx.render(&instances) {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        gfx.reconfigure();
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => {
                        log::error!("wgpu surface OOM, exiting");
                        event_loop.exit();
                    }
                    Err(e) => log::warn!("surface error: {e:?}"),
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop");
}
