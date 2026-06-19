//! Parallax space background — the 20-layer depth queue that scrolls behind the
//! whole scene.
//!
//! This is a faithful implementation of `ShipEditor/BROADSIDE_BACKGROUND_SPEC.md`
//! §4 (the authoritative render math) and §5 (driving it from gameplay). The
//! editor and the in-game renderer must match 1:1, so the slot math, parallax
//! constants, and draw order here mirror the spec exactly — and the constants
//! are **read from the manifest, never hardcoded** (the spec is explicit: the
//! look is still being tuned, so the exported `background_manifest.json` is the
//! source of truth for `frame`/`canvas`/`queue`/`parallax`).
//!
//! ## What it does each frame
//!
//! 1. [`Background::tween`] eases `focus` toward `focus_target` (depth: which
//!    campaign layer is at the front) and `player_pos` toward `pos_target`
//!    (the player's horizontal column 0..4), per spec §5's exponential ease.
//! 2. [`Background::draw`] computes the visible window via [`visible_layers`]
//!    (spec §4 slot math) and draws each visible layer as one textured quad
//!    into the 480×270 offscreen target, **far → near**, with straight-alpha
//!    blending — BEFORE anything else, so it sits behind the scene.
//!
//! ## Assets, and the fallback
//!
//! The real art (the manifest + `bg_layer_NN.png` files) does **not exist yet**
//! (v2 decision #10). So every layer slot ships with a SOLID-INK fallback: a
//! distinct flat tint per depth index, generated as a 1×1 texture. With nothing
//! loaded, the background still renders as a readable gradient of depth-tinted
//! bands that slide + scale + fade exactly like the real layers will — enough
//! to test the parallax math and the gameplay wiring now. When the editor's
//! real export lands, [`Background::load_manifest`] swaps the painted PNGs in at
//! their depth index and the fallback only covers the still-empty slots.
//!
//! ## Engine state (wired later)
//!
//! `focus_target` is `Run.layer` (0..19 — the campaign cursor doubles as the
//! background depth cursor, decision #3) and `pos_target` is the player's column
//! (0..4). Both are exposed as setters now ([`Background::set_focus_target`] /
//! [`Background::set_pos_target`]); the bin wires them to real engine state when
//! the v2 board lands. Until then they default to a centered mid-depth view.

use std::path::Path;

use serde::Deserialize;

/// Format of the per-layer textures and the offscreen target they draw into.
/// Must match [`crate::gfx`]'s `OFFSCREEN_FORMAT` so the background pass shares
/// the same render target.
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// (#137 Bruce, TEMPORARY) Blank the N NEAREST parallax layers (the biggest,
/// fastest-sliding bands closest to the camera — the ones that make the
/// placeholder background read "too busy"). A clean, easily-revertible toggle: the
/// layer SYSTEM is untouched (textures still load, the queue is intact) — `draw`
/// just SKIPS the nearest `N` visible layers (the last `N` of the far→near
/// `visible_layers` list). Set to `0` to restore the full background once real
/// level art exists. Bruce can confirm via capture whether the NEAR pair is the
/// right two (vs the far end — flip the skip to `&draws[..N]` if he meant far).
const BLANK_NEAR_LAYERS: usize = 2;

// ---------------------------------------------------------------------------
// Parallax parameters (spec §2). Defaults match the spec table, but the loader
// overwrites every field from the manifest — never trust these at runtime once
// a manifest is present.
// ---------------------------------------------------------------------------

/// The parallax constants from the manifest's `parallax` + `queue.visible`
/// blocks. See spec §2 for the meaning of each field; see [`visible_layers`]
/// for how they drive the per-layer transform.
#[derive(Debug, Clone, Copy)]
pub struct ParallaxParams {
    /// Horizontal slide (frame px) of the NEAREST layer per one position-step
    /// from center.
    pub step_px: f32,
    /// Horizontal-slide strength at the near / far slot (1.0 / 0.18).
    pub near_factor: f32,
    pub far_factor: f32,
    /// Render scale at the near / far slot (1.0 / 0.62).
    pub near_scale: f32,
    pub far_scale: f32,
    /// The centered position = `(positions - 1) / 2` (= 2 for 5 positions).
    pub center_position: f32,
    /// How many layers are drawn at once (the visible window width, = 5).
    pub visible: f32,
}

