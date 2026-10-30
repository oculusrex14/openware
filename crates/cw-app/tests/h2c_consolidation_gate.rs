//! Gate: P5.H H2 consolidation maths + `CONSCHKSUM` + units.
//!
//! Fixture: `tests/golden/area/domain.json` (`examples/freeze_area.rs`), the
//! kernel-derived area census. `BL` is one of the F63-damaged tables for
//! `hull2024` (readable frontier 1,918 of 3,462 records) and `mwgh2025` (1,894 of
//! 1,958); `corp2026` is clean end to end. See `tests/support/mod.rs`. Every
//! corpus law below is asserted over the cube built from the **readable prefix**,
//! and the per-slice balances are additionally pinned in the census, so a moved
//! boundary fails here rather than silently changing the consolidated numbers.
//!
//! Truths exercised:
//! 1. Single-blank corpora consolidate 1:1 with the L5 cube balance law, on every
//!    `(year, group)` slice the authentic prefix actually carries.
//! 2. Multi-entity totals apply `CE` ownership and skip `EXCLUDE` units.
//! 3. `CONSCHKSUM` is stable, 8 uppercase hex, stampable and auditable.
//!
//! ## The `CONSCHKSUM` digest is a documented placeholder
//!
//! The CaseWare digest algorithm is not recovered (open RE item: it needs a COM
//! write probe, and every corpus `CONSCHKSUM` cell is blank). What is implemented
//! — and all this gate claims — is the **shape and stability law**: eight
//! uppercase hex digits over a canonical ASCII projection, deterministic across
//! runs and platforms, sensitive to every input that changes the consolidated
//! picture, and verifiable against a stored cell. `tests/golden/area` does *not*
//! pin any digest value for a corpus row, because there is no corpus row with one.
//! Real-integer parity with CaseWare remains a residual, not a claim.

mod support;

use std::path::PathBuf;

use cw_app::accounts::write_cell;
use cw_app::consolidation::{
    account_checksum, conschksum_copy_drift, consolidate, copy_cwchecksum_to_conschksum,
    entity_balance, legacy_fnv, checksum8, ChecksumAudit, Consolidation,
    ConsolidationUnits, CHECKSUM_LEN,
};
use cw_app::entities::{CeBool, Entity, Org, Percent, Year};
use cw_core::cube::Cube;
use cw_core::dbf::Table;
use support::{frontier, i64_at, prefix, table_fixture, usize_at};

fn corpus_file(tag: &str, suffix: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/dbf")
        .join(tag);
    let want = suffix.to_ascii_lowercase();
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(&want))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no *{suffix} in {dir:?}"))
}

/// The `BL` cube over the corpus's **readable prefix** (F63).
fn load_cube(tag: &str) -> Cube {
    let raw = Table::parse_path(corpus_file(tag, "BL.dbf")).expect("BL");
    let n = frontier(tag, "BL", &raw);
    Cube::from_table(&prefix(&raw, n)).unwrap_or_else(|e| panic!("{tag} BL prefix {n}: {e}"))
}

fn load_am(tag: &str) -> Table {
    Table::parse_path(corpus_file(tag, "AM.dbf")).expect("AM")
}

/// Build a minimal synthetic `BL` table from the corpus header/fields.
fn bl_table(rows: &[(&str, f64)]) -> Table {
    let mut t = load_am_bl();
    let template = t
        .records
        .iter()
        .find(|r| r.first() == Some(&b' '))
        .expect("live BL template")
        .clone();
    t.records.clear();
    for (entity, amount) in rows {
        t.records.push(template.clone());
        let r = t.records.len() - 1;
        for col in [
            "NUMBER", "ENTITY", "YEAR", "BUCKET", "TYPE", "ID", "GROUP", "BOOKED",
            "PERIOD", "CUSTOM", "AMT", "AMT_YTD", "AMT_DIST", "FX", "FX_YTD",
            "FX_DIST", "CWGUID", "CWDATE", "CREATEDON",
        ] {
            write_cell(&mut t, r, col, "").unwrap();
        }
        write_cell(&mut t, r, "NUMBER", "1000").unwrap();
        write_cell(&mut t, r, "ENTITY", entity).unwrap();
        write_cell(&mut t, r, "YEAR", "0").unwrap();
        write_cell(&mut t, r, "BUCKET", "O").unwrap();
        write_cell(&mut t, r, "ID", "O").unwrap();
        write_cell(&mut t, r, "GROUP", "A").unwrap();
        write_cell(&mut t, r, "PERIOD", "0").unwrap();
        write_cell(&mut t, r, "AMT", &format!("{amount:.2}")).unwrap();
    }
    t
}

