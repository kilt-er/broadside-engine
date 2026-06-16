//! PNG sprite loading + per-ship sprite-handle lookup.
//!
//! Looks for `assets/sprites/<class>_<stance>_<view>.png` under the project
//! root. Returns `None` for missing files so the renderer can fall back to
//! the procedural silhouette. PNGs are decoded once at startup into RGBA8
//! buffers and uploaded to the GPU as separate textures by `gfx.rs`.
//!
//! Filename convention (mirrors `docs/SPRITE_SPEC.md`):
//!
//! - `class` ∈ `{ frigate, scout, gunboat }`
//! - `stance` ∈ `{ bowOnFore, bowOnAft, broadside }`
//! - `view` ∈ `{ side, top }`
//!
//! Example: `assets/sprites/frigate_broadside_side.png`.

use std::path::{Path, PathBuf};

/// Which side of a ship the artist painted. Side is the 0° silhouette;
/// top is the 90° silhouette. The renderer blends between them at runtime
/// via the camera-angle scrubber.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteView {
    Side,
    Top,
}

impl SpriteView {
    pub fn slug(self) -> &'static str {
        match self {
            SpriteView::Side => "side",
            SpriteView::Top => "top",
        }
    }
}

/// The orientation the sprite was painted for. `BowOnFore` and `BowOnAft`
/// are horizontally mirrored, but the artist may paint them separately if
/// the silhouette isn't symmetric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteStance {
    BowOnFore,
    BowOnAft,
    Broadside,
}

impl SpriteStance {
    pub fn slug(self) -> &'static str {
        match self {
            SpriteStance::BowOnFore => "bowOnFore",
            SpriteStance::BowOnAft => "bowOnAft",
            SpriteStance::Broadside => "broadside",
        }
    }
}

/// Which 3D loft mesh a ship renders with, when it renders as a live loft
/// ship rather than a 2D silhouette. The renderer keeps one uploaded vertex
/// buffer per variant and shares it across every ship of that kind (e.g. all
/// four enemy placeholders share the one vendored CAD hull).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoftMeshKind {
    /// The vendored CAD ship import (`assets/ships/broadside-ship.glb`), tinted
    /// a distinct cool/friendly hue for the PLAYER so it reads apart from the
    /// enemy fleet while sharing the same hull geometry.
    PlayerCad,
    /// The PLAYER's actual class hull, LOFTED from a `ShipDesign` (the Aegis
    /// design in `assets/ships/broadside-ship-library_v2.json`) via the
    /// `loft.rs` path, tinted the player hue. Preferred over [`Self::PlayerCad`]
    /// when installed, so the player renders as its real Aegis-class hull rather
    /// than the generic vendored CAD mesh.
    PlayerLoft,
    /// The vendored CAD ship import (`assets/ships/broadside-ship.glb`) — its
    /// authored colours (orange accent) — used for the enemy placeholders.
    EnemyCad,
}

/// A decoded sprite ready to upload to the GPU.
#[derive(Clone)]
pub struct SpriteImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixel data, top-row first.
    pub rgba: Vec<u8>,
}

