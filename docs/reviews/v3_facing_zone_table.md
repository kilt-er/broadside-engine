# V3 reference — the correct `facing_zone(Facing, Dir8) -> HullZone` table

**Status:** ✅ **V3 COMPLETE — APPROVE.** The resolver's `facing_zone` landed at `9ca7a9e`
(`src/geometry2d.rs`) and matches this pinned table exactly. Verdict log at the bottom. This
doc remains the authority; A3 wires the (stable, now-verified) table into the damage pipeline.

> ⚠️ This table was the authority resolver implemented to; V3 checked the impl against THIS,
> not the blueprint's old (inverted) line-30 wording.

> ⚠️ **The original blueprint line 30 was INVERTED** (lead's ruling, 2026-06-14, line corrected
> in the blueprint). For the **Broadside** case it had the on-axis/off-axis assignment
> backwards. **`grid.rs::Axis` (frozen, V1-approved) is the authority**; the resolver and this
> table implement to it. Do NOT check R2 against line-30's pre-correction text.

---

## The physics (the one rule everything follows)

**A hull running ALONG an axis presents its ENDS (Bow/Stern) along that axis, and its FLANKS
(Port/Starboard) perpendicular to it.**

A ship is a long thin shape. If the hull is laid out east–west, you see its broad side (flanks)
when you look from the north or south, and you see its nose/tail (ends) when you look from the
east or west. That is the whole derivation.

`grid.rs` Axis semantics (frozen — `grid.rs:344-361`):
- `Axis::EastWest` — "hull runs `E`↔`W`" (`row` fixed). `dirs() = (E, W)`.
- `Axis::NorthSouth` — "hull runs `N`↔`S`" (`col` fixed). `dirs() = (S, N)`.

`incoming_from` = the direction pointing **from the target back toward the attacker** (i.e.
where the shot comes from), as a `Dir8`.

---

## Table 1 — `Broadside(axis)`

Both flanks present outward symmetrically; ends lie along the hull axis.

| `Facing`                  | incoming_from (cardinal) | HullZone        |
|---------------------------|--------------------------|-----------------|
| `Broadside(EastWest)`     | `E` or `W`  (on-axis)    | **Bow / Stern** |
| `Broadside(EastWest)`     | `N` or `S`  (perpendicular) | **Port / Starboard** (flank) |
| `Broadside(NorthSouth)`   | `N` or `S`  (on-axis)    | **Bow / Stern** |
| `Broadside(NorthSouth)`   | `E` or `W`  (perpendicular) | **Port / Starboard** (flank) |

So: **on-axis → end (Bow/Stern); perpendicular → flank (Port/Starboard).** This is the
*opposite* of line-30's old "on-axis → Port/Starboard" — the correction.

### OPEN QUESTION for V3 (flag, don't assume)

The ruling fixes which **pair** (ends vs flanks) each direction hits, but a symmetric broadside
hull has **no inherent forward**, so the within-pair assignment needs a deterministic tiebreak
that the ruling doesn't fully pin:
1. **Bow vs Stern** for the two on-axis directions — e.g. `Broadside(EastWest)`: is `E → Bow`
   and `W → Stern`, or the reverse? `grid.rs Axis::dirs()` returns `(positive, negative)` =
   `(E, W)`, so the natural convention is **positive-dir → Bow, negative-dir → Stern** — but
   confirm R2 actually uses that and a test pins it.
2. **Port vs Starboard** for the two perpendicular directions — needs a fixed handedness. With
   no bow vector to take "right-of," this must be an arbitrary-but-stable rule (e.g. derived
   from `Axis::dirs().0` as a pseudo-forward, then right/left of it). **V3 must confirm R2
   picks one deterministically and pins it** — the old 1-D code had the analogous
   fore→starboard/aft→port stable split, so the 2-D version owes the same determinism.

Whatever R2 chooses, the bar is: **total + deterministic + unit-tested for every
`Dir8 × Facing`**, and self-consistent with `forward_axis()` (so the renderer's bow-arrow,
which uses `forward_axis`, points along the same axis the ends lie on).

---

## Table 2 — `Bow(dir)`

The nose points at cardinal `dir`; there IS a forward vector, so handedness is well-defined.
**The split is 3/3/1/1, ±45° INCLUSIVE** (lead correction 2026-06-14): "perpendicular" =
±90° **only** (exactly one incoming each), so Port/Starboard get the single perpendicular
direction each and the ±45° **diagonals belong to Bow/Stern**.

