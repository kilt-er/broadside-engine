//! THE FAITHFUL AEGIS (headless) — imports the REAL `assets/ships/Aegis.glb`
//! (Bruce's tool export, per `docs/BROADSIDE_RENDER_CONTRACT.md` v5 = our
//! BROADSIDE_REALTIME_RENDER_SPEC.md), VERIFIES the imported mesh matches the
//! design, then renders it: an at-rest lit shot + a 180° key-light sweep,
//! through the SAME `loft_gpu` pipeline the game uses. No Rust loft rebuild —
//! the geometry comes entirely from the GLB.
//!
//! Run: `cargo run --bin render_aegis --features render,runtime`
//! Output: `<crate>/../bugs/aegis/rest.png` + `frame_NN.png` (sweep) + a VERIFY
//! line. VERIFY-BEFORE-DECLARING (the wrong-ship lesson): asserts X-extent ≈ 12,
//! the engine nacelles are present at the stern, wide-low proportions, and
//! distinct material groups — cross-checked against the v2-json design. If a
//! check fails it logs a clear FAIL and exits non-zero rather than emit a
//! mislabelled ship.
//!
//! Per v5 §5/§6 (already enforced inside `mesh_import::load_glb`): positions
//! read verbatim (raw axes X=len/Y=up/Z=beam), the exported NORMAL is IGNORED
//! and flat per-face normals recomputed, one draw per material primitive
//! (albedo + emissive, unlit glow parts self-lit), light from scene.extras
//! laz/lel. Toon / outline / grade / full-3-light are Tier-1.5, NOT here.
//!
//! Gameplay 2D sprite path is untouched.

use broadside_engine::gfx::Gfx;
use broadside_engine::mesh_import::{load_glb, ImportedShip};
use std::path::PathBuf;

const AEGIS_GLB: &[u8] = include_bytes!("../../assets/ships/Aegis.glb");

/// Sweep config (same as the light_sweep proof).
const FRAMES: usize = 14;
const AZ_START_DEG: f32 = -90.0;
const AZ_END_DEG: f32 = 90.0;
const KEY_EL_DEG: f32 = 40.0;
const KEY_INTENSITY: f32 = 1.8;
/// Camera: a ¾ that shows the deck + a flank so the hull's mass + the stern
/// nacelles read. The Aegis is X-length 12 → the gameplay HALF_EXTENT (7 → 14u
/// box) already frames it; nudge the zoom in a touch for the hero shot.
const SHIP_YAW_DEG: f32 = 35.0;
const PITCH_DEG: f32 = 28.0;
const HALF_EXTENT: f32 = 7.5;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // ---- import the real GLB ----
    let ship = match load_glb(AEGIS_GLB) {
        Ok(s) => s,
        Err(e) => {
            log::error!("render_aegis: load_glb failed: {e}");
            std::process::exit(1);
        }
    };

    // ---- VERIFY BEFORE DECLARING (wrong-ship lesson) ----
    match verify(&ship) {
        Ok(report) => log::info!("render_aegis: VERIFY OK — {report}"),
        Err(fail) => {
            log::error!("render_aegis: VERIFY FAILED — {fail}");
            log::error!("render_aegis: NOT declaring this the Aegis. Flagging instead of emitting a mislabelled ship.");
            std::process::exit(2);
        }
    }

    let out_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bugs/aegis");
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        log::error!("render_aegis: cannot create {}: {e}", out_dir.display());
        std::process::exit(1);
    }

    let gfx = pollster::block_on(Gfx::new_headless());

    // ---- at-rest lit shot (the house key light: laz -50 / lel 60) ----
    let rest = out_dir.join("rest.png");
    if let Err(e) = gfx.render_loft_to_png(
        &ship,
        SHIP_YAW_DEG,
        -50.0,
        60.0,
        1.6,
        PITCH_DEG,
        HALF_EXTENT,
        &rest,
    ) {
        log::error!("render_aegis: rest shot failed: {e}");
        std::process::exit(1);
    }
    log::info!("render_aegis: at-rest shot → {}", rest.display());

    // ---- 180° key-light sweep ----
    for i in 0..FRAMES {
        let t = if FRAMES <= 1 {
            0.0
        } else {
            i as f32 / (FRAMES - 1) as f32
        };
        let az = AZ_START_DEG + (AZ_END_DEG - AZ_START_DEG) * t;
        let path = out_dir.join(format!("frame_{i:02}.png"));
        if let Err(e) = gfx.render_loft_to_png(
            &ship,
            SHIP_YAW_DEG,
            az,
            KEY_EL_DEG,
            KEY_INTENSITY,
            PITCH_DEG,
            HALF_EXTENT,
            &path,
        ) {
            log::error!("render_aegis: frame {i:02} failed: {e}");
            std::process::exit(1);
        }
        log::info!(
            "render_aegis: frame {i:02} key-az {az:+.0}° → {}",
            path.display()
        );
    }
    log::info!(
        "render_aegis: DONE — at-rest + {FRAMES}-frame sweep in {}",
        out_dir.display()
    );
}

