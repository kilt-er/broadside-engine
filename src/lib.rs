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
//! - [`geometry`] — spatial primitives: orientation, arcs, range bands,
//!   directional shield absorption (mirrors `engine/geometry.ts`).
//! - [`resolve`] — the combat resolver: four-phase round, arc/heat/cooldown
//!   gate, damage pipeline, ordnance advance (mirrors `engine/resolve.ts`).
//!   Content / AI effect bodies are stubbed pending the content slice.
//! - [`perspective`] — screen-space lane projection, ship sprite polygons,
//!   beam endpoints. The only module that knows about screen coordinates;
//!   consumed by the renderer (mirrors `engine/perspective.ts`).
//! - [`atlas`]    — procedural sprite atlas (ship faces, bow chevron,
//!   ordnance, HUD glyphs, parallax art).
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
//!
//! Content effect bodies and AI live in sibling modules added by other
//! teammates.

pub mod types;
pub mod cards;
pub mod catalog;
pub mod catalog_canonical;
pub mod classes;
pub mod geometry;
pub mod input;
pub mod meta;
pub mod perspective;
pub mod resolve;
pub mod runs;
pub mod subsystems;

#[cfg(feature = "render")]
pub mod atlas;
#[cfg(feature = "render")]
pub mod gfx;
#[cfg(feature = "render")]
pub mod hud;
#[cfg(all(feature = "render", feature = "runtime"))]
pub mod sprites;
#[cfg(feature = "audio")]
pub mod audio;
