//! wgpu state, instanced sprite batcher, and virtual-resolution presentation.
//!
//! Ported from `GameEngine/mvp/src/gfx.rs` and adapted for Broadside.
//! Structural changes from the source:
//!
//! 1. Virtual resolution is **480×270** (v2 decision #1, 2026-06-14): a 16:9
//!    pixel-art canvas that upscales by a FIXED ×4 NEAREST factor to exactly
//!    1920×1080. This re-founds the resolution from the old 1320×480-LINEAR
//!    board; the 1D HUD/loft positioning built for 1320×480 looks wrong until
//!    the v2 board lands — expected during the rebuild, not a regression.
//! 2. The view uniform projects ONTO the virtual-pixel grid: world is
//!    `[0, VIRTUAL_W] × [0, VIRTUAL_H]` with y-down to match `perspective`'s
//!    screen-space convention. The source engine used a NDC-half-size world;
//!    the Broadside renderer feeds raw pixel coordinates from
//!    [`crate::perspective`] straight through.
//! 3. The atlas comes from [`crate::atlas`] (Broadside content) rather than
//!    the `GameEngine` humanoid set.
//! 4. The clear color is deep-space ink (`#080c14`), matching the analysis
//!    HTML's `--ink` token.
//!
//! Two passes per frame, unchanged in spirit from the source:
//!
//!   1. **Sprite pass** — instanced colored quads drawn into the 480×270
//!      offscreen target. Every game pixel is one texel here.
//!   2. **Blit pass** — the offscreen texture is sampled with
//!      nearest-neighbor filtering and drawn to the swapchain at a FIXED ×4
//!      integer scale (→ 1920×1080), centered with black letterboxing on any
//!      other window size, for the crisp pixel look.
//!
//! Sprite content (the actual `Vec<SpriteInstance>` for a frame) lives in
//! [`crate::hud`]; this module is the pipeline scaffold only.

use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::atlas;

/// Virtual canvas size — every drawn sprite is in this coordinate space.
/// 480×270 (16:9) is the v2 pixel-art canvas (decision #1); it upscales by a
/// fixed ×[`FIXED_UPSCALE`] NEAREST factor to exactly 1920×1080.
pub const VIRTUAL_W: u32 = 480;
pub const VIRTUAL_H: u32 = 270;

/// Fixed integer upscale from the [`VIRTUAL_W`]×[`VIRTUAL_H`] offscreen to the
/// reference window: 480×270 × 4 = 1920×1080. The final blit always snaps to
/// this multiple (never a continuous fit-scale) so every offscreen texel maps
/// to an exact 4×4 block of window pixels — crisp, shimmer-free pixel art.
/// Windows larger than 1920×1080 letterbox the remainder; smaller windows fall
/// back to the largest integer scale that still fits (see `update_blit_uniform`).
pub const FIXED_UPSCALE: u32 = 4;

/// (#135 Bruce) The scene/background BOOT-default resolution: 640×360 (the middle
/// [`SCENE_RES_PRESETS`] step). Bruce wants the whole-scene canvas to boot crisper
/// than the 480×270 floor — this is the value a fresh [`Gfx`] starts at (the way
/// 480×300 is the ship-loft boot default, #91). `;`/`'` still cycle from here.
pub const BOOT_SCENE_W: u32 = 640;
pub const BOOT_SCENE_H: u32 = 360;

/// (#76 scene-res) The LIVE scene (offscreen) resolution, cycled by `;` / `'`.
/// Initialized to the [`BOOT_SCENE_W`]×[`BOOT_SCENE_H`] boot default (#135);
/// [`Gfx::cycle_scene_res`] recreates the offscreen texture, view, and blit at the
/// new size and updates these. Stored as process-global atomics rather than threaded
/// through every signature because the renderer's free helper functions
/// ([`crate::hud`], [`crate::background`]) need the runtime canvas size WITHOUT a
/// `&Gfx` param — the lead's "gfx getter, no public-hud signature sweep" call. Read
/// via [`scene_w`] / [`scene_h`]; written ONLY through [`Gfx::set_scene_size`]
/// (constructor + cycle), which keeps them in lock-step with the offscreen texture.
static SCENE_W: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(BOOT_SCENE_W);
static SCENE_H: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(BOOT_SCENE_H);

/// The current LIVE scene (offscreen) WIDTH in virtual pixels. Equals
/// [`VIRTUAL_W`] until a `;`/`'` scene-res cycle changes it. The renderer's
/// screen-space math ([`crate::hud`], [`crate::background`], the projector via
/// [`crate::projector::ProjectorConfig::for_scene`]) reads this so every overlay
/// tracks the live canvas. See [`SCENE_W`].
pub fn scene_w() -> u32 {
    SCENE_W.load(std::sync::atomic::Ordering::Relaxed)
}

/// The current LIVE scene (offscreen) HEIGHT in virtual pixels. Equals
/// [`VIRTUAL_H`] until a `;`/`'` scene-res cycle changes it. See [`scene_w`].
pub fn scene_h() -> u32 {
    SCENE_H.load(std::sync::atomic::Ordering::Relaxed)
}

/// (#139 Bruce) Live GRID-PITCH step, 0..=[`GRID_PITCH_STEPS`]. `0` = the chase-cam
/// look; each step (the `G` key) tilts ~5° toward TOP-DOWN. A process-global like
/// the scene size so every `ProjectorConfig::for_scene(..).with_pitch(grid_pitch_t())`
/// call site (grid, cells, movement, threats, ordnance) shares ONE pitch — the
/// projector is the single spatial source, so they all reproject together.
static GRID_PITCH_STEP: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(BOOT_GRID_PITCH_STEP);

/// (#165 Bruce) The grid pitch step the game BOOTS at. Bruce's pick: step 2 of 8 —
/// the board starts tilted partway toward top-down rather than the flat chase-cam
/// (step 0). `G` still cycles the full 0..=8 arc from here.
pub const BOOT_GRID_PITCH_STEP: u32 = 2;

/// Number of pitch steps from chase-cam (step 0) to near-top-down (the last step).
/// ~5° each toward overhead; 8 steps ≈ a 40° swing past the ~20° base.
pub const GRID_PITCH_STEPS: u32 = 8;

/// The live pitch as `t` ∈ [0, 1] for [`crate::projector::ProjectorConfig::with_pitch`]
/// (`0` chase-cam, `1` near-overhead). Step `n` → `n / GRID_PITCH_STEPS`.
pub fn grid_pitch_t() -> f32 {
    GRID_PITCH_STEP.load(std::sync::atomic::Ordering::Relaxed) as f32 / GRID_PITCH_STEPS as f32
}

/// The live pitch STEP (0..=[`GRID_PITCH_STEPS`]) for the debug readout.
pub fn grid_pitch_step() -> u32 {
    GRID_PITCH_STEP.load(std::sync::atomic::Ordering::Relaxed)
}

/// (#139) Cycle the grid pitch one step toward top-down, wrapping back to the
/// chase-cam (step 0) after the last step. Returns the new step. Bound to `G`.
pub fn cycle_grid_pitch() -> u32 {
    let next = (grid_pitch_step() + 1) % (GRID_PITCH_STEPS + 1);
    GRID_PITCH_STEP.store(next, std::sync::atomic::Ordering::Relaxed);
    next
}

/// (#140/#142/#151 Bruce) GRID MODE — which projection the `G` pitch arc feeds. The
/// `T` key cycles four modes:
///   0 = DRAWBRIDGE: constant-footprint `with_pitch` (the #139 ballooning one, kept
///       for comparison);
///   1 = STRETCH-CURVED: `with_stretch` — grid stretches vertically toward a uniform
///       top-down square (~constant ship size); column edges BOW through the mid-arc;
///   2 = STRETCH-STRAIGHT (stepped): `with_stretch_straight` — column edges straighten
///       per-cell, so depth lines KINK at row boundaries ("stepped per quadrant");
///   3 = STRETCH-CONTINUOUS: `with_stretch_continuous` — each depth line is ONE straight
///       front-to-back line, no kinks (Bruce's continuous-straight ask, #151).
/// At pitch step 0 all four are byte-identical to the chase-cam (each reduces to the
/// perspective base), so the step-0 no-regression gate holds in every mode.
static GRID_MODE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(BOOT_GRID_MODE);

/// (#165/#169 Bruce) The grid MODE the game BOOTS at. Bruce's pick: mode 3 =
/// STRETCH-CONTINUOUS (the "STRAIGHT+" tag) — each depth line is ONE straight
/// front-to-back line that converges cleanly to the vanishing point, with NO
/// per-cell kinks at the row boundaries. Mode 2 (stepped STRAIGHT) was the prior
/// boot pick but its column edges visibly KINK at each row line ("everything is
/// called straight … I want straight lines not stepped"); both modes 2 and 3 carry
/// "STRAIGHT" in the readout, which is why the two looked indistinguishable by tag.
/// Combined with [`BOOT_GRID_PITCH_STEP`] = 2 the board boots reading
/// "PITCH 2/8 STRAIGHT+" (partway tilted, truly straight converging lines). `T`
/// still cycles all four modes from here.
pub const BOOT_GRID_MODE: u32 = 3;

/// Number of grid modes the `T` key cycles through (drawbridge / stretch-curved /
/// stretch-straight-stepped / stretch-continuous).
pub const GRID_MODES: u32 = 4;

/// The active grid mode `0..GRID_MODES` (see [`GRID_MODE`]).
pub fn grid_mode() -> u32 {
    GRID_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// (#140) Back-compat: whether ANY stretch mode is active (modes 1/2/3). The capture
/// env + older call sites read this; the projector branch keys on [`grid_mode`].
pub fn grid_stretch_on() -> bool {
    grid_mode() != 0
}

/// (#142) Whether the active stretch mode is a STRAIGHT variant (stepped mode 2 OR
/// continuous mode 3).
pub fn grid_stretch_straight() -> bool {
    matches!(grid_mode(), 2 | 3)
}

/// (#140/#142/#151) Cycle the grid mode (drawbridge -> stretch-curved -> stretch-
/// straight-stepped -> stretch-continuous -> drawbridge); returns the new mode. `T`.
pub fn cycle_grid_mode() -> u32 {
    let next = (grid_mode() + 1) % GRID_MODES;
    GRID_MODE.store(next, std::sync::atomic::Ordering::Relaxed);
    next
}

/// (#142/#151/#169) A short tag for the active grid mode, for the debug readout:
/// "" (drawbridge), "STRETCH" (curved), "STEPPED" (per-cell kinked straight, mode 2),
/// or "STRAIGHT" (continuous front-to-back lines, mode 3). Modes 2 and 3 used to BOTH
/// read "STRAIGHT" / "STRAIGHT+", which made them indistinguishable at a glance
/// (Bruce: "both are called straight"); mode 3 is the TRUE straight one (and the boot
/// default) so it now owns the clean "STRAIGHT" name, and mode 2 reads "STEPPED" to
/// name its kink. Display-only — no logic keys on this string.
pub fn grid_mode_tag() -> &'static str {
    match grid_mode() {
        1 => "STRETCH",
        2 => "STEPPED",
        3 => "STRAIGHT",
        _ => "",
    }
}

/// (#140) Back-compat shim for the capture env, which flips stretch ON. Cycles to the
/// first stretch mode (curved) if currently OFF, else back to drawbridge. Returns
/// whether stretch is now on. Prefer [`cycle_grid_mode`] for the live `T` key.
pub fn toggle_grid_stretch() -> bool {
    let next = u32::from(grid_mode() == 0);
    GRID_MODE.store(next, std::sync::atomic::Ordering::Relaxed);
    next != 0
}

/// (Bruce debug overlay) Live toggle for the per-ship ANGLE OVERLAY — the
/// pitch / roll / yaw readout drawn above every ship ([`crate::hud::push_ship_angle_overlay`])
/// so the orientation can be read NUMERICALLY while dialing in the per-column
/// lane orientation + the grid/ship camera unification. OFF by default (no
/// clutter in normal play); the bin's `O` key flips it via [`toggle_angle_overlay`].
/// A plain atomic flag like [`GRID_MODE`], so no signature threads through the
/// render path — `hud` reads it where it composes the overlay.
static ANGLE_OVERLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the per-ship angle overlay is currently shown (see [`ANGLE_OVERLAY`]).
pub fn angle_overlay_enabled() -> bool {
    ANGLE_OVERLAY.load(std::sync::atomic::Ordering::Relaxed)
}

/// Flip the per-ship angle overlay on/off (the `O` key); returns the new state.
pub fn toggle_angle_overlay() -> bool {
    let next = !angle_overlay_enabled();
    ANGLE_OVERLAY.store(next, std::sync::atomic::Ordering::Relaxed);
    next
}

/// (#196 Bruce) Live toggle for the CONTROLS popup — a centered semi-transparent
/// panel listing every player + debug key, rendered by
/// [`crate::hud::push_controls_popup`]. OFF by default (no clutter); the bin's
/// `F1` key flips it via [`toggle_controls_popup`]. Mirrors the [`ANGLE_OVERLAY`]
/// pattern — plain atomic flag, no signature threads through the render path.
static CONTROLS_POPUP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the controls popup is currently shown (see [`CONTROLS_POPUP`]).
pub fn controls_popup_enabled() -> bool {
    CONTROLS_POPUP.load(std::sync::atomic::Ordering::Relaxed)
}

/// Flip the controls popup on/off (the `F1` key); returns the new state.
pub fn toggle_controls_popup() -> bool {
    let next = !controls_popup_enabled();
    CONTROLS_POPUP.store(next, std::sync::atomic::Ordering::Relaxed);
    next
}