impl Default for ParallaxParams {
    fn default() -> Self {
        // Spec §2 table values.
        Self {
            step_px: 120.0,
            near_factor: 1.0,
            far_factor: 0.18,
            near_scale: 1.0,
            far_scale: 0.62,
            center_position: 2.0,
            visible: 5.0,
        }
    }
}

/// The per-layer transform computed for one visible layer in one frame. One
/// [`LayerDraw`] becomes one textured quad. Mirrors the spec §4 reference
/// `LayerDraw`.
#[derive(Debug, Clone, Copy)]
pub struct LayerDraw {
    /// Index into the depth queue (0 = nearest/front).
    pub layer: usize,
    /// Render scale about the frame center (near = bigger).
    pub scale: f32,
    /// Horizontal offset in FRAME pixels; the layer moves OPPOSITE the player.
    pub shift_px: f32,
    /// Edge fade-in/out alpha in `[0, 1]`.
    pub alpha: f32,
    /// The layer's slot `s = i - focus` (0 at the front of the window, grows
    /// with depth). Used to sort far → near.
    pub s: f32,
}

/// Compute the visible layers for the current `focus` / `player_pos`, with each
/// layer's scale / horizontal shift / edge-fade alpha, sorted **far → near**.
///
/// This is the spec §4 reference algorithm verbatim (the editor runs the same
/// thing), kept as a free function so it is unit-testable headless with no GPU.
/// `count` is the queue length; empty / invisible layers are NOT culled here
/// (the caller skips them when it has no texture) so the slot math stays a pure
/// function of `focus` / `player_pos`.
pub fn visible_layers(
    focus: f32,
    player_pos: f32,
    count: usize,
    p: &ParallaxParams,
) -> Vec<LayerDraw> {
    let mut out = Vec::new();
    for i in 0..count {
        let s = i as f32 - focus;
        // Cull layers outside the visible band (spec §4): one slot of slack on
        // each end so a layer fades in/out across the boundary.
        if s < -1.0 || s > p.visible {
            continue;
        }
        // Normalized depth across the band, 0 at the near slot .. 1 at far.
        let t = (s / (p.visible - 1.0)).clamp(0.0, 1.0);
        let factor = p.near_factor + (p.far_factor - p.near_factor) * t;
        let scale = p.near_scale + (p.far_scale - p.near_scale) * t;

        // Fade in as a layer enters at the back edge (s -> visible) and out as
        // it exits the front (s < 0).
        let mut alpha = 1.0_f32;
        if s > p.visible - 1.0 {
            alpha *= (p.visible - s).clamp(0.0, 1.0);
        }
        if s < 0.0 {
            alpha *= (1.0 + s).clamp(0.0, 1.0);
        }
        let alpha = alpha.clamp(0.0, 1.0);

        // Horizontal parallax: layers move opposite the player (spec §4 / §6).
        let pos_off = player_pos - p.center_position;
        let shift_px = pos_off * p.step_px * factor;

        out.push(LayerDraw {
            layer: i,
            scale,
            shift_px,
            alpha,
            s,
        });
    }
    // Farthest first so nearer layers cover farther ones (spec §4).
    out.sort_by(|a, b| b.s.partial_cmp(&a.s).unwrap_or(std::cmp::Ordering::Equal));
    out
}

// ---------------------------------------------------------------------------
// Manifest schema (spec §3a). Only the fields the engine consumes are decoded.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Manifest {
    frame: FrameBlock,
    canvas: CanvasBlock,
    queue: QueueBlock,
    parallax: ParallaxBlock,
    layers: Vec<LayerEntry>,
}

#[derive(Debug, Deserialize)]
struct FrameBlock {
    w: u32,
    h: u32,
}

#[derive(Debug, Deserialize)]
struct CanvasBlock {
    w: u32,
    h: u32,
}

#[derive(Debug, Deserialize)]
struct QueueBlock {
    #[serde(rename = "layerCount")]
    layer_count: usize,
    visible: u32,
    #[allow(dead_code)]
    positions: u32,
}

