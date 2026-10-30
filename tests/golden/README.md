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

Verification fixtures (derived from the frozen bytes above; each is produced by
its own example and asserted by its own gate):

- `pkg/<corpus>.ac_` + `pkg/manifest.json` — the two verbatim containers and
  their CFB/zip census (`examples/freeze_container.rs` →
  `tests/container_gate.rs`).
- `pkg/lzma.json` — C6 pkg-payload law: per method-14 member `name/usize/crc/
  state/sha256` plus synthetic raw-LZMA1 vectors (`examples/freeze_lzma.rs` →
  `tests/lzma_c6_gate.rs`). `state: "clean"` means the member decodes to its
  declared size with crc32 parity; `"partial"` is the F63 residual.
- `cube/expectations.json` — P5.C4 cube audit + L5 balance table per corpus
  (`examples/freeze_cube.rs` → `tests/cube_golden.rs`).
- `interlink/expectations.json` — P5.C3 interlink census per corpus
  (`examples/freeze_interlink.rs` → `tests/web_parity.rs`).
- `strings/` — F6 `Form/Strings` text-pool law: the three corpus `.cvw` copies
  whose pool survives the F63 frontier, plus `expectations.json` (header words,
  CRC verdict, decoded length, entry count, sampled texts, whole-pool digest)
  → `crates/cw-cv/tests/f6_strings_gate.rs`. The remaining corpus documents have
  no pool (blank templates; text lives in `Layout`) or place it past the
  readable prefix — both recorded in the fixture, not fabricated.
- `area/domain.json` — the P5.E–P5.H domain census per corpus for the seven
  tables those gates read (`AM`, `BL`, `GL`, `MP`, `PR`, `SH`, `TP`)
  (`examples/freeze_area.rs` → `accounts_gate`, `adjustments_gate`,
  `groupings_gate`, `mp_tree_gate`, `grouping_assign_check`, `signoff_gate`,
  `program_gate`, `tax_gate`, `h2c_consolidation_gate`). Each table records
  `records`/`live`/`deleted` plus its population census: `MP` scheme sets,
  assignment and lead-schedule-link counts, `TP` code/UDEF censuses and
  reference audit, `SH` signoff ladder, `BL` per-slice consolidation, `AM` chart
  kinds, `GL` journal census. Per damaged table it also records `frontier`, the
  assertion window (see below).

## Provenance note (2026-10-28 rebuild)