/// Numeric verification against the Aegis design (the wrong-ship guard). Returns
/// Ok(report) when the imported mesh matches the v2-json's shape, Err(reason)
/// otherwise. Checks: scaled length ≈ 12 (the build script's TARGET_LEN), a
/// wide-low beam:length (wscale 1.92 → the hull is broad, not a needle), the
/// stern nacelle geometry is present (the 6 engines → multiple material groups
/// incl. an unlit glow), and there ARE distinct material groups.
fn verify(ship: &ImportedShip) -> Result<String, String> {
    let ps = &ship.mesh.positions;
    if ps.is_empty() {
        return Err("imported mesh is empty".into());
    }
    let ext = |axis: usize| {
        let lo = ps.iter().map(|p| p[axis]).fold(f32::INFINITY, f32::min);
        let hi = ps.iter().map(|p| p[axis]).fold(f32::NEG_INFINITY, f32::max);
        hi - lo
    };
    let (lx, ly, lz) = (ext(0), ext(1), ext(2));
    let tris = ps.len() / 3;

    // 1) Length ≈ 12 (the GLB exporter scales X-extent to TARGET_LEN = 12).
    if !(11.0..=13.0).contains(&lx) {
        return Err(format!(
            "X-length {lx:.2} not ≈12 (expected the exporter's TARGET_LEN scale)"
        ));
    }
    // 2) Wide-low: beam should be a healthy fraction of length (wscale 1.92),
    //    and the hull clearly wider than tall (low profile).
    let beam_len = lz / lx;
    if beam_len < 0.12 {
        return Err(format!(
            "beam:length {beam_len:.3} too narrow for wscale 1.92"
        ));
    }
    if lz <= ly {
        return Err(format!(
            "not wide-low: beam {lz:.2} should exceed height {ly:.2}"
        ));
    }
    // 3) Material groups: the hull + engine bells + the glow disc → ≥2 groups,
    //    with at least one UNLIT emissive (the engine exhaust = a light source).
    let groups = ship.group_ranges.len();
    if groups < 2 {
        return Err(format!(
            "only {groups} material group(s); expected hull + engine parts"
        ));
    }
    let has_unlit_glow = ship
        .materials
        .iter()
        .any(|m| m.unlit && (m.emissive[0] + m.emissive[1] + m.emissive[2]) > 0.3);
    if !has_unlit_glow {
        return Err("no unlit emissive material (the engine exhaust glow) found".into());
    }
    // 4) Engines at the stern: the unlit-glow group's geometry should sit aft of
    //    amidships (stern = min-x; the exhaust discs hang off the stern).
    let glow_mat: Vec<usize> = ship
        .materials
        .iter()
        .enumerate()
        .filter(|(_, m)| m.unlit)
        .map(|(i, _)| i)
        .collect();
    let min_x = ps.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    let max_x = ps.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
    let mid_x = (min_x + max_x) * 0.5;
    let mut glow_max_x = f32::NEG_INFINITY;
    for g in &ship.group_ranges {
        if glow_mat.contains(&g.material) {
            for p in &ps[g.start..g.start + g.len] {
                glow_max_x = glow_max_x.max(p[0]);
            }
        }
    }
    if glow_max_x.is_finite() && glow_max_x > mid_x {
        return Err(format!(
            "engine glow at x≤{glow_max_x:.2} extends past amidships {mid_x:.2} — not stern-mounted"
        ));
    }

    Ok(format!(
        "L×H×W {lx:.2}×{ly:.2}×{lz:.2} (beam:len {beam_len:.2}, wide-low ✓), {tris} tris, {groups} material groups, unlit-glow ✓ at stern (x≤{glow_max_x:.2} ≤ mid {mid_x:.2})"
    ))
}
