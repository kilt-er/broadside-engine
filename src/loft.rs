//! Pure-math hull **lofting**: turn a [`ShipDesign`]'s 2D profiles into a 3D
//! triangle-soup mesh ready for upload as flat-shaded geometry.
//!
//! This is stage 1 of the ship render pipeline (see `docs/RENDER_PIPELINE.md`):
//! a 2D cross-section is swept along the ship's length, scaled at each station
//! by the top-down plan outline, with an optional per-station height
//! multiplier. The result is a low-poly hull — the Star Destroyer "dagger"
//! the loft editor produces by default.
//!
//! ## No GPU here, on purpose
//!
//! This module is **pure arithmetic** — no `wgpu`, no `feature = "render"`
//! gate. It runs (and is unit-tested) headless on CI. The GPU side — uploading
//! [`HullMesh`] to vertex buffers, the depth-tested 3D pass, the posterize
//! pass — lives in the renderer's `loft_gpu` module and consumes the
//! [`HullMesh`] this produces. Keeping the math standalone means the loft
//! arithmetic is testable, deterministic, and direction-independent (it is the
//! same hull whatever the camera does), so it is safe to productionize ahead
//! of the visual POC verdict.
//!
//! ## Reference implementation
//!
//! Ported faithfully from the standalone POC's `loft` mod
//! (`src/bin/loft_poc.rs`), which is itself a line-for-line port of the loft
//! editor's `buildHull()` / `sampleSection()` (`docs/broadside-loft-editor.html`).
//! The one substantive change: the POC hardcodes the dagger profiles as
//! `[f32; 2]` consts and a `LoftParams { stretch, hscale }` default; this
//! module drives all of that from a loaded [`ShipDesign`] instead, reusing
//! [`ship_design::Point2`] for the profile points so the `.json` the editor
//! saves flows straight through.
//!
//! ## Coordinate convention (matches the three.js source)
//!
//! - **x** = length, prow toward `+x`.
//! - **y** = height, dorsal toward `+y`.
//! - **z** = half-width (port/starboard).
//!
//! Plan points are `[x (0..1 stern→prow), halfWidth (0..1)]`; section points
//! are `[z (0..1 half-width), y (-1..1 height)]` ordered top-dorsal → chine →
//! belly; the optional height profile is `[x (0..1), heightMult (~0..1.5)]`.

use crate::ship_design::{Point2, ShipDesign};

/// Section ring resolution — the number of samples taken across the section
/// profile for each station ring (the loft editor's `SECN`). The POC uses
/// `10`; a design may override it via [`LoftParams::sec_n`]. Each ring ends up
/// with `2·sec_n − 2` vertices (right side top→belly, then the mirrored left
/// side belly→top, skipping the shared top and belly endpoints).
pub const DEFAULT_SEC_N: usize = 10;

/// Vertical-mass multiplier applied to the PLAYER hull's `hscale` when lofting
/// it for the in-game top-down ¾ camera (#54). The Aegis design is authored at
/// `hscale = 0.7` (0.7u tall) for the CAD editor's near-side view, but from the
/// game's steep [`crate::loft_gpu::CAMERA_PITCH_DEG`] pitch a 0.7u hull reads as
/// a flat plank rather than a ship. Boosting the height gives the hull visible
/// mass without touching the design file (purely a render-readability tweak —
/// the source design is unchanged). The playable bin and the headless capture
/// tool BOTH apply this so the capture stays a faithful image of the game.
///
/// Tuned on the capture loop (#54): the loft hull seats correctly on its cell
/// (the camera now frames the hull's bbox centre) but the Aegis is flat-wide
/// (hscale 0.7, wscale ~1.9), so even seated it reads thin from the steep
/// top-down ¾. 3.0× gives the player hull clear vertical mass — the most
/// substantial ship on screen — without a grotesque stretched-tower look. This
/// is a display choice (the source design is unchanged); Bruce can pick the
/// final faithful-flat vs boosted-for-mass look.
pub const PLAYER_LOFT_HSCALE_BOOST: f32 = 3.0;