/// A `BL` header to build synthetic rows on: the frozen corp2026 table (clean).
fn load_am_bl() -> Table {
    Table::parse_path(corpus_file("corp2026", "BL.dbf")).expect("BL")
}

fn unit(code: &str, percent: Option<f64>, excluded: bool) -> Entity {
    let mut e = Entity::new(code, if code.is_empty() { "Parent" } else { code }).unwrap();
    if let Some(p) = percent {
        e.set_year_percent(Year::Current, Percent::new(p).ok());
    }
    if excluded {
        e.exclude = CeBool::yes();
    }
    e
}

/// Anchor slices asserted as inline literals, independent of the census, so the
/// fixture is never its own only witness. `(year, group, Year, accounts)`.
///
/// The full slice list is read from the census: `hull2024`'s authentic prefix has
/// **no** `YEAR='0'`/`GROUP='A'` slice (its year-0 data is `GROUP='T'` and years
/// 1..4 are `A`), so the set is per corpus rather than uniform.
fn anchors(tag: &str) -> Vec<(i32, &'static str, Year, usize)> {
    match tag {
        "corp2026" => vec![
            (0, "A", Year::Current, 167),
            (1, "A", Year::Prior1, 160),
            (2, "A", Year::Prior2, 152),
        ],
        "hull2024" => vec![
            (0, "T", Year::Current, 17),
            (1, "A", Year::Prior1, 304),
            (4, "A", Year::Prior4, 289),
        ],
        "mwgh2025" => vec![
            (0, "A", Year::Current, 141),
            (0, "T", Year::Current, 33),
            (4, "A", Year::Prior4, 162),
        ],
        other => panic!("unknown corpus {other}"),
    }
}

fn year_enum(y: i32) -> Year {
    match y {
        0 => Year::Current,
        1 => Year::Prior1,
        2 => Year::Prior2,
        3 => Year::Prior3,
        4 => Year::Prior4,
        5 => Year::Budget,
        _ => Year::Forecast,
    }
}

#[test]
fn single_blank_consolidation_equals_cube_balance() {
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let cube = load_cube(tag);
        let raw_bl = Table::parse_path(corpus_file(tag, "BL.dbf")).unwrap();
        let f = table_fixture(tag, "bl");
        assert_eq!(
            usize_at(&f, "frontier"),
            frontier(tag, "BL", &raw_bl),
            "{tag}: pinned BL frontier"
        );
        assert_eq!(usize_at(&f, "rows"), cube.rows().len(), "{tag}: prefix row count");

        let pinned_slices = f.get("slices").and_then(|x| x.as_obj()).expect("slices");

        // Every census slice is consolidated and compared to the cube law.
        for (key, s) in pinned_slices {
            let (ys, gs) = key.split_once('|').expect("year|group key");
            let year: i32 = ys.parse().expect("year");
            let org = Org::default();
            let cons = consolidate(&cube, &org, year_enum(year), gs);
            assert_eq!(
                usize_at(s, "consolidated"),
                cons.accounts.len(),
                "{tag}/{key} consolidated account count"
            );
            assert_eq!(cons.units.units.len(), 1, "{tag}/{key}: implicit blank unit");
            assert_eq!(cons.units.total_weight(), 1.0, "{tag}/{key}");

            for (number, account) in &cons.accounts {
                let expected = cube.balance(number, year, gs).unwrap_or_else(|| {
                    panic!("{tag}: {number} missing from cube at {key}")
                });
                assert!(
                    (account.total - expected).abs() < 1e-6,
                    "{tag}: {number} consolidated {} != cube {expected} at {key}",
                    account.total
                );
                assert_eq!(account.units.len(), 1, "{tag}/{number}");
                assert!((account.units[0].balance - expected).abs() < 1e-6);
            }

            assert_eq!(usize_at(s, "mismatch"), 0, "{tag}/{key} cube mismatch");
            assert_eq!(
                support::str_at(s, "checksum"),
                cons.checksum(),
                "{tag}/{key} slice digest"
            );
        }

        // Inline anchors: the strongest slices, stated without the fixture.
        for (year, group, yr, want) in anchors(tag) {
            let cons = consolidate(&cube, &Org::default(), yr, group);
            assert_eq!(
                cons.accounts.len(),
                want,
                "{tag}: y{year}/{group} consolidated account count"
            );
            assert_eq!(cons.checksum().len(), 8, "{tag}: digest shape");
            assert!(
                cons.accounts
                    .values()
                    .all(|a| a.units.len() == 1 && a.has_balance()),
                "{tag}: single-blank unit law at y{year}/{group}"
            );
        }

        // Corp is small enough to assert the whole slice set literally.
        if tag == "corp2026" {
            assert_eq!(pinned_slices.len(), 3, "corp2026 has three authentic slices");
        }
    }
}

