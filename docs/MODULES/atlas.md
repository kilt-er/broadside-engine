# `src/atlas.rs` — Module Companion

*A self-contained walkthrough of the procedural sprite atlas. The same content as
the [`atlas.rs` section of `LINE_BY_LINE.md`](../LINE_BY_LINE.md#srcatlasrs), but
scoped: this file assumes you only care about how the atlas is laid out, why it
looks the way it does, and how to add a new glyph. Read this if you are about to
add an art asset, change a palette color, or move a cell.*

**Source commit:** stabilized through Slice C / D. 818 lines, 7 inline tests, all
green.
**Mirrors:** No TS analog. The TS engine is headless; sprite atlases are a Rust-
port concern from day one. This module has **no Drift section** by design.
**Design anchor:** Task #28 (Slice C — flesh out atlas with ship faces, chevron,
ordnance, HUD glyphs, parallax art). Cell-slot layout documented canonically in
[`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md) § "Atlas slot allocation."

---

## Why this file exists

Broadside's renderer needs every HUD glyph, projectile sprite, status badge,
parallax layer tile, and tinted-quad source available *in a single GPU texture
binding*. Switching texture bindings between draws would kill batching; carrying
each glyph as a separate texture would multiply bind-group rebuilds.

`atlas.rs` packs all of them into one 256×256 RGBA8 texture, **generated procedurally
at startup** (no PNG asset on disk, no atlas-versioning story for art tweaks). The
sprite, polygon, and textured-ship pipelines in `gfx.rs` all sample from this one
texture; per-cell selection happens at the UV level via `cell_uvs(col, row)`.

Three things to know up front:

1. **8×8 grid of 32×32 cells = 256×256 RGBA8 total.** One texture binding for the
   entire HUD/parallax/glyph surface. The full slot map is in
   [SPRITE_SPEC.md](../SPRITE_SPEC.md); this companion explains *why* the layout
   looks that way, not *what's in each slot*.
2. **The atlas is decorative.** Ship hulls are drawn as tinted procedural
   polygons by `hud.rs` (using the `SOLID_WHITE` cell at (7, 7) as the texture
   source + per-instance color tint). The atlas does not carry ship-class art.
3. **No TS analog, no Drift section.** Atlas is a Broadside-port concern only.
   The absence is deliberate, not an oversight.

---

## The three design decisions

### Why 8×8 grid (one texture binding)

The sprite, polygon, and textured-ship pipelines all sample `group 0, binding 1`
in `gfx.rs`. If glyphs lived in separate textures, the renderer would have to
either swap bind groups per draw (kills batching) or maintain a per-glyph bind
group (proliferates GPU state). Packing everything into one 256×256 means
**zero per-cell texture switches** — the only pipeline rebinds happen at the
DrawCommand variant boundary.

### Why 32×32 cells

At the engine's 1320×480 virtual resolution, a 32px cell is ~2.5% of the canvas
width — about the right size for a status badge or queue glyph at native scale.
Smaller would lose recognisability; larger would waste the grid (fewer slots
available, and HUD elements would dominate the screen).

### Why procedural, not baked

`generate_atlas()` runs once at startup and produces the texture in memory. No
PNG asset file in the repo, no asset-versioning story for art tweaks:

- Change a `draw_*` function → rebuild → new atlas ships with the binary.
- No "art commit + code commit" coordination dance.
- Deterministic byte-for-byte: the same build always produces the same atlas,
  which matters for visual regression testing.

Bruce's hand-painted ship sprites are the exception — they *are* PNG assets,
loaded by [`sprites.rs`](../LINE_BY_LINE.md#srcspritesrs) (still pending), and
live outside this module.

---

## Constants

| Constant         | Value | Role                                                    |
|------------------|------:|---------------------------------------------------------|
| `ATLAS_SIZE`     | `256` | Side length of the RGBA8 texture in pixels.             |
| `CELL_SIZE`      | `32`  | Side length of one cell in pixels.                      |
| `CELLS_PER_ROW`  | `8`   | Derived: `ATLAS_SIZE / CELL_SIZE`.                      |

All three are public so `gfx.rs::Gfx::new` can use them when sizing the GPU
texture and computing bytes-per-row for the upload.

---

## Cell layout (summary)

The full table lives in [SPRITE_SPEC.md § Atlas slot allocation](../SPRITE_SPEC.md).
Quick reference:

| Row | Content                                                                |
|-----|------------------------------------------------------------------------|
| 0   | Projectiles + chevron (3 cells).                                       |
| 1   | Action-queue glyphs — one per `WeaponArchetype` (7 cells).             |
| 2   | Telegraph intent icons (6 cells).                                      |
| 3   | Status badges (4 cells).                                               |
| 4   | Parallax layer art (5 cells).                                          |
| 5–6 | **Reserved** for future ship-class detail / decals.                    |
| 7   | `SOLID_WHITE` at (7, 7) — the flat-color tint source.                  |

26 named cells in use today; 38 free slots in reserve rows 5–6 plus the unused
columns at the end of each used row.

---

## How a glyph gets onto the screen

```
   1. atlas.rs::generate_atlas() runs at startup
      └─► returns Vec<u8>, 256×256×4 = 262144 bytes
   2. gfx.rs::Gfx::new uploads it to a GPU texture (one queue.write_texture call)
   3. gfx.rs builds the sprite pipeline's bind group with the atlas texture view
   4. hud.rs::push_* emits a DrawCommand::Sprite with
      uv_min/uv_max derived from atlas::cell_uvs(GLYPH_X)
   5. Gfx::render binds the sprite pipeline, draws with those UVs
   6. Fragment shader samples the atlas at the interpolated UV,
      multiplies by the instance color tint, writes to the offscreen
```

The atlas is **uploaded once and never re-uploaded**. Mutations only happen at
build time (change `draw_*` source → rebuild).

---

## Generation flow

`generate_atlas() -> Vec<u8>` (line 83) does this in order:

```
   buf = vec![0u8; 256*256*4]                          // start transparent
   fill_cell(buf, SOLID_WHITE, [255,255,255,255])      // FIRST, defensive
   draw_bow_chevron(buf, ...);  draw_torpedo(buf, ...);  draw_missile(buf, ...)
   draw_glyph_*(buf, ...)        × 7  // action-queue archetypes
   draw_telegraph_*(buf, ...)    × 6
   draw_status_*(buf, ...)       × 4
   draw_parallax_*(buf, ...)     × 5
   return buf
```

**`SOLID_WHITE` is filled first** with explicit comment in the source: *"so every
tinted-quad path works even if the rest of the atlas hasn't run yet."* If any
subsequent `draw_*` panicked, the heat bars / range ticks / lane plate would still
render correctly via the SOLID_WHITE fallback path.

---

## The palette

10 RGBA constants near the top of the file, transcribed from the analysis HTML's
CSS tokens. The seven archetype colors (`C_BEAM`, `C_ORD`, `C_BROAD`, `C_DISP`,
`C_CTRL`, `C_MOVE`, `C_DEF`) map **1:1 to the design HTML's weapon-card
color-coding** — so the renderer's queue-glyph row reads exactly like the
designer's archetype legend. `GOLD`, `VERMILLION`, `PAPER_DIM` are the brand
neutrals.

---

## Adding a new cell

Three steps. The test suite enforces all three:

1. **Add a constant** in the cell-map section: `pub const GLYPH_X: (u32, u32) =
   (col, row);` with `col < 8` and `row < 8`. The
   `every_cell_inside_atlas_bounds` test catches out-of-bounds; the
   `named_cells_are_distinct` test catches collisions with existing cells.
2. **Write a `draw_*` function** that puts pixels into the named cell.
3. **Call it from `generate_atlas()`** (the `draw_*` row block). The
   `every_cell_has_some_content` test scans your cell for at least one opaque
   pixel; forgetting step 3 trips it.

Then update [`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md) § "Atlas slot allocation"
to match. The SPRITE_SPEC table is the canonical reader-facing reference; this
file's constants are the canonical machine-readable source.

