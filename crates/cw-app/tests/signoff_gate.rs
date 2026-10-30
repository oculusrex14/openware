//! P5.G G2 gate: 8-level signoff (`SH.A1..8`/`AD`/`AC`/`AU`) + the WPG
//! response documents.
//!
//! Corpus facts (frozen `tests\golden\dbf\corp2026`, `USI Compilation
//! Training(1155962)SH.dbf`, 200 physical rows / 93 live):
//! - 7 live `SH.TYPE='F'` response documents (`PRG='ac'`), all `LOCKDOWN='Y'`;
//! - **level 1 only** is populated, on 52 live rows, of which 4 are response
//!   docs (`0300 (M1)`, `0315.10 (M1)`, `0325 (M1)`, `0400 (M1)`): initials
//!   `N`, dates 20260826 / 20260907, `AU='E120235'`, `AC` empty; the other
//!   three forms (`0120`, `0126`, `0272`) are unsigned;
//! - levels 2..8 and `S1..S8` are empty in every corpus, so multi-level writes
//!   are exercised on synthetic puts over the real header.
//!
//! The gate proves: schema identity + byte-stable no-op save, the corpus
//! ladder projection (including the WPG subset), indexed put/clear/reload,
//! `SIGN`/`LOCKDOWN` flags, and the audited `DOCSIGN{n}` append through
//! `HistoryLog` (F47/F67/F68).

mod support;

use std::path::{Path, PathBuf};

use cw_app::history::{HistoryKind, HistoryLog};
use cw_app::signoff::{
    role, role_name, SignoffBook, SignoffRole, SignoffStage, PREPARED_LAST, REVIEWED_FIRST,
    SIGNOFF_LEVELS,
};
use cw_core::dbf::Table;
use cw_core::guid::Rng;
use support::{frontier, levels_at, table_fixture, usize_at};

const STEM: &str = "USI Compilation Training(1155962)";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn corpus() -> PathBuf {
    repo().join("tests/golden/dbf/corp2026")
}

fn sh_path() -> PathBuf {
    corpus().join(format!("{STEM}SH.dbf"))
}

fn load() -> (Vec<u8>, SignoffBook) {
    let bytes = std::fs::read(sh_path()).unwrap();
    let book = SignoffBook::from_bytes(&bytes).unwrap();
    (bytes, book)
}

#[test]
fn sh_signoff_schema_and_noop_save() {
    let (bytes, book) = load();
    assert_eq!(book.table().fields.len(), 124);
    for n in 1..=SIGNOFF_LEVELS {
        let families: [(&str, u8, u8); 4] =
            [("A", b'C', 8), ("AD", b'D', 8), ("AC", b'C', 22), ("AU", b'C', 22)];
        for (prefix, kind, len) in families {
            let name = format!("{prefix}{n}");
            let idx = book.table().field_index(&name).expect("signoff column");
            let f = &book.table().fields[idx];
            assert_eq!((f.kind, f.len, f.dec), (kind, len, 0), "{name}");
        }
    }
    assert_eq!(book.count(), 93, "live SH rows in corp2026");
    assert_eq!(book.encode(), bytes, "SH no-op save changed bytes");
}

#[test]
fn schema_drift_is_rejected() {
    let bytes = std::fs::read(sh_path()).unwrap();
    let mut table = Table::parse(&bytes).unwrap();
    let idx = table.field_index("A1").unwrap();
    table.fields[idx].name = "AX1".into();
    let err = SignoffBook::from_table(table).unwrap_err().to_string();
    assert!(err.contains("missing column A1"), "{err}");

    let mut table = Table::parse(&bytes).unwrap();
    let idx = table.field_index("AD1").unwrap();
    table.fields[idx].len = 10;
    let err = SignoffBook::from_table(table).unwrap_err().to_string();
    assert!(err.contains("schema drift at AD1"), "{err}");
}

