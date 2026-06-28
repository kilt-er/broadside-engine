//! GPU side of the loft render pipeline — the in-engine lift of the validated
//! `loft_poc` spike (`src/bin/loft_poc.rs`). Renders a [`crate::loft::HullMesh`]
//! as crisp ¾-view posterized pixel art:
//!
//!   1. **Depth-tested 3D pass** — orthographic ¾ camera + flat Lambert (key +
//!      fill + ambient), per-vertex albedo, into a low-res offscreen color +
//!      depth target. This is the ONLY depth-using pipeline in the engine; the
//!      2D compositor in [`crate::gfx`] stays `depth_stencil: None`. The depth
//!      texture lives entirely inside this module.
//!   2. **Posterize pass** — WGSL port of the loft editor's GLSL frag (HSV
//!      grade → quantize to [`BANDS`] → discard a<0.5), nearest-sampled. The
//!      result is an RGBA texture with a transparent (cut-out) background.
//!
//! That posterized texture is what the existing `gfx` 2D compositor blits into
//! the lane via its `TexturedShip` path — so the rest of the renderer never
//! sees 3D or depth. The ship buffer is a RUNTIME size ([`LOFT_RES_PRESETS`],
//! cycled live with `,`/`.`); it BOOTS at [`DEFAULT_LOFT_RES`] (480×300, Bruce's
//! pick — crisp out of the gate) and can step down to the chunky [`LOW_W`]×
//! [`LOW_H`] (160×100) floor. At [`BANDS`] (8) posterize bands; this is the SHIP
//! resolution only, independent of the 2D compositor / HUD virtual res.
//!
//! ## Camera / motion (orientation-driven, NOT auto-spin)
//!
//! In-game the ship holds its gameplay facing — yaw comes from the ship's
//! [`Orientation`], not a demo spin (see [`ShipPose`]):
//!   - **base yaw** from `Orientation` (the four canonical stance yaws),
//!   - a small **idle** bob/sway/roll so a resting ship reads as alive,
//!   - an **active reorient tween** that rotates yaw smoothly through the real
//!     3D when the ship flips bow-on↔broadside (the headline win over sprites),
//!   - **pitch** = the existing camera-angle scrubber (passed in, unchanged).
//!
//! The POC's auto-orbit is gone; the only continuous motion in-engine is the
//! low-amplitude idle and any in-flight reorient tween.

use crate::loft::HullMesh;
use crate::types::{LaneEnd, Orientation};

/// Loft offscreen render resolution — the SHIP pixel size. #48 drops this from
/// 320×200 to 160×100 for chunkier ship pixels: the loft output blits
/// nearest-neighbour into a fixed lane dest-rect, so a lower offscreen res =
/// bigger on-screen texels = more pixellated ships. This is the SHIP buffer
/// ONLY — the 2D compositor / HUD virtual res (SALVAGE text, ruler, background)
/// is untouched and stays crisp. Aspect stays 1.6 (160/100 = 320/200), so the
/// lane dest-rect in hud is unchanged. bruce dials the exact chunkiness on the
/// pixellation-check (one-line change here).
pub const LOW_W: u32 = 160;
pub const LOW_H: u32 = 100;

/// (#76) The SHIP-res cycle Bruce steps through with `,` / `.` (chunky → crisp):
/// 160×100 (default) → 220×138 → 320×200 → 480×300, then wraps. All ~1.6:1 so the
/// lane dest-quad aspect ([`crate::hud`]'s `LOFT_TEXTURE_ASPECT`) stays valid and
/// the hull never re-squashes (#74; 480/1.6 = 300). Bigger = finer ship pixels —
/// 480×300 is the crisp-hull step Bruce asked for past the old 320 max.
/// [`next_loft_res`] / [`prev_loft_res`] step this list, snapping an off-list
/// current size to the default first.
pub const LOFT_RES_PRESETS: [(u32, u32); 4] = [(160, 100), (220, 138), (320, 200), (480, 300)];

/// (#76) The SHIP-loft res the game LAUNCHES at — Bruce's pick after the A/B:
/// "480 for the ship is the winner." So the hull boots CRISP (480×300) instead of
/// the chunky 160×100 floor (which forced a cycle-up every run). `,`/`.` still
/// cycle all four [`LOFT_RES_PRESETS`] for experimentation; this only sets the
/// initial [`LoftGpu`] target size. = `LOFT_RES_PRESETS[3]` (the crisp end);
/// [`LOW_W`]/[`LOW_H`] (160×100) remain the chunky floor preset, no longer the
/// boot default.
pub const DEFAULT_LOFT_RES: (u32, u32) = LOFT_RES_PRESETS[3];

/// The next ship-res preset after `(w, h)` (wraps). If `(w, h)` isn't in the
/// list, returns the first preset. See [`LOFT_RES_PRESETS`].
pub fn next_loft_res(w: u32, h: u32) -> (u32, u32) {
    match LOFT_RES_PRESETS.iter().position(|&p| p == (w, h)) {
        Some(i) => LOFT_RES_PRESETS[(i + 1) % LOFT_RES_PRESETS.len()],
        None => LOFT_RES_PRESETS[0],
    }
}

/// The previous ship-res preset before `(w, h)` (wraps). See [`LOFT_RES_PRESETS`].
pub fn prev_loft_res(w: u32, h: u32) -> (u32, u32) {
    match LOFT_RES_PRESETS.iter().position(|&p| p == (w, h)) {
        Some(i) => LOFT_RES_PRESETS[(i + LOFT_RES_PRESETS.len() - 1) % LOFT_RES_PRESETS.len()],
        None => LOFT_RES_PRESETS[0],
    }
}
/// Posterize band count — kept at 8 (this is about pixel SIZE, not colour count).
pub const BANDS: f32 = 8.0;

/// Default hull albedo (loft editor `0xb4c6e0`, linear-ish sRGB stored) used
/// when a [`HullMesh`] carries no per-vertex colors.
const DEFAULT_HULL_ALBEDO: [f32; 3] = [0.706, 0.776, 0.878];

const LOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Fixed look-down pitch (degrees). (#62) Bruce's art-tool chase-cam reference
/// reads ~20° — a LOW behind-the-ship angle where the hull shows its stern +
/// engine glow toward the camera and reads with real MASS (a 48° near-overhead
/// view flattened the low-profile Aegis to a plank — proven inert to hscale).
/// Lower = more behind/level; higher = more overhead. 20° matches the tool's
/// PITCH slider; Bruce dials the exact amount on the angle-check.
pub const CAMERA_PITCH_DEG: f32 = 20.0;

/// The canonical stance yaws (degrees), keyed by [`Orientation`], fed to
/// [`camera_view_proj_zoom`] as the CAMERA yaw with the model at IDENTITY.
///
/// #47 — a DELIBERATE override of the POC stance snaps (28/152/118). bruce wants
/// the non-broadside ships PARALLEL to the lane, with the ¾ coming from the
/// top-down [`CAMERA_PITCH_DEG`] pitch rather than from yawing them to show a
/// front/side:
///   BowOn{Fore} = 0   — hull length along the lane, camera square to it,
///   BowOn{Aft}  = 180 — parallel too, bow pointing the other way,
///   Broadside   = 90  — perpendicular, camera looks down the hull's length
///                       (kept exactly as it reads now).
const STANCE_YAW_FORE: f32 = 0.0;
const STANCE_YAW_AFT: f32 = 180.0;
const STANCE_YAW_BROADSIDE: f32 = 90.0;

/// Base stance yaw (degrees) a ship at `orientation` rests at. The reorient
/// tween interpolates between two of these; the idle roll is added on top.
pub const fn orientation_yaw_deg(orientation: Orientation) -> f32 {
    match orientation {
        Orientation::BowOn { bow: LaneEnd::Fore } => STANCE_YAW_FORE,
        Orientation::BowOn { bow: LaneEnd::Aft } => STANCE_YAW_AFT,
        Orientation::Broadside => STANCE_YAW_BROADSIDE,
    }
}

/// Per-ship animated pose state the renderer keeps between frames: the resting
/// orientation, plus an optional in-flight reorient tween, plus the idle phase.
/// Pure state + math — no GPU — so it is unit-testable headless.
#[derive(Clone, Copy, Debug)]
pub struct ShipPose {
    /// The ship's current resting orientation (its base yaw).
    orientation: Orientation,
    /// In-flight reorient: `(from_yaw_deg, to_yaw_deg, elapsed_s, dur_s)`.
    /// `None` when at rest.
    tween: Option<(f32, f32, f32, f32)>,
    /// Idle animation phase (seconds), advanced every frame.
    idle_t: f32,
}

/// How long a bow-on↔broadside reorient tween takes (seconds). Snappy (#52,
/// bruce: the turn read slow) — a crisp ~quarter-second 90° swing. The tween
/// always interpolates the SHORTEST path between the two stance yaws (now a
/// clean 90°, since reorient toggles bow-on↔broadside rather than the old 180°
/// Fore↔Aft flip), so there's no over-spin to sit through.
pub const REORIENT_SECS: f32 = 0.28;
/// Idle bob/sway/roll amplitudes — low, so a resting ship "breathes" without
/// looking adrift. Roll is the visible one (a few degrees of yaw wobble); the
/// bob is a vertical pixel nudge applied by the caller via [`ShipPose::idle_bob`].
const IDLE_ROLL_DEG: f32 = 1.5;
const IDLE_BOB_PX: f32 = 1.5;
const IDLE_ROLL_HZ: f32 = 0.18;
const IDLE_BOB_HZ: f32 = 0.13;

impl ShipPose {
    pub const fn new(orientation: Orientation) -> Self {
        Self {
            orientation,
            tween: None,
            idle_t: 0.0,
        }
    }

    /// Begin a smooth reorient to `to`. Tweens from the *current* displayed
    /// yaw (so re-flips mid-tween don't snap) to `to`'s base yaw.
    pub fn reorient_to(&mut self, to: Orientation) {
        if to == self.orientation && self.tween.is_none() {
            return;
        }
        let from = self.yaw_deg_no_idle();
        let target = orientation_yaw_deg(to);
        self.orientation = to;
        if (target - from).abs() < f32::EPSILON {
            self.tween = None;
        } else {
            self.tween = Some((from, target, 0.0, REORIENT_SECS));
        }
    }

    /// Advance idle + any active reorient tween by `dt` seconds.
    pub fn advance(&mut self, dt: f32) {
        self.idle_t += dt;
        if let Some((from, to, mut elapsed, dur)) = self.tween {
            elapsed += dt;
            if elapsed >= dur {
                self.tween = None;
            } else {
                self.tween = Some((from, to, elapsed, dur));
            }
        }
    }

    /// Base/tweened yaw in degrees WITHOUT the idle roll (used as the tween
    /// origin so re-flips are continuous).
    fn yaw_deg_no_idle(&self) -> f32 {
        match self.tween {
            None => orientation_yaw_deg(self.orientation),
            Some((from, to, elapsed, dur)) => {
                let t = (elapsed / dur).clamp(0.0, 1.0);
                lerp(from, to, smoothstep(t))
            }
        }
    }

    /// The yaw (degrees) to render this frame: base/tween + low-amplitude idle
    /// roll.
    pub fn yaw_deg(&self) -> f32 {
        let idle = (self.idle_t * IDLE_ROLL_HZ * std::f32::consts::TAU).sin() * IDLE_ROLL_DEG;
        self.yaw_deg_no_idle() + idle
    }

    /// Vertical idle bob in virtual pixels — the caller adds this to the
    /// ship's screen-space y so a resting ship gently rises/settles.
    pub fn idle_bob(&self) -> f32 {
        (self.idle_t * IDLE_BOB_HZ * std::f32::consts::TAU).cos() * IDLE_BOB_PX
    }

    /// True while a reorient tween is in flight (the caller keeps requesting
    /// redraws while any ship is mid-reorient).
    pub const fn is_animating(&self) -> bool {
        self.tween.is_some()
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Smoothstep easing for the reorient tween (ease-in-out).
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------------------
// GPU vertex / uniform layout
// ---------------------------------------------------------------------------

/// One hull vertex: position + flat normal + per-vertex albedo + emissive,
/// 16-byte aligned (each `vec3` padded to a vec4 slot so the std-ish layout is
/// unambiguous and bytemuck-`Pod`-safe). `emissive.w` doubles as the unlit
/// flag (1.0 = unlit / flat color, 0.0 = Lambert-shaded) so no extra attribute
/// is needed.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    _pad0: f32,
    normal: [f32; 3],
    _pad1: f32,
    color: [f32; 3],
    _pad2: f32,
    /// xyz = linear emissive RGB (added post-Lambert so glow stays bright at
    /// any facing); w = unlit flag (1.0 → skip shading, draw flat `color`).
    emissive: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [f32; 16],
    model: [f32; 16],
    key_dir: [f32; 4],  // xyz toward key light, w = intensity
    fill_dir: [f32; 4], // xyz toward fill light, w = intensity
    ambient: [f32; 4],  // rgb ambient term; albedo travels per-vertex
}

/// Posterize band count as a uniform. Pads with THREE SCALAR f32s — never a
/// `vec3<f32>`, which under WGSL uniform layout would make the struct 32 bytes
/// vs this 16 and trip wgpu's late-min-binding-size check (the invalid-encoder
/// trap fixed twice in the POC / gfx.rs). The `size_of` asserts below pin it.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PostUniform {
    bands: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

// Compile-time guards: every uniform struct's Rust size MUST match the WGSL
// struct's byte size, or the late-min-binding-size check fails at draw time
// with a generic "Encoder is invalid". Making the sizes a hard compile error
// kills that whole bug class before it can reach the GPU. (Standing rule for
// this pipeline — see the gfx.rs BlendUniform / loft_poc PostUniform history.)
// pos+pad, normal+pad, color+pad, emissive(vec4) = 4 × 16 = 64.
const _: () = assert!(std::mem::size_of::<Vertex>() == 64);
// 2 mat4 (128) + 3 vec4 (48) = 176.
const _: () = assert!(std::mem::size_of::<SceneUniform>() == 176);
const _: () = assert!(std::mem::size_of::<PostUniform>() == 16);

const HULL_SHADER: &str = r"
struct Scene {
    view_proj: mat4x4<f32>,
    model:     mat4x4<f32>,
    key_dir:   vec4<f32>,
    fill_dir:  vec4<f32>,
    ambient:   vec4<f32>,
};
@group(0) @binding(0) var<uniform> scene: Scene;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_n: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) emissive: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) col: vec3<f32>,
    @location(3) emis: vec4<f32>,
) -> VsOut {
    let world = scene.model * vec4<f32>(pos, 1.0);
    let wn = (scene.model * vec4<f32>(nrm, 0.0)).xyz;
    var o: VsOut;
    o.clip = scene.view_proj * world;
    o.world_n = wn;
    o.color = col;
    o.emissive = emis;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // emissive.w == 1.0 → unlit: draw the flat material colour, no Lambert
    // (the CAD tool's MeshBasicMaterial engine-glow / glTF KHR_materials_unlit).
    if (in.emissive.w > 0.5) {
        // Still clamp so the posterize pass downstream bands it cleanly rather
        // than blowing to white.
        return vec4<f32>(clamp(in.color + in.emissive.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
    }
    let n = normalize(in.world_n);
    let key = max(dot(n, normalize(scene.key_dir.xyz)), 0.0) * scene.key_dir.w;
    let fill = max(dot(n, normalize(scene.fill_dir.xyz)), 0.0) * scene.fill_dir.w;
    let lit = in.color * (scene.ambient.rgb + vec3<f32>(key) + vec3<f32>(0.53, 0.67, 1.0) * fill);
    // Add emissive AFTER Lambert so glow surfaces (canopy / gun / battery /
    // engine) stay bright regardless of facing, then clamp into [0,1] so the
    // posterize stays inside its budget (banded glow, not a white blowout).
    let out = clamp(lit + in.emissive.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(out, 1.0);
}
";

const POST_SHADER: &str = r"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
// Scalar pads, NOT vec3<f32> (16-byte struct; see the Rust-side note).
struct Post { bands: f32, _pad0: f32, _pad1: f32, _pad2: f32 };
@group(0) @binding(2) var<uniform> post: Post;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_post(@builtin(vertex_index) idx: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -3.0), vec2<f32>(-1.0, 1.0), vec2<f32>(3.0, 1.0));
    let xy = p[idx];
    var o: VsOut;
    o.clip = vec4<f32>(xy, 0.0, 1.0);
    o.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
    return o;
}

@fragment
fn fs_post(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    // Keep the cut-out: transparent background stays transparent so the 2D
    // compositor blits only the ship silhouette.
    if (c.a < 0.5) {
        discard;
    }
    let q = floor(c.rgb * post.bands + 0.5) / post.bands;
    return vec4<f32>(q, 1.0);
}
";

/// A hull uploaded to the GPU: its vertex buffer + count, plus the vertical
/// centre of its bounding box (`center_y`, world units). `center_y` is the Y the
/// loft camera should look at so the hull renders CENTRED in its texture (see
/// [`LoftGpu::upload_hull`]); the caller stores it and passes it to
/// [`LoftGpu::render_ship`].
#[derive(Debug)]
pub struct UploadedHull {
    pub vbuf: wgpu::Buffer,
    pub vcount: u32,
    pub center_y: f32,
}

/// The loft GPU pipeline + offscreen targets. Owns the depth path; produces a
/// posterized RGBA texture view per render. The caller (`gfx`) takes that view
/// and feeds it to the existing `TexturedShip` blit.
#[derive(Debug)]
pub struct LoftGpu {
    hull_pipeline: wgpu::RenderPipeline,
    post_pipeline: wgpu::RenderPipeline,
    scene_ubo: wgpu::Buffer,
    scene_bg: wgpu::BindGroup,
    bands_ubo: wgpu::Buffer,
    /// Low-res color target the 3D pass draws into.
    scene_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// Final posterized RGBA target (`TEXTURE_BINDING` so gfx can sample it).
    out_tex: wgpu::Texture,
    out_view: wgpu::TextureView,
    post_bg: wgpu::BindGroup,
    /// (#76) The SHIP offscreen size — runtime now, not the [`LOW_W`]/[`LOW_H`]
    /// const, so Bruce can cycle the ship-pixel chunkiness live ([`Self::resize`]
    /// recreates the three targets + `post_bg` at the new size). Initialised to
    /// [`DEFAULT_LOFT_RES`] (480×300, Bruce's pick). The 2D compositor / HUD
    /// virtual res is untouched.
    low_w: u32,
    low_h: u32,
    /// Kept so [`Self::resize`] can rebuild the targets + `post_bg` without
    /// re-deriving the sampler / bind-group layout (size-independent).
    post_bgl: wgpu::BindGroupLayout,
    post_sampler: wgpu::Sampler,
}

impl LoftGpu {
    /// Linear-RGB ambient (loft editor `0x3a4560` × 0.9).
    fn ambient() -> [f32; 3] {
        [58.0 / 255.0 * 0.9, 69.0 / 255.0 * 0.9, 96.0 / 255.0 * 0.9]
    }

    /// Build the pipeline + offscreen + posterize targets. `device`/`queue`
    /// are borrowed from `gfx`; this does not own them.
    pub fn new(device: &wgpu::Device) -> Self {
        // ---- scene uniform + hull pipeline ----
        let scene_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loft scene ubo"),
            size: std::mem::size_of::<SceneUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("loft scene bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let scene_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loft scene bg"),
            layout: &scene_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_ubo.as_entire_binding(),
            }],
        });

        let hull_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("loft hull shader"),
            source: wgpu::ShaderSource::Wgsl(HULL_SHADER.into()),
        });
        let hull_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("loft hull layout"),
            bind_group_layouts: &[&scene_bgl],
            push_constant_ranges: &[],
        });
        let hull_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("loft hull pipeline"),
            layout: Some(&hull_layout),
            vertex: wgpu::VertexState {
                module: &hull_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            offset: 16,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            offset: 32,
                            format: wgpu::VertexFormat::Float32x3,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 3,
                            offset: 48,
                            format: wgpu::VertexFormat::Float32x4,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &hull_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LOW_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None, // closed loft / imported mesh; don't risk holes
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ---- offscreen scene color + depth + final posterized output ----
        // (#76) Created at the runtime ship res via the shared helper so
        // [`Self::resize`] can rebuild them. Boots at [`DEFAULT_LOFT_RES`] (480×300,
        // Bruce's pick) — crisp out of the gate, not the chunky 160×100 floor.
        let (low_w, low_h) = DEFAULT_LOFT_RES;
        let (scene_view, depth_view, out_tex, out_view) = make_loft_targets(device, low_w, low_h);

        // ---- posterize pipeline ----
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("loft post nearest"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let post_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("loft post shader"),
            source: wgpu::ShaderSource::Wgsl(POST_SHADER.into()),
        });
        let post_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("loft post bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bands_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loft bands ubo"),
            size: std::mem::size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loft post bg"),
            layout: &post_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: bands_ubo.as_entire_binding(),
                },
            ],
        });
        let post_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("loft post layout"),
            bind_group_layouts: &[&post_bgl],
            push_constant_ranges: &[],
        });
        let post_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("loft post pipeline"),
            layout: Some(&post_layout),
            vertex: wgpu::VertexState {
                module: &post_shader,
                entry_point: Some("vs_post"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &post_shader,
                entry_point: Some("fs_post"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LOW_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            hull_pipeline,
            post_pipeline,
            scene_ubo,
            scene_bg,
            bands_ubo,
            scene_view,
            depth_view,
            out_tex,
            out_view,
            post_bg,
            low_w,
            low_h,
            post_bgl,
            post_sampler: sampler,
        }
    }

    /// (#76) Resize the SHIP offscreen targets to `(w, h)` LIVE — recreate the
    /// scene-color / depth / posterized-out textures + views at the new size and
    /// rebuild `post_bg` (which samples the scene-color view). The pipelines,
    /// UBOs, sampler, and bind-group layout are size-independent and reused. No
    /// effect on the 2D compositor / HUD virtual res. Cheap (3 small textures +
    /// one bind group); called only on a Bruce keypress, not per frame. Ignores a
    /// zero dimension (keeps the current size) so a degenerate cycle can't crash.
    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let (scene_view, depth_view, out_tex, out_view) = make_loft_targets(device, w, h);
        // Rebuild post_bg against the NEW scene-color view (binding 0); the
        // sampler (1) + bands ubo (2) are unchanged.
        let post_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loft post bg (resized)"),
            layout: &self.post_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.bands_ubo.as_entire_binding(),
                },
            ],
        });
        self.scene_view = scene_view;
        self.depth_view = depth_view;
        self.out_tex = out_tex;
        self.out_view = out_view;
        self.post_bg = post_bg;
        self.low_w = w;
        self.low_h = h;
    }

    /// The posterized RGBA output texture view (`TEXTURE_BINDING`) for the gfx
    /// compositor to sample into the lane.
    pub const fn output_view(&self) -> &wgpu::TextureView {
        &self.out_view
    }

    /// Output dimensions (for the gfx side's UV / dest-rect math). Runtime now
    /// (#76 ship-res cycle), not the const.
    pub const fn output_size(&self) -> (u32, u32) {
        (self.low_w, self.low_h)
    }

    /// Read the posterized output texture back to an RGBA8 PNG on disk. For the
    /// dynamic-lighting test / any headless loft inspection — copies `out_tex`
    /// to a mappable buffer (stripping the 256-byte row alignment), maps it, and
    /// saves via `image`. `device.poll(Wait)` drives the readback to completion.
    /// NOT on the gameplay hot path (a per-frame GPU→CPU copy + map is slow);
    /// this is a capture/debug entry point only.
    pub fn read_output_png(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &std::path::Path,
    ) -> Result<(), String> {
        // (#76) Runtime ship res (cyclable), not the const.
        let (low_w, low_h) = (self.low_w, self.low_h);
        let unpadded = low_w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT; // 256
        let padded = unpadded.div_ceil(align) * align;
        let buf_size = u64::from(padded * low_h);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loft readback"),
            size: buf_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("loft rb"),
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.out_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(low_h),
                },
            },
            wgpu::Extent3d {
                width: low_w,
                height: low_h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::Wait).ok();
        rx.recv()
            .map_err(|e| format!("map channel: {e}"))?
            .map_err(|e| format!("map_async: {e}"))?;

        let data = slice.get_mapped_range();
        // Strip the row padding into a tight RGBA8 buffer.
        let mut rgba = Vec::with_capacity((unpadded * low_h) as usize);
        for row in 0..low_h {
            let start = (row * padded) as usize;
            let end = start + unpadded as usize;
            rgba.extend_from_slice(&data[start..end]);
        }
        drop(data);
        readback.unmap();

        image::save_buffer(path, &rgba, low_w, low_h, image::ColorType::Rgba8)
            .map_err(|e| format!("png save: {e}"))
    }

    /// Upload a hull's geometry to a fresh vertex buffer. `colors`, if present,
    /// is parallel to `mesh.positions` (one linear-RGB albedo per vertex);
    /// empty falls back to the default hull grey. `emissive` is likewise
    /// parallel — `xyz` linear emissive RGB, `w` the unlit flag (1.0 → flat,
    /// no Lambert); empty / short means no emissive + lit (the loft path, whose
    /// procedural hulls don't glow). Returns the buffer + vertex count for
    /// [`Self::render_ship`].
    ///
    /// Kept separate from `render_ship` so the caller can upload once per ship
    /// design and re-render every frame as the pose animates.
    pub fn upload_hull(
        &self,
        device: &wgpu::Device,
        mesh: &HullMesh,
        colors: &[[f32; 3]],
        emissive: &[[f32; 4]],
    ) -> UploadedHull {
        use wgpu::util::DeviceExt;
        let verts: Vec<Vertex> = (0..mesh.positions.len())
            .map(|i| Vertex {
                pos: mesh.positions[i],
                _pad0: 0.0,
                normal: mesh.normals[i],
                _pad1: 0.0,
                color: colors.get(i).copied().unwrap_or(DEFAULT_HULL_ALBEDO),
                _pad2: 0.0,
                emissive: emissive.get(i).copied().unwrap_or([0.0, 0.0, 0.0, 0.0]),
            })
            .collect();
        // Vertical centre of the hull's bounding box (world units). The loft
        // camera looks at this Y instead of the origin so the hull sits CENTRED
        // in its texture regardless of how the design distributes mass about
        // y=0 — e.g. the Aegis section runs deck +0.55 to belly −1.1, so its
        // mass centres BELOW the origin and, framed at origin, the hull rendered
        // low in the texture and read detached from its (cell-centred) chevron +
        // hull-bar (#54/#55). Centring the camera on the bbox fixes every mesh.
        let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in &mesh.positions {
            min_y = min_y.min(p[1]);
            max_y = max_y.max(p[1]);
        }
        let center_y = if min_y.is_finite() {
            (min_y + max_y) * 0.5
        } else {
            0.0
        };
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("loft hull vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        UploadedHull {
            vbuf: buf,
            vcount: verts.len() as u32,
            center_y,
        }
    }

    /// Upload an [`ImportedShip`] (the architect's `mesh_import` / CAD-glb output,
    /// and the shape the loft path also targets): expands the per-group
    /// materials onto per-vertex albedo + emissive (with the unlit flag) and
    /// delegates to [`Self::upload_hull`]. Both geometry sources reach the GPU
    /// through one path; glow surfaces (canopy / gun / battery / engine) render
    /// emissive, and unlit materials skip Lambert.
    pub fn upload_imported(
        &self,
        device: &wgpu::Device,
        ship: &crate::mesh_import::ImportedShip,
    ) -> UploadedHull {
        let (colors, emissive) = imported_vertex_attrs(ship);
        self.upload_hull(device, &ship.mesh, &colors, &emissive)
    }

    /// Like [`Self::upload_imported`] but multiplies every vertex's ALBEDO by
    /// `tint` (per-channel) — used to recolour the shared CAD hull a distinct hue
    /// for the player so it reads apart from the enemy fleet. EMISSIVE is left
    /// UNTOUCHED so the authored engine glow keeps its colour.
    ///
    /// (#111, reverting the #105 emissive-tint) #105 briefly tinted emissive too,
    /// to stop the bright cyan engine washing the (then pinkish) hull toward pink —
    /// but that turned the engine interiors RED, and Bruce wants RED hull + BLUE
    /// engines. The real fix is the SATURATED red hull tint ([1.9,0.16,0.14], lit
    /// ≈ [0.83,0.09,0.12]): against a deep-red hull the untouched cyan engine
    /// (lit ≈ [0.46,0.86,1.0]) reads as a distinct BLUE accent, NOT an averaged
    /// pink. So emissive stays authored — the player's engine is blue on a red
    /// hull, the enemy's blue on a grey hull. (Probed numerically before the fix.)
    pub fn upload_imported_tinted(
        &self,
        device: &wgpu::Device,
        ship: &crate::mesh_import::ImportedShip,
        tint: [f32; 3],
    ) -> UploadedHull {
        let (mut colors, emissive) = imported_vertex_attrs(ship);
        for c in &mut colors {
            c[0] *= tint[0];
            c[1] *= tint[1];
            c[2] *= tint[2];
        }
        self.upload_hull(device, &ship.mesh, &colors, &emissive)
    }

    /// Render one ship pose into the offscreen target and posterize it into
    /// [`Self::output_view`]. `yaw_deg` is the CAMERA yaw fed to
    /// [`camera_view_proj_zoom`] with the model left at identity; pitch fixed at
    /// [`CAMERA_PITCH_DEG`]. For the live PLAYER this is
    /// [`crate::gfx`]'s flat ground-plane yaw from
    /// [`chase_cam_ground_yaw_deg`] (base stern-on + tactical facing + lane-aim,
    /// #73/#75) — NOT [`ShipPose::yaw_deg`]; the `ShipPose` only gates the draw +
    /// drives the idle bob/tween, it does not supply this render yaw. (Other
    /// callers, e.g. the dynamic-lighting demo, pass their own yaw.) Records into
    /// `encoder`; the caller submits.
    pub fn render_ship(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vbuf: &wgpu::Buffer,
        vcount: u32,
        yaw_deg: f32,
        center_y: f32,
    ) {
        // The gameplay path: the house key light (loft editor setLight laz -50,
        // lel 60, intensity 1.6). Delegates to the light-parameterised path.
        self.render_ship_lit(
            queue, encoder, vbuf, vcount, yaw_deg, center_y, -50.0, 60.0, 1.6,
        );
    }

    /// (#140 ship-tilt) Like [`Self::render_ship`] (house lights + gameplay zoom)
    /// but with the CAMERA PITCH as a parameter, so the live grid-pitch arc
    /// ([`crate::gfx::loft_pitch_deg`]) can tilt the hull to stay PARALLEL to the
    /// raising grid plane. `pitch_deg == CAMERA_PITCH_DEG` reproduces
    /// [`Self::render_ship`] exactly (the byte-identical step-0 default); a higher
    /// pitch looks down the deck so the hull reads top-down. The house key light +
    /// fill + ambient + the gameplay [`HALF_EXTENT`] zoom are unchanged — only the
    /// look-down angle moves, so the hull re-lights in world space automatically.
    #[allow(clippy::too_many_arguments)]
    pub fn render_ship_pitched(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vbuf: &wgpu::Buffer,
        vcount: u32,
        yaw_deg: f32,
        center_y: f32,
        pitch_deg: f32,
    ) {
        self.render_ship_lit_framed(
            queue,
            encoder,
            vbuf,
            vcount,
            yaw_deg,
            center_y,
            -50.0,
            60.0,
            1.6,
            pitch_deg,
            HALF_EXTENT,
        );
    }

    /// Like [`Self::render_ship`] but with the KEY-LIGHT azimuth / elevation
    /// (degrees) + intensity as parameters, so a caller can SWEEP the light to
    /// show dynamic lighting (the dynamic-lighting test) — the gameplay path
    /// uses the fixed house values via [`Self::render_ship`]. Azimuth/elevation
    /// use the contract §5 basis (`dir = (cos el·sin az, sin el, cos el·cos az)`,
    /// dir-toward-light). The fill light + ambient stay fixed so only the key
    /// sweeps (the readable single-light demo).
    #[allow(clippy::too_many_arguments)]
    pub fn render_ship_lit(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vbuf: &wgpu::Buffer,
        vcount: u32,
        yaw_deg: f32,
        center_y: f32,
        key_az_deg: f32,
        key_el_deg: f32,
        key_intensity: f32,
    ) {
        // Gameplay framing: the house pitch + zoom. The demo path
        // ([`Self::render_ship_lit_framed`]) overrides pitch/zoom for a readable
        // single-ship capture.
        self.render_ship_lit_framed(
            queue,
            encoder,
            vbuf,
            vcount,
            yaw_deg,
            center_y,
            key_az_deg,
            key_el_deg,
            key_intensity,
            CAMERA_PITCH_DEG,
            HALF_EXTENT,
        );
    }

    /// As [`Self::render_ship_lit`] but with CAMERA pitch + ortho half-height
    /// (zoom) as parameters — for the dynamic-lighting test, which frames a
    /// single long hull tighter + steeper than the gameplay defaults so the deck
    /// shows and the light sweep reads. Gameplay never calls this directly.
    #[allow(clippy::too_many_arguments)]
    pub fn render_ship_lit_framed(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vbuf: &wgpu::Buffer,
        vcount: u32,
        yaw_deg: f32,
        center_y: f32,
        key_az_deg: f32,
        key_el_deg: f32,
        key_intensity: f32,
        pitch_deg: f32,
        half_extent: f32,
    ) {
        // (#76) Aspect from the RUNTIME ship res (cyclable) — the presets are all
        // ~1.6:1 so a cycle preserves the hull aspect, but reading it live keeps
        // the ortho framing correct for any future non-1.6 preset too.
        let aspect = self.low_w as f32 / self.low_h as f32;
        // The stance `yaw_deg` is the CAMERA yaw (orbit the ¾ camera around a
        // fixed hull at identity); pitch + zoom are parameters here. The camera
        // orbits about +Y (up), so the hull never tips vertical. `center_y` is
        // the hull's bbox vertical centre — the camera looks at THAT so the hull
        // sits centred in the texture.
        let view_proj = camera_view_proj_zoom(
            yaw_deg.to_radians(),
            pitch_deg.to_radians(),
            aspect,
            center_y,
            half_extent,
        );
        let model = identity4();

        // Key light: parameterised az/el (contract §5 basis). The fixed cool
        // fill (4,2,-3) + ambient stay constant so the SWEEP reads as one light
        // moving. Dir-toward-light = +position (three.js DirectionalLights shine
        // position→origin).
        let laz = key_az_deg.to_radians();
        let lel = key_el_deg.to_radians();
        let key_dir = normalize3([lel.cos() * laz.sin(), lel.sin(), lel.cos() * laz.cos()]);
        let fill_dir = normalize3([4.0, 2.0, -3.0]);
        let amb = Self::ambient();

        let scene = SceneUniform {
            view_proj,
            model,
            key_dir: [key_dir[0], key_dir[1], key_dir[2], key_intensity],
            fill_dir: [fill_dir[0], fill_dir[1], fill_dir[2], 0.45],
            ambient: [amb[0], amb[1], amb[2], 1.0],
        };
        queue.write_buffer(&self.scene_ubo, 0, bytemuck::bytes_of(&scene));
        queue.write_buffer(
            &self.bands_ubo,
            0,
            bytemuck::bytes_of(&PostUniform {
                bands: BANDS,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );

        // Pass 1: hull → low-res scene color (transparent background = cut-out).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loft hull pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.scene_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.hull_pipeline);
            pass.set_bind_group(0, &self.scene_bg, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.draw(0..vcount, 0..1);
        }

        // Pass 2: posterize → output texture (cut-out preserved via discard).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loft posterize pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.out_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.post_pipeline);
            pass.set_bind_group(0, &self.post_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        let _ = &self.out_tex; // kept alive; view borrows it
    }
}

/// (#76) Create the loft offscreen targets at `(w, h)`: the scene-color view,
/// the depth view, and the posterized-out texture + view. Shared by
/// [`LoftGpu::new`] (initial) and [`LoftGpu::resize`] (live ship-res cycle) so
/// the size + usage flags stay in ONE place. `out_tex` is returned (not just its
/// view) because the headless readback copies it.
fn make_loft_targets(
    device: &wgpu::Device,
    w: u32,
    h: u32,
) -> (
    wgpu::TextureView,
    wgpu::TextureView,
    wgpu::Texture,
    wgpu::TextureView,
) {
    let mk = |label, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let scene_tex = mk(
        "loft scene color",
        LOW_FORMAT,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    );
    let scene_view = scene_tex.create_view(&Default::default());
    let depth_tex = mk(
        "loft depth",
        DEPTH_FORMAT,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    let depth_view = depth_tex.create_view(&Default::default());
    let out_tex = mk(
        "loft posterized out",
        LOW_FORMAT,
        // COPY_SRC so the headless capture (read_output_png) can copy this back;
        // additive — the gameplay blit samples it as TEXTURE_BINDING unchanged.
        wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
    );
    let out_view = out_tex.create_view(&Default::default());
    (scene_view, depth_view, out_tex, out_view)
}

// ---------------------------------------------------------------------------
// Camera math — orthographic ¾ (port of the loft editor's setCam / the POC).
// Column-major mat4 (`c*4 + r`); right-handed; clip z in 0..1 for wgpu.
// ---------------------------------------------------------------------------

/// Orthographic ¾ view-projection with the ortho half-height (framing zoom) a
/// parameter. Gameplay passes [`HALF_EXTENT`] (the worst-case-stance zoom: one
/// fixed value across all ships so scale is preserved and a ship doesn't pop
/// size on reorient, sized so the perpendicular broadside ship's 12u length
/// clears the box vertically — #49, half=7 → 14u tall box); the dynamic-lighting
/// test frames a single hull tighter.
fn camera_view_proj_zoom(
    yaw_rad: f32,
    pitch_rad: f32,
    aspect: f32,
    target_y: f32,
    half: f32,
) -> [f32; 16] {
    let r = 30.0;
    // Orbit the camera around the look-AT point (0, target_y, 0), not the world
    // origin, so a hull whose mass doesn't straddle y=0 still frames centred.
    let target = [0.0, target_y, 0.0];
    let eye = [
        r * pitch_rad.cos() * yaw_rad.sin(),
        target_y + r * pitch_rad.sin(),
        r * pitch_rad.cos() * yaw_rad.cos(),
    ];
    let view = look_at(eye, target, [0.0, 1.0, 0.0]);
    let proj = ortho(-half * aspect, half * aspect, -half, half, 0.1, 100.0);
    mul4(proj, view)
}

/// Ortho half-height (world units) — the gameplay framing zoom. See
/// [`camera_view_proj_zoom`]. 7.0 clears the broadside ship's vertical
/// projection.
const HALF_EXTENT: f32 = 7.0;

/// (#73) CHASE-CAM base yaw (degrees): the loft camera orbits about `+Y` and the
/// hull's LENGTH is `+X` (prow `+X`, stern/engines `−X`). `270°` (−90) puts the
/// STERN toward the viewer with the bow UP-LANE (toward the vanishing point) —
/// the stern-on chase view = the facing-`N` case. The per-facing ground-yaw
/// (`N=0`, `E=+90`, `S=180`, `W=−90`) + the lane-aim convergence are added on
/// top by [`chase_cam_ground_yaw_deg`].
pub const CHASE_CAM_BASE_YAW_DEG: f32 = 270.0;

/// (#73) THE single source for the player loft hull's ground-plane camera yaw
/// (degrees), shared by the live renderer ([`crate::gfx`]) and the deterministic
/// bow gate so verification tests the REAL render path (the earlier camera-
/// perspective oracle tested the WRONG camera — the scene-space pinhole, not this
/// ortho loft camera — which is why the live bow stayed wrong while the oracle
/// passed).
///
/// The hull renders FLAT on the grid (Bruce's hard requirement: no barrel-roll);
/// only its ground-plane heading turns, via this yaw fed to the ortho loft
/// camera. Composes two flat terms:
///   - [`CHASE_CAM_BASE_YAW_DEG`] (270 = stern-on, bow up-lane),
///   - `facing_yaw_deg` — the tactical-facing offset (`N`=0 / `E`=+90 / `S`=180
///     / `W`=−90), so the four cardinals read as distinct flat poses.
///
/// This yaw stays the clean CARDINAL ground pose (no lane-lean) — it sets which way
/// the hull FACES in 3D, kept facing-correct for the firing read. The 2-D SCREEN
/// lane-lean Bruce wants (#186 — hull long-axis parallel to the converging column
/// lines) is applied SEPARATELY as a screen-space roll of the loft BLIT quad, via
/// [`loft_blit_roll_deg`]; it cannot be done here because the loft camera's shallow
/// chase pitch caps the achievable on-screen tilt-per-ground-yaw far short of the
/// steep off-centre lane slopes (a ground yaw big enough to match would swing the
/// hull toward broadside). See [`loft_blit_roll_deg`] for the geometry.
///
/// `aim_at` / `cfg` / `pitch_deg` stay in the signature (the live render + the bow
/// gate pass them) but no longer affect this ground yaw.
pub fn chase_cam_ground_yaw_deg(
    aim_at: [f32; 2],
    facing_yaw_deg: f32,
    cfg: &crate::projector::ProjectorConfig,
    pitch_deg: f32,
) -> f32 {
    let _ = (aim_at, cfg, pitch_deg);
    CHASE_CAM_BASE_YAW_DEG + facing_yaw_deg
}

/// (#186 Bruce) The 2-D SCREEN-SPACE roll (degrees, clockwise-positive in y-down
/// screen space) to apply to the loft BLIT quad so the hull's long axis is PIXEL-
/// PARALLEL to its column's lane lines as they converge to the vanishing point —
/// "clearly parallel with the lane lines towards the vanishing point". This is the
/// fix that supersedes #173's no-lean (which drew the hull straight up-screen while
/// its lane slanted to the VP — the rejected #170 "square to the screen" look).
///
/// WHY a 2-D blit roll, not a ground yaw: the lane lines at an off-centre column are
/// STEEPLY diagonal on screen (e.g. ~70° off vertical at the near-left cell at boot
/// pitch, since the VP sits only a little above the near row). The ortho loft
/// camera's shallow chase pitch turns a ground-plane yaw into only a small on-screen
/// tilt (≈40° of screen slope even at a full 90° ground yaw), so it physically can't
/// reach the lane slope without rotating the hull most of the way to broadside. A
/// pure 2-D roll of the finished hull sprite hits the EXACT lane angle and keeps the
/// hull's flat ground pose (facing stays correct). The hull stays flat on the deck —
/// this rolls the on-screen silhouette, it does NOT bank the hull off the ground.
///
/// Returns the roll for the along-lane N/S facings (`facing_yaw_deg` ≈ 0 or 180) and
/// ZERO for E/W broadside hulls (#172 — grid rows are screen-horizontal, so an
/// across-grid hull stays horizontal). The angle is the lane direction's tilt from
/// vertical (`atan2(lvx, −lvy)` of `lane = vp − aim_at`, scene px y-down); a centred
/// column has ~zero horizontal lane component → ~0 roll. The SAME angle serves N and
/// S — S's long axis is the same lane line (the hull image already faces bow-down via
/// the ground yaw 180), so rolling the whole sprite by the lane tilt keeps it
/// parallel either way.
#[must_use]
pub fn loft_blit_roll_deg(
    aim_at: [f32; 2],
    facing_yaw_deg: f32,
    cfg: &crate::projector::ProjectorConfig,
) -> f32 {
    // Only the along-lane N/S facings lean; E/W stay horizontal (#172).
    let f = facing_yaw_deg.rem_euclid(360.0);
    let along_lane = f < 1.0 || (f - 180.0).abs() < 1.0 || f > 359.0;
    if !along_lane {
        return 0.0;
    }
    let vp = crate::projector::vanishing_point(cfg);
    let (lvx, lvy) = (vp.x - aim_at[0], vp.y - aim_at[1]);
    if lvx.hypot(lvy) < 1e-4 {
        return 0.0;
    }
    // Lane tilt from straight-UP (screen −y). atan2(x, -y_down) = angle CW from up.
    // Centre column: lvx≈0 → 0. Left column (lvx>0, toward VP up-right): positive
    // (roll the bow clockwise/right to point up the slanted lane).
    lvx.atan2(-lvy).to_degrees()
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let m = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if m < 1e-8 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / m, v[1] / m, v[2] / m]
    }
}

/// Column-major identity. The hull is rendered un-transformed (model = identity)
/// — stance comes from the camera yaw, exactly as the POC does it.
const fn identity4() -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let f = normalize3(sub(eye, center));
    let s = normalize3(cross(up, f));
    let u = cross(f, s);
    [
        s[0],
        u[0],
        f[0],
        0.0,
        s[1],
        u[1],
        f[1],
        0.0,
        s[2],
        u[2],
        f[2],
        0.0,
        -dot(s, eye),
        -dot(u, eye),
        -dot(f, eye),
        1.0,
    ]
}

