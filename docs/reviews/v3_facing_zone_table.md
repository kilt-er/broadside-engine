# V3 reference — the correct `facing_zone(Facing, Dir8) -> HullZone` table

**Status:** PINNED authority for the V3 review (resolver R2). PRE-STAGED — resolver implements
to this; V3 checks the resolver's `facing_zone` against THIS table, not the blueprint's old
line-30 wording.

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

| incoming_from vs `dir`            | HullZone                          |
|-----------------------------------|-----------------------------------|
| within ±45° of `dir`              | **Bow**                           |
| within ±45° of `opposite(dir)`    | **Stern**                         |
| perpendicular, clockwise/right of `dir` | **Starboard**               |
| perpendicular, counter-clockwise/left of `dir` | **Port**               |
| diagonals                         | snap to nearest face by signed angle (clockwise step) |

Mechanically, with `dir` widened to `Dir8` and `s = (incoming.step() − dir.to_dir8().step()) mod 8`:
- `s == 0` → Bow; `s == 4` → Stern.
- `s ∈ {1, 2, 3}` → **Starboard** (clockwise / right side).
- `s ∈ {5, 6, 7}` → **Port** (counter-clockwise / left side).
- The exact diagonals `s ∈ {1,3,5,7}` are the "snap by signed angle" cases — they land on the
  flank on their side (1,3 → Starboard; 5,7 → Port). **V3 confirms R2's snap rule matches this
  handedness** and that a diagonal exactly between bow and a flank resolves deterministically
  (no 50/50 ambiguity left unhandled).

Sanity vs the 1-D engine: 1-D `Bow{bow:Fore}` with a fore hit → Bow, aft hit → Stern. In 2-D
that's `Bow(dir)` with on-axis-forward → Bow, on-axis-back → Stern — consistent. The 1-D flanks
"never took a lane hit" because the lane was 1-D; in 2-D a perpendicular hit now legitimately
lands on a flank, which is the intended new richness (decision: facing matters in 2-D).

---

## V3 checklist (against the resolver's R2 `facing_zone`)

- [ ] Signature is `facing_zone(facing: Facing, incoming_from: Dir8) -> HullZone` (Facing + Dir8,
      not the old `Orientation` + `LaneEnd`).
- [ ] **Broadside on-axis → Bow/Stern, perpendicular → Port/Starboard** (Table 1 — the
      corrected, NON-line-30 orientation). Both `EastWest` and `NorthSouth` covered.
- [ ] Broadside Bow-vs-Stern tiebreak is deterministic (positive-dir → Bow per `Axis::dirs()`?)
      and pinned by a test.
- [ ] Broadside Port-vs-Starboard handedness is deterministic + pinned (the open question above).
- [ ] **Bow(dir)**: ±45° → Bow, ±45°-of-opposite → Stern, clockwise-perp → Starboard,
      ccw-perp → Port (Table 2).
- [ ] **Diagonals snap by signed angle**, deterministically, with the handedness above — no
      unhandled exactly-between case.
- [ ] **Total**: every `Dir8 × Facing` (8 × {4 Bow dirs + 2 Broadside axes} = 48 combos) returns
      a defined zone. Exhaustive table test (this is T2's `facing_zone` coverage).
- [ ] **Self-consistent with `forward_axis()`**: the axis the ends lie on == `facing.forward_axis()`.
- [ ] Wired into `apply_damage` step 4 (`resolve.rs:837-841`) feeding `absorb_shield` — the
      damage-pipeline ORDER unchanged (V5 overlap; see V2 checklist §3).

---

*Authority: `grid.rs::Axis` + lead ruling 2026-06-14. Supersedes blueprint line 30
pre-correction. Cross-ref: V2 checklist §4 (facing faithfulness), T2 (exhaustive table test).*