/// Try to load a sprite PNG by class / stance / view. Returns `None` when
/// the file is missing or doesn't decode. **Never panics** — render
/// callers should fall back to the procedural silhouette on None.
pub fn load_sprite(
    asset_dir: &Path,
    class: &str,
    stance: SpriteStance,
    view: SpriteView,
) -> Option<SpriteImage> {
    let path = sprite_path(asset_dir, class, stance, view);
    let img = match image::open(&path) {
        Ok(i) => i,
        Err(e) => {
            log::debug!("sprite load skipped: {} ({})", path.display(), e);
            return None;
        }
    };
    let rgba = img.to_rgba8();
    Some(SpriteImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Filesystem path for a sprite PNG, given the asset directory root.
/// Public so the binary can log "looking for X" messages if a sprite is
/// missing.
pub fn sprite_path(
    asset_dir: &Path,
    class: &str,
    stance: SpriteStance,
    view: SpriteView,
) -> PathBuf {
    asset_dir
        .join("sprites")
        .join(format!("{}_{}_{}.png", class, stance.slug(), view.slug(),))
}

/// Load both views (side + top) for a ship sprite. Either or both may
/// return `None` if the artist hasn't painted that face yet — the renderer
/// blends what's available, falling back to procedural otherwise.
pub fn load_sprite_pair(
    asset_dir: &Path,
    class: &str,
    stance: SpriteStance,
) -> (Option<SpriteImage>, Option<SpriteImage>) {
    (
        load_sprite(asset_dir, class, stance, SpriteView::Side),
        load_sprite(asset_dir, class, stance, SpriteView::Top),
    )
}

/// Return a horizontally-mirrored copy of `src`. Row-major RGBA8, so we
/// reverse each row's pixel order while keeping rows themselves in place.
/// Used by the loader to derive a `bowOnAft` sprite from a `bowOnFore`
/// when the artist hasn't painted the aft variant separately —
/// bow-on ships are visually symmetric across the fore/aft flip, so the
/// mirror is a faithful render.
///
/// Explicit `bowOnAft_<view>.png` files take precedence; the loader only
/// invokes this when the explicit file is missing.
pub fn mirror_horizontal(src: &SpriteImage) -> SpriteImage {
    let w = src.width as usize;
    let h = src.height as usize;
    let mut rgba = Vec::with_capacity(src.rgba.len());
    for row in 0..h {
        let row_start = row * w * 4;
        // Walk pixels in reverse: rightmost pixel of the source becomes
        // leftmost of the mirror, etc. Each pixel is 4 bytes (RGBA).
        for col in (0..w).rev() {
            let p = row_start + col * 4;
            rgba.extend_from_slice(&src.rgba[p..p + 4]);
        }
    }
    SpriteImage {
        width: src.width,
        height: src.height,
        rgba,
    }
}

/// Return a 90° clockwise-rotated copy of `src`. Output dimensions swap
/// (`width` ↔ `height`). RGBA8 in / RGBA8 out.
///
/// Mapping in image (y-down) coordinates:
/// ```text
///   src[sx, sy]  →  dst[dh - 1 - sy, sx]   (with dw = src.height, dh = src.width)
/// ```
/// So the source's top edge becomes the destination's right edge, and a
/// "bow at +x" silhouette in the source becomes a "bow at -y" silhouette
/// in the destination. The renderer's broadside chevron overlay reads
/// the bow direction explicitly, so the absolute rotation handedness
/// isn't visually load-bearing — `rotate_90_cw` was chosen for the
/// alignment with `image::imageops::rotate90`'s conventions per the
/// brief.
///
/// Used by [`crate::gfx::Gfx::try_load_ship_sprites`] as step 2 of the
/// `broadside_top` fallback chain: explicit → rotate90(bowOnFore_top)
/// → procedural. `broadside_side` has NO auto-derivation — it's a
/// front-face view of the hull (beam × height) that can't be
/// reconstructed from the side or top of a bow-on sprite.
pub fn rotate_90_cw(src: &SpriteImage) -> SpriteImage {
    let sw = src.width as usize;
    let sh = src.height as usize;
    let dw = sh;
    let dh = sw;
    let mut rgba = vec![0u8; dw * dh * 4];
    for sy in 0..sh {
        for sx in 0..sw {
            let src_p = (sy * sw + sx) * 4;
            // 90° CW in y-down: dst x = dw - 1 - sy, dst y = sx.
            let dx = dw - 1 - sy;
            let dy = sx;
            let dst_p = (dy * dw + dx) * 4;
            rgba[dst_p..dst_p + 4].copy_from_slice(&src.rgba[src_p..src_p + 4]);
        }
    }
    SpriteImage {
        width: dw as u32,
        height: dh as u32,
        rgba,
    }
}

/// Read-only lookup of which ship sprites are currently uploaded.
/// `hud::compose_scene` queries this to decide whether to emit a textured
/// or procedural silhouette per ship. `Gfx` implements it over its own
/// internal registry.
pub trait SpriteRegistry {
    fn has(&self, class: &str, stance: SpriteStance, view: SpriteView) -> bool;

    /// Convenience: both views present.
    fn has_pair(&self, class: &str, stance: SpriteStance) -> bool {
        self.has(class, stance, SpriteView::Side) && self.has(class, stance, SpriteView::Top)
    }

    /// Whether the v2 **15-facing** baked frame `index` (0..15) is loaded for
    /// `class` — keyed `"<class>_f{index:02}"` (see [`crate::facing_wheel`]).
    /// This is the v2 chase-cam render path: each facing is ONE pre-lit PNG
    /// (no side/top blend), swapped per orientation, drawn UNLIT. Default
    /// `false` (the no-GPU / test registries hold no facing sheet); `Gfx`
    /// delegates to its uploaded-sprite map. The renderer falls back to the
    /// procedural placeholder when this is false, so the player shows a clean
    /// box until Bruce's `<class>_f00..f14` bake lands.
    fn has_facing(&self, _class: &str, _index: usize) -> bool {
        false
    }

    /// Which 3D loft mesh (if any) the given ship renders with. `Some(kind)`
    /// makes `hud::push_ship` emit a `LoftShip` quad for that ship and skip its
    /// 2D silhouette — the "loft if the ship has a 3D asset, else 2D" dispatch,
    /// generalised per-ship: the player demo renders the grey dagger, the enemy
    /// placeholders the vendored CAD hull. `is_player` distinguishes the player
    /// from enemies without this trait depending on `types::Faction`. Default
    /// `None` (the no-GPU / test registries don't loft).
    fn loft_kind(&self, _ship_id: &str, _is_player: bool) -> Option<LoftMeshKind> {
        None
    }
}

/// No-op registry — every lookup returns false. Useful for tests and for
/// `compose_scene` callers that don't have a GPU registry to query.
pub struct EmptySpriteRegistry;

impl SpriteRegistry for EmptySpriteRegistry {
    fn has(&self, _class: &str, _stance: SpriteStance, _view: SpriteView) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sprite_path_format_matches_spec() {
        let p = sprite_path(
            Path::new("assets"),
            "frigate",
            SpriteStance::BowOnFore,
            SpriteView::Side,
        );
        // Path separator is OS-specific; canonicalize to forward slashes
        // for the assertion.
        let s = p.to_string_lossy().replace('\\', "/");
        assert_eq!(s, "assets/sprites/frigate_bowOnFore_side.png");
    }

    #[test]
    fn sprite_path_uses_stance_and_view_slugs() {
        let p = sprite_path(
            Path::new("a"),
            "scout",
            SpriteStance::Broadside,
            SpriteView::Top,
        );
        let s = p.to_string_lossy().replace('\\', "/");
        assert_eq!(s, "a/sprites/scout_broadside_top.png");
    }

    #[test]
    fn load_sprite_returns_none_for_missing_file() {
        // No assets dir under the test temp; load_sprite should not panic.
        let result = load_sprite(
            Path::new("/nonexistent/asset/root"),
            "frigate",
            SpriteStance::BowOnFore,
            SpriteView::Side,
        );
        assert!(result.is_none());
    }

    #[test]
    fn mirror_horizontal_flips_pixel_order_within_each_row() {
        // 2-pixel-wide × 1-pixel-tall image, RGBA. Left pixel red, right
        // pixel blue. Mirror should swap them: left blue, right red.
        let src = SpriteImage {
            width: 2,
            height: 1,
            rgba: vec![
                255, 0, 0, 255, // red
                0, 0, 255, 255, // blue
            ],
        };
        let m = mirror_horizontal(&src);
        assert_eq!(m.width, 2);
        assert_eq!(m.height, 1);
        assert_eq!(
            m.rgba,
            vec![
                0, 0, 255, 255, // blue (was right, now left)
                255, 0, 0, 255, // red (was left, now right)
            ]
        );
    }

    #[test]
    fn mirror_horizontal_preserves_rows() {
        // 2x2: row 0 = [A B], row 1 = [C D]. Mirror should yield
        // row 0 = [B A], row 1 = [D C] — row order unchanged.
        let src = SpriteImage {
            width: 2,
            height: 2,
            rgba: vec![
                1, 1, 1, 1, 2, 2, 2, 2, // row 0
                3, 3, 3, 3, 4, 4, 4, 4, // row 1
            ],
        };
        let m = mirror_horizontal(&src);
        assert_eq!(
            m.rgba,
            vec![
                2, 2, 2, 2, 1, 1, 1, 1, // row 0 reversed
                4, 4, 4, 4, 3, 3, 3, 3, // row 1 reversed
            ]
        );
    }

    #[test]
    fn rotate_90_cw_swaps_dimensions() {
        let src = SpriteImage {
            width: 4,
            height: 2,
            rgba: vec![0; 4 * 2 * 4],
        };
        let r = rotate_90_cw(&src);
        assert_eq!(r.width, 2, "rotated width = source height");
        assert_eq!(r.height, 4, "rotated height = source width");
        assert_eq!(r.rgba.len(), src.rgba.len());
    }

    #[test]
    fn rotate_90_cw_maps_top_left_to_top_right() {
        // 2×2 source: row0 = [A B], row1 = [C D]. After 90° CW:
        //   col 0 (top of rotated, leftmost) = C, A (reading top→bottom)
        //   col 1 (right of rotated, rightmost) = D, B
        // Output is 2 wide × 2 tall, so:
        //   row 0 (top): [C A]   (was leftmost column of src, top→bottom)
        //   row 1 (bot): [D B]   (was rightmost column of src, top→bottom)
        let src = SpriteImage {
            width: 2,
            height: 2,
            rgba: vec![
                10, 10, 10, 10, 20, 20, 20, 20, // row 0: A B
                30, 30, 30, 30, 40, 40, 40, 40, // row 1: C D
            ],
        };
        let r = rotate_90_cw(&src);
        assert_eq!(r.width, 2);
        assert_eq!(r.height, 2);
        assert_eq!(
            r.rgba,
            vec![
                30, 30, 30, 30, 10, 10, 10, 10, // row 0: C A
                40, 40, 40, 40, 20, 20, 20, 20, // row 1: D B
            ]
        );
    }

    #[test]
    fn rotate_90_cw_four_times_is_identity() {
        // Property: rotating any sprite 90° four times returns it
        // bit-exact to the source (both dims AND bytes).
        let src = SpriteImage {
            width: 3,
            height: 4,
            rgba: (0..(3 * 4 * 4) as u8).collect(),
        };
        let r1 = rotate_90_cw(&src);
        let r2 = rotate_90_cw(&r1);
        let r3 = rotate_90_cw(&r2);
        let r4 = rotate_90_cw(&r3);
        assert_eq!(r4.width, src.width);
        assert_eq!(r4.height, src.height);
        assert_eq!(r4.rgba, src.rgba);
    }

    #[test]
    fn rotate_90_cw_on_frigate_top_dimensions_match_sprite_spec() {
        // SPRITE_SPEC says Frigate bowOnFore_top is 120×60 and
        // broadside_top is 60×120. Rotating fore_top should give
        // broadside_top's dimensions.
        let src = SpriteImage {
            width: 120,
            height: 60,
            rgba: vec![0; 120 * 60 * 4],
        };
        let r = rotate_90_cw(&src);
        assert_eq!(r.width, 60);
        assert_eq!(r.height, 120);
    }

    #[test]
    fn mirror_horizontal_double_flip_is_identity() {
        // Property: mirror(mirror(x)) == x for any sprite.
        let src = SpriteImage {
            width: 3,
            height: 2,
            rgba: (0..24).collect(),
        };
        let twice = mirror_horizontal(&mirror_horizontal(&src));
        assert_eq!(twice.rgba, src.rgba);
        assert_eq!(twice.width, src.width);
        assert_eq!(twice.height, src.height);
    }

    #[test]
    fn load_sprite_pair_is_resilient_to_partial_assets() {
        // Both views missing → (None, None).
        let (side, top) = load_sprite_pair(
            Path::new("/nonexistent"),
            "frigate",
            SpriteStance::Broadside,
        );
        assert!(side.is_none());
        assert!(top.is_none());
    }
}