| incoming_from vs `dir`                          | HullZone        |
|-------------------------------------------------|-----------------|
| within ±45° of `dir` **inclusive** (3 sectors)  | **Bow**         |
| within ±45° of `opposite(dir)` **inclusive** (3 sectors) | **Stern** |
| exactly perpendicular (±90°), clockwise/right of `dir`   | **Starboard** (1 sector) |
| exactly perpendicular (±90°), counter-clockwise/left of `dir` | **Port** (1 sector) |

Mechanically, with `dir` widened to `Dir8` and `s = (incoming.step() − dir.to_dir8().step()) mod 8`:
- `s ∈ {7, 0, 1}` → **Bow** (the cardinal `dir` plus the two ±45° diagonals flanking it).
- `s ∈ {3, 4, 5}` → **Stern** (`opposite(dir)` plus its two ±45° diagonals).
- `s == 2` → **Starboard** (the single clockwise/right perpendicular).
- `s == 6` → **Port** (the single counter-clockwise/left perpendicular).

So the four ±45° diagonals **snap to the END (Bow/Stern), not the flank**: `s=1` and `s=7` →
Bow (the two diagonals adjacent to `dir`), `s=3` and `s=5` → Stern (the two adjacent to
`opposite(dir)`). This is the "snap to nearest face by signed angle" rule resolving in favour of
the nearer end, because a ±45° offset is closer to the on-axis end than to the ±90° flank. Only
the exact ±90° directions (`s=2`, `s=6`) hit the flanks. **V3 confirms R2 implements this
3/3/1/1 split** (the resolver's impl is already 3/3/1/1 — correct) and that every `s ∈ 0..8` is
handled with no ambiguity.

Sanity vs the 1-D engine: 1-D `Bow{bow:Fore}` with a fore hit → Bow, aft hit → Stern. In 2-D
that's `Bow(dir)` with on-axis-forward → Bow, on-axis-back → Stern — consistent. The 1-D flanks
"never took a lane hit" because the lane was 1-D; in 2-D a perpendicular hit now legitimately
lands on a flank, which is the intended new richness (decision: facing matters in 2-D).

---

## V3 checklist (against the resolver's R2 `facing_zone`)

- [x] Signature is `facing_zone(facing: Facing, incoming_from: Dir8) -> HullZone` (Facing + Dir8,
      not the old `Orientation` + `LaneEnd`). ✓
- [x] **Broadside on-axis → Bow/Stern, perpendicular → Port/Starboard** (Table 1 — the
      corrected, NON-line-30 orientation). Both `EastWest` and `NorthSouth` covered. ✓
- [x] Broadside Bow-vs-Stern tiebreak deterministic (pseudo-forward = `Axis::dirs().0` → Bow)
      and pinned by `..._cardinals` tests. ✓
- [x] Broadside Port-vs-Starboard handedness deterministic + pinned. **Lead-confirmed rule
      (2026-06-14):** pseudo-forward = `Axis::dirs().0` → Bow; right-of-pseudo-forward → Starboard.
      R2 uses exactly this; T2's partition test pins it. ✓
- [x] **Bow(dir) is 3/3/1/1, ±45° INCLUSIVE** (Table 2): `s∈{7,0,1}→Bow`, `s∈{3,4,5}→Stern`,
      `s=2→Starboard`, `s=6→Port`. The ±45° diagonals snap to the END, NOT the flank. ✓
- [x] **Only exact ±90° hits a flank**; the four diagonals (`s∈{1,3,5,7}`) snap to Bow/Stern by
      nearest-end — no unhandled exactly-between case. (Bow stance only; Broadside diagonals are
      a true tie and snap 45° CW — see verdict.)
- [x] **Total**: every `Dir8 × Facing` (8 × {4 Bow dirs + 2 Broadside axes} = 48 combos) returns
      a defined zone. Exhaustive table test (this is T2's `facing_zone` coverage).
- [x] **Self-consistent with `forward_axis()`**: the axis the ends lie on == `facing.forward_axis()`.
- [ ] Wired into `apply_damage` step 4 feeding `absorb_shield` — the damage-pipeline ORDER
      unchanged. **Deferred to A3/R4** (correctly NOT in this commit); re-check at the damage-wiring
      commit (V5 overlap; see V2 checklist §3).

---

## V3 verdict — `9ca7a9e` (`src/geometry2d.rs`) — **APPROVE**

The resolver's `facing_zone`/`bow_zone`/`broadside_zone` match this table line-for-line.
Verified against the impl (`rel = (incoming.step + 8 − anchor.step) % 8`):

- **Bow** (`bow_zone`): `rel∈{7,0,1}→Bow`, `rel=2→Starboard`, `rel∈{3,4,5}→Stern`, `rel=6→Port`.
  Exactly the pinned **3/3/1/1, ±45° inclusive** split. ✓
- **Broadside** (`broadside_zone`, anchor = pseudo-forward = `axis.dirs().0`):
  `rel∈{7,0}→Bow`, `rel∈{1,2}→Starboard`, `rel∈{3,4}→Stern`, `rel∈{5,6}→Port`. Clean **2/2/2/2**:
  on-axis ends → Bow/Stern, perpendicular flanks → Port/Starboard, diagonals snap 45° CW. ✓
- **Handedness verified physically, not just internally:** "CW-in-step = physical right =
  Starboard" holds for all four bow cardinals (N→right=E, E→right=S, S→right=W, W→right=N). The
  NorthSouth-vs-EastWest "flip" the resolver's test comments note is **correct, not a bug** —
  facing S your right hand is W, so W=Starboard is right. Same uniform rule, opposite facing.
- **Totality:** both partition tests (`..._three_three_one_one`, `..._two_two_two_two`) iterate
  all 48 `Dir8 × Facing` combos; the `unreachable!` arms are genuinely unreachable (`% 8`). ✓
- **`forward_axis()` consistency:** Broadside ends lie on `axis`; `forward_axis(Broadside(axis))
  = axis` — consistent, so the renderer's bow-arrow (D4) and this table agree. ✓
- **25 tests pass; `clippy --all-targets` clean** (the 7 warnings I'd seen at the A3.1 snapshot
  were pre-`9ca7a9e` and are now gone — the current tree has zero).

Beyond strict V3 scope but noted (this commit is the whole R1 surface; deep coverage is T2):
`direction_to` magnitude-aware snap with the documented lower-`step` tiebreak is sound and
deterministic (load-bearing for telegraph≡shot); `band_falloff [1.0,0.6,0.3]` is correctly keyed
on the **actual** band (absolute curve per decision #6, distinct from the 1-D distance-from-
optimal delta); `in_band` enforces the decision-#7 deadzone; `arc_bears` cones reuse the same
±45° model as `facing_zone` (one consistent angular model for "my gun bears" and "my face is hit").

**Wiring into `apply_damage` step 4 is correctly NOT in this commit** — it's A3/R4, where the
attacker→target `direction_to` will feed `incoming_from`. The table is stable + verified now, as
the blueprint mandated ("pin + unit-test BEFORE the rewrite").

---

## V3 amendment — `arc_bears` cardinal-exact fix (`f1db141`)

**At `9ca7a9e` I approved `arc_bears` with a ±45° cone (`within_45`) and praised it for "reusing
the same angular model as `facing_zone`." That was a mistake** — the tester's T2 caught it. Firing
and receiving have DIFFERENT arities (the firing-direction contract): `arc_bears` is the FIRING
gate (cardinal-exact, 4-way), `facing_zone` is the RECEIVING table (8-way). The cone let a
`Broadside(NorthSouth)` battery "bear" toward SE while `facing_zone` snaps SE to a bow/stern face
(not a flank) — an inconsistency. `f1db141` changed `within_45` → exact cardinal match in
`arc_bears` (Forward `toward==dir`, Rear `==opposite`, BroadsideArc `==either flank cardinal`;
Turret always; all diagonals → false) and removed the dead `within_45`. **`facing_zone` itself is
UNCHANGED** — the V3 table above stands.

**Consistency property — VERIFIED BY FULL ENUMERATION** (all 6 facings × 4 cardinals = 24 cases):
at every cardinal `C`, `arc_bears(facing, arc, C)` is true for the arc whose face equals
`facing_zone(facing, C)`, and false otherwise:
- `facing_zone → Bow`  ⇔ `Forward` bears (Bow stance).
- `facing_zone → Stern` ⇔ `Rear` bears (Bow stance).
- `facing_zone → Port/Starboard (flank)` ⇔ `BroadsideArc` bears (Broadside stance).
- `facing_zone → an END on a Broadside` ⇔ NO arc bears (a broadside can't fire out its ends).

No contradiction in any case. The diagonal divergence is intended: `facing_zone` is total over 8
(receives off-axis BLAST splash / ordnance), `arc_bears` is cardinal-only (you can't FIRE
diagonally). T2 pins this as invariants (`firing_arcs_are_strictly_narrower_than_the_receiving_
sectors`, `no_arc_ever_bears_on_a_diagonal_under_cardinals_only_firing`). 25 unit + 42 T2 green;
clippy clean. **arc_bears↔facing_zone consistency item: CLOSED — re-bless granted.**

---

*Authority: `grid.rs::Axis` + lead ruling 2026-06-14. Supersedes blueprint line 30
pre-correction. Cross-ref: V2 checklist §4 (facing faithfulness), T2 (exhaustive table test),
V4 (resolve_targeting single-source — R3/#9, not in this commit). V3 done @ `9ca7a9e`;
arc_bears amendment @ `f1db141`.*