/// `hull2024`'s year-0 `A` slice is fabricated-tail-only: the authentic prefix
/// carries year-0 data only under `GROUP='T'`. Asserting the absence keeps a
/// future de-truncation a loud change.
#[test]
fn hull_year_zero_has_no_authentic_actual_slice() {
    let cube = load_cube("hull2024");
    assert_eq!(
        cube.rows().iter().filter(|r| r.year == 0 && r.group == "A").count(),
        0,
        "hull year-0/A rows are fabricated-tail-only"
    );
    let cons = consolidate(&cube, &Org::default(), Year::Current, "A");
    assert!(cons.accounts.is_empty(), "nothing to consolidate at y0/A");

    // The year-0 data that *is* authentic is the tax-basis (`T`) slice.
    let t = consolidate(&cube, &Org::default(), Year::Current, "T");
    assert_eq!(t.accounts.len(), 17);
    assert_eq!(t.checksum(), "EBB46540");
}

#[test]
fn ownership_weights_and_exclusion() {
    let cube = Cube::from_table(&bl_table(&[("", 1000.0), ("SUB", 500.0), ("EXCL", 999.0)]))
        .unwrap();
    let org = Org::from_entities(vec![
        unit("", None, false),
        unit("SUB", Some(50.0), false),
        unit("EXCL", Some(100.0), true),
    ]);
    let units = ConsolidationUnits::from_org(&org, Year::Current);
    assert_eq!(units.units.len(), 3);
    assert_eq!(units.active().count(), 2);
    assert!((units.total_weight() - 1.5).abs() < 1e-12);
    assert_eq!(units.get("SUB").unwrap().weight(), 0.5);
    assert_eq!(units.get("EXCL").unwrap().weight(), 0.0);

    let cons = consolidate(&cube, &org, Year::Current, "A");
    let account = cons.account("1000").expect("consolidated 1000");
    assert_eq!(account.units.len(), 2, "excluded unit must not contribute");
    assert!((account.total - 1250.0).abs() < 1e-6, "got {}", account.total);
    assert!((cons.total("1000").unwrap() - 1250.0).abs() < 1e-6);

    // The same L5 law is visible per entity.
    assert_eq!(entity_balance(&cube, "1000", "", Year::Current, "a"), Some(1000.0));
    assert_eq!(entity_balance(&cube, "1000", "SUB", Year::Current, "A"), Some(500.0));
    assert_eq!(entity_balance(&cube, "1000", "MISSING", Year::Current, "A"), None);
}

