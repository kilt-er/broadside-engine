//! Scene compositor — turns a [`crate::types::Board`] into a back-to-front
//! `Vec<SpriteInstance>` for [`crate::gfx::Gfx::render`].
//!
//! Render order (back to front):
//!
//! 1. Deep-space backdrop (clear color handles this today; later a starfield).
//! 2. Parallax layers — far nebula, distant planet, mid stars, foreground dust.
//! 3. Lane plate — trapezoid from [`crate::perspective::cell_footprint`].
//! 4. Range-band tick marks under the lane (five faint tick marks at band
//!    boundaries, colored per the analysis palette).
//! 5. Hazards (mines, drones, debris) — one cell each.
//! 6. Ships — front face + top face + bow chevron, composed via
//!    [`crate::perspective::ship_sprite`].
//! 7. Live ordnance (`Board.ordnance`) — torpedoes/missiles with a small trail.
//! 8. Beams — short-lived; not yet wired (event-bus driven later).
//! 9. Action queue glyphs above each ship.
//! 10. Telegraphed enemy intent icons above each enemy.
//! 11. Status badges (hullBreach / systemsOffline / targetLock / shieldsUp).
//! 12. End-state overlays (defeat / victory tints).
//!
//! Slice-A body is intentionally empty — the binary clears to deep-space ink
//! to prove the pipeline. Slice-D fills in the steps above.

use crate::gfx::SpriteInstance;
use crate::perspective::LaneGeometry;
use crate::types::Board;

/// Build the full frame's sprite instance list, back-to-front.
/// `lane` controls how cells project to screen; pass
/// [`crate::perspective::DEFAULT_LANE`] for the design-doc geometry, or a
/// custom one when the lane changes size at sector boundaries.
pub fn compose_scene(_board: &Board, _lane: &LaneGeometry) -> Vec<SpriteInstance> {
    // Slice-A: empty. Slice-D adds the documented render order.
    Vec::new()
}
