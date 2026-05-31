//! Loft render POC — standalone spike, does NOT import any `broadside_engine`
//! module. Self-contained: the loft math, camera math, and both shaders live
//! in this one file so the spike can be judged and, if it reads right, lifted
//! into the engine without entanglement.
//!
//! Run: `cargo run --bin loft_poc --features render,runtime`
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
//!   4. SMOOTH CONTINUOUS rotation — the camera advances yaw/pitch by the
//!      wall-clock delta every frame and renders the live pose. This is the
//!      thesis: live 3D rotates smoothly at every angle for free, with NO
//!      sprite interpolation and NO baked frames. (The handoff doc's
//!      "discrete frame-stepped" framing is explicitly superseded.) Default
//!      is a slow auto-orbit; ←→ steer yaw, ↑↓ scrub pitch, Space pauses,
//!      1-4 snap to the canonical stance yaws (right/left/fore/aft) as
//!      reference points only.
//!
//! Success criterion is visual (bruce judges): does the dagger rotate
//! smoothly and read as crisp capital-ship pixel art at every angle?

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
// parts — superstructure / engines / guns / batteries / greeble primitives,
// a port of rebuild()'s attached-part layer (~line 471 in the HTML tool).
// These are what make the lofted hull read as a *capital* ship: small dense
// detail on a big hull implies scale. Emitted as flat-shaded, per-vertex
// COLORED triangles so hull + parts draw in one pass. Colors are the tool's
// material albedos.
// ===========================================================================
mod parts {
    /// Flat-shaded, per-vertex-colored triangle soup (positions/normals/colors
    /// parallel; 3 entries per tri sharing one face normal).
    #[derive(Default)]
    pub struct ColoredMesh {
        pub positions: Vec<[f32; 3]>,
        pub normals: Vec<[f32; 3]>,
        pub colors: Vec<[f32; 3]>,
    }

    impl ColoredMesh {
        fn push_tri(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], color: [f32; 3]) {
            let nrm = face_normal(a, b, c);
            self.positions.extend_from_slice(&[a, b, c]);
            self.normals.extend_from_slice(&[nrm, nrm, nrm]);
            self.colors.extend_from_slice(&[color, color, color]);
        }

        pub fn append(&mut self, other: &ColoredMesh) {
            self.positions.extend_from_slice(&other.positions);
            self.normals.extend_from_slice(&other.normals);
            self.colors.extend_from_slice(&other.colors);
        }

        /// Axis-aligned box centered at `c`, full extents `(sx,sy,sz)`.
        fn add_box(&mut self, c: [f32; 3], sx: f32, sy: f32, sz: f32, color: [f32; 3]) {
            let (hx, hy, hz) = (sx * 0.5, sy * 0.5, sz * 0.5);
            let v = |dx: f32, dy: f32, dz: f32| [c[0] + dx * hx, c[1] + dy * hy, c[2] + dz * hz];
            let p = [
                v(-1.0, -1.0, -1.0),
                v(1.0, -1.0, -1.0),
                v(1.0, 1.0, -1.0),
                v(-1.0, 1.0, -1.0),
                v(-1.0, -1.0, 1.0),
                v(1.0, -1.0, 1.0),
                v(1.0, 1.0, 1.0),
                v(-1.0, 1.0, 1.0),
            ];
            let mut quad = |a: usize, b: usize, cc: usize, d: usize| {
                self.push_tri(p[a], p[b], p[cc], color);
                self.push_tri(p[a], p[cc], p[d], color);
            };
            quad(4, 5, 6, 7);
            quad(1, 0, 3, 2);
            quad(0, 4, 7, 3);
            quad(5, 1, 2, 6);
            quad(3, 7, 6, 2);
            quad(0, 1, 5, 4);
        }

        /// UV sphere centered at `c`, radius `r`, `seg`×`ring` divisions.
        fn add_sphere(&mut self, c: [f32; 3], r: f32, seg: usize, ring: usize, color: [f32; 3]) {
            let pt = |i: usize, j: usize| {
                let v = j as f32 / ring as f32;
                let u = i as f32 / seg as f32;
                let theta = v * std::f32::consts::PI;
                let phi = u * std::f32::consts::TAU;
                [
                    c[0] + r * theta.sin() * phi.cos(),
                    c[1] + r * theta.cos(),
                    c[2] + r * theta.sin() * phi.sin(),
                ]
            };
            for j in 0..ring {
                for i in 0..seg {
                    let a = pt(i, j);
                    let b = pt(i + 1, j);
                    let cc = pt(i + 1, j + 1);
                    let d = pt(i, j + 1);
                    self.push_tri(a, b, cc, color);
                    self.push_tri(a, cc, d, color);
                }
            }
        }