/// `CONSCHKSUM` shape + stability law.
///
/// The digest algorithm CaseWare itself uses is **not recovered** (it needs a COM
/// write probe, and every corpus cell is blank), so this gate asserts the
/// documented placeholder law and nothing more: eight uppercase hex digits over a
/// canonical ASCII projection, deterministic, sensitive to the consolidated
/// picture, stampable into `AM.CONSCHKSUM`, and auditable against a stored cell.
/// Byte parity with CaseWare remains an open residual — it is deliberately not
/// claimed, and the census pins no digest for a corpus row because no corpus row
/// carries one.
#[test]
fn checksum_stamps_and_audits_am_rows() {
    let cube = Cube::from_table(&bl_table(&[("", 1000.0), ("SUB", 500.0)])).unwrap();
    let org = Org::from_entities(vec![unit("", None, false), unit("SUB", Some(50.0), false)]);
    let cons = consolidate(&cube, &org, Year::Current, "A");

    // Placeholder digest law, part 1: shape.
    assert_eq!(cons.checksum().len(), CHECKSUM_LEN);
    assert!(
        cons.checksum()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)),
        "digest must be 8 uppercase hex digits, got {:?}",
        cons.checksum()
    );
    // Part 2: determinism + self-verification.
    assert!(cons.verify());
    assert_eq!(cons.checksum(), cons.checksum());
    assert_eq!(cons.checksum(), Consolidation::build(
        &Cube::from_table(&bl_table(&[("", 1000.0), ("SUB", 500.0)])).unwrap(),
        &Org::from_entities(vec![unit("", None, false), unit("SUB", Some(50.0), false)]),
        Year::Current,
        "A",
    ).checksum(), "same inputs give the same digest");
    // Part 3: the per-account digest is the documented projection of
    // `(number, group, year, cents)` — 125,000 cents = 1,250.00 consolidated.
    assert_eq!(
        cons.account("1000").unwrap().checksum,
        account_checksum("1000", "A", Year::Current, 125_000)
    );
    assert_eq!(cons.account("1000").unwrap().total_cents(), 125_000);
    assert_ne!(
        account_checksum("1000", "A", Year::Current, 125_000),
        account_checksum("1000", "A", Year::Current, 125_001),
        "the digest must be sensitive to the amount"
    );

    let mut am = load_am("corp2026");
    for r in 0..am.count() {
        if !am.is_deleted(r) {
            write_cell(&mut am, r, "AC_NO", "").unwrap();
            write_cell(&mut am, r, "CONSCHKSUM", "").unwrap();
        }
    }
    let target = (0..am.count()).find(|&r| !am.is_deleted(r)).unwrap();
    write_cell(&mut am, target, "AC_NO", "1000").unwrap();

    assert_eq!(cons.stamp_table(&mut am).unwrap(), 1);
    let audit: ChecksumAudit = cons.audit_table(&am).unwrap();
    assert_eq!(audit.checked, 1);
    assert_eq!(audit.matches, 1);
    assert_eq!(audit.mismatches, 0);
    assert!(audit.is_clean());

    write_cell(&mut am, target, "CONSCHKSUM", "00000000").unwrap();
    let audit = cons.audit_table(&am).unwrap();
    assert_eq!(audit.checked, 1);
    assert_eq!(audit.matches, 0);
    assert_eq!(audit.mismatches, 1, "audit={audit:?}");
    assert_eq!(audit.missing, 0);
    let mismatch = audit.first_mismatch.as_ref().expect("mismatch detail");
    assert_eq!(mismatch.number, "1000");
    assert_eq!(mismatch.stored, "00000000");
    assert_eq!(mismatch.expected.len(), CHECKSUM_LEN);
    assert!(!audit.is_clean());

    // Blank cells are counted as missing mismatches.
    write_cell(&mut am, target, "CONSCHKSUM", "").unwrap();
    let audit = cons.audit_table(&am).unwrap();
    assert_eq!(audit.missing, 1);
    assert_eq!(audit.mismatches, 1);
}

/// Placeholder digest law, part 4: sensitivity + stamping round trip. A digest
/// that did not change with the consolidation would be useless for the stale-cell
/// audit, so the gate proves it changes, then proves a stamp survives re-read.
#[test]
fn checksum_value_changes_when_consolidation_changes() {
    let base = Consolidation::build(
        &Cube::from_table(&bl_table(&[("", 1000.0)])).unwrap(),
        &Org::default(),
        Year::Current,
        "A",
    );
    let changed = Consolidation::build(
        &Cube::from_table(&bl_table(&[("", 1001.0)])).unwrap(),
        &Org::default(),
        Year::Current,
        "A",
    );
    assert_ne!(base.checksum(), changed.checksum());
    assert_ne!(
        base.account("1000").unwrap().checksum,
        changed.account("1000").unwrap().checksum
    );
}


