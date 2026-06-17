# Broadside — Render Contract

**Version 5** · 2026-06-17 · supersedes v4 (sprite-sheet export), v3 (facing-sign + group order), v2 (2D / 15-facing), v1 (1D lane)

**Purpose:** a single source of truth for how the *Loft Editor* (the Three.js sprite tool,
`broadside-loft-editor.html`) relates to the *runtime renderer* (Rust + wgpu). It defines what the
editor bakes into sprites versus what the game engine is responsible for, so the two never fight each
other and the in-game ships look like the editor preview.

> One-line summary: **the editor is a sprite bakery, not a runtime renderer.** Everything it does
> collapses into flat PNG pixels at export. The Rust engine consumes those pixels and must not
> re-derive lighting/shading from them.

---

## 0. Version history

**v5 — 2026-06-17 (current).** Added a **second export channel: a GLB mesh** for runtime-dynamic
lighting. The editor's new **⤓ Export GLB** button (`buildShipGLB` / `window.__exportShipGLB`, editor
build 6.52) emits the live ship (hull + engines + parts) as a glTF-binary the Rust/wgpu engine imports
and **lights live** — the §4 "dynamic-lighting escape hatch", now a first-class output rather than a
hypothetical. This is per `BROADSIDE_REALTIME_RENDER_SPEC.md` (the engine team's ingest spec). Format:
TRIANGLES only, axes raw (+X length / +Y up / +Z starboard), transforms baked into vertex positions,
**one primitive per material** (albedo as linear `baseColorFactor`, `emissiveFactor` for glow,
`KHR_materials_unlit` on pure-light parts), no vertex colors, centered + scaled so **X-length == 12**
(engine default), and the §3 look-params in `scene.extras` (`lights`/`lightSym`/`bands`/`shadeModel`/
`outlineWidth`+`outlineCorner`/`grade`) plus `laz`/`lel` for today's single-key engine. Full format in §5
("Export format — the GLB mesh"). **The sprite-sheet path (v4) stays** as preview/fallback and is
unchanged — nothing about the bake, camera, facings, or sprite export was removed. The runtime *direction*
is that the mesh path becomes the primary in-game renderer (meshes lit dynamically), with baked sprites as
the preview/fallback; the mesh format itself matches the proven CAD-editor GLB exporter, so the engine
ingests both tools' GLBs identically.

**v4 — 2026-06-16.** Pinned the **export format**: the editor now emits the 15 facings as one
packed PNG **sprite sheet + a JSON sidecar** (`buildShipSheet`, editor build 6.51). This resolves the two
rows that v3 left open: (a) **sprite sizing** — frames are exported **pre-normalized to one consistent
scale** (R-corrected to the forward-centre lane), so the runtime draws every facing at a single scale
factor and broadside is never re-shrunk; (b) **pivot** — every facing carries its trim rect plus a pivot
(the ship's projected local origin, in sheet pixels). Grid is lanes × rotation-groups in `[left, forward,
right]` order. Full schema in §5 ("Export format"). No change to the camera, facings, or bake/runtime
split.

**v3 — 2026-06-16.** Pinned the facing **sign convention** to what the editor's renderer
actually produces on screen — **+90° yaw = nose to screen-left**, −90° = screen-right (a v2 draft had
these reversed). Brought the editor's FACING toggle into line, and documented the **baked group /
atlas order** as `[left, forward, right]`, flat index `facingIndex(p, ri) = ri·5 + p` (forward =
sprites 5–9). No change to the camera, the 15-facing model, or the bake/runtime split. See §5.

**v2 — 2026-06-16.** Rewrote for the **2D board** (the original assumed a single 1D lane). The spine
(§1–§4: bake/runtime split, don't-double-light, dynamic-lighting escape hatch) carried over unchanged.
What changed:
- **4 stances → 15 facings.** The old `right / left / fore / aft` model was a 1D-lane artifact; the
  new set is the Background viewer's **5 lane positions × {forward, +90°, −90°} = 15** (§5).
- **Camera is identical.** All 15 are shot through the same pitch-20° camera; only hull yaw moves —
  existing baked sprites' viewing angle stays valid, you just bake 15 yaws instead of 5.
- **Visual facing and tactical stance decouple** (§5 callout): bow-on vs broadside — and which shield
  arc gets hit — is no longer a baked property, it's a runtime geometry calc.
- **Two new shared conventions:** a sprite **pivot/anchor** and a **depth-sort** rule (§5).
- The engine *does* need a change here — the facing→sprite lookup grows from a 4-entry map to a
  15-entry wheel + atlas layout (§2).

**v1.** Original contract for the single-lane (1D) board: four stances (right / left / fore / aft),
the bake-vs-runtime split, and the don't-double-light rule.

---

## Contract Surface — what to re-check on every editor change

A change to any **editor knob** on the left changes the **contract clause** on the right: bump the
version (§0) and update that clause. Anything *not* on this list is editor-internal and needs no
contract change.

| Editor knob (identifier / location) | Governs (contract clause) |
|---|---|
| `POSITIONS` — lane count (=5) | facings = lanes × rotations; atlas size; the N-entry facing wheel (§2, §5) |
| `SHIP_ROTS` (=`[90, 0, -90]`) | rotation groups, **facing sign** (+90°=screen-left), group order `[left, fwd, right]` (§5) |
| `FWD_ROT` (=1) | which group is "forward" / the player's default facing (§5) |
| `SHIP_TURN` — nose turn per lane (=15°) | the per-lane **yaw table** (§5) |
| `laneYaw(p)` / `facingYaw(p,ri)` | exact yaw of each facing (§5 table) |
| `facingIndex(p,ri) = ri·POSITIONS + p` | flat **atlas index** / group order (§5) |
| `SHIP_PERSP` {pitch 20°, fov 34°} + `D = R/tan(fov/2)·0.78` | the **camera spec** & framing (§5) |
| sprite **pivot / anchor** point | per-facing pivot = projected ship origin, in sheet px — **pinned v4** (§5 Export format) |
| **bake-export** layout (cols×rows, filename scheme, pivot sidecar) | packed sheet + JSON sidecar — **pinned v4** (§5 Export format) |
| **sprite sizing / normalization** (per-facing scale ∝ bounding-sphere R) | frames exported at one consistent scale — **pinned v4** (§5 Export format) |
| **GLB mesh export** (`buildShipGLB` / `__exportShipGLB`, ⤓ Export GLB) | the runtime-lit mesh format: axes/baked-transforms, 1 primitive per material, albedo+emissive+unlit, X-length 12, `scene.extras` look-params — **pinned v5** (§5 Export format — GLB) |

**Maintenance rule.** Every build, diff the change against this table. If a knob moved, bump the
version and edit the clause. If nothing on the table moved, that is a deliberate "checked, n/a" —
record it in the build summary so it reads as a decision, not an omission.

**Not on the contract surface** (editor-internal — change freely, no contract impact): posterize
palette & color grade, light-rig azimuth/elevation/intensity, shading model & normals mode, outline
weight/corner style, tab & UI layout, localStorage persistence, and the raw `R` value stored in the
bake JSON (the editor uses it for its own scene preview; the engine receives pre-normalized sprites
instead — see the sizing row).

> **Resolved in v4 (editor build 6.51).** The sizing/pivot/packaging rows above are now pinned — the
> editor exports the 15 facings **pre-normalized to one consistent scale** (R-corrected at export) as a
> packed sheet + JSON sidecar carrying per-facing rect and pivot. The runtime therefore draws every
> facing at a single scale factor and must still **not** re-normalize per sprite (that would reintroduce
> the broadside-shrink fixed in 6.50). Full schema in §5 ("Export format").

> **Added in v5 (editor build 6.52).** A **second, parallel export channel** — **⤓ Export GLB** — emits
> the live ship as a runtime-lit **mesh** (glTF-binary) instead of baked pixels. It is governed by its own
> row above and its own format spec in §5 ("Export format — the GLB mesh"). The two channels coexist:
> the **sprite sheet** is the preview/fallback look (fully baked, unlit at runtime), the **GLB** is the
> dynamically-lit path (albedo + look-params, the engine lights it). Editing the light rig, shading model,
> outline, palette, or grade still does **not** bump the contract for the *sprite* channel — but those
> same knobs now travel to the engine inside the GLB's `scene.extras`, so the GLB carries them as data
> (not as a contract clause). Changing the **GLB format itself** (axes, material mapping, extras schema,
> the 12-unit scale) is what bumps the contract — see the new surface row.

---

## 1. The two worlds

### Bake-time (the Loft Editor)
Runs in Three.js. Produces flat PNG sprites — **15 facing sprites per ship/part** (§5). All of the
following are *frozen into the pixels* at export and are invisible to the engine afterward:

- 3-light rig (port / starboard / hull): azimuth, elevation, per-light intensity, global-intensity toggle
- Surface **normals** mode: flat / smooth / crease(angle) / hard-edge(topology)
- Surface **shading model**: Lambert / Phong / Toon / Matcap / Standard(PBR) / Basic
- **Outline** pass (black "comic ink" silhouette) + outline weight + corner style (sharp / round)
- Posterize **palette bands** + color grade (hue / sat / bri / con / gam)
- Section vertices, nose taper, stretch/scale, background (background is NOT exported — sprites are
  transparent)

### Runtime (Rust / wgpu)
Consumes the baked PNGs. Operates on already-final pixels. Responsible for anything *dynamic* the
sprite can't contain:

- Sprite atlas / texture loading, nearest-neighbor sampling, pixel-perfect upscale
- **Facing selection** (which of the 15 baked facings to draw) from the hull's board orientation
- **Depth sort** of all on-board sprites (painter's order by board Y)
- Runtime effects layered ON TOP of the sprite: damage flashes, tint/palette swaps, additive glows
  for ordnance & heat, shield shimmer, selection highlights
- Compositing, draw order, camera, UI

---

## 2. Do I have to change the Rust renderer when I change the editor?

**No** — for *look* changes (light, shading model, outline, palette). Those only change the
*exported pixels*. Re-export the sprite; the engine needs zero code changes. Treat the editor like
Blender or Aseprite: you don't rewrite your renderer because you moved a light — you re-export.

**Yes** — for the 1D → 2D move itself, and for anything in §4. The dimensional change is structural,
not cosmetic: the engine's facing→sprite lookup goes from a 4-entry map to a **15-entry facing wheel**
(5 lanes × 3 hull rotations), plus the atlas layout for 15 frames and the depth-sort pass. That code
changes once; after that, re-baking the *look* is still free.

---

## 3. The "don't double-light" rule  ← most important gotcha

The editor bakes lighting INTO the sprite. Therefore, at runtime, sprites that were lit in the
editor must be drawn **unlit / neutral** by the engine. If the Rust renderer applies its own scene
lighting on top of an already-lit sprite, the result is muddy and wrong (light × light).

- Baked-look sprites → render with a flat/unlit shader (just sample the texture, optionally tint).
- Do NOT feed baked sprites into a dynamic lighting pass.

**Never rotate a hull sprite in-engine (new, and load-bearing in 2D).** Each of the 15 facings is
baked with the lights fixed in *world* space — the editor re-renders the hull at each yaw against the
same scene lights — so every facing reads as lit from the same screen direction. If you instead took
one sprite and rotated it in the engine to make a facing, the baked highlights/shadows would spin with
the hull (a ship turning right would look lit from below). So: the engine **swaps to the pre-lit
facing**; it never rotates the pixels.

---

## 4. Runtime-dynamic lighting — now a real export (the GLB path)

**As of v5 this is no longer hypothetical.** The editor's **⤓ Export GLB** emits a mesh the engine
lights live (format in §5, "Export format — the GLB mesh"); the runtime *direction* is that this mesh
path becomes the primary in-game renderer, with the baked sprite sheet kept as preview/fallback. This
section's conventions are the contract for that path.

If a ship is drawn from its **GLB** (not its sprite), the engine — not the editor — does the lighting.
The two systems must then agree on conventions:

- **Light direction convention** — match the editor's azimuth/elevation basis (see §5). The GLB carries
  the editor's light rig in `scene.extras.lights` (+ `laz`/`lel` for the current single-key engine) so
  the in-engine key light can match the editor preview.
- **Palette / grade** — `scene.extras.grade` (hue/sat/bri/con/gam) and `bands` ride along so the runtime
  can land tints on the same limited palette.
- **Albedo, not shaded pixels** — the GLB stores flat **albedo** per material (`baseColorFactor`, linear),
  `emissiveFactor` for glow, and `KHR_materials_unlit` on pure-light parts. The engine recomputes flat
  per-face normals and applies its own toon/outline pass (Tier-1.5) using the extras.

For the **sprite** path, the older guidance still holds: if you ever light *sprites* at runtime instead,
bake them flat and neutral first (Basic/Lambert, single flat light, no baked directional shading, no
outline) and reconcile the same light-direction + palette conventions. With 15 baked facings that is
rarely worth it — the GLB is the cleaner route to dynamic lighting.

---

## 5. Shared conventions (these DO cross the boundary)

The editor has not changed the axis mapping or the light basis. The orientation set is rewritten, and
two conventions are added.

### Axis mapping (hull local space) — unchanged
- **X = length** (fore/aft). Prow toward **+X**.
- **Y = height** (up/down).
- **Z = beam** (port/starboard). Starboard = **+Z**, port = **−Z**.

### Light azimuth basis (only relevant if lighting at runtime, §4) — unchanged
- Azimuth rotates in the **X–Z plane**; **az = 0 points toward +Z**.
- Light position = `(cos(el)·sin(az), sin(el), cos(el)·cos(az)) · r`.
- Port/starboard symmetry = azimuths mirrored across the **fore-aft (X) plane**
  (e.g. starboard az 45°, port az 135° — same X, same Y, opposite Z).

### Facings — the 15 baked orientations  (replaces "the 4 stances")

The set is **5 lane positions × 3 hull rotations = 15**.

**The 5 lane positions** come straight from the Background viewer's ship bake (the "AIM EACH LANE,
THEN BAKE" control). The heading for lane `p`:

```
laneYaw(p) = BASE_YAW + (p − SHIP_CENTER) · NOSE_TURN      p ∈ {0,1,2,3,4},  SHIP_CENTER = 2
```

At the shipped defaults `BASE_YAW = 0°`, `NOSE_TURN = 15°`:

| lane p | 0   | 1   | 2 (centre) | 3   | 4   |
|--------|-----|-----|------------|-----|-----|
| yaw    | −30°| −15°| 0°         | +15°| +30°|

All five are baked at **PITCH 20°** — the fixed game-camera tilt. The centre lane (p = 2) never moves
when NOSE_TURN changes.

**The 3 hull rotations** offset the *whole* 5-lane fan by a board-facing. The yaw sign is fixed by
what the editor's renderer actually produces on screen: **+90° yaw points the nose to screen-left**,
−90° to screen-right (verified in-tool).

```
facingYaw(p, r) = laneYaw(p) + r·90      r ∈ {+1 (left), 0 (forward), −1 (right)}
```

| group        | yaws                                  |
|--------------|---------------------------------------|
| left  (+90°) |  60,  75,  90, 105, 120               |
| forward (0°) | −30, −15, 0, +15, +30                 |
| right (−90°) | −120, −105, −90, −75, −60             |

This is **three discrete 60°-wide fans** (30° gaps between them), not a continuous 360° wheel — the
hull snaps to forward / left / right, and the lane indexes the aim within that facing. **No 180°
(backward) facing.**

Pitch is identical for all 15 — only hull yaw changes — so the existing bake camera is reused.

**Baked group / atlas order is `[left, forward, right]`** — flat index `facingIndex(p, ri) = ri·5 + p`
with `ri = 0` left, `1` forward, `2` right. So left = sprites 0–4, **forward = 5–9**, right = 10–14.

**Engine mapping:** `(hull facing → group) + (target/aim lane → index within group)` selects one of
the 15. **Mirror option:** the left fan is the right fan mirrored across screen-X (yaw negation), so
you may bake **10 and mirror to 15** to save atlas space — *only* if the baked lighting is
left/right-symmetric (a side key light breaks this; a top/fore light keeps it).

### Export format — the sprite sheet + sidecar  (pinned v4)
The editor's **⤓ Export sheet** button (`buildShipSheet`) writes two files:

**`broadside_ship_sheet.png`** — one packed sheet, transparent background, nearest-neighbor pixels at
the bake's native buffer resolution. Uniform grid: **cols = lanes (5), rows = rotation groups (3)**, row
order **`[left, forward, right]`** (= `SHIP_ROTS` sign order, +90 / 0 / −90). Cell = the largest facing +
a 2 px gutter; each facing is centered in its cell. All 15 frames are rescaled by `R_i / R_ref`
(R_ref = forward-centre lane) so they share **one consistent scale** — the broadside frame is genuinely
larger, and that is correct; do not re-normalize.

**`broadside_ship_sheet.json`** — the sidecar:
```
{
  "format":"broadside-ship-sheet", "version":1,
  "camera":{ "pitch_deg":20, "fov_deg":34, "framing":"D = R/tan(fov/2)*0.78" },
  "nose_turn_deg":15, "facing_sign":"+yaw = nose to screen-left",
  "group_order":["left","forward","right"],
  "grid":{ "cols":5, "rows":3, "cell_w":W, "cell_h":H, "pad":2 },
  "count":15,
  "facings":[
    { "index":7, "group":"forward", "lane":2, "yaw_deg":0,
      "cell":{ "col":2, "row":1 },
      "rect":{ "x":.., "y":.., "w":.., "h":.. },   // the frame's pixels within the sheet
      "pivot":{ "x":.., "y":.. } },                // ship anchor, in SHEET pixels
    ...
  ]
}
```
- **`index`** is the flat facing index `ri·5 + p` (forward-centre lane = 7).
- **`rect`** locates the actual (trimmed, cell-centered) pixels in sheet space.
- **`pivot`** is the projection of the ship's local origin — a **yaw-stable** world point — in sheet
  pixels. Place each facing so its pivot lands on the board cell; because the pivot tracks the same hull
  point in every facing, the ship never jumps when the facing changes. Pivot ≠ the frame's geometric
  centre (trim + perspective shift it per facing), which is exactly why it's stored.

Runtime contract: bind the sheet once, draw each facing from its `rect` at a **single integer scale**
(nearest-neighbor, sample texel centres — the 2 px gutter prevents bleed), positioned by `pivot`. One
texture bind covers all 15 facings (the reason a packed sheet beats 15 loose PNGs on the GPU). Loose
per-facing PNGs are not part of the contract — an optional authoring/debug convenience only.

(10 + runtime-mirror is still allowed to halve the sheet — *only* if baked lighting is L/R-symmetric,
see the mirror note above. The current light rig is not symmetric, so the editor exports all 15.)

### Export format — the GLB mesh  (runtime-lit path, pinned v5)
The editor's **⤓ Export GLB** button (`buildShipGLB` → `window.__exportShipGLB`) writes one file,
`broadside-ship.glb` — a standard **glTF-binary** the engine imports via `mesh_import::load_glb` and
**lights at runtime**. This is the dynamically-lit channel (§4); the sprite sheet above is the
preview/fallback channel. The format is identical to the proven CAD-editor GLB exporter, so the engine
ingests ships from either tool with one importer. Per `BROADSIDE_REALTIME_RENDER_SPEC.md`:

**Geometry.** TRIANGLES only; `POSITION` required; indexed. **Axes are raw hull space** — +X length
(prow +X), +Y up, +Z starboard — written verbatim, *no* glTF +Z-forward reorientation. All transforms
(engine bell rotation/offset, mirror, part placement) are **baked into vertex positions**; the engine
ignores the node/scene hierarchy. `NORMAL` is written but the **engine recomputes flat per-face normals**
(spec §1) — don't rely on the exported normals. **No `COLOR_0`** vertex colors (the engine groups by
material; in panel/facet mode the editor's baked vertex colors are dropped on export and the slot albedo
is used instead).

**Materials — one primitive per material**, grouped by the editor's slot (`hull`, `hullDark`, `tower`,
`dark`, `canopy`, `gun`, `batt`, `engine`). Each is a PBR material:
- `pbrMetallicRoughness.baseColorFactor` = the slot **albedo in linear RGBA** (sRGB→linear converted),
  `metallicFactor 0`, `roughnessFactor 0.85`.
- `emissiveFactor` (linear) on glowy slots (canopy / gun / batt).
- **`KHR_materials_unlit`** on pure-light parts (the engine glow disc, slot `engine`); those also set
  `emissiveFactor = baseColor` so they read as self-lit even under a unlit-unaware viewer.
Mesh slots are tagged at build time (`userData.matKey` on the hull mesh, engine bell = `dark`, glow disc
= `engine`); placed parts fall back to nearest-slot-by-color inference.

**Scale / framing.** The whole ship is **centered on the origin and uniformly scaled so its X-extent
(length) == 12 units** (the engine's default hull length), so every exported ship frames consistently
regardless of the editor's working scale. (The engine is forgiving of scale; this just keeps the fleet
proportionate.)

**Look-params in `scene.extras`** (proposed key names — the engine confirms its schema; consumable as
data today):
```
scene.extras = {
  "laz": <deg>, "lel": <deg>,                 // primary key-light az/el — what today's engine reads
  "lights": [ {"az":<deg>,"el":<deg>,"int":<f>}, ... ],   // full 3-light rig (engine Tier-1.5)
  "lightSym": <bool>,
  "bands": <int>,                             // posterize band count
  "shadeModel": "toon" | "lambert" | ...,
  "outlineWidth": <f>, "outlineCorner": "sharp" | "round",
  "grade": { "hue":<f>,"sat":<f>,"bri":<f>,"con":<f>,"gam":<f> }
}
```
The engine reads `laz`/`lel` **today** to light the imported mesh with one key light; the rest is
Tier-1.5 (3-light + toon banding + outline + grade), carried now so no re-export is needed when that
lands. These are **data, not a contract clause** — changing the light rig / shading / grade changes the
*values* in the GLB, not the format. Changing the *format* (axes, the material mapping, the extras schema,
the 12-unit scale rule) is what bumps this contract (see the surface row).

Runtime contract for the mesh path: import the GLB, recompute flat normals, build one draw per material
primitive, and light it in-engine using `extras` (key light from `laz`/`lel`, palette from `bands` +
`grade`). Do **not** expect baked shading in the vertex data — the mesh is albedo + emissive only.

### Depth sort  (new)
On-board sprites draw in painter's order by **board Y** (far rows first). A single lane was implicitly
sorted; a 2D field is not.

---

## Deepest change — visual facing ≠ tactical stance

On the 1D lane, the four stances served double duty: they *were* both the picture and the tactical
state (bow-on vs broadside). On the 2D board these **decouple**:

- The 15 facings are **pure visual orientation** — they only answer "which way is the hull pointing."
- **Bow-on vs broadside** — and therefore **which directional-shield arc takes a hit** — is now a
  **runtime geometry calc** from the two ships' board positions and facings. It is *not* encoded in
  the sprite. Two ships can both be drawn with the same facing sprite while presenting completely
  different aspects to each other.

Don't try to bake "bow-on" or "broadside" as sprite variants. Bake orientation; compute aspect.

---

## 6. Quick checklist when wiring export into the game

1. **Export the sheet** from the editor (⤓ Export sheet) — `broadside_ship_sheet.png` + `.json`. The 15
   facings (5 lanes × {−90, 0, +90}) come pre-normalized to one scale.
2. Load the sheet as one texture; read `facings[].rect` + `pivot` from the sidecar. Bind once, draw all
   facings from their rects — no per-sprite re-normalization.
3. Render with an **unlit** sprite shader (texture sample + optional tint). Do **not** light it, and
   do **not** rotate the pixels — swap facings.
4. Map `(hull facing, aim lane)` → the 15 facing indices.
5. **Depth-sort** on-board sprites by board Y, then layer runtime effects (damage, heat glow,
   shields) additively ON TOP.
6. Compute bow-on/broadside aspect at runtime from positions + facings (not from the sprite).
7. **(Mesh path, v5)** If a ship is drawn dynamically-lit instead of as a sprite, use **⤓ Export GLB**
   and the checklist below rather than the sprite steps.

**Wiring the GLB / mesh path (v5):**

1. **Export the GLB** from the editor (⤓ Export GLB) — `broadside-ship.glb`. One file: hull + engines +
   parts, one primitive per material, centered + scaled to 12-unit length.
2. Import via `mesh_import::load_glb`. Read positions **verbatim** (raw hull axes); **recompute flat
   per-face normals** — do not trust the exported `NORMAL`. Ignore the node hierarchy (transforms are
   baked).
3. One draw call **per material primitive**; use `baseColorFactor` (linear albedo), honor
   `emissiveFactor`, and treat `KHR_materials_unlit` primitives as self-lit (no lighting).
4. Light the mesh in-engine from `scene.extras`: key light from `laz`/`lel` today (the full `lights`
   rig + `lightSym` when Tier-1.5 lands); apply the toon/outline/grade pass from `bands` / `shadeModel`
   / `outlineWidth` / `outlineCorner` / `grade`.
5. Same board logic as the sprite path: orientation, depth-sort by board Y, runtime effects on top, and
   bow-on/broadside computed from positions + facings.
6. The CAD editor's GLBs and the Loft editor's GLBs share this exact format — one importer handles both.

---

*Mental model:* the editor is the **art tool** that produces canonical sprites. The Rust engine is
the **runtime** that arranges and embellishes them. Keep that boundary and the editor's render
features never obligate a renderer change — the one exception being the orientation set, which is a
shared convention both sides must agree on.