        /// Cylinder whose axis runs along x (the tool's z-rotated cylinders).
        fn add_cylinder_x(
            &mut self,
            c: [f32; 3],
            rtop: f32,
            rbot: f32,
            len: f32,
            seg: usize,
            color: [f32; 3],
        ) {
            let hx = len * 0.5;
            let ring = |radius: f32, x: f32, i: usize| {
                let a = i as f32 / seg as f32 * std::f32::consts::TAU;
                [c[0] + x, c[1] + radius * a.cos(), c[2] + radius * a.sin()]
            };
            for i in 0..seg {
                let tp0 = ring(rtop, hx, i);
                let tp1 = ring(rtop, hx, i + 1);
                let bt0 = ring(rbot, -hx, i);
                let bt1 = ring(rbot, -hx, i + 1);
                self.push_tri(bt0, bt1, tp1, color);
                self.push_tri(bt0, tp1, tp0, color);
                self.push_tri([c[0] + hx, c[1], c[2]], tp0, tp1, color);
                self.push_tri([c[0] - hx, c[1], c[2]], bt1, bt0, color);
            }
        }
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

    // Tool material albedos (sRGB 0..1).
    const MAT_TOWER: [f32; 3] = [0.784, 0.847, 0.933]; // 0xc8d8ee
    const MAT_DARK: [f32; 3] = [0.235, 0.290, 0.392]; // 0x3c4a64
    const MAT_CANOPY: [f32; 3] = [0.373, 0.847, 1.0]; // 0x5fd8ff
    const MAT_GUN: [f32; 3] = [1.0, 0.541, 0.282]; // 0xff8a48
    const MAT_BATT: [f32; 3] = [1.0, 0.847, 0.420]; // 0xffd86b
    const MAT_ENGINE: [f32; 3] = [0.435, 0.878, 1.0]; // 0x6fe0ff
                                                      // Greeble accent colors — pushed to high hull-contrast so the panel
                                                      // scatter reads clearly (bruce: "make them pop"). Very-dark recess vs the
                                                      // ~0.7-grey hull, plus a warm amber light.
    const GREEB_DARK: [f32; 3] = [0.08, 0.10, 0.16];
    const GREEB_LIGHT: [f32; 3] = [1.0, 0.78, 0.42];

    fn width_at(plan: &[[f32; 2]], u: f32) -> f32 {
        let n = plan.len();
        let f = u * (n - 1) as f32;
        let i = (f.floor() as usize).clamp(0, n - 2);
        let uu = f - i as f32;
        plan[i][1] + (plan[i + 1][1] - plan[i][1]) * uu
    }

    /// World y of the hull's dorsal (top) skin at a station, in ship space.
    /// The dorsal apex is the first SECTION point (`[0.0, 0.55]` for the
    /// dagger: z=0, y=0.55), so seat-on-skin y = `top.y * hscale * heightMult`.
    /// heightMult is 1.0 in the default (no traced side profile), so this is
    /// effectively constant — but expressing it via the loft math keeps parts
    /// seated if a height profile is later introduced. Attached parts add their
    /// own half-height to this so the box bottom rests ON the skin.
    fn dorsal_y(section: &[[f32; 2]], h: f32) -> f32 {
        section[0][1] * h
    }
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
    /// Seeded hash (not RNG) so the greeble scatter is stable frame-to-frame.
    fn hash01(n: u32) -> f32 {
        let mut x = n.wrapping_mul(0x9e3779b1);
        x ^= x >> 15;
        x = x.wrapping_mul(0x85ebca6b);
        x ^= x >> 13;
        (x & 0x00ff_ffff) as f32 / 0x0100_0000 as f32
    }

