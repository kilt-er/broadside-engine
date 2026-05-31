# `src/sprites.rs` — PNG sprite loading + handle lookup

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/sprites.rs`](../LINE_BY_LINE.md#srcspritesrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

The renderer can draw a ship two ways: as a **procedural silhouette** (built by
[`atlas.rs`](atlas.md)) or as a **hand-painted PNG sprite**. This module is the
loader for the PNG path: it finds `assets/sprites/<class>_<stance>_<view>.png`,
decodes it to RGBA8, and hands the buffer to [`gfx.rs`](gfx.md) for GPU upload.
Crucially, **every loader returns `None` rather than panicking** when a PNG is
missing or undecodable — the renderer simply falls back to the procedural
silhouette, so the demo runs with zero, some, or all sprites present.

It also owns two image transforms (`mirror_horizontal`, `rotate_90_cw`) that
**derive** missing sprite variants from ones the artist did paint — the #85/#86
fallback chains — plus the `SpriteRegistry` trait that lets `hud::compose_scene`
ask "is this ship's sprite uploaded?" without touching the GPU directly.

No TS analog — the TS engine had no sprite pipeline. This is Rust/render-only.

### Filename convention (mirrors `docs/SPRITE_SPEC.md`)

`assets/sprites/<class>_<stance>_<view>.png` where `class ∈ {frigate, scout,
gunboat, aegis, …}`, `stance ∈ {bowOnFore, bowOnAft, broadside}`, `view ∈
{side, top}`. Example: `frigate_broadside_side.png`.

---

## `enum SpriteView` + `enum SpriteStance` (src/sprites.rs:22, 40)

Two small enums naming the two axes of a sprite. `SpriteView` is `Side` (the 0°
silhouette) or `Top` (the 90° silhouette) — the renderer blends between them as
the camera-angle scrubber moves. `SpriteStance` is `BowOnFore` / `BowOnAft` /
`Broadside` — the hull orientation the art was painted for. Each has a
`slug(self) -> &'static str` returning the lowercase filename token
(`"side"`/`"top"`, `"bowOnFore"`/`"bowOnAft"`/`"broadside"`).

---

## `struct SpriteImage` (src/sprites.rs:58)

A decoded sprite ready for upload: `width`, `height`, and `rgba: Vec<u8>`
(RGBA8, top-row first). This is the common currency every function here produces
or consumes.

---

## `fn load_sprite(asset_dir, class, stance, view) -> Option<SpriteImage>` (src/sprites.rs:68)

**Intent:** Try to load one PNG. **Never panics.** Line 74: build the path via
`sprite_path`. Line 75-81: `image::open`; on any error, log at `debug` and return
`None` (the render fallback handles it). Line 82-87: convert to RGBA8 and wrap in
a `SpriteImage`.

**Cross-references:** Called by `load_sprite_pair` and by
`gfx::Gfx::try_load_ship_sprites`. **Worked example:**
`load_sprite_returns_none_for_missing_file` (src/sprites.rs:248) — a nonexistent
root yields `None`, no panic.

## `fn sprite_path(asset_dir, class, stance, view) -> PathBuf` (src/sprites.rs:93)

Builds `<asset_dir>/sprites/<class>_<stance-slug>_<view-slug>.png`. Public so the
binary can log "looking for X" diagnostics. Pinned by
`sprite_path_format_matches_spec` (src/sprites.rs:222) and
`sprite_path_uses_stance_and_view_slugs` (src/sprites.rs:236).

## `fn load_sprite_pair(asset_dir, class, stance) -> (Option, Option)` (src/sprites.rs:110)

Loads both views (side + top) for one stance. Either or both may be `None` if the
artist hasn't painted that face — the renderer blends whatever is available.
`load_sprite_pair_is_resilient_to_partial_assets` (src/sprites.rs:385) pins
`(None, None)` for a missing root.

---

## `fn mirror_horizontal(src: &SpriteImage) -> SpriteImage` (src/sprites.rs:130)

**Intent:** Horizontally flip a sprite (reverse each row's pixel order, rows
themselves stay put). This is the **#85 fallback**: derive a `bowOnAft` sprite
from a `bowOnFore` one when the artist hasn't painted the aft variant — bow-on
ships are visually symmetric across the fore/aft flip, so the mirror is faithful.
An explicit `bowOnAft_<view>.png` always takes precedence; the loader only calls
this when that file is missing.

Line 134-142: for each row, walk pixels in reverse (`(0..w).rev()`), copying each
4-byte RGBA pixel. The row index is preserved so the image flips left-right, not
top-bottom.

**Worked examples:** `mirror_horizontal_flips_pixel_order_within_each_row`
(src/sprites.rs:260) and `mirror_horizontal_preserves_rows` (src/sprites.rs:280)
pin the flip; `mirror_horizontal_double_flip_is_identity` (src/sprites.rs:370)
proves `mirror(mirror(x)) == x`.

---

## `fn rotate_90_cw(src: &SpriteImage) -> SpriteImage` (src/sprites.rs:170)

**Intent:** Rotate a sprite 90° clockwise; output dimensions swap (`width` ↔
`height`). This is the **#86 fallback**: step 2 of `gfx::try_load_ship_sprites`'s
`broadside_top` chain — explicit `broadside_top.png` → `rotate90(bowOnFore_top)`
→ procedural. (Note `broadside_side` has **no** auto-derivation — it's a
front-face view of beam × height that can't be reconstructed from a bow-on side
or top.)

Line 171-175: source dims `sw`/`sh`, destination dims swapped (`dw = sh`,
`dh = sw`), allocate the output buffer. Line 176-184: for each source pixel
`(sx, sy)`, the y-down 90°-CW mapping is `dst.x = dw - 1 - sy`, `dst.y = sx`;
copy the 4-byte pixel. The docstring (src/sprites.rs:153-163) notes the absolute
handedness isn't visually load-bearing because the renderer's broadside chevron
overlay reads bow direction explicitly; `_cw` was chosen to match
`image::imageops::rotate90`'s conventions.

**Worked examples:** `rotate_90_cw_swaps_dimensions` (src/sprites.rs:299),
`rotate_90_cw_maps_top_left_to_top_right` (src/sprites.rs:312, the exact pixel
mapping), `rotate_90_cw_four_times_is_identity` (src/sprites.rs:337, the property
test), and `rotate_90_cw_on_frigate_top_dimensions_match_sprite_spec`
(src/sprites.rs:355, 120×60 → 60×120 per SPRITE_SPEC).

---

## `trait SpriteRegistry` + `struct EmptySpriteRegistry` (src/sprites.rs:197, 208)

**Intent:** A read-only "which sprites are uploaded?" query.
`hud::compose_scene` calls `has(class, stance, view)` to decide whether to emit a
**textured** or **procedural** silhouette per ship; `Gfx` implements it over its
own texture registry. `has_pair` is a default method ("both views present").
`EmptySpriteRegistry` returns `false` for everything — used by tests and by
`compose_scene` callers with no GPU registry (so they always get procedural).

**Cross-references:** Implemented by [`gfx::Gfx`](gfx.md); consumed by
[`hud::compose_scene`](hud.md). The split keeps `hud` GPU-agnostic — it asks the
registry trait, not wgpu.
