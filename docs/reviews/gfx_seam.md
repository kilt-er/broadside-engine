# Review: gfx seam — first in-engine 3D loft ship in lane (7f15172)

Reviewer soundness pass (task #122 part 2, the milestone). Renderer-owned edits
to gfx.rs / loft_gpu.rs / sprites.rs / hud.rs / bin/broadside.rs. Status:
**APPROVE.** No findings. Diff scoped clean, 309 lib tests green (render,runtime).

## 1. Render-loop correctness (PASS)

Single encoder, three sequential passes, one submit (gfx.rs:1562):
1. **Loft PRE-PASS** (1422-1468): `self.loft.render_ship(&queue, &mut encoder, ...)` records the hull→posterize passes into the SHARED encoder BEFORE the compositor pass opens. Depth stays entirely inside LoftGpu (verified in loft_gpu review — module-internal depth texture). The blit bind group is built UP FRONT (borrows `self.loft.output_view()` + quad ubo) so the render pass holds it by reference — correct borrow ordering, no mid-pass mutable borrow of self.loft.
2. **Scene→offscreen** (1473): `depth_stencil_attachment: None`. LoftShip composites in the same in-order batch walk as sprites/polygons/textured-ships, so correct 2D z-order at its lane bbox.
3. **Offscreen→swap** (1542): `depth_stencil_attachment: None`.
No intermediate submit, no separate encoder → no "Encoder is invalid" risk. Compositor depth_stencil stays None on both passes — depth isolation holds.

## 2. The REPLACE gate (PASS — mutually exclusive by construction)

`hud::push_ship` (hud.rs:553): `if ship.faction == Player && sprites.loft_player() { out.push(LoftShip{..}); return; }` placed BEFORE the 2D textured/procedural draw, with an early `return`.
- Player + loft asset → LoftShip ONLY (returns before 2D). No double-draw.
- Player + no loft asset → falls through to 2D PNG/procedural.
- Non-player → faction check fails, always 2D.
No path draws both (early return), none draws neither. The LoftShip bbox is the same `[left,top]..[right,base]` the 2D silhouette would occupy, so the 3D ship sits exactly where the 2D one did; HUD overlays still draw on top.

## 3. Per-asset fallback intact (PASS)

The 2D PNG/procedural path is NOT ripped out — only gated past for the player-with-loft case. Every non-player ship and the no-loft player still take the full TexturedShip / procedural silhouette path. `loft_player()` defaults false (sprites.rs), so absent the demo install nothing changes.

## 4. Uniform size asserts (PASS)

- **LoftQuadUniform == 48**: 4×vec2 (32) + vec2 px_to_ndc (8) + vec2 pad (8). All vec2 (8-byte aligned), NO vec3 trap. repr(C) + Pod/Zeroable + `const _ = assert!(==48)` (gfx.rs:703).
- Pre-existing ViewUniform(16)/BlitUniform(16)/BlendUniform(16) asserts all still present (gfx.rs:316-318). The SceneUniform==176 assert lives in loft_gpu.rs (caught the mid-build miscount). No uniform missing an assert.

## 5. Shared-tree hygiene (PASS — scoped clean)

`git show --name-only` = exactly the 5 renderer files; grep for resolve/catalog/meta/runs/types/cards/subsystems/input = NONE. No content/architect WIP captured.
- loft_gpu.rs: ONLY the one-line dead `let _ = (from,to);` removal (the minor note from the loft_gpu review, folded in as the lead directed). Nothing else.
- broadside.rs: two hunks, both loft wiring (install_demo_loft_ship at startup, per-frame pose sync). hud.rs: the REPLACE gate + the LoftShipInstance import. All functional, no rustfmt churn.

## Notes (non-blocking, honestly scoped by the author)

- One loft ship this milestone: only `loft_quads[0]`'s bind group is built; the LoftShip arm `continue`s if absent. Comment says it generalizes to ≤ lane-count serially. Fine for the milestone.
- Grey only — per-vertex colors not fed yet (lights up when ShipDesign/glb drives the colors slice). Pitch fixed at 26° (¾ default; live scrubber is a tracked follow-up). emissive/unlit glow is base-colour-only (tracked). All documented in the commit msg, none a soundness issue.
