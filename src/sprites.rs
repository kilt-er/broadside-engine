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
    asset_dir.join("sprites").join(format!(
        "{}_{}_{}.png",
        class,
        stance.slug(),
        view.slug(),
    ))
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
