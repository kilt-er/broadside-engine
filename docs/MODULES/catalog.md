# `src/catalog.rs` — catalog loader + format auto-detect

*A line-by-line walkthrough scoped to one file. Companion to the
[`src/catalog.rs`](../LINE_BY_LINE.md#srccatalogrs) section of `LINE_BY_LINE.md`.*

---

## Why this module exists

This is the **front door** to the content catalog. Callers (the demo binary's
startup loader, tester's `tests/catalog_smoke.rs`) hand it a path or a byte
slice and get back a typed [`Catalog`](types.md). It owns two responsibilities:

1. **Error typing** — a `LoadError` enum that distinguishes I/O failures
   (file missing, unreadable) from parse failures (malformed JSON), each
   wrapping the underlying error as its `source()`.

2. **Format auto-detect** — the catalog has two on-disk shapes (see
   [`catalog_canonical.md`](catalog_canonical.md)). This module tries the strict
   shape first as a fast path, and on parse failure falls back to the canonical
   transformer. Every caller gets the same `Catalog` out regardless of which
   shape was on disk, so **no caller has to pick** between a strict loader and a
   canonical loader.

There is no TS analog: TypeScript loads its catalog inline in `demo.ts`. This is
Rust-specific loading glue.

---

## `enum LoadError` (src/catalog.rs:29)

**Intent:** Two failure modes, kept distinct so a caller can tell "the file
isn't there" from "the file is there but malformed."

Line 30: `#[non_exhaustive]` — downstream `match`es on `LoadError` get a
non-exhaustive warning, leaving room to add (e.g.) a `BadSchema(String)`
validation variant later without breaking callers.

Line 31-34: `Io(io::Error)` and `Parse(serde_json::Error)` — the two variants.

Line 36-43: `Display` — human-readable one-liners (`"io error reading catalog: …"`
/ `"parse error in catalog json: …"`).

Line 45-52: `Error::source` — returns the wrapped error so the standard
error-chain machinery (e.g. `anyhow`, `?`-propagation reporters) can walk to the
root cause.

Line 54-59: `From<io::Error>` and `From<serde_json::Error>` — the conversions
that make `?` work transparently inside `load_from_path` / `load_from_bytes`.

**Cross-references:** Returned by `load_from_path` and `load_from_bytes`. Wraps
errors that may originate inside
[`from_canonical_value`](catalog_canonical.md#fn-from_canonical_valueroot-value---resultcatalog-serde_jsonerror-srccatalog_canonicalrs70).

---

## `fn load_from_path(path: impl AsRef<Path>) -> Result<Catalog, LoadError>` (src/catalog.rs:72)

**Intent:** Read the file at `path` and decode it. Thin wrapper: read the bytes
(any I/O error becomes `LoadError::Io` via the `From` impl and `?`), then defer
to `load_from_bytes` for the format dispatch.

Line 73: `let bytes = fs::read(path)?;` — slurp the whole file; `?` lifts an
`io::Error` into `LoadError::Io`.

Line 74: `load_from_bytes(&bytes)` — single dispatch point so the path-based and
byte-based loaders share identical format-detect logic.

**Cross-references:** Called by the demo bin's startup and by integration tests.
Delegates to `load_from_bytes`.

---

## `fn load_from_bytes(bytes: &[u8]) -> Result<Catalog, LoadError>` (src/catalog.rs:79)

**Intent:** Decode an in-memory JSON byte slice with the strict-first /
canonical-fallback dispatch. Useful directly for embedded test fixtures.

Line 81-83: **strict fast path.** `serde_json::from_slice::<Catalog>(bytes)` — if
the bytes are already the engine's native nested shape, return immediately. The
`if let Ok(c)` swallows the strict error on purpose; a strict-parse failure just
means "try the other shape," not "fail."

Line 85: `let v: serde_json::Value = serde_json::from_slice(bytes)?;` — the
fallback parses the bytes into a loose `Value` tree. A failure *here* is a real
malformed-JSON error (not just shape drift), so it propagates as
`LoadError::Parse`.

Line 86: `Ok(crate::catalog_canonical::from_canonical_value(v)?)` — run the
canonical transformer. Its `serde_json::Error` also lifts to `LoadError::Parse`.

**Drift — auto-detect by trial, not by sniffing.** The module doesn't inspect a
schema-version field to decide which loader to use; it just *tries strict and
falls back*. This is intentional (src/catalog_canonical.rs:47-54): the canonical
export is the only loose shape expected today, and trial-decode keeps every
caller on one function. Future formats can extend the dispatch chain.

**Cross-references:** Called by `load_from_path` and tests. Calls
[`from_canonical_value`](catalog_canonical.md) on the fallback path. Produces a
[`Catalog`](types.md).

---

## Catalog-driven enemy synthesis (task #115)

*Added after the loader: this half of `catalog.rs` turns a [`ShipSpawn`](types.md)
into a real [`Ship`](types.md) using the catalog's `enemies[]` definitions, so a
`skiff` is hull 3 with a pulse laser, a `monitor` is hull 5 with Pursuit, etc. —
each enemy behaves per its canonical identity instead of the placeholder shell.*

Before #115, the demo synthesized every non-boss enemy from
[`runs::fallback_ship_for_spawn`](runs.md) (hull 3, one forward pulse_laser, no
traits) — so the AI's Pursuit/BurnHard/Agile nudges never fired because no enemy
carried traits. This synthesis path fixes that: it reads the canonical `EnemyDef`
and materializes the real hull, mounts, and traits.

### `fn enemy_ship_from_catalog(catalog, spawn) -> Option<Ship>` (src/catalog.rs:143)

**Intent:** The tier-1 entry point — find the `EnemyDef` whose id matches
`spawn.class_id`, materialize a `Ship` from it. `None` if no such enemy exists (the
caller falls back). Equivalent to `enemy_ship_from_catalog_at_tier(catalog, spawn, 1)`.

### `fn enemy_ship_from_catalog_at_tier(catalog, spawn, patrol_tier) -> Option<Ship>` (src/catalog.rs:161)

**Intent:** The patrol-tier-aware form. `patrol_tier` is the encounter's
[`Sector::patrol_tier`](types.md).

**Drift / dormant seam:** the canonical data carries a `hull5` field (effective hull
at Patrol 5+), but **no consumer wires it yet** — the demo escalates difficulty via
enemy count + traits, not stat scaling. So `patrol_tier` is *threaded but unused* for
stat math: [`select_hull`] returns base `hull` at every tier today. The parameter
exists so wiring tier-scaling later (`patrol_tier ≥ 5 → hull5`) is a one-line change
inside `select_hull` rather than a signature-breaking retrofit. Reviewer's audit
flagged this as dormant; it's a deliberate seam.

### `fn ship_from_enemy_def[_at_tier](catalog, def, spawn[, patrol_tier]) -> Ship` (src/catalog.rs:174, 182)

**Intent:** Materialize the `Ship` from a specific `EnemyDef` + spawn (split out so
tests can drive a hand-built `EnemyDef` without a whole catalog). Field mapping:
- **mounts** ← `EnemyDef.weapons` (src/catalog.rs:190-215). The canonical export lists
  weapons by **display name** ("Pulse Laser") — the same #82 drift the class set
  lists have — so each is resolved to an action id via `resolve_weapon_id`
  (src/catalog.rs:291; snake_case ids pass through, display names look up). The
  mount's `arc` comes from the resolved action's `targeting.requires_arc`
  (forward beam → Forward, broadside battery → BroadsideArc), defaulting to `Forward`
  for arc-less movement/defensive actions so they still surface in the AI fallback
  ladder. Unresolved weapons are skipped (logged), not mounted as a dangling id.
- **traits** ← `EnemyDef.traits` via `trait_from_str` (src/catalog.rs:217-221).
- **hull** ← `spawn.hp_override` if set, else `select_hull(def, patrol_tier)`
  (src/catalog.rs:223).
- **shield_profile** ← `enemy_shield_default` (src/catalog.rs:266): light all-round
  armour with a **soft stern** (the flank-from-behind invariant), distinct from the
  player default so enemy/player armour can be tuned independently. The boss keeps its
  own richer profile.
- `orientation`/`cell` from the spawn; heat_max 6, empty queue/cooldowns/statuses.

### Helpers (src/catalog.rs:252–328)

- `select_hull(def, patrol_tier)` (src/catalog.rs:252) — the dormant tier seam:
  returns `def.hull` at every tier today; the single place to switch to `def.hull5`
  at `patrol_tier ≥ 5` when tier-scaling is wired.
- `enemy_shield_default` (src/catalog.rs:266), `action_name_to_id` (src/catalog.rs:279,
  the display-name→id map, mirroring `catalog_canonical::transform_class`'s lookup at
  synthesis time so it works for both load paths), `resolve_weapon_id`
  (src/catalog.rs:291), `trait_from_str` (src/catalog.rs:307, maps canonical
  Title-Case-with-hyphens like "Burn-Hard"/"Reactor Breach" to camelCase `Trait`
  variants via a spaceless-lowercase key; unknown → `None`, skipped).

**Cross-references:** Called by the bin's `build_current_board`
([`broadside.rs`](broadside.md)) — the spawn closure now routes `warlord` →
[`boss_ship_for_spawn`](runs.md), else `enemy_ship_from_catalog_at_tier` → real
synthesis, else [`fallback_ship_for_spawn`](runs.md). The traits it attaches drive
[`resolve.rs`](resolve.md)'s `decide_enemy_action` AI nudges.

**Worked examples:** `synthesized_enemy_carries_catalog_traits_and_mounts`
(src/catalog.rs:436), `synthesized_enemy_honors_hp_override` (src/catalog.rs:486),
`unknown_class_id_returns_none` (src/catalog.rs:510),
`patrol_tier_seam_threads_through_without_changing_hull_yet` (src/catalog.rs:517, pins
the seam is dormant), `trait_from_str_maps_canonical_display_strings` (src/catalog.rs:410),
`resolve_weapon_id_handles_display_names_and_ids` (src/catalog.rs:424),
`real_catalog_synthesizes_canonical_enemies_with_traits` (src/catalog.rs:548, against
the real `assets/broadside.catalog.json`).

---

## Tests (src/catalog.rs:89)

Two embedded unit tests pin the loader's contract:

- **`loads_minimal_catalog`** (src/catalog.rs:132) — a hand-written
  `MINIMAL_CATALOG_JSON` in the **strict** shape (nested `cost`/`targeting`,
  `{ kind, amount }` effects) round-trips: schema, action count, and the first
  action id all match. Exercises the trickier serde shapes (tagged `Effect`,
  `Orientation`, `RangeBand` casing) on the fast path.
- **`placeholder_sections_default_to_empty_when_absent`** (src/catalog.rs:140) —
  the minimal fixture omits `capitals`/`classes`/`fieldkit`/`sectors`/
  `commendations` entirely; this asserts each defaults to an empty `Vec`,
  pinning the `#[serde(default)]` attributes on the [`Catalog`](types.md) struct
  (reviewer m3/m4 follow-up). If a default regresses, this fails with a
  `missing field` parse error.

Wider coverage lives in tester's integration suites: `tests/catalog_smoke.rs`
(the real `assets/broadside.catalog.json`, which exercises the **canonical**
fallback path) and `tests/catalog_placeholders.rs`.
