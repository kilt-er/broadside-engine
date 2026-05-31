//! Loft render POC — standalone spike, does NOT import any `broadside_engine`
//! module. Self-contained: the loft math, camera math, and both shaders live
//! in this one file so the spike can be judged and, if it reads right, lifted
//! into the engine without entanglement.
//!
//! Run: `cargo run --bin loft_poc --features render`
//!
//! Pipeline (mirrors docs/BROADSIDE_RENDER_PIPELINE_HANDOFF.md):
//!   1. Lofted dagger hull (`loft` mod) uploaded once as flat-shaded tris.
//!   2. Depth-tested 3D pass — orthographic ¾ camera + flat Lambert (key +
//!      fill + ambient) into a LOW-RES (160×100) offscreen color + depth
//!      target. No MSAA. (The engine is 2D-only today — no pipeline uses
//!      `depth_stencil: Some` — so this depth path is net-new, as expected.)
//!   3. Posterize pass — WGSL port of the tool's GLSL frag (HSV grade →
//!      quantize to BANDS → discard a<0.5), nearest-neighbor upscaled to the
//!      window; background pixels show a flat backdrop.
//!   4. Spins through the 4 discrete stance yaws (right 28° / left 152° /
//!      fore 118° / aft 298°) at 26° pitch, ~1.2 s each; LEFT/RIGHT arrows
//!      step stances manually and freeze the auto-spin.
//!
//! Success criterion is visual (bruce judges): does the dagger read as crisp
//! capital-ship pixel art like the browser tool?

use std::sync::Arc;
use std::time::Instant;

use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

// ===========================================================================
// loft — faithful Rust port of buildHull / sampleSection / sampleHeightProf
// from docs/broadside-loft-editor.html. Coordinate convention matches the
// three.js source: x = length (prow +x), y = height (dorsal +y), z = half-width.
// ===========================================================================
mod loft {
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    /// PLAN: `[x (0..1 stern→prow), halfWidth (0..1)]` — the dagger default.
    pub const DAGGER_PLAN: &[[f32; 2]] = &[
        [0.00, 0.95],
        [0.10, 0.98],
        [0.45, 0.72],
        [0.75, 0.42],
        [0.92, 0.18],
        [1.00, 0.02],
    ];

    /// SECTION: half cross-section `[z (0..1), y (-1..1)]`, top→chine→belly.
    pub const DAGGER_SECTION: &[[f32; 2]] = &[
        [0.00, 0.55],
        [0.55, 0.40],
        [1.00, 0.05],
        [0.60, -0.45],
        [0.00, -0.55],
    ];

    /// Ring resolution from the section (the tool's `SECN`).
    const SECN: usize = 10;

    #[derive(Clone, Copy)]
    pub struct LoftParams {
        pub stretch: f32,
        pub hscale: f32,
    }
    impl Default for LoftParams {
        fn default() -> Self {
            // ST.stretch = 2.0, ST.hscale = 0.7 in the tool.
            Self {
                stretch: 2.0,
                hscale: 0.7,
            }
        }
    }

    /// Flat 1.0 — no traced side-view profile in the default tool state.
    fn sample_height_prof(_x: f32) -> f32 {
        1.0
    }

    /// `sampleSection(t)` — piecewise-linear across SECTION, returns `[zf, y]`.
    fn sample_section(section: &[[f32; 2]], t: f32) -> (f32, f32) {
        let n = section.len();
        let f = t * (n - 1) as f32;
        let i = (f.floor() as usize).min(n - 2);
        let u = f - i as f32;
        (
            lerp(section[i][0], section[i + 1][0], u),
            lerp(section[i][1], section[i + 1][1], u),
        )
    }

    /// Lofted hull as flat-shaded triangle soup (positions + parallel normals,
    /// 3 verts per tri sharing the face normal).
    pub struct Hull {
        pub positions: Vec<[f32; 3]>,
        pub normals: Vec<[f32; 3]>,
    }

