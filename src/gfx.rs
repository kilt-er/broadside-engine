//! wgpu state, instanced sprite batcher, and virtual-resolution presentation.
//!
//! Ported from `GameEngine/mvp/src/gfx.rs` and adapted for Broadside.
//! Structural changes from the source:
//!
//! 1. Virtual resolution is **1320×480** (2× of the design doc's 660×240).
//!    Integer-scales cleanly on a 2560×1440 monitor (1× and 2×); keeps the
//!    `perspective::DEFAULT_LANE` coordinates usable after a uniform 2× map.
//! 2. The view uniform projects ONTO the virtual-pixel grid: world is
//!    `[0, VIRTUAL_W] × [0, VIRTUAL_H]` with y-down to match `perspective`'s
//!    screen-space convention. The source engine used a NDC-half-size world;
//!    the Broadside renderer feeds raw pixel coordinates from
//!    [`crate::perspective`] straight through.
//! 3. The atlas comes from [`crate::atlas`] (Broadside content) rather than
//!    the GameEngine humanoid set.
//! 4. The clear color is deep-space ink (`#080c14`), matching the analysis
//!    HTML's `--ink` token.
//!
//! Two passes per frame, unchanged in spirit from the source:
//!
//!   1. **Sprite pass** — instanced colored quads drawn into the 1320×480
//!      offscreen target. Every game pixel is one texel here.
//!   2. **Blit pass** — the offscreen texture is sampled with
//!      nearest-neighbor filtering and drawn to the swapchain at the largest
//!      integer scale that fits the window. The leftover area is letterboxed.
//!
//! Sprite content (the actual `Vec<SpriteInstance>` for a frame) lives in
//! [`crate::hud`]; this module is the pipeline scaffold only.

use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::atlas;

/// Virtual canvas size — every drawn sprite is in this coordinate space.
/// `2 × 660 × 240` for crisp 2× scaling of the perspective design coords.
pub const VIRTUAL_W: u32 = 1320;
pub const VIRTUAL_H: u32 = 480;

/// Maximum sprite instances in a frame. 4096 covers a worst-case scene
/// (lane plate + parallax + 9 ships × ~8 composed sprites + ordnance +
/// HUD glyphs) with generous headroom. Bumping this only costs one VRAM
/// allocation; the buffer is reused frame-to-frame.
const MAX_SPRITES: u64 = 4096;

/// Maximum polygon instances in a frame. A 9-cell lane plate plus 9 ships ×
/// 2 face polygons = ~27. 256 is plenty.
const MAX_POLYGONS: u64 = 256;

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Deep-space ink — matches the analysis HTML `--ink` token (`#080c14`).
/// Values are pre-converted to linear space because `OFFSCREEN_FORMAT` is
/// sRGB and `wgpu::Color` is interpreted linearly when the target is sRGB.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.001214,
    g: 0.002428,
    b: 0.006995,
    a: 1.0,
};
const LETTERBOX: wgpu::Color = wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVertex {
    pos: [f32; 2],
}

const QUAD_VERTS: [QuadVertex; 6] = [
    QuadVertex { pos: [-1.0, -1.0] },
    QuadVertex { pos: [ 1.0, -1.0] },
    QuadVertex { pos: [ 1.0,  1.0] },
    QuadVertex { pos: [-1.0, -1.0] },
    QuadVertex { pos: [ 1.0,  1.0] },
    QuadVertex { pos: [-1.0,  1.0] },
];

/// One drawable rectangle in virtual-pixel space. Position is the rectangle's
/// CENTER; `half_size` is half-width / half-height. `color` multiplies the
/// sampled atlas texel (use `1.0,1.0,1.0,1.0` for "no tint" or sample the
/// solid-white atlas cell to render a flat-color quad). UVs select the atlas
/// cell.
///
/// `rotation_rad` rotates the quad around its center. For axis-aligned HUD
/// elements pass `0.0`; for lane-slope-aligned sprites use
/// `perspective::cell_to_screen` rotation. The rotation pivot is the
/// instance's `pos`, not an external pivot — composed sprites whose pivot is
/// elsewhere (e.g. ships rotated about their base) precompute the rotated
/// vertex positions on the CPU and pass `rotation_rad: 0.0`.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpriteInstance {
    pub pos: [f32; 2],
    pub half_size: [f32; 2],
    pub color: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub rotation_rad: f32,
    pub _pad: [f32; 3],
}