/// Parameters that scale the loft. Pulled from [`ShipDesign`]'s `settings`
/// ([`crate::ship_design::Settings::stretch`] / `hscale`) plus the section
/// ring resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoftParams {
    /// Length stretch (the editor's `ST.stretch`; `2.0` = the dagger default).
    pub stretch: f32,
    /// Height scale applied to the section profile (the editor's `ST.hscale`;
    /// `0.7` default).
    pub hscale: f32,
    /// Width scale applied to the section's lateral (z/beam) extent — the
    /// editor's `ST.wscale` (`buildHull`: `z = zf * st.w * ww`). `1.0` = neutral
    /// (the dagger default). The Aegis authors this at `1.92`; omitting it (the
    /// old behaviour) rendered the hull ~half its real beam. See
    /// `ShipEditor/BROADSIDE_RENDER_CONTRACT_v2.md` + the tool's `buildHull`.
    pub wscale: f32,
    /// Nose taper, `0..1` — the editor's `ST.noseTaper`. Scales section HEIGHT
    /// toward the prow so the hull comes to a point instead of a full-height
    /// axe-head (`buildHull`'s `noseHeightScale`). `0` = no taper (full height
    /// to the tip), `1` = collapse to a point at the prow. The Aegis authors
    /// `0.73`; omitting it (the old behaviour) left a blunt full-height prow.
    pub nose_taper: f32,
    /// Section ring resolution. See [`DEFAULT_SEC_N`].
    pub sec_n: usize,
}

impl Default for LoftParams {
    fn default() -> Self {
        // The loft editor's default state: ST.stretch = 2.0, ST.hscale = 0.7,
        // wscale = 1.0 (neutral), noseTaper = 0 (no taper).
        Self {
            stretch: 2.0,
            hscale: 0.7,
            wscale: 1.0,
            nose_taper: 0.0,
            sec_n: DEFAULT_SEC_N,
        }
    }
}

/// A lofted hull as a flat-shaded **triangle soup**: `positions` and `normals`
/// run in lockstep, three vertices per triangle, and all three vertices of a
/// triangle share that face's normal (so the faceted low-poly look survives
/// upload without an index buffer or vertex-normal averaging).
///
/// `positions.len() == normals.len()` and both are a multiple of 3. Upload as
/// a non-indexed vertex buffer; draw `positions.len()` vertices.
#[derive(Clone, Debug, PartialEq)]
pub struct HullMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

impl HullMesh {
    /// Triangle count (`positions.len() / 3`).
    pub fn tri_count(&self) -> usize {
        self.positions.len() / 3
    }
}

/// Loft a hull straight from a loaded [`ShipDesign`]. Pulls the plan / section
/// / optional height profile and the stretch / hscale settings out of the
/// design; the section ring resolution uses [`DEFAULT_SEC_N`] (the design
/// format does not currently carry one).
pub fn loft_hull(design: &ShipDesign) -> HullMesh {
    // The v1 [`ShipDesign::settings`] schema carries no `wscale`/`noseTaper`
    // (those are v2-design fields), so they default to neutral here (`1.0` /
    // `0.0`). A faithful v2 hull is imported via `mesh_import` (the GLB the tool
    // bakes), not re-lofted in Rust — these knobs exist for the loft primitive
    // but the production ship path is GLB.
    let params = LoftParams {
        stretch: design.settings.stretch as f32,
        hscale: design.settings.hscale as f32,
        wscale: 1.0,
        nose_taper: 0.0,
        sec_n: DEFAULT_SEC_N,
    };
    loft_from_profiles(
        &design.plan,
        &design.section,
        design.height_profile.as_deref(),
        params,
    )
}