    /// Direct port of `buildHull()`.
    pub fn build_hull(plan: &[[f32; 2]], section: &[[f32; 2]], params: LoftParams) -> Hull {
        let l = 6.0 * params.stretch / 2.0;
        let h = params.hscale;

        struct Station {
            x: f32,
            w: f32,
            hm: f32,
        }
        let stations: Vec<Station> = plan
            .iter()
            .map(|p| Station {
                x: (p[0] - 0.5) * 2.0 * l,
                w: p[1],
                hm: sample_height_prof(p[0]),
            })
            .collect();

        let ring_pts = |st: &Station| -> Vec<[f32; 3]> {
            let mut pts = Vec::with_capacity((SECN - 1) * 2);
            let hh = h * st.hm;
            for s in 0..SECN {
                let (zf, y) = sample_section(section, s as f32 / (SECN - 1) as f32);
                pts.push([st.x, y * hh, zf * st.w]);
            }
            for s in (1..=(SECN - 2)).rev() {
                let (zf, y) = sample_section(section, s as f32 / (SECN - 1) as f32);
                pts.push([st.x, y * hh, -zf * st.w]);
            }
            pts
        };

        let rings: Vec<Vec<[f32; 3]>> = stations.iter().map(ring_pts).collect();
        let n = rings[0].len();

        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut push_tri = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
            let nrm = face_normal(a, b, c);
            positions.extend_from_slice(&[a, b, c]);
            normals.extend_from_slice(&[nrm, nrm, nrm]);
        };

        for s in 0..(stations.len() - 1) {
            let (ra, rb) = (&rings[s], &rings[s + 1]);
            for i in 0..n {
                let j = (i + 1) % n;
                push_tri(ra[i], ra[j], rb[i]);
                push_tri(ra[j], rb[j], rb[i]);
            }
        }
        Hull { positions, normals }
    }

    fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-8 {
            [0.0, 1.0, 0.0]
        } else {
            [n[0] / len, n[1] / len, n[2] / len]
        }
    }
}

// ===========================================================================
// math3 — minimal column-major mat4 for the ortho ¾ camera (no glam dep).
// element (row r, col c) at index c*4 + r.
// ===========================================================================
mod math3 {
    pub type Vec3 = [f32; 3];
    pub type Mat4 = [f32; 16];