impl SpriteInstance {
    /// Convenience for the common axis-aligned case.
    pub fn axis_aligned(
        pos: [f32; 2],
        half_size: [f32; 2],
        color: [f32; 4],
        uv: ([f32; 2], [f32; 2]),
    ) -> Self {
        Self {
            pos,
            half_size,
            color,
            uv_min: uv.0,
            uv_max: uv.1,
            rotation_rad: 0.0,
            _pad: [0.0; 3],
        }
    }
}

/// A quad defined by four explicit virtual-pixel corner positions in CCW
/// (with screen y-down: top-left, top-right, bottom-right, bottom-left).
/// Used for shapes the rotation-around-center `SpriteInstance` cannot
/// represent without pixel staircase, primarily the lane plate
/// parallelograms and ship-face polygons under forced perspective.
///
/// The fragment shader still samples the atlas times the color tint, so a
/// flat-color polygon uses `atlas::SOLID_WHITE`'s UVs and the desired
/// color, while a textured polygon can sample any atlas cell.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PolygonInstance {
    /// Top-left corner.
    pub p0: [f32; 2],
    /// Top-right corner.
    pub p1: [f32; 2],
    /// Bottom-right corner.
    pub p2: [f32; 2],
    /// Bottom-left corner.
    pub p3: [f32; 2],
    pub color: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
}

impl PolygonInstance {
    /// Build a polygon from a `[Point2-shaped; 4]` corner array using the
    /// SOLID_WHITE atlas cell so the `color` field is the visible tint.
    /// Caller supplies the SOLID_WHITE uv rect to keep this module
    /// decoupled from `crate::atlas`.
    pub fn flat(corners: [[f32; 2]; 4], color: [f32; 4], solid_white_uv: ([f32; 2], [f32; 2])) -> Self {
        Self {
            p0: corners[0],
            p1: corners[1],
            p2: corners[2],
            p3: corners[3],
            color,
            uv_min: solid_white_uv.0,
            uv_max: solid_white_uv.1,
        }
    }
}

/// A draw command in back-to-front order. `Gfx::render` batches consecutive
/// same-variant runs into single draw calls.
#[derive(Copy, Clone, Debug)]
pub enum DrawCommand {
    Sprite(SpriteInstance),
    Polygon(PolygonInstance),
}

impl From<SpriteInstance> for DrawCommand {
    fn from(s: SpriteInstance) -> Self { DrawCommand::Sprite(s) }
}