#[test]
fn corpus_ladder_is_level_one_only() {
    let (_, book) = load();
    let census = book.census();
    assert_eq!(census.documents, 93);
    assert_eq!(census.signed_documents, 52);
    assert_eq!(census.complete_documents, 0);
    assert_eq!(census.level_counts, [52, 0, 0, 0, 0, 0, 0, 0]);

    // Every populated row is the preparer ladder only.
    for sheet in book.sheets().iter().filter(|s| s.signed_count() > 0) {
        assert_eq!(sheet.signed_levels(), vec![1]);
        let l1 = sheet.level(1).unwrap();
        assert_eq!(l1.initials_text(), "N");
        assert_eq!(l1.user(), "E120235");
        assert_eq!(l1.role(), SignoffRole::Prepared(1));
        assert_eq!(l1.role_name(), "Prepared by 1");
        assert_eq!(sheet.stage(), SignoffStage::Prepared);
        assert_eq!(sheet.next_level(), Some(2));
        assert_eq!(sheet.highest_signed(), Some(1));
    }

    // The area fixture pins the same census (`SH` is undamaged in corp2026).
    let f = table_fixture("corp2026", "sh");
    assert_eq!(usize_at(&f, "records"), 200);
    assert_eq!(usize_at(&f, "frontier"), 200, "corp2026 SH needs no cap");
    assert_eq!(usize_at(&f, "live"), census.documents);
    assert_eq!(usize_at(&f, "deleted"), 200 - census.documents);
    let c = f.get("census").expect("census object");
    assert_eq!(usize_at(c, "documents"), census.documents);
    assert_eq!(usize_at(c, "signed"), census.signed_documents);
    assert_eq!(usize_at(c, "complete"), census.complete_documents);
    assert_eq!(levels_at(c, "levels"), census.level_counts.to_vec());
    let wf = f.get("wpg").expect("wpg object");
    let w = book.wpg_census();
    assert_eq!(usize_at(wf, "documents"), w.documents);
    assert_eq!(usize_at(wf, "signed"), w.signed_documents);
    assert_eq!(levels_at(wf, "levels"), w.level_counts.to_vec());
}

