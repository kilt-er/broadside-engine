//! # broadside-engine
//!
//! Rust port of the Broadside space-combat resolver. The canonical reference
//! is `broadside-engine/engine/types.ts` in the source design repo; when this
//! crate and the TypeScript engine disagree, the TypeScript is right.
//!
//! ## Module map
//!
//! - [`types`]    — the complete content + runtime type surface (mirrors
//!                  `engine/types.ts`).
//! - [`catalog`]  — loader for `assets/broadside.catalog.json` (the JSON
//!                  emitted by the analysis doc's "Copy JSON" button).
//! - [`geometry`] — spatial primitives: orientation, arcs, range bands,
//!                  directional shield absorption (mirrors `engine/geometry.ts`).
//! - [`resolve`]  — the combat resolver: four-phase round, arc/heat/cooldown
//!                  gate, damage pipeline, ordnance advance (mirrors
//!                  `engine/resolve.ts`). Content / AI effect bodies are
//!                  stubbed pending the content slice.
//!
//! Content effect bodies, AI, and rendering live in sibling modules added by
//! other teammates.

pub mod types;
pub mod catalog;
pub mod geometry;
pub mod resolve;
