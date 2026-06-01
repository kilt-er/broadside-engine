# Review: src/loft_gpu.rs (in-engine loft render pipeline, 50eb645)

Reviewer SOUNDNESS pass (task #122 part 1). feature=render, renderer-owned. The
engine's only depth-using pipeline. Status: **APPROVE.** No findings, no live
issues. All 6 headless tests green (`--features render,runtime`).

## Uniform layout — the key soundness gate (PASS)

All three GPU structs are `#[repr(C)]` + `bytemuck::Pod + Zeroable` + a
compile-time `size_of` assert, and every layout is std140-correct with NO
`vec3<f32>` in any uniform (the invalid-encoder trap class):
- **Vertex == 48**: pos/normal/color each a [f32;3] padded to a 16-byte slot. Vertex-buffer layout (explicit array_stride + attribute offsets), not std140, but the 16-byte slotting keeps it unambiguous. Assert pins 48.
- **SceneUniform == 176**: 2×mat4x4 (128) + 3×vec4 (48). `ambient` is a vec4 consumed via `.rgb` swizzle in the shader (fs_main:254) — the correct way to dodge the vec3 trap. The 176 assert already caught a real 160→176 miscount (per lead), so the guard is demonstrably load-bearing.
- **PostUniform == 16**: `bands: f32` + THREE SCALAR f32 pads — never a vec3. The standing rule from the gfx.rs BlendUniform / loft_poc history, correctly applied.

No uniform is missing a size assert. This is the gate the lead called out and it holds.

## Depth isolation (PASS — no cross-contamination)

- Depth texture created INSIDE the module (the `mk` texture closure) and never exposed on the public surface.
- Hull pass (pass 1): pipeline `depth_stencil: Some(DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: true, depth_compare: Less })`, render pass has a depth attachment (clear 1.0, store Discard — depth not needed post-pass).
- Posterize pass (pass 2): pipeline `depth_stencil: None`, render pass `depth_stencil_attachment: None`. Fullscreen 3-vert triangle.
- The 2D compositor in gfx.rs cannot reach this depth texture, so it stays `depth_stencil: None` with no leakage. Isolation is structural, not conventional.

## Soundness

- **Zero `unsafe`** in the module. All byte-casting via bytemuck Pod.
- **Hull shader**: flat Lambert (key + fill dirs from SceneUniform) × per-vertex albedo `in.color`, ambient added. cull_mode: None (deliberate — closed loft/imported mesh shouldn't risk holes; documented). Ccw front face. Correct.
- **Cut-out**: both passes clear color to transparent (a=0); the posterize pass preserves the cut-out. House style 320×200 (LOW_W/LOW_H) / 8 bands, locked engine-wide as documented.

## ShipPose / camera math (PASS — pure, headless-tested)

- `orientation_yaw_deg` distinct per stance; `reorient_to` tweens from the CURRENT displayed yaw (so a mid-tween re-flip is continuous, not a snap) with no-op + zero-delta guards; `yaw_deg_no_idle` lerps via `smoothstep(clamp(elapsed/dur))`; `yaw_deg`/`idle_bob` add bounded idle sine/cosine; `smoothstep` is the standard t²(3−2t) clamped. All pure f32 math, no GPU dependency.
- 6 headless tests: yaws-distinct, pose-at-rest-within-idle, reorient-tweens-then-settles, idle-advances-bounded, camera-view-proj-finite, imported-colors-expand-groups. Green.

## Minor (non-blocking, not flagged for fix)

`ShipPose::advance` (loft_gpu.rs:124) has a redundant `let _ = (from, to);` inside the tween branch — dead no-op binding, harmless. Not worth a commit on its own; fold into any future touch.
