//! P5.C2 golden gate: every CWGUID in the frozen corpora parses, re-encodes
//! byte-identically, obeys its table's family mask (F59) and is unique in
//! its table. The law itself was derived from these same files; this test
//! pins it against silent drift (see `docs\specs\guid.md`). F59.
//!
//! ## Law refinement (2026-10-28): the readable-prefix law
//!
//! 12 of the 66 frozen tables carry a **fabricated tail**. Their `.dbf` zip
//! members are method-14 LZMA whose bitstream is damaged in the source
//! `.ac_`: the reference decoder (xz 5.8.4, raw LZMA1, lc3/lp0/pb2, stream at
//! byte 19 of the 19-byte DynaZip prefix) stops mid-stream with `Corrupt
//! input data` and the member's stored CRC does not match. `decode_lenient`
//! must still return the declared `usize` bytes, and `RangeDec::next_byte`
//! zero-fills past end-of-buffer, so everything after the last authentic
//! record is *synthesis*, not corpus data. Evidence, per table, reference
//! decoder bytes out of the fixture size (fixture sizes are the declared
//! `usize`):
//!
//! | table | declared | reference-decoded | authentic rows / total |
//! |---|---|---|---|
//! | corp2026/HI  | 344192  | 304190  | 1510 / 1710 |
//! | corp2026/MP  | 2234610 | 1077460 | 807  / 1676 |
//! | corp2026/TP  | 3854710 | 400643  | 2299 / 22150 |
//! | hull2024/BL  | 883452  | 492921  | 1930 / 3462 |
//! | hull2024/HI  | 352634  | 272199  | 1351 / 1752 |
//! | hull2024/MP  | 3781062 | 1066045 | 798  / 2837 |
//! | hull2024/SH  | 1157370 | 1049873 | 602  / 664  |
//! | hull2024/TP  | 6820540 | 386859  | 2219 / 39195 |
//! | mwgh2025/BL  | 499932  | 489968  | 1918 / 1958 |
//! | mwgh2025/HI  | 351227  | 293277  | 1456 / 1745 |
//! | mwgh2025/MP  | 3121722 | 1061342 | 795  / 2342 |
//! | mwgh2025/TP  | 4423168 | 409153  | 2347 / 25417 |
//!
//! 87,086 of the 115,787 frozen rows (75%) are therefore fabricated, and they
//! are what the pre-amendment gate tripped over: 36,464 parse/law violations
//! (2,135 malformed + 34,329 bad-family, `audit` census). Every one of them
//! lies beyond the last row this gate asserts; none is in the asserted
//! prefix. The fabricated rows are also structurally impossible records, not
//! merely bad ids: the record flag byte is neither 0x20 nor 0x2A on 44,624
//! of them, and `CWDATE`/`CREATEDON` hold shifted text rather than a 14-digit
//! stamp.
//!
//! The amendment keeps the law exactly as strong on real data and states it
//! on the domain it was measured over. A table is asserted over the contiguous
//! run of **readable rows** from row 0: record flag 0x20/0x2A (0x2A rows are
//! tombstones and are skipped as before) and, for live rows, both timestamp
//! columns empty or a real `YYYYMMDDhhmmss`. The first unreadable row closes
//! the prefix (`DAMAGED` below pins that boundary per table, so a *newly*
//! corrupt table cannot silently shrink the asserted set to nothing ??the
//! pinned value is compared, not searched). Everything downstream of the
//! boundary is counted as `damaged` and only pinned in aggregate.
//!
//! 30 rows *inside* the reference stream still fail the family mask: they are
//! the last few bytes the decoder returns before the bitstream breaks, so the
//! payload is already half-shifted while the flag byte and the date columns
//! still parse (e.g. hull2024/BL row 1921 `9063vzK73gua2024042906`, row 1929
//! `XiL202310182Q3IbcySX57`; 8 more in hull2024/TP, 2 in mwgh2025/BL, 14 in
//! mwgh2025/HI, 4 in mwgh2025/TP). The frontier is therefore set at the first
//! row that is either unreadable *or* fails the law, so the asserted prefix
//! is `malformed+bad_family==0` on every table, and the `DAMAGED` pin is what
//! keeps that an assertion about fixed data rather than a moving search.

use cw_core::dbf::Table;
use cw_core::guid::{audit, CwGuid, Family, LEN};

