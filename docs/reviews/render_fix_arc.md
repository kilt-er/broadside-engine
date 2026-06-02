# Review: render-fix arc (eae0fe5 → 6fb546a, 8 commits) — task #43-#49/#14/#45/#46

Reviewer soundness pass on the complete render arc (origin 6fb546a). Status:
**APPROVE.** No blocking findings. 317 lib tests green (render,runtime). One stale
COMMENT noted (non-blocking). One architectural-supersession note for the record.

## Scope (PASS — all renderer-owned, no foreign WIP)

git show --stat per commit: every change touches only gfx.rs / hud.rs / loft_gpu.rs
/ sprites.rs / atlas.rs / bin/broadside.rs. Zero resolve.rs / catalog / meta / runs
/ glb / tester files. eae0fe5 is widest (5 files) but all renderer. No cross-lane
edit rode along.

## c1c1fda — HUD re-anchor + re-enable (the supersede chain) (PASS)

a501752 gated the per-ship overlays + range ruler OFF via `SHOW_PLACEHOLDER_HUD=false`
(nothing deleted — single const, three call sites suppressed). c1c1fda SUPERSEDES it:
- Gate flipped back ON (`SHOW_PLACEHOLDER_HUD = true`, line 99). All three sites
  (range ruler, per-ship overlay loop, view-angle overlay) execute. Nothing left
  gated off — confirmed by grep + reading compose_scene_tweened.
- Re-anchor: `ship_bbox` now returns the loft dest-rect extent
  (`LOFT_SHIP_HEIGHT_PX * LOFT_TEXTURE_ASPECT`, `LOFT_SHIP_HEIGHT_PX`) instead of
  the stale per-stance `scaled_ship_extent`. Since `ship_bbox` is the SHARED anchor
  for all four overlays, this one change re-anchors heat/pips/queue/badges to where
  the 3D ship actually blits. Correct root-cause fix (anchor, not the overlays).
- `scaled_ship_extent` correctly RETAINED for the 2D-silhouette fallback path
  (push_ship 2D branch) — not orphaned. (The subtle correctness point: deleting it
  would have broken the no-loft-mesh fallback.)
- "tan dots" traced to no HUD emitter — broadside hull overflow at half=5,
  eliminated by #49 (HALF_EXTENT=7). Consistent.

## 374a050 — HALF_EXTENT 5→7 framing math (PASS)

Sound and well-reasoned: the perpendicular broadside ship projects its full ~12u
length VERTICALLY under the 48° top-down pitch; at half=5 that was NDC 2.64 > 2.0
clip range → broadside bow clipped flat. half=7 (14u box) → NDC ~1.89, clears with
margin; bow-on ships have width to spare. ONE const across all stances → true
relative scale preserved, no size-pop on reorient. Correct fix for the clipped-bow
+ overflow-spill artifacts.

## 86eb9e6 — #47 stance override (PASS — deliberate, NOT flagged as POC-divergence)

bow-on Fore=0 / Aft=180 (PARALLEL to lane), Broadside=90 (perpendicular), pitch
26→48° (top-down ¾). Per the lead this is bruce's deliberate POC override — the ¾
read now comes from pitch, not from yawing ships to show a front. Internally
coherent: the broadside=90 perpendicular is exactly what HALF_EXTENT=7 was sized to
clear at 48°. Stance (#47) + framing (#49) + pitch cohere as one design. Test
asserts aft==180. NOT flagged as a divergence — it's intended.

## 4d354ce — planet seam fix (PASS — real math improvement)

Old `dx+dy<0` half-plane split = hard diagonal seam across the sphere. New: compute
the sphere surface normal `(dx, dy, nz)/r` with `nz = sqrt((r²−d²).max(0))` (clamped
→ no NaN at rim), dot with a normalized upper-left light, bucket `(N·L+1)/2` into
tone bands — a correct spherical Lambert terminator, no seam. Zero unsafe in atlas.
6fb546a adds background depth/height variation (single-row → varied). eae0fe5 player
tint + 7bd30d6 lane seating + de0344b 320→160×100 chunkier res all consistent.

## Architectural-supersession note (for the record — NOT a flag)

#44/#47 REVERTED the #36 fix's architecture I previously approved. render_ship now
sets `model = identity4()` and passes the stance `yaw_deg` as the CAMERA orbit angle
again (loft_gpu.rs:717-727) — explicitly replacing the #36/#37 fixed-camera +
model-rotation approach, which (per the new docstring) "collapsed bow-on to a plank
and went vertical on broadside." So the camera DOES orbit again — but with up=+Y
fixed (ship never tips vertical) and the #47 stance yaws chosen so each orbit angle
yields a clean profile, and bruce approved the resulting look. This is NOT the
resting-azimuth skew I diagnosed in #36 (that was an UNINTENDED 28° offset that
made broadside read wrong); this is an INTENDED orbit-per-stance the POC always
used. Consequence: my docs/reviews/loft_camera_stance.md (the #36/#47 "no orbit,
stance=model-rotation" verdict) is SUPERSEDED — the current design is camera-orbit,
model-identity. Flagging so the review record is honest about which architecture is
live; no action needed, the output is bruce-approved.

## Minor (non-blocking)

Stale comment at hud.rs:2182-2184: a test rationale still says "compose_scene gates
the per-ship overlay HUD off ... (SHOW_PLACEHOLDER_HUD = false)" but c1c1fda set it
true. The test itself is valid (asserts push_shield_pips directly, gate-independent),
but the comment now misstates the const. Cosmetic; fold into any next hud touch.

Net: the 8-commit arc is sound — scope-clean, the HUD re-anchor fully re-enabled +
correctly anchored, framing math correct, stance/planet changes coherent. APPROVE.
