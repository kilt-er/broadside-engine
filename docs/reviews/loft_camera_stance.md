# Review: loft render — fixed camera, clean stances, bigger framing (7d12234, #36)

Reviewer soundness pass. loft_gpu.rs only (commit confirmed via --name-only).
Status: **APPROVE.** No findings in the committed change. 6 loft_gpu tests green
(clean re-run). One shared-tree-hygiene note for the lead (not about this commit).

## Root cause (my earlier diagnostic lead, confirmed)

The skew was the camera ORBIT, not a yaw bug — a non-zero rest azimuth (28°,
the POC's eval-only framing angle) tilted every rest pose. Exactly the
"camera rotation not at 0" bruce flagged. Fixed by moving stance from camera
orbit → ship model rotation, camera fixed square to the lane.

## 1. Camera fixed (PASS)

`CAMERA_AZIMUTH_DEG = 0.0`, `CAMERA_PITCH_DEG = 26.0` (const). `render_ship`
calls `camera_view_proj(0_rad, 26_rad, aspect)` — azimuth literally 0, the ¾
from pitch alone. No time-advancing input to the camera; nothing orbits. The old
`pitch_deg` param is GONE from the signature (6 params now) — render_ship no
longer takes any per-call camera angle.

## 2. Stance → model rotation (PASS — winding/normals correct, NOT inside-out)

Stance is now the hull's MODEL yaw about vertical: Fore 0 / Aft 180 / Broadside
90 (`MODEL_YAW_*`). `render_ship` sets `model = rotation_y(yaw_rad)` (was
identity4).

The critical check — does the rotation flip winding / break shading? **No.**
`rotation_y` is column-major standard right-handed Y rotation:
`col0=[c,0,-s,0] col2=[s,0,c,0]` → orthonormal, **determinant +1**, handedness
preserved. So winding is unchanged AND the shader's normal transform
(`wn = (model * vec4(nrm,0)).xyz` in vs_main) rotates normals in lockstep with
the hull, so they stay outward-facing relative to the fixed light. No inside-out
shading. (This was the real risk — a det=−1 mirror or positions-rotated-but-not-
normals — and it's handled because the model matrix feeds both transforms.)
At yaw 90°: x→z, the hull length swings to receding +z, broad flank bears — clean
broadside. Tests updated to assert fore==0 (the canonical clean bow-on).

## 3. Idle roll at rest (PASS)

Unchanged: `sin(idle_t·IDLE_ROLL_HZ·TAU)·IDLE_ROLL_DEG`, which is 0 at idle_t=0.
Rest pose is exactly the canonical model yaw, no skew. (My earlier "idle is a red
herring" point — confirmed it never contributed an initial offset.)

## 4. Ortho 9→5 (PASS — single shared constant)

`HALF_EXTENT = 5.0`, one const used in `camera_view_proj`'s ortho bounds. NOT a
per-ship param — one fixed framing across all ships, so true relative scale is
preserved (the 7.75u CAD ship still renders ~65% of the ~12u dagger). No per-ship
fudge. Larger ship fills the 320×200 target instead of being a tiny island.

## Signature stability

render_ship dropped `pitch_deg` (7→6 params); the sole call site (gfx.rs:1611)
passes `yaw` with no pitch arg — updated in lockstep. gfx/hud/bin otherwise
untouched, no collision with the per-ship loft work (a4703d1) already approved.

## SHARED-TREE NOTE (for the lead — NOT a defect in 7d12234)

During this review the shared working tree briefly held an UNCOMMITTED mid-edit
to gfx.rs/loft_gpu.rs (a stale `render_ship(.., yaw, pitch)` call) that did not
compile — a transient snapshot of another teammate's in-flight edit, gone moments
later (git diff now clean, 7d12234 itself is loft_gpu.rs-only and builds). This is
the WIP-sweep-in collision the atomic-pathspec protocol guards against; flagging so
the team knows the tree was momentarily uncompilable. My APPROVE is of the
committed 7d12234, verified against a clean tree.

---

## Addendum: drop the dead pitch arg (9f4e71a) — APPROVE; explains the transient break

Cleanup follow-up. The full #36 fix is **7d12234 + 9f4e71a** (gfx.rs + loft_gpu.rs).

- `render_ship` signature DID change here (7d12234 had left `_pitch_deg` as an ignored param; 9f4e71a drops it → 6 params). The sole call site (gfx.rs:1611) is updated to the 6-arg form, and the stale `let pitch = 26.0` local + "live scrubber" comment are removed from gfx.rs. `grep render_ship(` = one caller, matched; `grep pitch src/gfx.rs` = nothing left. No stray caller.
- **No behavioral change**: pitch was already fixed at 26° internally via CAMERA_PITCH_DEG; this removes the ignored param + dead local only. Output identical. Docstring updated (camera owns the ¾ angle; stance is the only per-ship variable).
- **This resolves the transient shared-tree break I flagged**: the uncompilable window was exactly between 7d12234 (param dropped in loft_gpu) and 9f4e71a (caller updated in gfx). With both landed the tree is consistent and builds clean. So the earlier process flag was a real-but-transient two-commit split, now closed — not a lingering defect.

Full #36 verdict: APPROVE (7d12234 + 9f4e71a). Camera fixed, stance via det-+1 model rotation (shading correct), idle 0 at rest, single-const framing, API cleaned. 310 tests green.