impl From<PolygonInstance> for DrawCommand {
    fn from(p: PolygonInstance) -> Self { DrawCommand::Polygon(p) }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniform {
    /// 2.0 / VIRTUAL_W, 2.0 / VIRTUAL_H. Multiplying a virtual-pixel position
    /// by this gives NDC half-extent; subtracting 1.0 maps to [-1, 1]. Y is
    /// flipped in the shader so we feed y-down pixel coords directly.
    px_to_ndc: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitUniform {
    ndc_min: [f32; 2],
    ndc_max: [f32; 2],
}

const SPRITE_SHADER: &str = r#"
struct ViewUniform {
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv:    vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) v_pos:        vec2<f32>,
    @location(1) i_pos:        vec2<f32>,
    @location(2) i_half:       vec2<f32>,
    @location(3) i_color:      vec4<f32>,
    @location(4) i_uv_min:     vec2<f32>,
    @location(5) i_uv_max:     vec2<f32>,
    @location(6) i_rotation:   f32,
) -> VsOut {
    // Rotate the local quad vertex around the instance center.
    let cos_r = cos(i_rotation);
    let sin_r = sin(i_rotation);
    let local = v_pos * i_half;
    let rotated = vec2<f32>(
        local.x * cos_r - local.y * sin_r,
        local.x * sin_r + local.y * cos_r,
    );
    // Translate to virtual-pixel position, then map to NDC. Y is flipped so
    // virtual-pixel (0, 0) is the top-left corner of the offscreen, matching
    // the screen-space convention of perspective::cell_to_screen.
    let pixel = i_pos + rotated;
    let ndc_x = pixel.x * view.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * view.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.color = i_color;
    // Map v_pos in [-1, 1] to uv in [uv_min, uv_max]. Y flip so the top of
    // the atlas cell shows at the top of the quad in screen space (the quad's
    // top in local frame is +y, which after Y flip lands at screen-top).
    let t = (v_pos + vec2<f32>(1.0, 1.0)) * 0.5;
    o.uv = vec2<f32>(
        mix(i_uv_min.x, i_uv_max.x, t.x),
        mix(i_uv_max.y, i_uv_min.y, t.y)
    );
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(atlas, atlas_samp, in.uv);
    return s * in.color;
}
"#;

// Polygon shader. Each instance has four explicit corner positions; we expand
// to 6 vertices (two triangles, indices 0-1-2 and 0-2-3) using
// vertex_index % 6. The UV is barycentrically blended across the polygon
// from uv_min (top-left) to uv_max (bottom-right) in the polygon's own
// corner frame — so a textured polygon samples its full atlas cell across
// its quad, with the same y-flip convention as SpriteInstance.
const POLYGON_SHADER: &str = r#"
struct ViewUniform {
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv:    vec2<f32>,
};

@vertex
fn vs_poly(
    @builtin(vertex_index) v_idx: u32,
    @location(0) i_p0:     vec2<f32>,
    @location(1) i_p1:     vec2<f32>,
    @location(2) i_p2:     vec2<f32>,
    @location(3) i_p3:     vec2<f32>,
    @location(4) i_color:  vec4<f32>,
    @location(5) i_uv_min: vec2<f32>,
    @location(6) i_uv_max: vec2<f32>,
) -> VsOut {
    // Two triangles: 0-1-2 and 0-2-3.
    var corner_idx = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let c = corner_idx[v_idx];
    var pixel: vec2<f32>;
    var uv:    vec2<f32>;
    // p0 = top-left   -> uv (uv_min.x, uv_max.y)  (Y-flip mirrors SpriteInstance)
    // p1 = top-right  -> uv (uv_max.x, uv_max.y)
    // p2 = bot-right  -> uv (uv_max.x, uv_min.y)
    // p3 = bot-left   -> uv (uv_min.x, uv_min.y)
    if (c == 0u) { pixel = i_p0; uv = vec2<f32>(i_uv_min.x, i_uv_max.y); }
    else if (c == 1u) { pixel = i_p1; uv = vec2<f32>(i_uv_max.x, i_uv_max.y); }
    else if (c == 2u) { pixel = i_p2; uv = vec2<f32>(i_uv_max.x, i_uv_min.y); }
    else { pixel = i_p3; uv = vec2<f32>(i_uv_min.x, i_uv_min.y); }

    let ndc_x = pixel.x * view.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * view.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.color = i_color;
    o.uv = uv;
    return o;
}

@fragment
fn fs_poly(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(atlas, atlas_samp, in.uv);
    return s * in.color;
}
"#;

const BLIT_SHADER: &str = r#"
struct BlitUniform {
    ndc_min: vec2<f32>,
    ndc_max: vec2<f32>,
};
@group(0) @binding(0) var<uniform> blit: BlitUniform;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) idx: u32) -> VsOut {
    var verts = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let v = verts[idx];
    let x = mix(blit.ndc_min.x, blit.ndc_max.x, v.x);
    let y = mix(blit.ndc_min.y, blit.ndc_max.y, v.y);
    var o: VsOut;
    o.clip = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>(v.x, 1.0 - v.y);
    return o;
}