    /// All attached parts in ship space — direct port of rebuild()'s primitive
    /// layer. `l`/`h` = hull world half-length/height; `plan` = half-width
    /// outline; `section` = cross-section profile (for seating parts on the
    /// dorsal skin); `greeb` = density (tool default 0.6).
    pub fn build_parts(
        l: f32,
        h: f32,
        plan: &[[f32; 2]],
        section: &[[f32; 2]],
        greeb: f32,
    ) -> ColoredMesh {
        let mut m = ColoredMesh::default();
        let stern_w = plan[0][1];
        // Dorsal skin height (the surface greebles sit on).
        let skin_y = dorsal_y(section, h);

        let tower_x = -l * 0.6;
        m.add_box(
            [tower_x, h * 0.7, 0.0],
            l * 0.5,
            h * 0.6,
            stern_w,
            MAT_TOWER,
        );
        m.add_box(
            [tower_x, h * 1.15, 0.0],
            l * 0.3,
            h * 0.5,
            stern_w * 0.6,
            MAT_TOWER,
        );
        m.add_box(
            [tower_x - l * 0.05, h * 1.6, 0.0],
            l * 0.12,
            h * 0.5,
            stern_w * 0.36,
            MAT_TOWER,
        );
        for z in [-0.18f32, 0.18] {
            m.add_sphere(
                [tower_x - l * 0.05, h * 1.9, z * stern_w],
                h * 0.18,
                8,
                6,
                MAT_DARK,
            );
        }
        m.add_box(
            [tower_x + l * 0.04, h * 1.85, 0.0],
            l * 0.1,
            h * 0.2,
            stern_w * 0.3,
            MAT_CANOPY,
        );

        let ne = 4usize;
        for i in 0..ne {
            // Clamp the cluster z-spread to ≤ stern half-width so every bell
            // sits within the stern face (was stern_w * 1.4 → outer bells hung
            // off the edges). Seat the bell x just inside the broad stern (the
            // hull ends at x = -l) so they read as mounted, not hovering behind.
            let z = (i as f32 / (ne - 1) as f32 - 0.5) * stern_w * 0.8;
            m.add_cylinder_x([-l * 0.9, 0.0, z], h * 0.28, h * 0.34, h * 0.3, 8, MAT_DARK);
            m.add_cylinder_x(
                [-l * 0.99, 0.0, z],
                h * 0.2,
                h * 0.2,
                h * 0.08,
                8,
                MAT_ENGINE,
            );
        }

        m.add_cylinder_x(
            [l * 1.02, 0.0, 0.0],
            h * 0.06,
            h * 0.08,
            l * 0.25,
            6,
            MAT_GUN,
        );

        let count = lerp(3.0, 14.0, greeb).round() as usize;
        for sgn in [-1.0f32, 1.0] {
            for i in 0..count {
                let t = if count > 1 {
                    i as f32 / (count - 1) as f32
                } else {
                    0.0
                };
                let sx = lerp(-l * 0.85, l * 0.6, t);
                let w_at = width_at(plan, sx / (2.0 * l) + 0.5);
                m.add_box(
                    [sx, h * 0.12, sgn * w_at * 0.98],
                    l * 0.05,
                    h * 0.08,
                    h * 0.08,
                    MAT_BATT,
                );
            }
        }

        if greeb > 0.05 {
            // bruce wants the greebles to POP. Denser grid (more rows/cols),
            // less skipping (keep ~70% vs the old ~50%), bigger blocks (~2×),
            // and higher hull-contrast colors so they read clearly as hull
            // detail at 320×200 / 8 bands.
            let rows = lerp(3.0, 9.0, greeb).round() as usize;
            let cols = lerp(10.0, 38.0, greeb).round() as usize;
            let mut seed = 0u32;
            for r in 0..rows {
                for c in 0..cols {
                    seed += 1;
                    let t = if cols > 1 {
                        c as f32 / (cols - 1) as f32
                    } else {
                        0.0
                    };
                    let sx = lerp(-l * 0.9, l * 0.7, t);
                    let w_at = width_at(plan, sx / (2.0 * l) + 0.5);
                    // Clamp the z scatter to the actual half-width at this x
                    // (0.7× keeps panels on the dorsal deck, off the chine
                    // edges) so none hang past the hull sides where it tapers.
                    let zz = if rows > 1 {
                        (r as f32 / (rows - 1) as f32 - 0.5) * w_at * 0.7
                    } else {
                        0.0
                    };
                    // Keep ~70% of slots (skip only the top ~30%).
                    if hash01(seed) > 0.7 {
                        continue;
                    }
                    // Three-way pick for tonal variety + contrast: very-dark
                    // panel, bright canopy-blue, or warm amber light.
                    let pick = hash01(seed.wrapping_add(7));
                    let col = if pick > 0.66 {
                        GREEB_DARK
                    } else if pick > 0.33 {
                        MAT_CANOPY
                    } else {
                        GREEB_LIGHT
                    };
                    // Seat the block ON the dorsal skin: its center sits at the
                    // skin height plus half the block's own height, so the box
                    // bottom rests on the hull rather than floating at a fixed
                    // height (the prior `h*0.36` floated where the hull is
                    // lower). Block is ~2× the original size for read.
                    let box_h = h * 0.06;
                    m.add_box(
                        [sx, skin_y + box_h * 0.5, zz],
                        l * 0.025,
                        box_h,
                        h * 0.06,
                        col,
                    );
                }
            }
        }
        m
    }
}

