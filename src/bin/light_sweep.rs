//! DYNAMIC-LIGHTING TEST (headless) — the realtime-3D PROOF: a ship-like hull
//! rendered across a 180° KEY-LIGHT sweep, one numbered PNG per step, so Bruce
//! can flip through the frames and SEE the shadows + hues shift on the surface
//! as the light moves. THE POINT IS THE MOVING LIGHT, NOT THE SHIP SHAPE.
//!
//! PLACEHOLDER HULL — NOT the faithful Aegis. The subject here is a hand-built
//! volumetric WEDGE primitive (a chunky stylized hull with a tapered bow, real
//! height, and an emissive engine block at the stern) chosen ONLY because it
//! has relief in all three axes so the moving light casts clear, readable
//! shading — the engine's own flat loft hulls are near-flat ribbons that read
//! as a sliver from any angle and make a poor lighting demo. The FAITHFUL Aegis
//! comes via the GLB pipeline (`mesh_import`): Bruce's tool exports the fully-
//! built mesh (hull + engines, correct proportions / normals / materials) as a
//! `.glb`, the engine imports it, and this SAME light-sweep path lights it
//! live — no Rust loft rebuild. Read the LIGHTING here, NOT the shape.
//!
//! Run: `cargo run --bin light_sweep --features render,runtime`
//! Output: `<crate>/../bugs/light_sweep/frame_NN.png` (NN = 00..).
//!
//! Drawn through the SAME `loft_gpu` pipeline the game uses (depth + Lambert +
//! emissive + posterize). Only the KEY light's azimuth sweeps; the fill +
//! ambient + the emissive engine block stay fixed, so the moving shadow reads
//! as one light orbiting the hull. Touches NONE of the 2D sprite gameplay path.

use broadside_engine::gfx::Gfx;
use broadside_engine::loft::HullMesh;
use broadside_engine::mesh_import::{GroupRange, ImportLight, ImportedShip, MeshMaterial};
use std::path::PathBuf;

/// How many frames across the sweep + the arc covered.
const FRAMES: usize = 14;
const AZ_START_DEG: f32 = -90.0;
const AZ_END_DEG: f32 = 90.0;
/// Key light elevation (degrees) + intensity, held constant through the sweep.
const KEY_EL_DEG: f32 = 35.0;
const KEY_INTENSITY: f32 = 1.8;
/// DEMO camera (not the gameplay values): a ¾ pitch + zoom framing the wedge so
/// its lit faces fill the frame and the moving shadow reads clearly.
const DEMO_PITCH_DEG: f32 = 32.0;
const DEMO_HALF_EXTENT: f32 = 4.0;
/// Ship yaw (camera orbit, degrees) — a ¾ so both a side and the top read.
const SHIP_YAW_DEG: f32 = 35.0;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let ship = demo_wedge();
    log::info!(
        "light_sweep: PLACEHOLDER wedge ({} tris) — watch the LIGHTING, not the shape; faithful Aegis = GLB import",
        ship.mesh.positions.len() / 3
    );

    let out_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bugs/light_sweep");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        log::error!("light_sweep: cannot create {}: {e}", out_dir.display());
        std::process::exit(1);
    }

    let gfx = pollster::block_on(Gfx::new_headless());

    for i in 0..FRAMES {
        let t = if FRAMES <= 1 {
            0.0
        } else {
            i as f32 / (FRAMES - 1) as f32
        };
        let az = AZ_START_DEG + (AZ_END_DEG - AZ_START_DEG) * t;
        let path = out_dir.join(format!("frame_{i:02}.png"));
        match gfx.render_loft_to_png(
            &ship,
            SHIP_YAW_DEG,
            az,
            KEY_EL_DEG,
            KEY_INTENSITY,
            DEMO_PITCH_DEG,
            DEMO_HALF_EXTENT,
            &path,
        ) {
            Ok(()) => log::info!(
                "light_sweep: frame {i:02} key-az {az:+.0}° → {}",
                path.display()
            ),
            Err(e) => {
                log::error!("light_sweep: frame {i:02} failed: {e}");
                std::process::exit(1);
            }
        }
    }
    log::info!(
        "light_sweep: wrote {FRAMES} frames (key-az {AZ_START_DEG:+.0}°..{AZ_END_DEG:+.0}°) to {}",
        out_dir.display()
    );
}