fn ortho(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> [f32; 16] {
    let rl = right - left;
    let tb = top - bottom;
    let fln = far - near;
    [
        2.0 / rl,
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / tb,
        0.0,
        0.0,
        0.0,
        0.0,
        -1.0 / fln,
        0.0,
        -(right + left) / rl,
        -(top + bottom) / tb,
        -near / fln,
        1.0,
    ]
}

fn mul4(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = sum;
        }
    }
    out
}

/// Expand an [`ImportedShip`]'s per-group materials into per-vertex albedo +
/// emissive (both parallel to `ship.mesh.positions`). `colors[i]` is the
/// material base RGB; `emissive[i]` is `[er, eg, eb, unlit]` (xyz linear
/// emissive, w = 1.0 when the material is unlit). Vertices outside every
/// group, or with an out-of-range material index, fall back to the default
/// hull grey + no emissive + lit. Pure — unit-tested headless; `upload_imported`
/// is the thin GPU wrapper.
fn imported_vertex_attrs(
    ship: &crate::mesh_import::ImportedShip,
) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let n = ship.mesh.positions.len();
    let mut colors = vec![DEFAULT_HULL_ALBEDO; n];
    let mut emissive = vec![[0.0f32, 0.0, 0.0, 0.0]; n];
    for g in &ship.group_ranges {
        let mat = ship.materials.get(g.material).copied().unwrap_or_default();
        let rgb = [mat.color[0], mat.color[1], mat.color[2]];
        let emis = [
            mat.emissive[0],
            mat.emissive[1],
            mat.emissive[2],
            if mat.unlit { 1.0 } else { 0.0 },
        ];
        let end = (g.start + g.len).min(n);
        for i in g.start..end {
            colors[i] = rgb;
            emissive[i] = emis;
        }
    }
    (colors, emissive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_yaws_are_distinct_per_stance() {
        let fore = orientation_yaw_deg(Orientation::BowOn { bow: LaneEnd::Fore });
        let aft = orientation_yaw_deg(Orientation::BowOn { bow: LaneEnd::Aft });
        let broad = orientation_yaw_deg(Orientation::Broadside);
        assert!((fore - STANCE_YAW_FORE).abs() < 1e-6);
        assert!((aft - STANCE_YAW_AFT).abs() < 1e-6);
        assert!((broad - STANCE_YAW_BROADSIDE).abs() < 1e-6);
        assert_ne!(fore, aft);
        assert_ne!(fore, broad);
        // #47 stance mapping: bow-on PARALLEL to the lane (Fore 0 / Aft 180),
        // broadside perpendicular (90). The ¾ comes from the top-down pitch.
        assert_eq!(fore, 0.0);
        assert_eq!(aft, 180.0);
        assert_eq!(broad, 90.0);
    }

    #[test]
    fn pose_at_rest_holds_orientation_yaw_within_idle() {
        let pose = ShipPose::new(Orientation::Broadside);
        // No tween; yaw is the base ± the tiny idle roll.
        let base = orientation_yaw_deg(Orientation::Broadside);
        assert!((pose.yaw_deg() - base).abs() <= IDLE_ROLL_DEG + 1e-3);
        assert!(!pose.is_animating());
    }

    #[test]
    fn reorient_tweens_then_settles() {
        let mut pose = ShipPose::new(Orientation::BowOn { bow: LaneEnd::Fore });
        pose.reorient_to(Orientation::Broadside);
        assert!(
            pose.is_animating(),
            "a flip to a different yaw starts a tween"
        );
        // Halfway: yaw is between the two stance yaws.
        pose.advance(REORIENT_SECS * 0.5);
        let mid = pose.yaw_deg();
        let (lo, hi) = (
            STANCE_YAW_FORE.min(STANCE_YAW_BROADSIDE),
            STANCE_YAW_FORE.max(STANCE_YAW_BROADSIDE),
        );
        assert!(
            mid > lo - IDLE_ROLL_DEG && mid < hi + IDLE_ROLL_DEG,
            "mid yaw {mid} between stances"
        );
        // Past the duration: tween clears, settles at broadside.
        pose.advance(REORIENT_SECS);
        assert!(!pose.is_animating());
        let base = orientation_yaw_deg(Orientation::Broadside);
        assert!((pose.yaw_deg() - base).abs() <= IDLE_ROLL_DEG + 1e-3);
    }

    #[test]
    fn idle_advances_and_stays_bounded() {
        let mut pose = ShipPose::new(Orientation::Broadside);
        let base = orientation_yaw_deg(Orientation::Broadside);
        for _ in 0..200 {
            pose.advance(0.016);
            assert!((pose.yaw_deg() - base).abs() <= IDLE_ROLL_DEG + 1e-3);
            assert!(pose.idle_bob().abs() <= IDLE_BOB_PX + 1e-3);
        }
    }

    #[test]
    fn camera_view_proj_is_finite() {
        let m = camera_view_proj_zoom(
            28f32.to_radians(),
            26f32.to_radians(),
            1.6,
            -0.5,
            HALF_EXTENT,
        );
        assert!(m.iter().all(|v| v.is_finite()));
    }

    /// (#76) Ship-res presets cycle forward/back with wrap, are all ~1.6:1 (so the
    /// dest-quad aspect stays valid + the hull doesn't re-squash, #74), and an
    /// off-list size snaps to the first preset.
    #[test]
    fn loft_res_presets_cycle_and_are_1_6_aspect() {
        // All presets ~1.6:1.
        for (w, h) in LOFT_RES_PRESETS {
            let ar = w as f32 / h as f32;
            assert!(
                (ar - 1.6).abs() < 0.02,
                "preset {w}x{h} aspect {ar} not ~1.6"
            );
        }
        // Forward wraps through the whole list back to the start.
        let mut cur = LOFT_RES_PRESETS[0];
        for _ in 0..LOFT_RES_PRESETS.len() {
            cur = next_loft_res(cur.0, cur.1);
        }
        assert_eq!(cur, LOFT_RES_PRESETS[0], "forward cycle wraps to start");
        // Back is the inverse of forward.
        for p in LOFT_RES_PRESETS {
            assert_eq!(
                prev_loft_res(next_loft_res(p.0, p.1).0, next_loft_res(p.0, p.1).1),
                p
            );
        }
        // Off-list size snaps to the first preset (defensive).
        assert_eq!(next_loft_res(999, 999), LOFT_RES_PRESETS[0]);
        assert_eq!(prev_loft_res(999, 999), LOFT_RES_PRESETS[0]);
        // LOW_W/LOW_H is the chunky FLOOR preset (preset[0]) — still a valid cycle
        // stop, no longer the boot default.
        assert_eq!((LOW_W, LOW_H), LOFT_RES_PRESETS[0]);
        // The BOOT default (Bruce's pick) is 480×300, the crisp end — and must be
        // an in-cycle preset so `,`/`.` from it land cleanly (not the off-list snap).
        assert_eq!(DEFAULT_LOFT_RES, (480, 300));
        assert!(
            LOFT_RES_PRESETS.contains(&DEFAULT_LOFT_RES),
            "boot default must be one of the cyclable presets"
        );
    }

    /// Project a hull-LOCAL point through the REAL gameplay ortho loft camera at
    /// `yaw_deg` (the exact `render_ship` call) → loft-target NDC `(x, y)`. Ortho
    /// ⇒ `w = 1`, no perspective divide; column-major. The downstream blit maps
    /// the loft target's NDC-x linearly to screen-x (no flip) and NDC-y to a
    /// y-down dest quad, so NDC-x sign == screen-x sign and **NDC +y == screen
    /// UP**. Testing in NDC therefore tests the ACTUAL rendered bow direction.
    fn bow_loft_ndc(yaw_deg: f32, p: [f32; 3]) -> (f32, f32) {
        bow_loft_ndc_pitched(yaw_deg, p, CAMERA_PITCH_DEG)
    }

    /// As [`bow_loft_ndc`] but at an arbitrary camera `pitch_deg` — for the #155
    /// gate that projects the bow at the LIVE loft pitch as the grid tilts.
    fn bow_loft_ndc_pitched(yaw_deg: f32, p: [f32; 3], pitch_deg: f32) -> (f32, f32) {
        let aspect = LOW_W as f32 / LOW_H as f32;
        let m = camera_view_proj_zoom(
            yaw_deg.to_radians(),
            pitch_deg.to_radians(),
            aspect,
            0.0, // bbox centre y; the bow-vs-centre Δ is centre-y independent
            HALF_EXTENT,
        );
        let x = m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12];
        let y = m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13];
        (x, y)
    }

    /// (#73 LIVE-PATH bow gate) THE deterministic verification that the player
    /// hull's bow points the right way at all 4 cardinal facings × columns
    /// 0/2/4 — covering the REAL render path: the SAME ortho loft camera
    /// (`camera_view_proj_zoom` at [`CAMERA_PITCH_DEG`]/[`HALF_EXTENT`]) the
    /// renderer uses, posed by the SAME yaw formula
    /// ([`chase_cam_ground_yaw_deg`]) it uses, with the bow read in loft-target
    /// NDC (which the blit maps sign-preserving to screen). This is the gate the
    /// earlier camera-perspective oracle FAILED to be — it tested the scene-space
    /// pinhole, a DIFFERENT camera, so it passed green while the live ortho bow
    /// pointed the wrong way at off-centre columns.
    ///
    /// The hull stays FLAT (no roll); only its ground heading turns. Per facing,
    /// the projected bow (local `+X`, the prow) must sit, relative to the hull
    /// centre:
    ///   - `N` → ABOVE (NDC +y, up-lane) AND banked toward the VP-x at off-centre
    ///     columns (col 0 → right, col 4 → left — parallel to the lane, NEVER
    ///     toward the screen edge),
    ///   - `S` → BELOW (toward the camera),
    ///   - `E` → screen-RIGHT, `W` → screen-LEFT.
    ///
    /// (#186 Bruce) The along-lane (N/S) bow runs PIXEL-PARALLEL to the cell's lane
    /// lines — "clearly parallel with the lane lines towards the vanishing point". The
    /// no-lean #173 state drew the hull straight up-screen while its lane slanted to the
    /// VP (Bruce rejected it, the #170 "square to the screen" look). The lean is
    /// restored and solved numerically so the RENDERED long-axis screen angle equals the
    /// drawn lane direction (cell-centre → VP). The centre column has ~zero lane slope →
    /// no lean. E/W broadside hulls stay HORIZONTAL (#172 — grid rows are
    /// screen-horizontal). Asserted by the 2-D cross product of the rendered axis (in
    /// y-down screen space) with the lane vector being ~0 (parallel).
    #[test]
    fn live_loft_bow_points_correctly_all_facings_and_columns() {
        use crate::grid::{Dir4, Facing, Pos, COLS, ROWS};
        use crate::projector::{grid_cell_quad, vanishing_point, ProjectorConfig};

        let cfg = ProjectorConfig::default();
        let bow_local = [3.0_f32, 0.0, 0.0]; // prow +X, ~half hull length
        let ctr_local = [0.0_f32, 0.0, 0.0];
        let row = ROWS - 1; // the player's front row
        let vp = vanishing_point(&cfg);

        // facing_yaw_deg mirrors hud::loft_facing_ground_yaw for the cardinals.
        let facing_yaw = |f: Facing| match f {
            Facing::Bow(Dir4::N) => 0.0_f32,
            Facing::Bow(Dir4::E) => 90.0,
            Facing::Bow(Dir4::S) => 180.0,
            Facing::Bow(Dir4::W) => -90.0,
            _ => unreachable!("cardinal bow facings only"),
        };

        for &col in &[0usize, COLS / 2, COLS - 1] {
            let pos = Pos::new(col, row);
            let aim_at = grid_cell_quad(pos, &cfg).center;
            // Lane direction toward the VP, in scene px (y-down).
            let (lvx, lvy) = (vp.x - aim_at[0], vp.y - aim_at[1]);
            for facing in [
                Facing::Bow(Dir4::N),
                Facing::Bow(Dir4::S),
                Facing::Bow(Dir4::E),
                Facing::Bow(Dir4::W),
            ] {
                // The GROUND yaw is the clean cardinal pose (no lean) — it sets which
                // way the hull FACES in 3D. The screen lane-lean is the separate blit
                // roll, checked below.
                let yaw =
                    chase_cam_ground_yaw_deg(aim_at, facing_yaw(facing), &cfg, CAMERA_PITCH_DEG);
                let (bx, by) = bow_loft_ndc(yaw, bow_local);
                let (cx, cy) = bow_loft_ndc(yaw, ctr_local);
                let (dx, dy) = (bx - cx, by - cy);
                let roll = loft_blit_roll_deg(aim_at, facing_yaw(facing), &cfg);
                match facing {
                    Facing::Bow(Dir4::N) => {
                        assert!(
                            dy > 1e-3 && dx.abs() < 1e-3,
                            "col {col} N: ground pose bow straight UP (cardinal); dx={dx:.4} dy={dy:.4}"
                        );
                        // (#186) The blit roll, applied to the screen up-vector (0,-1)
                        // in y-down space, must align with the lane (lvx, lvy): cross ≈ 0.
                        let (rx, ry) = roll_up_vector(roll);
                        let crossp = rx * lvy - ry * lvx;
                        let scale = lvx.hypot(lvy);
                        assert!(
                            crossp.abs() < 1e-3 * scale.max(1.0),
                            "col {col} N: rolled hull must be PARALLEL to lane (roll={roll:.2}, cross={crossp:.5}, lane=({lvx:.2},{lvy:.2}))"
                        );
                    }
                    Facing::Bow(Dir4::S) => {
                        assert!(
                            dy < -1e-3 && dx.abs() < 1e-3,
                            "col {col} S: ground pose bow straight DOWN (cardinal); dx={dx:.4} dy={dy:.4}"
                        );
                        // Same lane line as N; roll keeps the long axis parallel.
                        let (rx, ry) = roll_up_vector(roll);
                        let crossp = rx * lvy - ry * lvx;
                        let scale = lvx.hypot(lvy);
                        assert!(
                            crossp.abs() < 1e-3 * scale.max(1.0),
                            "col {col} S: rolled hull must be PARALLEL to lane (roll={roll:.2}, cross={crossp:.5})"
                        );
                    }
                    Facing::Bow(Dir4::E) => {
                        assert!(
                            dx > 1e-3,
                            "col {col} E: bow must be screen-RIGHT of centre; dx={dx:.4}"
                        );
                        // (#172) BROADSIDE hulls stay HORIZONTAL — no lane lean, so the
                        // blit roll is ZERO in every column.
                        assert!(
                            roll.abs() < 1e-4,
                            "col {col} E: broadside blit roll must be 0 (horizontal); roll={roll:.4}"
                        );
                    }
                    Facing::Bow(Dir4::W) => {
                        assert!(
                            dx < -1e-3,
                            "col {col} W: bow must be screen-LEFT of centre; dx={dx:.4}"
                        );
                        assert!(
                            roll.abs() < 1e-4,
                            "col {col} W: broadside blit roll must be 0 (horizontal); roll={roll:.4}"
                        );
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    /// The screen up-vector `(0, -1)` (y-down) rolled CW by `roll_deg` — the direction
    /// the rolled hull's bow points on screen. Used to verify [`loft_blit_roll_deg`]
    /// aligns the hull with the lane.
    fn roll_up_vector(roll_deg: f32) -> (f32, f32) {
        let a = roll_deg.to_radians();
        // CW rotation of (0,-1) in y-down screen space: x = sin a, y = -cos a.
        (a.sin(), -a.cos())
    }

    /// (#76 scene-res POSE BUG regression; #171) The player's chase-cam ground yaw
    /// must be IDENTICAL at every scene-resolution preset. The #76 bug was a lane-aim
    /// vanishing point computed from a different projector than `aim_at`, so the hull
    /// yawed ~20deg on a `;`/`'` toggle. With `aim_at` AND the VP both from the same
    /// `for_scene` cfg the yaw is invariant across presets (all presets are 16:9, so
    /// the scaled geometry is similar → identical angles). Covers the centred column
    /// (exactly the base yaw, no lean) and off-centre columns (nonzero lean, but the
    /// SAME at every res). The lane-parallel lean is back since #171; this guards that
    /// it doesn't become scene-res-DEPENDENT.
    #[test]
    fn player_chase_yaw_is_identical_across_scene_presets() {
        use crate::gfx::SCENE_RES_PRESETS;
        use crate::grid::{Dir4, Facing, Pos, COLS, ROWS};
        use crate::projector::{grid_cell_quad, ProjectorConfig};

        let facing_yaw = |f: Facing| match f {
            Facing::Bow(Dir4::N) => 0.0_f32,
            Facing::Bow(Dir4::E) => 90.0,
            Facing::Bow(Dir4::S) => 180.0,
            Facing::Bow(Dir4::W) => -90.0,
            _ => unreachable!("cardinal bow facings only"),
        };
        let row = ROWS - 1; // the player's front row
        for &col in &[0usize, COLS / 2, COLS - 1] {
            for facing in [
                Facing::Bow(Dir4::N),
                Facing::Bow(Dir4::S),
                Facing::Bow(Dir4::E),
                Facing::Bow(Dir4::W),
            ] {
                // The yaw at the 480x270 default is the reference.
                let ref_cfg = ProjectorConfig::for_scene(480.0, 270.0);
                let ref_aim = grid_cell_quad(Pos::new(col, row), &ref_cfg).center;
                let ref_yaw = chase_cam_ground_yaw_deg(
                    ref_aim,
                    facing_yaw(facing),
                    &ref_cfg,
                    CAMERA_PITCH_DEG,
                );
                for &(w, h) in &SCENE_RES_PRESETS {
                    let cfg = ProjectorConfig::for_scene(w as f32, h as f32);
                    let aim = grid_cell_quad(Pos::new(col, row), &cfg).center;
                    let yaw =
                        chase_cam_ground_yaw_deg(aim, facing_yaw(facing), &cfg, CAMERA_PITCH_DEG);
                    assert!(
                        (yaw - ref_yaw).abs() < 1e-3,
                        "scene {w}x{h} col {col} {facing:?}: yaw {yaw:.4} != default {ref_yaw:.4} \
                         (lane-aim VP must scale WITH aim_at — the ship must NOT rotate on a scene-res toggle)"
                    );
                }
                // (#186) The GROUND yaw is the clean cardinal pose in EVERY column —
                // the screen lane-lean lives in the blit roll, not the ground yaw, so
                // this stays exactly base+facing (no lean) and scene-res-invariant.
                let base = CHASE_CAM_BASE_YAW_DEG + facing_yaw(facing);
                assert!(
                    (ref_yaw - base).abs() < 1e-2,
                    "col {col} {facing:?}: ground yaw {ref_yaw:.4} should be base {base:.4} (cardinal; lean is the blit roll)"
                );
            }
        }
    }

    /// (#186 Bruce) The blit ROLL (the 2-D screen lean Bruce wants) must be scene-res-
    /// INVARIANT — a `;`/`'` toggle must not change the hull's on-screen tilt. All
    /// presets are 16:9 so the lane geometry is similar → identical roll angles.
    #[test]
    fn loft_blit_roll_is_identical_across_scene_presets() {
        use crate::gfx::SCENE_RES_PRESETS;
        use crate::grid::{Pos, COLS, ROWS};
        use crate::projector::{grid_cell_quad, ProjectorConfig};

        let row = ROWS - 1;
        for &col in &[0usize, COLS / 2, COLS - 1] {
            for facing_yaw in [0.0_f32, 180.0, 90.0, -90.0] {
                let ref_cfg = ProjectorConfig::for_scene(480.0, 270.0);
                let ref_aim = grid_cell_quad(Pos::new(col, row), &ref_cfg).center;
                let ref_roll = loft_blit_roll_deg(ref_aim, facing_yaw, &ref_cfg);
                for &(w, h) in &SCENE_RES_PRESETS {
                    let cfg = ProjectorConfig::for_scene(w as f32, h as f32);
                    let aim = grid_cell_quad(Pos::new(col, row), &cfg).center;
                    let roll = loft_blit_roll_deg(aim, facing_yaw, &cfg);
                    assert!(
                        (roll - ref_roll).abs() < 1e-2,
                        "scene {w}x{h} col {col} fy={facing_yaw}: roll {roll:.3} != default {ref_roll:.3}"
                    );
                }
                // Centre column along-lane + all E/W = ~0 roll; off-centre along-lane ≠ 0.
                let is_along = facing_yaw.abs() < 1.0 || (facing_yaw - 180.0).abs() < 1.0;
                if is_along && col != COLS / 2 {
                    assert!(
                        ref_roll.abs() > 1e-2,
                        "col {col} along-lane: off-centre roll must be nonzero; got {ref_roll:.4}"
                    );
                } else {
                    assert!(
                        ref_roll.abs() < 1e-2,
                        "col {col} fy={facing_yaw}: roll must be ~0 (centre col or broadside); got {ref_roll:.4}"
                    );
                }
            }
        }
    }

    /// (#186 Bruce, supersedes the #173 no-lean test) The rendered N bow is PIXEL-
    /// PARALLEL to its column's lane lines (cell-centre → VP) in EVERY column AND at
    /// every grid-pitch step — "clearly parallel with the lane lines towards the
    /// vanishing point". The centre column has ~zero lane slope ⇒ ~straight up; an
    /// off-centre column banks to match its slanted lane. Verified by the 2-D cross of
    /// the rendered axis (screen y-down) with the lane vector being ~0 at every pitch.
    #[test]
    fn hull_bow_is_lane_parallel_all_columns_and_pitches() {
        use crate::grid::{Pos, COLS, ROWS};
        use crate::projector::{grid_cell_quad, vanishing_point, ProjectorConfig};

        let row = ROWS - 1; // the player's front row
        let facing_yaw = 0.0_f32; // N, up-lane
                                  // Live loft pitch across the arc (mirrors gfx::loft_pitch_deg: 20 -> 82).
        let loft_pitch =
            |t: f32| CAMERA_PITCH_DEG + (crate::gfx::LOFT_PITCH_TOPDOWN_DEG - CAMERA_PITCH_DEG) * t;

        let _ = loft_pitch; // lane-lean is the (pitch-independent) 2-D blit roll
        for &col in &[0usize, COLS / 2, COLS - 1] {
            for step in 0..=8u32 {
                let t = step as f32 / 8.0;
                // The SAME pitched projector the hud builds (drawbridge mode = with_pitch).
                let cfg = ProjectorConfig::for_scene(480.0, 270.0).with_pitch(t);
                let q = grid_cell_quad(Pos::new(col, row), &cfg);
                let vp = vanishing_point(&cfg);
                let (lvx, lvy) = (vp.x - q.center[0], vp.y - q.center[1]);
                // The blit roll applied to the screen up-vector must be parallel to the
                // lane (cross ≈ 0) at every pitch — the column lines reconverge as the
                // grid tilts, and the roll tracks the live VP.
                let roll = loft_blit_roll_deg(q.center, facing_yaw, &cfg);
                let (rx, ry) = roll_up_vector(roll);
                let crossp = rx * lvy - ry * lvx;
                let scale = lvx.hypot(lvy);
                assert!(
                    crossp.abs() < 1e-3 * scale.max(1.0),
                    "col {col} step {step} t={t:.2}: rolled hull must be PARALLEL to lane (roll={roll:.2}, cross={crossp:.5}, lane=({lvx:.2},{lvy:.2}))"
                );
            }
        }
    }

    #[test]
    fn imported_colors_expand_groups_and_fall_back_to_grey() {
        use crate::loft::HullMesh;
        use crate::mesh_import::{GroupRange, ImportLight, ImportedShip, MeshMaterial};
        // 9 vertices (3 tris). Group 0 (verts 0..3) = red, group 1 (3..6) =
        // green; verts 6..9 are in no group → default grey.
        let mesh = HullMesh {
            positions: vec![[0.0; 3]; 9],
            normals: vec![[0.0, 1.0, 0.0]; 9],
        };
        let ship = ImportedShip {
            mesh,
            materials: vec![
                // Group 0: plain red, lit, no emissive.
                MeshMaterial {
                    color: [1.0, 0.0, 0.0, 1.0],
                    ..Default::default()
                },
                // Group 1: green base + a green glow, marked unlit.
                MeshMaterial {
                    color: [0.0, 1.0, 0.0, 1.0],
                    emissive: [0.0, 0.8, 0.0, 1.0],
                    unlit: true,
                },
            ],
            group_ranges: vec![
                GroupRange {
                    start: 0,
                    len: 3,
                    material: 0,
                },
                GroupRange {
                    start: 3,
                    len: 3,
                    material: 1,
                },
            ],
            light: ImportLight::default(),
        };
        let (colors, emissive) = imported_vertex_attrs(&ship);
        assert_eq!(colors.len(), 9);
        assert_eq!(emissive.len(), 9);
        // Group 0: red, no emissive, lit (w == 0).
        assert_eq!(colors[0], [1.0, 0.0, 0.0]);
        assert_eq!(colors[2], [1.0, 0.0, 0.0]);
        assert_eq!(emissive[0], [0.0, 0.0, 0.0, 0.0]);
        // Group 1: green + green glow, unlit (w == 1).
        assert_eq!(colors[3], [0.0, 1.0, 0.0]);
        assert_eq!(colors[5], [0.0, 1.0, 0.0]);
        assert_eq!(emissive[3], [0.0, 0.8, 0.0, 1.0]);
        // Ungrouped verts: default grey, no emissive, lit.
        assert_eq!(colors[6], DEFAULT_HULL_ALBEDO, "ungrouped vert → grey");
        assert_eq!(colors[8], DEFAULT_HULL_ALBEDO);
        assert_eq!(emissive[6], [0.0, 0.0, 0.0, 0.0]);
    }
}