// ===========================================================================
// POC renderer
// ===========================================================================

/// Offscreen-resolution ladder (the tool's 120/160/220/320/480 selector, at
/// the POC's 8:5 aspect). `[` / `]` cycle it live so bruce can find the sweet
/// spot by eye: higher = sharper + greebles resolve, but too high loses the
/// pixel-art charm. Default index 3 (320×200) — 160×100 was too low for the
/// greebles to survive the downsample/posterize.
const RES_LADDER: [(u32, u32); 5] = [(120, 75), (160, 100), (220, 138), (320, 200), (480, 300)];
const DEFAULT_RES_IDX: usize = 3;

/// Posterize band ladder (`-` / `=` cycle). Band count interacts with how the
/// greebles read against the hull, so it's a live knob too.
const BANDS_LADDER: [f32; 5] = [2.0, 3.0, 4.0, 5.0, 8.0];
// bruce prefers the smoother, more-shaded HD-2D look over chunky-retro; 8
// bands keep the greebles' tonal separation from the hull.
const DEFAULT_BANDS_IDX: usize = 4; // 8 bands

/// Default look-down pitch (degrees). UP/DOWN arrows scrub it continuously.
const DEFAULT_PITCH_DEG: f32 = 26.0;
/// Auto-orbit yaw speed (degrees/second) when not being dragged. Slow so the
/// continuous read is easy to judge at every angle.
const AUTO_YAW_DEG_PER_SEC: f32 = 36.0;
/// Manual steer rates while an arrow key is held (deg/sec).
const STEER_YAW_DEG_PER_SEC: f32 = 90.0;
const STEER_PITCH_DEG_PER_SEC: f32 = 60.0;
/// Pitch clamp so the camera never crosses the poles (degrees).
const PITCH_MIN_DEG: f32 = 2.0;
const PITCH_MAX_DEG: f32 = 88.0;

/// The four canonical stance yaws (degrees) — right / left / fore / aft. These
/// are *reference snap points only* (press 1–4 to jump to one). The POC's
/// actual motion model is SMOOTH CONTINUOUS yaw, not stepping between these:
/// the whole thesis is that live 3D rotates smoothly at every angle for free,
/// with no sprite interpolation or baked frames.
const STANCE_YAWS_DEG: [f32; 4] = [28.0, 152.0, 118.0, 298.0];
const STANCE_NAMES: [&str; 4] = ["right", "left", "fore", "aft"];

/// Greeble density (tool default 0.6) — drives broadside-battery count and the
/// dorsal panel-block scatter in `parts::build_parts`.
const GREEB: f32 = 0.6;

const LOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    _pad0: f32,
    normal: [f32; 3],
    _pad1: f32,
    color: [f32; 3],
    _pad2: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniform {
    view_proj: [f32; 16],
    model: [f32; 16],
    key_dir: [f32; 4],  // xyz toward key light, w = intensity
    fill_dir: [f32; 4], // xyz toward fill light, w = intensity
    ambient: [f32; 4],  // hull/parts albedo now travels per-vertex
}

/// Posterize band count, live-tunable. Padded to 16 bytes (uniform alignment;
/// three scalar pads, never a vec3 — see the gfx.rs BlendUniform lesson).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PostUniform {
    bands: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