    fn sub(a: Vec3, b: Vec3) -> Vec3 {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: Vec3, b: Vec3) -> Vec3 {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn dot(a: Vec3, b: Vec3) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    pub fn normalize(v: Vec3) -> Vec3 {
        let m = dot(v, v).sqrt();
        if m < 1e-8 {
            [0.0, 0.0, 0.0]
        } else {
            [v[0] / m, v[1] / m, v[2] / m]
        }
    }

    fn look_at(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
        let f = normalize(sub(eye, center));
        let s = normalize(cross(up, f));
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

    fn ortho(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Mat4 {
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

    fn mul(a: Mat4, b: Mat4) -> Mat4 {
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

    pub fn rotate_y(rad: f32) -> Mat4 {
        let (s, c) = rad.sin_cos();
        [
            c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    }

    /// The tool's `setCam`: eye on a radius-`r` sphere at (yaw, pitch), looking
    /// at origin, orthographic half-height `9/zoom`, aspect `w/h`. Returns
    /// `proj * view`.
    pub fn camera_view_proj(yaw_rad: f32, pitch_rad: f32, aspect: f32, zoom: f32) -> Mat4 {
        let r = 30.0;
        let eye = [
            r * pitch_rad.cos() * yaw_rad.sin(),
            r * pitch_rad.sin(),
            r * pitch_rad.cos() * yaw_rad.cos(),
        ];
        let view = look_at(eye, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let half = 9.0 / zoom;
        let proj = ortho(-half * aspect, half * aspect, -half, half, 0.1, 100.0);
        mul(proj, view)
    }
}

// ===========================================================================
// POC renderer
// ===========================================================================

const LOW_W: u32 = 160;
const LOW_H: u32 = 100;
const PITCH_DEG: f32 = 26.0;
const STANCE_YAWS_DEG: [f32; 4] = [28.0, 152.0, 118.0, 298.0];
const STANCE_NAMES: [&str; 4] = ["right", "left", "fore", "aft"];
const STANCE_HOLD_SECS: f32 = 1.2;

const LOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    _pad0: f32,
    normal: [f32; 3],
    _pad1: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [f32; 16],
    model: [f32; 16],
    key_dir: [f32; 4],  // xyz toward key light, w = intensity
    fill_dir: [f32; 4], // xyz toward fill light, w = intensity
    base_color: [f32; 4],
    ambient: [f32; 4],
}

const HULL_SHADER: &str = r#"
struct Scene {
    view_proj: mat4x4<f32>,
    model:     mat4x4<f32>,
    key_dir:   vec4<f32>,
    fill_dir:  vec4<f32>,
    base_color: vec4<f32>,
    ambient:    vec4<f32>,
};
@group(0) @binding(0) var<uniform> scene: Scene;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_n: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>) -> VsOut {
    let world = scene.model * vec4<f32>(pos, 1.0);
    let wn = (scene.model * vec4<f32>(nrm, 0.0)).xyz;
    var o: VsOut;
    o.clip = scene.view_proj * world;
    o.world_n = wn;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_n);
    let key = max(dot(n, normalize(scene.key_dir.xyz)), 0.0) * scene.key_dir.w;
    let fill = max(dot(n, normalize(scene.fill_dir.xyz)), 0.0) * scene.fill_dir.w;
    let lit = scene.base_color.rgb * (scene.ambient.rgb + vec3<f32>(key) + vec3<f32>(0.53, 0.67, 1.0) * fill);
    return vec4<f32>(lit, 1.0);
}
"#;

const POST_SHADER: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

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

fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.bg, K.wz), vec4<f32>(c.gb, K.xy), step(c.b, c.g));
    let q = mix(vec4<f32>(p.xyw, c.r), vec4<f32>(c.r, p.yzx), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}
fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x) + K.xyz) * 6.0 - vec3<f32>(K.w));
    return c.z * mix(vec3<f32>(K.x), clamp(p - vec3<f32>(K.x), vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

const BANDS: f32 = 4.0;
const U_HUE: f32 = 0.0;
const U_SAT: f32 = 1.0;
const U_BRI: f32 = 1.0;
const U_CON: f32 = 1.0;
const U_GAM: f32 = 1.0;
const BACKDROP: vec3<f32> = vec3<f32>(0.031, 0.047, 0.078);

@fragment
fn fs_post(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    if (c.a < 0.5) {
        return vec4<f32>(BACKDROP, 1.0);
    }
    var col = c.rgb;
    var h = rgb2hsv(col);
    h.x = fract(h.x + U_HUE);
    h.y = clamp(h.y * U_SAT, 0.0, 1.0);
    col = hsv2rgb(h);
    col = col * U_BRI;
    col = (col - vec3<f32>(0.5)) * U_CON + vec3<f32>(0.5);
    col = pow(max(col, vec3<f32>(0.0)), vec3<f32>(1.0 / U_GAM));
    col = clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
    let q = floor(col * BANDS + 0.5) / BANDS;
    return vec4<f32>(q, 1.0);
}
"#;

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    hull_pipeline: wgpu::RenderPipeline,
    vbuf: wgpu::Buffer,
    vcount: u32,
    scene_ubo: wgpu::Buffer,
    scene_bg: wgpu::BindGroup,
    low_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    post_pipeline: wgpu::RenderPipeline,
    post_bg: wgpu::BindGroup,
    start: Instant,
    last_logged: isize,
    /// `Some(i)` freezes on stance i (arrow-key stepped); `None` auto-spins.
    manual_stance: Option<usize>,
}

impl Gpu {
    async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("adapter");
        eprintln!("[loft_poc] adapter: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("loft_poc device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("device");

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

        // geometry
        let hull = loft::build_hull(
            loft::DAGGER_PLAN,
            loft::DAGGER_SECTION,
            loft::LoftParams::default(),
        );
        let verts: Vec<Vertex> = hull
            .positions
            .iter()
            .zip(hull.normals.iter())
            .map(|(p, n)| Vertex {
                pos: *p,
                _pad0: 0.0,
                normal: *n,
                _pad1: 0.0,
            })
            .collect();
        let vcount = verts.len() as u32;
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("hull vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // low-res color + depth
        let low = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("low-res target"),
            size: wgpu::Extent3d {
                width: LOW_W,
                height: LOW_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let low_view = low.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width: LOW_W,
                height: LOW_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        // scene uniform + hull pipeline
        let scene_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene ubo"),
            size: std::mem::size_of::<SceneUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene bgl"),
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
            label: Some("scene bg"),
            layout: &scene_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_ubo.as_entire_binding(),
            }],
        });

        let hull_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hull shader"),
            source: wgpu::ShaderSource::Wgsl(HULL_SHADER.into()),
        });
        let hull_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hull layout"),
            bind_group_layouts: &[&scene_bgl],
            push_constant_ranges: &[],
        });
        let hull_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hull pipeline"),
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
                cull_mode: None, // closed loft; never punch holes at the prow
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

        // posterize pipeline
        let post_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let post_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("post shader"),
            source: wgpu::ShaderSource::Wgsl(POST_SHADER.into()),
        });
        let post_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post bgl"),
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
            ],
        });
        let post_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post bg"),
            layout: &post_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&low_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&post_sampler),
                },
            ],
        });
        let post_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post layout"),
            bind_group_layouts: &[&post_bgl],
            push_constant_ranges: &[],
        });
        let post_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("post pipeline"),
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
                    format,
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
            surface,
            device,
            queue,
            config,
            hull_pipeline,
            vbuf,
            vcount,
            scene_ubo,
            scene_bg,
            low_view,
            depth_view,
            post_pipeline,
            post_bg,
            start: Instant::now(),
            last_logged: -1,
            manual_stance: None,
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
    }

    fn current_stance(&self) -> usize {
        if let Some(i) = self.manual_stance {
            i
        } else {
            let elapsed = self.start.elapsed().as_secs_f32();
            ((elapsed / STANCE_HOLD_SECS) as usize) % STANCE_YAWS_DEG.len()
        }
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let stance = self.current_stance();
        if stance as isize != self.last_logged {
            eprintln!(
                "[loft_poc] stance: {} ({}\u{b0})",
                STANCE_NAMES[stance], STANCE_YAWS_DEG[stance]
            );
            self.last_logged = stance as isize;
        }

        let yaw = STANCE_YAWS_DEG[stance].to_radians();
        let pitch = PITCH_DEG.to_radians();
        let aspect = LOW_W as f32 / LOW_H as f32;
        let view_proj = math3::camera_view_proj(yaw, pitch, aspect, 1.0);
        let model = math3::rotate_y(0.0);

        // Lights ported from the tool's setLight (laz -50, lel 60) / fixed fill
        // (4,2,-3) / ambient 0x3a4560×0.9 / hull albedo 0xb4c6e0. Three.js
        // DirectionalLight shines position→origin, so dir-toward-light = +pos.
        let laz = (-50.0f32).to_radians();
        let lel = (60.0f32).to_radians();
        let key_dir = math3::normalize([lel.cos() * laz.sin(), lel.sin(), lel.cos() * laz.cos()]);
        let fill_dir = math3::normalize([4.0, 2.0, -3.0]);
        let amb = [58.0 / 255.0 * 0.9, 69.0 / 255.0 * 0.9, 96.0 / 255.0 * 0.9];
        let base = [180.0 / 255.0, 198.0 / 255.0, 224.0 / 255.0];

        let scene = SceneUniform {
            view_proj,
            model,
            key_dir: [key_dir[0], key_dir[1], key_dir[2], 1.6],
            fill_dir: [fill_dir[0], fill_dir[1], fill_dir[2], 0.45],
            base_color: [base[0], base[1], base[2], 1.0],
            ambient: [amb[0], amb[1], amb[2], 1.0],
        };
        self.queue
            .write_buffer(&self.scene_ubo, 0, bytemuck::bytes_of(&scene));

        let frame = self.surface.get_current_texture()?;
        let swap_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        // Pass 1: hull → low-res (alpha 0 background = cut-out).
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hull pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.low_view,
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
            pass.set_vertex_buffer(0, self.vbuf.slice(..));
            pass.draw(0..self.vcount, 0..1);
        }

        // Pass 2: posterize + nearest upscale → swapchain.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("posterize pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &swap_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
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

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Broadside Loft POC — arrows step stance, else auto-spin (26\u{b0} pitch)")
            .with_inner_size(winit::dpi::LogicalSize::new(
                (LOW_W * 6) as f64,
                (LOW_H * 6) as f64,
            ));
        let window = Arc::new(event_loop.create_window(attrs).expect("window"));
        let gpu = pollster::block_on(Gpu::new(window.clone()));
        self.window = Some(window);
        self.gpu = Some(gpu);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    let n = STANCE_YAWS_DEG.len();
                    match code {
                        KeyCode::ArrowRight => {
                            let cur = gpu.current_stance();
                            gpu.manual_stance = Some((cur + 1) % n);
                        }
                        KeyCode::ArrowLeft => {
                            let cur = gpu.current_stance();
                            gpu.manual_stance = Some((cur + n - 1) % n);
                        }
                        KeyCode::Space => {
                            // Toggle back to auto-spin.
                            gpu.manual_stance = None;
                            gpu.start = Instant::now();
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = self.gpu.as_mut() {
                    match gpu.render() {
                        Ok(()) => {}
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            gpu.surface.configure(&gpu.device, &gpu.config);
                        }
                        Err(e) => eprintln!("[loft_poc] surface error: {e:?}"),
                    }
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
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run");
}