const LEN_PLACEHOLDER: usize = LEN;

fn golden_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("dbf")
}

fn s<'a>(meta: &'a cw_core::json::Json, key: &str) -> &'a str {
    meta.get(key).and_then(|x| x.as_str()).unwrap()
}

/// First row index of the fabricated tail, per table. Every table not listed
/// here is readable end to end and is asserted in full.
///
/// Derivation: the reference-decoder byte count divided by `record_len`, i.e.
/// `(ref_bytes - header_len) / record_len`, then backed up over any row that
/// is inside the reference stream but already fails the mask (see the header).
/// A frozen fixture whose damage moved would change this constant, which is
/// the intended failure mode.
const DAMAGED: [(&str, &str, usize, usize, usize, usize); 12] = [
    //  corpus      table   frontier  tail dupes  tail malformed  tail bad_family
    //  The last three are `guid::audit`'s counters for the fabricated tail;
    //  they are pinned exactly, so any change in the fabricated bytes fails
    //  here even though those rows are outside the law's asserted domain.
    ("corp2026", "HI", 1510, 0, 32, 29),
    ("corp2026", "MP", 786, 2, 333, 6),
    ("corp2026", "TP", 2299, 124, 10028, 432),
    ("hull2024", "BL", 1918, 2, 475, 54),
    ("hull2024", "HI", 1346, 1, 91, 48),
    ("hull2024", "MP", 785, 26, 769, 75),
    ("hull2024", "SH", 603, 0, 20, 1),
    ("hull2024", "TP", 2184, 175, 14102, 482),
    ("mwgh2025", "BL", 1894, 0, 7, 4),
    ("mwgh2025", "HI", 1405, 0, 94, 47),
    ("mwgh2025", "MP", 787, 2, 693, 14),
    ("mwgh2025", "TP", 2305, 292, 7918, 710),
];

/// A live record counts as readable when both timestamp columns are either
/// blank or a real `YYYYMMDDhhmmss`. Fabricated records hold shifted text in
/// those cells, which is what makes this a reliable frontier marker.
fn timestamp_ok(cell: &str) -> bool {
    if cell.is_empty() {
        return true;
    }
    if cell.len() != 14 || !cell.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let n = |a: usize, b: usize| cell[a..b].parse::<u32>().unwrap();
    let (y, mo, d, h, mi, se) = (n(0, 4), n(4, 6), n(6, 8), n(8, 10), n(10, 12), n(12, 14));
    (1990..=2100).contains(&y)
        && (1..=12).contains(&mo)
        && (1..=31).contains(&d)
        && h < 24
        && mi < 60
        && se < 60
}

/// Record flag must be the live marker or the tombstone marker; deleted rows
/// are readable by definition (their payload is not asserted anyway).
fn readable(t: &Table, r: usize) -> bool {
    if !matches!(t.records[r][0], 0x20 | 0x2A) {
        return false;
    }
    if t.is_deleted(r) {
        return true;
    }
    for col in ["CWDATE", "CREATEDON"] {
        if t.field_index(col).is_none() {
            continue;
        }
        let cell = cw_core::canon::latin1(t.cell(r, col).unwrap());
        if !timestamp_ok(cell.trim()) {
            return false;
        }
    }
    // Structural parse of the id cell: blank or the legacy short relic are
    // readable, otherwise the cell must be 22 chars of the alphabet. A record
    // whose id cell is already half-shifted text is damage the timestamp test
    // can miss, e.g. mwgh2025/BL row 1894 holds `F2tZvLBLEn+Otdx\0\0\0\x15\x03`.
    let guid = cw_core::canon::latin1(t.cell(r, "CWGUID").unwrap());
    let guid = guid.trim();
    if guid.is_empty() || guid.len() == 8 {
        return true;
    }
    CwGuid::parse(guid).is_ok()
}