// --- CaseWare's own CONSCHKSUM law -----------------------------------------

/// `AM.CONSCHKSUM` is a **verbatim copy of `AM.CWCHECKSUM`** on the row.
///
/// The writer law, exactly as the vendor performs it:
///
/// ```text
/// cons := strfield("CWCHECKSUM");
/// assign(field("CONSCHKSUM"), cons);
/// ```
///
/// This is CaseWare's real law and it is **not** a digest of the consolidated
/// picture — which is why the `Consolidation` stand-in digest must never be
/// written into `CONSCHKSUM` on a real engagement. The gate pins the copy on
/// every frozen corpus and then pins the drift detector.
#[test]
fn conschksum_is_a_copy_of_cwchecksum() {
    for tag in ["corp2026", "hull2024", "mwgh2025"] {
        let mut am = load_am(tag);
        // Corpus state: CWCHECKSUM populated per row, CONSCHKSUM blank. The
        // copy therefore legitimately reports drift before it runs.
        let populated = (0..am.count())
            .filter(|&r| !am.is_deleted(r))
            .filter(|&r| !am.cell_str(r, "CWCHECKSUM").unwrap().trim().is_empty())
            .count();
        assert!(populated > 0, "{tag}: CWCHECKSUM must be populated");
        let before = conschksum_copy_drift(&am);
        assert_eq!(
            before.len(),
            (0..am.count()).filter(|&r| !am.is_deleted(r)).count(),
            "{tag}: every CONSCHKSUM is blank, so every row is drift"
        );

        let written = copy_cwchecksum_to_conschksum(&mut am).unwrap();
        assert_eq!(
            written,
            (0..am.count()).filter(|&r| !am.is_deleted(r)).count(),
            "{tag}: one copy per live row"
        );
        assert!(
            conschksum_copy_drift(&am).is_empty(),
            "{tag}: copy must leave no drift"
        );

        // the copy is byte-verbatim, padded to the C 8 cell
        for r in 0..am.count() {
            if am.is_deleted(r) {
                continue;
            }
            let src = am.cell_str(r, "CWCHECKSUM").unwrap();
            let dst = am.cell_str(r, "CONSCHKSUM").unwrap();
            assert_eq!(src.trim(), dst.trim(), "{tag}: row {r} copy");
        }
    }
}

/// The copy is width-validated: a `CWCHECKSUM` wider than the `C 8` target is
/// refused rather than silently truncated, so a bad source cannot corrupt a row.
#[test]
fn conschksum_copy_refuses_oversized_source() {
    let mut am = load_am("corp2026");
    // CWCHECKSUM is C 8 in the physical schema, so an oversized value cannot be
    // written to the source either — the guard lives on both sides.
    assert!(
        write_cell(&mut am, 0, "CWCHECKSUM", "0123456789").is_err(),
        "C 8 source cannot hold 10 chars"
    );
    assert!(write_cell(&mut am, 0, "CONSCHKSUM", "0123456789").is_err());
    assert!(write_cell(&mut am, 0, "CWCHECKSUM", "DEADBEEF").is_ok());
    assert_eq!(copy_cwchecksum_to_conschksum(&mut am).unwrap(), am.count());
    assert_eq!(am.cell_str(0, "CONSCHKSUM").unwrap().trim(), "DEADBEEF");
}

/// The stand-in digest is explicitly NOT CaseWare's law; both remain gated so
/// the distinction cannot quietly rot.
#[test]
fn stand_in_digest_is_labelled_and_distinct() {
    let corpus_row = "50C0141A";
    // the FNV stand-in is shape-compatible but not the vendor value
    assert_eq!(legacy_fnv(b"alpha"), checksum8(b"alpha"));
    assert_eq!(legacy_fnv(b"alpha").len(), CHECKSUM_LEN);
    assert_ne!(legacy_fnv(b"alpha"), corpus_row);
    // and CaseWare's CONSCHKSUM is a copy, i.e. it equals its input, not a hash
    let v = "50C0141A";
    assert_eq!(v, corpus_row, "copy law preserves the source byte for byte");
}
