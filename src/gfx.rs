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

/// Maximum textured-ship draws per frame. Each consumes one tiny uniform
/// buffer + one cached bind group. 16 covers the maximum 9-cell lane with
/// headroom.
const MAX_TEXTURED_SHIPS: usize = 16;

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

/// Inline-storage slug identifying a loaded ship sprite. `Copy`, no heap
/// allocation — `DrawCommand` is `Copy` and the renderer batches commands
/// frame-to-frame, so we can't hold a `String`. Truncates silently at 31
/// bytes (every legal slug is < 32 bytes — see `SPRITE_SPEC.md`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpriteSlug {
    bytes: [u8; 32],
    len: u8,
}

impl SpriteSlug {
    pub fn new(s: &str) -> Self {
        let src = s.as_bytes();
        let n = src.len().min(32);
        let mut bytes = [0u8; 32];
        bytes[..n].copy_from_slice(&src[..n]);
        Self { bytes, len: n as u8 }
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

/// Per-ship textured-quad draw. The bbox quad (`p0..p3`) is identical to
/// what the procedural silhouette would produce; the fragment shader
/// samples both `side` and `top` textures (looked up via the slugs at
/// draw time) and blends them by `blend_t = sin(view_angle_rad)`.
///
/// Emitted by `hud::push_ship` only when both side and top PNGs are
/// registered for the ship's `class_stance`. Otherwise the procedural
/// polygon-set is emitted instead.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct TexturedShipInstance {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    pub p2: [f32; 2],
    pub p3: [f32; 2],
    pub blend_t: f32,
    pub side: SpriteSlug,
    pub top: SpriteSlug,
}

/// A draw command in back-to-front order. `Gfx::render` batches consecutive
/// same-variant runs into single draw calls (except `TexturedShip`, which
/// is always drawn one-at-a-time since each ship has its own texture pair).
#[derive(Copy, Clone, Debug)]
pub enum DrawCommand {
    Sprite(SpriteInstance),
    Polygon(PolygonInstance),
    TexturedShip(TexturedShipInstance),
}

impl From<SpriteInstance> for DrawCommand {
    fn from(s: SpriteInstance) -> Self { DrawCommand::Sprite(s) }
}

impl From<PolygonInstance> for DrawCommand {
    fn from(p: PolygonInstance) -> Self { DrawCommand::Polygon(p) }
}

impl From<TexturedShipInstance> for DrawCommand {
    fn from(t: TexturedShipInstance) -> Self { DrawCommand::TexturedShip(t) }
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

/// Per-textured-ship blend factor. Padded to 16 bytes (wgpu uniform alignment).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendUniform {
    blend_t: f32,
    _pad: [f32; 3],
}

// Each of these structs is bound as a uniform buffer whose bind group entry
// uses `min_binding_size: None`, so wgpu defers the size check to draw time
// against the matching WGSL struct's byte layout. If the Rust size and the
// WGSL size disagree, the draw fails with a generic "Encoder is invalid"
// (see the BlendUniform vec3-padding bug fixed under task #92). These
// `const _` assertions make the Rust side a hard compile-time invariant.
//
// IMPORTANT: pad WGSL structs with scalars or `vec2<f32>` (4- / 8-byte
// alignment), never `vec3<f32>` — a vec3 forces 16-byte member alignment and
// silently inflates the WGSL struct past these sizes. When adding a uniform,
// add its assertion here AND keep its WGSL twin's byte layout in lockstep.
const _: () = assert!(std::mem::size_of::<ViewUniform>() == 16);
const _: () = assert!(std::mem::size_of::<BlitUniform>() == 16);
const _: () = assert!(std::mem::size_of::<BlendUniform>() == 16);

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

// Textured-ship shader. Same vertex layout as POLYGON_SHADER (four explicit
// corners expanded by vertex_index), but the fragment samples two textures
// (side, top) and blends by `blend_t` carried in a uniform — one uniform
// per ship since each batch is a single instance with its own texture pair.
const TEXTURED_SHIP_SHADER: &str = r#"
struct ViewUniform {
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
// NOTE: the pad must be three scalar f32s, NOT a vec3<f32>. In WGSL uniform
// layout a vec3<f32> has 16-byte alignment, which would push it to offset 16
// and make this struct 32 bytes — but the matching Rust `BlendUniform`
// (#[repr(C)] { f32, [f32; 3] }) and its uniform buffer are only 16 bytes.
// The mismatch trips wgpu's late-min-binding-size check at draw time
// (bound_size 16 < shader_expect_size 32), invalidating the encoder. Three
// scalars keep the WGSL struct at 16 bytes to match.
struct BlendUniform {
    blend_t: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};
@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(1) @binding(0) var<uniform> ship: BlendUniform;
@group(1) @binding(1) var side_tex: texture_2d<f32>;
@group(1) @binding(2) var top_tex:  texture_2d<f32>;
@group(1) @binding(3) var ship_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_ship(
    @builtin(vertex_index) v_idx: u32,
    @location(0) i_p0: vec2<f32>,
    @location(1) i_p1: vec2<f32>,
    @location(2) i_p2: vec2<f32>,
    @location(3) i_p3: vec2<f32>,
) -> VsOut {
    var corner_idx = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let c = corner_idx[v_idx];
    var pixel: vec2<f32>;
    var uv:    vec2<f32>;
    // p0 = top-left  -> uv (0, 0)
    // p1 = top-right -> uv (1, 0)
    // p2 = bot-right -> uv (1, 1)
    // p3 = bot-left  -> uv (0, 1)
    if (c == 0u) { pixel = i_p0; uv = vec2<f32>(0.0, 0.0); }
    else if (c == 1u) { pixel = i_p1; uv = vec2<f32>(1.0, 0.0); }
    else if (c == 2u) { pixel = i_p2; uv = vec2<f32>(1.0, 1.0); }
    else { pixel = i_p3; uv = vec2<f32>(0.0, 1.0); }
    let ndc_x = pixel.x * view.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * view.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_ship(in: VsOut) -> @location(0) vec4<f32> {
    let side_px = textureSample(side_tex, ship_samp, in.uv);
    let top_px  = textureSample(top_tex,  ship_samp, in.uv);
    // blend_t = sin(view_angle): 0 at pure side-on, 1 at pure top-down.
    //
    // A naive `mix(side, top, blend_t)` cross-DISSOLVES the two orthographic
    // sprites, so mid-angle reads as a double-exposure rather than a hull
    // tilting. The bounding box already foreshortens correctly (height·cosθ +
    // depth·sinθ) — the fix is to stop dissolving the *fill*:
    //
    //   B (faster curve): collapse the crossfade to a narrow smoothstep band
    //     so we snap through the muddy 50/50 instead of lingering there.
    //   A (dominant sprite): the band is centered on the θ=45° swap point
    //     (sin 45° = 0.7071), so for θ<45° we show ~pure side and for θ≥45°
    //     ~pure top. Outside the band there is zero double-exposure; the
    //     bbox foreshorten supplies the tilt illusion.
    //
    // HALF_BAND is the crossfade half-width in blend_t units. Small enough to
    // read as a crisp flip, wide enough (a few frames) to avoid a 1-frame
    // pop. Bruce iterates this and SWAP if needed.
    let SWAP: f32 = 0.70710677;   // sin(45°)
    let HALF_BAND: f32 = 0.06;
    let t = smoothstep(SWAP - HALF_BAND, SWAP + HALF_BAND, ship.blend_t);
    return mix(side_px, top_px, t);
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
    textured_ships: TexturedShipPipeline,
    blit: BlitPipeline,
    /// Loaded ship sprite textures keyed by `<class>_<stance>_<view>` slug.
    /// `gfx.rs` uploads each PNG to its own GPU texture; the textured-ship
    /// render path looks up handles here and supplies them as side/top
    /// bindings.
    ship_sprites: std::collections::HashMap<String, ShipSpriteEntry>,
    /// Cache of per-slot bind groups by (slot_idx, side_slug, top_slug).
    /// The bind group includes the slot's blend uniform (slot-specific)
    /// AND the texture pair. Cleared on `try_load_ship_sprites` since
    /// loaded textures may have changed.
    ship_bg_cache: std::collections::HashMap<(u32, SpriteSlug, SpriteSlug), wgpu::BindGroup>,
}

/// One uploaded ship sprite. `dimensions` is the source PNG size in
/// pixels so the renderer can compute the dest rect from the sprite's
/// intended bbox in the SPRITE_SPEC table.
struct ShipSpriteEntry {
    texture_view: wgpu::TextureView,
    #[allow(dead_code)]
    dimensions: (u32, u32),
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

/// Per-ship texture-blend pipeline. One pipeline + a shared per-ship bind
/// group layout (uniform + side tex + top tex + sampler). Each ship's
/// draw uses its own bind group built from the loaded ship sprite
/// textures, cached on `Gfx::ship_bg_cache` keyed by the slug pair.
struct TexturedShipPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_vbuf: wgpu::Buffer,
    /// view ubo bind group (group 0). Identical to sprites/polygons.
    view_bg: wgpu::BindGroup,
    /// Layout for per-ship bind group (group 1). Used to build cached
    /// (side, top) bind groups on demand.
    ship_bgl: wgpu::BindGroupLayout,
    /// Shared sampler used by every per-ship bind group.
    sampler: wgpu::Sampler,
    /// Pre-allocated per-frame uniform buffers — one entry per drawn ship.
    /// Sized for `MAX_TEXTURED_SHIPS` ships; each draw writes its slice
    /// before binding. Storing as one big buffer with dynamic offsets
    /// would be neater but the count is tiny so individual buffers are
    /// simpler.
    blend_ubos: Vec<wgpu::Buffer>,
}

struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    ubo: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl crate::sprites::SpriteRegistry for Gfx {
    fn has(
        &self,
        class: &str,
        stance: crate::sprites::SpriteStance,
        view: crate::sprites::SpriteView,
    ) -> bool {
        Gfx::has_ship_sprite(self, class, stance, view)
    }
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
        let textured_ships = TexturedShipPipeline::new(&device, &sprites.view_ubo);
        let blit = BlitPipeline::new(&device, format, &offscreen_view);

        let g = Self {
            surface,
            device,
            queue,
            config,
            offscreen_view,
            sprites,
            polygons,
            textured_ships,
            blit,
            ship_sprites: std::collections::HashMap::new(),
            ship_bg_cache: std::collections::HashMap::new(),
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

    /// Walk `assets/sprites/` and upload any `<class>_<stance>_<view>.png`
    /// files to GPU textures. Missing files are silently skipped (the
    /// procedural silhouette renders as the fallback). Each successfully
    /// loaded sprite is keyed by the same slug the SPRITE_SPEC defines.
    ///
    /// **Fallback chain** (applied in order per `<class>_<stance>_<view>`
    /// slot, fall through on miss):
    ///
    /// | Slot                       | 1. Explicit | 2. Derived | 3. Procedural |
    /// |----------------------------|:-----------:|:----------:|:-------------:|
    /// | `<class>_bowOnFore_*.png`  | ✓           | —          | renderer side |
    /// | `<class>_bowOnAft_*.png`   | ✓           | `mirror_horizontal(bowOnFore_<view>)` | renderer side |
    /// | `<class>_broadside_top.png`| ✓           | `rotate_90_cw(bowOnFore_top)` | renderer side |
    /// | `<class>_broadside_side.png` | ✓         | — (no derivation possible) | renderer side |
    ///
    /// Net result: bruce paints just `bowOnFore_side.png` and
    /// `bowOnFore_top.png` for each class; the loader derives 3 of the
    /// other 4 sprite slots. `broadside_side` is the only slot that
    /// stays procedural until painted explicitly (it's an end-on view
    /// of the hull that can't be reconstructed from the other faces).
    ///
    /// Explicit files always take precedence over derivations. Bruce
    /// can paint asymmetric ships per-class by dropping explicit
    /// bowOnAft / broadside_top PNGs.
    ///
    /// Returns the count of sprites loaded so the caller can log it.
    pub fn try_load_ship_sprites(&mut self, asset_dir: &std::path::Path) -> usize {
        use crate::sprites::{load_sprite, mirror_horizontal, rotate_90_cw, SpriteStance, SpriteView};
        // Invalidate cached bind groups — the underlying texture views
        // may have been replaced.
        self.ship_bg_cache.clear();
        // Class slugs the loader probes. `aegis` is the first
        // broadside-native player class (bruce's hand-painted art, see
        // assets/sprites/aegis_*.png). frigate / scout / gunboat are
        // placeholder names from the early-scaffold demo phase and
        // will eventually disappear when the canonical class roster
        // lands; keeping them in the probe list is free (missing files
        // are silently skipped) and avoids breaking the existing
        // procedural fallback.
        let classes = ["aegis", "frigate", "scout", "gunboat"];
        let views = [SpriteView::Side, SpriteView::Top];
        let mut loaded = 0;
        for class in &classes {
            for &view in &views {
                // Step 1: bowOnFore — always explicit-only. Remember
                // the image; we'll derive bowOnAft + (for top) broadside
                // from it.
                let fore = load_sprite(asset_dir, class, SpriteStance::BowOnFore, view);
                if let Some(img) = fore.as_ref() {
                    let slug = format!("{}_{}_{}", class, SpriteStance::BowOnFore.slug(), view.slug());
                    self.upload_ship_sprite(&slug, img);
                    loaded += 1;
                }
                // Step 2: bowOnAft = explicit, else mirror(bowOnFore).
                let aft_explicit = load_sprite(asset_dir, class, SpriteStance::BowOnAft, view);
                match (aft_explicit, fore.as_ref()) {
                    (Some(img), _) => {
                        let slug = format!("{}_{}_{}", class, SpriteStance::BowOnAft.slug(), view.slug());
                        self.upload_ship_sprite(&slug, &img);
                        loaded += 1;
                    }
                    (None, Some(fore_img)) => {
                        let mirrored = mirror_horizontal(fore_img);
                        let slug = format!("{}_{}_{}", class, SpriteStance::BowOnAft.slug(), view.slug());
                        log::debug!("sprite: deriving {} from horizontally-mirrored bowOnFore", slug);
                        self.upload_ship_sprite(&slug, &mirrored);
                        loaded += 1;
                    }
                    (None, None) => {}
                }
                // Step 3: broadside.
                //
                // - For `Top`: explicit → rotate90(bowOnFore_top) → none.
                // - For `Side`: explicit → none (no derivation possible).
                let bs_explicit = load_sprite(asset_dir, class, SpriteStance::Broadside, view);
                match (bs_explicit, view, fore.as_ref()) {
                    (Some(img), _, _) => {
                        let slug = format!("{}_{}_{}", class, SpriteStance::Broadside.slug(), view.slug());
                        self.upload_ship_sprite(&slug, &img);
                        loaded += 1;
                    }
                    (None, SpriteView::Top, Some(fore_top)) => {
                        let rotated = rotate_90_cw(fore_top);
                        let slug = format!("{}_{}_{}", class, SpriteStance::Broadside.slug(), view.slug());
                        log::debug!("sprite: deriving {} from rotate90(bowOnFore_top)", slug);
                        self.upload_ship_sprite(&slug, &rotated);
                        loaded += 1;
                    }
                    _ => {
                        // No explicit broadside, and either:
                        //  - view==Side (no derivation defined), or
                        //  - view==Top but no bowOnFore_top to rotate.
                        // Leave the slot empty; the procedural
                        // silhouette covers it.
                    }
                }
            }
        }
        loaded
    }

    /// Internal: upload one decoded sprite image to a wgpu texture and
    /// register it in `ship_sprites` under `slug`.
    fn upload_ship_sprite(&mut self, slug: &str, img: &crate::sprites::SpriteImage) {
        let size = wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(slug),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: Some(img.height),
            },
            size,
        );
        let texture_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.ship_sprites.insert(
            slug.to_string(),
            ShipSpriteEntry { texture_view, dimensions: (img.width, img.height) },
        );
    }

    /// True if a sprite has been loaded for the given class/stance/view.
    /// The textured-ship path uses this to decide whether to render the
    /// PNG or fall back to the procedural silhouette.
    pub fn has_ship_sprite(
        &self,
        class: &str,
        stance: crate::sprites::SpriteStance,
        view: crate::sprites::SpriteView,
    ) -> bool {
        let slug = format!("{}_{}_{}", class, stance.slug(), view.slug());
        self.ship_sprites.contains_key(&slug)
    }

    /// Build the per-slot (slot, side, top) bind group on first request
    /// and cache it. If either texture slug is missing from
    /// `ship_sprites`, the cache entry is **not** populated — the
    /// render loop checks the cache and skips drawing if absent.
    fn ensure_ship_bind_group(&mut self, slot_idx: usize, side: SpriteSlug, top: SpriteSlug) {
        let key = (slot_idx as u32, side, top);
        if self.ship_bg_cache.contains_key(&key) {
            return;
        }
        let side_entry = self.ship_sprites.get(side.as_str());
        let top_entry  = self.ship_sprites.get(top.as_str());
        let (side_view, top_view) = match (side_entry, top_entry) {
            (Some(s), Some(t)) => (&s.texture_view, &t.texture_view),
            _ => {
                log::debug!(
                    "ship bg skipped: side={} top={} (one or both not loaded)",
                    side.as_str(),
                    top.as_str()
                );
                return;
            }
        };
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ship per-instance bg"),
            layout: &self.textured_ships.ship_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.textured_ships.blend_ubos[slot_idx].as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(side_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(top_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.textured_ships.sampler),
                },
            ],
        });
        self.ship_bg_cache.insert(key, bg);
    }

    /// Compute the aspect-preserving, letterboxed NDC quad that maps the
    /// virtual-resolution offscreen target into the swapchain. Recomputed on
    /// every resize so the letterboxing tracks window changes.
    ///
    /// **Continuous fit-scale, not integer-floor.** The original integer-only
    /// scale (`min(w/VIRTUAL_W, h/VIRTUAL_H)` floored, clamped to ≥1) snapped
    /// the whole canvas to 1× unless the window was a full ≥2× multiple of the
    /// virtual size on BOTH axes. On a 2560×1080 monitor (canvas 1320×480)
    /// width gives 2560/1320 = 1.94 → floor 1×, so a maximized window rendered
    /// at native 1320×480 in a black letterbox and nothing scaled up — the
    /// real cause of "ships look too small". We now scale by the continuous
    /// limiting-axis factor (here ~1.94×, ≈2562×931 inside 2560×1080) and
    /// letterbox only the aspect-ratio remainder, so every window size scales
    /// smoothly and maximize fills the screen.
    fn update_blit_uniform(&self) {
        let wf = self.config.width as f32;
        let hf = self.config.height as f32;

        // Limiting-axis scale, preserving the virtual canvas's aspect ratio.
        let scale = (wf / VIRTUAL_W as f32).min(hf / VIRTUAL_H as f32).max(1.0);
        let scaled_w = VIRTUAL_W as f32 * scale;
        let scaled_h = VIRTUAL_H as f32 * scale;
        // Center the scaled canvas; the leftover on the non-limiting axis is
        // the (aspect-ratio) letterbox.
        let offset_x = (wf - scaled_w) / 2.0;
        let offset_y = (hf - scaled_h) / 2.0;

        let ndc_x_min = (offset_x / wf) * 2.0 - 1.0;
        let ndc_x_max = ((offset_x + scaled_w) / wf) * 2.0 - 1.0;
        let ndc_y_max = 1.0 - (offset_y / hf) * 2.0;
        let ndc_y_min = 1.0 - ((offset_y + scaled_h) / hf) * 2.0;

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
        // Textured-ship instances are stored as the 4 corner positions only
        // (the slugs + blend_t are CPU-side per-batch metadata).
        let mut ship_corner_buf: Vec<[f32; 8]> = Vec::with_capacity(MAX_TEXTURED_SHIPS);
        let mut ship_meta: Vec<(SpriteSlug, SpriteSlug, f32)> = Vec::with_capacity(MAX_TEXTURED_SHIPS);
        enum BatchKind {
            Sprite,
            Polygon,
            // index into ship_corner_buf / ship_meta
            TexturedShip(u32),
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
                DrawCommand::TexturedShip(t) => {
                    if ship_corner_buf.len() >= MAX_TEXTURED_SHIPS {
                        continue;
                    }
                    let idx = ship_corner_buf.len() as u32;
                    ship_corner_buf.push([
                        t.p0[0], t.p0[1], t.p1[0], t.p1[1],
                        t.p2[0], t.p2[1], t.p3[0], t.p3[1],
                    ]);
                    ship_meta.push((t.side, t.top, t.blend_t));
                    // Each textured-ship draw is its own batch (different
                    // bind group per ship).
                    batches.push(Batch { kind: BatchKind::TexturedShip(idx), start: idx, count: 1 });
                }
            }
        }
        if (commands.len() as u64) > MAX_SPRITES + MAX_POLYGONS + MAX_TEXTURED_SHIPS as u64 {
            log::warn!(
                "draw command count {} exceeds capacity; truncating",
                commands.len()
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
        if !ship_corner_buf.is_empty() {
            self.queue.write_buffer(
                &self.textured_ships.instance_vbuf,
                0,
                bytemuck::cast_slice(&ship_corner_buf),
            );
            // Write per-ship blend uniforms and ensure bind groups exist.
            for (i, (side, top, blend)) in ship_meta.iter().enumerate() {
                let blend_u = BlendUniform { blend_t: *blend, _pad: [0.0; 3] };
                self.queue.write_buffer(
                    &self.textured_ships.blend_ubos[i],
                    0,
                    bytemuck::bytes_of(&blend_u),
                );
                self.ensure_ship_bind_group(i, *side, *top);
            }
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
                    BatchKind::TexturedShip(slot_idx) => {
                        let (side, top, _blend) = ship_meta[slot_idx as usize];
                        let bg = match self.ship_bg_cache.get(&(slot_idx, side, top)) {
                            Some(bg) => bg,
                            None => {
                                // Bind group missing — sprites for this slug
                                // pair aren't loaded. Skip the draw; the
                                // procedural polygons below stay visible.
                                continue;
                            }
                        };
                        pass.set_pipeline(&self.textured_ships.pipeline);
                        pass.set_bind_group(0, &self.textured_ships.view_bg, &[]);
                        pass.set_bind_group(1, bg, &[]);
                        // Offset the vbuf to this slot's 32 bytes.
                        let off = (slot_idx as u64) * 32;
                        pass.set_vertex_buffer(0, self.textured_ships.instance_vbuf.slice(off..off + 32));
                        // Draw 6 verts (two triangles) of one instance.
                        pass.draw(0..6, 0..1);
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

impl TexturedShipPipeline {
    fn new(device: &wgpu::Device, view_ubo: &wgpu::Buffer) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-ship shader"),
            source: wgpu::ShaderSource::Wgsl(TEXTURED_SHIP_SHADER.into()),
        });

        let view_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ship view bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let view_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ship view bg"),
            layout: &view_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_ubo.as_entire_binding(),
            }],
        });

        let ship_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ship per-instance bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ship texture sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ship layout"),
            bind_group_layouts: &[&view_bgl, &ship_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ship pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_ship"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    // Instance stride matches the four corner positions
                    // packed at the head of TexturedShipInstance.
                    array_stride: 32, // 4 × Float32x2
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute { shader_location: 0, offset: 0,  format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { shader_location: 1, offset: 8,  format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { shader_location: 2, offset: 16, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { shader_location: 3, offset: 24, format: wgpu::VertexFormat::Float32x2 },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ship"),
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

        // Per-frame instance buffer: one slot per ship that may draw.
        let instance_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ship instance vbuf"),
            // Each instance is 4 × Float32x2 = 32 bytes (matches the
            // vertex layout — the slug bytes and blend_t in
            // TexturedShipInstance are CPU-side only and don't go in
            // the vbuf).
            size: 32 * MAX_TEXTURED_SHIPS as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut blend_ubos = Vec::with_capacity(MAX_TEXTURED_SHIPS);
        for i in 0..MAX_TEXTURED_SHIPS {
            blend_ubos.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("ship blend ubo {}", i)),
                size: std::mem::size_of::<BlendUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        Self { pipeline, instance_vbuf, view_bg, ship_bgl, sampler, blend_ubos }
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

        // LINEAR for the final canvas→window blit. The blit is now a
        // CONTINUOUS fit-scale (see `update_blit_uniform`), not an integer
        // multiple — e.g. ~1.94× when maximized on 2560×1080. NEAREST at a
        // non-integer scale unevenly doubles source texels (some 1 px, some
        // 2 px wide), which shimmers on motion and looks chunky on bruce's
        // hand-painted (non-pixel-art) ship PNGs. LINEAR resamples smoothly
        // for consistent edges. (The offscreen scene itself is still drawn
        // 1 texel = 1 virtual pixel; this softening only happens once, on the
        // upscale to the window.) Bruce can flip this back to Nearest if he
        // prefers crisp-but-uneven.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit linear sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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
