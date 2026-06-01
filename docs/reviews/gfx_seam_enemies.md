# Review: render 4 enemies as CAD ship + per-ship loft generalization (a4703d1, #29/4b)

Reviewer soundness pass. gfx.rs/hud.rs/sprites.rs/broadside.rs (renderer-owned).
Status: **APPROVE.** No findings. 310 lib tests green (render,runtime), incl. the
new per-ship LoftShip-emission test.

## 1. Per-ship generalization (PASS — no pose aliasing)

`demo_loft: Option<DemoLoftShip>` → two maps:
- `loft_meshes: HashMap<LoftMeshKind, LoftMesh>` — SHARED geometry, one per kind (PlayerDagger / EnemyCad). Correct to share: the mesh is identical across all enemies; only the pose differs.
- `loft_poses: HashMap<String /*ship_id*/, ShipPose>` — per-ship animation state, keyed by Ship::id. Each enemy (cells 2/3/5/6) gets its own ShipPose via `sync_loft_pose(ship_id, orientation)`, and the render loop looks up yaw by `loft_poses.get(q.ship_id)` (gfx.rs:1601). So four enemies sharing one EnemyCad mesh still animate from four distinct poses — NO aliasing. `retain_loft_poses(live_ids)` prunes departed ships; `advance_loft_poses` ticks all.

## 2. Generalized REPLACE gate (PASS — still mutually exclusive)

`hud::push_ship`: `if let Some(loft_kind) = sprites.loft_kind(&ship.id, is_player) { push LoftShip{ id, kind, corners }; return; }` — same early-return shape as the player-only version, generalized to per-ship Option. Some → LoftShip only (return before 2D); None → 2D. No ship draws both or neither. The player path I approved in 7f15172 still holds (player → PlayerDagger).

Crucially, `Gfx::loft_kind` (gfx.rs:773) gates on `has_loft_mesh(kind).then_some(kind)` — a ship emits a LoftShip ONLY if its mesh is actually uploaded; otherwise it falls back to 2D. So there's no path where an enemy emits a LoftShip the render loop then skips (which would leave a blank lane slot). Gap-free.

## 3. Render-loop restructure — the risky part (PASS, scrutinized)

The shared 320×200 loft target + write_buffer-flushes-at-submit means each loft ship must render+blit before the next overwrites the target. Verified the segmentation:
- **`cleared` flag is monotonic** — set true by either flush_scene_run (when it drains a non-empty run) or a loft blit; never reset. Both paths choose `load = if cleared { Load } else { Clear(CLEAR) }`. So the offscreen is cleared EXACTLY ONCE (first segment), and every later segment Loads — no double-clear (which would wipe earlier 2D/loft work), no missing clear (undefined contents). flush_scene_run returns early without touching the encoder when the run is empty, so it can't falsely flip `cleared`.
- **Empty-frame guard** (gfx.rs:1673): no batches → cleared stays false → a dedicated clear pass runs so the swap blit reads defined contents.
- **Multi-submit encoder correctness**: each segment (scene run / loft hull render / loft blit) creates its OWN encoder, finishes + submits within scope. No encoder reused after finish, none outlives its submit. The loft hull render submits BEFORE its blit (1617 then 1664); the quad-ubo write_buffer (1629) lands before the blit submit, so each ship's blit carries its own corners — exactly why per-ship submit isolation is required (a single batched encoder would let ship N+1 clobber N's shared uniforms pre-blit). The restructure is the correct solution to the shared-target constraint, not an over-complication.
- **Depth still isolated**: render_ship uses LoftGpu's internal depth; both the scene-run pass and the blit pass are depth_stencil_attachment: None. Holds.
- Cost note (NOT a soundness issue): N+1 submits/frame for N loft ships. Documented tradeoff for the shared target; fine at lane-count scale.

## 4. glb load path (PASS)

install_enemy_cad: include_bytes!(vendored glb) → load_glb → upload_imported (so the emissive orange accent survives via the #123 path). upload_imported's signature is unchanged — no collision with architect's ship_asset loader (which also calls it). Player still install_player_dagger (grey loft).

## Scope / known

Diff = the 4 renderer files. broadside.rs limited to two functional hunks (pose sync + mesh install); its pre-existing fmt drift deliberately left for the bin owners (#22/#100), per the standing no-touch — confirmed the diff didn't capture unrelated reformatting. CAD ship at true scale (7.75u), no scale fudge.