/// Force the controls popup on/off (the capture bin sets it from its env so the
/// headless shot can verify the popup geometry).
pub fn set_controls_popup(on: bool) {
    CONTROLS_POPUP.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// (#198 Bruce) Live toggle for the board's vertical anchor MODE — toggled by `M`.
/// false (default) = Mode A "snap-to-menu" (the #197 behavior: near edge parked
/// just above the bottom menu, board grows UP into the sky as -/= zooms in).
/// true = Mode B "centered" (board sits vertically centered in the window,
/// equal margin top + bottom; overlaps the menu strip at zoom max — fine for
/// a debug pose).
static ANCHOR_MODE_CENTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the board is in vertically-CENTERED mode (see [`ANCHOR_MODE_CENTERED`]).
/// `false` = the default #197 snap-to-menu mode.
pub fn anchor_mode_centered() -> bool {
    ANCHOR_MODE_CENTERED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Cycle the board's vertical anchor mode (the `M` key); returns the new state.
pub fn toggle_anchor_mode() -> bool {
    let next = !anchor_mode_centered();
    ANCHOR_MODE_CENTERED.store(next, std::sync::atomic::Ordering::Relaxed);
    next
}

/// Force the anchor mode (the capture bin sets it from its env so a headless
/// shot can verify either pose). `true` = centered, `false` = snap-to-menu.
pub fn set_anchor_mode_centered(on: bool) {
    ANCHOR_MODE_CENTERED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// (UNIFY, Bruce order) Live toggle for the UNIFIED camera: the grid AND the 3-D
/// hulls render through ONE real-perspective camera ([`crate::projector::unified_view_proj`])
/// instead of the legacy `1/z` fan + separate per-ship loft bake. ON by default
/// (#84): the legacy chase-cam loft bake renders every hull through its OWN
/// screen-space yaw ([`crate::hud::loft_facing_ground_yaw`]), which sits the hull
/// square-to-WINDOW (vertical for FACE N at every column) instead of square-to-GRID.
/// The unified pass orients each hull's heading as a world-space ray + projects it
/// through the same camera the grid lines use, so the hull's long axis converges
/// on the VP automatically — col0 leans up-right, col4 up-left, col2 stays
/// vertical — by construction. `U` flips back to the legacy bake for A/B.
static UNIFIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether the unified camera is active (see [`UNIFIED`]).
pub fn unified_enabled() -> bool {
    UNIFIED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Flip the unified camera on/off (the `U` key); returns the new state.
pub fn toggle_unified() -> bool {
    let next = !unified_enabled();
    UNIFIED.store(next, std::sync::atomic::Ordering::Relaxed);
    next
}

/// Force the unified camera on/off (the capture bin sets it from its env so the
/// headless shot matches the live `U` toggle).
pub fn set_unified(on: bool) {
    UNIFIED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// (#190 Bruce) Minimum allowed ship scale for the live `[` adjuster. Clamp keeps
/// the hull from going silly tiny. Named so the band widens via a one-liner.
pub const UNIFIED_SHIP_SCALE_MIN: f32 = 0.05;
/// (#190) Maximum allowed ship scale for the live `]` adjuster. Named so the band
/// widens via a one-liner if Bruce wants larger hulls later.
pub const UNIFIED_SHIP_SCALE_MAX: f32 = 0.15;
/// (#190) Per-press step size for the `[` / `]` ship-scale adjuster.
pub const UNIFIED_SHIP_SCALE_STEP: f32 = 0.01;
/// (#190) Boot value — Bruce's #188 pick (0.10). Within
/// `[UNIFIED_SHIP_SCALE_MIN, UNIFIED_SHIP_SCALE_MAX]`.
pub const BOOT_SHIP_SCALE: f32 = 0.10;

/// (#190) Live ship-scale stored as `scale * 1000` rounded to u32 so we can use a
/// stdlib atomic (no `AtomicF32`). Resolution = 0.001, plenty for a 0.01-step
/// adjuster. Read by the loft render loop via [`unified_ship_scale`], adjusted
/// by [`adjust_ship_scale`] (the `[` and `]` keys).
static SHIP_SCALE_MILLI: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new((BOOT_SHIP_SCALE * 1000.0) as u32);

/// (#190) The live ship-scale value. Reads the atomic + converts back to f32.
/// Safe to call from any thread / render hot path.
pub fn unified_ship_scale() -> f32 {
    SHIP_SCALE_MILLI.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0
}

/// (#190) Adjust the live ship scale by `delta` (positive grows, negative shrinks),
/// clamping into `[UNIFIED_SHIP_SCALE_MIN, UNIFIED_SHIP_SCALE_MAX]`. Returns the
/// new value. Bound to `[` (delta=-STEP) and `]` (delta=+STEP) in the bin.
pub fn adjust_ship_scale(delta: f32) -> f32 {
    let next = (unified_ship_scale() + delta).clamp(UNIFIED_SHIP_SCALE_MIN, UNIFIED_SHIP_SCALE_MAX);
    SHIP_SCALE_MILLI.store(
        (next * 1000.0).round() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
    next
}

/// (#192 Bruce) Minimum allowed unified-camera orbit distance for the live `-`
/// adjuster. At 3.5 the board nearly fills the frame (no menu clearance) — the
/// pre-#191 framing. Floor named so the band widens via a one-liner.
pub const UNIFIED_CAM_DIST_MIN: f32 = 3.5;
/// (#192) Maximum allowed unified-camera orbit distance for the live `=`
/// adjuster. At 7.0 the board is small enough to leave wide margins above + below.
pub const UNIFIED_CAM_DIST_MAX: f32 = 7.0;
/// (#192) Per-press step size for the `-` / `=` board-size adjuster. 0.25 gives
/// Bruce ~14 stops across the [3.5, 7.0] band — coarse enough to feel each press,
/// fine enough to dial the exact framing.
pub const UNIFIED_CAM_DIST_STEP: f32 = 0.25;
/// (#192/#193 Bruce verify) Boot value — bumped 5.0 → 5.5 after Bruce verified
/// the shrink-only capture at 5.5 was a cleaner default than #191's 5.0 (more
/// margin between near row + bottom menu, board sits in a clearer central band).
/// Within `[UNIFIED_CAM_DIST_MIN, UNIFIED_CAM_DIST_MAX]`. The `-` / `=` keys still
/// dial freely from this seat.
pub const BOOT_UNIFIED_CAM_DIST: f32 = 5.5;

/// (#192) Live unified-camera orbit distance stored as `dist * 1000` rounded to
/// u32 so we can use a stdlib atomic (no `AtomicF32`). Resolution = 0.001, plenty
/// for a 0.25-step adjuster. Read by [`projector::unified_eye`] via
/// [`unified_cam_dist`], adjusted by [`adjust_cam_dist`] (the `-` and `=` keys).
static CAM_DIST_MILLI: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new((BOOT_UNIFIED_CAM_DIST * 1000.0) as u32);

/// (#192) The live unified-camera orbit distance. Reads the atomic + converts
/// back to f32. Safe to call from any thread / render hot path.
pub fn unified_cam_dist() -> f32 {
    CAM_DIST_MILLI.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0
}

/// (#192) Adjust the live unified-camera distance by `delta` (positive pushes
/// the camera further → board shrinks; negative pulls it closer → board grows),
/// clamping into `[UNIFIED_CAM_DIST_MIN, UNIFIED_CAM_DIST_MAX]`. Returns the new
/// value. Bound to `-` (delta=+STEP, zoom OUT) and `=` (delta=-STEP, zoom IN)
/// in the bin — note the sign convention: `-` shrinks the board (Bruce reads
/// "zoom out" as the camera retreating, the board getting smaller).
pub fn adjust_cam_dist(delta: f32) -> f32 {
    let next = (unified_cam_dist() + delta).clamp(UNIFIED_CAM_DIST_MIN, UNIFIED_CAM_DIST_MAX);
    CAM_DIST_MILLI.store(
        (next * 1000.0).round() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
    next
}

/// (#195 Bruce) Minimum allowed grid cell-size multiplier for the live `K`
/// adjuster. 0.5 = half-size cells (tight grid). Floor named so the band widens
/// via a one-liner.
pub const UNIFIED_GRID_CELL_SCALE_MIN: f32 = 0.5;
/// (#195) Maximum allowed grid cell-size multiplier for the live `L` adjuster.
/// 2.0 = double-size cells (wide grid).
pub const UNIFIED_GRID_CELL_SCALE_MAX: f32 = 2.0;
/// (#195) Per-press step size for the `K` / `L` cell-size adjuster. 0.1 gives
/// ~15 stops across [0.5, 2.0] — coarse enough to feel each press.
pub const UNIFIED_GRID_CELL_SCALE_STEP: f32 = 0.1;
/// (#195) Boot value — 1.0 = the existing world cell spacing (1 world unit
/// per cell), byte-equivalent default.
pub const BOOT_UNIFIED_GRID_CELL_SCALE: f32 = 1.0;

/// (#195) Live grid cell-size multiplier stored as `scale * 1000` rounded to
/// u32 so we can use a stdlib atomic. Read by [`projector::cell_world_center`]
/// AND [`projector::cell_world_corners`] (BOTH must read the same multiplier so
/// the cell-center == grid-cell-center invariant holds — #188 regression guard).
static GRID_CELL_SCALE_MILLI: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new((BOOT_UNIFIED_GRID_CELL_SCALE * 1000.0) as u32);

/// (#195) The live grid cell-size multiplier. Reads the atomic + converts back
/// to f32. Safe to call from any thread / render hot path.
pub fn unified_grid_cell_scale() -> f32 {
    GRID_CELL_SCALE_MILLI.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0
}

/// (#195) Adjust the live grid cell-size multiplier by `delta` (positive grows
/// the cells, negative shrinks them), clamping into
/// `[UNIFIED_GRID_CELL_SCALE_MIN, UNIFIED_GRID_CELL_SCALE_MAX]`. Returns the new
/// value. Bound to `K` (delta=-STEP) and `L` (delta=+STEP) in the bin.
pub fn adjust_grid_cell_scale(delta: f32) -> f32 {
    let next = (unified_grid_cell_scale() + delta)
        .clamp(UNIFIED_GRID_CELL_SCALE_MIN, UNIFIED_GRID_CELL_SCALE_MAX);
    GRID_CELL_SCALE_MILLI.store(
        (next * 1000.0).round() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
    next
}

/// (UNIFY) Vertical lift (world units) of the hull above the ground plane so it
/// sits ON the grid rather than half-buried at its mesh origin. (#188 Bruce: enemies
/// "float above their cells" — diagnosis: _03.glb Y-bbox is [-1.48, +1.32], so at
/// scale 0.12 the hull's keel sits at world Y = -0.178 (BELOW the ground plane the
/// grid lines are drawn on), with the visible mass straddling above. Lifting by
/// +0.18 seats the keel exactly on the grid plane so the hull's BOTTOM touches the
/// cell line Bruce reads as "the ship's cell").
const UNIFIED_SHIP_LIFT: f32 = 0.18;

/// (UNIFY) THE single place that builds the live scene [`crate::projector::ProjectorConfig`]
/// from the global look state — the grid pitch arc ([`grid_pitch_t`]), the grid
/// MODE ([`grid_mode`]), and the unified toggle ([`unified_enabled`]) — so the bin,
/// the capture, and the loft render loop never drift. Unified takes precedence over
/// the stretch/pitch fan modes (it IS the camera, not a fan tweak).
pub fn scene_projector_cfg(frame_w: f32, frame_h: f32) -> crate::projector::ProjectorConfig {
    let base = crate::projector::ProjectorConfig::for_scene(frame_w, frame_h);
    let t = grid_pitch_t();
    if unified_enabled() {
        return base.with_unified(t);
    }
    match grid_mode() {
        1 => base.with_stretch(t),
        2 => base.with_stretch_straight(t),
        3 => base.with_stretch_continuous(t),
        _ => base.with_pitch(t),
    }
}

/// (#140 Bruce ship-tilt) The LIVE loft-camera pitch (degrees) the player + enemy
/// 3-D hulls render at, so the hulls TILT to stay PARALLEL to the grid plane as the
/// `G` pitch arc raises. At grid-pitch step 0 this is exactly
/// [`crate::loft_gpu::CAMERA_PITCH_DEG`] (the chase-cam look — so the default frame
/// is byte-identical); as the arc steps toward top-down it lerps up toward
/// [`LOFT_PITCH_TOPDOWN_DEG`] (near-overhead), where the loft camera looks down the
/// deck and the hull reads as a top-down silhouette. ONE global off `grid_pitch_t()`
/// so the player + every enemy + the grid all pitch together. Independent of the
/// STRETCH toggle — the hulls tilt in BOTH modes (the grid plane raises either way).
pub fn loft_pitch_deg() -> f32 {
    let base = crate::loft_gpu::CAMERA_PITCH_DEG;
    base + (LOFT_PITCH_TOPDOWN_DEG - base) * grid_pitch_t()
}

/// (#140) The loft-camera pitch at full grid-pitch (`t = 1`): near-overhead so the
/// hull reads top-down. Capped below 90° — a true straight-down ortho view is a
/// degenerate edge-on read of a flat hull (the deck collapses to a line); ~82°
/// gives a clear top-down silhouette while keeping a sliver of hull thickness.
pub const LOFT_PITCH_TOPDOWN_DEG: f32 = 82.0;

/// (#76 scene-res) The scene-resolution presets `;` / `'` step through, all 16:9.
/// 480×270 is the MINIMUM + the pixel-identity baseline (Bruce: 480 is the floor —
/// the old 320×180 was dropped as too chunky); 640×360 is the BOOT default (#135);
/// 960×540 is the crispest. `'` (next) from 480 goes to 640→960→wraps; `;` (prev)
/// from 480 wraps to 960 (the max). [`next_scene_res`] / [`prev_scene_res`] step
/// this list, snapping an off-list current size to the 480×270 floor first.
pub const SCENE_RES_PRESETS: [(u32, u32); 3] = [(480, 270), (640, 360), (960, 540)];

/// The next scene-res preset after `(w, h)` (wraps). An off-list `(w, h)` returns
/// the 480×270 floor preset ([`SCENE_RES_PRESETS`][0]). See [`SCENE_RES_PRESETS`].
pub fn next_scene_res(w: u32, h: u32) -> (u32, u32) {
    match SCENE_RES_PRESETS.iter().position(|&p| p == (w, h)) {
        Some(i) => SCENE_RES_PRESETS[(i + 1) % SCENE_RES_PRESETS.len()],
        None => (VIRTUAL_W, VIRTUAL_H),
    }
}

/// The previous scene-res preset before `(w, h)` (wraps). An off-list `(w, h)`
/// returns the 480×270 default. See [`SCENE_RES_PRESETS`].
pub fn prev_scene_res(w: u32, h: u32) -> (u32, u32) {
    match SCENE_RES_PRESETS.iter().position(|&p| p == (w, h)) {
        Some(i) => SCENE_RES_PRESETS[(i + SCENE_RES_PRESETS.len() - 1) % SCENE_RES_PRESETS.len()],
        None => (VIRTUAL_W, VIRTUAL_H),
    }
}

/// Maximum sprite instances in a frame. (#196 Bruce) Bumped 4096 → 8192 to
/// accommodate the F1 controls-popup overlay: 20 lines × ~22 chars × ~10 set
/// 5x7-font cells/char ≈ 4400 extra sprite instances when the popup is open,
/// stacked on top of the worst-case scene (~1300 = parallax stars + ships +
/// HUD glyphs). Bumping this only costs one VRAM allocation; the buffer is
/// reused frame-to-frame.
const MAX_SPRITES: u64 = 8192;

/// Maximum polygon instances in a frame. A 9-cell lane plate plus 9 ships ×
/// 2 face polygons = ~27. 256 is plenty.
const MAX_POLYGONS: u64 = 256;

/// Maximum textured-ship draws per frame. Each consumes one tiny uniform
/// buffer + one cached bind group. 16 covers the maximum 9-cell lane with
/// headroom.
const MAX_TEXTURED_SHIPS: usize = 16;

const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Deep-space ink — matches the analysis HTML `--ink` token (`#080c14`).
/// Values are pre-converted to linear space because `OFFSCREEN_FORMAT` is
/// sRGB and `wgpu::Color` is interpreted linearly when the target is sRGB.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.001_214,
    g: 0.002_428,
    b: 0.006_995,
    a: 1.0,
};
const LETTERBOX: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVertex {
    pos: [f32; 2],
}

const QUAD_VERTS: [QuadVertex; 6] = [
    QuadVertex { pos: [-1.0, -1.0] },
    QuadVertex { pos: [1.0, -1.0] },
    QuadVertex { pos: [1.0, 1.0] },
    QuadVertex { pos: [-1.0, -1.0] },
    QuadVertex { pos: [1.0, 1.0] },
    QuadVertex { pos: [-1.0, 1.0] },
];

/// One drawable rectangle in virtual-pixel space. Position is the rectangle's
/// CENTER; `half_size` is half-width / half-height. `color` multiplies the
/// sampled atlas texel (use `1.0,1.0,1.0,1.0` for "no tint" or sample the
/// solid-white atlas cell to render a flat-color quad). UVs select the atlas
/// cell.
///
/// `rotation_rad` rotates the quad around its center. For axis-aligned HUD
/// elements pass `0.0`; for lane-slope-aligned sprites use
/// `perspective::cell_to_screen` rotation. The rotation pivot is the
/// instance's `pos`, not an external pivot — composed sprites whose pivot is
/// elsewhere (e.g. ships rotated about their base) precompute the rotated
/// vertex positions on the CPU and pass `rotation_rad: 0.0`.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SpriteInstance {
    pub pos: [f32; 2],
    pub half_size: [f32; 2],
    pub color: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub rotation_rad: f32,
    // GPU std140/430 alignment padding; pub for the `#[repr(C)]` Pod layout and
    // named `_pad` by convention (never read), so opt out of pub_underscore_fields.
    #[allow(clippy::pub_underscore_fields)]
    pub _pad: [f32; 3],
}

impl SpriteInstance {
    /// Convenience for the common axis-aligned case.
    pub const fn axis_aligned(
        pos: [f32; 2],
        half_size: [f32; 2],
        color: [f32; 4],
        uv: ([f32; 2], [f32; 2]),
    ) -> Self {
        Self {
            pos,
            half_size,
            color,
            uv_min: uv.0,
            uv_max: uv.1,
            rotation_rad: 0.0,
            _pad: [0.0; 3],
        }
    }
}

/// A quad defined by four explicit virtual-pixel corner positions in CCW
/// (with screen y-down: top-left, top-right, bottom-right, bottom-left).
/// Used for shapes the rotation-around-center `SpriteInstance` cannot
/// represent without pixel staircase, primarily the lane plate
/// parallelograms and ship-face polygons under forced perspective.
///
/// The fragment shader still samples the atlas times the color tint, so a
/// flat-color polygon uses `atlas::SOLID_WHITE`'s UVs and the desired
/// color, while a textured polygon can sample any atlas cell.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PolygonInstance {
    /// Top-left corner.
    pub p0: [f32; 2],
    /// Top-right corner.
    pub p1: [f32; 2],
    /// Bottom-right corner.
    pub p2: [f32; 2],
    /// Bottom-left corner.
    pub p3: [f32; 2],
    pub color: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
}

impl PolygonInstance {
    /// Build a polygon from a `[Point2-shaped; 4]` corner array using the
    /// `SOLID_WHITE` atlas cell so the `color` field is the visible tint.
    /// Caller supplies the `SOLID_WHITE` uv rect to keep this module
    /// decoupled from `crate::atlas`.
    pub const fn flat(
        corners: [[f32; 2]; 4],
        color: [f32; 4],
        solid_white_uv: ([f32; 2], [f32; 2]),
    ) -> Self {
        Self {
            p0: corners[0],
            p1: corners[1],
            p2: corners[2],
            p3: corners[3],
            color,
            uv_min: solid_white_uv.0,
            uv_max: solid_white_uv.1,
        }
    }
}

/// Inline-storage slug identifying a loaded ship sprite. `Copy`, no heap
/// allocation — `DrawCommand` is `Copy` and the renderer batches commands
/// frame-to-frame, so we can't hold a `String`. Truncates silently at 31
/// bytes (every legal slug is < 32 bytes — see `SPRITE_SPEC.md`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpriteSlug {
    bytes: [u8; 32],
    len: u8,
}

impl SpriteSlug {
    pub fn new(s: &str) -> Self {
        let src = s.as_bytes();
        let n = src.len().min(32);
        let mut bytes = [0u8; 32];
        bytes[..n].copy_from_slice(&src[..n]);
        Self {
            bytes,
            len: n as u8,
        }
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

/// Per-ship textured-quad draw. The bbox quad (`p0..p3`) is identical to
/// what the procedural silhouette would produce; the fragment shader
/// samples both `side` and `top` textures (looked up via the slugs at
/// draw time) and blends them by `blend_t = sin(view_angle_rad)`.
///
/// Emitted by `hud::push_ship` only when both side and top PNGs are
/// registered for the ship's `class_stance`. Otherwise the procedural
/// polygon-set is emitted instead.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct TexturedShipInstance {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    pub p2: [f32; 2],
    pub p3: [f32; 2],
    pub blend_t: f32,
    pub side: SpriteSlug,
    pub top: SpriteSlug,
}

/// A draw command in back-to-front order. `Gfx::render` batches consecutive
/// same-variant runs into single draw calls (except `TexturedShip`, which
/// is always drawn one-at-a-time since each ship has its own texture pair).
#[derive(Copy, Clone, Debug)]
pub enum DrawCommand {
    Sprite(SpriteInstance),
    Polygon(PolygonInstance),
    TexturedShip(TexturedShipInstance),
    /// Blit the live loft-rendered ship texture onto a lane quad (four
    /// virtual-pixel corners: top-left, top-right, bot-right, bot-left).
    /// Emitted by `hud::push_ship` for a ship that has a 3D asset, in place of
    /// its 2D silhouette. `gfx` renders the ship's pose into the loft target
    /// (pre-pass) and samples it here. The dest-rect is computed in hud (one
    /// source of the lane geometry); the pose/mesh live in `gfx`.
    LoftShip(LoftShipInstance),
}

/// Lane destination quad for a loft-rendered ship. Corners in virtual-pixel
/// space, same y-down convention as [`PolygonInstance`]. Also carries which
/// ship this is (`ship_id`, so the renderer looks up its animated pose) and
/// which 3D mesh to render it with (`kind`) — the pre-pass renders the right
/// mesh at the right pose into the shared loft target before blitting it here.
#[derive(Copy, Clone, Debug)]
pub struct LoftShipInstance {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    pub p2: [f32; 2],
    pub p3: [f32; 2],
    /// Interned ship id (e.g. `"player"`, `"enemy-2"`) — keys the per-ship
    /// [`crate::loft_gpu::ShipPose`] the renderer animates.
    pub ship_id: SpriteSlug,
    /// Which uploaded loft mesh to render this ship with.
    pub kind: crate::sprites::LoftMeshKind,
    /// (#70) The ship's CELL-centre screen point — the anchor the chase-cam
    /// nose-aim measures the angle to the vanishing point FROM. NOT the draw
    /// quad's centre: the player's hero quad is dragged to the screen bottom
    /// (clamped above the HUD band + upscaled), far below the cell, which over-
    /// steepened the aim. Aiming from the true cell centre keeps the lane-aim
    /// small + correct. (Set from `grid_cell_quad(pos).center`.)
    pub aim_at: [f32; 2],
    /// (#70) The ship's TACTICAL FACING as a ground-plane yaw offset (degrees),
    /// composed on top of the up-lane stern-on base + the lane-aim convergence.
    /// This is what makes the hull SHOW its orientation (the core hook): toward
    /// the VP (N/up-lane) = 0, away (S) = 180, the two broadside flanks (E/W) =
    /// ±90 — all FLAT on the grid. Set by `hud` from `ship.facing`; the renderer
    /// adds it to the base yaw so all 4 cardinals render as distinct flat poses.
    pub facing_yaw_deg: f32,
    /// (UNIFY) The ship's INTEGER grid cell `[col, row]` — the logical / snapped
    /// position. Kept alongside [`cell_frac`] as the rest-state baseline (#188
    /// alignment guard reads this); when the ship is not mid-slide both fields
    /// agree exactly. The legacy per-cell blit path ignores both and uses the
    /// screen quad above.
    pub cell: [u32; 2],
    /// (#201 fix A) FRACTIONAL grid cell `[col, row]` — the unified ship pass
    /// projects through [`crate::projector::cell_world_center_frac`] using this,
    /// so a moving ship SLIDES cell-to-cell instead of snapping the loft hull on
    /// the integer cell while the rest of the HUD tweened. Set by `hud` to the
    /// `Tween2d`-eased fractional Pos when a move/turn is in flight; otherwise
    /// equal to `cell` cast to `f32` so the steady-state frame is byte-identical.
    pub cell_frac: [f32; 2],
    /// (UNIFY) The hull's world HEADING yaw (radians) about `+Y` for the unified
    /// model matrix: local prow `+X` rotates to the facing direction (N = up-lane
    /// `+Z`, etc.). Set by `hud` from `ship.facing`. Distinct from `facing_yaw_deg`
    /// (the legacy ortho-loft ground yaw), which the unified path does not use.
    pub unified_yaw_rad: f32,
}

impl From<SpriteInstance> for DrawCommand {
    fn from(s: SpriteInstance) -> Self {
        Self::Sprite(s)
    }
}

impl From<PolygonInstance> for DrawCommand {
    fn from(p: PolygonInstance) -> Self {
        Self::Polygon(p)
    }
}

impl From<TexturedShipInstance> for DrawCommand {
    fn from(t: TexturedShipInstance) -> Self {
        Self::TexturedShip(t)
    }
}

impl From<LoftShipInstance> for DrawCommand {
    fn from(l: LoftShipInstance) -> Self {
        Self::LoftShip(l)
    }
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniform {
    /// 2.0 / `VIRTUAL_W`, 2.0 / `VIRTUAL_H`. Multiplying a virtual-pixel position
    /// by this gives NDC half-extent; subtracting 1.0 maps to [-1, 1]. Y is
    /// flipped in the shader so we feed y-down pixel coords directly.
    px_to_ndc: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlitUniform {
    ndc_min: [f32; 2],
    ndc_max: [f32; 2],
}

/// Per-textured-ship blend factor. Padded to 16 bytes (wgpu uniform alignment).
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendUniform {
    blend_t: f32,
    _pad: [f32; 3],
}

// Each of these structs is bound as a uniform buffer whose bind group entry
// uses `min_binding_size: None`, so wgpu defers the size check to draw time
// against the matching WGSL struct's byte layout. If the Rust size and the
// WGSL size disagree, the draw fails with a generic "Encoder is invalid"
// (see the BlendUniform vec3-padding bug fixed under task #92). These
// `const _` assertions make the Rust side a hard compile-time invariant.
//
// IMPORTANT: pad WGSL structs with scalars or `vec2<f32>` (4- / 8-byte
// alignment), never `vec3<f32>` — a vec3 forces 16-byte member alignment and
// silently inflates the WGSL struct past these sizes. When adding a uniform,
// add its assertion here AND keep its WGSL twin's byte layout in lockstep.
const _: () = assert!(std::mem::size_of::<ViewUniform>() == 16);
const _: () = assert!(std::mem::size_of::<BlitUniform>() == 16);
const _: () = assert!(std::mem::size_of::<BlendUniform>() == 16);

const SPRITE_SHADER: &str = r"
struct ViewUniform {
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv:    vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) v_pos:        vec2<f32>,
    @location(1) i_pos:        vec2<f32>,
    @location(2) i_half:       vec2<f32>,
    @location(3) i_color:      vec4<f32>,
    @location(4) i_uv_min:     vec2<f32>,
    @location(5) i_uv_max:     vec2<f32>,
    @location(6) i_rotation:   f32,
) -> VsOut {
    // Rotate the local quad vertex around the instance center.
    let cos_r = cos(i_rotation);
    let sin_r = sin(i_rotation);
    let local = v_pos * i_half;
    let rotated = vec2<f32>(
        local.x * cos_r - local.y * sin_r,
        local.x * sin_r + local.y * cos_r,
    );
    // Translate to virtual-pixel position, then map to NDC. Y is flipped so
    // virtual-pixel (0, 0) is the top-left corner of the offscreen, matching
    // the screen-space convention of perspective::cell_to_screen.
    let pixel = i_pos + rotated;
    let ndc_x = pixel.x * view.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * view.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.color = i_color;
    // Map v_pos in [-1, 1] to uv in [uv_min, uv_max]. Y flip so the top of
    // the atlas cell shows at the top of the quad in screen space (the quad's
    // top in local frame is +y, which after Y flip lands at screen-top).
    let t = (v_pos + vec2<f32>(1.0, 1.0)) * 0.5;
    o.uv = vec2<f32>(
        mix(i_uv_min.x, i_uv_max.x, t.x),
        mix(i_uv_max.y, i_uv_min.y, t.y)
    );
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(atlas, atlas_samp, in.uv);
    return s * in.color;
}
";

// Polygon shader. Each instance has four explicit corner positions; we expand
// to 6 vertices (two triangles, indices 0-1-2 and 0-2-3) using
// vertex_index % 6. The UV is barycentrically blended across the polygon
// from uv_min (top-left) to uv_max (bottom-right) in the polygon's own
// corner frame — so a textured polygon samples its full atlas cell across
// its quad, with the same y-flip convention as SpriteInstance.
const POLYGON_SHADER: &str = r"
struct ViewUniform {
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv:    vec2<f32>,
};

@vertex
fn vs_poly(
    @builtin(vertex_index) v_idx: u32,
    @location(0) i_p0:     vec2<f32>,
    @location(1) i_p1:     vec2<f32>,
    @location(2) i_p2:     vec2<f32>,
    @location(3) i_p3:     vec2<f32>,
    @location(4) i_color:  vec4<f32>,
    @location(5) i_uv_min: vec2<f32>,
    @location(6) i_uv_max: vec2<f32>,
) -> VsOut {
    // Two triangles: 0-1-2 and 0-2-3.
    var corner_idx = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let c = corner_idx[v_idx];
    var pixel: vec2<f32>;
    var uv:    vec2<f32>;
    // p0 = top-left   -> uv (uv_min.x, uv_max.y)  (Y-flip mirrors SpriteInstance)
    // p1 = top-right  -> uv (uv_max.x, uv_max.y)
    // p2 = bot-right  -> uv (uv_max.x, uv_min.y)
    // p3 = bot-left   -> uv (uv_min.x, uv_min.y)
    if (c == 0u) { pixel = i_p0; uv = vec2<f32>(i_uv_min.x, i_uv_max.y); }
    else if (c == 1u) { pixel = i_p1; uv = vec2<f32>(i_uv_max.x, i_uv_max.y); }
    else if (c == 2u) { pixel = i_p2; uv = vec2<f32>(i_uv_max.x, i_uv_min.y); }
    else { pixel = i_p3; uv = vec2<f32>(i_uv_min.x, i_uv_min.y); }

    let ndc_x = pixel.x * view.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * view.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.color = i_color;
    o.uv = uv;
    return o;
}

@fragment
fn fs_poly(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(atlas, atlas_samp, in.uv);
    return s * in.color;
}
";

// Textured-ship shader. Same vertex layout as POLYGON_SHADER (four explicit
// corners expanded by vertex_index), but the fragment samples two textures
// (side, top) and blends by `blend_t` carried in a uniform — one uniform
// per ship since each batch is a single instance with its own texture pair.
const TEXTURED_SHIP_SHADER: &str = r"
struct ViewUniform {
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
// NOTE: the pad must be three scalar f32s, NOT a vec3<f32>. In WGSL uniform
// layout a vec3<f32> has 16-byte alignment, which would push it to offset 16
// and make this struct 32 bytes — but the matching Rust `BlendUniform`
// (#[repr(C)] { f32, [f32; 3] }) and its uniform buffer are only 16 bytes.
// The mismatch trips wgpu's late-min-binding-size check at draw time
// (bound_size 16 < shader_expect_size 32), invalidating the encoder. Three
// scalars keep the WGSL struct at 16 bytes to match.
struct BlendUniform {
    blend_t: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};
@group(0) @binding(0) var<uniform> view: ViewUniform;
@group(1) @binding(0) var<uniform> ship: BlendUniform;
@group(1) @binding(1) var side_tex: texture_2d<f32>;
@group(1) @binding(2) var top_tex:  texture_2d<f32>;
@group(1) @binding(3) var ship_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_ship(
    @builtin(vertex_index) v_idx: u32,
    @location(0) i_p0: vec2<f32>,
    @location(1) i_p1: vec2<f32>,
    @location(2) i_p2: vec2<f32>,
    @location(3) i_p3: vec2<f32>,
) -> VsOut {
    var corner_idx = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let c = corner_idx[v_idx];
    var pixel: vec2<f32>;
    var uv:    vec2<f32>;
    // p0 = top-left  -> uv (0, 0)
    // p1 = top-right -> uv (1, 0)
    // p2 = bot-right -> uv (1, 1)
    // p3 = bot-left  -> uv (0, 1)
    if (c == 0u) { pixel = i_p0; uv = vec2<f32>(0.0, 0.0); }
    else if (c == 1u) { pixel = i_p1; uv = vec2<f32>(1.0, 0.0); }
    else if (c == 2u) { pixel = i_p2; uv = vec2<f32>(1.0, 1.0); }
    else { pixel = i_p3; uv = vec2<f32>(0.0, 1.0); }
    let ndc_x = pixel.x * view.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * view.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_ship(in: VsOut) -> @location(0) vec4<f32> {
    let side_px = textureSample(side_tex, ship_samp, in.uv);
    let top_px  = textureSample(top_tex,  ship_samp, in.uv);
    // blend_t = sin(view_angle): 0 at pure side-on, 1 at pure top-down.
    //
    // A naive `mix(side, top, blend_t)` cross-DISSOLVES the two orthographic
    // sprites, so mid-angle reads as a double-exposure rather than a hull
    // tilting. The bounding box already foreshortens correctly (height·cosθ +
    // depth·sinθ) — the fix is to stop dissolving the *fill*:
    //
    //   B (faster curve): collapse the crossfade to a narrow smoothstep band
    //     so we snap through the muddy 50/50 instead of lingering there.
    //   A (dominant sprite): the band is centered on the θ=45° swap point
    //     (sin 45° = 0.7071), so for θ<45° we show ~pure side and for θ≥45°
    //     ~pure top. Outside the band there is zero double-exposure; the
    //     bbox foreshorten supplies the tilt illusion.
    //
    // HALF_BAND is the crossfade half-width in blend_t units. Small enough to
    // read as a crisp flip, wide enough (a few frames) to avoid a 1-frame
    // pop. Bruce iterates this and SWAP if needed.
    let SWAP: f32 = 0.70710677;   // sin(45°)
    let HALF_BAND: f32 = 0.06;
    let t = smoothstep(SWAP - HALF_BAND, SWAP + HALF_BAND, ship.blend_t);
    return mix(side_px, top_px, t);
}
";

const BLIT_SHADER: &str = r"
struct BlitUniform {
    ndc_min: vec2<f32>,
    ndc_max: vec2<f32>,
};
@group(0) @binding(0) var<uniform> blit: BlitUniform;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) idx: u32) -> VsOut {
    var verts = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let v = verts[idx];
    let x = mix(blit.ndc_min.x, blit.ndc_max.x, v.x);
    let y = mix(blit.ndc_min.y, blit.ndc_max.y, v.y);
    var o: VsOut;
    o.clip = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>(v.x, 1.0 - v.y);
    return o;
}

@fragment
fn fs_blit(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src_tex, src_samp, in.uv);
}
";

/// wgpu state owner. Builds the surface, virtual-res offscreen target, the
/// procedural atlas texture, and all render pipelines on `new`. Renders one
/// frame on `render` given a pre-built draw command list.
// `Debug` is intentionally not derived: `Gfx` is the wgpu god-object and
// aggregates ~8 render-internal pipeline structs (Layer, *Pipeline, LoftMesh,
// ...) that would each need their own derive in turn. A `Debug` listing eight
// opaque GPU pipelines carries no diagnostic value, so this struct opts out
// rather than cascade the derive through the whole render backend.
#[allow(missing_debug_implementations)]
pub struct Gfx {
    /// `None` in HEADLESS mode ([`Gfx::new_headless`], used by the offscreen PNG
    /// capture tool) — there is no window/swapchain to present to; only the
    /// offscreen target + readback are used. `Some` for the normal windowed path.
    surface: Option<wgpu::Surface<'static>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The offscreen render target's texture (kept, not just its view) so the
    /// headless [`Gfx::capture_png`] can `copy_texture_to_buffer` it for readback.
    offscreen_tex: wgpu::Texture,
    offscreen_view: wgpu::TextureView,
    sprites: SpritePipeline,
    polygons: PolygonPipeline,
    textured_ships: TexturedShipPipeline,
    blit: BlitPipeline,
    /// Loaded ship sprite textures keyed by `<class>_<stance>_<view>` slug.
    /// `gfx.rs` uploads each PNG to its own GPU texture; the textured-ship
    /// render path looks up handles here and supplies them as side/top
    /// bindings.
    ship_sprites: std::collections::HashMap<String, ShipSpriteEntry>,
    /// Cache of per-slot bind groups by (`slot_idx`, `side_slug`, `top_slug`).
    /// The bind group includes the slot's blend uniform (slot-specific)
    /// AND the texture pair. Cleared on `try_load_ship_sprites` since
    /// loaded textures may have changed.
    ship_bg_cache: std::collections::HashMap<(u32, SpriteSlug, SpriteSlug), wgpu::BindGroup>,

    /// 3D loft render pipeline (depth + ortho-¾ + posterize). Renders a hull
    /// into its own 320×200 offscreen target; `gfx` then blits that texture
    /// into the lane via [`LoftShipBlit`]. The only depth-using pipeline in
    /// the engine — its depth texture lives inside `LoftGpu`.
    loft: crate::loft_gpu::LoftGpu,
    /// Blits `loft.output_view()` onto a lane-positioned quad in the offscreen
    /// scene. Samples an arbitrary texture view (the loft output), unlike the
    /// `TexturedShip` path which samples the procedural atlas.
    loft_blit: LoftShipBlit,
    /// Uploaded loft meshes, one per [`crate::sprites::LoftMeshKind`]. Every
    /// ship of a given kind shares the one vertex buffer (e.g. all four enemy
    /// placeholders share the vendored CAD hull). Uploaded once at startup via
    /// [`Gfx::install_player_dagger`] / [`Gfx::install_enemy_cad`].
    loft_meshes: std::collections::HashMap<crate::sprites::LoftMeshKind, LoftMesh>,
    /// Per-ship animated poses, keyed by `Ship::id`. The bin syncs these each
    /// frame from board orientation ([`Gfx::sync_loft_pose`]); the pre-pass
    /// renders each loft ship at its pose. Ships that leave the board are
    /// pruned by [`Gfx::retain_loft_poses`].
    loft_poses: std::collections::HashMap<String, crate::loft_gpu::ShipPose>,

    /// Parallax space background: the 20-layer painted-PNG queue loaded from
    /// `assets/backgrounds` (with a solid-ink fallback per slot). Drawn into the
    /// offscreen FIRST each frame so the scene composites on top of it.
    background: Option<crate::background::Background>,
}

/// One uploaded loft mesh + its vertex count, shared across every ship that
/// renders with this [`crate::sprites::LoftMeshKind`].
struct LoftMesh {
    vbuf: wgpu::Buffer,
    vcount: u32,
    /// Vertical centre of the hull bbox — the loft camera looks at this Y so the
    /// hull renders centred in its texture (see [`crate::loft_gpu::upload_hull`]).
    center_y: f32,
}

/// One uploaded ship sprite. `dimensions` is the source PNG size in
/// pixels so the renderer can compute the dest rect from the sprite's
/// intended bbox in the `SPRITE_SPEC` table.
struct ShipSpriteEntry {
    texture_view: wgpu::TextureView,
    #[allow(dead_code)]
    dimensions: (u32, u32),
}

struct SpritePipeline {
    pipeline: wgpu::RenderPipeline,
    quad_vbuf: wgpu::Buffer,
    instance_vbuf: wgpu::Buffer,
    view_ubo: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

struct PolygonPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_vbuf: wgpu::Buffer,
    // view uniform and atlas bind group are shared with SpritePipeline at
    // bind point 0 — same bgl layout, just bound from this pipeline's
    // perspective. The bind group is owned by SpritePipeline; we keep a
    // reference at construction time via shared underlying resources.
    bind_group: wgpu::BindGroup,
}

/// Per-ship texture-blend pipeline. One pipeline + a shared per-ship bind
/// group layout (uniform + side tex + top tex + sampler). Each ship's
/// draw uses its own bind group built from the loaded ship sprite
/// textures, cached on `Gfx::ship_bg_cache` keyed by the slug pair.
struct TexturedShipPipeline {
    pipeline: wgpu::RenderPipeline,
    instance_vbuf: wgpu::Buffer,
    /// view ubo bind group (group 0). Identical to sprites/polygons.
    view_bg: wgpu::BindGroup,
    /// Layout for per-ship bind group (group 1). Used to build cached
    /// (side, top) bind groups on demand.
    ship_bgl: wgpu::BindGroupLayout,
    /// Shared sampler used by every per-ship bind group.
    sampler: wgpu::Sampler,
    /// Pre-allocated per-frame uniform buffers — one entry per drawn ship.
    /// Sized for `MAX_TEXTURED_SHIPS` ships; each draw writes its slice
    /// before binding. Storing as one big buffer with dynamic offsets
    /// would be neater but the count is tiny so individual buffers are
    /// simpler.
    blend_ubos: Vec<wgpu::Buffer>,
}

struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    ubo: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// (#76 scene-res) Kept so [`BlitPipeline::rebind`] can rebuild `bind_group`
    /// against a NEW offscreen view after a scene-res cycle recreates the texture.
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Blits the loft pipeline's posterized output texture onto a lane-positioned
/// quad in the offscreen scene. Distinct from [`TexturedShipPipeline`]: that
/// samples the procedural atlas (two cells + blend); this samples ONE
/// arbitrary external texture view (the live loft output), alpha-blended over
/// the scene so the posterize pass's transparent cut-out composites cleanly.
///
/// The destination quad is four explicit virtual-pixel corners (same y-down
/// convention as [`PolygonInstance`]), written to a per-frame uniform; the
/// source UV spans the full output texture. One ship per draw (each has its own
/// freshly-rendered loft output), so this is not instanced.
struct LoftShipBlit {
    pipeline: wgpu::RenderPipeline,
    /// Per-draw quad-corner uniform (4 × vec2 + the view px→ndc scale).
    quad_ubo: wgpu::Buffer,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LoftQuadUniform {
    /// Four virtual-pixel corners: top-left, top-right, bot-right, bot-left.
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    /// `2/VIRTUAL_W`, `2/VIRTUAL_H` — same px→NDC map as the sprite/polygon view.
    px_to_ndc: [f32; 2],
    _pad: [f32; 2],
}

// 4 × vec2 (32) + vec2 (8) + vec2 pad (8) = 48 bytes. Size-pinned so a layout
// drift can't silently mismatch the WGSL struct (the late-min-binding-size
// invalid-encoder trap, made a compile error).
const _: () = assert!(std::mem::size_of::<LoftQuadUniform>() == 48);

// Loft-ship blit shader. Expands a per-draw 4-corner quad (vertex_index → the
// two triangles 0-1-2 / 0-2-3) into virtual-pixel space → NDC, samples the
// loft output texture across the quad, and returns it straight (already
// posterized + cut-out). Y-flip on both clip and UV matches the sprite path.
const LOFT_SHIP_SHADER: &str = r"
struct Quad {
    p0: vec2<f32>,
    p1: vec2<f32>,
    p2: vec2<f32>,
    p3: vec2<f32>,
    px_to_ndc: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> quad: Quad;
@group(0) @binding(1) var ship_tex: texture_2d<f32>;
@group(0) @binding(2) var ship_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_loft(@builtin(vertex_index) v_idx: u32) -> VsOut {
    var corner_idx = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u);
    let c = corner_idx[v_idx];
    var pixel: vec2<f32>;
    var uv: vec2<f32>;
    // p0 top-left (0,0), p1 top-right (1,0), p2 bot-right (1,1), p3 bot-left (0,1).
    if (c == 0u) { pixel = quad.p0; uv = vec2<f32>(0.0, 0.0); }
    else if (c == 1u) { pixel = quad.p1; uv = vec2<f32>(1.0, 0.0); }
    else if (c == 2u) { pixel = quad.p2; uv = vec2<f32>(1.0, 1.0); }
    else { pixel = quad.p3; uv = vec2<f32>(0.0, 1.0); }
    let ndc_x = pixel.x * quad.px_to_ndc.x - 1.0;
    let ndc_y = 1.0 - pixel.y * quad.px_to_ndc.y;
    var o: VsOut;
    o.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_loft(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(ship_tex, ship_samp, in.uv);
}
";

impl crate::sprites::SpriteRegistry for Gfx {
    fn has(
        &self,
        class: &str,
        stance: crate::sprites::SpriteStance,
        view: crate::sprites::SpriteView,
    ) -> bool {
        Self::has_ship_sprite(self, class, stance, view)
    }

    fn has_facing(&self, class: &str, index: usize) -> bool {
        Self::has_facing_sprite(self, class, index)
    }

    fn loft_kind(&self, ship_id: &str, is_player: bool) -> Option<crate::sprites::LoftMeshKind> {
        use crate::sprites::LoftMeshKind;
        if is_player {
            // Player → the lofted Aegis-class hull if installed (preferred), else
            // the generic tinted CAD hull, else 2D silhouette.
            if self.has_loft_mesh(LoftMeshKind::PlayerLoft) {
                return Some(LoftMeshKind::PlayerLoft);
            }
            return self
                .has_loft_mesh(LoftMeshKind::PlayerCad)
                .then_some(LoftMeshKind::PlayerCad);
        }
        // Enemies → the enemy-tinted GLB hull(s) if installed (preferred, Bruce's
        // ask), else the authored-colour CAD hull, else 2D silhouette.
        // (#187) When BOTH enemy meshes are installed, pick per-ship for fleet
        // variety: a deterministic id-byte fold → parity selects EnemyLoft vs
        // EnemyLoftB, so a given enemy ALWAYS draws the same hull (stable across
        // frames) but the fleet shows a mix. Only one installed → every enemy uses it.
        let has_a = self.has_loft_mesh(LoftMeshKind::EnemyLoft);
        let has_b = self.has_loft_mesh(LoftMeshKind::EnemyLoftB);
        match (has_a, has_b) {
            (true, true) => {
                let fold = ship_id
                    .bytes()
                    .fold(0u32, |acc, b| acc.wrapping_add(u32::from(b)));
                Some(if fold % 2 == 0 {
                    LoftMeshKind::EnemyLoft
                } else {
                    LoftMeshKind::EnemyLoftB
                })
            }
            (true, false) => Some(LoftMeshKind::EnemyLoft),
            (false, true) => Some(LoftMeshKind::EnemyLoftB),
            (false, false) => self
                .has_loft_mesh(LoftMeshKind::EnemyCad)
                .then_some(LoftMeshKind::EnemyCad),
        }
    }
}

impl Gfx {
    /// Async constructor — opens the wgpu device, uploads the atlas, builds
    /// both pipelines, and configures the swapchain to the window's current
    /// size. Call once at startup; thereafter use [`Gfx::resize`] on window
    /// resize and [`Gfx::render`] each frame.
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request adapter");

        log::info!("adapter: {:?}", adapter.get_info());

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("primary device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
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
        Self::assemble(Some(surface), device, queue, format, config)
    }

    /// Shared post-device setup for both [`Gfx::new`] (windowed) and
    /// [`Gfx::new_headless`] (capture): builds the offscreen target + atlas +
    /// every pipeline + the parallax background and assembles the struct, given
    /// an already-acquired device/queue/format/config and an optional surface.
    /// Keeping this one body means the windowed and headless paths render through
    /// the IDENTICAL pipelines (the whole point of capture: see the real frame).
    fn assemble(
        surface: Option<wgpu::Surface<'static>>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        config: wgpu::SurfaceConfiguration,
    ) -> Self {
        // (#135 Bruce) Each fresh Gfx starts at the 640×360 boot default — reset the
        // process-global scene size so a tool/test that built a prior Gfx and
        // cycled its res doesn't leak that into this one (the statics are global).
        Self::set_scene_size(BOOT_SCENE_W, BOOT_SCENE_H);
        // Create the offscreen at the LIVE scene size (now the default). One helper
        // so the constructor and the live `cycle_scene_res` build an identical
        // texture.
        let (offscreen, offscreen_view) = Self::make_offscreen(&device, scene_w(), scene_h());

        let atlas_data = atlas::generate_atlas();
        let atlas_size = wgpu::Extent3d {
            width: atlas::ATLAS_SIZE,
            height: atlas::ATLAS_SIZE,
            depth_or_array_layers: 1,
        };
        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprite atlas"),
            size: atlas_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas::ATLAS_SIZE * 4),
                rows_per_image: Some(atlas::ATLAS_SIZE),
            },
            atlas_size,
        );
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let sprites = SpritePipeline::new(&device, &atlas_view, &atlas_sampler);
        let polygons =
            PolygonPipeline::new(&device, &sprites.view_ubo, &atlas_view, &atlas_sampler);
        let textured_ships = TexturedShipPipeline::new(&device, &sprites.view_ubo);
        let blit = BlitPipeline::new(&device, format, &offscreen_view);
        let loft = crate::loft_gpu::LoftGpu::new(&device);
        let loft_blit = LoftShipBlit::new(&device);

        // Parallax background: build the 20-slot queue (solid-ink fallbacks) then
        // swap in Bruce's painted PNGs from assets/backgrounds. A missing/failed
        // manifest keeps the fallback bands rather than blanking the screen.
        let mut background = crate::background::Background::new(&device, &queue);
        match background.load_manifest(&device, &queue, std::path::Path::new("assets/backgrounds"))
        {
            Ok(n) => log::info!("background: loaded {n} painted layer(s) from assets/backgrounds"),
            Err(e) => log::warn!("background: manifest load failed ({e}); using fallback bands"),
        }

        let g = Self {
            surface,
            device,
            queue,
            config,
            offscreen_tex: offscreen,
            offscreen_view,
            sprites,
            polygons,
            textured_ships,
            blit,
            ship_sprites: std::collections::HashMap::new(),
            ship_bg_cache: std::collections::HashMap::new(),
            loft,
            loft_blit,
            loft_meshes: std::collections::HashMap::new(),
            loft_poses: std::collections::HashMap::new(),
            background: Some(background),
        };

        // (#76 scene-res) px→NDC from the LIVE scene size (default 480×270). The
        // scene-res cycle rewrites this so virtual-pixel coords map to NDC over the
        // resized offscreen.
        g.update_view_uniform();
        g.update_blit_uniform();
        g
    }

    /// (#76 scene-res) Build the offscreen render target + its view at `(w, h)`.
    /// One body for the constructor and the live [`Self::cycle_scene_res`] so the
    /// texture is identical (same format + usages); the `COPY_SRC` usage lets the
    /// headless capture read the frame back.
    fn make_offscreen(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen virtual-res target"),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            // COPY_SRC so the headless capture tool can read the rendered frame
            // back for a PNG (harmless for the windowed path).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    /// (#76 scene-res) Write the sprite/polygon view uniform's px→NDC from the
    /// LIVE scene size, so a virtual-pixel position maps to NDC over the current
    /// offscreen. Called at startup and after a scene-res cycle.
    fn update_view_uniform(&self) {
        let view = ViewUniform {
            px_to_ndc: [2.0 / scene_w() as f32, 2.0 / scene_h() as f32],
            _pad: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.sprites.view_ubo, 0, bytemuck::bytes_of(&view));
    }

    /// (#76 scene-res) Set the process-global scene size statics ([`SCENE_W`] /
    /// [`SCENE_H`]) so the renderer's free helpers ([`crate::hud`],
    /// [`crate::background`], the projector) read the live canvas. Called by the
    /// constructor (to the default) and the live cycle; the ACTUAL offscreen
    /// texture is recreated alongside in [`Self::cycle_scene_res`].
    fn set_scene_size(w: u32, h: u32) {
        SCENE_W.store(w.max(1), std::sync::atomic::Ordering::Relaxed);
        SCENE_H.store(h.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// (#76 scene-res) Cycle the LIVE scene (offscreen) resolution — `forward`
    /// steps to the next [`SCENE_RES_PRESETS`] (`'` key), else the previous (`;`).
    /// Recreates the offscreen texture at the new size, repoints the blit pipeline
    /// to the new view, and rewrites the px→NDC view + blit uniforms; the next
    /// frame composites the WHOLE scene (background + lanes + ships + HUD) at the
    /// new pixel count. The bin rebuilds its `ProjectorConfig` via
    /// [`crate::projector::ProjectorConfig::for_scene`] off the returned `(w, h)`
    /// so the lane geometry reprojects to match. Returns the new `(w, h)` for the
    /// bin's "SCENE wxh" readout.
    pub fn cycle_scene_res(&mut self, forward: bool) -> (u32, u32) {
        let (w, h) = (scene_w(), scene_h());
        let (nw, nh) = if forward {
            next_scene_res(w, h)
        } else {
            prev_scene_res(w, h)
        };
        Self::set_scene_size(nw, nh);
        let (tex, view) = Self::make_offscreen(&self.device, nw, nh);
        self.offscreen_tex = tex;
        self.offscreen_view = view;
        // The blit pipeline samples the offscreen view; rebuild its bind group to
        // point at the NEW view (the old one is dropped with the old texture).
        self.blit.rebind(&self.device, &self.offscreen_view);
        self.update_view_uniform();
        self.update_blit_uniform();
        (nw, nh)
    }

    /// The current LIVE scene (offscreen) resolution `(w, h)` — for the bin's
    /// readout. Mirrors the [`scene_w`] / [`scene_h`] globals.
    pub fn scene_res(&self) -> (u32, u32) {
        (scene_w(), scene_h())
    }

    /// HEADLESS constructor for the offscreen PNG capture tool (no window or
    /// swapchain). Builds the SAME device, atlas, sprite/polygon/loft pipelines,
    /// and parallax background as [`Gfx::new`], so a captured frame renders
    /// through the exact same path the game uses, but with no surface
    /// (`surface: None`). Headlessly, [`Gfx::render`] composites the scene into
    /// the offscreen then skips the (absent) swapchain blit and present, and
    /// [`Gfx::capture_png`] reads that offscreen back to a file. The permanent
    /// "give the team eyes without a display" tool.
    pub async fn new_headless() -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .expect("request adapter (headless)");
        log::info!("headless adapter: {:?}", adapter.get_info());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("headless device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .expect("request device (headless)");
        // No surface to derive a format from; the blit pipeline is built but never
        // run headlessly, so any sRGB format works. A dummy 1×1 config keeps the
        // shared `update_blit_uniform` math well-defined.
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: VIRTUAL_W * FIXED_UPSCALE,
            height: VIRTUAL_H * FIXED_UPSCALE,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        Self::assemble(None, device, queue, format, config)
    }

    /// Render ONE frame of `commands` into the 480×270 offscreen and save it as a
    /// PNG at `path` (the headless capture). Runs the SAME scene-to-offscreen
    /// composite as [`Gfx::render`] (background → batched sprites/polygons/loft
    /// ships), then copies the offscreen texture back to the CPU and writes RGBA
    /// PNG via the `image` crate. The team (and the renderer) Read the PNG to SEE
    /// the actual frame with no window/display needed.
    ///
    /// wgpu requires `copy_texture_to_buffer`'s `bytes_per_row` to be a multiple
    /// of 256; 480×4 = 1920 is NOT, so we pad to the next multiple and strip the
    /// padding per row before saving.
    /// Headless RGBA8 readback sibling of [`Self::capture_png`] — composites the
    /// scene then returns `(width, height, Vec<u8>)` of tight (un-padded) RGBA8
    /// bytes, stopping before any encode. The live-preview path in
    /// `broadside_vfx_editor` uses this to upload the engine frame into an egui
    /// texture (each preview frame), reusing the same pipeline `capture_png`
    /// proves headlessly.
    pub fn capture_rgba(
        &mut self,
        commands: &[DrawCommand],
    ) -> Result<(u32, u32, Vec<u8>), String> {
        self.render(commands)
            .map_err(|e| format!("render: {e:?}"))?;
        let (cap_w, cap_h) = (scene_w(), scene_h());
        let bytes_per_pixel = 4u32;
        let unpadded = cap_w * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let buf_size = u64::from(padded * cap_h);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture_rgba readback"),
            size: buf_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture_rgba copy"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.offscreen_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(cap_h),
                },
            },
            wgpu::Extent3d {
                width: cap_w,
                height: cap_h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(enc.finish()));
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        rx.recv()
            .map_err(|e| format!("map recv: {e}"))?
            .map_err(|e| format!("map_async: {e:?}"))?;
        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((unpadded * cap_h) as usize);
        for row in 0..cap_h {
            let start = (row * padded) as usize;
            rgba.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();
        Ok((cap_w, cap_h, rgba))
    }

    pub fn capture_png(
        &mut self,
        commands: &[DrawCommand],
        path: &std::path::Path,
    ) -> Result<(), String> {
        // 1) Composite the scene into the offscreen exactly as render() does
        //    (minus the swapchain blit) by reusing render() — headless, render()
        //    composites the offscreen then early-returns at the (absent) surface.
        self.render(commands)
            .map_err(|e| format!("render: {e:?}"))?;

        // 2) Read the offscreen back. Re-create a matching readback target: the
        //    offscreen view's texture is private, so render a 2nd time into a
        //    fresh COPY_SRC texture we own here is overkill — instead copy from the
        //    existing offscreen. We kept its handle via offscreen_view's texture;
        //    but TextureView doesn't expose its texture, so capture keeps its own
        //    readback by copying through a dedicated capture texture.
        // (#76 scene-res) Read back at the LIVE scene size (default 480×270) so a
        // capture after a `;`/`'` cycle saves the resized frame, not a clipped /
        // overrun 480×270 window of it.
        let (cap_w, cap_h) = (scene_w(), scene_h());
        let bytes_per_pixel = 4u32;
        let unpadded = cap_w * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT; // 256
        let padded = unpadded.div_ceil(align) * align;
        let buf_size = u64::from(padded * cap_h);
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture readback"),
            size: buf_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("capture copy"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.offscreen_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(cap_h),
                },
            },
            wgpu::Extent3d {
                width: cap_w,
                height: cap_h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(enc.finish()));

        // 3) Map + block until ready (headless tool, blocking is fine).
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        rx.recv()
            .map_err(|e| format!("map recv: {e}"))?
            .map_err(|e| format!("map_async: {e:?}"))?;

        // 4) Strip the per-row padding into a tight RGBA buffer.
        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((unpadded * cap_h) as usize);
        for row in 0..cap_h {
            let start = (row * padded) as usize;
            rgba.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();

        // 5) Save PNG (RGBA8). image is in-tree (the bg loader uses it).
        image::save_buffer(path, &rgba, cap_w, cap_h, image::ColorType::Rgba8)
            .map_err(|e| format!("png save: {e}"))?;
        Ok(())
    }

    /// Distinct cool/friendly tint multiplier applied to the player's copy of
    /// the shared CAD hull so it reads apart from the orange-accented enemy
    /// fleet (boosts blue, pulls red/green down a touch). See
    /// [`Self::install_player_cad`].
    const PLAYER_TINT: [f32; 3] = [0.62, 0.82, 1.15];

    /// Install the PLAYER loft ship: the same vendored CAD hull the enemies use
    /// (`assets/ships/broadside-ship.glb`), recoloured a distinct cool hue via
    /// [`Self::PLAYER_TINT`] so the player reads apart from the enemy fleet
    /// while sharing the faceted geometry. (Replaces the bare loft dagger, which
    /// — being a near-flat 0.77u-tall hull with no superstructure — read as a
    /// tilted plank bow-on.) Uploaded as [`LoftMeshKind::PlayerCad`]. Idempotent;
    /// per-ship pose is created lazily by [`Self::sync_loft_pose`]. Returns the
    /// import error if the bytes don't parse (caller logs + falls back to 2D).
    pub fn install_player_cad(
        &mut self,
        glb_bytes: &[u8],
    ) -> Result<(), crate::mesh_import::ImportError> {
        let ship = crate::mesh_import::load_glb(glb_bytes)?;
        let hull = self
            .loft
            .upload_imported_tinted(&self.device, &ship, Self::PLAYER_TINT);
        self.loft_meshes.insert(
            crate::sprites::LoftMeshKind::PlayerCad,
            LoftMesh {
                vbuf: hull.vbuf,
                vcount: hull.vcount,
                center_y: hull.center_y,
            },
        );
        Ok(())
    }

    /// Base hull albedo the lofted player hull is tinted from. (#62) DARKENED to a
    /// deep slate so the lit hull reads as a defined dark-gray SHAPE (matching the
    /// ref ship) rather than a pale light-blue fill/slab — and so the bright cyan
    /// stern engine glow POPS against it. Multiplied by [`Self::PLAYER_TINT`] then
    /// lit by the loft shader (which lifts it well above the raw albedo), so a low
    /// albedo lands a medium-dark hull, not black. Was [0.706,0.776,0.878] (pale).
    const LOFT_HULL_ALBEDO: [f32; 3] = [0.12, 0.14, 0.20];

    /// RED tint multiplier for the PLAYER Aegis hull (Bruce: the player ship is
    /// red). A per-channel multiply over the GLB's authored albedo — boosts red,
    /// pulls green/blue down — so the hull reads clearly red while keeping its
    /// shape + Lambert shading. EMISSIVE is untouched by `upload_imported_tinted`,
    /// so the unlit cyan stern engine glow stays bright + readable (lead: don't
    /// wash out the glow).
    // (#109) TRUE saturated red. The old [1.6, 0.42, 0.40] kept G+B near 0.4,
    // which desaturated the hull toward PINK (Bruce: "why is the player ship
    // pink? make it red"). Crushing green/blue hard while boosting red gives a
    // saturated true red; the AUTHORED emissive (cyan engine glow) is still
    // untouched by upload_imported_tinted so it stays readable over the red hull.
    const PLAYER_RED_TINT: [f32; 3] = [1.9, 0.16, 0.14];

    /// Install the PLAYER's faithful class hull directly from a `.glb` — the
    /// Aegis export (`assets/ships/Aegis.glb`) Bruce's tool bakes per the v5
    /// render contract. Imports via [`crate::mesh_import::load_glb`] and uploads
    /// through [`crate::loft_gpu::LoftGpu::upload_imported_tinted`] with
    /// [`Self::PLAYER_RED_TINT`] so the hull reads RED (Bruce's call) while the
    /// AUTHORED emissive (the unlit cyan engine glow) is left untouched and stays
    /// readable. Geometry is the faithful Aegis `render_aegis` proved (X-length 12,
    /// wide-low, stern nacelles). Uploaded as [`LoftMeshKind::PlayerLoft`], which
    /// [`Self::loft_kind`] prefers over the generic CAD hull. This is the LIVE-GAME
    /// player ship path; preferred over [`Self::install_player_loft_mesh`] now that
    /// the GLB pipeline carries the real mesh. Idempotent; per-ship pose created
    /// lazily by [`Self::sync_loft_pose`]. Returns the import error if the bytes
    /// don't parse (caller logs + falls back to 2D / sprite).
    ///
    /// (#187) `flip_prow`: when the GLB's prow sits at `−X` instead of the contract
    /// `+X` (e.g. `broadside-ship_03.glb`, confirmed by tip-width probe), pass `true`
    /// to half-turn the mesh about `Y` at install via
    /// [`crate::mesh_import::with_prow_flipped_180`] so it presents a `+X` prow to the
    /// shared chase-cam yaw math and renders bow-correct at every facing. A contract-
    /// conformant `+X` GLB passes `false` (no-op).
    pub fn install_player_glb(
        &mut self,
        glb_bytes: &[u8],
        flip_prow: bool,
    ) -> Result<(), crate::mesh_import::ImportError> {
        let mut ship = crate::mesh_import::load_glb(glb_bytes)?;
        if flip_prow {
            ship = crate::mesh_import::with_prow_flipped_180(ship);
        }
        let hull = self
            .loft
            .upload_imported_tinted(&self.device, &ship, Self::PLAYER_RED_TINT);
        self.loft_meshes.insert(
            crate::sprites::LoftMeshKind::PlayerLoft,
            LoftMesh {
                vbuf: hull.vbuf,
                vcount: hull.vcount,
                center_y: hull.center_y,
            },
        );
        Ok(())
    }

    /// Install the PLAYER's actual class hull as an already-LOFTED [`HullMesh`]
    /// (the Aegis hull, lofted by the caller from the design in
    /// `assets/ships/broadside-ship-library_v2.json`), recoloured the player hue.
    /// Uploaded as [`LoftMeshKind::PlayerLoft`], which [`Self::loft_kind`] prefers
    /// over the generic [`LoftMeshKind::PlayerCad`] — so the player renders as its
    /// real Aegis-class hull, not the vendored CAD mesh (Bruce: "use the Aegis
    /// there").
    ///
    /// Takes the lofted mesh (not the `ShipDesign`) so the GPU layer stays
    /// decoupled from the design-file schema — the bin lofts via
    /// [`crate::loft::loft_from_profiles`] from whatever design format it parsed.
    /// Idempotent; per-ship pose is created lazily by [`Self::sync_loft_pose`].
    pub fn install_player_loft_mesh(&mut self, mesh: &crate::loft::HullMesh) {
        let tint = Self::PLAYER_TINT;
        let base = Self::LOFT_HULL_ALBEDO;
        let tinted = [base[0] * tint[0], base[1] * tint[1], base[2] * tint[2]];
        // One tinted albedo per tri-soup vertex; no emissive (lofted hulls don't
        // glow, unlike the CAD canopy/gun accents).
        let colors = vec![tinted; mesh.positions.len()];
        let hull = self.loft.upload_hull(&self.device, mesh, &colors, &[]);
        self.loft_meshes.insert(
            crate::sprites::LoftMeshKind::PlayerLoft,
            LoftMesh {
                vbuf: hull.vbuf,
                vcount: hull.vcount,
                center_y: hull.center_y,
            },
        );
    }

    /// Install the enemy CAD loft mesh from glTF binary (`.glb`) bytes (the
    /// vendored `assets/ships/broadside-ship.glb`). Imports through
    /// [`crate::mesh_import::load_glb`] and uploads via
    /// [`crate::loft_gpu::LoftGpu::upload_imported`] so per-group materials —
    /// including the emissive orange accent — reach the shader. Every enemy
    /// placeholder shares this one mesh. Idempotent. Returns the import error if
    /// the bytes don't parse (the caller logs + falls back to 2D silhouettes).
    pub fn install_enemy_cad(
        &mut self,
        glb_bytes: &[u8],
    ) -> Result<(), crate::mesh_import::ImportError> {
        let ship = crate::mesh_import::load_glb(glb_bytes)?;
        let hull = self.loft.upload_imported(&self.device, &ship);
        self.loft_meshes.insert(
            crate::sprites::LoftMeshKind::EnemyCad,
            LoftMesh {
                vbuf: hull.vbuf,
                vcount: hull.vcount,
                center_y: hull.center_y,
            },
        );
        Ok(())
    }

    /// Neutral STEEL-GREY tint multiplier for the ENEMY copy of the Aegis hull. The
    /// PLAYER is now RED ([`Self::PLAYER_RED_TINT`], Bruce's call), so enemies take a
    /// neutral grey (lead: "prefer grey over cyan") — max contrast with the red hero
    /// AND it won't clash with the player's CYAN fire beams the way a blue hull
    /// would. Near-equal channels, a hair cool. A multiply over the GLB's authored
    /// albedo — keeps the hull shape/shading, desaturates to grey. (Was red, then
    /// steel-blue — both superseded by the lead's final grey-vs-red plan.)
    const ENEMY_TINT: [f32; 3] = [0.64, 0.67, 0.72];

    /// Install the ENEMY hull from the SAME Aegis `.glb` the player uses
    /// ([`Self::install_player_glb`]) but COOL STEEL-BLUE-tinted via
    /// [`Self::ENEMY_TINT`], so every enemy renders as the Aegis ship-class in a
    /// cool tone that reads apart from the RED player hull, instead of the generic
    /// CAD box. Uploaded as [`crate::sprites::LoftMeshKind::EnemyLoft`], which
    /// [`Self::loft_kind`] prefers over [`crate::sprites::LoftMeshKind::EnemyCad`].
    /// Enemies face the player (bow-on / oncoming), so the hull renders toward the
    /// camera — the `loft_facing_ground_yaw` Bow(S)=180 case. Runtime tint only, no
    /// GLB re-export. Idempotent; per-ship pose created lazily by
    /// [`Self::sync_loft_pose`]. Returns the import error if the bytes don't parse
    /// (caller logs + falls back to the CAD/2D enemy path).
    pub fn install_enemy_glb(
        &mut self,
        glb_bytes: &[u8],
    ) -> Result<(), crate::mesh_import::ImportError> {
        let ship = crate::mesh_import::load_glb(glb_bytes)?;
        let hull = self
            .loft
            .upload_imported_tinted(&self.device, &ship, Self::ENEMY_TINT);
        self.loft_meshes.insert(
            crate::sprites::LoftMeshKind::EnemyLoft,
            LoftMesh {
                vbuf: hull.vbuf,
                vcount: hull.vcount,
                center_y: hull.center_y,
            },
        );
        Ok(())
    }

    /// (#187) Install a SECOND enemy hull from `.glb` bytes as
    /// [`crate::sprites::LoftMeshKind::EnemyLoftB`], enemy-tinted like
    /// [`Self::install_enemy_glb`], so the fleet renders a MIX of two ship-classes
    /// for variety (Bruce: reuse the old player hull `broadside-ship_01.glb` as a
    /// second enemy alongside `broadside-ship_02.glb`). [`Self::loft_kind`] picks
    /// between `EnemyLoft` and `EnemyLoftB` per ship deterministically; when only one
    /// is installed every enemy falls back to it, so this is purely additive. Same
    /// `flip_prow` contract as [`Self::install_player_glb`] (01/02 are `+X`-prow → no
    /// flip needed; pass `true` only for a `−X`-prow hull). Idempotent; returns the
    /// import error if the bytes don't parse (caller logs + keeps the single mesh).
    pub fn install_enemy_glb_b(
        &mut self,
        glb_bytes: &[u8],
        flip_prow: bool,
    ) -> Result<(), crate::mesh_import::ImportError> {
        let mut ship = crate::mesh_import::load_glb(glb_bytes)?;
        if flip_prow {
            ship = crate::mesh_import::with_prow_flipped_180(ship);
        }
        let hull = self
            .loft
            .upload_imported_tinted(&self.device, &ship, Self::ENEMY_TINT);
        self.loft_meshes.insert(
            crate::sprites::LoftMeshKind::EnemyLoftB,
            LoftMesh {
                vbuf: hull.vbuf,
                vcount: hull.vcount,
                center_y: hull.center_y,
            },
        );
        Ok(())
    }

    /// Whether a loft mesh of the given kind is uploaded (so hud emits a
    /// `LoftShip` command for ships of that kind instead of their 2D
    /// silhouette).
    pub fn has_loft_mesh(&self, kind: crate::sprites::LoftMeshKind) -> bool {
        self.loft_meshes.contains_key(&kind)
    }

    /// (#76) Cycle the SHIP loft-render resolution LIVE — `forward` steps to the
    /// next [`crate::loft_gpu::LOFT_RES_PRESETS`] (`]` key), else the previous
    /// (`[`). Recreates the loft offscreen targets at the new size; the next
    /// frame's loft pre-pass renders the hull at the new pixel chunkiness. Cheap
    /// (3 small textures); the bin calls this on a keypress + redraws. Returns the
    /// new `(w, h)` for the bin's "SHIP wxh" readout.
    pub fn cycle_loft_res(&mut self, forward: bool) -> (u32, u32) {
        let (w, h) = self.loft.output_size();
        let (nw, nh) = if forward {
            crate::loft_gpu::next_loft_res(w, h)
        } else {
            crate::loft_gpu::prev_loft_res(w, h)
        };
        self.loft.resize(&self.device, nw, nh);
        (nw, nh)
    }

    /// The current SHIP loft-render resolution `(w, h)` — for the bin's readout.
    pub const fn loft_res(&self) -> (u32, u32) {
        self.loft.output_size()
    }

    /// DYNAMIC-LIGHTING TEST (headless): upload an [`crate::mesh_import::ImportedShip`]
    /// (e.g. the v2-lofted Aegis from [`crate::ship_loft_v2`]), render ONE loft
    /// frame at `yaw_deg` with the KEY light at (`key_az_deg`, `key_el_deg`,
    /// `key_intensity`), and write the posterized 3D ship to `path` as a PNG.
    /// A caller sweeps `key_az_deg` across an arc + captures each frame to show
    /// the shadows/hues shift on the hull — the dynamic-lighting proof. This is
    /// the real loft render path (depth + Lambert + emissive + posterize),
    /// isolated from the 2D gameplay compositor (which is untouched). Slow
    /// (per-call GPU→CPU readback) — a capture/demo entry point only.
    #[allow(clippy::too_many_arguments)]
    pub fn render_loft_to_png(
        &self,
        ship: &crate::mesh_import::ImportedShip,
        yaw_deg: f32,
        key_az_deg: f32,
        key_el_deg: f32,
        key_intensity: f32,
        pitch_deg: f32,
        half_extent: f32,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let hull = self.loft.upload_imported(&self.device, ship);
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("loft sweep frame"),
            });
        self.loft.render_ship_lit_framed(
            &self.queue,
            &mut enc,
            &hull.vbuf,
            hull.vcount,
            yaw_deg,
            hull.center_y,
            key_az_deg,
            key_el_deg,
            key_intensity,
            pitch_deg,
            half_extent,
        );
        self.queue.submit(Some(enc.finish()));
        self.loft.read_output_png(&self.device, &self.queue, path)
    }

    /// Ensure a [`crate::loft_gpu::ShipPose`] exists for `ship_id` and matches
    /// `orientation`: creates one resting at `orientation` if absent, otherwise
    /// reorients it toward `orientation` (a no-op when already there, so this
    /// auto-detects bow-on↔broadside flips and tweens them). Called per ship
    /// per frame by the bin. No GPU work — pure pose state.
    pub fn sync_loft_pose(&mut self, ship_id: &str, orientation: crate::types::Orientation) {
        match self.loft_poses.get_mut(ship_id) {
            Some(pose) => pose.reorient_to(orientation),
            None => {
                self.loft_poses.insert(
                    ship_id.to_string(),
                    crate::loft_gpu::ShipPose::new(orientation),
                );
            }
        }
    }

    /// Drop poses for ships no longer on the board (their ids are absent from
    /// `live_ids`), so a defeated/departed ship's pose doesn't linger. Called
    /// per frame after [`Self::sync_loft_pose`] over the live ships.
    pub fn retain_loft_poses(&mut self, live_ids: &[String]) {
        self.loft_poses
            .retain(|id, _| live_ids.iter().any(|l| l == id));
    }

    /// Advance every loft ship's pose by `dt` seconds (idle + any reorient
    /// tween). Returns `true` if any pose exists, so the caller keeps the
    /// redraw loop alive — the idle bob/roll is continuous, so resting 3D ships
    /// need steady frames to "breathe" (cost: one 320×200 render/frame per
    /// on-screen 3D ship; acceptable for ≤ lane-count ships). `false` when no
    /// loft ships.
    pub fn advance_loft_poses(&mut self, dt: f32) -> bool {
        if self.loft_poses.is_empty() {
            return false;
        }
        for pose in self.loft_poses.values_mut() {
            pose.advance(dt);
        }
        true
    }

    /// Drive the parallax background each frame (#57): set its horizontal target
    /// to the player's column (`pos_target`, 0..positions-1) and its depth target
    /// to the campaign `level` (`focus_target`), then ease both toward target by
    /// `dt`. This is what makes the background PAN as the player moves side to
    /// side — without it the layers are static (Bruce: "parallax isn't working").
    /// The slot/parallax math lives in `background.rs` (spec §4, unit-tested); the
    /// bin calls this once per redraw with the player's live column + level.
    pub fn update_background(&mut self, focus_target: usize, pos_target: usize, dt: f32) {
        if let Some(bg) = self.background.as_mut() {
            bg.set_focus_target(focus_target);
            bg.set_pos_target(pos_target);
            bg.tween(dt);
        }
    }

    /// Begin a smooth reorient of one loft ship to `orientation`. No-op if that
    /// ship has no pose yet (use [`Self::sync_loft_pose`] to create + reorient
    /// in one call).
    pub fn reorient_loft_pose(&mut self, ship_id: &str, orientation: crate::types::Orientation) {
        if let Some(pose) = self.loft_poses.get_mut(ship_id) {
            pose.reorient_to(orientation);
        }
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width > 0 && size.height > 0 {
            self.config.width = size.width;
            self.config.height = size.height;
            if let Some(surface) = self.surface.as_ref() {
                surface.configure(&self.device, &self.config);
            }
            self.update_blit_uniform();
        }
    }

    pub fn reconfigure(&mut self) {
        if let Some(surface) = self.surface.as_ref() {
            surface.configure(&self.device, &self.config);
        }
        self.update_blit_uniform();
    }

    /// Walk `assets/sprites/` and upload any `<class>_<stance>_<view>.png`
    /// files to GPU textures. Missing files are silently skipped (the
    /// procedural silhouette renders as the fallback). Each successfully
    /// loaded sprite is keyed by the same slug the `SPRITE_SPEC` defines.
    ///
    /// **Fallback chain** (applied in order per `<class>_<stance>_<view>`
    /// slot, fall through on miss):
    ///
    /// | Slot                       | 1. Explicit | 2. Derived | 3. Procedural |
    /// |----------------------------|:-----------:|:----------:|:-------------:|
    /// | `<class>_bowOnFore_*.png`  | ✓           | —          | renderer side |
    /// | `<class>_bowOnAft_*.png`   | ✓           | `mirror_horizontal(bowOnFore_<view>)` | renderer side |
    /// | `<class>_broadside_top.png`| ✓           | `rotate_90_cw(bowOnFore_top)` | renderer side |
    /// | `<class>_broadside_side.png` | ✓         | — (no derivation possible) | renderer side |
    ///
    /// Net result: bruce paints just `bowOnFore_side.png` and
    /// `bowOnFore_top.png` for each class; the loader derives 3 of the
    /// other 4 sprite slots. `broadside_side` is the only slot that
    /// stays procedural until painted explicitly (it's an end-on view
    /// of the hull that can't be reconstructed from the other faces).
    ///
    /// Explicit files always take precedence over derivations. Bruce
    /// can paint asymmetric ships per-class by dropping explicit
    /// bowOnAft / `broadside_top` PNGs.
    ///
    /// Returns the count of sprites loaded so the caller can log it.
    pub fn try_load_ship_sprites(&mut self, asset_dir: &std::path::Path) -> usize {
        use crate::sprites::{
            load_sprite, mirror_horizontal, rotate_90_cw, SpriteStance, SpriteView,
        };
        // Invalidate cached bind groups — the underlying texture views
        // may have been replaced.
        self.ship_bg_cache.clear();
        // Class slugs the loader probes. `aegis` is the first
        // broadside-native player class (bruce's hand-painted art, see
        // assets/sprites/aegis_*.png). frigate / scout / gunboat are
        // placeholder names from the early-scaffold demo phase and
        // will eventually disappear when the canonical class roster
        // lands; keeping them in the probe list is free (missing files
        // are silently skipped) and avoids breaking the existing
        // procedural fallback.
        let classes = ["aegis", "frigate", "scout", "gunboat"];
        let views = [SpriteView::Side, SpriteView::Top];
        let mut loaded = 0;
        for class in &classes {
            for &view in &views {
                // Step 1: bowOnFore — always explicit-only. Remember
                // the image; we'll derive bowOnAft + (for top) broadside
                // from it.
                let fore = load_sprite(asset_dir, class, SpriteStance::BowOnFore, view);
                if let Some(img) = fore.as_ref() {
                    let slug = format!(
                        "{}_{}_{}",
                        class,
                        SpriteStance::BowOnFore.slug(),
                        view.slug()
                    );
                    self.upload_ship_sprite(&slug, img);
                    loaded += 1;
                }
                // Step 2: bowOnAft = explicit, else mirror(bowOnFore).
                let aft_explicit = load_sprite(asset_dir, class, SpriteStance::BowOnAft, view);
                match (aft_explicit, fore.as_ref()) {
                    (Some(img), _) => {
                        let slug = format!(
                            "{}_{}_{}",
                            class,
                            SpriteStance::BowOnAft.slug(),
                            view.slug()
                        );
                        self.upload_ship_sprite(&slug, &img);
                        loaded += 1;
                    }
                    (None, Some(fore_img)) => {
                        let mirrored = mirror_horizontal(fore_img);
                        let slug = format!(
                            "{}_{}_{}",
                            class,
                            SpriteStance::BowOnAft.slug(),
                            view.slug()
                        );
                        log::debug!("sprite: deriving {slug} from horizontally-mirrored bowOnFore");
                        self.upload_ship_sprite(&slug, &mirrored);
                        loaded += 1;
                    }
                    (None, None) => {}
                }
                // Step 3: broadside.
                //
                // - For `Top`: explicit → rotate90(bowOnFore_top) → none.
                // - For `Side`: explicit → none (no derivation possible).
                let bs_explicit = load_sprite(asset_dir, class, SpriteStance::Broadside, view);
                match (bs_explicit, view, fore.as_ref()) {
                    (Some(img), _, _) => {
                        let slug = format!(
                            "{}_{}_{}",
                            class,
                            SpriteStance::Broadside.slug(),
                            view.slug()
                        );
                        self.upload_ship_sprite(&slug, &img);
                        loaded += 1;
                    }
                    (None, SpriteView::Top, Some(fore_top)) => {
                        let rotated = rotate_90_cw(fore_top);
                        let slug = format!(
                            "{}_{}_{}",
                            class,
                            SpriteStance::Broadside.slug(),
                            view.slug()
                        );
                        log::debug!("sprite: deriving {slug} from rotate90(bowOnFore_top)");
                        self.upload_ship_sprite(&slug, &rotated);
                        loaded += 1;
                    }
                    _ => {
                        // No explicit broadside, and either:
                        //  - view==Side (no derivation defined), or
                        //  - view==Top but no bowOnFore_top to rotate.
                        // Leave the slot empty; the procedural
                        // silhouette covers it.
                    }
                }
            }
        }

        // (#67) v2 15-FACING set: probe the contract-v2 facing sheet
        // (`<class>_f00.png` .. `<class>_f14.png` — one PNG per facing,
        // `facing_wheel::facing_slug` naming) and upload each present frame under
        // its slug. ADDITIVE + forward-compatible: the frames don't exist yet
        // (Bruce's 15-facing bake is pending), so this loads 0 today and does NOT
        // disturb the 4-set path above — push_ship_2d keeps drawing the existing
        // sprites until the f-frames land, then flips to the wheel. Per-facing
        // PIVOT is deferred to a real format once the bake shows whether it ships
        // pivot metadata / trimmed frames; until then the renderer assumes the
        // sprite is centred on the cell (untrimmed canvas).
        for class in &classes {
            for i in 0..crate::facing_wheel::FACING_COUNT {
                let slug = format!("{class}_f{i:02}");
                let path = asset_dir.join("sprites").join(format!("{slug}.png"));
                match image::open(&path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let sprite = crate::sprites::SpriteImage {
                            width: w,
                            height: h,
                            rgba: rgba.into_raw(),
                        };
                        self.upload_ship_sprite(&slug, &sprite);
                        loaded += 1;
                    }
                    Err(e) => {
                        log::debug!("facing sprite skipped: {} ({e})", path.display());
                    }
                }
            }
        }
        loaded
    }

    /// Internal: upload one decoded sprite image to a wgpu texture and
    /// register it in `ship_sprites` under `slug`.
    fn upload_ship_sprite(&mut self, slug: &str, img: &crate::sprites::SpriteImage) {
        let size = wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(slug),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: Some(img.height),
            },
            size,
        );
        let texture_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.ship_sprites.insert(
            slug.to_string(),
            ShipSpriteEntry {
                texture_view,
                dimensions: (img.width, img.height),
            },
        );
    }

    /// True if a sprite has been loaded for the given class/stance/view.
    /// The textured-ship path uses this to decide whether to render the
    /// PNG or fall back to the procedural silhouette.
    pub fn has_ship_sprite(
        &self,
        class: &str,
        stance: crate::sprites::SpriteStance,
        view: crate::sprites::SpriteView,
    ) -> bool {
        let slug = format!("{}_{}_{}", class, stance.slug(), view.slug());
        self.ship_sprites.contains_key(&slug)
    }

    /// (#67) Whether the v2 15-facing frame `index` (`0..FACING_COUNT`) is loaded
    /// for `class` — i.e. `<class>_f{index:02}` was found by
    /// [`Self::try_load_ship_sprites`]. `push_ship_2d` gates the wheel draw on this:
    /// true once Bruce's 15-facing bake is dropped, false today (→ keep the
    /// current sprite/flat-box path, no regression).
    pub fn has_facing_sprite(&self, class: &str, index: usize) -> bool {
        self.ship_sprites
            .contains_key(&format!("{class}_f{index:02}"))
    }

    /// Build the per-slot (slot, side, top) bind group on first request
    /// and cache it. If either texture slug is missing from
    /// `ship_sprites`, the cache entry is **not** populated — the
    /// render loop checks the cache and skips drawing if absent.
    fn ensure_ship_bind_group(&mut self, slot_idx: usize, side: SpriteSlug, top: SpriteSlug) {
        let key = (slot_idx as u32, side, top);
        if self.ship_bg_cache.contains_key(&key) {
            return;
        }
        let side_entry = self.ship_sprites.get(side.as_str());
        let top_entry = self.ship_sprites.get(top.as_str());
        let (side_view, top_view) = if let (Some(s), Some(t)) = (side_entry, top_entry) {
            (&s.texture_view, &t.texture_view)
        } else {
            log::debug!(
                "ship bg skipped: side={} top={} (one or both not loaded)",
                side.as_str(),
                top.as_str()
            );
            return;
        };
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ship per-instance bg"),
            layout: &self.textured_ships.ship_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.textured_ships.blend_ubos[slot_idx].as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(side_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(top_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.textured_ships.sampler),
                },
            ],
        });
        self.ship_bg_cache.insert(key, bg);
    }

    /// Compute the integer-scaled, letterboxed NDC quad that maps the
    /// virtual-resolution offscreen target into the swapchain. Recomputed on
    /// every resize so the letterboxing tracks window changes.
    ///
    /// **Fixed ×[`FIXED_UPSCALE`] integer scale (v2 decision #1).** The canvas
    /// is 480×270 and the reference window is 1920×1080, so the intended scale
    /// is exactly ×4: every offscreen texel becomes a 4×4 block of window
    /// pixels with NEAREST sampling — crisp, shimmer-free pixel art. We pick
    /// `min(FIXED_UPSCALE, w/VIRTUAL_W floored, h/VIRTUAL_H floored)` clamped to
    /// ≥1, so the common 1920×1080 (and any larger) window renders at the full
    /// ×4 and letterboxes the remainder, while a smaller window falls back to
    /// the largest integer scale that still fits rather than clipping. The
    /// scaled canvas is centered; the leftover on each axis is black letterbox.
    fn update_blit_uniform(&self) {
        let w = self.config.width;
        let h = self.config.height;

        // (#65) FILL the window — Bruce: "isn't taking up the full screen, it is
        // framed." The old fixed-×4 INTEGER scale floored to ×3 on his ~1920×1040
        // window → a 1440×810 canvas centred with big black margins. The board is
        // 480×270 = 16:9, so an aspect-preserving FRACTIONAL scale-to-fit fills a
        // 16:9 window edge-to-edge (and only thin bars on an off-aspect window),
        // matching v1's full-screen feel. Trade: non-integer Nearest upscaling can
        // shimmer slightly, but full-screen > pixel-perfect integer here (Bruce's
        // call). Aspect preserved (fit, not crop) so nothing is cut off.
        // (#76 scene-res) Fit the LIVE offscreen size (default 480×270) into the
        // window. All presets are 16:9 like the default, so the fit-to-window math
        // is unchanged at any preset — the source is just a different pixel count
        // of the same aspect, NEAREST-upscaled to fill.
        let (sw, sh) = (scene_w() as f32, scene_h() as f32);
        let wf = w as f32;
        let hf = h as f32;
        let scale = (wf / sw).min(hf / sh).max(1.0);
        let scaled_w = sw * scale;
        let scaled_h = sh * scale;
        // Center the scaled canvas; any leftover (off-aspect window) is letterbox.
        let offset_x = (wf - scaled_w) * 0.5;
        let offset_y = (hf - scaled_h) * 0.5;

        let ndc_x_min = (offset_x / wf) * 2.0 - 1.0;
        let ndc_x_max = ((offset_x + scaled_w) / wf) * 2.0 - 1.0;
        let ndc_y_max = 1.0 - (offset_y / hf) * 2.0;
        let ndc_y_min = 1.0 - ((offset_y + scaled_h) / hf) * 2.0;

        let blit = BlitUniform {
            ndc_min: [ndc_x_min, ndc_y_min],
            ndc_max: [ndc_x_max, ndc_y_max],
        };
        self.queue
            .write_buffer(&self.blit.ubo, 0, bytemuck::bytes_of(&blit));
    }

    /// (UNIFY) Render the WHOLE loft fleet as real 3-D hulls through the unified
    /// camera ([`crate::projector::unified_view_proj`]) — the same camera the grid is
    /// drawn with — then blit the posterized output FULL-SCREEN over the offscreen
    /// scene. Because every hull goes through the grid's own projection, the fleet
    /// LIVES in the grid: nose→VP + per-column outward lean fall out of the
    /// perspective, no per-ship bake/blit fudging. Each hull is placed at its cell's
    /// world point ([`crate::projector::cell_world_center`]) and yawed to its world
    /// heading ([`LoftShipInstance::unified_yaw_rad`]). Replaces the per-cell loft
    /// blit when [`unified_enabled`]. `cleared` tracks whether the offscreen has been
    /// cleared yet (the full-screen blit clears on the first draw of the frame).
    fn render_unified_fleet(&self, loft_quads: &[LoftShipInstance], cleared: &mut bool) {
        // (#188) Build view_proj from the SCENE config so SHIPS and GRID share ONE
        // projection — same aspect, same FOV, same camera. The grid is drawn via
        // `scene_projector_cfg(scene_w, scene_h)`, so the ships MUST go through the
        // exact same cfg or back-row/edge-col ships drift OFF their cells (the
        // earlier #84 attempt overrode frame_w/h to the loft TARGET aspect 1.6 to
        // expose the per-column lean, but that aspect mismatch made back-row ships
        // diverge horizontally from their grid cells — col 4 row 1 player rendered
        // OUTSIDE the right grid edge). The loft offscreen is rendered into at
        // scene-aspect view_proj, then full-screen blit to the scene quad: the blit
        // resamples the (loft_h ≠ scene_h)-tall loft texture to the scene quad
        // 1:1 horizontally and vertically scaled by scene_h/loft_h, which preserves
        // the projected ship positions (the world→ndc→pixel chain matches the
        // direct-render-to-scene case exactly). Per-column lean still emerges from
        // world geometry (constant-X edges of a +Z-pointing hull converge to the VP).
        let cfg = scene_projector_cfg(scene_w() as f32, scene_h() as f32);
        let view_proj = crate::projector::unified_view_proj(&cfg);
        // Build the per-hull (vbuf, vcount, model) draw list; scoped so the
        // immutable borrow of `self.loft_meshes` drops before the blit below.
        {
            let mut draws: Vec<(&wgpu::Buffer, u32, [f32; 16])> =
                Vec::with_capacity(loft_quads.len());
            for lq in loft_quads {
                let Some(mesh) = self.loft_meshes.get(&lq.kind) else {
                    continue;
                };
                if !self.loft_poses.contains_key(lq.ship_id.as_str()) {
                    continue;
                }
                // (#201 fix A) Read the FRACTIONAL cell so a tweened ship's hull
                // SLIDES cell-to-cell through this pass instead of snapping. When
                // the ship is at rest cell_frac == cell (cast to f32), so this
                // matches the prior cell_world_center path exactly + the #188
                // alignment guard holds.
                let mut center = crate::projector::cell_world_center_frac(
                    lq.cell_frac[0],
                    lq.cell_frac[1],
                    &cfg,
                );
                center[1] += UNIFIED_SHIP_LIFT; // sit ON the plane, not half-buried
                let model = crate::loft_gpu::unified_model(
                    center,
                    lq.unified_yaw_rad,
                    unified_ship_scale(),
                );
                draws.push((&mesh.vbuf, mesh.vcount, model));
            }
            if draws.is_empty() {
                return;
            }
            self.loft
                .render_unified_ships(&self.device, &self.queue, view_proj, &draws);
        }

        // Blit the posterized fleet FULL-SCREEN onto the offscreen scene (over the
        // grid, under the HUD). Full-frame quad in virtual-pixel space.
        let (w, h) = (scene_w() as f32, scene_h() as f32);
        let qu = LoftQuadUniform {
            p0: [0.0, 0.0],
            p1: [w, 0.0],
            p2: [w, h],
            p3: [0.0, h],
            px_to_ndc: [2.0 / w, 2.0 / h],
            _pad: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.loft_blit.quad_ubo, 0, bytemuck::bytes_of(&qu));
        let bg = self
            .loft_blit
            .bind_group(&self.device, self.loft.output_view());
        let load = if *cleared {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(CLEAR)
        };
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("loft unified fleet blit"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loft unified fleet blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.offscreen_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.loft_blit.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(std::iter::once(enc.finish()));
        *cleared = true;
    }

    /// Render one frame. `commands` is the full draw command list in
    /// back-to-front order; the scene compositor in [`crate::hud`] builds it.
    /// Consecutive same-variant commands are batched into a single GPU draw.
    /// Sprites and polygons each have their own buffer, sized once at startup
    /// ([`MAX_SPRITES`] / [`MAX_POLYGONS`]).
    pub fn render(&mut self, commands: &[DrawCommand]) -> Result<(), wgpu::SurfaceError> {
        // Walk the commands once, splitting into contiguous batches by variant.
        // For each batch we record (kind, offset, count) where offset is the
        // index into the per-variant buffer the batch occupies.
        let mut sprite_buf: Vec<SpriteInstance> = Vec::with_capacity(commands.len());
        let mut polygon_buf: Vec<PolygonInstance> = Vec::with_capacity(commands.len());
        // Textured-ship instances are stored as the 4 corner positions only
        // (the slugs + blend_t are CPU-side per-batch metadata).
        let mut ship_corner_buf: Vec<[f32; 8]> = Vec::with_capacity(MAX_TEXTURED_SHIPS);
        let mut ship_meta: Vec<(SpriteSlug, SpriteSlug, f32)> =
            Vec::with_capacity(MAX_TEXTURED_SHIPS);
        // Loft-ship blit destination quads (one per LoftShip command). For the
        // milestone there is one (the demo player); the design generalizes to
        // ≤ lane-count.
        let mut loft_quads: Vec<LoftShipInstance> = Vec::new();
        enum BatchKind {
            Sprite,
            Polygon,
            // index into ship_corner_buf / ship_meta
            TexturedShip(u32),
            // index into loft_quads
            LoftShip(u32),
        }
        struct Batch {
            kind: BatchKind,
            start: u32,
            count: u32,
        }
        let mut batches: Vec<Batch> = Vec::new();
        for cmd in commands {
            match cmd {
                DrawCommand::Sprite(s) => {
                    if (sprite_buf.len() as u64) >= MAX_SPRITES {
                        continue;
                    }
                    let start = sprite_buf.len() as u32;
                    sprite_buf.push(*s);
                    match batches.last_mut() {
                        Some(b) if matches!(b.kind, BatchKind::Sprite) => b.count += 1,
                        _ => batches.push(Batch {
                            kind: BatchKind::Sprite,
                            start,
                            count: 1,
                        }),
                    }
                }
                DrawCommand::Polygon(p) => {
                    if (polygon_buf.len() as u64) >= MAX_POLYGONS {
                        continue;
                    }
                    let start = polygon_buf.len() as u32;
                    polygon_buf.push(*p);
                    match batches.last_mut() {
                        Some(b) if matches!(b.kind, BatchKind::Polygon) => b.count += 1,
                        _ => batches.push(Batch {
                            kind: BatchKind::Polygon,
                            start,
                            count: 1,
                        }),
                    }
                }
                DrawCommand::TexturedShip(t) => {
                    if ship_corner_buf.len() >= MAX_TEXTURED_SHIPS {
                        continue;
                    }
                    let idx = ship_corner_buf.len() as u32;
                    ship_corner_buf.push([
                        t.p0[0], t.p0[1], t.p1[0], t.p1[1], t.p2[0], t.p2[1], t.p3[0], t.p3[1],
                    ]);
                    ship_meta.push((t.side, t.top, t.blend_t));
                    // Each textured-ship draw is its own batch (different
                    // bind group per ship).
                    batches.push(Batch {
                        kind: BatchKind::TexturedShip(idx),
                        start: idx,
                        count: 1,
                    });
                }
                DrawCommand::LoftShip(l) => {
                    let idx = loft_quads.len() as u32;
                    loft_quads.push(*l);
                    // Its own batch (own bind group + the live loft texture).
                    batches.push(Batch {
                        kind: BatchKind::LoftShip(idx),
                        start: idx,
                        count: 1,
                    });
                }
            }
        }
        if (commands.len() as u64) > MAX_SPRITES + MAX_POLYGONS + MAX_TEXTURED_SHIPS as u64 {
            log::warn!(
                "draw command count {} exceeds capacity; truncating",
                commands.len()
            );
        }

        if !sprite_buf.is_empty() {
            self.queue.write_buffer(
                &self.sprites.instance_vbuf,
                0,
                bytemuck::cast_slice(&sprite_buf),
            );
        }
        if !polygon_buf.is_empty() {
            self.queue.write_buffer(
                &self.polygons.instance_vbuf,
                0,
                bytemuck::cast_slice(&polygon_buf),
            );
        }
        if !ship_corner_buf.is_empty() {
            self.queue.write_buffer(
                &self.textured_ships.instance_vbuf,
                0,
                bytemuck::cast_slice(&ship_corner_buf),
            );
            // Write per-ship blend uniforms and ensure bind groups exist.
            for (i, (side, top, blend)) in ship_meta.iter().enumerate() {
                let blend_u = BlendUniform {
                    blend_t: *blend,
                    _pad: [0.0; 3],
                };
                self.queue.write_buffer(
                    &self.textured_ships.blend_ubos[i],
                    0,
                    bytemuck::bytes_of(&blend_u),
                );
                self.ensure_ship_bind_group(i, *side, *top);
            }
        }

        // Acquire the swapchain image. On a STALE surface (Outdated/Lost — from a
        // resize, a compositor change, or GPU/memory pressure) reconfigure and
        // re-acquire ONCE in place, rather than letting `?` propagate and the
        // caller DROP the frame: a dropped frame presents nothing (the previous
        // backbuffer / a black image), so a run of Outdated frames reads as the
        // "fade to black → snap to grey" flicker (#47). Re-acquiring here renders
        // this frame instead. Only a SECOND failure (or a non-stale error)
        // propagates to the caller's fallback.
        //
        // The swap image is acquired LATER (just before the final blit) — the
        // offscreen composite below needs no surface, so a headless Gfx (no
        // surface, e.g. `capture_png`) still composites the full scene into the
        // offscreen and only skips the swapchain blit/present.

        // The scene-to-offscreen composite walks the batches in z-order. Most
        // batches (sprites / polygons / textured ships) draw directly into the
        // offscreen target in one render pass. Loft ships are special: each one
        // first renders its 3D hull at its animated pose into the loft
        // pipeline's SHARED 320×200 target, then blits that posterized texture
        // onto its lane quad. Because the loft target (and the loft's scene/quad
        // uniforms) are shared across all ships, ship N's render+blit MUST be
        // fully submitted before ship N+1 overwrites them — `queue.write_buffer`
        // only flushes at submit time, so batching every ship into one encoder
        // would clobber all but the last ship's yaw + quad. So the composite is
        // split into segments at each loft-ship boundary: a run of non-loft
        // batches renders into the offscreen in one pass and is submitted, then
        // each loft ship renders+blits in its own submitted encoder. The first
        // segment CLEARS the offscreen; every later segment LOADs it (preserving
        // what earlier segments drew), keeping the single-pass z-order intact
        // across the splits.
        // `cleared` is a running flag mutated across the segment loop below (first
        // segment clears, later ones load), not a simple let-then-if assignment.
        #[allow(clippy::useless_let_if_seq)]
        let mut cleared = false;

        // (#8/#50) Draw the parallax background into the offscreen FIRST, clearing
        // it; the scene segments below then LOAD on top. Replaces the bin-side
        // band placeholder with the real painted PNG layers.
        if let Some(bg) = self.background.as_ref() {
            let mut bg_encoder =
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("background to offscreen"),
                    });
            bg.draw(
                &self.device,
                &self.queue,
                &mut bg_encoder,
                &self.offscreen_view,
                Some(CLEAR),
            );
            self.queue.submit(std::iter::once(bg_encoder.finish()));
            cleared = true;
        }

        // Draw a contiguous run of non-loft batches into the offscreen target.
        // Returns the index past the run. `cleared` selects clear-vs-load.
        let flush_scene_run =
            |gfx: &Self, batches: &[Batch], start: usize, cleared: bool| -> usize {
                // Collect the run [start, end) of non-loft batches.
                let mut end = start;
                while end < batches.len() && !matches!(batches[end].kind, BatchKind::LoftShip(_)) {
                    end += 1;
                }
                if end == start {
                    return end;
                }
                let load = if cleared {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(CLEAR)
                };
                let mut encoder =
                    gfx.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("scene to offscreen"),
                        });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("scene to offscreen"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &gfx.offscreen_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    for b in &batches[start..end] {
                        match b.kind {
                            BatchKind::Sprite => {
                                pass.set_pipeline(&gfx.sprites.pipeline);
                                pass.set_bind_group(0, &gfx.sprites.bind_group, &[]);
                                pass.set_vertex_buffer(0, gfx.sprites.quad_vbuf.slice(..));
                                pass.set_vertex_buffer(1, gfx.sprites.instance_vbuf.slice(..));
                                pass.draw(0..6, b.start..(b.start + b.count));
                            }
                            BatchKind::Polygon => {
                                pass.set_pipeline(&gfx.polygons.pipeline);
                                pass.set_bind_group(0, &gfx.polygons.bind_group, &[]);
                                pass.set_vertex_buffer(0, gfx.polygons.instance_vbuf.slice(..));
                                pass.draw(0..6, b.start..(b.start + b.count));
                            }
                            BatchKind::TexturedShip(slot_idx) => {
                                let (side, top, _blend) = ship_meta[slot_idx as usize];
                                // Bind group missing — sprites for this slug pair
                                // aren't loaded. Skip the draw; the procedural
                                // polygons below stay visible.
                                let Some(bg) = gfx.ship_bg_cache.get(&(slot_idx, side, top)) else {
                                    continue;
                                };
                                pass.set_pipeline(&gfx.textured_ships.pipeline);
                                pass.set_bind_group(0, &gfx.textured_ships.view_bg, &[]);
                                pass.set_bind_group(1, bg, &[]);
                                // Offset the vbuf to this slot's 32 bytes.
                                let off = u64::from(slot_idx) * 32;
                                pass.set_vertex_buffer(
                                    0,
                                    gfx.textured_ships.instance_vbuf.slice(off..off + 32),
                                );
                                // Draw 6 verts (two triangles) of one instance.
                                pass.draw(0..6, 0..1);
                            }
                            BatchKind::LoftShip(_) => unreachable!("run excludes loft batches"),
                        }
                    }
                }
                gfx.queue.submit(std::iter::once(encoder.finish()));
                end
            };

        let mut i = 0usize;
        // (UNIFY) The unified ship pass renders the WHOLE fleet through one camera
        // in a single shot the first time a loft batch is hit; later loft batches
        // are no-ops. This flag gates that.
        let mut unified_ships_done = false;
        while i < batches.len() {
            // Drain the non-loft run preceding the next loft ship.
            let next = flush_scene_run(self, &batches, i, cleared);
            if next > i {
                cleared = true;
                i = next;
                continue;
            }
            // (UNIFY) Unified path: render every loft ship as real 3-D geometry
            // through the SAME camera the grid uses (so the fleet lives in the
            // grid), then blit the whole posterized output full-screen. Done once,
            // on the first loft batch (composites over the grid, under the HUD).
            if unified_enabled() && matches!(batches[i].kind, BatchKind::LoftShip(_)) {
                if !unified_ships_done {
                    self.render_unified_fleet(&loft_quads, &mut cleared);
                    unified_ships_done = true;
                }
                i += 1;
                continue;
            }
            // `i` is a loft-ship batch. Render its 3D hull at its pose into the
            // shared loft target, then blit it onto its lane quad — each in its
            // own submitted encoder so the shared loft uniforms aren't clobbered
            // by the next ship.
            if let BatchKind::LoftShip(idx) = batches[i].kind {
                let q = loft_quads[idx as usize];
                // Look up this ship's uploaded mesh + animated pose. Skip
                // cleanly if either is absent (mesh not installed / pose not yet
                // synced) — the lane just shows no ship there rather than crash.
                let mesh = self.loft_meshes.get(&q.kind);
                // The pose only GATES the draw (created on first sight); the
                // ORIENTATION comes from q.facing_yaw_deg (hud, from ship.facing),
                // not the pose's animated yaw.
                let has_pose = self.loft_poses.contains_key(q.ship_id.as_str());
                if let (Some(mesh), true) = (mesh, has_pose) {
                    // (#70/#73) FLAT ground-plane chase-cam yaw: the hull stays FLAT
                    // on the grid (Bruce's requirement: no barrel-roll) and only its
                    // heading turns. Composes the stern-on base (270) + the tactical
                    // facing offset (`q.facing_yaw_deg`: N=0 / E=+90 / S=180 / W=−90).
                    // (#173 Bruce FINAL ruling — concludes the #170→#171→#172 lean arc)
                    // NO perspective lane-lean, ever: every ship sits at its clean
                    // CARDINAL pose, player + enemies, all facings, every column. Reason:
                    // combat is read by FIRING CAPABILITY (front/side bears on a target),
                    // and any lean tilted off-centre hulls off-cardinal, which muddied
                    // that read (an off-centre bow-on enemy looked like it was aiming).
                    // The yaw formula lives in ONE pure, CPU-tested place
                    // (`chase_cam_ground_yaw_deg`), gated by a bow test that replicates
                    // THIS ortho loft camera (not the scene-space pinhole the earlier
                    // oracle wrongly tested). `cfg` + the live loft pitch are still
                    // passed (the function keeps them in its signature) but no longer
                    // affect the pose; the cfg also still drives `loft_quads`/`q.aim_at`
                    // elsewhere, so it's built from the live scene size + grid mode.
                    let base = crate::projector::ProjectorConfig::for_scene(
                        scene_w() as f32,
                        scene_h() as f32,
                    );
                    let t = grid_pitch_t();
                    let cfg = match grid_mode() {
                        1 => base.with_stretch(t),
                        2 => base.with_stretch_straight(t),
                        3 => base.with_stretch_continuous(t),
                        _ => base.with_pitch(t),
                    };
                    let base_yaw = crate::loft_gpu::chase_cam_ground_yaw_deg(
                        q.aim_at,
                        q.facing_yaw_deg,
                        &cfg,
                        loft_pitch_deg(),
                    );

                    // 1) Render the hull into the shared loft target at the ground-
                    // yawed pose. The fixed house key light (laz -50 / lel 60)
                    // relights the yawed hull in world space automatically — no
                    // screen counter-rotation needed (the hull turns IN 3D, it
                    // doesn't spin on screen).
                    let mut enc =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("loft ship render"),
                            });
                    // (#140 ship-tilt) Render at the LIVE loft-camera pitch so the
                    // hull TILTS to stay parallel to the raising grid plane (Bruce:
                    // ships must tilt WITH the grid). At grid-pitch step 0 this is
                    // CAMERA_PITCH_DEG, so the default frame is byte-identical; as the
                    // `G` arc steps toward top-down the camera looks down the deck and
                    // the hull reads top-down. Same pitch for player + enemy lofts.
                    self.loft.render_ship_pitched(
                        &self.queue,
                        &mut enc,
                        &mesh.vbuf,
                        mesh.vcount,
                        base_yaw,
                        mesh.center_y,
                        loft_pitch_deg(),
                    );
                    self.queue.submit(std::iter::once(enc.finish()));

                    // 2) Blit the posterized output onto this ship's lane quad — UPRIGHT
                    //    (axis-aligned). (#186 Bruce, reverted the 2-D blit-roll) The roll
                    //    banked the hull off the grid plane (read as rolled up on its edge)
                    //    while enemies lay flat; Bruce wants the player FLAT on the plane
                    //    like the enemies. The lane-lean now lives in the FLAT ground yaw
                    //    (chase_cam_ground_yaw_deg's psi) so the hull stays on the deck and
                    //    only its ground heading turns toward the lane — no screen roll.
                    let qu = LoftQuadUniform {
                        p0: q.p0,
                        p1: q.p1,
                        p2: q.p2,
                        p3: q.p3,
                        // (#76 scene-res) px→NDC over the LIVE offscreen size so the
                        // loft dest-quad lands on the right cell at any scene res.
                        px_to_ndc: [2.0 / scene_w() as f32, 2.0 / scene_h() as f32],
                        _pad: [0.0, 0.0],
                    };
                    self.queue
                        .write_buffer(&self.loft_blit.quad_ubo, 0, bytemuck::bytes_of(&qu));
                    let bg = self
                        .loft_blit
                        .bind_group(&self.device, self.loft.output_view());
                    let load = if cleared {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(CLEAR)
                    };
                    let mut enc =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("loft ship blit"),
                            });
                    {
                        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("loft ship blit"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &self.offscreen_view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load,
                                    store: wgpu::StoreOp::Store,
                                },
                                depth_slice: None,
                            })],
                            depth_stencil_attachment: None,
                            timestamp_writes: None,
                            occlusion_query_set: None,
                        });
                        pass.set_pipeline(&self.loft_blit.pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.draw(0..6, 0..1);
                    }
                    self.queue.submit(std::iter::once(enc.finish()));
                    cleared = true;
                }
            }
            i += 1;
        }

        // If the whole frame had no batches at all, the offscreen was never
        // cleared — clear it once so the swap blit reads defined contents.
        if !cleared {
            let mut enc = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("clear offscreen"),
                });
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.offscreen_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.queue.submit(std::iter::once(enc.finish()));
        }

        // Headless (no surface): the offscreen now holds the fully composited
        // scene (read back by `capture_png`). There's no swapchain to blit to or
        // present, so we're done.
        let Some(surface) = self.surface.as_ref() else {
            return Ok(());
        };

        // Acquire the swapchain image. On a STALE surface (Outdated/Lost — from a
        // resize, a compositor change, or GPU/memory pressure) reconfigure and
        // re-acquire ONCE in place, rather than letting `?` propagate and the
        // caller DROP the frame: a dropped frame presents nothing (the previous
        // backbuffer / a black image), so a run of Outdated frames reads as the
        // "fade to black → snap to grey" flicker (#47). The offscreen scene is
        // already composited above, so we only needed the swap image for the
        // final blit. Only a SECOND failure (or a non-stale error) propagates.
        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                surface.configure(&self.device, &self.config);
                surface.get_current_texture()?
            }
            Err(e) => return Err(e),
        };
        let swap_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Final pass: blit offscreen → swapchain with continuous-scale
        // letterboxing, then present.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blit to swap"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit offscreen to swap"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &swap_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(LETTERBOX),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.blit.pipeline);
            pass.set_bind_group(0, &self.blit.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}