/// The other two corpora carry real multi-level ladders. Pin the full per-level
/// census and the deepest-level histogram over each readable frontier, after
/// excluding the rows whose ladder cells are fabricated payload.
///
/// **Fabricated ladder cells inside a readable prefix.** `hull2024` rows 601 and
/// 602 pass the readable-prefix test (record flag, timestamps, guid) while their
/// signoff cells are shifted text: row 601 holds `A5='uDg4m09N'`, `AD5='VGssJex/'`,
/// `AC5='tEg2tWRdKrOTi'` and junk in `AC4`/`AU4`; row 602 holds `AD8="'Uni0B0"` and
/// `AC8='ABQZa' + 0x97 0x91 + '7WktZ'`. They are the entire apparent "L8" story —
/// the pre-amendment reading of a deepest-L8 document was not evidence. The gate
/// therefore re-derives the exclusion from the cell widths/ASCII range (not from a
/// hardcoded row list), asserts it names exactly those two rows, and pins the
/// histograms over the clean remainder.
#[test]
fn other_corpus_ladders_are_pinned_over_their_frontiers() {
    let cases: [(&str, usize, usize, [usize; 8], [usize; 8], [usize; 8]); 2] = [
        // corpus, frontier, live documents, per-level (all), per-level (clean),
        // deepest-level histogram (clean)
        (
            "hull2024",
            603,
            103,
            [3, 64, 70, 47, 4, 0, 0, 1],
            [3, 63, 69, 47, 3, 0, 0, 0],
            [0, 0, 22, 45, 3, 0, 0, 0],
        ),
        (
            "mwgh2025",
            258,
            94,
            [76, 58, 76, 52, 0, 0, 66, 0],
            [76, 58, 76, 52, 0, 0, 66, 0],
            [0, 0, 0, 10, 0, 0, 66, 0],
        ),
    ];
    for (tag, want_frontier, want_docs, want_levels, want_clean, want_deepest) in cases {
        let raw = support::open(tag, "SH.dbf");
        let n = frontier(tag, "SH", &raw);
        assert_eq!(
            n,
            want_frontier,
            "{tag}: SH readable frontier moved (guid_law_gate DAMAGED)"
        );
        let t = support::prefix(&raw, n);
        let book = SignoffBook::from_table(t.clone()).unwrap();

        // --- per-level census, all rows (the SignoffBook projection) -----------
        let c = book.census();
        assert_eq!(c.documents, want_docs, "{tag} live documents");
        assert_eq!(c.level_counts, want_levels, "{tag} per-level signed census");
        assert_eq!(c.complete_documents, 0, "{tag}: no fully-signed document");

        // --- the exclusion, re-derived from cell shape -------------------------
        let mut excluded: Vec<usize> = Vec::new();
        let mut clean_levels = [0usize; 8];
        let mut deepest = [0usize; 9];
        for r in 0..t.count() {
            if t.is_deleted(r) {
                continue;
            }
            let clean = ladder_cells_are_clean(&t, r);
            if !clean {
                excluded.push(r);
            }
            let mut d = 0usize;
            for l in 1..=8u8 {
                if !level_signed(&t, r, l) {
                    continue;
                }
                if clean {
                    clean_levels[(l - 1) as usize] += 1;
                }
                d = l as usize;
            }
            if d > 0 && clean {
                deepest[d] += 1;
            }
        }
        assert_eq!(
            clean_levels, want_clean,
            "{tag}: per-level census over rows with well-formed ladder cells"
        );
        assert_eq!(
            &deepest[1..],
            &want_deepest[..],
            "{tag}: deepest-level histogram over clean rows"
        );
        if tag == "hull2024" {
            assert_eq!(
                excluded,
                vec![601, 602],
                "hull2024: the fabricated-ladder rows inside the readable prefix"
            );
            // The apparent L8 row is excluded, so the deepest authentic level is 5.
            assert_eq!(deepest[8], 0, "no authentic level-8 document");
            assert_eq!(
                deepest.iter().rposition(|n| *n > 0),
                Some(5),
                "deepest authentic level is 5"
            );
        } else {
            assert!(excluded.is_empty(), "{tag}: no fabricated ladder cells");
            assert_eq!(
                deepest.iter().rposition(|n| *n > 0),
                Some(7),
                "mwgh2025 deepest authentic level is 7"
            );
        }

        // --- AC/AU companion slots ----------------------------------------------
        // `AU` (7-char user id) is populated on **every** signed level of every
        // signed document. `AC` (22-char GUID width) is all-or-nothing *per
        // document*: a document's signed levels are either all tagged or all
        // blank, never mixed. Both width invariants hold on every populated cell.
        let mut ac_pop = 0usize;
        let mut au_pop = 0usize;
        let mut ac_rows_full = 0usize;
        let mut ac_rows_blank = 0usize;
        let mut au_rows_full = 0usize;
        let mut signed_levels = 0usize;
        let mut clean_signed_docs = 0usize;
        for r in 0..t.count() {
            if t.is_deleted(r) || !ladder_cells_are_clean(&t, r) {
                continue;
            }
            let (mut ac_here, mut ac_blank, mut au_here, mut au_blank) = (0usize, 0usize, 0usize, 0usize);
            for l in 1..=8u8 {
                if !level_signed(&t, r, l) {
                    continue;
                }
                signed_levels += 1;
                let ac = t.cell_str(r, &format!("AC{l}")).unwrap_or_default();
                let au = t.cell_str(r, &format!("AU{l}")).unwrap_or_default();
                if ac.trim().is_empty() {
                    ac_blank += 1;
                } else {
                    ac_here += 1;
                    ac_pop += 1;
                    assert_eq!(
                        ac.trim().chars().count(),
                        22,
                        "{tag} row {r} AC{l}: a populated AC is GUID width"
                    );
                }
                if au.trim().is_empty() {
                    au_blank += 1;
                } else {
                    au_here += 1;
                    au_pop += 1;
                    assert!(
                        au.trim().chars().count() <= 7,
                        "{tag} row {r} AU{l}: a populated AU is a short user id"
                    );
                }
            }
            if ac_here + ac_blank > 0 {
                clean_signed_docs += 1;
                if ac_here > 0 && ac_blank == 0 {
                    ac_rows_full += 1;
                } else if ac_here == 0 {
                    ac_rows_blank += 1;
                }
            }
            if au_here > 0 && au_blank == 0 {
                au_rows_full += 1;
            }
        }
        assert_eq!(
            au_pop, signed_levels,
            "{tag}: AU is populated on every signed level ({au_pop}/{signed_levels})"
        );
        // The basis is the clean signed-document count: the excluded rows are
        // themselves signed, so `c.signed_documents` (all rows) is larger.
        assert_eq!(
            clean_signed_docs + excluded.len(),
            c.signed_documents,
            "{tag}: every signed document is either clean or in `excluded`"
        );
        assert_eq!(
            au_rows_full, clean_signed_docs,
            "{tag}: every clean signed document carries AU on all its levels"
        );
        assert!(
            ac_pop <= signed_levels,
            "{tag}: AC can only be populated on signed levels"
        );
        assert_eq!(
            ac_rows_full + ac_rows_blank,
            clean_signed_docs,
            "{tag}: AC is all-or-nothing per clean document"
        );

        // --- the fixture pins the same census ----------------------------------
        let f = table_fixture(tag, "sh");
        assert_eq!(usize_at(&f, "records"), raw.count(), "{tag} records");
        assert_eq!(usize_at(&f, "frontier"), n, "{tag} frontier");
        assert_eq!(usize_at(&f, "live"), c.documents, "{tag} live");
        let cf = f.get("census").expect("census");
        assert_eq!(usize_at(cf, "documents"), c.documents, "{tag} fixture docs");
        assert_eq!(usize_at(cf, "signed"), c.signed_documents, "{tag} fixture signed");
        assert_eq!(levels_at(cf, "levels"), c.level_counts.to_vec(), "{tag} fixture levels");
        let lf = f.get("ladder").expect("ladder");
        assert_eq!(levels_at(lf, "per_level"), want_levels.to_vec(), "{tag} ladder");
        assert_eq!(levels_at(lf, "clean_per_level"), clean_levels.to_vec(), "{tag}");
        assert_eq!(
            levels_at(lf, "clean_deepest"),
            want_deepest.to_vec(),
            "{tag} fixture deepest"
        );
        assert_eq!(usize_at(lf, "ac_populated"), ac_pop, "{tag} fixture AC cells");
        assert_eq!(usize_at(lf, "ac_rows_full"), ac_rows_full, "{tag} fixture AC rows");
        assert_eq!(usize_at(lf, "ac_rows_blank"), ac_rows_blank, "{tag} fixture AC blanks");
        assert_eq!(usize_at(lf, "au_populated"), au_pop, "{tag} fixture AU cells");
        assert_eq!(usize_at(lf, "au_rows_full"), au_rows_full, "{tag} fixture AU rows");
    }
}

