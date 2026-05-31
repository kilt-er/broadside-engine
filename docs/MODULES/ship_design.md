# `src/ship_design.rs` — loft-editor `.json` asset format

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/ship_design.rs`](../LINE_BY_LINE.md#srcship_designrs) section of
`LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **data half of the render-pipeline pivot** (see
[`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)). The loft editor
(`docs/broadside-loft-editor.html`) lets an artist trace a hull's plan and section,
tweak render settings, and **save the design as JSON** (its `collectDesign()` at
`broadside-loft-editor.html:718`). This module is the Rust mirror of that payload —
the serde shape that lets the engine loft a ship from *design data* instead of from
baked sprite PNGs.

It is **pure data — no rendering here.** The loft/render POC (task #109) owns the
geometry generation and the wgpu side; this module only parses and re-serializes the
design, so that POC graduates straight into "the engine loads a `.json`." Together
they replace the [`sprites.rs`](sprites.md) side/top PNG path.

The design goal throughout is **byte-stable round-trip with the tool's output** — a
`.json` the editor saved must parse here, and a `ShipDesign` serialized here must
match what the tool would emit (modulo insignificant whitespace). Every type choice
below serves that.

### Wire schema (verbatim from `collectDesign()`)

```jsonc
{
  "format": "broadside-ship",
  "version": 1,
  "plan":    [[x, halfWidth], ...],          // stern(x=0) -> prow(x=1)
  "section": [[z, y], ...],                  // top -> belly half cross-section
  "heightProfile": [[x, mult], ...] | null,  // side-view height, null = flat 1.0
  "settings": { "pitch":26, "yaw":28, "zoom":1, "stretch":2.0, "hscale":0.7,
                "sup":true, "greeb":0.6, "bands":4, "laz":-50, "lel":60,
                "res": { "w":160, "h":100 } },
  "grade": { "hue":0, "sat":1, "bri":1, "con":1, "gam":1 }
}
```

---

## `struct Point2` (src/ship_design.rs:95)

**Intent:** A single 2-D point as the tool stores it — a **bare `[x, y]` JSON
array**, not a `{ "x":…, "y":… }` object. `#[serde(transparent)]` over the inner
`[f64; 2]` (src/ship_design.rs:94) is what makes `plan` / `section` /
`heightProfile` round-trip byte-for-byte with the tool's `[[x, hw], ...]` shape.

The crucial subtlety: **the two components mean different things per list**, so the
newtype carries named `.x()` / `.y()` accessors (src/ship_design.rs:101, 108) with
documented semantics rather than letting callers index `[0]`/`[1]` blindly:
- **plan** — `.x()` = x-along-length (stern 0 → prow 1), `.y()` = half-width (0..1).
- **section** — `.x()` = z (half-width 0..1), `.y()` = y-height (-1..1, top → belly).
- **heightProfile** — `.x()` = x-along-length, `.y()` = height multiplier (~0..1.5).

`From<[f64;2]>` and `From<(f64,f64)>` (src/ship_design.rs:113, 119) are construction
conveniences. **Worked example:** `point2_is_a_bare_array_on_the_wire`
(src/ship_design.rs:317) pins that `Point2([0.6, 0.22])` serializes as `[0.6,0.22]`
(not an object) and round-trips with the right `.x()`/`.y()`.

---

## `struct Resolution` / `struct Settings` / `struct Grade` (src/ship_design.rs:127, 136, 164)

The sub-objects, field names matching the JSON keys verbatim:
- `Resolution { w, h }` (u32) — target sprite pixel dimensions (`settings.res`).
- `Settings` — camera `pitch`/`yaw`/`zoom`, hull `stretch`/`hscale`, the `sup`
  superstructure toggle, `greeb` density, posterize `bands`, light `laz`/`lel`, and
  `res`. The terse tool keys (`sup`, `greeb`, `laz`, `lel`) get spelled-out doc
  comments.
- `Grade` — the HSV/contrast/gamma post-process uniforms (`hue`/`sat`/`bri`/`con`/
  `gam`), all `f64` (the tool stores live shader uniform `.value`s, which are
  numbers; `hue` is already the 0..1 slider/360 form, matching
  [`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)'s Stage 4 grade).

---

## `struct ShipDesign` (src/ship_design.rs:181)

**Intent:** The complete parsed design. Fields: `format` (the discriminator,
parsed-not-enforced), `version` (parsed permissively), `plan` / `section`
(`Vec<Point2>`), `height_profile`, `settings`, `grade`.

The load-bearing field is **`height_profile: Option<Vec<Point2>>`**
(src/ship_design.rs:197) with `#[serde(default, rename = "heightProfile")]` and
**deliberately no `skip_serializing_if`**. The tool writes literal `null` when no
side-view image has been traced (`HEIGHTPROF = null`, meaning "flat 1.0 height
everywhere"), and **always emits the key**. So `None` must round-trip *as* JSON
`null`, not be omitted — omitting it would diverge from the tool's output and from
its `applyDesign` reader (which checks `Array.isArray(d.heightProfile)`). This
mirrors the `SubsystemDef::unlock_salvage` `number | null` precedent in
[`types.rs`](types.md). The `#[serde(default)]` is kept *defensively* so a future
tool version that drops the key still parses (→ `None`), even though today's tool
never does.

**Worked examples:** `height_profile_none_round_trips_as_json_null`
(src/ship_design.rs:338) pins the explicit-`null` emit;
`height_profile_some_parses_and_round_trips` (src/ship_design.rs:350) the populated
case; `missing_height_profile_key_defaults_to_none` (src/ship_design.rs:366) the
defensive default.

---

## `enum DesignError` (src/ship_design.rs:208)

`Io` / `Parse`, `#[non_exhaustive]`, with `Display` / `Error::source` / `From<io::Error>`
— the same shape as [`catalog::LoadError`](catalog.md) and
[`save::SaveError`](save.md), so callers `?` it uniformly. (No `From<serde_json::Error>`
blanket — the two methods that produce parse errors map explicitly.)

---

## `impl ShipDesign` — load / save / tolerance (src/ship_design.rs:240)

- `load_from_json(bytes)` (src/ship_design.rs:244) — parse from in-memory bytes (e.g.
  an `include_bytes!` asset). **Version-tolerant**: any `version` parses, and serde
  ignores unknown extra fields, so a v2 file loads its v1-compatible subset.
- `load_from_path(path)` (src/ship_design.rs:249) — read the file, then `load_from_json`.
- `to_json_pretty()` (src/ship_design.rs:256) — serialize back with the same 2-space
  indent the tool uses (`JSON.stringify(…, null, 2)`).
- `has_expected_format()` (src/ship_design.rs:263) — is `format == "broadside-ship"`?
  Lets a caller mirror the tool's "this isn't a Broadside ship design — load anyway?"
  confirm instead of hard-failing.
- `is_known_version()` (src/ship_design.rs:270) — is `version == KNOWN_VERSION` (1)? A
  `false` is a hint to warn / migrate, **not** a parse failure.

**Parse-don't-enforce version tolerance** is the through-line: the struct accepts any
`format`/`version` at parse time; the two `*_version`/`*_format` predicates let the
*caller* decide whether to bail or proceed. Newer files never hard-fail on the schema
tag alone.

**Cross-references:** This is the asset format the 3D render path (POC #109, then the
in-engine lift) consumes — see [`RENDER_PIPELINE.md`](../RENDER_PIPELINE.md)'s phased
plan (step 4, "load designs from the tool"). Constants `EXPECTED_FORMAT`
(src/ship_design.rs:80) and `KNOWN_VERSION` (src/ship_design.rs:84) mirror the tool's
`DESIGN_VERSION`.

**Worked examples:** `parses_the_sample_design` (src/ship_design.rs:298, the default
dagger hull), `round_trips_through_serialize` (src/ship_design.rs:330),
`future_version_still_parses_but_flags_unknown` (src/ship_design.rs:375, v2 + unknown
field loads, `is_known_version()` false), `wrong_format_parses_but_flags_format`
(src/ship_design.rs:392), `malformed_json_is_a_parse_error` (src/ship_design.rs:404),
`missing_required_field_is_a_parse_error` (src/ship_design.rs:410, dropping `settings`
rejects — the loft path needs real camera/res values, so it must not silently default).