impl SpritePipeline {
    fn new(
        device: &wgpu::Device,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite shader"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_SHADER.into()),
        });

        let view_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view uniform"),
            size: std::mem::size_of::<ViewUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<QuadVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        }],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SpriteInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &[
                            wgpu::VertexAttribute {
                                shader_location: 1,
                                offset: 0,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                shader_location: 2,
                                offset: 8,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                shader_location: 3,
                                offset: 16,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                            wgpu::VertexAttribute {
                                shader_location: 4,
                                offset: 32,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                shader_location: 5,
                                offset: 40,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                shader_location: 6,
                                offset: 48,
                                format: wgpu::VertexFormat::Float32,
                            },
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let quad_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad vbuf"),
            contents: bytemuck::cast_slice(&QUAD_VERTS),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let instance_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instance vbuf"),
            size: (std::mem::size_of::<SpriteInstance>() as u64) * MAX_SPRITES,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            quad_vbuf,
            instance_vbuf,
            view_ubo,
            bind_group,
        }
    }
}

impl PolygonPipeline {
    /// Build the polygon pipeline. Shares the sprite pipeline's view uniform
    /// and atlas texture/sampler — same bind group layout, same resources,
    /// just bound from this pipeline's perspective. The vertex buffer layout
    /// describes a single `PolygonInstance` (no separate quad vertex buffer
    /// — corners come from the instance directly).
    fn new(
        device: &wgpu::Device,
        view_ubo: &wgpu::Buffer,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("polygon shader"),
            source: wgpu::ShaderSource::Wgsl(POLYGON_SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polygon bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polygon bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("polygon layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("polygon pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_poly"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PolygonInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        }, // p0
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            offset: 8,
                            format: wgpu::VertexFormat::Float32x2,
                        }, // p1
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            offset: 16,
                            format: wgpu::VertexFormat::Float32x2,
                        }, // p2
                        wgpu::VertexAttribute {
                            shader_location: 3,
                            offset: 24,
                            format: wgpu::VertexFormat::Float32x2,
                        }, // p3
                        wgpu::VertexAttribute {
                            shader_location: 4,
                            offset: 32,
                            format: wgpu::VertexFormat::Float32x4,
                        }, // color
                        wgpu::VertexAttribute {
                            shader_location: 5,
                            offset: 48,
                            format: wgpu::VertexFormat::Float32x2,
                        }, // uv_min
                        wgpu::VertexAttribute {
                            shader_location: 6,
                            offset: 56,
                            format: wgpu::VertexFormat::Float32x2,
                        }, // uv_max
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_poly"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let instance_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polygon instance vbuf"),
            size: (std::mem::size_of::<PolygonInstance>() as u64) * MAX_POLYGONS,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            instance_vbuf,
            bind_group,
        }
    }
}

