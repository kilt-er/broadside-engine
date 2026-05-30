//! Procedural sprite atlas. Generates a single RGBA8 texture at startup with
//! placeholder pixel-art for every Broadside sprite. Slice-A: only the
//! SOLID_WHITE cell exists; later slices fill in ship faces, bow chevron,
//! ordnance, HUD glyphs, parallax layer art.
//!
//! The atlas is a fixed 8x8 grid of 32x32 cells, packed into a 256x256 RGBA8
//! texture. A cell is referenced by `(col, row)`; `cell_uvs()` converts a
//! cell coord into the normalized UV rectangle used by the sprite shader.

pub const ATLAS_SIZE: u32 = 256;
pub const CELL_SIZE: u32 = 32;
pub const CELLS_PER_ROW: u32 = ATLAS_SIZE / CELL_SIZE; // 8

/// Solid white cell. Multiply by the instance color tint to render a flat
/// colored quad — the workhorse for heat bars, range-band ticks, the lane
/// plate, the deep-space backdrop, and end-state overlays.
pub const SOLID_WHITE: (u32, u32) = (7, 7);

/// Convert (col, row) cell coordinates to a `(uv_min, uv_max)` tuple, each
/// in normalized [0, 1] texture space.
pub fn cell_uvs(cell: (u32, u32)) -> ([f32; 2], [f32; 2]) {
    let s = CELL_SIZE as f32 / ATLAS_SIZE as f32;
    let (c, r) = cell;
    (
        [c as f32 * s, r as f32 * s],
        [(c + 1) as f32 * s, (r + 1) as f32 * s],
    )
}

/// Generate the entire atlas as a tight RGBA8 byte buffer
/// (ATLAS_SIZE * ATLAS_SIZE * 4 bytes). Slice-A body is intentionally
/// minimal — only the SOLID_WHITE cell. Later slices extend this.
pub fn generate_atlas() -> Vec<u8> {
    let mut buf = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
    fill_cell(&mut buf, SOLID_WHITE, [255, 255, 255, 255]);
    buf
}

/* ---- primitives ----------------------------------------------------------- */

#[allow(dead_code)]
pub(crate) fn put_pixel(buf: &mut [u8], x: u32, y: u32, rgba: [u8; 4]) {
    if x >= ATLAS_SIZE || y >= ATLAS_SIZE {
        return;
    }
    let i = ((y * ATLAS_SIZE + x) * 4) as usize;
    buf[i] = rgba[0];
    buf[i + 1] = rgba[1];
    buf[i + 2] = rgba[2];
    buf[i + 3] = rgba[3];
}

#[allow(dead_code)]
pub(crate) fn fill_rect(buf: &mut [u8], x: u32, y: u32, w: u32, h: u32, rgba: [u8; 4]) {
    for dy in 0..h {
        for dx in 0..w {
            put_pixel(buf, x + dx, y + dy, rgba);
        }
    }
}

pub(crate) fn fill_cell(buf: &mut [u8], cell: (u32, u32), rgba: [u8; 4]) {
    let cx = cell.0 * CELL_SIZE;
    let cy = cell.1 * CELL_SIZE;
    fill_rect(buf, cx, cy, CELL_SIZE, CELL_SIZE, rgba);
}

#[allow(dead_code)]
pub(crate) fn cell_origin(cell: (u32, u32)) -> (u32, u32) {
    (cell.0 * CELL_SIZE, cell.1 * CELL_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_uvs_at_origin_is_unit_cell() {
        let (mn, mx) = cell_uvs((0, 0));
        assert_eq!(mn, [0.0, 0.0]);
        let expected = CELL_SIZE as f32 / ATLAS_SIZE as f32;
        assert!((mx[0] - expected).abs() < 1e-6);
        assert!((mx[1] - expected).abs() < 1e-6);
    }

    #[test]
    fn cell_uvs_at_corner_is_inside_unit_square() {
        let (mn, mx) = cell_uvs((CELLS_PER_ROW - 1, CELLS_PER_ROW - 1));
        assert!(mn[0] >= 0.0 && mn[1] >= 0.0);
        assert!(mx[0] <= 1.0 && mx[1] <= 1.0);
    }

    #[test]
    fn generate_atlas_sized_correctly() {
        let buf = generate_atlas();
        assert_eq!(buf.len(), (ATLAS_SIZE * ATLAS_SIZE * 4) as usize);
    }

    #[test]
    fn solid_white_cell_is_white() {
        let buf = generate_atlas();
        let (cx, cy) = (SOLID_WHITE.0 * CELL_SIZE, SOLID_WHITE.1 * CELL_SIZE);
        let i = ((cy * ATLAS_SIZE + cx) * 4) as usize;
        assert_eq!(&buf[i..i + 4], &[255, 255, 255, 255]);
    }
}
