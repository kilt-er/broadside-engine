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
//! sees 3D or depth. **House style is locked engine-wide: [`LOW_W`]×[`LOW_H`]
//! (320×200) internal, [`BANDS`] (8).**
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

/// Locked house-style internal render resolution (bruce's confirmed look:
/// 320×200 resolves the greebles, 8 posterize bands gives the smoother HD-2D
/// shading). Engine-wide, not per-ship.
pub const LOW_W: u32 = 320;
pub const LOW_H: u32 = 200;
/// Posterize band count (house style).
pub const BANDS: f32 = 8.0;

/// Default hull albedo (loft editor `0xb4c6e0`, linear-ish sRGB stored) used
/// when a [`HullMesh`] carries no per-vertex colors.
const DEFAULT_HULL_ALBEDO: [f32; 3] = [0.706, 0.776, 0.878];

const LOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Fixed look-down pitch (degrees). #47 raises this from the POC's 26° toward a
/// more top-down "tactical-map" ¾: with the bow-on ships now PARALLEL to the
/// lane (see the stance yaws below), the ¾ read comes from THIS pitch, not from
/// yawing the ships to show a front/side. Higher = more overhead. 48° gives a
/// clear top-down ¾ without flattening the hull to a plan view; bruce dials the
/// exact amount on the angle-check.
pub const CAMERA_PITCH_DEG: f32 = 48.0;