---

## The starfield is *not* LCG-driven

The two starfield cells (`draw_parallax_far_stars` and `draw_parallax_mid_stars`)
use **hardcoded `(x, y)` coordinate arrays**, not a seeded LCG / Wang-hash.
Determinism comes from the array literals themselves; visual variety comes from
a hand-tuned scatter. Brightness tints alternate via `i % 4 == 0` (far) and
`i % 3 == 0` (mid).

If a future change needs more randomness — e.g. larger starfields with thousands
of stars — an LCG seeded by a build-time constant would be the natural
replacement. The current 12-star-per-cell approach is small enough that
hardcoding the positions is cleaner than a procedural scatter.

---

## Tests

7 inline tests at `atlas.rs:692–818`:

```
cell_uvs_at_origin_is_unit_cell
cell_uvs_at_corner_is_inside_unit_square
generate_atlas_sized_correctly
solid_white_cell_is_white
every_cell_inside_atlas_bounds      ← bounds drift guard
named_cells_are_distinct            ← collision drift guard
every_cell_has_some_content         ← forgotten-draw_* drift guard
```

The three "drift guard" tests are the structural safety net for the add-a-cell
workflow above. Together they catch:

- Adding a constant outside the 8×8 grid.
- Adding a constant that collides with an existing slot.
- Adding a constant + `draw_*` function but forgetting to wire the call in
  `generate_atlas`.

The first four tests verify the UV math and the `SOLID_WHITE`-is-actually-white
contract.

---

## Cross-references

- **Atlas consumer:** [`src/gfx.rs`](gfx.md). The sprite + polygon pipelines bind
  the atlas texture; `Gfx::new` uploads it once at startup.
- **HUD producer:** [`src/hud.rs`](hud.md) (pending). Every `push_*` helper that
  emits a non-flat-color sprite picks a cell via the `atlas::*` constants and
  calls `atlas::cell_uvs(cell)` to get the UV rect.
- **Slot allocation:** [`docs/SPRITE_SPEC.md`](../SPRITE_SPEC.md) § "Atlas slot
  allocation" — the canonical layout reference.
- **Design source:** Task #28 (Slice C — flesh out atlas with ship faces,
  chevron, ordnance, HUD glyphs, parallax art).
