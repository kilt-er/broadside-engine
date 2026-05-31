# `src/subsystems.rs` — runtime subsystem behavior

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/subsystems.rs`](../LINE_BY_LINE.md#srcsubsystemsrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

[`SubsystemDef`](types.md) is the catalog *wire shape* — "Marksman costs 15
salvage, max level 3." This module is the **behavioral** layer that turns "this
ship has Marksman installed" into actual damage / heat modifications at runtime.
It owns the per-ship installation registry and the two dispatch functions the
resolver calls.

The defining design decision is **why subsystems are content-side data, not
EventBus closures** (module docstring, src/subsystems.rs:7-39). The tempting
design — each subsystem registers an `FnMut(&mut HookContext)` on the bus — fails
on two counts:

1. **Pipeline ordering.** The damage modifier must apply at `apply_damage` step 2
   (`apply_modifiers`), but the resolver's `OnDamageDealt` hook fires once at the
   *end* of the queue, well after step 2. A bus closure couldn't hook the right
   moment without reordering the emit — a pipeline change the role boundary
   forbids.
2. **State ownership.** Each closure would need `Rc<RefCell<Registry>>` to know
   which ships have which subsystems — an Rc graph with surprising lifetimes, not
   thread-movable.

So subsystems live on the [`Content`](resolve.md) impl as plain data. The resolver
calls `Content::damage_modifier` at step 2 and `Content::on_turn_end` at
end-of-turn; each walks the installed list and does the math directly. No closures,
no Rc, no aliasing. No TS analog — the TS engine didn't have a subsystem layer yet.

---

## `type SubsystemId` + `struct Installations` (src/subsystems.rs:66, 70)

`SubsystemId` is just `String` (the catalog id), wrapped as a type alias so a
future "subsystem trees / variants" feature can swap storage without breaking
callers. `Installations` is `HashMap<ship_id → Vec<SubsystemId>>` — the Content
impl owns one. `install` (src/subsystems.rs:83) appends (order within the vec
doesn't matter — effects are commutative); `for_ship` (src/subsystems.rs:89)
returns the installed slice (empty if none). Pinned by
`installations_install_and_lookup` (src/subsystems.rs:246).

---

## The example subsystems (src/subsystems.rs:109–125)

Three `const` ids with documented effects:
- `MARKSMAN` — `+1` damage at `Long` band.
- `POINT_BLANK_DOCTRINE` — `+2` damage at `PointBlank` band (synergizes with
  bow-on stance).
- `HEAT_SINK` — `-1` extra heat at end of turn, stacking with itself and with the
  base passive dissipation (so a ship with HeatSink cools 2/turn instead of 1).

`SUBSYSTEM_IDS` (src/subsystems.rs:125) lists all three; adding a subsystem touches
the const, this list, and both dispatch fns.

---

## `fn damage_modifier_for(installed, attacker, band, board) -> i32` (src/subsystems.rs:142)

**Intent:** Step 2 of the damage pipeline — sum the per-hit damage bonus from every
subsystem installed on the **attacker**. Walk the installed ids; `MARKSMAN`
contributes `+1` iff `band == Long`, `POINT_BLANK_DOCTRINE` contributes `+2` iff
`band == PointBlank`; everything else 0. Additive across multiple subsystems.

**Drift / direction (audit #67):** modifiers are **attacker-side**. The catalog
descs read "+1 damage *when firing*" — attacker-frame verbs. Pre-audit code
consulted the *target's* subsystems and tests still passed only because each Phase-2
demo installed the same set on both sides; the audit fixed the direction and the
`DemoContent::damage_modifier` impl now passes the attacker's list.

**Cross-references:** Called by [`DemoContent::damage_modifier`](input.md), which
the resolver invokes at `apply_damage` step 2 ([`apply_modifiers`](resolve.md)).
**Worked examples:** `marksman_only_adds_at_long` (src/subsystems.rs:257),
`point_blank_doctrine_only_adds_at_point_blank` (src/subsystems.rs:269),
`multiple_subsystems_stack_additively` (src/subsystems.rs:279, 2×PBD at PB = 4).

---

## `fn on_turn_end_for(installations, board)` (src/subsystems.rs:168)

**Intent:** The end-of-turn pass. For each ship, sum its HeatSink count and subtract
that much extra heat (floored at 0), clearing lockout if the ship drops below
`heat_max` — matching the same invariant `end_of_turn` enforces for base
dissipation. Called by [`end_of_turn`](resolve.md) **after** base dissipation and
**before** the `OnTurnEnd` emit, so subscribers see already-cooled heat (matching
TS pipeline order).

**Cross-references:** Called by [`DemoContent::on_turn_end`](input.md) →
[`resolve::end_of_turn`](resolve.md). **Worked examples:**
`heat_sink_dissipates_one_extra_heat_per_turn_end` (src/subsystems.rs:296),
`heat_sink_clears_lockout_when_dropping_below_max` (src/subsystems.rs:306),
`heat_sink_floors_at_zero` (src/subsystems.rs:319), `heat_sink_stacks`
(src/subsystems.rs:329, two HeatSinks → -2), `ship_without_subsystems_is_untouched`
(src/subsystems.rs:340).
