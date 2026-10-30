# golden fixtures — port contract

Frozen expectations regenerated 2026-10-28 from the engagement packages
(corp2026 / hull2024 / mwgh2025) by `examples/freeze_roundtrip`.

- `<corpus>/<KEY>.dbf` — canonicalized table bytes (writer form; CW omits the
trailing 0x1A EOF on some tables — the length law stays exact, the byte is
normalized on ingest).
- `<corpus>/<KEY>.fpt` / `.cdx` — verbatim sidecars.
- `roundtrip.json` — C6a law: per-table sha/census/header-sha/multiset-sha.
- `<corpus>/oracle.json` — C1 law: shape, schema, canonical first record,
all-records canonical sha (via `cw_core::canon`).
