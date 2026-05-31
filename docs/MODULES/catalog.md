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