/// `true` when every populated ladder cell of record `r` sits in its declared
/// width family (`A`<=8, `AD`=8 digits, `AC`=22, `AU`<=7, `S`<=20, `SD`=8 digits)
/// and is printable ASCII. Fabricated payload fails this on width *and* range.
fn ladder_cells_are_clean(t: &Table, r: usize) -> bool {
    for l in 1..=8u8 {
        for (col, w) in [("A", 8usize), ("AC", 22), ("AU", 7), ("S", 20)] {
            let v = t.cell_str(r, &format!("{col}{l}")).unwrap_or_default();
            let v = v.trim();
            if !v.is_empty() && (!v.is_ascii() || v.chars().count() > w) {
                return false;
            }
        }
        for col in ["AD", "SD"] {
            let v = t.cell_str(r, &format!("{col}{l}")).unwrap_or_default();
            let v = v.trim();
            if !v.is_empty() && !(v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit())) {
                return false;
            }
        }
    }
    true
}

/// `true` when level `l` carries the COM-established pair (`A{n}` initials or `AD{n}`
/// date).
fn level_signed(t: &Table, r: usize, l: u8) -> bool {
    let a = t.cell_str(r, &format!("A{l}")).unwrap_or_default();
    let ad = t.cell_str(r, &format!("AD{l}")).unwrap_or_default();
    !a.trim().is_empty() || !ad.trim().is_empty()
}