@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src_tex, src_samp, in.uv);
}
"#;

/// wgpu state owner. Builds the surface, virtual-res offscreen target, the
/// procedural atlas texture, and all render pipelines on `new`. Renders one
/// frame on `render` given a pre-built draw command list.
pub struct Gfx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    offscreen_view: wgpu::TextureView,
    sprites: SpritePipeline,
    polygons: PolygonPipeline,
    blit: BlitPipeline,
}

struct SpritePipeline {
    pipeline: wgpu::RenderPipeline,
    quad_vbuf: wgpu::Buffer,
    instance_vbuf: wgpu::Buffer,
    view_ubo: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct PolygonPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_vbuf: wgpu::Buffer,
    // view uniform and atlas bind group are shared with SpritePipeline at
    // bind point 0 — same bgl layout, just bound from this pipeline's
    // perspective. The bind group is owned by SpritePipeline; we keep a
    // reference at construction time via shared underlying resources.
    bind_group: wgpu::BindGroup,
}

struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    ubo: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl Gfx {
    /// Async constructor — opens the wgpu device, uploads the atlas, builds
    /// both pipelines, and configures the swapchain to the window's current
    /// size. Call once at startup; thereafter use [`Gfx::resize`] on window
    /// resize and [`Gfx::render`] each frame.
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request adapter");

        log::info!("adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("primary device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let offscreen = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen virtual-res target"),
            size: wgpu::Extent3d {
                width: VIRTUAL_W,
                height: VIRTUAL_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let offscreen_view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());

        let atlas_data = atlas::generate_atlas();
        let atlas_size = wgpu::Extent3d {
            width: atlas::ATLAS_SIZE,
            height: atlas::ATLAS_SIZE,
            depth_or_array_layers: 1,
        };
        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprite atlas"),
            size: atlas_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas::ATLAS_SIZE * 4),
                rows_per_image: Some(atlas::ATLAS_SIZE),
            },
            atlas_size,
        );
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let sprites = SpritePipeline::new(&device, &atlas_view, &atlas_sampler);
        let polygons = PolygonPipeline::new(&device, &sprites.view_ubo, &atlas_view, &atlas_sampler);
        let blit = BlitPipeline::new(&device, format, &offscreen_view);

        let g = Self {
            surface,
            device,
            queue,
            config,
            offscreen_view,
            sprites,
            polygons,
            blit,
        };

        let view = ViewUniform {
            px_to_ndc: [2.0 / VIRTUAL_W as f32, 2.0 / VIRTUAL_H as f32],
            _pad: [0.0, 0.0],
        };
        g.queue.write_buffer(&g.sprites.view_ubo, 0, bytemuck::bytes_of(&view));

        g.update_blit_uniform();
        g
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
            self.update_blit_uniform();
        }
    }

    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
        self.update_blit_uniform();
    }

    /// Compute the integer-scaled, letterboxed NDC quad that maps the
    /// virtual-resolution offscreen target into the swapchain. Recomputed on
    /// every resize so the letterboxing tracks window changes.
    fn update_blit_uniform(&self) {
        let w = self.config.width;
        let h = self.config.height;
        let scale = (w / VIRTUAL_W).min(h / VIRTUAL_H).max(1);
        let scaled_w = VIRTUAL_W * scale;
        let scaled_h = VIRTUAL_H * scale;
        let offset_x = (w - scaled_w) / 2;
        let offset_y = (h - scaled_h) / 2;

        let wf = w as f32;
        let hf = h as f32;

        let ndc_x_min = (offset_x as f32 / wf) * 2.0 - 1.0;
        let ndc_x_max = ((offset_x + scaled_w) as f32 / wf) * 2.0 - 1.0;
        let ndc_y_max = 1.0 - (offset_y as f32 / hf) * 2.0;
        let ndc_y_min = 1.0 - ((offset_y + scaled_h) as f32 / hf) * 2.0;

        let blit = BlitUniform {
            ndc_min: [ndc_x_min, ndc_y_min],
            ndc_max: [ndc_x_max, ndc_y_max],
        };
        self.queue.write_buffer(&self.blit.ubo, 0, bytemuck::bytes_of(&blit));
    }

    /// Render one frame. `commands` is the full draw command list in
    /// back-to-front order; the scene compositor in [`crate::hud`] builds it.
    /// Consecutive same-variant commands are batched into a single GPU draw.
    /// Sprites and polygons each have their own buffer, sized once at startup
    /// ([`MAX_SPRITES`] / [`MAX_POLYGONS`]).
    pub fn render(&mut self, commands: &[DrawCommand]) -> Result<(), wgpu::SurfaceError> {
        // Walk the commands once, splitting into contiguous batches by variant.
        // For each batch we record (kind, offset, count) where offset is the
        // index into the per-variant buffer the batch occupies.
        let mut sprite_buf: Vec<SpriteInstance> = Vec::with_capacity(commands.len());
        let mut polygon_buf: Vec<PolygonInstance> = Vec::with_capacity(commands.len());
        // Each batch references one of the two buffers.
        enum BatchKind {
            Sprite,
            Polygon,
        }
        struct Batch {
            kind: BatchKind,
            start: u32,
            count: u32,
        }
        let mut batches: Vec<Batch> = Vec::new();
        for cmd in commands {
            match cmd {
                DrawCommand::Sprite(s) => {
                    if (sprite_buf.len() as u64) >= MAX_SPRITES {
                        // Truncate silently; warn once outside the loop.
                        continue;
                    }
                    let start = sprite_buf.len() as u32;
                    sprite_buf.push(*s);
                    match batches.last_mut() {
                        Some(b) if matches!(b.kind, BatchKind::Sprite) => b.count += 1,
                        _ => batches.push(Batch { kind: BatchKind::Sprite, start, count: 1 }),
                    }
                }
                DrawCommand::Polygon(p) => {
                    if (polygon_buf.len() as u64) >= MAX_POLYGONS {
                        continue;
                    }
                    let start = polygon_buf.len() as u32;
                    polygon_buf.push(*p);
                    match batches.last_mut() {
                        Some(b) if matches!(b.kind, BatchKind::Polygon) => b.count += 1,
                        _ => batches.push(Batch { kind: BatchKind::Polygon, start, count: 1 }),
                    }
                }
            }
        }
        if (commands.len() as u64) > MAX_SPRITES + MAX_POLYGONS {
            log::warn!(
                "draw command count {} exceeds MAX_SPRITES+MAX_POLYGONS {}; truncating",
                commands.len(),
                MAX_SPRITES + MAX_POLYGONS
            );
        }

        if !sprite_buf.is_empty() {
            self.queue.write_buffer(
                &self.sprites.instance_vbuf,
                0,
                bytemuck::cast_slice(&sprite_buf),
            );
        }
        if !polygon_buf.is_empty() {
            self.queue.write_buffer(
                &self.polygons.instance_vbuf,
                0,
                bytemuck::cast_slice(&polygon_buf),
            );
        }

        let frame = self.surface.get_current_texture()?;
        let swap_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            });

        // Pass 1: walk batches in order; switch pipelines as the variant
        // changes. One render pass, clear once.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene to offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.offscreen_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            for b in &batches {
                match b.kind {
                    BatchKind::Sprite => {
                        pass.set_pipeline(&self.sprites.pipeline);
                        pass.set_bind_group(0, &self.sprites.bind_group, &[]);
                        pass.set_vertex_buffer(0, self.sprites.quad_vbuf.slice(..));
                        pass.set_vertex_buffer(1, self.sprites.instance_vbuf.slice(..));
                        pass.draw(0..6, b.start..(b.start + b.count));
                    }
                    BatchKind::Polygon => {
                        pass.set_pipeline(&self.polygons.pipeline);
                        pass.set_bind_group(0, &self.polygons.bind_group, &[]);
                        pass.set_vertex_buffer(0, self.polygons.instance_vbuf.slice(..));
                        pass.draw(0..6, b.start..(b.start + b.count));
                    }
                }
            }
        }

        // Pass 2: blit offscreen → swapchain with integer-scale letterboxing.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit offscreen to swap"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &swap_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(LETTERBOX),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.blit.pipeline);
            pass.set_bind_group(0, &self.blit.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}