/// A chunky volumetric WEDGE hull (placeholder, NOT the Aegis): a 6-unit-long
/// body that tapers to a bow point at +X, with a full-height boxy stern, plus a
/// small emissive engine block hanging off the stern face. Built as flat-shaded
/// tri-soup with two material groups (lit hull + unlit cyan engine) so it
/// exercises the exact same pipeline a GLB import would. Volume in all three
/// axes is the point — the moving light casts obvious shading across the angled
/// faces, which a near-flat loft ribbon cannot show.
fn demo_wedge() -> ImportedShip {
    // 8 hull corners: stern box (x=-3) full W×H, tapering to a bow point (x=+3).
    let (hw, hh) = (1.4f32, 1.0f32); // half-width, half-height at the stern
    let stern = -3.0f32;
    let bow = 3.0f32;
    // Stern face corners (a box).
    let s_bl = [stern, -hh, -hw];
    let s_br = [stern, -hh, hw];
    let s_tr = [stern, hh, hw];
    let s_tl = [stern, hh, -hw];
    // Bow: a short vertical edge (a chisel prow, not a needle) so the bow has a
    // small face to catch light rather than collapsing to a line.
    let bw = hw * 0.12;
    let bh = hh * 0.45;
    let b_bl = [bow, -bh, -bw];
    let b_br = [bow, -bh, bw];
    let b_tr = [bow, bh, bw];
    let b_tl = [bow, bh, -bw];

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut quad = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]| {
        for tri in [[a, b, c], [a, c, d]] {
            let n = face_normal(tri[0], tri[1], tri[2]);
            positions.extend_from_slice(&tri);
            normals.extend_from_slice(&[n, n, n]);
        }
    };
    // Hull faces (outward winding).
    quad(s_tl, s_tr, b_tr, b_tl); // top deck
    quad(s_br, s_bl, b_bl, b_br); // belly
    quad(s_bl, s_tl, b_tl, b_bl); // port side
    quad(s_tr, s_br, b_br, b_tr); // starboard side
    quad(s_bl, s_br, s_tr, s_tl); // stern cap
    quad(b_br, b_bl, b_tl, b_tr); // bow cap
    let hull_len = positions.len();

    // Emissive engine block: a small box just aft of the stern face.
    let er = 0.55f32;
    let ex0 = stern;
    let ex1 = stern - 0.9;
    let mut eng = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]| {
        for tri in [[a, b, c], [a, c, d]] {
            let n = face_normal(tri[0], tri[1], tri[2]);
            positions.extend_from_slice(&tri);
            normals.extend_from_slice(&[n, n, n]);
        }
    };
    let p = |x: f32, sy: f32, sz: f32| [x, sy * er, sz * er];
    let (f_bl, f_br, f_tr, f_tl) = (
        p(ex0, -1.0, -1.0),
        p(ex0, -1.0, 1.0),
        p(ex0, 1.0, 1.0),
        p(ex0, 1.0, -1.0),
    );
    let (k_bl, k_br, k_tr, k_tl) = (
        p(ex1, -1.0, -1.0),
        p(ex1, -1.0, 1.0),
        p(ex1, 1.0, 1.0),
        p(ex1, 1.0, -1.0),
    );
    eng(k_br, k_bl, k_tl, k_tr); // exhaust face (the glow disc, faces -X)
    eng(f_bl, f_br, f_tr, f_tl); // front
    eng(f_bl, k_bl, k_br, f_br); // belly
    eng(f_tr, k_tr, k_tl, f_tl); // top
    eng(f_br, k_br, k_tr, f_tr); // starboard
    eng(f_tl, k_tl, k_bl, f_bl); // port
    let eng_len = positions.len() - hull_len;

    let group_ranges = vec![
        GroupRange {
            start: 0,
            len: hull_len,
            material: 0,
        },
        GroupRange {
            start: hull_len,
            len: eng_len,
            material: 1,
        },
    ];
    let materials = vec![
        // Lit hull grey.
        MeshMaterial {
            color: [0.706, 0.776, 0.878, 1.0],
            emissive: [0.0, 0.0, 0.0, 1.0],
            unlit: false,
        },
        // Unlit cyan engine glow (a light source the dynamic hull lighting plays
        // against — stays constant through the sweep).
        MeshMaterial {
            color: [0.45, 0.95, 1.0, 1.0],
            emissive: [0.45, 0.95, 1.0, 1.0],
            unlit: true,
        },
    ];

    ImportedShip {
        mesh: HullMesh { positions, normals },
        materials,
        group_ranges,
        light: ImportLight::default(),
    }
}

/// Normalized face normal of `(a,b,c)` via `(b−a)×(c−a)`; `+y` fallback for a
/// degenerate triangle.
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