/// **`SH.S{n}` is an offset mirror of `AU{n+1}`, not a parallel ladder.**
///
/// `S{n}` names the user who signed level `n+1`: on hull2024's 58 authentic `S`
/// cells, 45 equal `AU{n+1}` (case-insensitively), **0** equal `AU{n}`, and 13
/// match no `AU` at all (free-text schedule/required-signoff annotations). The
/// pre-amendment docstring called the family "empty in all corpora"; it is empty
/// in corp2026 and mwgh2025 but populated in hull2024. `SD{n}` stays empty
/// everywhere.
#[test]
fn s_family_mirrors_next_level_au() {
    let raw = support::open("hull2024", "SH.dbf");
    let t = support::prefix(&raw, frontier("hull2024", "SH", &raw));
    let mut cells = 0usize;
    let mut mirror_next = 0usize;
    let mut same_level = 0usize;
    let mut no_match = 0usize;
    let mut sd_cells = 0usize;
    for r in 0..t.count() {
        if t.is_deleted(r) || !ladder_cells_are_clean(&t, r) {
            continue;
        }
        for l in 1..=8u8 {
            let sv = t.cell_str(r, &format!("S{l}")).unwrap_or_default();
            let sv = sv.trim();
            let sd = t.cell_str(r, &format!("SD{l}")).unwrap_or_default();
            if !sd.trim().is_empty() {
                sd_cells += 1;
            }
            if sv.is_empty() {
                continue;
            }
            cells += 1;
            let same = t.cell_str(r, &format!("AU{l}")).unwrap_or_default();
            let next = if l < 8 {
                t.cell_str(r, &format!("AU{}", l + 1)).unwrap_or_default()
            } else {
                String::new()
            };
            if !next.trim().is_empty() && sv.eq_ignore_ascii_case(next.trim()) {
                mirror_next += 1;
            } else if !same.trim().is_empty() && sv.eq_ignore_ascii_case(same.trim()) {
                same_level += 1;
            } else {
                no_match += 1;
            }
        }
    }
    assert_eq!(cells, 58, "hull2024 authentic S cells");
    assert_eq!(mirror_next, 45, "S mirrors the *next* level's AU");
    assert_eq!(same_level, 0, "S is never AU at the same level");
    assert_eq!(no_match, 13, "free-text annotations, not user ids");
    assert_eq!(sd_cells, 0, "SD is empty in every corpus");

    // Zero in the other two corpora, which the fixture records.
    for tag in ["corp2026", "mwgh2025"] {
        let f = table_fixture(tag, "sh");
        assert_eq!(usize_at(f.get("s_family").expect("s_family"), "cells"), 0, "{tag} S");
    }
    let f = table_fixture("hull2024", "sh");
    let sf = f.get("s_family").expect("s_family");
    assert_eq!(usize_at(sf, "cells"), cells);
    assert_eq!(usize_at(sf, "mirror_next_level"), mirror_next);
    assert_eq!(usize_at(sf, "same_level"), same_level);
    assert_eq!(usize_at(sf, "no_match"), no_match);
}