impl SpritePipeline {
    fn new(
        device: &wgpu::Device,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite shader"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_SHADER.into()),
        });

        let view_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view uniform"),
            size: std::mem::size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<QuadVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        }],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SpriteInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            wgpu::VertexAttribute { shader_location: 1, offset: 0,  format: wgpu::VertexFormat::Float32x2 },
                            wgpu::VertexAttribute { shader_location: 2, offset: 8,  format: wgpu::VertexFormat::Float32x2 },
                            wgpu::VertexAttribute { shader_location: 3, offset: 16, format: wgpu::VertexFormat::Float32x4 },
                            wgpu::VertexAttribute { shader_location: 4, offset: 32, format: wgpu::VertexFormat::Float32x2 },
                            wgpu::VertexAttribute { shader_location: 5, offset: 40, format: wgpu::VertexFormat::Float32x2 },
                            wgpu::VertexAttribute { shader_location: 6, offset: 48, format: wgpu::VertexFormat::Float32   },
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
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

        let quad_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad vbuf"),
            contents: bytemuck::cast_slice(&QUAD_VERTS),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let instance_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instance vbuf"),
            size: (std::mem::size_of::<SpriteInstance>() as u64) * MAX_SPRITES,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, quad_vbuf, instance_vbuf, view_ubo, bind_group }
    }
}

