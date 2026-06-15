//! # broadside-engine
//!
//! Rust port of the Broadside space-combat resolver. The canonical reference
//! is `broadside-engine/engine/types.ts` in the source design repo; when this
//! crate and the TypeScript engine disagree, the TypeScript is right.
//!
//! ## Module map
//!
//! - [`types`] — the complete content + runtime type surface (mirrors
//!   `engine/types.ts`).
//! - [`catalog`] — loader for `assets/broadside.catalog.json` (the JSON
//!   emitted by the analysis doc's "Copy JSON" button).
//! - [`grid`] — v2 2D spatial type surface for the 5×4 grid: `Pos`,
//!   `Dir8`, `Facing{Bow(Dir4)/Broadside(Axis)}`, `Range`, plus index /
//!   offset / from_to / neighbors helpers. Lands standalone ahead of the
//!   atomic `cell:usize→Pos` migration (blueprint lane task A2). Pure data.
//! - [`geometry`] — spatial primitives: orientation, arcs, range bands,
//!   directional shield absorption (mirrors `engine/geometry.ts`). The 1-D
//!   lane version, live until the A3 `cell:usize→Pos` migration retires it.
//! - [`geometry2d`] — the v2 2-D replacement for [`geometry`], built over
//!   [`grid`]: `in_band` / `band_falloff` `[1.0,0.6,0.3]`, the magnitude-aware
//!   `direction_to`, the 2-D `arc_bears` cone, the `facing_zone(Facing,Dir8)`
//!   quadrant table, and `absorb_shield`/`default_shield_profile` kept verbatim.
//!   Lands additively alongside the 1-D `geometry` (expand-contract); the
//!   architect `git mv`s it onto `geometry.rs` once A3 removes the 1-D world
//!   (blueprint resolver task R1). Pure + deterministic — the single source the
//!   firing path and the ThreatMap telegraph both run.
//! - [`resolve`] — the combat resolver: four-phase round, arc/heat/cooldown
//!   gate, damage pipeline, ordnance advance (mirrors `engine/resolve.ts`).
//!   Content / AI effect bodies are stubbed pending the content slice.
//! - [`perspective`] — screen-space lane projection, ship sprite polygons,
//!   beam endpoints. The only module that knows about screen coordinates;
//!   consumed by the renderer (mirrors `engine/perspective.ts`).
//! - [`projector`] — v2 5×4 perspective projector: maps a [`grid::Pos`] to a
//!   screen-space [`projector::CellQuad`] in 480×270 frame space with
//!   Star-Wars-crawl foreshortening (rows recede, columns fan). The v2 spatial
//!   replacement for the flat-strip `perspective` lane; pure over `grid`
//!   (blueprint lane task D2).
//! - [`atlas`]    — procedural sprite atlas (ship faces, bow chevron,
//!   ordnance, HUD glyphs, parallax art).
//! - [`background`] — the 20-layer parallax space background (depth queue +
//!   horizontal parallax). Implements `BROADSIDE_BACKGROUND_SPEC.md` §4 slot
//!   math; reads parallax constants from `background_manifest.json` and ships a
//!   solid-ink-per-layer fallback so it renders before the painted PNGs exist.
//! - [`gfx`]      — wgpu state, instanced sprite batcher, virtual-resolution
//!   blit. Pipeline scaffold only; scene content lives in [`hud`].
//! - [`hud`]      — turns a [`types::Board`] into a back-to-front
//!   `Vec<SpriteInstance>` for the renderer.
//! - [`input`]    — framework-agnostic Key enum, canonical key->Intent
//!   mapping for the Phase 1 demo, and `DemoContent` (a small `Content`
//!   impl pre-loaded with the synthetic move/flip/vent actions and the
//!   demo's mount weapons).
//! - [`subsystems`] — runtime subsystem layer (Phase 2). Holds the
//!   `Installations` registry + behavioral dispatch for `damage_modifier`
//!   and `on_turn_end`. `DemoContent` owns an `Installations` and routes
//!   the two Content trait methods through this module.
//! - [`classes`] — three placeholder [`types::ClassDef`]s + their
//!   Signature actions (Vanguard/Overcharge, Wraith/Phase Drift,
//!   Bulwark/Broadside Volley). `DemoContent::default` registers the
//!   three Signature actions; the ClassDefs are exposed via
//!   `placeholder_classes()` for catalog seeding. Input wiring deferred
//!   per task #62's "just have the Action defs in place."
//! - [`save`] — JSON (serde_json) save/load for the per-run state
//!   ([`types::Run`]). Methods on `Run`: `save_to_disk(path)`,
//!   `load_from_disk(path)`, `delete_save(path)`. Path is the caller's
//!   choice — bin decides where; meta-progression lives at a separate
//!   path with separate lifecycle (see [`meta`]).
//! - [`ship_design`] — serde shape for the loft editor's ship-design
//!   `.json` (`docs/broadside-loft-editor.html`'s `collectDesign()`).
//!   Data only — the asset format the 3D loft/render path consumes;
//!   no rendering here.
//! - [`loft`] — pure-math hull lofting: a [`ship_design::ShipDesign`]'s 2D
//!   profiles swept into a 3D triangle-soup [`loft::HullMesh`]. No GPU, no
//!   feature gate (CI-testable headless); the renderer's `loft_gpu` uploads
//!   the mesh and runs the depth + posterize passes. Stage 1 of the ship
//!   render pipeline (`docs/RENDER_PIPELINE.md`).
//! - [`mesh_import`] — import a CAD-authored baked ship mesh (glTF `.glb`,
//!   from the son's Broadside CAD editor) into the same [`loft::HullMesh`]
//!   the loft path emits, plus per-group material colours. Data only (the
//!   `gltf` crate, no GPU); the second geometry producer alongside [`loft`],
//!   both meeting the render path at the `HullMesh` boundary.
//! - [`ship_asset`] — data-only selector over the two geometry producers:
//!   given a ship asset (`.json` loft design or `.glb` CAD mesh) it returns
//!   `(HullMesh, per-vertex colours)`, dispatching to [`loft`] (uniform grey)
//!   or [`mesh_import`] (per-material colours). No GPU, no bin wiring.
//!
//! Content effect bodies and AI live in sibling modules added by other
//! teammates.

pub mod types;
pub mod ai;
pub mod cards;
pub mod catalog;
pub mod catalog_canonical;
pub mod classes;
pub mod geometry;
pub mod geometry2d;
pub mod grid;
pub mod input;
pub mod loft;
pub mod mesh_import;
pub mod meta;
pub mod perspective;
pub mod resolve;
pub mod runs;
pub mod save;
pub mod ship_asset;
pub mod ship_design;
pub mod subsystems;

#[cfg(feature = "render")]
pub mod atlas;
#[cfg(feature = "render")]
pub mod background;
#[cfg(feature = "render")]
pub mod gfx;
#[cfg(feature = "render")]
pub mod hud;
#[cfg(feature = "render")]
pub mod loft_gpu;
#[cfg(feature = "render")]
pub mod projector;
#[cfg(feature = "render")]
pub mod vfx;
#[cfg(all(feature = "render", feature = "runtime"))]
pub mod sprites;
#[cfg(feature = "audio")]
pub mod audio;