#[test]
fn wpg_response_documents_carry_the_signoff() {
    let (_, book) = load();
    let docs = book.wpg_documents();
    assert_eq!(docs.len(), 7, "live SH TYPE='F' response documents");
    let schedules: Vec<String> = docs
        .iter()
        .map(|d| d.schedule.split_whitespace().next().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        schedules,
        vec!["0120", "0126", "0272", "0300", "0315.10", "0325", "0400"]
    );
    for d in &docs {
        assert_eq!(d.prg, "ac");
        assert!(d.locked_down, "{} is locked down in corp2026", d.schedule);
    }
    let signed: Vec<&str> = docs
        .iter()
        .filter(|d| d.is_signed(1).unwrap())
        .map(|d| d.schedule.as_str())
        .collect();
    assert_eq!(
        signed,
        vec!["0300 (M1)", "0315.10 (M1)", "0325 (M1)", "0400 (M1)"]
    );
    let program = docs.iter().find(|d| d.schedule.starts_with("0400")).unwrap();
    assert_eq!(program.title, "Program for Review Engagements");
    assert_eq!(program.level(1).unwrap().date, "20260907");
    assert!(!program.sign, "SH.SIGN stays blank until doc.Sign (F47)");
    assert!(book.sheet(&program.guid).is_some());

    let wpg = book.wpg_census();
    assert_eq!(wpg.documents, 7);
    assert_eq!(wpg.signed_documents, 4);
    assert_eq!(wpg.level_counts, [4, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn put_clear_and_flags_roundtrip() {
    let (bytes, mut book) = load();
    let doc = book
        .wpg_documents()
        .into_iter()
        .find(|d| d.schedule.starts_with("0272"))
        .unwrap()
        .guid
        .clone();
    assert!(!book.sheet(&doc).unwrap().is_signed(1).unwrap());

    // Levels 1..4 with the companion slots on the audited-shape put.
    book.set_signoff(&doc, 1, "N", "20261023", None, Some("E109586"))
        .unwrap();
    book.set_signoff(&doc, 2, "QB", "20261023", None, None).unwrap();
    book.set_signoff(&doc, 3, "AB", "20261023", None, None).unwrap();
    book.set_signoff(&doc, 4, "TS", "20261024", Some("AbCdEfGhIjKlMnOpQrStUv"), None)
        .unwrap();
    book.set_sign(&doc, true).unwrap();
    book.set_lockdown(&doc, false).unwrap();

    let sheet = book.sheet(&doc).unwrap();
    assert_eq!(sheet.signed_levels(), vec![1, 2, 3, 4]);
    assert_eq!(sheet.stage(), SignoffStage::Reviewed);
    assert_eq!(sheet.next_level(), Some(5));
    assert_eq!(sheet.highest_signed(), Some(4));
    assert_eq!(sheet.open_review_levels(), vec![5, 6, 7, 8]);
    assert_eq!(sheet.level(1).unwrap().user(), "E109586");
    assert_eq!(sheet.level(4).unwrap().signature_guid(), "AbCdEfGhIjKlMnOpQrStUv");
    assert!(sheet.sign);
    assert!(!sheet.locked_down);

    let out = book.encode();
    assert_ne!(out, bytes, "written SH must differ from the frozen table");
    let reloaded = SignoffBook::from_bytes(&out).unwrap();
    let again = reloaded.sheet(&doc).unwrap();
    assert_eq!(again.signed_levels(), vec![1, 2, 3, 4]);
    assert_eq!(again.level(4).unwrap().date, "20261024");
    assert_eq!(again.level(3).unwrap().initials_text(), "AB");
    assert!(again.sign);
    assert_eq!(reloaded.count(), 93);

    // Year-end reset law (F65): A/AD/AC/AU cleared, SIGN/LOCKDOWN untouched.
    let mut book = reloaded;
    book.clear_signoffs(&doc).unwrap();
    let cleared = book.sheet(&doc).unwrap();
    assert!(cleared.signed_levels().is_empty());
    assert_eq!(cleared.stage(), SignoffStage::Unsigned);
    assert_eq!(cleared.next_level(), Some(1));
    assert!(cleared.sign, "clear_signoffs must not touch SH.SIGN");
    let cleared_bytes = book.encode();
    let again = SignoffBook::from_bytes(&cleared_bytes).unwrap();
    assert!(again.sheet(&doc).unwrap().signed_levels().is_empty());
}

#[test]
fn audited_signoff_appends_docsign_row() {
    // The history store is the corpus directory itself: the freeze carries the
    // verbatim `HI.dbf`/`.fpt`, and `HistoryLog::open` reads them case-insensitively
    // (the old `sandbox/comwork2` staging tree is not part of the workspace).
    let src = corpus();
    let mut log = HistoryLog::open(&src, STEM).unwrap();
    let base = log.table.count();
    let (_, mut book) = load();
    let doc = book
        .wpg_documents()
        .into_iter()
        .find(|d| d.schedule.starts_with("0400"))
        .unwrap()
        .guid
        .clone();
    let mut rng = Rng::new_seeded(0x62_2026);
    book.sign_off(
        &mut log,
        &doc,
        2,
        "QB",
        "20261023",
        "E109586",
        Some("9.99"),
        "ZzYyXxWwVvUuTtSsRrQqPp",
        "20261023180000",
        &mut rng,
    )
    .unwrap();

    assert_eq!(log.table.count(), base + 1);
    let ev = log.event(base).unwrap();
    assert_eq!(ev.category, "DOCS");
    assert_eq!(ev.event_type, "DOCSIGN2");
    assert_eq!(ev.kind(), HistoryKind::Signoff(2));
    assert_eq!(ev.object_type, "SH");
    assert_eq!(ev.object_id, doc);
    assert_eq!(ev.user, "E109586");
    assert_eq!(ev.user_initials().as_deref(), Some("QB"));
    assert_eq!(ev.value("version").as_deref(), Some("9.99"));
    assert_eq!(ev.checksum, "ZzYyXxWwVvUuTtSsRrQqPp");

    // The SH side of the same call is the level-2 put. Level 2 is
    // "Prepared by 2" (levels 1..=3 are preparer roles), so the document is
    // still at the Prepared stage - Reviewed starts at level 4.
    let sheet = book.sheet(&doc).unwrap();
    assert!(sheet.is_signed(2).unwrap());
    assert_eq!(sheet.stage(), SignoffStage::Prepared);
    // Level 1 was already signed in the corpus; level 2 is the new one.
    assert_eq!(sheet.signed_levels(), vec![1, 2]);

    // Source tree untouched (in-memory only).
    let after = HistoryLog::open(&src, STEM).unwrap();
    assert_eq!(after.table.count(), base);
}

#[test]
fn role_ladder_names() {
    for level in 1..=PREPARED_LAST {
        assert_eq!(role(level).unwrap(), SignoffRole::Prepared(level));
        assert_eq!(role_name(level).unwrap(), format!("Prepared by {level}"));
    }
    for level in REVIEWED_FIRST..=SIGNOFF_LEVELS {
        assert_eq!(role(level).unwrap(), SignoffRole::Reviewed(level - PREPARED_LAST));
        assert_eq!(
            role_name(level).unwrap(),
            format!("Reviewed by {}", level - PREPARED_LAST)
        );
    }
    assert!(role(0).is_err());
    assert!(role(9).is_err());
}