/// The canonical stance yaws (degrees), keyed by [`Orientation`], fed to
/// [`camera_view_proj`] as the CAMERA yaw with the model at IDENTITY.
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
pub fn orientation_yaw_deg(orientation: Orientation) -> f32 {
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

/// How long a bow-on↔broadside reorient tween takes (seconds).
pub const REORIENT_SECS: f32 = 0.45;
/// Idle bob/sway/roll amplitudes — low, so a resting ship "breathes" without
/// looking adrift. Roll is the visible one (a few degrees of yaw wobble); the
/// bob is a vertical pixel nudge applied by the caller via [`ShipPose::idle_bob`].
const IDLE_ROLL_DEG: f32 = 1.5;
const IDLE_BOB_PX: f32 = 1.5;
const IDLE_ROLL_HZ: f32 = 0.18;
const IDLE_BOB_HZ: f32 = 0.13;

impl ShipPose {
    pub fn new(orientation: Orientation) -> Self {
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
    pub fn is_animating(&self) -> bool {
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

const HULL_SHADER: &str = r#"
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
"#;

const POST_SHADER: &str = r#"
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
"#;

/// The loft GPU pipeline + offscreen targets. Owns the depth path; produces a
/// posterized RGBA texture view per render. The caller (`gfx`) takes that view
/// and feeds it to the existing `TexturedShip` blit.
pub struct LoftGpu {
    hull_pipeline: wgpu::RenderPipeline,
    post_pipeline: wgpu::RenderPipeline,
    scene_ubo: wgpu::Buffer,
    scene_bg: wgpu::BindGroup,
    bands_ubo: wgpu::Buffer,
    /// Low-res color target the 3D pass draws into.
    scene_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// Final posterized RGBA target (TEXTURE_BINDING so gfx can sample it).
    out_tex: wgpu::Texture,
    out_view: wgpu::TextureView,
    post_bg: wgpu::BindGroup,
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
        let mk = |label, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: LOW_W,
                    height: LOW_H,
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
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let out_view = out_tex.create_view(&Default::default());

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
        }
    }

    /// The posterized RGBA output texture view (TEXTURE_BINDING) for the gfx
    /// compositor to sample into the lane.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.out_view
    }

    /// Output dimensions (for the gfx side's UV / dest-rect math).
    pub fn output_size(&self) -> (u32, u32) {
        (LOW_W, LOW_H)
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
    ) -> (wgpu::Buffer, u32) {
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
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("loft hull vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        (buf, verts.len() as u32)
    }

    /// Upload an [`ImportedShip`] (the architect's mesh_import / CAD-glb output,
    /// and the shape the loft path also targets): expands the per-group
    /// materials onto per-vertex albedo + emissive (with the unlit flag) and
    /// delegates to [`Self::upload_hull`]. Both geometry sources reach the GPU
    /// through one path; glow surfaces (canopy / gun / battery / engine) render
    /// emissive, and unlit materials skip Lambert.
    pub fn upload_imported(
        &self,
        device: &wgpu::Device,
        ship: &crate::mesh_import::ImportedShip,
    ) -> (wgpu::Buffer, u32) {
        let (colors, emissive) = imported_vertex_attrs(ship);
        self.upload_hull(device, &ship.mesh, &colors, &emissive)
    }

    /// Like [`Self::upload_imported`] but multiplies every vertex's albedo by
    /// `tint` (per-channel) — used to recolour the shared CAD hull a distinct
    /// hue for the player so it reads apart from the enemy fleet. Emissive is
    /// left untouched so any glow accent keeps its authored colour.
    pub fn upload_imported_tinted(
        &self,
        device: &wgpu::Device,
        ship: &crate::mesh_import::ImportedShip,
        tint: [f32; 3],
    ) -> (wgpu::Buffer, u32) {
        let (mut colors, emissive) = imported_vertex_attrs(ship);
        for c in &mut colors {
            c[0] *= tint[0];
            c[1] *= tint[1];
            c[2] *= tint[2];
        }
        self.upload_hull(device, &ship.mesh, &colors, &emissive)
    }

    /// Render one ship pose into the offscreen target and posterize it into
    /// [`Self::output_view`]. `yaw_deg` is the ship's stance yaw (from
    /// [`ShipPose::yaw_deg`]) — fed to [`camera_view_proj`] as the CAMERA yaw,
    /// exactly as the POC does, with the model left at identity; pitch is fixed
    /// at [`CAMERA_PITCH_DEG`]. Records into `encoder`; the caller submits.
    pub fn render_ship(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vbuf: &wgpu::Buffer,
        vcount: u32,
        yaw_deg: f32,
    ) {
        let aspect = LOW_W as f32 / LOW_H as f32;
        // EXACTLY the POC: the stance `yaw_deg` is the CAMERA yaw (orbit the
        // ¾ camera around a fixed hull), model = identity. The camera always
        // orbits about the vertical (Y) axis with up = +Y, so the ship can
        // never tip vertical; bow-on / broadside present clean horizontal
        // profiles like the approved reference. (This replaces the #36/#37
        // model-rotation experiment that collapsed bow-on to a plank and went
        // vertical on broadside.) `yaw_deg` is static per ship (its stance);
        // idle roll + reorient tween only nudge it.
        let view_proj =
            camera_view_proj(yaw_deg.to_radians(), CAMERA_PITCH_DEG.to_radians(), aspect);
        let model = identity4();

        // Lights ported from the loft editor's setLight (laz -50, lel 60) /
        // fixed cool fill (4,2,-3). Dir-toward-light = +position (three.js
        // DirectionalLights shine position→origin).
        let laz = (-50.0f32).to_radians();
        let lel = (60.0f32).to_radians();
        let key_dir = normalize3([lel.cos() * laz.sin(), lel.sin(), lel.cos() * laz.cos()]);
        let fill_dir = normalize3([4.0, 2.0, -3.0]);
        let amb = Self::ambient();

        let scene = SceneUniform {
            view_proj,
            model,
            key_dir: [key_dir[0], key_dir[1], key_dir[2], 1.6],
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

// ---------------------------------------------------------------------------
// Camera math — orthographic ¾ (port of the loft editor's setCam / the POC).
// Column-major mat4 (`c*4 + r`); right-handed; clip z in 0..1 for wgpu.
// ---------------------------------------------------------------------------

fn camera_view_proj(yaw_rad: f32, pitch_rad: f32, aspect: f32) -> [f32; 16] {
    let r = 30.0;
    let eye = [
        r * pitch_rad.cos() * yaw_rad.sin(),
        r * pitch_rad.sin(),
        r * pitch_rad.cos() * yaw_rad.cos(),
    ];
    let view = look_at(eye, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
    // Ortho half-height in world units. Tight enough that the largest ship (the
    // dagger, ~12u long at the default 2.0 stretch) fills most of the 320×200
    // offscreen — otherwise the ship is a small island in a mostly-transparent
    // texture and reads tiny once blit into the lane. ONE fixed value across all
    // ships preserves their TRUE relative scale (the 7.75u CAD ship renders
    // ~65% of the dagger, as authored — no per-ship fudge). bruce dials true
    // ship scale at the asset source.
    let half = HALF_EXTENT;
    let proj = ortho(-half * aspect, half * aspect, -half, half, 0.1, 100.0);
    mul4(proj, view)
}

/// Ortho half-height (world units) — the framing zoom. See [`camera_view_proj`].
const HALF_EXTENT: f32 = 5.0;

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
fn identity4() -> [f32; 16] {
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
        let m = camera_view_proj(28f32.to_radians(), 26f32.to_radians(), 1.6);
        assert!(m.iter().all(|v| v.is_finite()));
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