const HULL_SHADER: &str = r#"
struct Scene {
    view_proj: mat4x4<f32>,
    model:     mat4x4<f32>,
    key_dir:   vec4<f32>,
    fill_dir:  vec4<f32>,
    ambient:    vec4<f32>,
};
@group(0) @binding(0) var<uniform> scene: Scene;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_n: vec3<f32>,
    @location(1) color: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>, @location(2) col: vec3<f32>) -> VsOut {
    let world = scene.model * vec4<f32>(pos, 1.0);
    let wn = (scene.model * vec4<f32>(nrm, 0.0)).xyz;
    var o: VsOut;
    o.clip = scene.view_proj * world;
    o.world_n = wn;
    o.color = col;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_n);
    let key = max(dot(n, normalize(scene.key_dir.xyz)), 0.0) * scene.key_dir.w;
    let fill = max(dot(n, normalize(scene.fill_dir.xyz)), 0.0) * scene.fill_dir.w;
    let lit = in.color * (scene.ambient.rgb + vec3<f32>(key) + vec3<f32>(0.53, 0.67, 1.0) * fill);
    return vec4<f32>(lit, 1.0);
}
"#;

const POST_SHADER: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
// Scalar pads, NOT vec3<f32>: a WGSL vec3 forces 16-byte alignment and would
// make this struct 32 bytes vs the Rust PostUniform's 16 — the late-min-
// binding-size trap (same class of bug fixed in gfx.rs BlendUniform).
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
    let q = floor(col * post.bands + 0.5) / post.bands;
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

    // ---- live offscreen-resolution + posterize knobs ----
    /// Kept so the offscreen targets + post bind group can be rebuilt when the
    /// resolution changes ([ / ]).
    post_bgl: wgpu::BindGroupLayout,
    post_sampler: wgpu::Sampler,
    bands_ubo: wgpu::Buffer,
    /// Index into RES_LADDER (current offscreen size) and BANDS_LADDER.
    res_idx: usize,
    bands_idx: usize,

    // ---- continuous-motion camera state ----
    /// Current yaw / pitch in degrees, advanced every frame (smooth, live —
    /// NOT stepped between discrete stances). Yaw auto-orbits unless paused or
    /// being steered; pitch scrubs with UP/DOWN.
    yaw_deg: f32,
    pitch_deg: f32,
    /// `false` = continuous auto-orbit; `true` = paused (steer-only).
    paused: bool,
    /// Held-key steer state (set on press, cleared on release) for smooth
    /// continuous nudging while a key is down.
    steer_left: bool,
    steer_right: bool,
    steer_up: bool,
    steer_down: bool,
    /// Wall-clock of the previous frame, for frame-rate-independent motion.
    last_frame: Instant,
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

        // Geometry: lofted hull (tinted with the tool's hull albedo) folded
        // together with the attached parts into one per-vertex-colored soup.
        let params = loft::LoftParams::default();
        let hull = loft::build_hull(loft::DAGGER_PLAN, loft::DAGGER_SECTION, params);
        // Hull world half-extents (match buildHull's `L = 6*stretch/2`, `H`).
        let hull_l = 6.0 * params.stretch / 2.0;
        let hull_h = params.hscale;
        const HULL_ALBEDO: [f32; 3] = [180.0 / 255.0, 198.0 / 255.0, 224.0 / 255.0]; // 0xb4c6e0

        let mut mesh = parts::ColoredMesh::default();
        for (p, n) in hull.positions.iter().zip(hull.normals.iter()) {
            mesh.positions.push(*p);
            mesh.normals.push(*n);
            mesh.colors.push(HULL_ALBEDO);
        }
        let parts_mesh = parts::build_parts(
            hull_l,
            hull_h,
            loft::DAGGER_PLAN,
            loft::DAGGER_SECTION,
            GREEB,
        );
        mesh.append(&parts_mesh);

        let verts: Vec<Vertex> = (0..mesh.positions.len())
            .map(|i| Vertex {
                pos: mesh.positions[i],
                _pad0: 0.0,
                normal: mesh.normals[i],
                _pad1: 0.0,
                color: mesh.colors[i],
                _pad2: 0.0,
            })
            .collect();
        let vcount = verts.len() as u32;
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ship vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let res_idx = DEFAULT_RES_IDX;
        let bands_idx = DEFAULT_BANDS_IDX;
        let (low_w, low_h) = RES_LADDER[res_idx];

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
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            offset: 32,
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
            label: Some("bands ubo"),
            size: std::mem::size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &bands_ubo,
            0,
            bytemuck::bytes_of(&PostUniform {
                bands: BANDS_LADDER[bands_idx],
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );
        // Offscreen color+depth at the current resolution, plus the post bind
        // group wired to them. Rebuilt by `set_resolution` when [ / ] cycle.
        let (low_view, depth_view, post_bg) =
            Self::offscreen_targets(&device, low_w, low_h, &post_bgl, &post_sampler, &bands_ubo);
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
            post_bgl,
            post_sampler,
            bands_ubo,
            res_idx,
            bands_idx,
            yaw_deg: STANCE_YAWS_DEG[0],
            pitch_deg: DEFAULT_PITCH_DEG,
            paused: false,
            steer_left: false,
            steer_right: false,
            steer_up: false,
            steer_down: false,
            last_frame: Instant::now(),
        }
    }

    /// (Re)build the low-res offscreen color + depth textures at `(w, h)` and
    /// the post bind group wired to them (color view + sampler + bands ubo).
    /// Called once in `new` and again whenever the resolution knob changes.
    fn offscreen_targets(
        device: &wgpu::Device,
        w: u32,
        h: u32,
        post_bgl: &wgpu::BindGroupLayout,
        post_sampler: &wgpu::Sampler,
        bands_ubo: &wgpu::Buffer,
    ) -> (wgpu::TextureView, wgpu::TextureView, wgpu::BindGroup) {
        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let low = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("low-res target"),
            size,
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
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let post_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post bg"),
            layout: post_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&low_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(post_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: bands_ubo.as_entire_binding(),
                },
            ],
        });
        (low_view, depth_view, post_bg)
    }

    /// Step the offscreen-resolution ladder by `delta` (clamped) and rebuild
    /// the offscreen targets + post bind group at the new size.
    fn cycle_resolution(&mut self, delta: isize) {
        let n = RES_LADDER.len() as isize;
        let next = (self.res_idx as isize + delta).clamp(0, n - 1) as usize;
        if next == self.res_idx {
            return;
        }
        self.res_idx = next;
        let (w, h) = RES_LADDER[self.res_idx];
        let (low_view, depth_view, post_bg) = Self::offscreen_targets(
            &self.device,
            w,
            h,
            &self.post_bgl,
            &self.post_sampler,
            &self.bands_ubo,
        );
        self.low_view = low_view;
        self.depth_view = depth_view;
        self.post_bg = post_bg;
        eprintln!("[loft_poc] offscreen resolution: {w}x{h}");
    }

    /// Step the posterize band ladder by `delta` (clamped) and reupload.
    fn cycle_bands(&mut self, delta: isize) {
        let n = BANDS_LADDER.len() as isize;
        let next = (self.bands_idx as isize + delta).clamp(0, n - 1) as usize;
        if next == self.bands_idx {
            return;
        }
        self.bands_idx = next;
        let bands = BANDS_LADDER[self.bands_idx];
        self.queue.write_buffer(
            &self.bands_ubo,
            0,
            bytemuck::bytes_of(&PostUniform {
                bands,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );
        eprintln!("[loft_poc] posterize bands: {bands}");
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Advance the live camera angles by the wall-clock delta since the last
    /// frame — this is the continuous-motion model: smooth yaw/pitch every
    /// frame, no discrete stance stepping. Manual steer (held arrows) adds to
    /// the auto-orbit; auto-orbit pauses while steering yaw or when paused.
    fn advance(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1); // clamp hitches
        self.last_frame = now;

        let mut yaw_rate = 0.0;
        if self.steer_left {
            yaw_rate -= STEER_YAW_DEG_PER_SEC;
        }
        if self.steer_right {
            yaw_rate += STEER_YAW_DEG_PER_SEC;
        }
        // Auto-orbit only when not paused and not actively steering yaw.
        if !self.paused && yaw_rate == 0.0 {
            yaw_rate = AUTO_YAW_DEG_PER_SEC;
        }
        self.yaw_deg = (self.yaw_deg + yaw_rate * dt).rem_euclid(360.0);

        let mut pitch_rate = 0.0;
        if self.steer_up {
            pitch_rate += STEER_PITCH_DEG_PER_SEC;
        }
        if self.steer_down {
            pitch_rate -= STEER_PITCH_DEG_PER_SEC;
        }
        self.pitch_deg = (self.pitch_deg + pitch_rate * dt).clamp(PITCH_MIN_DEG, PITCH_MAX_DEG);
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        self.advance();

        let yaw = self.yaw_deg.to_radians();
        let pitch = self.pitch_deg.to_radians();
        let (low_w, low_h) = RES_LADDER[self.res_idx];
        let aspect = low_w as f32 / low_h as f32;
        let view_proj = math3::camera_view_proj(yaw, pitch, aspect, 1.0);
        let model = math3::rotate_y(0.0);

        // Lights ported from the tool's setLight (laz -50, lel 60) / fixed fill
        // (4,2,-3) / ambient 0x3a4560×0.9. Hull/parts albedo now travels
        // per-vertex. Three.js DirectionalLight shines position→origin, so
        // dir-toward-light = +pos.
        let laz = (-50.0f32).to_radians();
        let lel = (60.0f32).to_radians();
        let key_dir = math3::normalize([lel.cos() * laz.sin(), lel.sin(), lel.cos() * laz.cos()]);
        let fill_dir = math3::normalize([4.0, 2.0, -3.0]);
        let amb = [58.0 / 255.0 * 0.9, 69.0 / 255.0 * 0.9, 96.0 / 255.0 * 0.9];

        let scene = SceneUniform {
            view_proj,
            model,
            key_dir: [key_dir[0], key_dir[1], key_dir[2], 1.6],
            fill_dir: [fill_dir[0], fill_dir[1], fill_dir[2], 0.45],
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
            .with_title(
                "Loft POC — ←→ yaw · ↑↓ pitch · Space pause · 1-4 snap · [ ] res · - = bands",
            )
            .with_inner_size(winit::dpi::LogicalSize::new(
                (RES_LADDER[DEFAULT_RES_IDX].0 * 3) as f64,
                (RES_LADDER[DEFAULT_RES_IDX].1 * 3) as f64,
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
                        state,
                        ..
                    },
                ..
            } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    let pressed = state == ElementState::Pressed;
                    match code {
                        // Held arrows = continuous steer (set the flag; the
                        // per-frame `advance` integrates it). Steering yaw
                        // suspends the auto-orbit only while the key is down.
                        KeyCode::ArrowLeft => gpu.steer_left = pressed,
                        KeyCode::ArrowRight => gpu.steer_right = pressed,
                        KeyCode::ArrowUp => gpu.steer_up = pressed,
                        KeyCode::ArrowDown => gpu.steer_down = pressed,
                        // Space toggles the auto-orbit on/off (on press only).
                        KeyCode::Space if pressed => gpu.paused = !gpu.paused,
                        // [ / ] cycle the offscreen resolution ladder.
                        KeyCode::BracketLeft if pressed => gpu.cycle_resolution(-1),
                        KeyCode::BracketRight if pressed => gpu.cycle_resolution(1),
                        // - / = cycle the posterize band count.
                        KeyCode::Minus if pressed => gpu.cycle_bands(-1),
                        KeyCode::Equal if pressed => gpu.cycle_bands(1),
                        // 1-4 snap yaw to a canonical stance (reference points,
                        // not the motion model) and pause so it can be studied.
                        KeyCode::Digit1 if pressed => {
                            gpu.yaw_deg = STANCE_YAWS_DEG[0];
                            gpu.paused = true;
                            eprintln!("[loft_poc] snap: {} (28\u{b0})", STANCE_NAMES[0]);
                        }
                        KeyCode::Digit2 if pressed => {
                            gpu.yaw_deg = STANCE_YAWS_DEG[1];
                            gpu.paused = true;
                            eprintln!("[loft_poc] snap: {} (152\u{b0})", STANCE_NAMES[1]);
                        }
                        KeyCode::Digit3 if pressed => {
                            gpu.yaw_deg = STANCE_YAWS_DEG[2];
                            gpu.paused = true;
                            eprintln!("[loft_poc] snap: {} (118\u{b0})", STANCE_NAMES[2]);
                        }
                        KeyCode::Digit4 if pressed => {
                            gpu.yaw_deg = STANCE_YAWS_DEG[3];
                            gpu.paused = true;
                            eprintln!("[loft_poc] snap: {} (298\u{b0})", STANCE_NAMES[3]);
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