impl PolygonPipeline {
    /// Build the polygon pipeline. Shares the sprite pipeline's view uniform
    /// and atlas texture/sampler — same bind group layout, same resources,
    /// just bound from this pipeline's perspective. The vertex buffer layout
    /// describes a single `PolygonInstance` (no separate quad vertex buffer
    /// — corners come from the instance directly).
    fn new(
        device: &wgpu::Device,
        view_ubo: &wgpu::Buffer,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("polygon shader"),
            source: wgpu::ShaderSource::Wgsl(POLYGON_SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polygon bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polygon bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: view_ubo.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(atlas_sampler) },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("polygon layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("polygon pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_poly"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PolygonInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute { shader_location: 0, offset: 0,  format: wgpu::VertexFormat::Float32x2 }, // p0
                        wgpu::VertexAttribute { shader_location: 1, offset: 8,  format: wgpu::VertexFormat::Float32x2 }, // p1
                        wgpu::VertexAttribute { shader_location: 2, offset: 16, format: wgpu::VertexFormat::Float32x2 }, // p2
                        wgpu::VertexAttribute { shader_location: 3, offset: 24, format: wgpu::VertexFormat::Float32x2 }, // p3
                        wgpu::VertexAttribute { shader_location: 4, offset: 32, format: wgpu::VertexFormat::Float32x4 }, // color
                        wgpu::VertexAttribute { shader_location: 5, offset: 48, format: wgpu::VertexFormat::Float32x2 }, // uv_min
                        wgpu::VertexAttribute { shader_location: 6, offset: 56, format: wgpu::VertexFormat::Float32x2 }, // uv_max
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_poly"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
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

        let instance_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polygon instance vbuf"),
            size: (std::mem::size_of::<PolygonInstance>() as u64) * MAX_POLYGONS,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self { pipeline, instance_vbuf, bind_group }
    }
}

impl BlitPipeline {
    fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        src_view: &wgpu::TextureView,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit uniform"),
            size: std::mem::size_of::<BlitUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Nearest-neighbor for pixel-art crispness on the integer-scale blit.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubo.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_blit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blit"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
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

        Self { pipeline, ubo, bind_group }
    }
}