#[derive(Debug, Deserialize)]
struct ParallaxBlock {
    #[serde(rename = "stepPx")]
    step_px: f32,
    #[serde(rename = "nearFactor")]
    near_factor: f32,
    #[serde(rename = "farFactor")]
    far_factor: f32,
    #[serde(rename = "nearScale")]
    near_scale: f32,
    #[serde(rename = "farScale")]
    far_scale: f32,
    #[serde(rename = "centerPosition")]
    center_position: f32,
}

#[derive(Debug, Deserialize)]
struct LayerEntry {
    index: usize,
    #[serde(default)]
    empty: bool,
    #[serde(default)]
    file: Option<String>,
}

/// Error from loading a background manifest. Non-fatal: the caller keeps the
/// solid-ink fallback for any layer it can't load.
#[derive(Debug)]
pub enum BackgroundLoadError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Image(String),
}

impl std::fmt::Display for BackgroundLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackgroundLoadError::Io(e) => write!(f, "background manifest io: {e}"),
            BackgroundLoadError::Json(e) => write!(f, "background manifest json: {e}"),
            BackgroundLoadError::Image(e) => write!(f, "background layer image: {e}"),
        }
    }
}

impl std::error::Error for BackgroundLoadError {}

// ---------------------------------------------------------------------------
// GPU uniform — one per layer per frame: the 4 quad corners (virtual px), the
// px→NDC scale (shared with the sprite/loft views), and the per-draw alpha.
// Mirrors gfx::LoftQuadUniform's corner layout so the math reads the same.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    /// Four virtual-pixel corners: top-left, top-right, bot-right, bot-left.
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    /// 2/VIRTUAL_W, 2/VIRTUAL_H — same px→NDC map as the sprite/polygon view.
    px_to_ndc: [f32; 2],
    /// Per-draw edge-fade alpha; `_pad` keeps the struct 16-byte aligned.
    alpha: f32,
    _pad: f32,
}

// 4×vec2 (32) + vec2 (8) + f32 (4) + f32 pad (4) = 48 bytes. Pinned so a layout
// drift can't silently mismatch the WGSL twin (the late-min-binding-size
// invalid-encoder trap, made a compile error — see gfx.rs for the rationale).
const _: () = assert!(std::mem::size_of::<LayerUniform>() == 48);