impl TexturedShipPipeline {
    fn new(device: &wgpu::Device, view_ubo: &wgpu::Buffer) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("textured-ship shader"),
            source: wgpu::ShaderSource::Wgsl(TEXTURED_SHIP_SHADER.into()),
        });

        let view_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ship view bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let view_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ship view bg"),
            layout: &view_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: view_ubo.as_entire_binding(),
            }],
        });

        let ship_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ship per-instance bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ship texture sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ship layout"),
            bind_group_layouts: &[&view_bgl, &ship_bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ship pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_ship"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    // Instance stride matches the four corner positions
                    // packed at the head of TexturedShipInstance.
                    array_stride: 32, // 4 × Float32x2
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            shader_location: 0,
                            offset: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 1,
                            offset: 8,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 2,
                            offset: 16,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            shader_location: 3,
                            offset: 24,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ship"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // Per-frame instance buffer: one slot per ship that may draw.
        let instance_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ship instance vbuf"),
            // Each instance is 4 × Float32x2 = 32 bytes (matches the
            // vertex layout — the slug bytes and blend_t in
            // TexturedShipInstance are CPU-side only and don't go in
            // the vbuf).
            size: 32 * MAX_TEXTURED_SHIPS as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut blend_ubos = Vec::with_capacity(MAX_TEXTURED_SHIPS);
        for i in 0..MAX_TEXTURED_SHIPS {
            blend_ubos.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("ship blend ubo {i}")),
                size: std::mem::size_of::<BlendUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        Self {
            pipeline,
            instance_vbuf,
            view_bg,
            ship_bgl,
            sampler,
            blend_ubos,
        }
    }
}