The original `cube` and `interlink` fixtures came from an external Python
oracle cross-checked leaf-for-leaf against the kernel ("two parsers, one
truth"). That oracle is not part of this rebuild, so both are now
**kernel-derived** over the frozen dbf corpora: the gates pin the kernel against
its own frozen bytes, i.e. regression + coverage laws. `lzma.json` is
kernel-derived for the container census and liblzma-derived for the synthetic
vectors (so `decode_raw` parity is still an independent-implementation check).

`area/domain.json` is kernel-derived in the same sense, and it is deliberately
the *weakest* of the three provenance tiers the domain gates use. Every gate
states its facts in this order, and never lets the fixture override a stronger
tier:

1. **spec/format constants** — physical schemas, record lengths, column widths
   from the shipped type-library headers (`cw_core::dbf`,
   `inventory/dbf-schema.tsv`). Asserted as inline literals in each gate.
2. **reference-decoder evidence** — the `DAMAGED` frontier table in
   `cw_core/tests/guid_law_gate.rs`, derived from liblzma/xz 5.8.4 aborting
   mid-stream. Restated as `support::FRONTIER` and asserted, not searched.
3. **engine self-freeze** — the censuses in `area/domain.json`.

## F63 caveat (affects all fixtures derived from the dbf corpora)

Several `.dbf` members inside the shipped `.ac_` packages are **truncated LZMA
streams**: the reference decoder (liblzma, raw LZMA1 lc3/lp0/pb2 from offset 19)
aborts with "Corrupt input data" mid-stream — hull2024 MP after 1,055,945/
3,781,062 output bytes, BL after 489,701/883,452, TP after 385,778/6,820,540;
corp2026 and mwgh2025 MP/TP (and hull SH/HI) likewise. `cw_core::lzma`'s
`RangeDec` zero-fills past end-of-buffer, so those members decode to their
declared size with fabricated tail bytes; the frozen `.dbf` fixtures inherit
those zeros. Consequences baked into the fixtures and documented at each
assertion:

- cube balances include fabricated-number rows (hull2024 NUMBER cells may carry
  `0x7C`), so balance keys are parsed from the right.
- interlink `typed` censuses record the MP/TP anchors that cannot resolve
  (hull2024 AN: AM 175/175, MP 0/94, TP 6/48; mwgh2025 AN: AM 183/183, MP 0/26).
- the domain `area` census is scoped to a per-table `frontier` = `min`(reference
  readable rows, model-decodable rows); the decodable term matters for `MP`, a
  row can hold a valid `CWGUID` while its numeric cells are already half-shifted.
  The resulting authentic windows are corp2026 MP 786/1676, TP 2299/22150,
  HI 1510/1710; hull2024 MP 785/2837, TP 2184/39195, BL 1918/3462, SH 603/664,
  HI 1346/1752; mwgh2025 MP 785/2342, TP 2305/25417, BL 1894/1958, HI 1405/1745.
  Consequences the domain gates state at each assertion: corp2026's TP window is
  **100% tombstones** (no tax-code law is assertable for that corpus); `MP.GROUP1`
  is populated on zero authentic rows (the lead-schedule link is tail-only);
  `hull2024`'s 553 `AM.GROUPn` values all target tail-only schemes, and its
  authentic `BL` window has no `YEAR='0'`/`GROUP='A'` slice.

## D-area fixtures (2026-10-28 rebuild)

Six areas pinned by `crates/cw-app/examples/freeze_*.rs` and consumed by the
`cw-app` D-lane gates plus `cw-cli/edit_paths_gate`:

- `lifecycle/` — `law.json` + `tables/{ac,am,bl,gl}.dbf` + `tables/ac.fpt`
  (`freeze_lifecycle.rs` → `lifecycle_gate`). The copy-probe source is **kernel
  derived** from `dbf/corp2026`: AC `77/1` and AM `325/0` still match the F64
  probe census exactly, BL/GL are re-grounded on the shipped bytes (`962/1198`,
  `17/3`).
- `lockdown/` — `before.ac` + `law.json` (`freeze_lockdown.rs` → `lockdown_gate`).
  `before.ac` is the corpus hub rebuilt from `pkg/corp2026.ac_`'s own container
  streams (the genuine 5,204-byte `\x05DocumentSummaryInformation`); CW
  synthesizes the working hub from exactly those (`attachments/backup.md` §7.2).
  The constants are spec-recorded from `attachments/lockdown.md` §2 (F68). The CW
  destination tables are lost; the gate derives them from the corpus plus those
  constants instead of comparing to CW output.
- `history/` — `law.json` (`freeze_history.rs` → `history_gate`). Kernel-derived
  over `dbf/corp2026`; `sandbox/comwork2` no longer exists. Value-level laws are
  stated over the **readable prefix** (frontier 1,510 of 1,710 `HI` rows, F63).
- `backup/` — `expand-corp2026.json` + `law.json` (`freeze_backup.rs` →
  `backup_gate`). Two authorities: `reference_sha256` is liblzma's digest
  (independent implementation, 105 clean members of 132) and `sha256` is our own
  restore output pinned for all 132, damaged minority included.
- `yearend/` — `law.json` + `on/*.dbf` + `off/*.dbf` (`freeze_yearend.rs` →
  `yearend_gate`). The **numbers** are spec-recorded from
  `attachments/yearend.md` v1.1 (F46/F65) and asserted as literals; the
  destination tables are self-frozen regression pins (CW's yec1/yec2 tables are
  not part of the rebuild).
- `documents/` — `law.json` (`freeze_documents.rs` → `documents_gate`). Per
  corpus: the `SH` readable frontier (`hull2024` 603 of 664, F63; the other two
  end to end) and the census of `rows[..frontier]`, plus `WR`/`WT` sizes and the
  form/heading title lists. `hull2024/SH` therefore reads `103 live / 500
  tombstones` here rather than the pre-rebuild full-table `118 / 546`.

Shared rule for all six: a damaged table's assertion domain is the contiguous
**readable prefix**, with the frontier frozen as a constant in the fixture (never
searched), exactly like `guid_law_gate.rs`'s `DAMAGED` table.