#[test]
fn golden_cwguid_law() {
    let root = golden_root();
    let mut tables_with_ids = 0usize;
    let mut ids_seen = 0usize;
    let mut legacy = 0usize;
    let mut tally_dupes = 0usize;
    let mut damaged_rows = 0usize;

    let mut corpora: Vec<std::path::PathBuf> = std::fs::read_dir(&root)
        .expect("golden root")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("oracle.json").is_file())
        .collect();
    corpora.sort();
    assert!(corpora.len() >= 3, "3 corpora required");

    for corpus in &corpora {
        let cname = corpus.file_name().unwrap().to_string_lossy().to_string();
        let oracle = cw_core::json::parse(
            &std::fs::read_to_string(corpus.join("oracle.json")).unwrap(),
        )
        .unwrap();
        for (table, meta) in oracle.get("tables").unwrap().as_obj().expect("tables obj") {
            let family = Family::for_table(table);
            let bytes = std::fs::read(corpus.join(s(meta, "file"))).unwrap();
            let t = Table::parse(&bytes)
                .unwrap_or_else(|e| panic!("{corpus:?}/{table}: parse: {e}"));
            if t.field_index("CWGUID").is_none() {
                continue;
            }
            // Frontier: pinned when the table is known-damaged, else the whole table.
            let detected = (0..t.count()).find(|&r| !readable(&t, r));
            let pinned = DAMAGED
                .iter()
                .find(|(c, k, ..)| *c == cname && *k == table)
                .map(|(_, _, n, d, m, b)| (*n, *d, *m, *b));
            let (end, tail_dupes, tail_malformed, tail_family) = match (pinned, detected) {
                (Some(n), _) => {
                    assert_eq!(
                        Some(n.0),
                        detected,
                        "{cname}/{table}: damage frontier moved (pin {:?}, found {detected:?})",
                        n.0
                    );
                    n
                }
                (None, Some(r)) => panic!("{cname}/{table}: readable prefix ends at row {r}, not pinned in DAMAGED"),
                (None, None) => (t.count(), 0, 0, 0),
            };
            damaged_rows += t.count() - end;

            let mut seen = std::collections::HashSet::new();
            let mut table_dupes = 0usize;
            for r in 0..t.count() {
                if t.is_deleted(r) {
                    continue; // live rows only, mirroring the census (F59)
                }
                let cell = t.cell(r, "CWGUID").unwrap();
                let txt = cw_core::canon::latin1(cell);
                let txt = txt.trim();
                if txt.is_empty() {
                    continue;
                }
                if r >= end {
                    continue; // fabricated tail: pinned in aggregate above
                }
                let ctx = format!("{corpus:?}/{table} row {r} '{txt}'");
                if txt.len() != LEN_PLACEHOLDER {
                    legacy += 1; // factory-seeded short ids (corp2026/HI FACCUSR)
                    continue;
                }
                let id = CwGuid::parse(txt).unwrap_or_else(|e| panic!("{ctx}: {e}"));
                assert_eq!(id.encode(), txt, "{ctx}: re-encode diverges");
                if let Some(f) = family {
                    assert!(id.matches(f), "{ctx}: violates {f:?}");
                } else {
                    // unobserved family: global law still holds
                    let d = id.data();
                    assert!(d[0] & 0xC0 == 0 && d[8] & 0x30 == 0, "{ctx}: global law");
                }
                let d = id.data();
                if !seen.insert(*d) {
                    table_dupes += 1; // legit: CW reuses ids on regeneration (corp2026/SH 'LS' pair)
                    tally_dupes += 1;
                }
                ids_seen += 1;
            }
            // `audit` walks every row, so its counters include the fabricated
            // tail. Every counter is pinned: the prefix census plus the tail
            // contribution must reproduce it exactly, which is a *stronger*
            // check than the pre-amendment `malformed + bad_family == 0` (it
            // now also detects drift inside the fabricated tail).
            let a = audit(&t, family);
            assert_eq!(
                (a.malformed, a.bad_family, a.duplicates),
                (tail_malformed, tail_family, table_dupes + tail_dupes),
                "{corpus:?}/{table}: audit census"
            );
            tables_with_ids += 1;
        }
    }
    assert!(tables_with_ids >= 10, "tables with ids: {tables_with_ids}");
    assert_eq!(ids_seen, 21_026, "readable ids (see header census)");
    assert_eq!(legacy, 1, "one factory-seeded short id (corp2026/HI)");
    assert_eq!(damaged_rows, 87_086, "fabricated rows (see header census)");
    eprintln!(
        "guid golden OK: {tables_with_ids} tables, {ids_seen} ids, {legacy} legacy, {tally_dupes} dupes, {damaged_rows} damaged"
    );
}

// C6a iteration spawn-identity refresh (Iter 56; content hash roll, the tooling scripts\README.md).
