# `src/catalog_canonical.rs` — canonical → strict catalog transformer

*A line-by-line walkthrough scoped to one file. This is the companion to the
[`src/catalog_canonical.rs`](../LINE_BY_LINE.md#srccatalog_canonicalrs) section
of `LINE_BY_LINE.md`; the two are kept in sync.*

---

## Why this module exists

The Broadside content catalog has **two on-disk shapes**, and this module is the
bridge between them.

1. **The strict shape** — the engine's native format. Every field the resolver
   needs is present and nested exactly as the `Catalog` types in
   [`src/types.rs`](types.md) expect. This is what the engine *would* emit if it
   serialized its own runtime state.

2. **The canonical shape** — what the design document
   (`broadside-analysis.html`) actually produces. The analysis HTML has a "Copy
   JSON" button; what it copies is a *flat*, human-authored shape with terse
   field names (`heat`, `cd`, `band`, `pattern`, `arc`, `freeplay`) and
   `effects` listed as **bare strings** (`["DAMAGE"]`) rather than fully-formed
   effect records.

The canonical shape is the source of truth bruce edits by hand in the analysis
doc. Rather than force the design doc to emit verbose engine JSON, this module
*infers* the missing structure: it walks the loose `serde_json::Value` tree,
renames fields, nests them into `cost`/`targeting` sub-objects, and inflates
each bare-string effect into a typed record using documented defaults. The
output is a `Catalog` that is byte-for-byte indistinguishable from one parsed
out of strict JSON, so **the resolver never learns there were two formats.**

The dispatch lives in [`src/catalog.rs`](catalog.md): `load_from_bytes` tries
strict first (fast path), and on parse failure falls back to
`from_canonical_value` here. There is no `Mirrors:` for this module — TypeScript
loads its catalog inline in `demo.ts` and never grew a loose/strict split. This
is **Rust-specific glue** born of the decision to consume the analysis HTML's
export directly.

### The flat → nested shape, concretely

The canonical export emits an action like this:

```json
{ "id": "pulse_laser", "heat": 1, "cd": 0, "band": "close",
  "pattern": "BEAM", "arc": "forward", "freeplay": false,
  "effects": ["DAMAGE"] }
```

The strict types expect:

```json
{ "id": "pulse_laser",
  "cost":      { "heat": 1, "cooldownMax": 0, "advancesTurn": true },
  "targeting": { "pattern": "BEAM", "optimalBand": "close",
                 "band": ["close"], "requiresArc": "forward",
                 "facingRelative": true, "hitsAll": false },
  "effects":   [{ "kind": "DAMAGE", "amount": 3 }] }
```

Every difference between those two blocks is something this module synthesizes.

---

## Inference rules at a glance

These are the documented defaults the module applies. They are deliberately
*conservative* — the brief was "sensible defaults you document in comments,"
preferring under-tuned values that playtesting can flag over magic numbers
scraped out of prose `desc` fields.

| Canonical field / signal | Strict field | Rule |
|---|---|---|
| `heat` | `cost.heat` | direct rename |
| `cd` | `cost.cooldownMax` | direct rename |
| `freeplay` | `cost.advancesTurn` | **negated**: `advancesTurn = !freeplay` |
| `band` | `targeting.optimalBand` | direct rename |
| `band` | `targeting.band` | seeded as a **single-element array** `[band]` |
| `pattern` | `targeting.pattern` | direct rename |
| `arc` | `targeting.requiresArc` | passthrough; `null` if absent |
| (always) | `targeting.facingRelative` | hardcoded `true` |
| `hits_all` / `hitsAll` | `targeting.hitsAll` | default `false` |
| `unlock` (subsystem) | `unlockSalvage` | rename |
| (missing) | subsystem `level` | default `1` |
| `affinity: "bow-on"` | `affinity: "bowOn"` | hyphen → camelCase |
| class `set1`/`set2` display names | action ids | looked up via `action_name_to_id` |
| class `signature` prose | signature id | leading title before the dash, snake_cased |

The single-element `band` array deserves emphasis: the canonical shape names
only the *optimal* band, not the full set of allowed bands. The conservative
read is that a weapon fires **only** at its optimal range, so the strict `band`
array is seeded with just that one entry. The resolver's "is this band allowed?"
gate then accepts only that band. When a real allowed-bands field lands in the
export, this can widen — until then it is intentionally narrow.

---

## `fn from_canonical_value(root: Value) -> Result<Catalog, serde_json::Error>` (src/catalog_canonical.rs:70)

**Intent:** The single public entry point. Takes the loose root JSON object,
transforms the three sections that have structural drift (`actions`,
`subsystems`, `classes`), leaves every other section untouched, then hands the
rebuilt `Value` tree to `serde_json::from_value` for the final typed decode.
Returns the same `serde_json::Error` any conversion would surface, so the caller
in [`catalog.rs`](catalog.md) can fold it into `LoadError::Parse`.

Line 71-74: `match root { Value::Object(o) => o, other => return serde_json::from_value(other) }`
— peel the root down to its object map. If the root *isn't* an object, this
clearly isn't the canonical shape; rather than guess, hand the value straight to
serde so the error message describes the real type mismatch. This is the
"degrade to serde's own diagnostics" pattern used throughout the module.

Line 84: `let mut action_name_to_id: HashMap<String, String> = HashMap::new();`
— declares the lookup the class normalizer needs. Built *before* classes are
transformed, populated *during* action transformation. This ordering constraint
is the whole reason actions are processed first (task #82).

Line 85-102: the **actions** block. `obj.remove("actions")` pulls the array out
(so it can be re-inserted transformed). Each element runs through
`transform_action`; `filter_map(...ok())` **silently drops** any action that
fails to transform — losing one weapon is better than failing the entire
catalog load. After transforming, lines 93-100 walk the transformed actions and
populate `action_name_to_id` with `name.to_lowercase() -> id`. The case-fold
means `"Twin-Linked"` and `"twin-linked"` both resolve to the same id. Line 101
re-inserts the transformed array.

Line 103-109: the **subsystems** block. Simpler — `transform_subsystem` is
infallible (returns `Value` directly, never an error), so a plain `map` suffices.

Line 110-116: the **classes** block. Each class runs through `transform_class`,
which borrows the `action_name_to_id` map built above. This is why actions had
to go first.

Line 118-121: a comment, not code — documents that the canonical export carries
top-level `archetypes` and `bays` keys (UI metadata) that the strict `Catalog`
doesn't model. Because the `Catalog` struct does **not** use
`#[serde(deny_unknown_fields)]`, serde silently ignores them; the comment exists
so the next reader doesn't go hunting for where they're stripped (they aren't).

Line 123-124: `let rebuilt = Value::Object(obj); serde_json::from_value(rebuilt)`
— reassemble the mutated map into a `Value` and do the real typed decode. Any
remaining shape error (an effect kind the engine doesn't know, a malformed
nested field) surfaces here as a `serde_json::Error`.

**Cross-references:** Called by `load_from_bytes` ([catalog.rs](catalog.md))
as the canonical fallback. Calls `transform_action`, `transform_subsystem`,
`transform_class`. Produces a [`Catalog`](types.md) consumed by the resolver's
content layer.

---

## `fn transform_action(v: Value) -> Result<Value, &'static str>` (src/catalog_canonical.rs:134)

**Intent:** Convert one flat action object into the strict nested shape. Returns
`Err(&'static str)` on any missing *required* field so the caller's `filter_map`
can skip it; the canonical export always supplies these fields, so an error here
signals genuinely malformed input that's better dropped than fatal.

Line 135-137: `let Value::Object(mut a) = v else { return Err("not an object") };`
— a let-else; bail immediately if the array element isn't an object.

Line 141-147: pull and rename the **required** scalar fields. `heat` and `cd`
are read as `i64` (the JSON-native integer width), and missing either is an
`Err`. `band` and `pattern` are read as owned `String`s; missing either is an
`Err`. Each uses `remove` (not `get`) so the original flat keys don't survive
into the strict object.

Line 143: `freeplay` defaults to `false` when absent — a weapon with no
`freeplay` flag is assumed to advance the turn (the common case).

Line 148: `let arc = a.remove("arc");` — `arc` may be `null` or absent for
arc-less actions (e.g. self-targeting moves), so it's kept as a raw `Option<Value>`
and normalized later rather than required.

Line 149-152: `hits_all` accepts either snake_case `hits_all` or camelCase
`hitsAll` (canonical exports have drifted on this), defaulting to `false`. Only
`SPINAL_LINE` patterns pierce all targets, and only when explicitly flagged.

Line 154-176: **effect inflation.** `archetype` (defaulting to `"beam"`) and the
action `id` are read as hints. Line 169 runs the
[`rewrite_self_relative_signature`](#fn-rewrite_self_relative_signatureaction_id-str-effects-value---value-srccatalog_canonicalrs397)
pre-pass on the loose effects (remapping the four dead self-relative class
signatures from `DISPLACE_TARGET` to `DISPLACE_SELF` before inflation). The
resulting bare-string `effects` array is then mapped through `inflate_effect`,
which turns each string into a typed effect record. A non-array `effects` value is
wrapped in a one-element vec so serde can report the shape error downstream.

Line 167-172: build the **`cost`** sub-object: `{ heat, cooldownMax: cd,
advancesTurn: !freeplay }`. The negation is the one non-obvious rename — a
"freeplay" action is one that does *not* advance the turn clock.

Line 174-193: build the **`targeting`** sub-object. Note line 182-183: `band` is
seeded as a single-element array `[band]` while `optimalBand` gets the same band
scalar — the conservative single-band read described above. Line 184-190
normalizes `arc`: a real string becomes `requiresArc: "<arc>"`, anything else
(including the `null`/absent case) becomes JSON `null`. Line 191 hardcodes
`facingRelative: true` — the canonical engine treats all targeting as
facing-relative by default.

Line 197: `a.remove("desc");` — strip the UI-only description. The strict
`Action` type has no `desc` field; leaving it would be harmless (no
`deny_unknown_fields`) but removing it keeps the rebuilt object lean.

Line 199: `Ok(Value::Object(a))` — the now-strict action object.

**Cross-references:** Called by `from_canonical_value`. Calls `inflate_effect`.
Produces objects decoded into [`Action`](types.md) (with nested `ActionCost` and
`Targeting`).

**Worked example** (`canonical_pulse_laser_parses`, src/catalog_canonical.rs:617):
the flat `pulse_laser` above decodes to `cost.heat == 1`, `cost.cooldown_max == 0`,
`cost.advances_turn == true` (because `freeplay: false`), and a single
`Effect::DAMAGE { amount: 3 }` — the `beam + heat 1 → heat + 2 = 3` inflation rule.

---

## `fn transform_subsystem(v: Value) -> Value` (src/catalog_canonical.rs:205)

**Intent:** Flat subsystem → strict subsystem. Infallible (returns `Value`), so
a malformed subsystem passes through unchanged rather than being dropped. Two
real drifts only.

Line 206: let-else; a non-object passes through verbatim.

Line 209-211: `unlock → unlockSalvage` rename, **value preserved**. A `null`
unlock stays `null` (which deserializes to `unlock_salvage: None`); a salvage
cost integer stays the integer.

Line 213: `s.entry("level").or_insert(Value::from(1));` — the canonical shape
omits `level`; the strict shape requires it. Default to `1` (the base tier).

Line 215: `s.remove("desc");` — drop the UI-only description.

**Cross-references:** Called by `from_canonical_value`. Produces a
[`SubsystemDef`](types.md).

**Worked example** (`subsystem_unlock_renames_to_unlock_salvage_and_level_defaults`,
src/catalog_canonical.rs:700): a `marksman` subsystem with `"unlock": null` and
no `level` decodes to `unlock_salvage == None`, `level == 1`, `max_level == 3`.

---

## `fn transform_class(v: Value, action_name_to_id: &HashMap<String, String>) -> Value` (src/catalog_canonical.rs:237)

**Intent:** Flat class → strict class. Three drifts: affinity rename, set1/set2
display-name → action-id normalization (task #82), and signature-prose → id
derivation (task #82). Infallible; a non-object passes through.

Line 240: capture `class_id` up front (defaulting to `"?"`) purely for warning
messages.

Line 242-248: **affinity rename.** Only `"bow-on"` needs rewriting to `"bowOn"`
(the strict [`ClassAffinity`](types.md) deserializes camelCase); `"flexible"`
and `"broadside"` already match, so `other => other` passes them through.

Line 251-259: **set1 / set2 normalization.** The canonical shape lists action
*display names* (`"Broadside Battery"`); the engine expects action *ids*
(`"broadside_battery"`). Each entry runs through `normalize_action_ref` against
the lookup built in `from_canonical_value`. Both sets share one loop.

Line 262-273: **signature derivation.** The canonical `signature` is prose
(`"Slip — move forward to trade places…"`). `signature_id_from_prose` extracts
the leading title and snake-cases it. On parse failure (empty id) line 265-269
logs an `eprintln!` and falls back to the raw prose so the load doesn't lose the
field; on success the derived id replaces it.

**Cross-references:** Called by `from_canonical_value`. Calls
`normalize_action_ref` and `signature_id_from_prose`. Produces a
[`ClassDef`](types.md).

**Worked example** (`canonical_class_normalizes_set_refs_and_signature`,
src/catalog_canonical.rs:995): the `wanderer` class with display-name sets and a
prose signature decodes to `set1 == ["broadside_battery", "pulse_laser"]`,
`set2 == ["railgun_broadside", "grav_snare"]`, and `signature == "slip"`.

---

## `fn normalize_action_ref(...) -> Value` (src/catalog_canonical.rs:281)

**Intent:** Resolve a single set-ref entry. If it's a display name found in the
lookup, return the id; if it's already a snake_case id, skip the lookup; if it's
an unmapped display name, log a warning and pass it through (the resolver will
silently skip an unknown ref — better than failing the load over a typo).

Line 287-289: let-else; a non-string entry (already an id object, or some other
type) passes through.

Line 291-293: **skip-if-already-an-id** guard. If the string is entirely
lowercase ASCII alphanumerics and underscores, it already looks like a
snake_case id — return it untouched. This lets *hybrid* catalogs (some loose,
some strict) work without the normalizer over-rewriting ids that don't need it.

Line 294-304: case-folded lookup. A hit returns the mapped id; a miss logs the
class id, the field name, and the offending display name, then passes the
original through.

**Cross-references:** Called by `transform_class`. Reads the `action_name_to_id`
map built in `from_canonical_value`.

**Worked examples:** `unmapped_set_ref_passes_through` (src/catalog_canonical.rs:1051)
— `"Ghost Weapon"` has no id, stays verbatim. `snake_case_set_ref_skips_lookup`
(src/catalog_canonical.rs:1083) — `"pulse_laser"` is already an id, skips the lookup.

---

## `fn signature_id_from_prose(prose: &str) -> String` (src/catalog_canonical.rs:317)

**Intent:** Pull a snake_case id out of a Signature prose string. Canonical
format is `"<Title> — <description>"` (em-dash U+2014) or `"<Title> - <description>"`
(ASCII hyphen with spaces). The leading title is the human name; lowercasing it
and converting spaces to underscores yields the id. Returns the empty string on
failure so the caller can decide to fall back to raw prose.

Line 320-324: split on em-dash first (the canonical separator), then on `" - "`
(degraded ASCII exports), else treat the whole string as the title.

Line 325-328: trim; an empty title returns `""` immediately.

Line 331-344: the snake_case loop. ASCII alphanumerics are lowercased and
appended; whitespace, `-`, and `_` collapse into a single `_` (the
`prev_underscore` flag suppresses runs and leading underscores); all other
punctuation is dropped silently.

Line 346-348: strip any trailing underscore so `"Ram The Target."` → `"ram_the_target"`,
not `"ram_the_target_"`.

**Cross-references:** Called by `transform_class`.

**Worked examples** (src/catalog_canonical.rs:959): `"Slip — move forward…"` → `"slip"`;
`"Swap Toss — move into a ship…"` → `"swap_toss"`; `"Phase - move forward…"` →
`"phase"`; `"Ram The Target"` → `"ram_the_target"`; `""`, `"   "`, and `"—"` all
→ `""`.

---

## `fn rewrite_self_relative_signature(action_id: &str, effects: Value) -> Value` (src/catalog_canonical.rs:397)

**Intent:** A **pre-pass** run by `transform_action` (src/catalog_canonical.rs:169)
*before* `inflate_effect`, added by the #84 follow-up fix to repair four
mechanically-dead class signatures.

The problem it solves: the canonical export ships `slip` / `swap_toss` / `ram` /
`throw` with `pattern: SELF` **and** a `DISPLACE_TARGET` effect string. That
combination is dead — `resolve_targeting` returns `[ship_cell]` for a `SELF`
pattern, so a `DISPLACE_TARGET` runs against the source ship itself: a no-op for
the swap modes, a wrong-direction self-shove for the rams (and the trailing
`DAMAGE` then strikes the now-vacated origin cell and hits nothing). The
canonical *prose*, though, is self-relative ("move forward to trade places…",
"shove the ship ahead…"), so the faithful representation is a `DISPLACE_SELF`,
which the resolver's `resolve_self_move` implements correctly.

Line 375, 382: two id sets — `SELF_RELATIVE_SWAP_SIGNATURES = ["slip",
"swap_toss"]` and `SELF_RELATIVE_RAM_SIGNATURES = ["ram", "throw"]`. Line 398-403:
if the action id is in neither set, pass the effects through untouched. Line
404-417: for the four signature ids, rewrite the loose effect list —
`"DISPLACE_TARGET"` becomes `"DISPLACE_SELF"`, and for the ram-style ids the
trailing `"DAMAGE"` is **dropped** (collision billing inside `resolve_self_move`
owns that damage; a separate `SELF`-pattern `DAMAGE` would hit the empty origin).
The actual movement-mode mapping (`TRACTOR_SWAP` / `BURN`) is applied downstream
by `inflate_effect`'s `DISPLACE_SELF` arm, keyed off the same id. Already-strict
(object) effects are left as-is.

**Cross-references:** Called by `transform_action` at src/catalog_canonical.rs:169.
Its `DISPLACE_SELF` output is then mode-mapped by `inflate_effect` (below).
Produces effects the resolver's `resolve_self_move` runs faithfully (TRACTOR_SWAP
for the swaps, BURN-with-collision-billing for the rams).

**Drift — `swap_toss` is a faithful subset.** The canonical prose for `swap_toss`
wants a *two-sided* swap (trade the cells directly fore AND aft around a
stationary centre). The engine has no effect for that, so the loader uses the
single bow-side `TRACTOR_SWAP` — the faithful subset. Flagged to the lead for
whether a two-sided swap effect is worth adding (src/catalog_canonical.rs:532-538).

**Worked example:** `signature_actions_change_board_state_through_resolver`
(src/catalog_canonical.rs:1165) fires each signature through the real resolver and
asserts the board actually changed — the regression that proves these four are no
longer dead.

---

## `fn inflate_effect(v: Value, archetype: &str, heat: i32, action_id: &str) -> Value` (src/catalog_canonical.rs:437)

**Intent:** Turn one bare-string effect (`"DAMAGE"`) into a strict
`kind`-tagged effect object, inferring the per-variant fields from the action's
archetype, heat, and id. Effects that are *already* objects pass through
untouched (hybrid-catalog support).

Line 369-374: extract the effect `kind`. An object is already strict — return
it. A string is the kind. Anything else returns verbatim so serde fails clearly.

Line 376-377: seed the output map with `kind`.

Line 379-501: the big `match kind` — one arm per effect verb. Each arm fills the
fields the strict [`Effect`](types.md) variant requires:

- **`DAMAGE`** (380-399): `amount` by archetype tier. Direct-damage archetypes
  (`beam`, `broadside`) scale with heat (`heat + 2`); `ordnance` contributes `0`
  (the damage rides the projectile, not the launcher); `displacement`/`control`
  give a small fixed `2`; everything else gives `max(heat, 1)`. `bandFalloff` is
  deliberately omitted so the strict `None` default (apply falloff) holds.
- **`APPLY_STATUS`** (400-416): `status` by archetype — `ordnance` and
  `displacement`/`control` apply `systemsOffline`; everything else applies
  `hullBreach`. `duration` defaults to `3`.
- **`DISPLACE_TARGET`** (486-519): `mode` by **id keyword** — `tractor+toss` →
  `swap`; `tractor`/`pull` → `pull`; the repulsor/push/snare family → `push`;
  default `push`. `distance` defaults to `2`. After the #84 follow-up fix this arm
  only ever sees **genuine target-displacement** actions (`tractor_beam`,
  `repulsor`, `grav_snare`, `tractor_toss`); the four self-relative class
  signatures (`slip`/`swap_toss`/`ram`/`throw`) are rewritten to `DISPLACE_SELF`
  *before* inflation by `rewrite_self_relative_signature` (see below), so the
  `slip`/`swap_toss` branch here is now dead — kept only as a harmless guard if a
  future export drops the rewrite.
- **`DISPLACE_SELF`** (520-563): `mode`/`distance` by id keyword —
  `phase` → `("SLIP", 2)` (pass through the ship ahead, landing in the first free
  cell); `slip`/`swap_toss` → `("TRACTOR_SWAP", 1)` (swap with the bow-adjacent
  occupant); `ram`/`throw` → `("BURN", 2)` (a self-burn that the resolver's
  `resolve_self_move` bills collision damage on when blocked by the ship ahead —
  so the collision *is* the damage); everything else → `("THRUST", 1)`. `direction`
  is omitted (orientation-relative) for all modes **except `throw`**, which sets
  `direction: "aft"` ("hurl the ship behind you") so it heads sternward regardless
  of stance.
- **`REORIENT`** (464-466): `to: "flip"`.
- **`SPAWN_ORDNANCE`** (467-473): `projectile` defaults to the action id;
  `Content::spawn_projectile` looks it up at runtime.
- **`VENT_HEAT`** (474-479): `amount: 3` (the canonical Vent value),
  `rechargeCooldowns: false`.
- **`DEPLOY`** (480-490): `hazard` is `drone` if the id contains "drone", else
  `mine`.
- **`BOARD`** (491-495): `note` defaults to the action id so
  `Content::apply_board_effect` can dispatch on it.
- **`_`** (496-500): unknown kind — emit just `{ kind }` and let serde fail
  downstream, surfacing the drift loudly rather than silently swallowing it.

The closing `Value::Object(m)` returns the inflated strict effect.

**Cross-references:** Called by `transform_action` (after
`rewrite_self_relative_signature` has already remapped the self-relative
signature effects). Produces objects decoded into [`Effect`](types.md) variants
consumed by the resolver's `apply_damage` / effect dispatch / `resolve_self_move`.

**Worked examples:**
- `ordnance_apply_status_infers_systems_offline` (src/catalog_canonical.rs:720) —
  Heavy Torpedo's `["SPAWN_ORDNANCE", "APPLY_STATUS"]` yields
  `APPLY_STATUS { status: SystemsOffline, duration: 3 }`.
- `tractor_beam_displace_infers_pull` (src/catalog_canonical.rs:747) — `tractor_beam`
  → `DISPLACE_TARGET { mode: Pull }`.
- `repulsor_displace_infers_push` (src/catalog_canonical.rs:772) — `repulsor` → `Push`.
- `tractor_toss_infers_swap` (src/catalog_canonical.rs:884) — `tractor_toss` (a
  genuine target-displacement) → `DISPLACE_TARGET { mode: Swap }`.
- `slip_inflates_to_self_tractor_swap` (src/catalog_canonical.rs:796) and
  `swap_toss_inflates_to_self_tractor_swap` (src/catalog_canonical.rs:827) — both
  the rewritten `DISPLACE_SELF` strings inflate to
  `DISPLACE_SELF { mode: TRACTOR_SWAP }`. (Pre-#84-fix these were named
  `*_infers_swap` and asserted the dead `DISPLACE_TARGET { Swap }`.)
- `phase_infers_slip_movement_mode` (src/catalog_canonical.rs:856) — `phase` →
  `DISPLACE_SELF { mode: SLIP }`.
- `self_relative_signatures_inflate_to_displace_self` (src/catalog_canonical.rs:1116) —
  the consolidated pin: `slip`/`swap_toss` → `DISPLACE_SELF{TRACTOR_SWAP}` (no
  trailing effect), `ram` → `DISPLACE_SELF{BURN, direction: None}` (bow-relative),
  `throw` → `DISPLACE_SELF{BURN, direction: Some(Aft)}`, `phase` stays
  `DISPLACE_SELF{SLIP}`.
- `signature_actions_change_board_state_through_resolver` (src/catalog_canonical.rs:1165) —
  the end-to-end proof: firing each signature through the actual resolver mutates
  board state (the whole point of the #84 fix — they used to no-op).
- `already_strict_effect_passes_through` (src/catalog_canonical.rs:935) — a pre-built
  `{ kind: DAMAGE, amount: 99 }` survives inflation untouched.

---

## Drift notes

**Drift — no TS analog.** TypeScript loads its catalog inline in `demo.ts` from
a single hand-authored object literal; it never grew a loose/strict format
split, so there is nothing to mirror. This module is a Rust-side adapter that
exists solely so the engine can eat the analysis HTML's "Copy JSON" export
directly.

**Drift — single-band targeting.** The canonical shape names only the optimal
band, so `targeting.band` is seeded with a single entry. The TS resolver's
`Targeting.band` is conceptually a set of allowed bands; here it's narrowed to
one until the export grows a real allowed-bands field. Documented at
src/catalog_canonical.rs:174-183.

**Drift — inferred effect numerics.** Effect amounts/durations/distances are
*inferred from archetype + heat*, not present in the canonical data. These are
intentionally conservative defaults (src/catalog_canonical.rs:429-433) meant to
be tuned by playtesting, not authoritative balance numbers.

**Drift — self-relative signature repair (#84 follow-up).** The canonical export
literally lists `slip`/`swap_toss`/`ram`/`throw` as `SELF`-pattern
`DISPLACE_TARGET` actions, which are mechanically dead in the resolver (a
`DISPLACE_TARGET` against the source ship). The loader knowingly **deviates from
the literal export**, rewriting them to `DISPLACE_SELF`
(`rewrite_self_relative_signature`) so they match the canonical *prose* and run
faithfully. `swap_toss` is further narrowed to a single bow-side swap because the
engine has no two-sided swap effect — a faithful subset, flagged to the lead.
