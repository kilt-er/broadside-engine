//! Offline tool: render the procedural ship silhouette at fixed view angles
//! and write the result to PNG files under `docs/sprite-refs/`. These
//! serve as visual templates for bruce's hand-painted PNG sprite art.
//!
//! Run with:
//!
//! ```bash
//! cargo run --bin render_refs --features render,runtime
//! ```
//!
//! Outputs (per the `SPRITE_SPEC.md` table):
//! - `docs/sprite-refs/frigate_bowOnFore_<deg>.png`
//! - `docs/sprite-refs/frigate_bowOnAft_<deg>.png`
//! - `docs/sprite-refs/frigate_broadside_<deg>.png`
//!
//! For each of three angles: 0° (pure side), 45° (isometric), 90° (top).
//! Scout / Gunboat dims aren't defined yet — when content lands them,
//! extend the `CLASSES` constant below.

use std::fs;
use std::path::PathBuf;

use broadside_engine::perspective::{ShipDims, FRIGATE_DIMS};

/// One ship class entry in the reference render table.
struct ClassDef {
    name: &'static str,
    dims: ShipDims,
}

/// Classes to render. Add Scout / Gunboat once their `ShipDims` land in
/// `perspective.rs` (see `SPRITE_SPEC.md`).
const CLASSES: &[ClassDef] = &[ClassDef {
    name: "frigate",
    dims: FRIGATE_DIMS,
}];

/// View angles to render references for. Three anchors out of the seven
/// scrub steps — the angles bruce will paint side / mid / top variants of.
const ANGLES_DEG: &[u32] = &[0, 45, 90];

#[derive(Clone, Copy)]
enum Orientation {
    BowOnFore,
    BowOnAft,
    Broadside,
}

impl Orientation {
    const fn slug(self) -> &'static str {
        match self {
            Self::BowOnFore => "bowOnFore",
            Self::BowOnAft => "bowOnAft",
            Self::Broadside => "broadside",
        }
    }
}

const ORIENTATIONS: &[Orientation] = &[
    Orientation::BowOnFore,
    Orientation::BowOnAft,
    Orientation::Broadside,
];

/// PNG fill / stroke / background colors (RGBA8). Match the renderer's
/// `PLAYER_HULL` palette so bruce's hand-painted art has a matching tone.
const BG: [u8; 4] = [12, 18, 28, 255]; // deep-space ink-ish
const FILL: [u8; 4] = [26, 42, 62, 255]; // player hull fill
const STROKE: [u8; 4] = [84, 207, 201, 255]; // player hull stroke (--gold)

fn main() {
    let out_dir = PathBuf::from("docs/sprite-refs");
    fs::create_dir_all(&out_dir).expect("create sprite-refs dir");

    for class in CLASSES {
        for &deg in ANGLES_DEG {
            for &orient in ORIENTATIONS {
                let path = out_dir.join(format!("{}_{}_{:02}.png", class.name, orient.slug(), deg));
                let img = render_silhouette(class.dims, orient, deg);
                img.save(&path).expect("write png");
                println!("wrote {}", path.display());
            }
        }
    }
}

/// Render one silhouette to an image buffer at the given angle. Includes
/// a small margin around the silhouette so the strokes aren't clipped.
fn render_silhouette(dims: ShipDims, orient: Orientation, deg: u32) -> image::RgbaImage {
    let angle = (deg as f32).to_radians();
    let cos_a = angle.cos();
    let sin_a = angle.sin();
    let (width, depth_dim) = match orient {
        Orientation::Broadside => (dims.beam, dims.length),
        _ => (dims.length, dims.beam),
    };
    let total_h = dims.height * cos_a + depth_dim * sin_a;

    // Image canvas: fit silhouette + 16-px margin on each side.
    let margin = 16i32;
    let w = (width as i32) + 2 * margin;
    let h = (total_h.max(2.0) as i32) + 2 * margin;
    let mut img = image::RgbaImage::from_pixel(w as u32, h as u32, image::Rgba(BG));

    // Silhouette anchor: centered horizontally, centered vertically on
    // canvas (mirroring how the renderer centers it on the lane line).
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let half_h = total_h / 2.0;
    let top_y = cy - half_h;
    let base_y = cy + half_h;

    match orient {
        Orientation::Broadside => rasterize_broadside(&mut img, cx, top_y, base_y, width, cos_a),
        Orientation::BowOnFore => rasterize_bow_on(&mut img, cx, top_y, base_y, width, cos_a, true),
        Orientation::BowOnAft => rasterize_bow_on(&mut img, cx, top_y, base_y, width, cos_a, false),
    }
    img
}