/// Loft a hull from raw profile points + [`LoftParams`]. The lower-level entry
/// point — [`loft_hull`] is the thin wrapper that unpacks a [`ShipDesign`].
///
/// `plan` and `section` must each have at least two points (a single station
/// or a degenerate section can't be swept into a surface). `height` is
/// `None` for a flat `1.0` multiplier everywhere (the editor's no-traced-image
/// default).
pub fn loft_from_profiles(
    plan: &[Point2],
    section: &[Point2],
    height: Option<&[Point2]>,
    params: LoftParams,
) -> HullMesh {
    // Mirrors the POC `build_hull`. `l` is half the stretched length; a plan
    // x of 0.5 maps to world x = 0 (amidships), 0 → stern (−l), 1 → prow (+l).
    let l = 6.0 * params.stretch / 2.0;
    let h = params.hscale;
    let ww = if params.wscale.is_finite() {
        params.wscale
    } else {
        1.0
    };
    let nose_taper = params.nose_taper;
    let sec_n = params.sec_n.max(3);

    // Each plan point becomes a station: world x, half-width, the height-profile
    // multiplier sampled at that x, and the plan-x `px` (0=stern .. 1=prow) the
    // nose taper keys off.
    struct Station {
        x: f32,
        w: f32,
        hm: f32,
        px: f32,
    }
    let stations: Vec<Station> = plan
        .iter()
        .map(|p| Station {
            x: (p.x() as f32 - 0.5) * 2.0 * l,
            w: p.y() as f32,
            hm: sample_height_prof(height, p.x() as f32),
            px: p.x() as f32,
        })
        .collect();

    // Build one ring of vertices for a station: sample the section at `sec_n`
    // steps for the right (+z) side top→belly, then mirror the interior points
    // back for the left (−z) side, skipping the shared top and belly endpoints.
    // Beam is scaled by `ww` (wscale); section HEIGHT is scaled by the nose
    // taper toward the prow (`nose_height_scale`) — both faithful to the tool's
    // `buildHull` (`z = zf*st.w*ww`, `hh = H*hm*noseHeightScale(px)`).
    let ring_pts = |st: &Station| -> Vec<[f32; 3]> {
        let mut pts = Vec::with_capacity(2 * sec_n - 2);
        let hh = h * st.hm * nose_height_scale(nose_taper, st.px);
        for s in 0..sec_n {
            let (zf, y) = sample_section(section, s as f32 / (sec_n - 1) as f32);
            pts.push([st.x, y * hh, zf * st.w * ww]);
        }
        for s in (1..=(sec_n - 2)).rev() {
            let (zf, y) = sample_section(section, s as f32 / (sec_n - 1) as f32);
            pts.push([st.x, y * hh, -zf * st.w * ww]);
        }
        pts
    };

    let rings: Vec<Vec<[f32; 3]>> = stations.iter().map(ring_pts).collect();

    let mut positions = Vec::new();
    let mut normals = Vec::new();

    // A surface needs at least two stations and a non-empty ring.
    if rings.len() < 2 || rings[0].is_empty() {
        return HullMesh { positions, normals };
    }
    let n = rings[0].len();

    let mut push_tri = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
        let nrm = face_normal(a, b, c);
        positions.extend_from_slice(&[a, b, c]);
        normals.extend_from_slice(&[nrm, nrm, nrm]);
    };

    // Stitch consecutive rings: two triangles per quad around the loop.
    for s in 0..(stations.len() - 1) {
        let (ra, rb) = (&rings[s], &rings[s + 1]);
        for i in 0..n {
            let j = (i + 1) % n;
            push_tri(ra[i], ra[j], rb[i]);
            push_tri(ra[j], rb[j], rb[i]);
        }
    }

    HullMesh { positions, normals }
}

/// `sampleHeightProf(x)` — piecewise-linear over the height profile, or flat
/// `1.0` when no profile is present (the editor's no-traced-image default).
/// The profile is assumed sorted by x; out-of-range x clamps to the ends.
fn sample_height_prof(height: Option<&[Point2]>, x: f32) -> f32 {
    let prof = match height {
        Some(p) if !p.is_empty() => p,
        _ => return 1.0,
    };
    if x <= prof[0].x() as f32 {
        return prof[0].y() as f32;
    }
    let last = prof[prof.len() - 1];
    if x >= last.x() as f32 {
        return last.y() as f32;
    }
    for w in prof.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (ax, bx) = (a.x() as f32, b.x() as f32);
        if x >= ax && x <= bx {
            let span = bx - ax;
            let u = if span.abs() < 1e-8 { 0.0 } else { (x - ax) / span };
            return lerp(a.y() as f32, b.y() as f32, u);
        }
    }
    1.0
}

