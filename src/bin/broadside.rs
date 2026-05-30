//! Runnable demo: opens a window and renders a Broadside scene.
//!
//! Slice-A scope: opens a 1320×480 (virtual) window, configures wgpu, clears
//! to deep-space ink, and re-renders on resize. No scene content yet —
//! `hud::compose_scene` returns an empty list, so the user sees the clear
//! color and the letterboxed virtual canvas.
//!
//! Later slices add a starfield, the lane trapezoid, ships, ordnance, HUD.
//!
//! Run with:
//!
//! ```bash
//! cargo run --bin broadside
//! ```

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use broadside_engine::gfx::{Gfx, VIRTUAL_H, VIRTUAL_W};
use broadside_engine::hud;
use broadside_engine::perspective::DEFAULT_LANE;
use broadside_engine::types::{Board, EventBus};

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    board: Board,
}

impl App {
    fn new() -> Self {
        // Slice-A placeholder board: empty 7-cell lane. Replaced in slice-E by
        // the render-example.ts scenario (player + 4 enemies).
        let size = 7;
        Self {
            window: None,
            gfx: None,
            board: Board {
                size,
                cells: (0..size).map(|_| None).collect(),
                ordnance: Vec::new(),
                hazards: (0..size).map(|_| Vec::new()).collect(),
                patrol: 1,
                bus: EventBus::default(),
                destroys_this_window: 0,
            },
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Window is 2× the virtual canvas so the integer-scale blit lands at
        // 2× by default — sharp pixel-art without a giant initial window.
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
                let instances = hud::compose_scene(&self.board, &DEFAULT_LANE);
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