/// Bow-on silhouette: square stern + tapering bow triangle.
fn rasterize_bow_on(
    img: &mut image::RgbaImage,
    cx: f32,
    top_y: f32,
    base_y: f32,
    width: f32,
    cos_a: f32,
    bow_fore: bool,
) {
    let full_bow_w = width * 0.25;
    let bow_w = full_bow_w * cos_a;
    let body_w = width - bow_w;
    let mid_y = f32::midpoint(top_y, base_y);
    let sign = if bow_fore { 1.0 } else { -1.0 };
    let stern_edge_x = cx - sign * width / 2.0;
    let bow_corner_x = cx - sign * width / 2.0 + sign * body_w;
    let bow_tip_x = cx + sign * width / 2.0;

    let left = stern_edge_x.min(bow_corner_x);
    let right = stern_edge_x.max(bow_corner_x);

    // Body quad fill.
    fill_quad(
        img,
        [
            [left, top_y],
            [right, top_y],
            [right, base_y],
            [left, base_y],
        ],
        FILL,
    );
    // Bow triangle fill (3-vertex polygon).
    fill_polygon(
        img,
        &[
            (bow_corner_x, top_y),
            (bow_tip_x, mid_y),
            (bow_corner_x, base_y),
        ],
        FILL,
    );

    // Outline strokes.
    stroke_line(img, stern_edge_x, top_y, stern_edge_x, base_y, STROKE);
    stroke_line(img, stern_edge_x, top_y, bow_corner_x, top_y, STROKE);
    stroke_line(img, stern_edge_x, base_y, bow_corner_x, base_y, STROKE);
    stroke_line(img, bow_corner_x, top_y, bow_tip_x, mid_y, STROKE);
    stroke_line(img, bow_corner_x, base_y, bow_tip_x, mid_y, STROKE);
}

/// Broadside silhouette: rectangle + superstructure bump.
fn rasterize_broadside(
    img: &mut image::RgbaImage,
    cx: f32,
    top_y: f32,
    base_y: f32,
    width: f32,
    cos_a: f32,
) {
    let half_w = width / 2.0;
    let height = base_y - top_y;
    // Main hull.
    fill_quad(
        img,
        [
            [cx - half_w, top_y],
            [cx + half_w, top_y],
            [cx + half_w, base_y],
            [cx - half_w, base_y],
        ],
        FILL,
    );
    // Superstructure bump.
    let bump_w = width * 0.4;
    let bump_h = height * 0.30 * cos_a.max(0.1);
    fill_quad(
        img,
        [
            [cx - bump_w / 2.0, top_y - bump_h],
            [cx + bump_w / 2.0, top_y - bump_h],
            [cx + bump_w / 2.0, top_y],
            [cx - bump_w / 2.0, top_y],
        ],
        FILL,
    );
    // Outlines.
    stroke_line(img, cx - half_w, top_y, cx + half_w, top_y, STROKE);
    stroke_line(img, cx + half_w, top_y, cx + half_w, base_y, STROKE);
    stroke_line(img, cx + half_w, base_y, cx - half_w, base_y, STROKE);
    stroke_line(img, cx - half_w, base_y, cx - half_w, top_y, STROKE);
    stroke_line(
        img,
        cx - bump_w / 2.0,
        top_y - bump_h,
        cx + bump_w / 2.0,
        top_y - bump_h,
        STROKE,
    );
    stroke_line(
        img,
        cx + bump_w / 2.0,
        top_y - bump_h,
        cx + bump_w / 2.0,
        top_y,
        STROKE,
    );
    stroke_line(
        img,
        cx - bump_w / 2.0,
        top_y - bump_h,
        cx - bump_w / 2.0,
        top_y,
        STROKE,
    );
}

/// Fill a 4-vertex convex polygon with the given color (axis-aligned-ish).
fn fill_quad(img: &mut image::RgbaImage, q: [[f32; 2]; 4], color: [u8; 4]) {
    let poly = [
        (q[0][0], q[0][1]),
        (q[1][0], q[1][1]),
        (q[2][0], q[2][1]),
        (q[3][0], q[3][1]),
    ];
    fill_polygon(img, &poly, color);
}

/// Convex-polygon scanline fill. For each integer scanline within the
/// polygon's y-range, find the leftmost and rightmost x crossings of the
/// polygon edges and paint between them.
fn fill_polygon(img: &mut image::RgbaImage, verts: &[(f32, f32)], color: [u8; 4]) {
    if verts.len() < 3 {
        return;
    }
    let y_min = verts.iter().map(|(_, y)| *y).fold(f32::INFINITY, f32::min);
    let y_max = verts
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::NEG_INFINITY, f32::max);
    let (w, h) = (img.width() as i32, img.height() as i32);
    let y_start = (y_min.floor() as i32).max(0);
    let y_end = (y_max.ceil() as i32).min(h - 1);
    for y in y_start..=y_end {
        let yf = y as f32 + 0.5;
        let mut xs: Vec<f32> = Vec::new();
        for i in 0..verts.len() {
            let (ax, ay) = verts[i];
            let (bx, by) = verts[(i + 1) % verts.len()];
            // Edge crosses scanline if (ay <= yf < by) || (by <= yf < ay)
            if (ay <= yf) != (by <= yf) {
                let t = (yf - ay) / (by - ay);
                xs.push(ax + t * (bx - ax));
            }
        }
        if xs.len() < 2 {
            continue;
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // For a convex polygon there are exactly 2 crossings.
        let x_left = (xs[0].floor() as i32).max(0);
        let x_right = (xs[xs.len() - 1].ceil() as i32).min(w - 1);
        for x in x_left..=x_right {
            img.put_pixel(x as u32, y as u32, image::Rgba(color));
        }
    }
}

/// Draw a 1-pixel line segment from (x0, y0) to (x1, y1) using Bresenham.
fn stroke_line(img: &mut image::RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 4]) {
    let (mut x0, mut y0) = (x0.round() as i32, y0.round() as i32);
    let (x1, y1) = (x1.round() as i32, y1.round() as i32);
    let (w, h) = (img.width() as i32, img.height() as i32);
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        if x0 >= 0 && x0 < w && y0 >= 0 && y0 < h {
            img.put_pixel(x0 as u32, y0 as u32, image::Rgba(color));
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}