impl BlitPipeline {
    fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        src_view: &wgpu::TextureView,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });

        let ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blit uniform"),
            size: std::mem::size_of::<BlitUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // NEAREST for the final canvas→window blit (v2 decision #1). The blit
        // is now a FIXED ×4 INTEGER scale (see `update_blit_uniform`): 480×270
        // → 1920×1080, so every offscreen texel maps to an exact 4×4 block of
        // window pixels with no fractional coverage — nearest is crisp and
        // shimmer-free, the intended pixel-art look. (At the integer-floor
        // fallback scale used on sub-1080p windows the mapping stays integer,
        // so nearest is still exact.)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_blit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blit"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            ubo,
            bind_group,
            bgl,
            sampler,
        }
    }

    /// (#76 scene-res) Repoint the blit at a NEW offscreen view (after a scene-res
    /// cycle recreates the offscreen texture). Rebuilds the bind group binding the
    /// SAME ubo + sampler to the new view; the pipeline + layout are unchanged.
    fn rebind(&mut self, device: &wgpu::Device, src_view: &wgpu::TextureView) {
        self.bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
    }
}

impl LoftShipBlit {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("loft-ship blit shader"),
            source: wgpu::ShaderSource::Wgsl(LOFT_SHIP_SHADER.into()),
        });

        let quad_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("loft-ship quad ubo"),
            size: std::mem::size_of::<LoftQuadUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // NEAREST sampler — matches the POC's post_sampler (loft_poc.rs). The
        // loft output is already posterized pixel art; sampling it Linear blends
        // the bands into fuzz and kills the pixel-art read (the #37 fuzziness
        // bug). Nearest keeps every posterized texel crisp into the lane. The
        // non-integer-scale shimmer Nearest can give is the lesser evil — the
        // POC reaches the screen via Nearest and that's the approved look.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("loft-ship nearest sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("loft-ship bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Non-filterable: the loft output is sampled Nearest
                        // (pixel-art crisp), matching the loft post pass.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("loft-ship layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("loft-ship pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_loft"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_loft"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            quad_ubo,
            bgl,
            sampler,
        }
    }

    /// Build the per-draw bind group for one loft-ship blit: the quad uniform +
    /// the loft output texture view + the linear sampler. Cheap (one bind group
    /// per ship per frame); fine at ≤9 ships.
    fn bind_group(&self, device: &wgpu::Device, ship_view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("loft-ship bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.quad_ubo.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(ship_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }
}