/// `noseHeightScale(px)` — section-height multiplier toward the prow, a
/// line-for-line port of the loft editor's `buildHull` helper. `nt` is the
/// nose taper (`0..1`); `px` is plan-x (`0` = stern .. `1` = prow). The taper
/// begins partway along (`start = 0.4`) and eases in (`t²`) to `1 − nt` at the
/// prow, so the hull comes to a point (`nt = 1` → `0`) instead of a full-height
/// axe-head. `nt <= 0` returns `1.0` (no taper) — the dagger default + the path
/// every existing caller takes (`LoftParams::default().nose_taper == 0.0`).
fn nose_height_scale(nt: f32, px: f32) -> f32 {
    if nt <= 0.0 {
        return 1.0;
    }
    const START: f32 = 0.4; // where the taper starts (toward the prow)
    if px <= START {
        return 1.0;
    }
    let t = (px - START) / (1.0 - START); // 0..1 from start to prow
    let eased = t * t; // ease-in so it stays full longer
    lerp(1.0, 1.0 - nt, eased) // at px=1, scale = 1-nt
}

/// `sampleSection(t)` — piecewise-linear across the section profile for
/// `t in 0..=1`, returning `(z half-width factor, y height)`. Mirrors the
/// POC's `sample_section`: maps `t` onto the `[0, n−1]` index space and lerps
/// within the bracketing pair.
fn sample_section(section: &[Point2], t: f32) -> (f32, f32) {
    let n = section.len();
    debug_assert!(n >= 2, "section needs at least two points");
    let f = t * (n - 1) as f32;
    let i = (f.floor() as usize).min(n - 2);
    let u = f - i as f32;
    (
        lerp(section[i].x() as f32, section[i + 1].x() as f32, u),
        lerp(section[i].y() as f32, section[i + 1].y() as f32, u),
    )
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Normalized face normal of triangle `(a, b, c)` via `(b−a) × (c−a)`.
/// Degenerate (zero-area) triangles fall back to `+y` so the result is always
/// a unit vector — mirrors the POC's guard.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The dagger plan from the POC (`DAGGER_PLAN`): needle prow (small
    /// half-width near x=1) widening to a broad stern (large half-width near
    /// x=0).
    fn dagger_plan() -> Vec<Point2> {
        vec![
            Point2([0.00, 0.95]),
            Point2([0.10, 0.98]),
            Point2([0.45, 0.72]),
            Point2([0.75, 0.42]),
            Point2([0.92, 0.18]),
            Point2([1.00, 0.02]),
        ]
    }

    /// The dagger section from the POC (`DAGGER_SECTION`): top → chine → belly.
    fn dagger_section() -> Vec<Point2> {
        vec![
            Point2([0.00, 0.55]),
            Point2([0.55, 0.40]),
            Point2([1.00, 0.05]),
            Point2([0.60, -0.45]),
            Point2([0.00, -0.55]),
        ]
    }

    #[test]
    fn vertex_count_matches_ring_stitch_formula() {
        let plan = dagger_plan();
        let section = dagger_section();
        let params = LoftParams::default();
        let mesh = loft_from_profiles(&plan, &section, None, params);

        let stations = plan.len();
        let ring_n = 2 * params.sec_n - 2;
        // Two tris per quad, 3 verts per tri, ring_n quads per station gap,
        // (stations - 1) gaps.
        let expected_verts = (stations - 1) * ring_n * 2 * 3;
        assert_eq!(mesh.positions.len(), expected_verts);
        assert_eq!(mesh.normals.len(), expected_verts);
        assert_eq!(mesh.positions.len() % 3, 0);
        assert_eq!(mesh.tri_count(), expected_verts / 3);
    }

    #[test]
    fn all_normals_are_unit_length() {
        let mesh = loft_from_profiles(&dagger_plan(), &dagger_section(), None, LoftParams::default());
        assert!(!mesh.normals.is_empty());
        for n in &mesh.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-4,
                "normal {n:?} has length {len}, expected unit",
            );
        }
    }

    #[test]
    fn loft_is_deterministic() {
        let a = loft_from_profiles(&dagger_plan(), &dagger_section(), None, LoftParams::default());
        let b = loft_from_profiles(&dagger_plan(), &dagger_section(), None, LoftParams::default());
        assert_eq!(a, b);
    }

    #[test]
    fn prow_is_narrower_than_stern() {
        // The dagger reads as a dagger: the bow (max +x) must be narrower in z
        // than the stern (min −x... here min x). Measure the z-extent of the
        // vertices nearest each end.
        let mesh = loft_from_profiles(&dagger_plan(), &dagger_section(), None, LoftParams::default());
        let xs: Vec<f32> = mesh.positions.iter().map(|p| p[0]).collect();
        let min_x = xs.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_x = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

        let z_extent_near = |target_x: f32| -> f32 {
            let mut zmax = 0.0f32;
            for p in &mesh.positions {
                if (p[0] - target_x).abs() < 1e-3 {
                    zmax = zmax.max(p[2].abs());
                }
            }
            zmax
        };
        let stern_width = z_extent_near(min_x); // x small = stern
        let prow_width = z_extent_near(max_x); // x large = prow
        assert!(
            prow_width < stern_width,
            "prow z-width {prow_width} should be < stern z-width {stern_width}",
        );
    }

    #[test]
    fn stretch_scales_length() {
        // Doubling stretch doubles the world-x extent of the hull.
        let base = loft_from_profiles(
            &dagger_plan(),
            &dagger_section(),
            None,
            LoftParams { stretch: 1.0, ..LoftParams::default() },
        );
        let stretched = loft_from_profiles(
            &dagger_plan(),
            &dagger_section(),
            None,
            LoftParams { stretch: 2.0, ..LoftParams::default() },
        );
        let x_extent = |m: &HullMesh| {
            let xs: Vec<f32> = m.positions.iter().map(|p| p[0]).collect();
            let lo = xs.iter().cloned().fold(f32::INFINITY, f32::min);
            let hi = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            hi - lo
        };
        let ratio = x_extent(&stretched) / x_extent(&base);
        assert!((ratio - 2.0).abs() < 1e-3, "stretch x2 should double length, ratio {ratio}");
    }

    #[test]
    fn height_profile_scales_height() {
        // A height profile of 2.0 everywhere doubles the y-extent vs flat 1.0.
        let flat = loft_from_profiles(&dagger_plan(), &dagger_section(), None, LoftParams::default());
        let tall_prof = vec![Point2([0.0, 2.0]), Point2([1.0, 2.0])];
        let tall = loft_from_profiles(
            &dagger_plan(),
            &dagger_section(),
            Some(&tall_prof),
            LoftParams::default(),
        );
        let y_extent = |m: &HullMesh| {
            let ys: Vec<f32> = m.positions.iter().map(|p| p[1]).collect();
            let lo = ys.iter().cloned().fold(f32::INFINITY, f32::min);
            let hi = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            hi - lo
        };
        let ratio = y_extent(&tall) / y_extent(&flat);
        assert!((ratio - 2.0).abs() < 1e-3, "height profile 2.0 should double height, ratio {ratio}");
    }

    #[test]
    fn loft_hull_unpacks_a_ship_design() {
        // End-to-end: a ShipDesign parsed from JSON lofts the same mesh as
        // calling loft_from_profiles with the unpacked fields.
        let json = r#"{
            "format": "broadside-ship",
            "version": 1,
            "plan": [[0.0, 0.95], [0.5, 0.6], [1.0, 0.02]],
            "section": [[0.0, 0.55], [1.0, 0.05], [0.0, -0.55]],
            "heightProfile": null,
            "settings": {
                "pitch": 26, "yaw": 28, "zoom": 1, "stretch": 2.0, "hscale": 0.7,
                "sup": true, "greeb": 0.6, "bands": 4, "laz": -50, "lel": 60,
                "res": { "w": 160, "h": 100 }
            },
            "grade": { "hue": 0, "sat": 1, "bri": 1, "con": 1, "gam": 1 }
        }"#;
        let design = ShipDesign::load_from_json(json.as_bytes()).unwrap();
        let via_design = loft_hull(&design);
        let via_profiles = loft_from_profiles(
            &design.plan,
            &design.section,
            None,
            LoftParams { stretch: 2.0, hscale: 0.7, ..LoftParams::default() },
        );
        assert_eq!(via_design, via_profiles);
        assert!(via_design.tri_count() > 0);
    }

    #[test]
    fn degenerate_single_station_yields_empty_mesh() {
        // One plan point can't be swept into a surface — no panic, empty mesh.
        let plan = vec![Point2([0.5, 0.5])];
        let mesh = loft_from_profiles(&plan, &dagger_section(), None, LoftParams::default());
        assert!(mesh.positions.is_empty());
        assert!(mesh.normals.is_empty());
        assert_eq!(mesh.tri_count(), 0);
    }
}