const BACKGROUND_SHADER: &str = r#"
struct Layer {
    p0: vec2<f32>,
    p1: vec2<f32>,
    p2: vec2<f32>,
    p3: vec2<f32>,
    px_to_ndc: vec2<f32>,
    alpha: f32,
    _pad: f32,
};
@group(0) @binding(0) var<uniform> layer: Layer;
@group(0) @binding(1) var bg_tex: texture_2d<f32>;
@group(0) @binding(2) var bg_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_bg(@builtin(vertex_index) v_idx: u32) -> VsOut {
    var corner_idx = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let c = corner_idx[v_idx];
    var pixel: vec2<f32>;
    var uv: vec2<f32>;
    // p0 top-left (0,0), p1 top-right (1,0), p2 bot-right (1,1), p3 bot-left (0,1).
    if (c == 0u) { pixel = layer.p0; uv = vec2<f32>(0.0, 0.0); }
    else if (c == 1u) { pixel = layer.p1; uv = vec2<f32>(1.0, 0.0); }
    else if (c == 2u) { pixel = layer.p2; uv = vec2<f32>(1.0, 1.0); }
    else { pixel = layer.p3; uv = vec2<f32>(0.0, 1.0); }
    let ndc_x = pixel.x * layer.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * layer.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_bg(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(bg_tex, bg_samp, in.uv);
    // Straight (non-premultiplied) alpha; the edge fade multiplies the whole
    // texel. ALPHA_BLENDING in the pipeline composites this over farther layers.
    return vec4<f32>(s.rgb, s.a) * vec4<f32>(1.0, 1.0, 1.0, layer.alpha);
}
"#;

/// One depth-queue layer: either a loaded texture or the solid-ink fallback.
/// Both expose a `view` so the draw path is uniform.
struct Layer {
    view: wgpu::TextureView,
}

/// The whole parallax background as one GPU resource (spec §7 ECS shape).
pub struct Background {
    /// One slot per depth index (len = `layer_count`); always populated — a
    /// slot with no painted PNG holds its solid-ink fallback texture.
    layers: Vec<Layer>,
    params: ParallaxParams,

    /// Frame (visible) size and canvas (per-layer paint buffer) size, in virtual
    /// pixels. `canvas_w` is 2× `frame_w` so layers have room to slide (spec §2).
    frame_w: f32,
    frame_h: f32,
    canvas_w: f32,
    canvas_h: f32,

    /// Depth cursor (continuous; tweened toward `focus_target`). 0 = nearest
    /// layer at the front of the window. Wired to `Run.layer` later.
    pub focus: f32,
    pub focus_target: f32,
    /// Horizontal player position (continuous; tweened toward `pos_target`),
    /// 0..(positions-1). Wired to the player's column later.
    pub player_pos: f32,
    pub pos_target: f32,

    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Background {
    /// Build the background with the spec-default parallax params and a 20-slot
    /// queue, every slot filled with its solid-ink fallback. Call
    /// [`Background::load_manifest`] afterward to swap in real art when it
    /// exists. Defaults to a centered, mid-depth view so the fallback shows a
    /// spread of depth slots immediately.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let params = ParallaxParams::default();
        let layer_count = 20usize;

        let (pipeline, bgl, sampler) = build_pipeline(device);

        let layers = (0..layer_count)
            .map(|i| Layer {
                view: fallback_layer(device, queue, i, layer_count),
            })
            .collect();

        // Centered horizontally; focus mid-queue so several fallback bands are
        // visible at once (purely a sensible default for the asset-less state).
        let center = params.center_position;
        Self {
            layers,
            params,
            frame_w: crate::gfx::VIRTUAL_W as f32,
            frame_h: crate::gfx::VIRTUAL_H as f32,
            canvas_w: (crate::gfx::VIRTUAL_W * 2) as f32,
            canvas_h: crate::gfx::VIRTUAL_H as f32,
            focus: 0.0,
            focus_target: 0.0,
            player_pos: center,
            pos_target: center,
            pipeline,
            bgl,
            sampler,
        }
    }

    /// Load `background_manifest.json` from `dir` and swap each non-empty
    /// layer's painted PNG into its depth index. Reads ALL parallax constants
    /// (`frame`/`canvas`/`queue`/`parallax`) from the manifest — never the
    /// hardcoded defaults (spec §2). Layers that are empty or fail to load keep
    /// their solid-ink fallback, so a partial / missing manifest degrades
    /// gracefully rather than blanking the background.
    ///
    /// Returns the count of painted layers actually loaded. A missing manifest
    /// is an `Err` the caller can ignore (the fallback is already in place).
    pub fn load_manifest(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dir: &Path,
    ) -> Result<usize, BackgroundLoadError> {
        let manifest_path = dir.join("background_manifest.json");
        let text = std::fs::read_to_string(&manifest_path).map_err(BackgroundLoadError::Io)?;
        let manifest: Manifest = serde_json::from_str(&text).map_err(BackgroundLoadError::Json)?;

        // Pull the geometry + parallax constants straight from the manifest.
        self.params = ParallaxParams {
            step_px: manifest.parallax.step_px,
            near_factor: manifest.parallax.near_factor,
            far_factor: manifest.parallax.far_factor,
            near_scale: manifest.parallax.near_scale,
            far_scale: manifest.parallax.far_scale,
            center_position: manifest.parallax.center_position,
            visible: manifest.queue.visible as f32,
        };
        self.frame_w = manifest.frame.w as f32;
        self.frame_h = manifest.frame.h as f32;
        self.canvas_w = manifest.canvas.w as f32;
        self.canvas_h = manifest.canvas.h as f32;

        // Resize the slot vector to the manifest's layer count, filling any new
        // slots with fallbacks (so the queue is always fully populated).
        let count = manifest.queue.layer_count;
        if self.layers.len() != count {
            self.layers = (0..count)
                .map(|i| Layer {
                    view: fallback_layer(device, queue, i, count),
                })
                .collect();
        }

        let mut loaded = 0usize;
        for entry in &manifest.layers {
            if entry.index >= self.layers.len() {
                continue;
            }
            if entry.empty {
                continue;
            }
            let Some(file) = entry.file.as_ref() else {
                continue;
            };
            let path = dir.join(file);
            match load_layer_texture(device, queue, &path) {
                Ok(view) => {
                    self.layers[entry.index] = Layer { view };
                    loaded += 1;
                }
                Err(e) => {
                    // Keep the fallback for this slot; log and continue.
                    log::warn!(
                        "background layer {} ({}) failed to load: {e}",
                        entry.index,
                        file
                    );
                }
            }
        }
        Ok(loaded)
    }

    /// Number of layers in the depth queue.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Set the depth target (campaign layer 0..count-1 — `Run.layer`). Clamped
    /// to the queue; the editor authors a fixed stack and does not wrap
    /// (spec §5), so we clamp rather than ring-buffer.
    pub fn set_focus_target(&mut self, layer: usize) {
        let max = self.layers.len().saturating_sub(1) as f32;
        self.focus_target = (layer as f32).clamp(0.0, max);
    }

    /// Set the horizontal target (player column 0..positions-1). Clamped to the
    /// valid position range derived from `center_position`.
    pub fn set_pos_target(&mut self, position: usize) {
        let max = (self.params.center_position * 2.0).max(0.0);
        self.pos_target = (position as f32).clamp(0.0, max);
    }

    /// Ease `focus` / `player_pos` toward their integer targets (spec §5). The
    /// editor uses a fixed-per-frame exponential ease (`x += (t-x)*k` at 60 fps);
    /// we make it `dt`-correct so the feel is frame-rate-independent. `dt` is in
    /// seconds.
    pub fn tween(&mut self, dt: f32) {
        // Per-frame eases from the spec, expressed as a per-second rate so the
        // dt-correct factor `1 - (1-k)^(dt*60)` reproduces the editor at 60 fps.
        const FOCUS_K_60: f32 = 0.18;
        const POS_K_60: f32 = 0.2;
        let focus_k = ease_factor(FOCUS_K_60, dt);
        let pos_k = ease_factor(POS_K_60, dt);
        self.focus += (self.focus_target - self.focus) * focus_k;
        self.player_pos += (self.pos_target - self.player_pos) * pos_k;
    }

    /// Draw the visible layers into `target_view` (the 480×270 offscreen),
    /// far → near, with straight-alpha blending. Call FIRST in the frame, before
    /// any scene content, so the background sits behind everything. `load` lets
    /// the caller choose whether this pass clears the offscreen (it is normally
    /// the first writer, so it clears to the deep-space ink behind layer 19).
    ///
    /// One render pass; one quad per visible non-fallback-culled layer. The
    /// uniform buffer is rewritten and the draw submitted per layer (≤5), which
    /// is cheap at this layer count and keeps the draw ordering exact.
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        clear: Option<wgpu::Color>,
    ) {
        let draws = visible_layers(self.focus, self.player_pos, self.layers.len(), &self.params);

        let px_to_ndc = [2.0 / self.frame_w, 2.0 / self.frame_h];
        let frame_cx = self.frame_w * 0.5;
        let frame_cy = self.frame_h * 0.5;

        // Pre-build a bind group + uniform value per visible layer. We can't
        // reuse a single UBO across draws within one pass (the writes would all
        // land before the pass executes), so allocate a small uniform + bind
        // group per visible layer for this frame. At ≤5 layers this is trivial.
        // (#137 Bruce TEMP) De-clutter: the nearest N layers are the LAST N of the
        // far→near `draws`. Draw only the rest. Layer system intact (textures load,
        // queue full); this just skips the front pair. `BLANK_NEAR_LAYERS = 0` draws
        // all; flip to `&draws[BLANK_NEAR_LAYERS..]` if Bruce meant the FAR pair.
        let keep_to = draws.len().saturating_sub(BLANK_NEAR_LAYERS);
        let mut per_layer: Vec<(wgpu::Buffer, wgpu::BindGroup)> = Vec::with_capacity(draws.len());
        for d in &draws[..keep_to] {
            // Half-extent of the scaled 960×270 canvas about the frame center.
            let half_w = self.canvas_w * d.scale * 0.5;
            let half_h = self.canvas_h * d.scale * 0.5;
            // Center the canvas on the frame center, then slide by -shift_px
            // (layers move opposite the player — the sign is already in shift_px).
            let cx = frame_cx - d.shift_px;
            let left = cx - half_w;
            let right = cx + half_w;
            let top = frame_cy - half_h;
            let bottom = frame_cy + half_h;

            let uni = LayerUniform {
                p0: [left, top],
                p1: [right, top],
                p2: [right, bottom],
                p3: [left, bottom],
                px_to_ndc,
                alpha: d.alpha,
                _pad: 0.0,
            };
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bg layer uniform"),
                size: std::mem::size_of::<LayerUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buf, 0, bytemuck::bytes_of(&uni));
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bg layer bind group"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.layers[d.layer].view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            per_layer.push((buf, bg));
        }

        let load = match clear {
            Some(c) => wgpu::LoadOp::Clear(c),
            None => wgpu::LoadOp::Load,
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("background layers"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        for (_, bg) in &per_layer {
            pass.set_bind_group(0, bg, &[]);
            pass.draw(0..6, 0..1);
        }
    }
}

/// `dt`-correct exponential-ease factor reproducing a per-frame ease of `k_60`
/// at 60 fps. At `dt = 1/60` this returns `k_60`; for other `dt` it returns
/// `1 - (1 - k_60)^(dt * 60)` so the approach speed is frame-rate independent.
fn ease_factor(k_60: f32, dt: f32) -> f32 {
    let frames = dt * 60.0;
    1.0 - (1.0 - k_60).powf(frames)
}

/// Build the background render pipeline + bind-group layout + sampler. NEAREST
/// sampler everywhere (spec §6) for crisp pixels; ALPHA_BLENDING so layers
/// composite straight-alpha far → near.
fn build_pipeline(
    device: &wgpu::Device,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Sampler) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("background shader"),
        source: wgpu::ShaderSource::Wgsl(BACKGROUND_SHADER.into()),
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("background nearest sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("background bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("background layout"),
        bind_group_layouts: &[&bgl],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("background pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_bg"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_bg"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: LAYER_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    (pipeline, bgl, sampler)
}

/// Generate the solid-ink fallback texture for depth slot `i` of `count`: a 1×1
/// RGBA texel whose colour is a distinct flat tint per depth, ramping from a
/// near (front) tone to a far (back) tone. With nearest sampling this fills the
/// layer quad with a flat band, so the asset-less background reads as a spread
/// of depth-tinted slabs that slide / scale / fade like real layers — enough to
/// test the parallax math + gameplay wiring before the painted PNGs exist.
fn fallback_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    i: usize,
    count: usize,
) -> wgpu::TextureView {
    let rgba = fallback_color(i, count);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bg fallback layer"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: LAYER_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The distinct flat tint for depth slot `i` of `count` (sRGB RGBA bytes). A
/// deep-space ramp: near layers are a cool slate that fades toward a darker,
/// blue-violet deep void at the back, with the hue nudged per index so adjacent
/// slots are visibly distinct (not just a smooth gradient). Fully opaque — the
/// per-draw edge-fade alpha handles the window fade-in/out.
fn fallback_color(i: usize, count: usize) -> [u8; 4] {
    let denom = (count.max(2) - 1) as f32;
    let t = (i as f32 / denom).clamp(0.0, 1.0); // 0 near .. 1 far
                                                // Near tone (#3a4660 cool slate) -> far tone (#0a0e1c deep void).
    let near = [0.227_f32, 0.275, 0.376];
    let far = [0.039_f32, 0.055, 0.110];
    let mut rgb = [
        near[0] + (far[0] - near[0]) * t,
        near[1] + (far[1] - near[1]) * t,
        near[2] + (far[2] - near[2]) * t,
    ];
    // Per-index hue nudge so neighbouring slabs read as separate bands: a small
    // alternating warm/cool tint keyed on the index parity + a sweep on blue.
    let nudge = ((i % 4) as f32 / 3.0 - 0.5) * 0.06;
    rgb[0] = (rgb[0] - nudge).clamp(0.0, 1.0);
    rgb[2] = (rgb[2] + nudge).clamp(0.0, 1.0);
    [
        (rgb[0] * 255.0).round() as u8,
        (rgb[1] * 255.0).round() as u8,
        (rgb[2] * 255.0).round() as u8,
        255,
    ]
}

/// Decode a painted layer PNG (canvas.w × canvas.h, RGBA straight alpha) into a
/// GPU texture and return its view. Used by [`Background::load_manifest`] when
/// real art exists.
fn load_layer_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    path: &Path,
) -> Result<wgpu::TextureView, BackgroundLoadError> {
    let bytes = std::fs::read(path).map_err(BackgroundLoadError::Io)?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| BackgroundLoadError::Image(e.to_string()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bg layer"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: LAYER_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    Ok(tex.create_view(&wgpu::TextureViewDescriptor::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> ParallaxParams {
        ParallaxParams::default()
    }

    #[test]
    fn slot_zero_focus_front_layer_is_near_full() {
        // focus=0, centered player: layer 0 sits at the near slot (s=0), full
        // scale, full alpha, no shift.
        let v = visible_layers(0.0, 2.0, 20, &p());
        let near = v.iter().find(|d| d.layer == 0).expect("layer 0 visible");
        assert!((near.s - 0.0).abs() < 1e-6);
        assert!((near.scale - 1.0).abs() < 1e-6);
        assert!((near.alpha - 1.0).abs() < 1e-6);
        assert!((near.shift_px - 0.0).abs() < 1e-6);
    }

    #[test]
    fn culls_outside_visible_band() {
        // With visible=5, only slots in (-1, 5] survive. At focus=0 that's
        // layers 0..=5 (s = 0..5); layer 6 (s=6) is culled.
        let v = visible_layers(0.0, 2.0, 20, &p());
        assert!(v.iter().any(|d| d.layer == 5));
        assert!(!v.iter().any(|d| d.layer == 6));
        // Window is at most visible+2 entries (one slack slot each side).
        assert!(v.len() <= 7);
    }

    #[test]
    fn sorted_far_to_near() {
        let v = visible_layers(2.5, 2.0, 20, &p());
        for w in v.windows(2) {
            assert!(w[0].s >= w[1].s, "must be sorted far->near by s desc");
        }
    }

    #[test]
    fn far_slot_smaller_and_weaker_parallax() {
        // At focus=0, layer 4 is the far slot (s=4 == visible-1, t=1).
        let v = visible_layers(0.0, 2.0, 20, &p());
        let far = v.iter().find(|d| d.layer == 4).unwrap();
        assert!((far.scale - p().far_scale).abs() < 1e-6);
        // Off-center player: far layer shifts far_factor× the near layer's shift.
        let v2 = visible_layers(0.0, 4.0, 20, &p());
        let near = v2.iter().find(|d| d.layer == 0).unwrap();
        let far = v2.iter().find(|d| d.layer == 4).unwrap();
        assert!(far.shift_px.abs() < near.shift_px.abs());
        // Near layer at extreme position slides 2*stepPx = 240 px (spec §2).
        assert!((near.shift_px.abs() - 240.0).abs() < 1e-3);
    }

    #[test]
    fn layers_move_opposite_player() {
        // Spec §4: `shift_px = pos_off * stepPx * factor` (NOT yet negated —
        // the placement applies `-shift_px`). So for player RIGHT of center,
        // `pos_off > 0` ⇒ `shift_px > 0`, and the layer's final x is
        // `frame_cx - shift_px` (to the LEFT — opposite the player). We assert
        // the spec's pre-negation sign here; the negation lives in `draw`.
        let right = visible_layers(0.0, 4.0, 20, &p());
        let near_r = right.iter().find(|d| d.layer == 0).unwrap();
        assert!(near_r.shift_px > 0.0, "player right ⇒ shift_px>0 (spec §4)");

        // Player LEFT of center ⇒ pos_off < 0 ⇒ shift_px < 0 ⇒ final x to the
        // right (opposite the player).
        let left = visible_layers(0.0, 0.0, 20, &p());
        let near_l = left.iter().find(|d| d.layer == 0).unwrap();
        assert!(near_l.shift_px < 0.0, "player left ⇒ shift_px<0 (spec §4)");

        // Centered player ⇒ no shift.
        let center = visible_layers(0.0, 2.0, 20, &p());
        let near_c = center.iter().find(|d| d.layer == 0).unwrap();
        assert!(near_c.shift_px.abs() < 1e-6);
    }

    #[test]
    fn edge_fade_in_and_out() {
        // A layer at the back edge (s just under visible) fades toward 0.
        let v = visible_layers(-0.5, 2.0, 20, &p()); // layer 0 at s=0.5..; layer where s>4 fades
                                                     // Construct a focus that puts layer 0 at the exiting-front edge (s<0).
        let exiting = visible_layers(0.5, 2.0, 20, &p());
        let l0 = exiting.iter().find(|d| d.layer == 0).unwrap();
        assert!(l0.s < 0.0 && l0.alpha < 1.0 && l0.alpha > 0.0);
        let _ = v;
    }

    #[test]
    fn ease_factor_matches_editor_at_60fps() {
        // At dt=1/60 the dt-correct factor equals the editor's per-frame k.
        assert!((ease_factor(0.18, 1.0 / 60.0) - 0.18).abs() < 1e-6);
        assert!((ease_factor(0.2, 1.0 / 60.0) - 0.2).abs() < 1e-6);
        // Larger dt → faster approach (bigger factor), still < 1.
        let f = ease_factor(0.18, 1.0 / 30.0);
        assert!(f > 0.18 && f < 1.0);
    }

    #[test]
    fn fallback_colors_distinct_per_slot() {
        // Adjacent slots differ (so the asset-less depth queue is readable).
        for i in 0..19 {
            assert_ne!(
                fallback_color(i, 20),
                fallback_color(i + 1, 20),
                "fallback slots {i} and {} must differ",
                i + 1
            );
        }
    }

    /// A spec **v1.0** manifest carries new top-level `sprites[]` / `placements[]`
    /// blocks (BACKGROUND_SPEC_v1.0 §0/§9) that the engine does not consume here.
    /// The `Manifest` deserializer must IGNORE those unknown fields (no
    /// `deny_unknown_fields`) so a v1.0 bundle still loads its parallax + layers
    /// — the layer background renders even before the §9 animated-sprite path
    /// (task D7) exists. This pins that forward-compatibility so a future
    /// `deny_unknown_fields` can't silently break v1.0 manifests.
    #[test]
    fn v1_manifest_with_sprites_and_placements_still_deserializes() {
        let json = r#"{
            "format": "broadside-background-manifest",
            "v": 2,
            "frame":  { "w": 480, "h": 270, "upscale": 4 },
            "canvas": { "w": 960, "h": 270 },
            "queue":  { "layerCount": 20, "visible": 5, "positions": 5 },
            "parallax": {
                "stepPx": 120,
                "nearFactor": 1.0, "farFactor": 0.18,
                "nearScale": 1.0,  "farScale": 0.62,
                "centerPosition": 2
            },
            "layers": [
                { "index": 0, "name": "starfield bright", "visible": true, "empty": false, "file": "bg_layer_00_starfield_bright.png" },
                { "index": 1, "name": "empty slot",       "visible": true, "empty": true,  "file": null }
            ],
            "sprites": [
                { "id": "spr_ab12cd3", "name": "beacon", "w": 32, "h": 32, "fps": 8, "frameCount": 4,
                  "strip": "sprite_beacon_spr_ab12cd3.png" }
            ],
            "placements": [
                { "spriteId": "spr_ab12cd3", "layer": 9, "x": 480, "y": 135, "scale": 1.0, "vx": 12, "vy": 0, "loop": "wrap" }
            ]
        }"#;

        let m: Manifest = serde_json::from_str(json).expect("v1.0 manifest must deserialize");
        // The fields the engine DOES read survive the unknown-field ignore.
        assert_eq!(m.frame.w, 480);
        assert_eq!(m.frame.h, 270);
        assert_eq!(m.canvas.w, 960);
        assert_eq!(m.queue.layer_count, 20);
        assert_eq!(m.queue.visible, 5);
        assert_eq!((m.parallax.step_px, m.parallax.far_scale), (120.0, 0.62));
        assert_eq!(m.layers.len(), 2);
        assert!(!m.layers[0].empty && m.layers[0].file.is_some());
        assert!(m.layers[1].empty && m.layers[1].file.is_none());
    }
}
